[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $CandidateDirectory,

    [Parameter(Mandatory)]
    [string] $ResultsDirectory,

    [ValidatePattern('^0\.6\.0$')]
    [string] $ExpectedVersion = '0.6.0',

    [ValidatePattern('^[0-9a-f]{40}$')]
    [string] $ExpectedCommitSha,

    [ValidatePattern('^[1-9][0-9]*$')]
    [string] $ExpectedCandidateRunId
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Candidate acceptance requires Windows x64.'
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$resultsRoot = [System.IO.Path]::GetFullPath($ResultsDirectory)
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$inventoryPath = Join-Path $candidateRoot 'release-candidate-inventory.json'

& (Join-Path $PSScriptRoot 'verify-release-candidate.ps1') `
    -CandidateDirectory $candidateRoot `
    -ExpectedVersion $ExpectedVersion `
    -ExpectedCommitSha $ExpectedCommitSha | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw 'Candidate contract verification failed before acceptance.'
}

$inventory = Get-Content -Raw -LiteralPath $inventoryPath | ConvertFrom-Json
$installerName = "Nuclear.Downloader_${ExpectedVersion}_x64-setup.exe"
$portableName = "Nuclear.Downloader_${ExpectedVersion}_x64-portable.zip"
$installerPath = Join-Path $candidateRoot $installerName
$portablePath = Join-Path $candidateRoot $portableName
foreach ($required in @($installerPath, $portablePath)) {
    $item = Get-Item -LiteralPath $required -Force
    if (-not $item.PSIsContainer -and
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
        continue
    }
    throw "Candidate acceptance input must be a regular non-reparse file: $required"
}

$acceptanceBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$acceptanceBase = [System.IO.Path]::GetFullPath($acceptanceBase)
$acceptanceRoot = Join-Path $acceptanceBase "nuclear-acceptance-$([Guid]::NewGuid().ToString('N'))"
$acceptanceLeaf = [System.IO.Path]::GetFileName($acceptanceRoot)
if ($acceptanceLeaf -cnotmatch '^nuclear-acceptance-[0-9a-f]{32}$') {
    throw 'Refusing to create an ambiguously named acceptance workspace.'
}

$installRoot = Join-Path $acceptanceRoot 'installed'
$portableRoot = Join-Path $acceptanceRoot 'portable'
$fixtureRoot = Join-Path $acceptanceRoot 'fixture'
$profileRoot = Join-Path $acceptanceRoot 'profile'
$localDataRoot = Join-Path $profileRoot 'LocalAppData'
$roamingDataRoot = Join-Path $profileRoot 'RoamingAppData'
$downloadsRoot = Join-Path $profileRoot 'Downloads'
$ownershipMarker = Join-Path $acceptanceRoot '.nuclear-candidate-acceptance'
$serverReadyPath = Join-Path $fixtureRoot 'server.port'
$fixtureMediaPath = Join-Path $fixtureRoot 'fixture-video.mp4'
$serverProcess = $null
$startedAt = [DateTimeOffset]::UtcNow
$steps = [ordered]@{}
$processInvocation = 0

function Limit-RetainedProcessLog {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [long] $MaximumBytes = 4MB
    )

    $item = Get-Item -LiteralPath $Path
    if ($item.Length -le $MaximumBytes) {
        return
    }

    $buffer = [byte[]]::new([int]$MaximumBytes)
    $source = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        [void]$source.Seek(-$MaximumBytes, [System.IO.SeekOrigin]::End)
        $offset = 0
        while ($offset -lt $buffer.Length) {
            $read = $source.Read($buffer, $offset, $buffer.Length - $offset)
            if ($read -eq 0) { break }
            $offset += $read
        }
    } finally {
        $source.Dispose()
    }

    $destination = [System.IO.File]::Open($Path, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $notice = [System.Text.Encoding]::UTF8.GetBytes("[earlier process output omitted; retained tail follows]`n")
        $destination.Write($notice, 0, $notice.Length)
        $destination.Write($buffer, 0, $offset)
    } finally {
        $destination.Dispose()
    }
}

function Write-ProcessLogTail {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Label
    )

    if ((Get-Item -LiteralPath $Path).Length -eq 0) {
        return
    }
    Write-Host "---- $Label (last 120 lines) ----"
    Get-Content -LiteralPath $Path -Tail 120 | ForEach-Object { Write-Host $_ }
}

function Invoke-OwnedProcess {
    param(
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)]
        [ValidatePattern('^[a-z0-9-]+$')]
        [string] $LogName,
        [string[]] $ArgumentList = @(),
        [int] $TimeoutSeconds = 300,
        [hashtable] $Environment = @{},
        [string] $WorkingDirectory = $repositoryRoot
    )

    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $WorkingDirectory
    foreach ($argument in $ArgumentList) {
        [void]$start.ArgumentList.Add($argument)
    }
    foreach ($entry in $Environment.GetEnumerator()) {
        $start.Environment[[string]$entry.Key] = [string]$entry.Value
    }
    $script:processInvocation++
    $logStem = '{0:D2}-{1}' -f $script:processInvocation, $LogName
    $stdoutPath = Join-Path $resultsRoot "$logStem.stdout.log"
    $stderrPath = Join-Path $resultsRoot "$logStem.stderr.log"
    $stdoutStream = [System.IO.File]::Open($stdoutPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $stderrStream = [System.IO.File]::Open($stderrPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $process = $null
    $stdoutCopy = $null
    $stderrCopy = $null
    $timedOut = $false
    $processExitCode = $null
    try {
        $process = [System.Diagnostics.Process]::Start($start)
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderrStream)
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Write-Warning $_ }
            [void]$process.WaitForExit(10000)
        }
        $processExitCode = $process.ExitCode
        $stdoutCopy.GetAwaiter().GetResult()
        $stderrCopy.GetAwaiter().GetResult()
    } finally {
        $stdoutStream.Dispose()
        $stderrStream.Dispose()
        if ($process) { $process.Dispose() }
    }

    Limit-RetainedProcessLog -Path $stdoutPath
    Limit-RetainedProcessLog -Path $stderrPath
    if ($timedOut) {
        Write-ProcessLogTail -Path $stdoutPath -Label "$LogName stdout"
        Write-ProcessLogTail -Path $stderrPath -Label "$LogName stderr"
        throw "Process exceeded its $TimeoutSeconds-second acceptance limit: $FilePath"
    }
    if ($processExitCode -ne 0) {
        Write-ProcessLogTail -Path $stdoutPath -Label "$LogName stdout"
        Write-ProcessLogTail -Path $stderrPath -Label "$LogName stderr"
        throw "Process exited with code $processExitCode`: $FilePath"
    }
}

if (Test-Path -LiteralPath $resultsRoot) {
    throw "Acceptance results directory already exists: $resultsRoot"
}
New-Item -ItemType Directory -Path $resultsRoot | Out-Null
New-Item -ItemType Directory -Path $acceptanceRoot, $installRoot, $portableRoot, $fixtureRoot, $profileRoot, $localDataRoot, $roamingDataRoot, $downloadsRoot | Out-Null
[System.IO.File]::WriteAllText($ownershipMarker, $ExpectedCommitSha, [System.Text.UTF8Encoding]::new($false))

$processEnvironment = @{
    USERPROFILE = $profileRoot
    LOCALAPPDATA = $localDataRoot
    APPDATA = $roamingDataRoot
    NUCLEAR_E2E_FIXTURE_TITLE = 'fixture-video'
}

try {
    Expand-Archive -LiteralPath $portablePath -DestinationPath $portableRoot
    $portableExecutable = Join-Path $portableRoot 'nuclear.exe'
    $ffmpegExecutable = Join-Path $portableRoot 'ffmpeg.exe'
    foreach ($required in @($portableExecutable, $ffmpegExecutable)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "Portable candidate is missing $([System.IO.Path]::GetFileName($required))."
        }
    }
    $steps.portableExtracted = 'passed'

    Invoke-OwnedProcess -FilePath $ffmpegExecutable -LogName 'fixture-ffmpeg' -TimeoutSeconds 180 -ArgumentList @(
        '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'lavfi', '-i', 'testsrc2=size=960x540:rate=30',
        '-f', 'lavfi', '-i', 'sine=frequency=1000:sample_rate=48000',
        '-t', '20', '-c:v', 'mpeg4', '-q:v', '3', '-c:a', 'aac',
        '-movflags', '+faststart', $fixtureMediaPath
    )
    $fixtureItem = Get-Item -LiteralPath $fixtureMediaPath
    if ($fixtureItem.Length -lt 1MB -or $fixtureItem.Length -gt 256MB) {
        throw 'Generated fixture media is outside the bounded 1-256 MiB acceptance range.'
    }
    $steps.fixtureGenerated = 'passed'

    $nodeExecutable = (Get-Command node.exe -ErrorAction Stop).Source
    $serverStart = [System.Diagnostics.ProcessStartInfo]::new()
    $serverStart.FileName = $nodeExecutable
    $serverStart.UseShellExecute = $false
    $serverStart.CreateNoWindow = $true
    [void]$serverStart.ArgumentList.Add((Join-Path $appRoot 'e2e\fixtures\media-server.mjs'))
    [void]$serverStart.ArgumentList.Add($fixtureMediaPath)
    [void]$serverStart.ArgumentList.Add($serverReadyPath)
    $serverProcess = [System.Diagnostics.Process]::Start($serverStart)
    $readyDeadline = [DateTimeOffset]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $serverReadyPath -PathType Leaf)) {
        if ($serverProcess.HasExited) { throw 'Fixture HTTP server exited before readiness.' }
        if ([DateTimeOffset]::UtcNow -ge $readyDeadline) { throw 'Fixture HTTP server did not become ready.' }
        Start-Sleep -Milliseconds 100
    }
    $fixturePort = [int]([System.IO.File]::ReadAllText($serverReadyPath, [System.Text.Encoding]::ASCII))
    if ($fixturePort -lt 1024 -or $fixturePort -gt 65535) { throw 'Fixture server returned an invalid port.' }
    $processEnvironment.NUCLEAR_E2E_FIXTURE_URL = "http://127.0.0.1:$fixturePort/fixture-video.mp4"
    $processEnvironment.NUCLEAR_E2E_SLOW_FIXTURE_URL = "http://127.0.0.1:$fixturePort/slow-fixture-video.mp4"
    $steps.fixtureServer = 'passed'

    if ($installRoot.Contains(' ')) {
        throw 'The isolated NSIS install root must not contain spaces because /D must be the final raw argument.'
    }
    Invoke-OwnedProcess -FilePath $installerPath -LogName 'nsis-install' -ArgumentList @('/S', "/D=$installRoot") -TimeoutSeconds 300
    $installedExecutable = Join-Path $installRoot 'nuclear.exe'
    $uninstaller = Join-Path $installRoot 'uninstall.exe'
    if (-not (Test-Path -LiteralPath $installedExecutable -PathType Leaf) -or
        -not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
        throw 'Silent NSIS install did not create the expected application and uninstaller bytes.'
    }
    $steps.cleanInstall = 'passed'

    $wdioEnvironment = @{}
    foreach ($entry in $processEnvironment.GetEnumerator()) { $wdioEnvironment[$entry.Key] = $entry.Value }
    $wdioEnvironment.NUCLEAR_E2E_APP_BINARY = $installedExecutable
    $wdioEnvironment.NUCLEAR_E2E_NATIVE_SUITE = 'full'
    $wdioCli = Join-Path $appRoot 'node_modules\@wdio\cli\bin\wdio.js'
    if (-not (Test-Path -LiteralPath $wdioCli -PathType Leaf)) {
        throw 'Pinned WebdriverIO CLI is missing; run npm ci before candidate acceptance.'
    }
    Invoke-OwnedProcess -FilePath $nodeExecutable -LogName 'wdio-installed-full' -ArgumentList @($wdioCli, 'run', './e2e/wdio.native.conf.mjs') -TimeoutSeconds 600 -Environment $wdioEnvironment -WorkingDirectory $appRoot
    $steps.installedFixtureDownloadConversionCancelReloadDiagnostics = 'passed'

    $journalPath = Join-Path $localDataRoot 'Nuclear Downloader\state-v1.dpapi'
    if (-not (Test-Path -LiteralPath $journalPath -PathType Leaf)) {
        throw 'The backend journal was not persisted after the fixture lifecycle.'
    }

    $wdioEnvironment.NUCLEAR_E2E_NATIVE_SUITE = 'restart'
    $wdioEnvironment.NUCLEAR_E2E_RESTART_TITLE = 'fixture-video'
    Invoke-OwnedProcess -FilePath $nodeExecutable -LogName 'wdio-installed-restart' -ArgumentList @($wdioCli, 'run', './e2e/wdio.native.conf.mjs') -TimeoutSeconds 180 -Environment $wdioEnvironment -WorkingDirectory $appRoot
    $steps.processRestartJournalRecovery = 'passed'

    $wdioEnvironment.NUCLEAR_E2E_APP_BINARY = $portableExecutable
    $wdioEnvironment.NUCLEAR_E2E_NATIVE_SUITE = 'smoke'
    Invoke-OwnedProcess -FilePath $nodeExecutable -LogName 'wdio-portable-smoke' -ArgumentList @($wdioCli, 'run', './e2e/wdio.native.conf.mjs') -TimeoutSeconds 180 -Environment $wdioEnvironment -WorkingDirectory $appRoot
    $steps.portableStartup = 'passed'

    Invoke-OwnedProcess -FilePath $uninstaller -LogName 'nsis-uninstall' -ArgumentList @('/S') -TimeoutSeconds 300
    if (Test-Path -LiteralPath $installedExecutable -PathType Leaf) {
        throw 'NSIS uninstall left the installed application executable behind.'
    }
    if (-not (Test-Path -LiteralPath $journalPath -PathType Leaf)) {
        throw 'NSIS uninstall unexpectedly removed per-user queue history.'
    }
    $steps.uninstallAndRetainedUserData = 'passed'

    & (Join-Path $PSScriptRoot 'verify-release-candidate.ps1') `
        -CandidateDirectory $candidateRoot `
        -ExpectedVersion $ExpectedVersion `
        -ExpectedCommitSha $ExpectedCommitSha | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Candidate bytes changed during acceptance.' }
    $steps.postAcceptanceHashVerification = 'passed'

    $result = [ordered]@{
        schemaVersion = 1
        releaseVersion = $ExpectedVersion
        sourceCommit = $ExpectedCommitSha
        candidateRunId = $ExpectedCandidateRunId
        candidateCreatedAt = [string]$inventory.createdAt
        startedAt = $startedAt.ToString('yyyy-MM-ddTHH:mm:ssZ')
        completedAt = [DateTimeOffset]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
        os = [ordered]@{
            description = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
            architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        }
        steps = $steps
        manualAcceptanceRequired = @(
            'Dedicated-account cookie/login test with no CI cookie secret',
            'Maintainer review of managed-runtime update and rollback UI against protected signed test assets',
            'Maintainer acceptance decision before publication'
        )
        candidateAssets = $inventory.assets
    }
    $resultJson = $result | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText(
        (Join-Path $resultsRoot 'windows-x64-acceptance.json'),
        $resultJson,
        [System.Text.UTF8Encoding]::new($false)
    )
    Write-Output (Join-Path $resultsRoot 'windows-x64-acceptance.json')
} finally {
    if ($serverProcess -and -not $serverProcess.HasExited) {
        try { $serverProcess.Kill($true) } catch { Write-Warning $_ }
        try { [void]$serverProcess.WaitForExit(10000) } catch { Write-Warning $_ }
    }
    if (Test-Path -LiteralPath $acceptanceRoot) {
        $canonicalRoot = [System.IO.Path]::GetFullPath($acceptanceRoot)
        $relative = [System.IO.Path]::GetRelativePath($acceptanceBase, $canonicalRoot)
        $markerItem = Get-Item -LiteralPath $ownershipMarker -Force -ErrorAction SilentlyContinue
        $rootItem = Get-Item -LiteralPath $canonicalRoot -Force
        $safeRelative = $relative -match '^nuclear-acceptance-[0-9a-f]{32}$'
        $safeItems = $markerItem -and
            -not $markerItem.PSIsContainer -and
            ($markerItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0 -and
            ($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
        if ($safeRelative -and $safeItems -and
            (Get-Content -Raw -LiteralPath $ownershipMarker) -ceq $ExpectedCommitSha) {
            Remove-Item -LiteralPath $canonicalRoot -Recurse -Force
        } else {
            Write-Warning "Acceptance workspace cleanup was skipped because ownership could not be proven: $canonicalRoot"
        }
    }
}
