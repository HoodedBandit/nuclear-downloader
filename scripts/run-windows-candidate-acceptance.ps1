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
if ($env:GITHUB_ACTIONS -cne 'true' -or [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw 'Candidate acceptance must run on the disposable Windows account provided by GitHub Actions.'
}
Add-Type -Path (Join-Path $PSScriptRoot 'WindowsUserProcess.cs')
$integrityRid = [WindowsUserProcess]::IntegrityRid()
if ($integrityRid -ne 8192) {
    throw "Candidate acceptance requires Medium integrity (8192); found $integrityRid."
}
Write-Host "Verified normal-user acceptance token: integrity RID $integrityRid."

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$publishedResultsRoot = [System.IO.Path]::GetFullPath($ResultsDirectory)
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$inventoryPath = Join-Path $candidateRoot 'release-candidate-inventory.json'

& (Join-Path $PSScriptRoot 'verify-release-candidate.ps1') `
    -CandidateDirectory $candidateRoot `
    -ExpectedVersion $ExpectedVersion `
    -ExpectedCommitSha $ExpectedCommitSha | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw 'Candidate contract verification failed before acceptance.'
}

. (Join-Path $PSScriptRoot 'release-json.ps1')
$inventory = Read-BoundedReleaseJson -Path $inventoryPath -Limit 1MB
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

$acceptanceBase = [System.IO.Path]::GetFullPath($env:RUNNER_TEMP)
$acceptanceRoot = Join-Path $acceptanceBase "nuclear-acceptance-$([Guid]::NewGuid().ToString('N'))"
$acceptanceLeaf = [System.IO.Path]::GetFileName($acceptanceRoot)
if ($acceptanceLeaf -cnotmatch '^nuclear-acceptance-[0-9a-f]{32}$') {
    throw 'Refusing to create an ambiguously named acceptance workspace.'
}

$installRoot = Join-Path $acceptanceRoot 'installed'
$portableRoot = Join-Path $acceptanceRoot 'portable'
$fixtureRoot = Join-Path $acceptanceRoot 'fixture'
$resultsRoot = Join-Path $acceptanceRoot 'results-staging'
$ownershipMarker = Join-Path $acceptanceRoot '.nuclear-candidate-acceptance'
$serverReadyPath = Join-Path $fixtureRoot 'server.port'
$fixtureMediaPath = Join-Path $fixtureRoot 'fixture-video.mp4'
$localDataRoot = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
if ([string]::IsNullOrWhiteSpace($localDataRoot)) {
    throw 'Windows did not provide the disposable runner account local application-data folder.'
}
$localDataRoot = [System.IO.Path]::GetFullPath($localDataRoot)
$persistentAppDataRoot = Join-Path $localDataRoot 'Nuclear Downloader'
$managedAppDataRoot = Join-Path $localDataRoot 'NuclearDownloader'
$webviewDataRoot = Join-Path $localDataRoot 'com.mrw.nuclear'
$edgeDriverDataRoot = Join-Path $webviewDataRoot 'EBWebView'
foreach ($appDataRoot in @($persistentAppDataRoot, $managedAppDataRoot, $webviewDataRoot)) {
    if (Test-Path -LiteralPath $appDataRoot) {
        throw "The disposable runner account is not clean; refusing to reuse application data: $appDataRoot"
    }
}
$serverProcess = $null
$startedAt = [DateTimeOffset]::UtcNow
$steps = [ordered]@{}
$processInvocation = 0

. (Join-Path $PSScriptRoot 'windows-edgedriver.ps1')
. (Join-Path $PSScriptRoot 'acceptance-log-privacy.ps1')

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
        [void]$stdoutCopy.GetAwaiter().GetResult()
        [void]$stderrCopy.GetAwaiter().GetResult()
    } finally {
        try {
            $stdoutStream.Dispose()
            $stderrStream.Dispose()
            if ($process) { $process.Dispose() }
        } finally {
            Protect-AcceptanceLog -Path $stdoutPath -ProtectedValues @($protectedFixtureUrls)
            Protect-AcceptanceLog -Path $stderrPath -ProtectedValues @($protectedFixtureUrls)
        }
    }
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

function Get-WebDriverProcesses {
    @(
        Get-CimInstance Win32_Process | Where-Object {
            $_.Name -in @('tauri-driver.exe', 'msedgedriver.exe')
        }
    )
}

function Stop-ProcessTreeAndWait {
    param(
        [Parameter(Mandatory)] [int] $ProcessId,
        [Parameter(Mandatory)] [string] $Label
    )

    try {
        $process = [System.Diagnostics.Process]::GetProcessById($ProcessId)
    } catch [System.ArgumentException] {
        return
    }
    try {
        if (-not $process.HasExited) {
            try {
                $process.Kill($true)
            } catch [System.InvalidOperationException] {
                if (-not $process.HasExited) { throw }
            }
        }
        if (-not $process.WaitForExit(10000)) {
            throw "$Label process $ProcessId did not exit after tree termination."
        }
    } finally {
        $process.Dispose()
    }
}

function Stop-NewWebDriverProcesses {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [int[]] $BaselineProcessIds,
        [Parameter(Mandatory)] [string] $ExpectedTauriDriverPath,
        [Parameter(Mandatory)] [string] $ExpectedNativeDriverPath
    )

    Start-Sleep -Milliseconds 250
    $baseline = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($driverProcessId in $BaselineProcessIds) { [void]$baseline.Add($driverProcessId) }

    $newProcesses = @(Get-WebDriverProcesses | Where-Object { -not $baseline.Contains([int]$_.ProcessId) })
    foreach ($driver in @($newProcesses | Where-Object Name -CEQ 'tauri-driver.exe')) {
        $actualPath = [System.IO.Path]::GetFullPath([string]$driver.ExecutablePath)
        if ($actualPath -cne $ExpectedTauriDriverPath) {
            throw "Refusing to stop an unexpected tauri-driver executable: $actualPath"
        }
        Stop-ProcessTreeAndWait -ProcessId ([int]$driver.ProcessId) -Label 'tauri-driver'
    }

    Start-Sleep -Milliseconds 250

    $remaining = @(Get-WebDriverProcesses | Where-Object { -not $baseline.Contains([int]$_.ProcessId) })
    foreach ($driver in $remaining) {
        if ($driver.Name -cne 'msedgedriver.exe') {
            throw "Acceptance left an unexpected WebDriver process running: $($driver.Name) ($($driver.ProcessId))"
        }
        $actualPath = [System.IO.Path]::GetFullPath([string]$driver.ExecutablePath)
        if ($actualPath -cne $ExpectedNativeDriverPath) {
            throw "Refusing to stop an unexpected EdgeDriver executable: $actualPath"
        }
        Stop-ProcessTreeAndWait -ProcessId ([int]$driver.ProcessId) -Label 'msedgedriver'
    }

    $leaked = @(Get-WebDriverProcesses | Where-Object { -not $baseline.Contains([int]$_.ProcessId) })
    if ($leaked.Count -ne 0) {
        throw "Acceptance leaked WebDriver process IDs: $(($leaked.ProcessId -join ', '))"
    }
}

function Invoke-WdioSuite {
    param(
        [Parameter(Mandatory)] [string] $NodeExecutable,
        [Parameter(Mandatory)] [string] $WdioCli,
        [Parameter(Mandatory)] [string] $AppBinary,
        [Parameter(Mandatory)]
        [ValidateSet('full', 'interrupt', 'restart', 'smoke')]
        [string] $Suite,
        [Parameter(Mandatory)]
        [ValidatePattern('^wdio-[a-z0-9-]+$')]
        [string] $LogName,
        [Parameter(Mandatory)] [hashtable] $BaseEnvironment,
        [Parameter(Mandatory)] [string] $ExpectedTauriDriverPath,
        [Parameter(Mandatory)] [string] $ExpectedNativeDriverPath,
        [int] $TimeoutSeconds = 180
    )

    $environment = @{}
    foreach ($entry in $BaseEnvironment.GetEnumerator()) { $environment[$entry.Key] = $entry.Value }
    $environment.NUCLEAR_E2E_APP_BINARY = $AppBinary
    $environment.NUCLEAR_E2E_NATIVE_SUITE = $Suite
    $baselineProcessIds = @(
        Get-WebDriverProcesses | ForEach-Object { [int]$_.ProcessId }
    )
    try {
        Invoke-OwnedProcess `
            -FilePath $NodeExecutable `
            -LogName $LogName `
            -ArgumentList @($WdioCli, 'run', './e2e/wdio.native.conf.mjs') `
            -TimeoutSeconds $TimeoutSeconds `
            -Environment $environment `
            -WorkingDirectory $appRoot
    } finally {
        Stop-NewWebDriverProcesses `
            -BaselineProcessIds $baselineProcessIds `
            -ExpectedTauriDriverPath $ExpectedTauriDriverPath `
            -ExpectedNativeDriverPath $ExpectedNativeDriverPath
    }
}

function Invoke-InterruptedWdioSuite {
    param(
        [Parameter(Mandatory)] [string] $NodeExecutable,
        [Parameter(Mandatory)] [string] $WdioCli,
        [Parameter(Mandatory)] [string] $AppBinary,
        [Parameter(Mandatory)] [hashtable] $BaseEnvironment,
        [Parameter(Mandatory)] [string] $ExpectedTauriDriverPath,
        [Parameter(Mandatory)] [string] $ExpectedNativeDriverPath,
        [Parameter(Mandatory)] [string] $SentinelPath
    )

    $environment = @{}
    foreach ($entry in $BaseEnvironment.GetEnumerator()) { $environment[$entry.Key] = $entry.Value }
    $environment.NUCLEAR_E2E_APP_BINARY = $AppBinary
    $environment.NUCLEAR_E2E_NATIVE_SUITE = 'interrupt'
    $environment.NUCLEAR_E2E_INTERRUPT_SENTINEL = $SentinelPath

    $baselineDriverIds = @(Get-WebDriverProcesses | ForEach-Object { [int]$_.ProcessId })
    $canonicalApp = [System.IO.Path]::GetFullPath($AppBinary)
    $baselineAppIds = [System.Collections.Generic.HashSet[int]]::new()
    Get-CimInstance Win32_Process | Where-Object {
        $_.ExecutablePath -and [System.IO.Path]::GetFullPath([string]$_.ExecutablePath) -ceq $canonicalApp
    } | ForEach-Object { [void]$baselineAppIds.Add([int]$_.ProcessId) }

    $script:processInvocation++
    $logStem = '{0:D2}-wdio-installed-interrupt' -f $script:processInvocation
    $stdoutPath = Join-Path $resultsRoot "$logStem.stdout.log"
    $stderrPath = Join-Path $resultsRoot "$logStem.stderr.log"
    $stdoutStream = [System.IO.File]::Open($stdoutPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $stderrStream = [System.IO.File]::Open($stderrPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $NodeExecutable
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $appRoot
    foreach ($argument in @($WdioCli, 'run', './e2e/wdio.native.conf.mjs')) {
        [void]$start.ArgumentList.Add($argument)
    }
    foreach ($entry in $environment.GetEnumerator()) {
        $start.Environment[[string]$entry.Key] = [string]$entry.Value
    }

    $process = $null
    $stdoutCopy = $null
    $stderrCopy = $null
    $exitCode = $null
    try {
        $process = [System.Diagnostics.Process]::Start($start)
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderrStream)
        $readyDeadline = [DateTimeOffset]::UtcNow.AddMinutes(3)
        while (-not (Test-Path -LiteralPath $SentinelPath -PathType Leaf)) {
            if ($process.HasExited) {
                throw "Interruption seed exited before its active-download sentinel (exit $($process.ExitCode))."
            }
            if ([DateTimeOffset]::UtcNow -ge $readyDeadline) {
                throw 'Interruption seed did not reach an active download within three minutes.'
            }
            Start-Sleep -Milliseconds 100
        }

        $expectedTitle = [string]$environment.NUCLEAR_E2E_RESTART_TITLE
        if ([System.IO.File]::ReadAllText($SentinelPath, [System.Text.Encoding]::UTF8) -cne $expectedTitle) {
            throw 'Interruption sentinel did not identify the expected queue item.'
        }
        $appProcesses = @(
            Get-CimInstance Win32_Process | Where-Object {
                $_.ExecutablePath -and
                [System.IO.Path]::GetFullPath([string]$_.ExecutablePath) -ceq $canonicalApp -and
                -not $baselineAppIds.Contains([int]$_.ProcessId)
            }
        )
        if ($appProcesses.Count -ne 1) {
            throw "Expected one newly launched installed application process; found $($appProcesses.Count)."
        }
        Stop-ProcessTreeAndWait -ProcessId ([int]$appProcesses[0].ProcessId) -Label 'interrupted installed application'
        if (-not $process.WaitForExit(30000)) {
            try { $process.Kill($true) } catch { Write-Warning $_ }
            [void]$process.WaitForExit(10000)
        }
        $exitCode = $process.ExitCode
        if ($exitCode -eq 0) {
            throw 'Interruption WebDriver suite unexpectedly succeeded after the app process was forcibly terminated.'
        }
    } finally {
        if ($process -and -not $process.HasExited) {
            try { $process.Kill($true) } catch { Write-Warning $_ }
            try { [void]$process.WaitForExit(10000) } catch { Write-Warning $_ }
        }
        Get-CimInstance Win32_Process | Where-Object {
            $_.ExecutablePath -and
            [System.IO.Path]::GetFullPath([string]$_.ExecutablePath) -ceq $canonicalApp -and
            -not $baselineAppIds.Contains([int]$_.ProcessId)
        } | ForEach-Object {
            Stop-ProcessTreeAndWait -ProcessId ([int]$_.ProcessId) -Label 'interruption cleanup application'
        }
        try {
            Stop-NewWebDriverProcesses `
                -BaselineProcessIds $baselineDriverIds `
                -ExpectedTauriDriverPath $ExpectedTauriDriverPath `
                -ExpectedNativeDriverPath $ExpectedNativeDriverPath
        } finally {
            if ($stdoutCopy) { [void]$stdoutCopy.GetAwaiter().GetResult() }
            if ($stderrCopy) { [void]$stderrCopy.GetAwaiter().GetResult() }
            $stdoutStream.Dispose()
            $stderrStream.Dispose()
            if ($process) { $process.Dispose() }
            Protect-AcceptanceLog -Path $stdoutPath -ProtectedValues @($protectedFixtureUrls)
            Protect-AcceptanceLog -Path $stderrPath -ProtectedValues @($protectedFixtureUrls)
        }
    }
}

if (Test-Path -LiteralPath $publishedResultsRoot) {
    throw "Acceptance results directory already exists: $publishedResultsRoot"
}
New-Item -ItemType Directory -Path $acceptanceRoot, $installRoot, $portableRoot, $fixtureRoot, $resultsRoot | Out-Null
[System.IO.File]::WriteAllText($ownershipMarker, $ExpectedCommitSha, [System.Text.UTF8Encoding]::new($false))

$processEnvironment = @{
    NUCLEAR_E2E_FIXTURE_TITLE = 'fixture-video'
    NUCLEAR_E2E_COLLISION_STEM = "nuclear-collision-$ExpectedCandidateRunId"
}
$controlledSitePassed = @()
$controlledSiteMissing = @()
$protectedFixtureUrls = [System.Collections.Generic.List[string]]::new()

try {
    $controlledSiteDescriptors = @(
        [ordered]@{
            CaseId = 'youtube-maintainer-fixture'
            UrlVariable = 'NUCLEAR_E2E_YOUTUBE_FIXTURE_URL'
            IdVariable = 'NUCLEAR_E2E_YOUTUBE_FIXTURE_ID'
            AllowedHosts = @('youtube.com', 'www.youtube.com', 'youtu.be')
        },
        [ordered]@{
            CaseId = 'x-maintainer-fixture'
            UrlVariable = 'NUCLEAR_E2E_X_FIXTURE_URL'
            IdVariable = 'NUCLEAR_E2E_X_FIXTURE_ID'
            AllowedHosts = @('x.com', 'www.x.com', 'twitter.com', 'www.twitter.com')
        }
    )
    foreach ($descriptor in $controlledSiteDescriptors) {
        $fixtureUrl = [Environment]::GetEnvironmentVariable([string]$descriptor.UrlVariable)
        $fixtureId = [Environment]::GetEnvironmentVariable([string]$descriptor.IdVariable)
        $hasUrl = -not [string]::IsNullOrWhiteSpace($fixtureUrl)
        $hasId = -not [string]::IsNullOrWhiteSpace($fixtureId)
        if (-not $hasUrl -and -not $hasId) {
            $controlledSiteMissing += [string]$descriptor.CaseId
            continue
        }
        if ($hasUrl -ne $hasId) {
            throw "Controlled fixture $($descriptor.CaseId) requires both its protected URL and opaque ID variables."
        }
        if ($fixtureId -cnotmatch '^[a-z0-9][a-z0-9._-]{0,127}$') {
            throw "Controlled fixture $($descriptor.CaseId) has an invalid opaque fixture ID."
        }
        $parsedFixtureUrl = $null
        if ($fixtureUrl.Length -gt 4096 -or
            -not [Uri]::TryCreate($fixtureUrl, [UriKind]::Absolute, [ref]$parsedFixtureUrl) -or
            $parsedFixtureUrl.Scheme -cne 'https' -or
            -not [string]::IsNullOrEmpty($parsedFixtureUrl.UserInfo) -or
            $parsedFixtureUrl.Host -cnotin @($descriptor.AllowedHosts)) {
            throw "Controlled fixture $($descriptor.CaseId) has an invalid protected site URL."
        }
        $protectedFixtureUrls.Add($fixtureUrl)
        $processEnvironment[[string]$descriptor.UrlVariable] = $fixtureUrl
        $processEnvironment[[string]$descriptor.IdVariable] = $fixtureId
        $controlledSitePassed += [ordered]@{
            caseId = [string]$descriptor.CaseId
            fixtureId = $fixtureId
        }
    }

    Expand-Archive -LiteralPath $portablePath -DestinationPath $portableRoot
    $portableExecutable = Join-Path $portableRoot 'nuclear.exe'
    $ffmpegExecutable = Join-Path $portableRoot 'ffmpeg.exe'
    $ytDlpExecutable = Join-Path $portableRoot 'yt-dlp.exe'
    foreach ($required in @($portableExecutable, $ffmpegExecutable, $ytDlpExecutable)) {
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
    $processEnvironment.NUCLEAR_E2E_PLAYLIST_FIXTURE_URL = "http://127.0.0.1:$fixturePort/generic-playlist.html"
    $processEnvironment.NUCLEAR_E2E_FIXTURE_FILE = $fixtureMediaPath
    $playlistInvocation = $processInvocation + 1
    $playlistLogStem = '{0:D2}-playlist-validation' -f $playlistInvocation
    $playlistJsonPath = Join-Path $resultsRoot "$playlistLogStem.stdout.log"
    $playlistErrorPath = Join-Path $resultsRoot "$playlistLogStem.stderr.log"
    try {
        Invoke-OwnedProcess -FilePath $ytDlpExecutable -LogName 'playlist-validation' -TimeoutSeconds 60 -ArgumentList @(
            '--no-warnings', '--flat-playlist', '--dump-single-json',
            $processEnvironment.NUCLEAR_E2E_PLAYLIST_FIXTURE_URL
        )
        $playlistJsonItem = Get-Item -LiteralPath $playlistJsonPath
        if ($playlistJsonItem.Length -le 0 -or $playlistJsonItem.Length -gt 1MB) {
            throw 'Pinned yt-dlp generic-playlist validation returned invalid bounded output.'
        }
        $playlist = Get-Content -Raw -LiteralPath $playlistJsonPath | ConvertFrom-Json -AsHashtable
        if ([string]$playlist._type -cne 'playlist' -or @($playlist.entries).Count -ne 2) {
            throw 'Pinned yt-dlp did not recognize the loopback HTML as an exact two-entry generic playlist.'
        }
    } finally {
        foreach ($validationLog in @($playlistJsonPath, $playlistErrorPath)) {
            if (Test-Path -LiteralPath $validationLog -PathType Leaf) {
                [System.IO.File]::WriteAllText(
                    $validationLog,
                    "Loopback generic-playlist validation output intentionally omitted.`n",
                    [System.Text.UTF8Encoding]::new($false)
                )
            }
        }
    }
    $steps.genericPlaylistValidated = 'passed'
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
    $wdioEnvironment.NUCLEAR_E2E_WEBVIEW_DATA_FOLDER = $edgeDriverDataRoot
    $webView2RuntimeVersion = Get-WebView2RuntimeVersion
    $edgeDriver = Install-CompatibleEdgeDriver `
        -DestinationDirectory (Join-Path $acceptanceRoot 'webdriver') `
        -RuntimeVersion $webView2RuntimeVersion
    $edgeDriverPath = [string]$edgeDriver.Path
    $edgeDriverVersion = [string]$edgeDriver.Version
    $wdioEnvironment.NUCLEAR_E2E_NATIVE_DRIVER_PATH = $edgeDriverPath
    $wdioCli = Join-Path $appRoot 'node_modules\@wdio\cli\bin\wdio.js'
    if (-not (Test-Path -LiteralPath $wdioCli -PathType Leaf)) {
        throw 'Pinned WebdriverIO CLI is missing; run npm ci before candidate acceptance.'
    }
    $tauriDriverExecutable = [System.IO.Path]::GetFullPath(
        (Get-Command tauri-driver.exe -ErrorAction Stop).Source
    )
    Invoke-WdioSuite -NodeExecutable $nodeExecutable -WdioCli $wdioCli `
        -AppBinary $installedExecutable -Suite 'full' -LogName 'wdio-installed-full' `
        -BaseEnvironment $wdioEnvironment -ExpectedTauriDriverPath $tauriDriverExecutable `
        -ExpectedNativeDriverPath $edgeDriverPath `
        -TimeoutSeconds 1800
    $installedPath = [System.IO.Path]::GetFullPath($installedExecutable)
    $lingeringInstalled = @(
        Get-CimInstance Win32_Process | Where-Object {
            $_.ExecutablePath -and
            [System.IO.Path]::GetFullPath([string]$_.ExecutablePath) -ceq $installedPath
        }
    )
    if ($lingeringInstalled.Count -ne 0) {
        throw 'The installed application did not complete graceful shutdown after the full native suite.'
    }
    $steps.installedMp4RetryCollisionPlaylistCancelAllRuntimeChecks = 'passed'

    $journalPath = Join-Path $persistentAppDataRoot 'state-v1.dpapi'
    if (-not (Test-Path -LiteralPath $journalPath -PathType Leaf)) {
        throw 'The backend journal was not persisted after the fixture lifecycle.'
    }

    $interruptSentinel = Join-Path $fixtureRoot 'active-download.ready'
    $wdioEnvironment.NUCLEAR_E2E_RESTART_TITLE = "nuclear-interrupted-$ExpectedCandidateRunId"
    Invoke-InterruptedWdioSuite -NodeExecutable $nodeExecutable -WdioCli $wdioCli `
        -AppBinary $installedExecutable -BaseEnvironment $wdioEnvironment `
        -ExpectedTauriDriverPath $tauriDriverExecutable -ExpectedNativeDriverPath $edgeDriverPath `
        -SentinelPath $interruptSentinel
    $steps.forcedActiveProcessTermination = 'passed'

    Invoke-WdioSuite -NodeExecutable $nodeExecutable -WdioCli $wdioCli `
        -AppBinary $installedExecutable -Suite 'restart' -LogName 'wdio-installed-restart' `
        -BaseEnvironment $wdioEnvironment -ExpectedTauriDriverPath $tauriDriverExecutable `
        -ExpectedNativeDriverPath $edgeDriverPath
    $steps.interruptedOperationRestartRecovery = 'passed'

    Invoke-WdioSuite -NodeExecutable $nodeExecutable -WdioCli $wdioCli `
        -AppBinary $portableExecutable -Suite 'smoke' -LogName 'wdio-portable-smoke' `
        -BaseEnvironment $wdioEnvironment -ExpectedTauriDriverPath $tauriDriverExecutable `
        -ExpectedNativeDriverPath $edgeDriverPath
    $steps.portableStartup = 'passed'

    Invoke-OwnedProcess -FilePath $uninstaller -LogName 'nsis-uninstall' -ArgumentList @('/S') -TimeoutSeconds 300
    # The uninstaller can hand off to a temporary process. Require the actual
    # installed files to disappear, not merely the launcher process to exit.
    $uninstallDeadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $installedExecutable -PathType Leaf) -and
        [DateTimeOffset]::UtcNow -lt $uninstallDeadline) {
        Start-Sleep -Milliseconds 100
    }
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

    $manualAcceptanceRequired = @(
        'clean-windows11-installer',
        'clean-windows11-portable',
        'youtube-maintainer-fixture',
        'x-maintainer-fixture',
        'dedicated-account-cookie-login',
        'signed-app-update',
        'signed-runtime-update-rollback'
    )
    $result = [ordered]@{
        schemaVersion = 1
        releaseVersion = $ExpectedVersion
        sourceCommit = $ExpectedCommitSha
        candidateRunId = $ExpectedCandidateRunId
        candidateCreatedAt = [string]$inventory.createdAt
        startedAt = $startedAt.ToString('yyyy-MM-ddTHH:mm:ssZ')
        completedAt = [DateTimeOffset]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
        qualificationStatus = 'incomplete'
        incompleteRequirements = @($manualAcceptanceRequired)
        extractorQualificationStatus = if ($controlledSiteMissing.Count -eq 0) { 'complete' } else { 'incomplete' }
        controlledSiteFixtures = [ordered]@{
            passed = @($controlledSitePassed)
            missing = @($controlledSiteMissing)
        }
        os = [ordered]@{
            description = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
            architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        }
        webdriver = [ordered]@{
            integrityRid = $integrityRid
            webView2RuntimeVersion = $webView2RuntimeVersion
            edgeDriverVersion = $edgeDriverVersion
        }
        steps = $steps
        manualAcceptanceRequired = @($manualAcceptanceRequired)
        candidateAssets = $inventory.assets
    }
    $resultJson = $result | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText(
        (Join-Path $resultsRoot 'windows-x64-acceptance.json'),
        $resultJson,
        [System.Text.UTF8Encoding]::new($false)
    )
    Write-Output (Join-Path $publishedResultsRoot 'windows-x64-acceptance.json')
} finally {
    if ($serverProcess -and -not $serverProcess.HasExited) {
        try { $serverProcess.Kill($true) } catch { Write-Warning $_ }
        try { [void]$serverProcess.WaitForExit(10000) } catch { Write-Warning $_ }
    }
    $publicationFailure = $null
    if (Test-Path -LiteralPath $resultsRoot -PathType Container) {
        try {
            Publish-SafeAcceptanceEvidence `
                -StagingDirectory $resultsRoot `
                -PublishedDirectory $publishedResultsRoot `
                -ProtectedValues @($protectedFixtureUrls)
        } catch {
            $publicationFailure = $_
        }
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
    if ($publicationFailure) {
        throw "Acceptance evidence was not retained because protected-log finalization failed: $($publicationFailure.Exception.Message)"
    }
}
