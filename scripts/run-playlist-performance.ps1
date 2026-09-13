[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string] $Profile = 'release',

    [ValidateRange(1, 20)]
    [int] $Repeats = 3
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repositoryRoot 'nuclear-app\src-tauri\Cargo.toml'
$stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$runRoot = Join-Path $repositoryRoot "target\playlist-performance\$stamp-$([Guid]::NewGuid().ToString('N'))"
$buildLog = Join-Path $runRoot 'cargo-no-run.jsonl'
$frozenExecutable = Join-Path $runRoot 'nuclear-playlist-performance-tests.exe'
$testName = 'services::preparation::tests::synthetic_playlist_admission_meets_durable_registration_gates'

function Get-SourceFingerprint {
    Push-Location $repositoryRoot
    try {
        $paths = @(& git ls-files --cached --others --exclude-standard)
        if ($LASTEXITCODE -ne 0 -or $paths.Count -eq 0) {
            throw 'Could not enumerate the source tree for hashing.'
        }
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            foreach ($relativePath in ($paths | Sort-Object)) {
                $absolutePath = Join-Path $repositoryRoot $relativePath
                if (-not (Test-Path -LiteralPath $absolutePath -PathType Leaf)) { continue }
                $pathBytes = [Text.Encoding]::UTF8.GetBytes(($relativePath -replace '\\', '/') + "`n")
                [void] $sha.TransformBlock($pathBytes, 0, $pathBytes.Length, $pathBytes, 0)
                $content = [IO.File]::ReadAllBytes($absolutePath)
                [void] $sha.TransformBlock($content, 0, $content.Length, $content, 0)
            }
            [void] $sha.TransformFinalBlock([byte[]]::new(0), 0, 0)
            return [Convert]::ToHexString($sha.Hash).ToLowerInvariant()
        }
        finally {
            $sha.Dispose()
        }
    }
    finally {
        Pop-Location
    }
}

[IO.Directory]::CreateDirectory($runRoot) | Out-Null
$sourceHashBefore = Get-SourceFingerprint
Push-Location $repositoryRoot
try {
    $revision = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not record the Git revision.' }
    $cargoVersion = (& cargo --version).Trim()
    $rustcVersion = (& rustc --version).Trim()

    $buildArguments = @(
        'test', '--manifest-path', $manifestPath, '--locked', '--offline', '--all-features',
        '--no-run', '--message-format=json'
    )
    if ($Profile -eq 'release') { $buildArguments += '--release' }
    & cargo @buildArguments 2>&1 | Tee-Object -LiteralPath $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Cargo test compilation failed; evidence: $buildLog" }

    $executables = @(
        Get-Content -LiteralPath $buildLog | ForEach-Object {
            try {
                $record = $_ | ConvertFrom-Json -ErrorAction Stop
                if ($record.reason -eq 'compiler-artifact' -and
                    $record.target.name -eq 'nuclear_app_lib' -and
                    $record.profile.test -eq $true -and
                    $record.executable) {
                    [string] $record.executable
                }
            }
            catch {}
        } | Select-Object -Unique
    )
    if ($executables.Count -ne 1 -or -not (Test-Path -LiteralPath $executables[0] -PathType Leaf)) {
        throw "Expected one compiled nuclear_app_lib test executable, found $($executables.Count)."
    }
    Copy-Item -LiteralPath $executables[0] -Destination $frozenExecutable
    $executableHash = (Get-FileHash -LiteralPath $frozenExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $sourceHashAfterBuild = Get-SourceFingerprint
    if ($sourceHashAfterBuild -cne $sourceHashBefore) {
        throw 'The source tree changed during compilation; refusing to benchmark an unpinned candidate.'
    }

    $measurements = @()
    for ($repeat = 1; $repeat -le $Repeats; $repeat++) {
        $logPath = Join-Path $runRoot ("repeat-{0:D2}.log" -f $repeat)
        & $frozenExecutable $testName '--exact' '--ignored' '--nocapture' '--test-threads=1' 2>&1 |
            Tee-Object -LiteralPath $logPath
        if ($LASTEXITCODE -ne 0) { throw "Playlist benchmark repeat $repeat failed; evidence: $logPath" }
        $matches = @(Select-String -LiteralPath $logPath -Pattern '^synthetic_playlist_admission count=(\d+) elapsed_ms=(\d+) gate_ms=(\d+)$')
        if ($matches.Count -ne 3) {
            throw "Playlist benchmark repeat $repeat emitted $($matches.Count) metrics; expected 3."
        }
        foreach ($match in $matches) {
            $measurements += [ordered]@{
                repeat = $repeat
                count = [int] $match.Matches[0].Groups[1].Value
                elapsedMs = [int64] $match.Matches[0].Groups[2].Value
                gateMs = [int64] $match.Matches[0].Groups[3].Value
                passed = ([int64] $match.Matches[0].Groups[2].Value -le [int64] $match.Matches[0].Groups[3].Value)
                log = [IO.Path]::GetRelativePath($runRoot, $logPath)
            }
        }
    }

    $sourceHashAfterRuns = Get-SourceFingerprint
    if ($sourceHashAfterRuns -cne $sourceHashBefore) {
        throw 'The source tree changed during benchmark execution; evidence is not pinned.'
    }
    $summary = [ordered]@{
        schemaVersion = 1
        benchmark = 'synthetic-durable-playlist-admission'
        limitations = @('No network or extractor work', 'No renderer or end-to-end UI measurement')
        recordedAtUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        revision = $revision
        sourceSha256 = $sourceHashBefore
        executableSha256 = $executableHash
        executable = [IO.Path]::GetFileName($frozenExecutable)
        profile = $Profile
        repeats = $Repeats
        toolchain = [ordered]@{ cargo = $cargoVersion; rustc = $rustcVersion }
        buildLog = [IO.Path]::GetFileName($buildLog)
        measurements = $measurements
    }
    $summaryPath = Join-Path $runRoot 'summary.json'
    $summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath -Encoding utf8
    Write-Host "PLAYLIST_PERFORMANCE_RUN=$runRoot"
    Write-Host "PLAYLIST_PERFORMANCE_SUMMARY=$summaryPath"
}
finally {
    Pop-Location
}
