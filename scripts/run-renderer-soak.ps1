[CmdletBinding(DefaultParameterSetName = 'Minutes')]
param(
    [Parameter(ParameterSetName = 'Minutes')]
    [ValidateRange(2, 180)]
    [int]$DurationMinutes = 120,

    [Parameter(Mandatory = $true, ParameterSetName = 'Seconds')]
    [ValidateRange(10, 10800)]
    [int]$DurationSeconds
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$appRoot = Join-Path $repoRoot 'nuclear-app'
$runnerPath = [IO.Path]::GetFullPath($PSCommandPath)
$nodePath = Join-Path $appRoot 'src-tauri\target\toolchains\node-v22.23.1-win-x64\node.exe'
$vitestPath = Join-Path $appRoot 'node_modules\vitest\vitest.mjs'
$testPath = Join-Path $appRoot 'src\routes\page-lifecycle-soak.test.ts'
$configuredSeconds = if ($PSCmdlet.ParameterSetName -eq 'Seconds') {
    $DurationSeconds
} else {
    $DurationMinutes * 60
}
$stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$runId = [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path $repoRoot "target\renderer-soak\$stamp-$runId"
$stdoutPath = Join-Path $runRoot 'vitest.stdout.log'
$stderrPath = Join-Path $runRoot 'vitest.stderr.log'
$beforeManifestPath = Join-Path $runRoot 'inputs-before.json'
$afterManifestPath = Join-Path $runRoot 'inputs-after.json'
$receiptPath = Join-Path $runRoot 'renderer-soak-receipt.json'
$sourceArchiveRoot = Join-Path $runRoot 'inputs-before'
$originalDuration = [Environment]::GetEnvironmentVariable('NUCLEAR_RENDERER_SOAK_SECONDS', 'Process')
$logStopThresholdBytes = 10MB
$process = $null
$runnerError = $null
$timedOut = $false
$exitCode = $null

function Get-InputManifest {
    $files = [Collections.Generic.List[IO.FileInfo]]::new()
    foreach ($relative in @(
        'nuclear-app\package.json',
        'nuclear-app\package-lock.json',
        'nuclear-app\svelte.config.js',
        'nuclear-app\vite.config.js',
        'nuclear-app\vitest.config.ts',
        'nuclear-app\jsconfig.json',
        'nuclear-app\src\routes\page-lifecycle-soak.test.ts',
        'scripts\run-renderer-soak.ps1'
    )) {
        $path = Join-Path $repoRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required soak input is missing: $relative" }
        $files.Add((Get-Item -LiteralPath $path))
    }
    foreach ($directory in @('nuclear-app\src', 'nuclear-app\static')) {
        $path = Join-Path $repoRoot $directory
        if (Test-Path -LiteralPath $path -PathType Container) {
            $rootDirectory = Get-Item -LiteralPath $path -Force
            if (($rootDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Required soak input tree is a reparse point: $directory"
            }
            $reparseDirectories = @(Get-ChildItem -LiteralPath $path -Recurse -Directory -Force |
                Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 })
            if ($reparseDirectories.Count -gt 0) {
                throw "Required soak input tree contains a reparse directory: $($reparseDirectories[0].FullName)"
            }
            Get-ChildItem -LiteralPath $path -Recurse -File -Force |
                ForEach-Object { $files.Add($_) }
        }
    }
    @($files | Sort-Object FullName -Unique | ForEach-Object {
        if (($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Required soak input is a reparse file: $($_.FullName)"
        }
        [ordered]@{
            path = [IO.Path]::GetRelativePath($repoRoot, $_.FullName).Replace('\', '/')
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
}

function Test-PositiveJsonInteger {
    param([AllowNull()][object]$Value)
    return (($Value -is [int]) -or ($Value -is [long])) -and [long]$Value -gt 0
}

function Stop-OwnedProcessTree {
    param([Diagnostics.Process]$OwnedProcess)
    if ($null -eq $OwnedProcess) { return }
    $OwnedProcess.Refresh()
    if (-not $OwnedProcess.HasExited) {
        $OwnedProcess.Kill($true)
        if (-not $OwnedProcess.WaitForExit(10000)) {
            throw 'The owned renderer soak process tree did not stop within 10 seconds.'
        }
    }
}

if (-not (Test-Path -LiteralPath $nodePath -PathType Leaf)) {
    throw "Pinned Node executable is missing: $nodePath"
}
if (-not (Test-Path -LiteralPath $vitestPath -PathType Leaf)) {
    throw "Vitest entrypoint is missing: $vitestPath"
}

New-Item -ItemType Directory -Path $runRoot | Out-Null
$beforeManifest = Get-InputManifest
$beforeManifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $beforeManifestPath -Encoding utf8
$beforeManifestHash = (Get-FileHash -LiteralPath $beforeManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$nodeHash = (Get-FileHash -LiteralPath $nodePath -Algorithm SHA256).Hash.ToLowerInvariant()
$nodeVersion = (& $nodePath --version).Trim()
if ($nodeVersion -cne 'v22.23.1') { throw "Unexpected pinned Node version: $nodeVersion" }
$gcSupport = (& $nodePath --expose-gc -p "typeof global.gc").Trim()
if ($LASTEXITCODE -ne 0 -or $gcSupport -cne 'function') {
    throw 'Pinned Node did not expose controlled GC with --expose-gc.'
}
$headCommit = (& git -C $repoRoot rev-parse HEAD).Trim()
$gitStatus = @(& git -C $repoRoot status --porcelain=v1)
$null = New-Item -ItemType Directory -Path $sourceArchiveRoot
foreach ($entry in $beforeManifest) {
    $source = Join-Path $repoRoot ([string]$entry.path).Replace('/', '\')
    $destination = Join-Path $sourceArchiveRoot ([string]$entry.path).Replace('/', '\')
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force
    Copy-Item -LiteralPath $source -Destination $destination
    $copied = Get-Item -LiteralPath $destination
    $copiedHash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($copied.Length -ne [long]$entry.bytes -or $copiedHash -cne [string]$entry.sha256) {
        throw "Archived soak input did not match its manifest entry: $($entry.path)"
    }
}
$windows = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$quotedVitestPath = '"' + $vitestPath + '"'
$arguments = @(
    '--expose-gc',
    $quotedVitestPath,
    'run',
    'src/routes/page-lifecycle-soak.test.ts',
    '--pool=forks',
    '--execArgv=--expose-gc',
    '--maxWorkers=1',
    '--disableConsoleIntercept'
)
$startedAt = [DateTimeOffset]::UtcNow
$deadline = $startedAt.AddSeconds($configuredSeconds + 60)

try {
    [Environment]::SetEnvironmentVariable(
        'NUCLEAR_RENDERER_SOAK_SECONDS',
        [string]$configuredSeconds,
        'Process'
    )
    $process = Start-Process `
        -FilePath $nodePath `
        -ArgumentList $arguments `
        -WorkingDirectory $appRoot `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -WindowStyle Hidden `
        -PassThru
    while (-not $process.HasExited) {
        $remainingMs = [Math]::Floor(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
        if ($remainingMs -le 0) {
            $timedOut = $true
            $runnerError = 'Renderer soak exceeded its configured duration plus 60-second grace.'
            break
        }
        [void]$process.WaitForExit([int][Math]::Min(1000, [Math]::Max(1, $remainingMs)))
        $liveBytes = 0
        if (Test-Path -LiteralPath $stdoutPath) { $liveBytes += (Get-Item $stdoutPath).Length }
        if (Test-Path -LiteralPath $stderrPath) { $liveBytes += (Get-Item $stderrPath).Length }
        if ($liveBytes -gt $logStopThresholdBytes) {
            $runnerError = 'Renderer soak logs crossed the 10 MiB observed stop threshold during execution.'
            break
        }
    }
    if ($timedOut) { Stop-OwnedProcessTree -OwnedProcess $process }
    $process.Refresh()
    if ($process.HasExited) { $exitCode = $process.ExitCode }
    if ($null -eq $runnerError -and $exitCode -ne 0) {
        $runnerError = "Renderer soak exited with code $exitCode."
    }
}
catch {
    $runnerError = $_.Exception.Message
}
finally {
    try { Stop-OwnedProcessTree -OwnedProcess $process } catch {
        if ($null -eq $runnerError) { $runnerError = $_.Exception.Message }
    }
    [Environment]::SetEnvironmentVariable(
        'NUCLEAR_RENDERER_SOAK_SECONDS',
        $originalDuration,
        'Process'
    )
}

$endedAt = [DateTimeOffset]::UtcNow
$afterManifest = Get-InputManifest
$afterManifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $afterManifestPath -Encoding utf8
$afterManifestHash = (Get-FileHash -LiteralPath $afterManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$nodeHashAfter = (Get-FileHash -LiteralPath $nodePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($nodeHashAfter -cne $nodeHash -and $null -eq $runnerError) {
    $runnerError = 'The pinned Node executable changed during execution.'
}
$manifestUnchanged = $beforeManifestHash -ceq $afterManifestHash
if (-not $manifestUnchanged -and $null -eq $runnerError) {
    $runnerError = 'A renderer soak input changed during execution.'
}
$stdoutBytes = if (Test-Path -LiteralPath $stdoutPath) { (Get-Item $stdoutPath).Length } else { 0 }
$stderrBytes = if (Test-Path -LiteralPath $stderrPath) { (Get-Item $stderrPath).Length } else { 0 }
if (($stdoutBytes + $stderrBytes) -gt $logStopThresholdBytes -and $null -eq $runnerError) {
    $runnerError = 'Renderer soak logs crossed the 10 MiB observed stop threshold.'
}
$result = $null
if (Test-Path -LiteralPath $stdoutPath) {
    $resultLines = @(Get-Content -LiteralPath $stdoutPath | Where-Object { $_ -like 'NUCLEAR_RENDERER_SOAK_RESULT=*' })
    if ($resultLines.Count -eq 1) {
        $resultLine = $resultLines[0]
        try { $result = $resultLine.Substring('NUCLEAR_RENDERER_SOAK_RESULT='.Length) | ConvertFrom-Json }
        catch { if ($null -eq $runnerError) { $runnerError = 'Renderer soak result JSON was invalid.' } }
    }
    elseif ($null -eq $runnerError) { $runnerError = "Expected exactly one renderer soak result marker, found $($resultLines.Count)." }
}
if ($null -eq $result -and $null -eq $runnerError) { $runnerError = 'Renderer soak result was missing.' }
if ($null -ne $result -and $null -eq $runnerError) {
    $runnerElapsedMs = ($endedAt - $startedAt).TotalMilliseconds
    $validCycleCounts =
        (Test-PositiveJsonInteger $result.activeOperationCycles) -and
        (Test-PositiveJsonInteger $result.lateListenerCycles) -and
        (Test-PositiveJsonInteger $result.delayedStartupCycles) -and
        (Test-PositiveJsonInteger $result.completedWorkflowCycles) -and
        (Test-PositiveJsonInteger $result.mounts)
    $cycleSum = if ($validCycleCounts) {
        [long]$result.activeOperationCycles + [long]$result.lateListenerCycles +
            [long]$result.delayedStartupCycles + [long]$result.completedWorkflowCycles
    } else { -1 }
    if ([double]$result.configuredSeconds -ne $configuredSeconds -or
        -not [double]::IsFinite([double]$result.elapsedMs) -or
        [double]$result.elapsedMs -lt ($configuredSeconds * 1000) -or
        [double]$result.elapsedMs -gt (($configuredSeconds + 60) * 1000) -or
        [double]$result.elapsedMs -gt ($runnerElapsedMs + 5000) -or
        -not $validCycleCounts -or
        $cycleSum -ne [long]$result.mounts -or
        -not ($result.gcAvailable -is [bool]) -or
        $result.gcAvailable -ne $true) {
        $runnerError = 'Renderer soak metrics did not prove the configured duration and all four cycles.'
    }
}
$receipt = [ordered]@{
    schemaVersion = 1
    scope = 'Vitest jsdom with mocked Tauri IPC; no native renderer or backend qualification claim'
    status = if ($null -eq $runnerError) { 'passed' } else { 'failed' }
    error = $runnerError
    runId = $runId
    configuredSeconds = $configuredSeconds
    startedAtUnixMs = $startedAt.ToUnixTimeMilliseconds()
    endedAtUnixMs = $endedAt.ToUnixTimeMilliseconds()
    deadlineExceeded = $timedOut
    exitCode = $exitCode
    headCommit = $headCommit
    gitStatusPorcelain = $gitStatus
    host = [ordered]@{
        windowsDisplayVersion = [string]$windows.DisplayVersion
        windowsBuild = "{0}.{1}" -f $windows.CurrentBuildNumber, $windows.UBR
    }
    pinnedNode = [ordered]@{ path = $nodePath; version = $nodeVersion; sha256Before = $nodeHash; sha256After = $nodeHashAfter; unchanged = $nodeHash -ceq $nodeHashAfter; exposeGc = $true }
    command = [ordered]@{ executable = $nodePath; arguments = $arguments; workingDirectory = $appRoot }
    inputs = [ordered]@{
        before = $beforeManifestPath
        beforeSha256 = $beforeManifestHash
        after = $afterManifestPath
        afterSha256 = $afterManifestHash
        unchanged = $manifestUnchanged
        fileCount = $beforeManifest.Count
        archivedBytesRoot = $sourceArchiveRoot
    }
    logs = [ordered]@{ stdout = $stdoutPath; stdoutBytes = $stdoutBytes; stderr = $stderrPath; stderrBytes = $stderrBytes; observedStopThresholdBytes = $logStopThresholdBytes; monitoringIntervalMilliseconds = 1000 }
    metrics = $result
}
$receipt | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $receiptPath -Encoding utf8
Write-Host "RENDERER_SOAK_RUN=$runRoot"
Write-Host "RENDERER_SOAK_RECEIPT=$receiptPath"
if ($null -ne $runnerError) { throw "$runnerError Evidence remains at $runRoot." }
