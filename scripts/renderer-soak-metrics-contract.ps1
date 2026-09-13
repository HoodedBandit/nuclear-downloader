function Test-RendererSoakPositiveInteger {
    param([AllowNull()][object] $Value)
    return (($Value -is [int]) -or ($Value -is [long])) -and [long] $Value -gt 0
}

function Test-RendererSoakMetrics {
    param(
        [Parameter(Mandatory)] [object] $Metrics,
        [Parameter(Mandatory)] [int] $ConfiguredSeconds,
        [Parameter(Mandatory)] [double] $RunnerElapsedMilliseconds
    )

    $requiredProperties = @(
        'configuredSeconds', 'elapsedMs', 'mounts', 'activeOperationCycles',
        'lateListenerCycles', 'delayedStartupCycles', 'completedWorkflowCycles',
        'playlistWorkflowCycles', 'playlistResyncCycles', 'gcAvailable'
    )
    $propertyNames = @($Metrics.PSObject.Properties.Name)
    if ($requiredProperties.Where({ $_ -notin $propertyNames }).Count -ne 0) { return $false }

    $validCounts =
        (Test-RendererSoakPositiveInteger $Metrics.activeOperationCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.lateListenerCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.delayedStartupCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.completedWorkflowCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.playlistWorkflowCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.playlistResyncCycles) -and
        (Test-RendererSoakPositiveInteger $Metrics.mounts)
    if (-not $validCounts) { return $false }

    $cycleSum =
        [long] $Metrics.activeOperationCycles +
        [long] $Metrics.lateListenerCycles +
        [long] $Metrics.delayedStartupCycles +
        [long] $Metrics.completedWorkflowCycles +
        [long] $Metrics.playlistWorkflowCycles
    return (
        [double] $Metrics.configuredSeconds -eq $ConfiguredSeconds -and
        [double]::IsFinite([double] $Metrics.elapsedMs) -and
        [double] $Metrics.elapsedMs -ge ($ConfiguredSeconds * 1000) -and
        [double] $Metrics.elapsedMs -le (($ConfiguredSeconds + 60) * 1000) -and
        [double] $Metrics.elapsedMs -le ($RunnerElapsedMilliseconds + 5000) -and
        $cycleSum -eq [long] $Metrics.mounts -and
        [long] $Metrics.playlistResyncCycles -eq [long] $Metrics.playlistWorkflowCycles -and
        ($Metrics.gcAvailable -is [bool]) -and
        $Metrics.gcAvailable -eq $true
    )
}
