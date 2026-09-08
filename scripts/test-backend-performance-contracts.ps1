[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$comparator = Join-Path $repositoryRoot 'scripts\compare-backend-performance.ps1'
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "nuclear-performance-contracts-$([Guid]::NewGuid().ToString('N'))"
$testMarker = Join-Path $testRoot '.nuclear-performance-contract-test-owned'
$utf8NoBom = [Text.UTF8Encoding]::new($false)

function Assert-True {
    param(
        [Parameter(Mandatory)][bool]$Condition,
        [Parameter(Mandatory)][string]$Message
    )
    if (-not $Condition) {
        throw $Message
    }
}

function New-LatencyMetric {
    param(
        [Parameter(Mandatory)][int]$Samples,
        [Parameter(Mandatory)][double]$P99
    )
    return [ordered]@{ samples = $Samples; p50 = 100.0; p95 = 200.0; p99 = $P99 }
}

function New-StateRecord {
    param(
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)][int]$QueueSize,
        [Parameter(Mandatory)][double]$LatencyP99,
        [Parameter(Mandatory)][long]$WorkingSetAfterSamples
    )
    $workload = [ordered]@{
        queueSize = $QueueSize
        snapshotSamples = 4
        snapshotWireSamples = 4
        durableCommandSamples = 3
        journalSamples = 2
    }
    return [ordered]@{
        schemaVersion = 1
        label = $Label
        profile = 'debug'
        benchmark = 'state_journal'
        workload = $workload
        metrics = [ordered]@{
            latencyMicros = [ordered]@{
                snapshot = New-LatencyMetric -Samples 4 -P99 $LatencyP99
                snapshotWire = New-LatencyMetric -Samples 4 -P99 $LatencyP99
                durableCommand = New-LatencyMetric -Samples 3 -P99 $LatencyP99
                journalSave = New-LatencyMetric -Samples 2 -P99 $LatencyP99
                enqueueBatch = New-LatencyMetric -Samples 1 -P99 $LatencyP99
            }
            blockedJournalSnapshot = [ordered]@{
                observedPreCommitState = $true
                latencyMicros = 100.0
            }
            eventBacklog = [ordered]@{
                deltaCount = $QueueSize * 2
                serializedBytes = $QueueSize * 100
                sequencesContiguous = $true
            }
            outbox = [ordered]@{
                queuedBatches = 1
                queuedDeltas = 1
                estimatedBytes = 100
                coalescedResyncs = 0
                limits = [ordered]@{
                    maxBatches = 100
                    maxDeltas = 100
                    maxEstimatedBytes = 10000
                }
            }
            memoryBytes = [ordered]@{
                workingSetAfterLoad = 10MB
                workingSetAfterSamples = $WorkingSetAfterSamples
                privateAfterLoad = 10MB
                privateAfterSamples = 12MB
            }
            storageBytes = [ordered]@{
                snapshotJson = 100
                journalAfterDurable = 200
                journalAfterEnqueue = 300
            }
        }
    }
}

function Write-PerformanceRun {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Label,
        [int]$RuntimeIterations = 10,
        [double]$LatencyP99 = 1000.0,
        [long]$WorkingSetAfterSamples = 12MB
    )
    New-Item -ItemType Directory -Path $Path | Out-Null
    $records = @(
        [ordered]@{
            schemaVersion = 1
            label = $Label
            profile = 'debug'
            benchmark = 'host'
            metrics = [ordered]@{
                windowsDisplayVersion = 'fixture'
                windowsBuild = '26100'
                processorName = 'fixture-cpu'
                logicalProcessors = 8
                repositoryVolumeFormat = 'NTFS'
                repositoryVolumeAvailableBytes = 100GB
            }
        },
        [ordered]@{
            schemaVersion = 1
            label = $Label
            profile = 'debug'
            benchmark = 'runtime_hash'
            workload = [ordered]@{
                iterations = $RuntimeIterations
                toolNames = @('yt-dlp', 'ffmpeg')
            }
            metrics = [ordered]@{
                hashes = [ordered]@{
                    totalInvocations = 5
                    totalBytes = 500
                    manifestInvocations = 3
                    manifestBytes = 300
                    toolInvocations = 2
                    toolBytes = 200
                }
                leases = [ordered]@{
                    resolutionCalls = 10
                    successfulResolutions = 10
                }
            }
        },
        (New-StateRecord -Label $Label -QueueSize 1 -LatencyP99 $LatencyP99 -WorkingSetAfterSamples $WorkingSetAfterSamples),
        (New-StateRecord -Label $Label -QueueSize 100 -LatencyP99 $LatencyP99 -WorkingSetAfterSamples $WorkingSetAfterSamples),
        (New-StateRecord -Label $Label -QueueSize 1000 -LatencyP99 $LatencyP99 -WorkingSetAfterSamples $WorkingSetAfterSamples)
    )
    Assert-True -Condition ($records.Count -eq 5) -Message "Fixture construction returned $($records.Count) records instead of five."
    $names = @('0-host.json', '1-runtime.json', '2-state-1.json', '3-state-100.json', '4-state-1000.json')
    for ($index = 0; $index -lt $records.Count; $index++) {
        [IO.File]::WriteAllText(
            (Join-Path $Path $names[$index]),
            ($records[$index] | ConvertTo-Json -Depth 20 -Compress),
            $utf8NoBom
        )
    }
    [IO.File]::WriteAllText(
        (Join-Path $Path 'summary.json'),
        ($records | ConvertTo-Json -Depth 20 -Compress),
        $utf8NoBom
    )
}

function Assert-Rejected {
    param(
        [Parameter(Mandatory)][scriptblock]$Action,
        [Parameter(Mandatory)][string]$ExpectedMessage
    )
    $message = $null
    try {
        & $Action
    }
    catch {
        $message = $_.Exception.Message
    }
    Assert-True -Condition (-not [string]::IsNullOrWhiteSpace($message)) -Message "Expected rejection containing: $ExpectedMessage"
    Assert-True -Condition $message.Contains($ExpectedMessage) -Message "Unexpected rejection: $message"
}

try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    [IO.File]::WriteAllText($testMarker, 'owned', $utf8NoBom)

    foreach ($scriptPath in @($comparator, $PSCommandPath)) {
        $tokens = $null
        $parseErrors = $null
        [Management.Automation.Language.Parser]::ParseFile(
            $scriptPath,
            [ref]$tokens,
            [ref]$parseErrors
        ) | Out-Null
        Assert-True -Condition ($parseErrors.Count -eq 0) -Message "PowerShell parser errors in $scriptPath"
    }

    $before = Join-Path $testRoot 'before-after-label'
    $after = Join-Path $testRoot 'after-after-label'
    Write-PerformanceRun -Path $before -Label 'after'
    Write-PerformanceRun -Path $after -Label 'after'
    $fixtureRecords = @(Get-Content -LiteralPath (Join-Path $before 'summary.json') -Raw | ConvertFrom-Json)
    Assert-True -Condition ($fixtureRecords.Count -eq 5) -Message "Fixture summary parsed as $($fixtureRecords.Count) records instead of five."
    $comparisonRoot = Join-Path $testRoot 'valid-comparison'
    & $comparator -Baseline $before -After $after -ComparisonMode maintainability -OutputRoot $comparisonRoot *> $null
    $comparison = Get-Content -LiteralPath (Join-Path $comparisonRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition ([string]$comparison.comparisonMode -ceq 'maintainability') -Message 'Maintainability mode was not recorded.'
    Assert-True -Condition ([string]$comparison.hardGateStatus -ceq 'passed') -Message 'Matched fixture hard gates did not pass.'
    Assert-True -Condition ([string]$comparison.performanceReviewStatus -ceq 'passed') -Message 'Matched evidence did not report a passed performance review status.'
    Assert-True -Condition (-not [bool]$comparison.performanceReviewRequired) -Message 'Identical evidence incorrectly required performance review.'

    $legacyBaseline = Join-Path $testRoot 'legacy-baseline-label'
    Write-PerformanceRun -Path $legacyBaseline -Label 'baseline'
    $legacyRoot = Join-Path $testRoot 'legacy-comparison'
    & $comparator -Baseline $legacyBaseline -After $after -OutputRoot $legacyRoot *> $null
    $legacy = Get-Content -LiteralPath (Join-Path $legacyRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition ([string]$legacy.comparisonMode -ceq 'overhaul') -Message 'Default comparison mode changed.'

    $legacyPositive = Join-Path $testRoot 'legacy-positive-after'
    Write-PerformanceRun -Path $legacyPositive -Label 'after' -LatencyP99 1001.0
    $legacyPositiveRoot = Join-Path $testRoot 'legacy-positive-comparison'
    & $comparator -Baseline $legacyBaseline -After $legacyPositive -OutputRoot $legacyPositiveRoot *> $null
    $legacyPositiveResult = Get-Content -LiteralPath (Join-Path $legacyPositiveRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition ([bool]$legacyPositiveResult.performanceReviewRequired) -Message 'Default overhaul mode stopped flagging a positive timing change.'
    Assert-True -Condition ([string]$legacyPositiveResult.note -ceq 'Timing changes are informational and never determine hardGateStatus.') -Message 'Default overhaul informational wording changed.'

    Assert-Rejected -ExpectedMessage "Expected label 'baseline'" -Action {
        & $comparator -Baseline $before -After $after -OutputRoot (Join-Path $testRoot 'wrong-mode') *> $null
    }

    $changedWorkload = Join-Path $testRoot 'changed-workload'
    Write-PerformanceRun -Path $changedWorkload -Label 'after' -RuntimeIterations 11
    Assert-Rejected -ExpectedMessage 'Runtime benchmark workloads did not match.' -Action {
        & $comparator -Baseline $before -After $changedWorkload -ComparisonMode maintainability -OutputRoot (Join-Path $testRoot 'wrong-workload') *> $null
    }

    $smallBefore = Join-Path $testRoot 'small-threshold-before'
    $smallAfter = Join-Path $testRoot 'small-threshold-after'
    Write-PerformanceRun -Path $smallBefore -Label 'after' -LatencyP99 100.0 -WorkingSetAfterSamples 4MB
    Write-PerformanceRun -Path $smallAfter -Label 'after' -LatencyP99 150.0 -WorkingSetAfterSamples (4MB + 1MB)
    $smallRoot = Join-Path $testRoot 'small-threshold-comparison'
    & $comparator -Baseline $smallBefore -After $smallAfter -ComparisonMode maintainability -OutputRoot $smallRoot *> $null
    $small = Get-Content -LiteralPath (Join-Path $smallRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition (-not [bool]$small.performanceReviewRequired) -Message 'Relative-only changes incorrectly crossed the absolute review thresholds.'

    $zeroBefore = Join-Path $testRoot 'zero-before'
    $zeroAfter = Join-Path $testRoot 'zero-after'
    Write-PerformanceRun -Path $zeroBefore -Label 'after' -LatencyP99 0.0 -WorkingSetAfterSamples 0
    Write-PerformanceRun -Path $zeroAfter -Label 'after' -LatencyP99 101.0 -WorkingSetAfterSamples 3MB
    $zeroRoot = Join-Path $testRoot 'zero-comparison'
    & $comparator -Baseline $zeroBefore -After $zeroAfter -ComparisonMode maintainability -OutputRoot $zeroRoot *> $null
    $zero = Get-Content -LiteralPath (Join-Path $zeroRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition ([bool]$zero.performanceReviewRequired) -Message 'Positive-from-zero regression did not require review.'
    Assert-True -Condition ([int]$zero.timingReviewCount -gt 0) -Message 'Positive-from-zero latency was not counted.'
    Assert-True -Condition ([int]$zero.memoryReviewCount -gt 0) -Message 'Positive-from-zero memory was not counted.'

    $regressed = Join-Path $testRoot 'threshold-regression'
    Write-PerformanceRun -Path $regressed -Label 'after' -LatencyP99 1201.0 -WorkingSetAfterSamples 15MB
    $regressionRoot = Join-Path $testRoot 'regression-comparison'
    & $comparator -Baseline $before -After $regressed -ComparisonMode maintainability -OutputRoot $regressionRoot *> $null
    $regression = Get-Content -LiteralPath (Join-Path $regressionRoot 'comparison.json') -Raw | ConvertFrom-Json
    Assert-True -Condition ([bool]$regression.performanceReviewRequired) -Message 'Threshold regression did not require review.'
    Assert-True -Condition ([string]$regression.performanceReviewStatus -ceq 'review_required') -Message 'Threshold regression did not report review_required.'
    Assert-True -Condition ([int]$regression.timingReviewCount -gt 0) -Message 'Latency threshold regression was not counted.'
    Assert-True -Condition ([int]$regression.memoryReviewCount -gt 0) -Message 'Memory threshold regression was not counted.'
    Assert-True -Condition ([string]$regression.hardGateStatus -ceq 'passed') -Message 'Review-only regression changed hard-gate status.'

    Write-Host 'Backend performance contract tests passed (8 cases).'
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        $resolvedRoot = [IO.Path]::GetFullPath($testRoot)
        $expectedParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        $item = Get-Item -LiteralPath $resolvedRoot -Force
        if (-not $resolvedRoot.StartsWith($expectedParent, [StringComparison]::OrdinalIgnoreCase) -or
            -not $item.Name.StartsWith('nuclear-performance-contracts-', [StringComparison]::Ordinal) -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not (Test-Path -LiteralPath $testMarker -PathType Leaf) -or
            (Get-Content -LiteralPath $testMarker -Raw) -cne 'owned') {
            throw 'Refusing to remove an unverified performance-contract fixture directory.'
        }
        Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
    }
}
