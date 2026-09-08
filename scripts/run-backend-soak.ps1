[CmdletBinding()]
param(
    [ValidateRange(2, 180)]
    [int]$DurationMinutes = 120,

    [ValidateSet('after')]
    [string]$Label = 'after',

    [ValidateSet('debug')]
    [string]$Profile = 'debug'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$runnerScriptPath = [System.IO.Path]::GetFullPath($PSCommandPath)
$manifestPath = Join-Path $repoRoot 'nuclear-app\src-tauri\Cargo.toml'
$crateRoot = Split-Path -Parent $manifestPath
$runStamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$runId = [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path $repoRoot "target\soak\$Label-$runStamp-$runId"
$binRoot = Join-Path $runRoot 'bin'
$fixtureRoot = Join-Path $runRoot 'fixtures'
$ownerMarker = Join-Path $runRoot '.nuclear-phase5-soak-owner'
$frozenExecutable = Join-Path $binRoot 'nuclear-soak-tests.exe'
$buildLog = Join-Path $runRoot 'cargo-build.jsonl'
$sourceManifestPath = Join-Path $runRoot 'source-manifest.json'
$sourceManifestAfterBuildPath = Join-Path $runRoot 'source-manifest-after-build.json'
$buildEvidencePath = Join-Path $runRoot 'build-evidence.json'
$listLog = Join-Path $runRoot 'test-list.log'
$listErrorLog = Join-Path $runRoot 'test-list.stderr.log'
$discoveryResultPath = Join-Path $runRoot 'discovery-result.json'
$stdoutLog = Join-Path $runRoot 'soak.stdout.log'
$stderrLog = Join-Path $runRoot 'soak.stderr.log'
$soakEvidencePath = Join-Path $runRoot 'soak.json'
$samplesPath = Join-Path $runRoot 'samples.jsonl'
$runnerResultPath = Join-Path $runRoot 'runner-result.json'

$trackedEnvironment = @(
    'TEMP',
    'TMP',
    'NUCLEAR_SOAK_DURATION_SECS',
    'NUCLEAR_SOAK_RUN_ROOT',
    'NUCLEAR_SOAK_EVIDENCE_PATH',
    'NUCLEAR_SOAK_SAMPLES_PATH',
    'NUCLEAR_SOAK_LABEL',
    'NUCLEAR_SOAK_PROFILE'
)
$originalEnvironment = @{}
foreach ($name in $trackedEnvironment) {
    $originalEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Set-ProcessEnvironment {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [AllowNull()][string]$Value
    )
    [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
}

function Start-OwnedFrozenProcess {
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string]$StandardOutputPath,
        [Parameter(Mandatory = $true)][string]$StandardErrorPath
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $frozenExecutable
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }

    $stdoutStream = $null
    $stderrStream = $null
    $process = $null
    try {
        $stdoutStream = [IO.FileStream]::new(
            $StandardOutputPath,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::Read
        )
        $stderrStream = [IO.FileStream]::new(
            $StandardErrorPath,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::Read
        )
        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $startInfo
        if (-not $process.Start()) {
            throw 'The frozen soak process did not start.'
        }
        $stdoutPump = $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
        $stderrPump = $process.StandardError.BaseStream.CopyToAsync($stderrStream)
        $process | Add-Member -NotePropertyName SoakStdoutPump -NotePropertyValue $stdoutPump
        $process | Add-Member -NotePropertyName SoakStderrPump -NotePropertyValue $stderrPump
        $process | Add-Member -NotePropertyName SoakStdoutStream -NotePropertyValue $stdoutStream
        $process | Add-Member -NotePropertyName SoakStderrStream -NotePropertyValue $stderrStream
        $process | Add-Member -NotePropertyName SoakPumpsCompleted -NotePropertyValue $false
        return $process
    }
    catch {
        if ($null -ne $process) {
            try {
                $process.Refresh()
                if (-not $process.HasExited) {
                    $process.Kill($true)
                    [void]$process.WaitForExit(10000)
                }
            }
            catch {}
            $process.Dispose()
        }
        if ($null -ne $stderrStream) {
            $stderrStream.Dispose()
        }
        if ($null -ne $stdoutStream) {
            $stdoutStream.Dispose()
        }
        throw
    }
}

function Complete-OwnedFrozenProcessOutput {
    param([Parameter(Mandatory = $true)][Diagnostics.Process]$Process)
    if ([bool]$Process.SoakPumpsCompleted) {
        return
    }

    $pumpDeadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
    try {
        foreach ($pump in @($Process.SoakStdoutPump, $Process.SoakStderrPump)) {
            $remainingMilliseconds = [Math]::Floor(
                ($pumpDeadline - [DateTimeOffset]::UtcNow).TotalMilliseconds
            )
            if ($remainingMilliseconds -le 0 -or
                -not $pump.Wait([int][Math]::Min([int]::MaxValue, $remainingMilliseconds))) {
                throw 'Frozen soak output did not drain within the 10-second bound.'
            }
            [void]$pump.GetAwaiter().GetResult()
        }
        $Process.SoakStdoutStream.Flush($true)
        $Process.SoakStderrStream.Flush($true)
    }
    finally {
        $Process.SoakStdoutStream.Dispose()
        $Process.SoakStderrStream.Dispose()
        $Process.SoakPumpsCompleted = $true
    }
}

function Stop-OwnedTestProcess {
    param([Diagnostics.Process]$Process)
    if ($null -eq $Process) {
        return
    }
    $Process.Refresh()
    if (-not $Process.HasExited) {
        # This Process object was created from the exact frozen soak executable.
        # Kill its owned descendants as well, then leave the run tree as evidence.
        $Process.Kill($true)
        if (-not $Process.WaitForExit(10000)) {
            throw 'The owned soak process tree did not exit within the 10-second termination bound.'
        }
    }
    Complete-OwnedFrozenProcessOutput -Process $Process
}

function Get-SourceManifest {
    $files = [Collections.Generic.List[System.IO.FileInfo]]::new()
    foreach ($path in @(
        (Join-Path $crateRoot 'Cargo.toml'),
        (Join-Path $crateRoot 'Cargo.lock'),
        (Join-Path $crateRoot 'build.rs'),
        (Join-Path $crateRoot 'build_config.rs'),
        (Join-Path $crateRoot 'deny.toml'),
        (Join-Path $crateRoot 'sidecars.lock.json'),
        (Join-Path $crateRoot 'tauri.conf.json'),
        $runnerScriptPath
    )) {
        $files.Add((Get-Item -LiteralPath $path))
    }
    foreach ($directory in @('src', 'binaries', 'capabilities', 'icons', 'gen')) {
        $path = Join-Path $crateRoot $directory
        if (Test-Path -LiteralPath $path -PathType Container) {
            Get-ChildItem -LiteralPath $path -Recurse -File |
                Sort-Object FullName |
                ForEach-Object { $files.Add($_) }
        }
    }

    return @($files | Sort-Object FullName | ForEach-Object {
        $relative = [System.IO.Path]::GetRelativePath($repoRoot, $_.FullName).Replace('\', '/')
        [ordered]@{
            path = $relative
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
}

function Get-HostEvidence {
    $windows = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $processorName = [Microsoft.Win32.Registry]::GetValue(
        'HKEY_LOCAL_MACHINE\HARDWARE\DESCRIPTION\System\CentralProcessor\0',
        'ProcessorNameString',
        $null
    )
    $repoDrive = [System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($repoRoot))
    return [ordered]@{
        windowsDisplayVersion = [string]$windows.DisplayVersion
        windowsBuild = "{0}.{1}" -f $windows.CurrentBuildNumber, $windows.UBR
        processorName = ([string]$processorName).Trim()
        logicalProcessors = [Environment]::ProcessorCount
        repositoryVolumeFormat = $repoDrive.DriveFormat
        repositoryVolumeAvailableBytes = $repoDrive.AvailableFreeSpace
    }
}

New-Item -ItemType Directory -Path $binRoot | Out-Null
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
Set-Content -LiteralPath $ownerMarker -Value $runId -NoNewline

$sourceManifest = Get-SourceManifest
$sourceManifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $sourceManifestPath -Encoding utf8
$sourceManifestHash = (Get-FileHash -LiteralPath $sourceManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$headCommit = (& git -C $repoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) {
    throw 'Could not record the source commit for the soak executable.'
}
$gitStatus = @(& git -C $repoRoot status --porcelain=v1)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not record the source working tree for the soak executable.'
}
$cargoVersion = (& cargo --version).Trim()
$rustcVersion = (& rustc --version).Trim()

$buildArguments = @(
    'test',
    '--manifest-path', $manifestPath,
    '--locked',
    '--offline',
    '--all-features',
    '--lib',
    '--no-run',
    '--message-format=json-render-diagnostics'
)
Write-Host 'Compiling the isolated Phase 5 soak executable...'
$buildLines = @(& cargo @buildArguments 2>&1 | ForEach-Object { [string]$_ })
$buildExitCode = $LASTEXITCODE
$buildLines | Set-Content -LiteralPath $buildLog -Encoding utf8
if ($buildExitCode -ne 0) {
    throw "Soak executable compilation failed with exit code $buildExitCode. Evidence remains at $runRoot."
}

$sourceManifestAfterBuild = Get-SourceManifest
$sourceManifestAfterBuild | ConvertTo-Json -Depth 6 |
    Set-Content -LiteralPath $sourceManifestAfterBuildPath -Encoding utf8
$sourceManifestAfterBuildHash = (Get-FileHash -LiteralPath $sourceManifestAfterBuildPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sourceManifestAfterBuildHash -cne $sourceManifestHash) {
    throw "A source or build input changed while compiling the soak executable. Evidence remains at $runRoot."
}

$executables = @($buildLines | ForEach-Object {
    try {
        $message = $_ | ConvertFrom-Json -ErrorAction Stop
        if ($message.reason -ceq 'compiler-artifact' -and
            $message.target.name -ceq 'nuclear_app_lib' -and
            [bool]$message.profile.test -and
            -not [string]::IsNullOrWhiteSpace([string]$message.executable)) {
            [System.IO.Path]::GetFullPath([string]$message.executable)
        }
    }
    catch {
        # Rendered diagnostics and status lines are retained in the raw build log.
    }
} | Sort-Object -Unique)
if ($executables.Count -ne 1) {
    throw "Expected one nuclear_app_lib test executable, found $($executables.Count). Evidence remains at $runRoot."
}
$sourceExecutable = $executables[0]
$sourceExecutableInfo = Get-Item -LiteralPath $sourceExecutable
$sourceExecutableHash = (Get-FileHash -LiteralPath $sourceExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
Copy-Item -LiteralPath $sourceExecutable -Destination $frozenExecutable
$frozenHashBefore = (Get-FileHash -LiteralPath $frozenExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
if ($frozenHashBefore -cne $sourceExecutableHash) {
    throw "The frozen soak executable hash did not match its build artifact. Evidence remains at $runRoot."
}
[System.IO.File]::SetAttributes(
    $frozenExecutable,
    [System.IO.File]::GetAttributes($frozenExecutable) -bor [System.IO.FileAttributes]::ReadOnly
)

$buildEvidence = [ordered]@{
    schemaVersion = 1
    runId = $runId
    recordedAtUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    label = $Label
    profile = $Profile
    durationMinutes = $DurationMinutes
    command = [ordered]@{ executable = 'cargo'; arguments = $buildArguments }
    cargoVersion = $cargoVersion
    rustcVersion = $rustcVersion
    headCommit = $headCommit
    gitStatusPorcelain = $gitStatus
    sourceManifest = [ordered]@{
        path = $sourceManifestPath
        sha256 = $sourceManifestHash
        afterBuildPath = $sourceManifestAfterBuildPath
        afterBuildSha256 = $sourceManifestAfterBuildHash
        unchangedDuringBuild = $true
        fileCount = $sourceManifest.Count
    }
    runnerScript = [ordered]@{
        path = $runnerScriptPath
        sha256 = (Get-FileHash -LiteralPath $runnerScriptPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    sourceExecutable = [ordered]@{
        path = $sourceExecutable
        bytes = $sourceExecutableInfo.Length
        lastWriteTimeUtc = $sourceExecutableInfo.LastWriteTimeUtc.ToString('O')
        sha256 = $sourceExecutableHash
    }
    frozenExecutable = [ordered]@{
        path = $frozenExecutable
        sha256 = $frozenHashBefore
        readOnly = $true
    }
    host = Get-HostEvidence
}
$buildEvidence | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $buildEvidencePath -Encoding utf8

$testFilter = 'soak_harness::phase5_soak'
$listArguments = @($testFilter, '--exact', '--ignored', '--list')
$listProcess = $null
$listExitCode = $null
$listTimedOut = $false
$listError = $null
try {
    $listProcess = Start-OwnedFrozenProcess `
        -Arguments $listArguments `
        -WorkingDirectory $runRoot `
        -StandardOutputPath $listLog `
        -StandardErrorPath $listErrorLog
    if (-not $listProcess.WaitForExit(60000)) {
        $listTimedOut = $true
        $listError = 'Frozen soak test discovery exceeded its 60-second bound.'
        Stop-OwnedTestProcess -Process $listProcess
    }
    Stop-OwnedTestProcess -Process $listProcess
    $listProcess.Refresh()
    if ($listProcess.HasExited) {
        $listExitCode = $listProcess.ExitCode
    }
}
catch {
    $listError = "Frozen soak test discovery failed: $($_.Exception.Message)"
    if ($null -ne $listProcess) {
        try { Stop-OwnedTestProcess -Process $listProcess } catch {}
    }
}
$listOutput = if (Test-Path -LiteralPath $listLog -PathType Leaf) {
    @(Get-Content -LiteralPath $listLog | ForEach-Object { [string]$_ })
}
else {
    @()
}
$listedTests = @($listOutput | Where-Object { $_ -match ': test$' })
if ($null -eq $listError -and
    ($listExitCode -ne 0 -or
    $listedTests.Count -ne 1 -or
    [string]$listedTests[0] -cne 'soak_harness::phase5_soak: test')) {
    $listError = 'The frozen executable did not expose exactly the requested Phase 5 soak test.'
}
$discoveryResult = [ordered]@{
    schemaVersion = 1
    status = if ($null -eq $listError) { 'passed' } else { 'failed' }
    error = $listError
    timeoutSeconds = 60
    timedOut = $listTimedOut
    processId = if ($null -eq $listProcess) { $null } else { $listProcess.Id }
    exitCode = $listExitCode
    arguments = $listArguments
    listedTests = $listedTests
    exactTestCount = $listedTests.Count
    frozenExecutableSha256 = $frozenHashBefore
}
$discoveryResult | ConvertTo-Json -Depth 8 |
    Set-Content -LiteralPath $discoveryResultPath -Encoding utf8
if ($null -ne $listError) {
    throw "$listError Evidence remains at $runRoot."
}

foreach ($pair in ([ordered]@{
    TEMP = $fixtureRoot
    TMP = $fixtureRoot
    NUCLEAR_SOAK_DURATION_SECS = [string]($DurationMinutes * 60)
    NUCLEAR_SOAK_RUN_ROOT = $runRoot
    NUCLEAR_SOAK_EVIDENCE_PATH = $soakEvidencePath
    NUCLEAR_SOAK_SAMPLES_PATH = $samplesPath
    NUCLEAR_SOAK_LABEL = $Label
    NUCLEAR_SOAK_PROFILE = $Profile
}).GetEnumerator()) {
    Set-ProcessEnvironment -Name $pair.Key -Value ([string]$pair.Value)
}

$testArguments = @($testFilter, '--exact', '--ignored', '--nocapture', '--test-threads=1')
$testProcess = $null
$testExitCode = $null
$runnerError = $null
$deadlineExceeded = $false
$startedAt = [DateTimeOffset]::UtcNow
$deadline = $startedAt.AddSeconds(($DurationMinutes * 60) + 120)

try {
    Write-Host "Starting frozen Phase 5 soak for $DurationMinutes minute(s)."
    $testProcess = Start-OwnedFrozenProcess `
        -Arguments $testArguments `
        -WorkingDirectory $runRoot `
        -StandardOutputPath $stdoutLog `
        -StandardErrorPath $stderrLog
    while (-not $testProcess.HasExited) {
        $remainingMilliseconds = [Math]::Floor(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
        if ($remainingMilliseconds -le 0) {
            $deadlineExceeded = $true
            $runnerError = "Frozen soak test exceeded its requested duration plus the 2-minute cleanup grace."
            break
        }
        $waitMilliseconds = [Math]::Min(30000, [Math]::Max(1, $remainingMilliseconds))
        if ($testProcess.WaitForExit([int]$waitMilliseconds)) {
            break
        }
        $elapsed = [DateTimeOffset]::UtcNow - $startedAt
        $sampleCount = if (Test-Path -LiteralPath $samplesPath) {
            @(Get-Content -LiteralPath $samplesPath).Count
        }
        else {
            0
        }
        Write-Host ("Phase 5 soak running: {0:hh\:mm\:ss}, samples={1}" -f $elapsed, $sampleCount)
    }
    if ($deadlineExceeded) {
        Stop-OwnedTestProcess -Process $testProcess
    }
    $testProcess.Refresh()
    if ($testProcess.HasExited) {
        $testExitCode = $testProcess.ExitCode
    }
    if ($null -eq $runnerError -and $testExitCode -ne 0) {
        $runnerError = "Frozen soak test failed with exit code $testExitCode."
    }
}
catch {
    $runnerError = $_.Exception.Message
}
finally {
    if ($null -ne $testProcess) {
        try {
            Stop-OwnedTestProcess -Process $testProcess
        }
        catch {
            if ($null -eq $runnerError) {
                $runnerError = "Failed to terminate the owned soak process tree: $($_.Exception.Message)"
            }
        }
    }
    foreach ($name in $trackedEnvironment) {
        Set-ProcessEnvironment -Name $name -Value $originalEnvironment[$name]
    }
}

$endedAt = [DateTimeOffset]::UtcNow
$frozenHashAfter = (Get-FileHash -LiteralPath $frozenExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
if ($frozenHashAfter -cne $frozenHashBefore) {
    if ($null -eq $runnerError) {
        $runnerError = 'The frozen soak executable changed during the run.'
    }
}
$soakEvidence = $null
$soakEvidenceValidated = $false
if (Test-Path -LiteralPath $soakEvidencePath -PathType Leaf) {
    try {
        $soakEvidence = Get-Content -LiteralPath $soakEvidencePath -Raw | ConvertFrom-Json -ErrorAction Stop
        $expectedDurationSeconds = $DurationMinutes * 60
        $runnerElapsedMilliseconds = [uint64][Math]::Max(0, ($endedAt - $startedAt).TotalMilliseconds)
        $soakElapsedMilliseconds = [uint64]$soakEvidence.actualDurationMillis
        if ([int]$soakEvidence.schemaVersion -ne 1 -or
            [string]$soakEvidence.status -cne 'passed' -or
            $null -ne $soakEvidence.error -or
            [string]$soakEvidence.label -cne $Label -or
            [string]$soakEvidence.profile -cne $Profile -or
            [uint64]$soakEvidence.requestedDurationSeconds -ne [uint64]$expectedDurationSeconds -or
            $soakElapsedMilliseconds -lt [uint64]($expectedDurationSeconds * 1000) -or
            $soakElapsedMilliseconds -gt [uint64](($expectedDurationSeconds + 120) * 1000) -or
            $soakElapsedMilliseconds -gt ($runnerElapsedMilliseconds + 5000) -or
            -not [bool]$soakEvidence.finalJournalReopened) {
            throw 'soak.json did not report a complete passing run for the requested duration.'
        }
        $soakEvidenceValidated = $true
    }
    catch {
        if ($null -eq $runnerError) {
            $runnerError = "soak.json validation failed: $($_.Exception.Message)"
        }
    }
}
elseif ($null -eq $runnerError) {
    $runnerError = 'The frozen soak test did not publish soak.json.'
}
$runnerResult = [ordered]@{
    schemaVersion = 1
    runId = $runId
    label = $Label
    profile = $Profile
    startedAtUnixMs = $startedAt.ToUnixTimeMilliseconds()
    endedAtUnixMs = $endedAt.ToUnixTimeMilliseconds()
    requestedDurationMinutes = $DurationMinutes
    deadlineUnixMs = $deadline.ToUnixTimeMilliseconds()
    deadlineExceeded = $deadlineExceeded
    testExitCode = $testExitCode
    status = if ($null -eq $runnerError) { 'passed' } else { 'failed' }
    error = $runnerError
    frozenExecutableHashBefore = $frozenHashBefore
    frozenExecutableHashAfter = $frozenHashAfter
    frozenExecutableUnchanged = $frozenHashBefore -ceq $frozenHashAfter
    soakEvidenceValidated = $soakEvidenceValidated
    soakEvidenceStatus = if ($null -eq $soakEvidence) { $null } else { [string]$soakEvidence.status }
    soakRequestedDurationSeconds = if ($null -eq $soakEvidence) { $null } else { [uint64]$soakEvidence.requestedDurationSeconds }
    soakActualDurationMillis = if ($null -eq $soakEvidence) { $null } else { [uint64]$soakEvidence.actualDurationMillis }
    artifacts = [ordered]@{
        runRoot = $runRoot
        buildEvidence = $buildEvidencePath
        sourceManifest = $sourceManifestPath
        sourceManifestAfterBuild = $sourceManifestAfterBuildPath
        buildLog = $buildLog
        testList = $listLog
        testListStderr = $listErrorLog
        discoveryResult = $discoveryResultPath
        stdout = $stdoutLog
        stderr = $stderrLog
        soakEvidence = $soakEvidencePath
        samples = $samplesPath
    }
}
$runnerResult | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $runnerResultPath -Encoding utf8

Write-Host "PHASE5_SOAK_RUN=$runRoot"
Write-Host "PHASE5_SOAK_RESULT=$runnerResultPath"
if ($null -ne $runnerError) {
    throw "$runnerError Evidence remains at $runRoot."
}
