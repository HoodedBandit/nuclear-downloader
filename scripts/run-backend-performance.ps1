[CmdletBinding()]
param(
    [ValidateSet('baseline', 'after')]
    [string]$Label = 'baseline',

    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repoRoot 'nuclear-app\src-tauri\Cargo.toml'
$performanceRoot = Join-Path $repoRoot 'target\performance'
$temporaryParent = Join-Path $repoRoot 'nuclear-app\src-tauri\target\performance-temp'
$runStamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$runId = [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path $performanceRoot "$Label-$runStamp-$runId"
$temporaryRoot = Join-Path $temporaryParent $runId
$ownerMarker = Join-Path $temporaryRoot '.nuclear-phase4-performance-runner'

$trackedEnvironment = @(
    'TEMP',
    'TMP',
    'NUCLEAR_PERF_QUEUE_SIZE',
    'NUCLEAR_PERF_LABEL',
    'NUCLEAR_PERF_PROFILE',
    'NUCLEAR_PERF_EVIDENCE_PATH'
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

function Invoke-PerformanceTest {
    param(
        [Parameter(Mandatory = $true)][string]$Filter,
        [Parameter(Mandatory = $true)][string]$EvidencePath,
        [AllowNull()][string]$QueueSize
    )

    Set-ProcessEnvironment -Name 'NUCLEAR_PERF_QUEUE_SIZE' -Value $QueueSize
    Set-ProcessEnvironment -Name 'NUCLEAR_PERF_EVIDENCE_PATH' -Value $EvidencePath

    $cargoArguments = @(
        'test',
        '--manifest-path', $manifestPath,
        '--locked',
        '--offline',
        '--all-features'
    )
    if ($Profile -eq 'release') {
        $cargoArguments += '--release'
    }
    $cargoArguments += @(
        $Filter,
        '--',
        '--ignored',
        '--nocapture',
        '--test-threads=1'
    )

    Write-Host "Running $Filter (queueSize=$QueueSize, profile=$Profile)..."
    & cargo @cargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Performance test '$Filter' failed with exit code $LASTEXITCODE."
    }
    if (-not (Test-Path -LiteralPath $EvidencePath -PathType Leaf)) {
        throw "Performance test '$Filter' did not create $EvidencePath."
    }
}

$completed = $false
try {
    New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $temporaryRoot -Force | Out-Null
    Set-Content -LiteralPath $ownerMarker -Value 'owned phase4 runner fixture' -NoNewline

    Set-ProcessEnvironment -Name 'TEMP' -Value $temporaryRoot
    Set-ProcessEnvironment -Name 'TMP' -Value $temporaryRoot
    Set-ProcessEnvironment -Name 'NUCLEAR_PERF_LABEL' -Value $Label
    Set-ProcessEnvironment -Name 'NUCLEAR_PERF_PROFILE' -Value $Profile

    $windows = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $processorName = [Microsoft.Win32.Registry]::GetValue(
        'HKEY_LOCAL_MACHINE\HARDWARE\DESCRIPTION\System\CentralProcessor\0',
        'ProcessorNameString',
        $null
    )
    $repoDrive = [System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($repoRoot))
    $hostEvidence = [ordered]@{
        schemaVersion = 1
        label = $Label
        profile = $Profile
        benchmark = 'host'
        recordedAtUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        metrics = [ordered]@{
            windowsDisplayVersion = [string]$windows.DisplayVersion
            windowsBuild = "{0}.{1}" -f $windows.CurrentBuildNumber, $windows.UBR
            processorName = ([string]$processorName).Trim()
            logicalProcessors = [Environment]::ProcessorCount
            repositoryVolumeFormat = $repoDrive.DriveFormat
            repositoryVolumeAvailableBytes = $repoDrive.AvailableFreeSpace
        }
    }
    $hostEvidence | ConvertTo-Json -Depth 5 -Compress |
        Set-Content -LiteralPath (Join-Path $runRoot 'host.json') -Encoding utf8

    Push-Location $repoRoot
    try {
        foreach ($queueSize in @(1, 100, 1000)) {
            $evidencePath = Join-Path $runRoot ("state-journal-q{0:D4}.json" -f $queueSize)
            Invoke-PerformanceTest `
                -Filter 'performance_harness::phase4_baseline' `
                -EvidencePath $evidencePath `
                -QueueSize ([string]$queueSize)
        }

        $runtimeEvidencePath = Join-Path $runRoot 'runtime-hash.json'
        Invoke-PerformanceTest `
            -Filter 'phase4_hash_baseline' `
            -EvidencePath $runtimeEvidencePath `
            -QueueSize $null
    }
    finally {
        Pop-Location
    }

    $records = Get-ChildItem -LiteralPath $runRoot -Filter '*.json' -File |
        Sort-Object Name |
        ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json }
    $summaryPath = Join-Path $runRoot 'summary.json'
    $records | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $summaryPath -Encoding utf8
    Write-Host "PHASE4_PERFORMANCE_RUN=$runRoot"
    Write-Host "PHASE4_PERFORMANCE_SUMMARY=$summaryPath"
    $completed = $true
}
finally {
    foreach ($name in $trackedEnvironment) {
        Set-ProcessEnvironment -Name $name -Value $originalEnvironment[$name]
    }

    if ($completed -and (Test-Path -LiteralPath $temporaryRoot -PathType Container)) {
        $resolvedParent = [System.IO.Path]::GetFullPath($temporaryParent).TrimEnd('\') + '\'
        $resolvedTemporary = [System.IO.Path]::GetFullPath($temporaryRoot)
        $owned = Test-Path -LiteralPath $ownerMarker -PathType Leaf
        if ($owned -and $resolvedTemporary.StartsWith($resolvedParent, [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force
        }
        else {
            Write-Warning "Preserving unverified temporary path: $resolvedTemporary"
        }
    }
    elseif (-not $completed) {
        Write-Warning "Performance run failed; preserving evidence and fixtures at $runRoot and $temporaryRoot."
    }
}
