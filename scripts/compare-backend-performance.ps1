[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Baseline,

    [Parameter(Mandatory = $true)]
    [string]$After,

    [string]$OutputRoot,

    [Nullable[long]]$MaxEventBacklogBytes = $null,

    [Nullable[long]]$MaxPrivateGrowthBytes = $null,

    [Nullable[long]]$MaxWorkingSetGrowthBytes = $null,

    [Nullable[long]]$MaxJournalBytes = $null
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Require-Evidence {
    param(
        [Parameter(Mandatory = $true)][bool]$Condition,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if (-not $Condition) {
        throw $Message
    }
}

function Get-CanonicalJson {
    param([Parameter(Mandatory = $true)]$Value)
    return ($Value | ConvertTo-Json -Depth 30 -Compress)
}

function Read-PerformanceRun {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedLabel
    )

    $resolved = (Resolve-Path -LiteralPath $Path).Path
    Require-Evidence -Condition (Test-Path -LiteralPath $resolved -PathType Container) `
        -Message "Performance run is not a directory: $resolved"
    $summaryPath = Join-Path $resolved 'summary.json'
    Require-Evidence -Condition (Test-Path -LiteralPath $summaryPath -PathType Leaf) `
        -Message "Performance run has no summary.json: $resolved"

    $records = @(Get-Content -LiteralPath $summaryPath -Raw | ConvertFrom-Json)
    Require-Evidence -Condition ($records.Count -eq 5) `
        -Message "Performance run must contain one host, one runtime, and three state records: $resolved"
    $individualFiles = @(Get-ChildItem -LiteralPath $resolved -Filter '*.json' -File |
        Where-Object Name -CNE 'summary.json' |
        Sort-Object Name)
    Require-Evidence -Condition ($individualFiles.Count -eq 5) `
        -Message "Performance run must preserve exactly five individual evidence files: $resolved"
    $individualRecords = @($individualFiles | ForEach-Object {
        Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json
    })
    Require-Evidence `
        -Condition ((Get-CanonicalJson $records) -ceq (Get-CanonicalJson $individualRecords)) `
        -Message "summary.json does not match the individual raw evidence in $resolved"
    foreach ($record in $records) {
        Require-Evidence -Condition ([int64]$record.schemaVersion -eq 1) `
            -Message "Unsupported performance evidence schema in $resolved"
        Require-Evidence -Condition ([string]$record.label -ceq $ExpectedLabel) `
            -Message "Expected label '$ExpectedLabel' in every record from $resolved"
    }

    $hostRecord = @($records | Where-Object benchmark -CEQ 'host')
    $runtimeRecord = @($records | Where-Object benchmark -CEQ 'runtime_hash')
    $stateRecords = @($records | Where-Object benchmark -CEQ 'state_journal' | Sort-Object { [int]$_.workload.queueSize })
    Require-Evidence -Condition ($hostRecord.Count -eq 1) -Message "Expected exactly one host record in $resolved"
    Require-Evidence -Condition ($runtimeRecord.Count -eq 1) -Message "Expected exactly one runtime_hash record in $resolved"
    Require-Evidence -Condition ($stateRecords.Count -eq 3) -Message "Expected exactly three state_journal records in $resolved"
    Require-Evidence -Condition ((Get-CanonicalJson @($stateRecords.workload.queueSize)) -ceq '[1,100,1000]') `
        -Message "State evidence must contain queue sizes 1, 100, and 1000 in $resolved"

    $profiles = @($records | ForEach-Object { [string]$_.profile } | Sort-Object -Unique)
    Require-Evidence -Condition ($profiles.Count -eq 1) -Message "Every record must use one profile in $resolved"

    $artifacts = @(Get-ChildItem -LiteralPath $resolved -Filter '*.json' -File | Sort-Object Name | ForEach-Object {
        $hash = Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256
        [ordered]@{
            name = $_.Name
            bytes = $_.Length
            sha256 = $hash.Hash.ToLowerInvariant()
        }
    })

    return [pscustomobject]@{
        path = $resolved
        profile = $profiles[0]
        host = $hostRecord[0]
        runtime = $runtimeRecord[0]
        states = $stateRecords
        artifacts = $artifacts
    }
}

function Add-HardGate {
    param(
        [Parameter(Mandatory = $true)]$List,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Scope,
        [Parameter(Mandatory = $true)][bool]$Passed,
        [Parameter(Mandatory = $true)]$Expected,
        [Parameter(Mandatory = $true)]$Actual
    )
    $List.Add([ordered]@{
        name = $Name
        scope = $Scope
        passed = $Passed
        expected = $Expected
        actual = $Actual
    }) | Out-Null
}

function New-NumericComparison {
    param(
        [Parameter(Mandatory = $true)][string]$Category,
        [Parameter(Mandatory = $true)][string]$Scope,
        [Parameter(Mandatory = $true)][string]$Metric,
        [AllowNull()][string]$Percentile,
        [Parameter(Mandatory = $true)][double]$BaselineValue,
        [Parameter(Mandatory = $true)][double]$AfterValue,
        [Parameter(Mandatory = $true)][string]$Unit,
        [bool]$Timing = $false
    )
    $delta = $AfterValue - $BaselineValue
    $ratio = if ($BaselineValue -eq 0) { $null } else { $AfterValue / $BaselineValue }
    $percentChange = if ($null -eq $ratio) { $null } else { ($ratio - 1.0) * 100.0 }
    return [ordered]@{
        category = $Category
        scope = $Scope
        metric = $Metric
        percentile = $Percentile
        unit = $Unit
        baseline = $BaselineValue
        after = $AfterValue
        absoluteDelta = $delta
        ratio = $ratio
        percentChange = $percentChange
        direction = if ($delta -lt 0) { 'decreased' } elseif ($delta -gt 0) { 'increased' } else { 'unchanged' }
        reviewRequired = $Timing -and $delta -gt 0
        releaseGate = $false
    }
}

function Format-Number {
    param([AllowNull()]$Value)
    if ($null -eq $Value) {
        return 'n/a'
    }
    return ([double]$Value).ToString('0.###', [Globalization.CultureInfo]::InvariantCulture)
}

function Escape-Markdown {
    param([AllowNull()]$Value)
    return ([string]$Value).Replace('|', '\|').Replace("`r", ' ').Replace("`n", ' ')
}

$baselineRun = Read-PerformanceRun -Path $Baseline -ExpectedLabel 'baseline'
$afterRun = Read-PerformanceRun -Path $After -ExpectedLabel 'after'

Require-Evidence -Condition ([string]$baselineRun.profile -ceq [string]$afterRun.profile) `
    -Message 'Baseline and after evidence used different build profiles.'

$environmentFields = @(
    'windowsDisplayVersion',
    'windowsBuild',
    'processorName',
    'logicalProcessors',
    'repositoryVolumeFormat'
)
foreach ($field in $environmentFields) {
    $baselineValue = $baselineRun.host.metrics.$field
    $afterValue = $afterRun.host.metrics.$field
    Require-Evidence -Condition ([string]$baselineValue -ceq [string]$afterValue) `
        -Message "Baseline and after host field '$field' did not match."
}

Require-Evidence `
    -Condition ((Get-CanonicalJson $baselineRun.runtime.workload) -ceq (Get-CanonicalJson $afterRun.runtime.workload)) `
    -Message 'Runtime benchmark workloads did not match.'

$hardGates = [Collections.Generic.List[object]]::new()
$comparisons = [Collections.Generic.List[object]]::new()
$afterOnlyEvidence = [Collections.Generic.List[object]]::new()
$latencyMetrics = @('snapshot', 'snapshotWire', 'durableCommand', 'journalSave', 'enqueueBatch')
$percentiles = @('p50', 'p95', 'p99')
$resourceFields = @(
    'workingSetAfterLoad',
    'workingSetAfterSamples',
    'privateAfterLoad',
    'privateAfterSamples'
)
$storageFields = @('snapshotJson', 'journalAfterDurable', 'journalAfterEnqueue')

for ($index = 0; $index -lt $baselineRun.states.Count; $index++) {
    $baselineState = $baselineRun.states[$index]
    $afterState = $afterRun.states[$index]
    $queueSize = [int]$baselineState.workload.queueSize
    $scope = "queue-$queueSize"
    Require-Evidence -Condition ([int]$afterState.workload.queueSize -eq $queueSize) `
        -Message "State queue size mismatch at $scope."
    Require-Evidence `
        -Condition ((Get-CanonicalJson $baselineState.workload) -ceq (Get-CanonicalJson $afterState.workload)) `
        -Message "State workload mismatch at $scope."

    foreach ($metric in $latencyMetrics) {
        $expectedSamples = if ($metric -eq 'enqueueBatch') {
            1
        }
        elseif ($metric -eq 'snapshot') {
            [int]$afterState.workload.snapshotSamples
        }
        elseif ($metric -eq 'snapshotWire') {
            [int]$afterState.workload.snapshotWireSamples
        }
        elseif ($metric -eq 'durableCommand') {
            [int]$afterState.workload.durableCommandSamples
        }
        else {
            [int]$afterState.workload.journalSamples
        }
        Add-HardGate -List $hardGates -Name 'latency_sample_count' -Scope "$scope/$metric" `
            -Passed ([int]$afterState.metrics.latencyMicros.$metric.samples -eq $expectedSamples) `
            -Expected $expectedSamples -Actual ([int]$afterState.metrics.latencyMicros.$metric.samples)
        foreach ($percentile in $percentiles) {
            $comparisons.Add((New-NumericComparison `
                -Category 'latency' `
                -Scope $scope `
                -Metric $metric `
                -Percentile $percentile `
                -BaselineValue ([double]$baselineState.metrics.latencyMicros.$metric.$percentile) `
                -AfterValue ([double]$afterState.metrics.latencyMicros.$metric.$percentile) `
                -Unit 'microseconds' `
                -Timing $true)) | Out-Null
        }
    }

    Add-HardGate -List $hardGates -Name 'blocked_io_snapshot_isolation' -Scope $scope `
        -Passed ([bool]$afterState.metrics.blockedJournalSnapshot.observedPreCommitState) `
        -Expected $true -Actual ([bool]$afterState.metrics.blockedJournalSnapshot.observedPreCommitState)
    $comparisons.Add((New-NumericComparison `
        -Category 'blocked_io' -Scope $scope -Metric 'snapshot' -Percentile $null `
        -BaselineValue ([double]$baselineState.metrics.blockedJournalSnapshot.latencyMicros) `
        -AfterValue ([double]$afterState.metrics.blockedJournalSnapshot.latencyMicros) `
        -Unit 'microseconds' -Timing $true)) | Out-Null

    $expectedDeltaCount = $queueSize * 2
    Add-HardGate -List $hardGates -Name 'event_sequence_contiguous' -Scope $scope `
        -Passed ([bool]$afterState.metrics.eventBacklog.sequencesContiguous) `
        -Expected $true -Actual ([bool]$afterState.metrics.eventBacklog.sequencesContiguous)
    Add-HardGate -List $hardGates -Name 'event_delta_count' -Scope $scope `
        -Passed ([int]$afterState.metrics.eventBacklog.deltaCount -eq $expectedDeltaCount) `
        -Expected $expectedDeltaCount -Actual ([int]$afterState.metrics.eventBacklog.deltaCount)
    foreach ($field in @('deltaCount', 'serializedBytes')) {
        $comparisons.Add((New-NumericComparison `
            -Category 'event_backlog' -Scope $scope -Metric $field -Percentile $null `
            -BaselineValue ([double]$baselineState.metrics.eventBacklog.$field) `
            -AfterValue ([double]$afterState.metrics.eventBacklog.$field) `
            -Unit $(if ($field -eq 'serializedBytes') { 'bytes' } else { 'count' }))) | Out-Null
    }

    $outboxProperty = $afterState.metrics.PSObject.Properties['outbox']
    Require-Evidence -Condition ($null -ne $outboxProperty) `
        -Message "After evidence is missing StateStore outbox statistics at $scope."
    $outbox = $outboxProperty.Value
    $outboxChecks = @(
        [ordered]@{ metric = 'queuedBatches'; value = [int64]$outbox.queuedBatches; bound = [int64]$outbox.limits.maxBatches; unit = 'count' },
        [ordered]@{ metric = 'queuedDeltas'; value = [int64]$outbox.queuedDeltas; bound = [int64]$outbox.limits.maxDeltas; unit = 'count' },
        [ordered]@{ metric = 'estimatedBytes'; value = [int64]$outbox.estimatedBytes; bound = [int64]$outbox.limits.maxEstimatedBytes; unit = 'bytes' },
        [ordered]@{ metric = 'coalescedResyncs'; value = [int64]$outbox.coalescedResyncs; bound = 0; unit = 'count' }
    )
    foreach ($check in $outboxChecks) {
        $passed = $check.value -le $check.bound
        Add-HardGate -List $hardGates -Name "outbox_$($check.metric)_bound" -Scope $scope `
            -Passed $passed -Expected "<= $($check.bound)" -Actual $check.value
        $afterOnlyEvidence.Add([ordered]@{
            category = 'outbox'
            scope = $scope
            metric = $check.metric
            unit = $check.unit
            after = $check.value
            configuredMaximum = $check.bound
            passed = $passed
            releaseGate = $true
            baseline = $null
            note = 'After-only evidence checked against the production outbox bound.'
        }) | Out-Null
    }

    foreach ($field in $resourceFields) {
        $comparisons.Add((New-NumericComparison `
            -Category 'memory' -Scope $scope -Metric $field -Percentile $null `
            -BaselineValue ([double]$baselineState.metrics.memoryBytes.$field) `
            -AfterValue ([double]$afterState.metrics.memoryBytes.$field) -Unit 'bytes')) | Out-Null
    }
    $baselinePrivateGrowth = [double]$baselineState.metrics.memoryBytes.privateAfterSamples - [double]$baselineState.metrics.memoryBytes.privateAfterLoad
    $afterPrivateGrowth = [double]$afterState.metrics.memoryBytes.privateAfterSamples - [double]$afterState.metrics.memoryBytes.privateAfterLoad
    $comparisons.Add((New-NumericComparison -Category 'memory' -Scope $scope -Metric 'privateGrowth' `
        -Percentile $null -BaselineValue $baselinePrivateGrowth -AfterValue $afterPrivateGrowth -Unit 'bytes')) | Out-Null
    $baselineWorkingSetGrowth = [double]$baselineState.metrics.memoryBytes.workingSetAfterSamples - [double]$baselineState.metrics.memoryBytes.workingSetAfterLoad
    $afterWorkingSetGrowth = [double]$afterState.metrics.memoryBytes.workingSetAfterSamples - [double]$afterState.metrics.memoryBytes.workingSetAfterLoad
    $comparisons.Add((New-NumericComparison -Category 'memory' -Scope $scope -Metric 'workingSetGrowth' `
        -Percentile $null -BaselineValue $baselineWorkingSetGrowth -AfterValue $afterWorkingSetGrowth -Unit 'bytes')) | Out-Null

    foreach ($field in $storageFields) {
        $comparisons.Add((New-NumericComparison `
            -Category 'storage' -Scope $scope -Metric $field -Percentile $null `
            -BaselineValue ([double]$baselineState.metrics.storageBytes.$field) `
            -AfterValue ([double]$afterState.metrics.storageBytes.$field) -Unit 'bytes')) | Out-Null
    }

    if ($null -ne $MaxEventBacklogBytes) {
        Add-HardGate -List $hardGates -Name 'configured_event_backlog_bound' -Scope $scope `
            -Passed ([int64]$afterState.metrics.eventBacklog.serializedBytes -le [int64]$MaxEventBacklogBytes) `
            -Expected "<= $([int64]$MaxEventBacklogBytes)" -Actual ([int64]$afterState.metrics.eventBacklog.serializedBytes)
    }
    if ($null -ne $MaxPrivateGrowthBytes) {
        Add-HardGate -List $hardGates -Name 'configured_private_growth_bound' -Scope $scope `
            -Passed ($afterPrivateGrowth -le [int64]$MaxPrivateGrowthBytes) `
            -Expected "<= $([int64]$MaxPrivateGrowthBytes)" -Actual $afterPrivateGrowth
    }
    if ($null -ne $MaxWorkingSetGrowthBytes) {
        Add-HardGate -List $hardGates -Name 'configured_working_set_growth_bound' -Scope $scope `
            -Passed ($afterWorkingSetGrowth -le [int64]$MaxWorkingSetGrowthBytes) `
            -Expected "<= $([int64]$MaxWorkingSetGrowthBytes)" -Actual $afterWorkingSetGrowth
    }
    if ($null -ne $MaxJournalBytes) {
        Add-HardGate -List $hardGates -Name 'configured_journal_bound' -Scope $scope `
            -Passed ([int64]$afterState.metrics.storageBytes.journalAfterEnqueue -le [int64]$MaxJournalBytes) `
            -Expected "<= $([int64]$MaxJournalBytes)" -Actual ([int64]$afterState.metrics.storageBytes.journalAfterEnqueue)
    }
}

$hashFields = @(
    'totalInvocations',
    'totalBytes',
    'manifestInvocations',
    'manifestBytes',
    'toolInvocations',
    'toolBytes'
)
foreach ($field in $hashFields) {
    $comparisons.Add((New-NumericComparison `
        -Category 'runtime_hash' -Scope 'runtime' -Metric $field -Percentile $null `
        -BaselineValue ([double]$baselineRun.runtime.metrics.hashes.$field) `
        -AfterValue ([double]$afterRun.runtime.metrics.hashes.$field) `
        -Unit $(if ($field.EndsWith('Bytes')) { 'bytes' } else { 'count' }))) | Out-Null
}
foreach ($field in @('resolutionCalls', 'successfulResolutions')) {
    $comparisons.Add((New-NumericComparison `
        -Category 'runtime_lease' -Scope 'runtime' -Metric $field -Percentile $null `
        -BaselineValue ([double]$baselineRun.runtime.metrics.leases.$field) `
        -AfterValue ([double]$afterRun.runtime.metrics.leases.$field) -Unit 'count')) | Out-Null
}

$afterResolutionCalls = [int64]$afterRun.runtime.metrics.leases.resolutionCalls
$afterSuccessfulResolutions = [int64]$afterRun.runtime.metrics.leases.successfulResolutions
Add-HardGate -List $hardGates -Name 'runtime_resolution_workload_preserved' -Scope 'runtime' `
    -Passed ($afterResolutionCalls -eq [int64]$baselineRun.runtime.metrics.leases.resolutionCalls) `
    -Expected ([int64]$baselineRun.runtime.metrics.leases.resolutionCalls) -Actual $afterResolutionCalls
Add-HardGate -List $hardGates -Name 'runtime_resolutions_successful' -Scope 'runtime' `
    -Passed ($afterSuccessfulResolutions -eq $afterResolutionCalls) `
    -Expected $afterResolutionCalls -Actual $afterSuccessfulResolutions

$uniqueToolCount = @($afterRun.runtime.workload.toolNames | Sort-Object -Unique).Count
$afterToolHashes = [int64]$afterRun.runtime.metrics.hashes.toolInvocations
$additionalExecutableHashes = [Math]::Max(0, $afterToolHashes - $uniqueToolCount)
Add-HardGate -List $hardGates -Name 'unchanged_leased_operations_add_no_executable_hashes' -Scope 'runtime' `
    -Passed ($additionalExecutableHashes -eq 0) `
    -Expected 0 -Actual $additionalExecutableHashes

$hardGateFailures = @($hardGates | Where-Object { -not $_.passed })
$timingReviewRows = @($comparisons | Where-Object { $_.category -in @('latency', 'blocked_io') -and $_.reviewRequired })
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
    $id = [Guid]::NewGuid().ToString('N')
    $OutputRoot = Join-Path $repoRoot "target\performance\comparison-$stamp-$id"
}
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
Require-Evidence -Condition (-not (Test-Path -LiteralPath $OutputRoot)) `
    -Message "Comparison output already exists: $OutputRoot"
New-Item -ItemType Directory -Path $OutputRoot | Out-Null

$result = [ordered]@{
    schemaVersion = 1
    benchmark = 'backend_performance_comparison'
    recordedAtUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    profile = [string]$baselineRun.profile
    baseline = [ordered]@{ path = $baselineRun.path; artifacts = $baselineRun.artifacts }
    after = [ordered]@{ path = $afterRun.path; artifacts = $afterRun.artifacts }
    environment = [ordered]@{
        windowsDisplayVersion = [string]$afterRun.host.metrics.windowsDisplayVersion
        windowsBuild = [string]$afterRun.host.metrics.windowsBuild
        processorName = [string]$afterRun.host.metrics.processorName
        logicalProcessors = [int]$afterRun.host.metrics.logicalProcessors
        repositoryVolumeFormat = [string]$afterRun.host.metrics.repositoryVolumeFormat
        baselineAvailableBytes = [int64]$baselineRun.host.metrics.repositoryVolumeAvailableBytes
        afterAvailableBytes = [int64]$afterRun.host.metrics.repositoryVolumeAvailableBytes
    }
    configuredResourceBounds = [ordered]@{
        maxEventBacklogBytes = $MaxEventBacklogBytes
        maxPrivateGrowthBytes = $MaxPrivateGrowthBytes
        maxWorkingSetGrowthBytes = $MaxWorkingSetGrowthBytes
        maxJournalBytes = $MaxJournalBytes
    }
    hardGateStatus = if ($hardGateFailures.Count -eq 0) { 'passed' } else { 'failed' }
    hardGates = @($hardGates)
    performanceReviewRequired = $timingReviewRows.Count -gt 0
    timingReviewCount = $timingReviewRows.Count
    note = 'Timing changes are informational and never determine hardGateStatus.'
    comparisons = @($comparisons)
    afterOnlyEvidence = @($afterOnlyEvidence)
}

$jsonPath = Join-Path $OutputRoot 'comparison.json'
$markdownPath = Join-Path $OutputRoot 'comparison.md'
$json = $result | ConvertTo-Json -Depth 30
$json | Set-Content -LiteralPath $jsonPath -Encoding utf8

$markdown = [Collections.Generic.List[string]]::new()
$markdown.Add('# Backend performance comparison')
$markdown.Add('')
$markdown.Add("- Profile: ``$($result.profile)``")
$markdown.Add("- Hard gates: **$($result.hardGateStatus)**")
$markdown.Add("- Timing rows requiring review: $($result.timingReviewCount)")
$markdown.Add('- Timing changes are informational and do not determine the hard-gate result.')
$markdown.Add('')
$markdown.Add('## Hard gates')
$markdown.Add('')
$markdown.Add('| Gate | Scope | Result | Expected | Actual |')
$markdown.Add('|---|---|---:|---:|---:|')
foreach ($gate in $hardGates) {
    $markdown.Add("| $(Escape-Markdown $gate.name) | $(Escape-Markdown $gate.scope) | $(if ($gate.passed) { 'pass' } else { 'FAIL' }) | $(Escape-Markdown $gate.expected) | $(Escape-Markdown $gate.actual) |")
}
$markdown.Add('')
$markdown.Add('## Measurements')
$markdown.Add('')
$markdown.Add('| Category | Scope | Metric | Percentile | Baseline | After | Delta | Change | Review |')
$markdown.Add('|---|---|---|---:|---:|---:|---:|---:|---:|')
foreach ($row in $comparisons) {
    $change = if ($null -eq $row.percentChange) { 'n/a' } else { "$(Format-Number $row.percentChange)%" }
    $markdown.Add("| $(Escape-Markdown $row.category) | $(Escape-Markdown $row.scope) | $(Escape-Markdown $row.metric) | $(Escape-Markdown $row.percentile) | $(Format-Number $row.baseline) | $(Format-Number $row.after) | $(Format-Number $row.absoluteDelta) | $change | $(if ($row.reviewRequired) { 'review' } else { '' }) |")
}
$markdown.Add('')
$markdown.Add('## After-only bounded evidence')
$markdown.Add('')
$markdown.Add('| Category | Scope | Metric | After | Configured maximum | Result |')
$markdown.Add('|---|---|---|---:|---:|---:|')
foreach ($row in $afterOnlyEvidence) {
    $markdown.Add("| $(Escape-Markdown $row.category) | $(Escape-Markdown $row.scope) | $(Escape-Markdown $row.metric) | $(Format-Number $row.after) | $(Format-Number $row.configuredMaximum) | $(if ($row.passed) { 'pass' } else { 'FAIL' }) |")
}
$markdown.Add('')
$markdown.Add('## Raw evidence')
$markdown.Add('')
$markdown.Add("- Baseline: ``$($baselineRun.path)``")
$markdown.Add("- After: ``$($afterRun.path)``")
$markdown | Set-Content -LiteralPath $markdownPath -Encoding utf8

$singleLine = $result | ConvertTo-Json -Depth 30 -Compress
Write-Host "PHASE4_PERF_COMPARISON=$singleLine"
Write-Host "PHASE4_PERF_COMPARISON_JSON=$jsonPath"
Write-Host "PHASE4_PERF_COMPARISON_MARKDOWN=$markdownPath"

if ($hardGateFailures.Count -ne 0) {
    exit 2
}
