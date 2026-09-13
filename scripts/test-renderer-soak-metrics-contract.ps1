$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'renderer-soak-metrics-contract.ps1')

function Assert-Contract {
    param(
        [Parameter(Mandatory)] [bool] $Expected,
        [Parameter(Mandatory)] [object] $Metrics,
        [string] $Label = 'fixture'
    )
    $actual = Test-RendererSoakMetrics `
        -Metrics $Metrics `
        -ConfiguredSeconds 60 `
        -RunnerElapsedMilliseconds 65000
    if ($actual -ne $Expected) { throw "$Label expected $Expected but received $actual." }
}

$valid = [pscustomobject]@{
    configuredSeconds = 60
    elapsedMs = 60050
    mounts = 14
    activeOperationCycles = 3
    lateListenerCycles = 3
    delayedStartupCycles = 3
    completedWorkflowCycles = 3
    playlistWorkflowCycles = 2
    playlistResyncCycles = 2
    gcAvailable = $true
}
Assert-Contract -Expected $true -Metrics $valid -Label 'valid five-mode evidence'

foreach ($invalid in @(
    [pscustomobject]@{ Label = 'missing playlist evidence'; Value = [pscustomobject]@{ configuredSeconds=60; elapsedMs=60050; mounts=12; activeOperationCycles=3; lateListenerCycles=3; delayedStartupCycles=3; completedWorkflowCycles=3; gcAvailable=$true } },
    [pscustomobject]@{ Label = 'missing playlist cycles'; Value = [pscustomobject]@{ configuredSeconds=60; elapsedMs=60050; mounts=12; activeOperationCycles=3; lateListenerCycles=3; delayedStartupCycles=3; completedWorkflowCycles=3; playlistWorkflowCycles=0; playlistResyncCycles=0; gcAvailable=$true } },
    [pscustomobject]@{ Label = 'playlist resync mismatch'; Value = [pscustomobject]@{ configuredSeconds=60; elapsedMs=60050; mounts=14; activeOperationCycles=3; lateListenerCycles=3; delayedStartupCycles=3; completedWorkflowCycles=3; playlistWorkflowCycles=2; playlistResyncCycles=1; gcAvailable=$true } },
    [pscustomobject]@{ Label = 'five-mode sum mismatch'; Value = [pscustomobject]@{ configuredSeconds=60; elapsedMs=60050; mounts=15; activeOperationCycles=3; lateListenerCycles=3; delayedStartupCycles=3; completedWorkflowCycles=3; playlistWorkflowCycles=2; playlistResyncCycles=2; gcAvailable=$true } }
)) {
    Assert-Contract -Expected $false -Metrics $invalid.Value -Label $invalid.Label
}

Write-Host 'Renderer soak metrics contract fixtures passed.'
