[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $EvidenceDirectory,
    [Parameter(Mandatory)] [string] $CandidateDirectory,
    [ValidatePattern('^0\.7\.1$')] [string] $ExpectedVersion = '0.7.1',
    [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommitSha,
    [ValidatePattern('^[1-9][0-9]*$')] [string] $ExpectedCandidateRunId
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'release-json.ps1')

function Assert-ExactProperties {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string[]] $Expected, [Parameter(Mandatory)] [string] $Label)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $wanted = @($Expected | Sort-Object)
    if (($actual -join "`n") -cne ($wanted -join "`n")) {
        throw "$Label has an unexpected field set."
    }
}

function Assert-OpaqueFixtureId {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    if ([string]$Value -cnotmatch '^[a-z0-9][a-z0-9._-]{0,127}$') {
        throw "$Label is not a bounded opaque fixture ID."
    }
}

$evidenceRoot = (Resolve-Path -LiteralPath $EvidenceDirectory).Path
$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$evidenceFiles = @(Get-ChildItem -LiteralPath $evidenceRoot -Force)
if ($evidenceFiles.Count -lt 1 -or $evidenceFiles.Count -gt 64) {
    throw 'Acceptance evidence must contain a bounded set of files.'
}
foreach ($file in $evidenceFiles) {
    if ($file.PSIsContainer -or ($file.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Acceptance evidence may contain only regular, non-reparse files.'
    }
    if ($file.Name -ceq 'windows-x64-acceptance.json') { continue }
    if ($file.Name -cnotmatch '^[0-9]{2}-[a-z0-9-]+\.(stdout|stderr)\.log$' -or $file.Length -gt (4MB + 128)) {
        throw 'Acceptance evidence contains an unexpected or oversized diagnostic log.'
    }
}

$evidence = Read-BoundedReleaseJson -Path (Join-Path $evidenceRoot 'windows-x64-acceptance.json') -Limit 1MB
$inventory = Read-BoundedReleaseJson -Path (Join-Path $candidateRoot 'release-candidate-inventory.json') -Limit 1MB
Assert-ExactProperties -Value $evidence -Expected @(
    'schemaVersion', 'releaseVersion', 'sourceCommit', 'candidateRunId', 'candidateCreatedAt',
    'startedAt', 'completedAt', 'os', 'webdriver', 'steps',
    'controlledSiteFixtures', 'extractorQualificationStatus', 'qualificationStatus', 'incompleteRequirements',
    'manualAcceptanceRequired', 'candidateAssets'
) -Label 'Acceptance evidence'
Assert-ExactProperties -Value $evidence.os -Expected @('description', 'architecture') -Label 'Acceptance operating system'
Assert-ExactProperties -Value $evidence.webdriver -Expected @(
    'webView2RuntimeVersion', 'edgeDriverVersion', 'integrityRid'
) -Label 'Acceptance WebDriver environment'

if ([int]$evidence.schemaVersion -ne 1 -or
    [string]$evidence.releaseVersion -cne $ExpectedVersion -or
    [string]$evidence.sourceCommit -cne $ExpectedCommitSha -or
    [string]$evidence.candidateRunId -cne $ExpectedCandidateRunId -or
    [string]$evidence.candidateCreatedAt -cne [string]$inventory.createdAt -or
    [string]$evidence.os.architecture -cne 'X64' -or
    $evidence.webdriver.integrityRid -isnot [long] -or
    $evidence.webdriver.integrityRid -ne 8192) {
    throw 'Acceptance evidence identity or platform does not match the candidate.'
}

$webView2RuntimeVersion = [string]$evidence.webdriver.webView2RuntimeVersion
$edgeDriverVersion = [string]$evidence.webdriver.edgeDriverVersion
if ($webView2RuntimeVersion -cnotmatch '^[1-9][0-9]*\.[0-9]+\.[0-9]+\.[0-9]+$' -or
    $edgeDriverVersion -cnotmatch '^[1-9][0-9]*\.[0-9]+\.[0-9]+\.[0-9]+$' -or
    (($webView2RuntimeVersion.Split('.')[0..2] -join '.') -cne
        ($edgeDriverVersion.Split('.')[0..2] -join '.'))) {
    throw 'Acceptance evidence contains an invalid or incompatible WebView2/EdgeDriver pair.'
}

foreach ($timestampName in @('startedAt', 'completedAt')) {
    $timestamp = [string]$evidence.$timestampName
    $parsed = [DateTimeOffset]::MinValue
    if ($timestamp -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$' -or
        -not [DateTimeOffset]::TryParseExact(
            $timestamp,
            'yyyy-MM-ddTHH:mm:ssZ',
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::AssumeUniversal,
            [ref]$parsed
        )) {
        throw "Acceptance evidence $timestampName is not a canonical UTC timestamp."
    }
}

$requiredSteps = @(
    'portableExtracted',
    'fixtureGenerated',
    'genericPlaylistValidated',
    'fixtureServer',
    'cleanInstall',
    'installedMp4RetryCollisionPlaylistCancelAllRuntimeChecks',
    'forcedActiveProcessTermination',
    'interruptedOperationRestartRecovery',
    'portableStartup',
    'uninstallAndRetainedUserData',
    'postAcceptanceHashVerification'
)
Assert-ExactProperties -Value $evidence.steps -Expected $requiredSteps -Label 'Acceptance steps'
foreach ($step in $requiredSteps) {
    if ([string]$evidence.steps.$step -cne 'passed') {
        throw "Acceptance step did not pass: $step"
    }
}

$requiredManual = @(
    'clean-windows11-installer',
    'clean-windows11-portable',
    'youtube-maintainer-fixture',
    'x-maintainer-fixture',
    'dedicated-account-cookie-login',
    'signed-app-update',
    'signed-runtime-update-rollback'
)
$controlledFixtureCaseIds = @('youtube-maintainer-fixture', 'x-maintainer-fixture')
Assert-ExactProperties -Value $evidence.controlledSiteFixtures -Expected @('passed', 'missing') -Label 'Controlled site fixtures'
$passedFixtures = @($evidence.controlledSiteFixtures.passed)
$missingFixtures = @($evidence.controlledSiteFixtures.missing)
$passedCaseIds = @()
foreach ($fixture in $passedFixtures) {
    Assert-ExactProperties -Value $fixture -Expected @('caseId', 'fixtureId') -Label 'Passed controlled site fixture'
    $caseId = [string]$fixture.caseId
    if ($caseId -notin $controlledFixtureCaseIds -or $caseId -in $passedCaseIds) {
        throw "Controlled site evidence contains an unexpected or duplicate passed case: $caseId"
    }
    Assert-OpaqueFixtureId -Value $fixture.fixtureId -Label "Controlled fixture ID for $caseId"
    $passedCaseIds += $caseId
}
$expectedPassedOrder = @($controlledFixtureCaseIds | Where-Object { $_ -in $passedCaseIds })
if (($passedCaseIds -join "`n") -cne ($expectedPassedOrder -join "`n")) {
    throw 'Passed controlled site fixtures are not in canonical order.'
}
$expectedMissing = @($controlledFixtureCaseIds | Where-Object { $_ -notin $passedCaseIds })
if (($missingFixtures -join "`n") -cne ($expectedMissing -join "`n")) {
    throw 'Controlled site fixtures do not account for each required site exactly once.'
}
$expectedExtractorStatus = if ($expectedMissing.Count -eq 0) { 'complete' } else { 'incomplete' }
if ([string]$evidence.extractorQualificationStatus -cne $expectedExtractorStatus -or
    [string]$evidence.qualificationStatus -cne 'incomplete' -or
    (@($evidence.incompleteRequirements) -join "`n") -cne ($requiredManual -join "`n")) {
    throw 'Automated acceptance qualification status does not match its controlled-site evidence.'
}

if ((@($evidence.manualAcceptanceRequired) -join "`n") -cne ($requiredManual -join "`n")) {
    throw 'Acceptance evidence manual-review list is not exact.'
}

$expectedAssets = @($inventory.assets | Sort-Object -Property fileName)
$evidenceAssets = @($evidence.candidateAssets | Sort-Object -Property fileName)
if ($expectedAssets.Count -ne $evidenceAssets.Count) {
    throw 'Acceptance evidence asset count does not match the candidate inventory.'
}
for ($index = 0; $index -lt $expectedAssets.Count; $index++) {
    $expected = $expectedAssets[$index]
    $actual = $evidenceAssets[$index]
    Assert-ExactProperties -Value $actual -Expected @('fileName', 'size', 'sha256') -Label 'Acceptance asset'
    if ([string]$actual.fileName -cne [string]$expected.fileName -or
        [long]$actual.size -ne [long]$expected.size -or
        [string]$actual.sha256 -cne [string]$expected.sha256) {
        throw "Acceptance evidence does not bind candidate asset $([string]$expected.fileName)."
    }
}

Write-Output "Verified Windows x64 acceptance evidence for candidate run $ExpectedCandidateRunId."
