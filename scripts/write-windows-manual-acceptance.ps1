[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $CandidateDirectory,
    [Parameter(Mandatory)] [string] $InputPath,
    [Parameter(Mandatory)] [string] $OutputDirectory,
    [ValidatePattern('^0\.7\.1$')] [string] $ExpectedVersion = '0.7.1',
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommitSha,
    [Parameter(Mandatory)] [ValidatePattern('^[1-9][0-9]*$')] [string] $ExpectedCandidateRunId,
    [Parameter(Mandatory)] [ValidatePattern('^[A-Za-z0-9](?:[A-Za-z0-9_.@-]{0,78}[A-Za-z0-9])?$')] [string] $Submitter
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'release-json.ps1')

function Assert-ExactProperties {
    param(
        [Parameter(Mandatory)] [object] $Value,
        [Parameter(Mandatory)] [string[]] $Expected,
        [Parameter(Mandatory)] [string] $Label
    )
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $wanted = @($Expected | Sort-Object)
    if (($actual -join "`n") -cne ($wanted -join "`n")) {
        throw "$Label has an unexpected field set."
    }
}

$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$outputRoot = (Resolve-Path -LiteralPath $OutputDirectory).Path
$outputRootItem = Get-Item -LiteralPath $outputRoot -Force
if (-not $outputRootItem.PSIsContainer -or
    ($outputRootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'The manual evidence output must be a regular directory, not a reparse point.'
}

$outputPath = Join-Path $outputRoot 'windows-x64-manual-acceptance.json'
if (Test-Path -LiteralPath $outputPath) {
    throw 'Manual acceptance evidence already exists; refusing to overwrite it.'
}

$inputRecord = Read-BoundedReleaseJson -Path $InputPath -Limit 256KB
$inventoryPath = Join-Path $candidateRoot 'release-candidate-inventory.json'
$inventory = Read-BoundedReleaseJson -Path $inventoryPath -Limit 1MB
Assert-ExactProperties -Value $inputRecord -Expected @('schemaVersion', 'clientEnvironment', 'cases') -Label 'Manual acceptance input'
if ($inputRecord.schemaVersion -ne 1) {
    throw 'Manual acceptance input uses an unsupported schema.'
}

$inventorySha256 = (Get-FileHash -LiteralPath $inventoryPath -Algorithm SHA256).Hash.ToLowerInvariant()
foreach ($case in @($inputRecord.cases)) {
    if ($null -eq $case.PSObject.Properties['candidateInventorySha256'] -or
        [string]$case.candidateInventorySha256 -cne $inventorySha256) {
        throw 'Every manual case must already be bound to the exact candidate inventory before writing evidence.'
    }
}

$submittedAt = [DateTimeOffset]::UtcNow.ToString(
    'yyyy-MM-ddTHH:mm:ssZ',
    [Globalization.CultureInfo]::InvariantCulture
)
$evidence = [ordered]@{
    schemaVersion = 1
    releaseVersion = [string]$inventory.releaseVersion
    sourceCommit = [string]$inventory.sourceCommit
    candidateRunId = $ExpectedCandidateRunId
    candidateCreatedAt = [string]$inventory.createdAt
    candidateInventorySha256 = $inventorySha256
    candidateAssets = @($inventory.assets)
    submittedBy = $Submitter
    submittedAt = $submittedAt
    clientEnvironment = $inputRecord.clientEnvironment
    cases = @($inputRecord.cases)
}

$temporaryPath = Join-Path $outputRoot ".windows-x64-manual-acceptance-$([Guid]::NewGuid().ToString('N')).json"
try {
    $json = $evidence | ConvertTo-Json -Depth 12
    [System.IO.File]::WriteAllText($temporaryPath, $json, [System.Text.UTF8Encoding]::new($false))
    & (Join-Path $PSScriptRoot 'verify-manual-acceptance-evidence.ps1') `
        -EvidencePath $temporaryPath `
        -CandidateDirectory $candidateRoot `
        -ExpectedVersion $ExpectedVersion `
        -ExpectedCommitSha $ExpectedCommitSha `
        -ExpectedCandidateRunId $ExpectedCandidateRunId `
        -ExpectedSubmitter $Submitter | Out-Host
    Move-Item -LiteralPath $temporaryPath -Destination $outputPath
    Write-Output $outputPath
} finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
}
