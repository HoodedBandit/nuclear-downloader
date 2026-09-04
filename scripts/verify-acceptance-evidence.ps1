[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $EvidenceDirectory,
    [Parameter(Mandatory)] [string] $CandidateDirectory,
    [ValidatePattern('^0\.6\.0$')] [string] $ExpectedVersion = '0.6.0',
    [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommitSha,
    [ValidatePattern('^[1-9][0-9]*$')] [string] $ExpectedCandidateRunId
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Read-BoundedJson {
    param([Parameter(Mandatory)] [string] $Path, [Parameter(Mandatory)] [long] $Limit)
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or
        $item.Length -gt $Limit) {
        throw "Evidence JSON is not a bounded regular file: $Path"
    }
    $bytes = [System.IO.File]::ReadAllBytes($item.FullName)
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf) {
        throw 'Evidence JSON must use UTF-8 without a byte-order mark.'
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $arguments = @{ InputObject = $utf8.GetString($bytes) }
    if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) {
        $arguments.DateKind = 'String'
    }
    return ConvertFrom-Json @arguments
}

function Assert-ExactProperties {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string[]] $Expected, [Parameter(Mandatory)] [string] $Label)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $wanted = @($Expected | Sort-Object)
    if (($actual -join "`n") -cne ($wanted -join "`n")) {
        throw "$Label has an unexpected field set."
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

$evidence = Read-BoundedJson -Path (Join-Path $evidenceRoot 'windows-x64-acceptance.json') -Limit 1MB
$inventory = Read-BoundedJson -Path (Join-Path $candidateRoot 'release-candidate-inventory.json') -Limit 1MB
Assert-ExactProperties -Value $evidence -Expected @(
    'schemaVersion', 'releaseVersion', 'sourceCommit', 'candidateRunId', 'candidateCreatedAt',
    'startedAt', 'completedAt', 'os', 'webdriver', 'steps',
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
    'fixtureServer',
    'cleanInstall',
    'installedFixtureDownloadConversionCancelReloadDiagnostics',
    'processRestartJournalRecovery',
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
    'Dedicated-account cookie/login test with no CI cookie secret',
    'Maintainer review of managed-runtime update and rollback UI against protected signed test assets',
    'Maintainer acceptance decision before publication'
)
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
