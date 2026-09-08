[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $EvidencePath,
    [Parameter(Mandatory)] [string] $CandidateDirectory,
    [ValidatePattern('^0\.6\.0$')] [string] $ExpectedVersion = '0.6.0',
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommitSha,
    [Parameter(Mandatory)] [ValidatePattern('^[1-9][0-9]*$')] [string] $ExpectedCandidateRunId,
    [Parameter(Mandatory)] [ValidatePattern('^[A-Za-z0-9](?:[A-Za-z0-9_.@-]{0,78}[A-Za-z0-9])?$')] [string] $ExpectedSubmitter
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

function Get-LowerSha256 {
    param([Parameter(Mandatory)] [string] $Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-CanonicalTimestamp {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    $timestamp = [string]$Value
    $parsed = [DateTimeOffset]::MinValue
    if ($timestamp -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$' -or
        -not [DateTimeOffset]::TryParseExact(
            $timestamp,
            'yyyy-MM-ddTHH:mm:ssZ',
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::AssumeUniversal,
            [ref]$parsed
        )) {
        throw "$Label is not a canonical UTC timestamp."
    }
    return $parsed.ToUniversalTime()
}

function Assert-SafeIdentifier {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    if ([string]$Value -cnotmatch '^[A-Za-z0-9](?:[A-Za-z0-9_.@-]{0,78}[A-Za-z0-9])?$') {
        throw "$Label is not a bounded operator identifier."
    }
}

function Assert-OpaqueFixtureId {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    if ([string]$Value -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$') {
        throw "$Label is not a bounded opaque fixture ID."
    }
}

function Assert-BoundedDisplayText {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    $text = [string]$Value
    if ($text.Length -lt 1 -or $text.Length -gt 128 -or $text -match '[\x00-\x1f\x7f]') {
        throw "$Label is empty, oversized, or contains control characters."
    }
}

function Convert-ThreePartVersion {
    param([Parameter(Mandatory)] [object] $Value, [Parameter(Mandatory)] [string] $Label)
    $text = [string]$Value
    if ($text -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
        throw "$Label is not a three-component version."
    }
    try {
        return [Version]::Parse($text)
    } catch {
        throw "$Label is outside the supported numeric version range."
    }
}

function Get-InventoryAsset {
    param(
        [Parameter(Mandatory)] [object[]] $Assets,
        [Parameter(Mandatory)] [string] $FileName
    )
    $matches = @($Assets | Where-Object { [string]$_.fileName -ceq $FileName })
    if ($matches.Count -ne 1) {
        throw "Candidate inventory does not contain exactly one $FileName."
    }
    return $matches[0]
}

function Read-RuntimeManifest {
    param(
        [Parameter(Mandatory)] [string] $ArchivePath,
        [Parameter(Mandatory)] [string] $ExpectedRuntimeVersion
    )
    $archiveItem = Get-Item -LiteralPath $ArchivePath -Force
    if ($archiveItem.PSIsContainer -or
        ($archiveItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $archiveItem.Length -le 0 -or $archiveItem.Length -gt 1GB) {
        throw 'The managed-runtime archive is not a bounded regular file.'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archiveItem.FullName)
    try {
        $entries = @($archive.Entries | Where-Object { $_.FullName -ceq 'runtime-manifest.json' })
        if ($entries.Count -ne 1 -or $entries[0].Length -le 0 -or $entries[0].Length -gt 64KB) {
            throw 'The managed-runtime archive has no unique bounded root manifest.'
        }
        $stream = $entries[0].Open()
        try {
            $memory = [System.IO.MemoryStream]::new()
            try {
                $stream.CopyTo($memory)
                $bytes = $memory.ToArray()
            } finally {
                $memory.Dispose()
            }
        } finally {
            $stream.Dispose()
        }
    } finally {
        $archive.Dispose()
    }
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf) {
        throw 'The managed-runtime manifest must use UTF-8 without a byte-order mark.'
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $manifest = ConvertFrom-Json -InputObject $utf8.GetString($bytes) -DateKind String
    Assert-ExactProperties -Value $manifest -Expected @('schemaVersion', 'runtimeVersion', 'platform', 'tools') -Label 'Runtime manifest'
    if ($manifest.schemaVersion -ne 1 -or
        [string]$manifest.runtimeVersion -cne $ExpectedRuntimeVersion -or
        [string]$manifest.platform -cne 'windows-x64') {
        throw 'The managed-runtime manifest identity is invalid.'
    }
    return $manifest
}

$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$inventoryPath = Join-Path $candidateRoot 'release-candidate-inventory.json'
$inventory = Read-BoundedReleaseJson -Path $inventoryPath -Limit 1MB
$evidence = Read-BoundedReleaseJson -Path $EvidencePath -Limit 1MB

Assert-ExactProperties -Value $inventory -Expected @(
    'schemaVersion', 'releaseVersion', 'releaseTag', 'platform', 'keyId', 'sourceCommit',
    'createdAt', 'toolchains', 'assets'
) -Label 'Candidate inventory'
Assert-ExactProperties -Value $evidence -Expected @(
    'schemaVersion', 'releaseVersion', 'sourceCommit', 'candidateRunId', 'candidateCreatedAt',
    'candidateInventorySha256', 'candidateAssets', 'submittedBy', 'submittedAt',
    'clientEnvironment', 'cases'
) -Label 'Manual acceptance evidence'

$inventorySha256 = Get-LowerSha256 -Path $inventoryPath
if ($evidence.schemaVersion -ne 1 -or
    [string]$evidence.releaseVersion -cne $ExpectedVersion -or
    [string]$evidence.releaseVersion -cne [string]$inventory.releaseVersion -or
    [string]$evidence.sourceCommit -cne $ExpectedCommitSha -or
    [string]$evidence.sourceCommit -cne [string]$inventory.sourceCommit -or
    [string]$evidence.candidateRunId -cne $ExpectedCandidateRunId -or
    [string]$evidence.candidateCreatedAt -cne [string]$inventory.createdAt -or
    [string]$evidence.candidateInventorySha256 -cne $inventorySha256) {
    throw 'Manual acceptance evidence is not bound to the expected candidate.'
}

Assert-SafeIdentifier -Value $evidence.submittedBy -Label 'Manual acceptance submitter'
if ([string]$evidence.submittedBy -cne $ExpectedSubmitter) {
    throw 'Manual acceptance evidence was not submitted by the workflow operator.'
}
$candidateCreatedAt = Assert-CanonicalTimestamp -Value $evidence.candidateCreatedAt -Label 'Candidate creation time'
$submittedAt = Assert-CanonicalTimestamp -Value $evidence.submittedAt -Label 'Manual acceptance submission time'
if ($submittedAt -lt $candidateCreatedAt -or $submittedAt -gt [DateTimeOffset]::UtcNow.AddMinutes(5)) {
    throw 'Manual acceptance submission time is outside the candidate lifetime.'
}

$expectedAssets = @($inventory.assets | Sort-Object -Property fileName)
$actualAssets = @($evidence.candidateAssets | Sort-Object -Property fileName)
if ($expectedAssets.Count -lt 1 -or $expectedAssets.Count -ne $actualAssets.Count) {
    throw 'Manual acceptance candidate asset count is not exact.'
}
for ($index = 0; $index -lt $expectedAssets.Count; $index++) {
    $expectedAsset = $expectedAssets[$index]
    $actualAsset = $actualAssets[$index]
    Assert-ExactProperties -Value $expectedAsset -Expected @('fileName', 'size', 'sha256') -Label 'Candidate inventory asset'
    Assert-ExactProperties -Value $actualAsset -Expected @('fileName', 'size', 'sha256') -Label 'Manual acceptance asset'
    if ([string]$expectedAsset.fileName -cnotmatch '^[A-Za-z0-9._-]+$' -or
        $expectedAsset.size -isnot [long] -or [long]$expectedAsset.size -le 0 -or
        [string]$expectedAsset.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$actualAsset.fileName -cne [string]$expectedAsset.fileName -or
        $actualAsset.size -isnot [long] -or [long]$actualAsset.size -ne [long]$expectedAsset.size -or
        [string]$actualAsset.sha256 -cne [string]$expectedAsset.sha256 -or
        [string]$actualAsset.sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw "Manual acceptance evidence does not bind candidate asset $([string]$expectedAsset.fileName)."
    }
}

$runtimeDescriptorPath = Join-Path $candidateRoot 'nuclear-downloader-runtime-windows-x64.json'
$runtimeDescriptor = Read-BoundedReleaseJson -Path $runtimeDescriptorPath -Limit 64KB
Assert-ExactProperties -Value $runtimeDescriptor -Expected @(
    'schemaVersion', 'keyId', 'runtimeVersion', 'platform', 'archiveName',
    'compressedSize', 'sha256', 'manifestSha256'
) -Label 'Runtime descriptor'
if ($runtimeDescriptor.schemaVersion -ne 1 -or
    [string]$runtimeDescriptor.runtimeVersion -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$' -or
    [string]$runtimeDescriptor.platform -cne 'windows-x64' -or
    [string]$runtimeDescriptor.archiveName -cne "nuclear-downloader-runtime-$([string]$runtimeDescriptor.runtimeVersion)-windows-x64.zip") {
    throw 'The candidate runtime descriptor identity is invalid.'
}
$runtimeArchivePath = Join-Path $candidateRoot ([string]$runtimeDescriptor.archiveName)
$runtimeManifest = Read-RuntimeManifest -ArchivePath $runtimeArchivePath -ExpectedRuntimeVersion ([string]$runtimeDescriptor.runtimeVersion)

$toolVersions = @{}
foreach ($tool in @($runtimeManifest.tools)) {
    Assert-ExactProperties -Value $tool -Expected @('name', 'version', 'path', 'sha256') -Label 'Runtime tool'
    $name = [string]$tool.name
    if ($name -notin @('yt-dlp', 'ffmpeg', 'ffprobe', 'deno') -or $toolVersions.ContainsKey($name)) {
        throw "The runtime manifest contains an unexpected or duplicate tool: $name"
    }
    Assert-BoundedDisplayText -Value $tool.version -Label "Runtime version for $name"
    $toolVersions[$name] = [string]$tool.version
}
if ($toolVersions.Count -ne 4) {
    throw 'The runtime manifest does not contain the exact required tool set.'
}

$environment = $evidence.clientEnvironment
Assert-ExactProperties -Value $environment -Expected @(
    'installationType', 'productName', 'displayVersion', 'buildNumber', 'updateBuildRevision',
    'architecture', 'webView2RuntimeVersion', 'applicationVersion', 'managedRuntimeVersion',
    'runtimeToolVersions'
) -Label 'Manual acceptance client environment'
Assert-ExactProperties -Value $environment.runtimeToolVersions -Expected @(
    'ytDlp', 'ffmpeg', 'ffprobe', 'deno'
) -Label 'Manual acceptance runtime tool versions'
Assert-BoundedDisplayText -Value $environment.productName -Label 'Windows product name'
Assert-BoundedDisplayText -Value $environment.displayVersion -Label 'Windows display version'
if ([string]$environment.installationType -cne 'Client' -or
    [string]$environment.architecture -cne 'X64' -or
    $environment.buildNumber -isnot [long] -or [long]$environment.buildNumber -lt 22000 -or
    $environment.updateBuildRevision -isnot [long] -or [long]$environment.updateBuildRevision -lt 0) {
    throw 'Manual acceptance requires an x64 Windows 11 client installation.'
}
if ([string]$environment.webView2RuntimeVersion -cnotmatch '^[1-9][0-9]*\.[0-9]+\.[0-9]+\.[0-9]+$' -or
    [string]$environment.applicationVersion -cne $ExpectedVersion -or
    [string]$environment.managedRuntimeVersion -cne [string]$runtimeDescriptor.runtimeVersion -or
    [string]$environment.runtimeToolVersions.ytDlp -cne $toolVersions['yt-dlp'] -or
    [string]$environment.runtimeToolVersions.ffmpeg -cne $toolVersions['ffmpeg'] -or
    [string]$environment.runtimeToolVersions.ffprobe -cne $toolVersions['ffprobe'] -or
    [string]$environment.runtimeToolVersions.deno -cne $toolVersions['deno']) {
    throw 'Manual acceptance client versions do not match the tested candidate runtime.'
}

$installerName = "Nuclear.Downloader_${ExpectedVersion}_x64-setup.exe"
$portableName = "Nuclear.Downloader_${ExpectedVersion}_x64-portable.zip"
$appManifestName = "nuclear-downloader-v${ExpectedVersion}-update.json"
$runtimeDescriptorName = 'nuclear-downloader-runtime-windows-x64.json'
$installerAsset = Get-InventoryAsset -Assets $expectedAssets -FileName $installerName
$portableAsset = Get-InventoryAsset -Assets $expectedAssets -FileName $portableName
$appManifestAsset = Get-InventoryAsset -Assets $expectedAssets -FileName $appManifestName
$runtimeDescriptorAsset = Get-InventoryAsset -Assets $expectedAssets -FileName $runtimeDescriptorName
$runtimeArchiveAsset = Get-InventoryAsset -Assets $expectedAssets -FileName ([string]$runtimeDescriptor.archiveName)

$requiredCaseIds = @(
    'clean-windows11-installer',
    'clean-windows11-portable',
    'youtube-maintainer-fixture',
    'x-maintainer-fixture',
    'dedicated-account-cookie-login',
    'signed-app-update',
    'signed-runtime-update-rollback'
)
$cases = @($evidence.cases)
if ($cases.Count -ne $requiredCaseIds.Count) {
    throw 'Manual acceptance evidence does not contain the exact required case count.'
}
$seenCases = @{}
foreach ($case in $cases) {
    Assert-ExactProperties -Value $case -Expected @(
        'caseId', 'outcome', 'operator', 'completedAt', 'candidateInventorySha256', 'details'
    ) -Label 'Manual acceptance case'
    $caseId = [string]$case.caseId
    if ($caseId -notin $requiredCaseIds -or $seenCases.ContainsKey($caseId)) {
        throw "Manual acceptance evidence contains an unexpected or duplicate case: $caseId"
    }
    $seenCases[$caseId] = $true
    if ([string]$case.outcome -cne 'passed' -or
        [string]$case.candidateInventorySha256 -cne $inventorySha256) {
        throw "Manual acceptance case did not pass against the current candidate: $caseId"
    }
    Assert-SafeIdentifier -Value $case.operator -Label "Operator for $caseId"
    $completedAt = Assert-CanonicalTimestamp -Value $case.completedAt -Label "Completion time for $caseId"
    if ($completedAt -lt $candidateCreatedAt -or $completedAt -gt $submittedAt) {
        throw "Manual acceptance case time is outside the candidate and submission interval: $caseId"
    }

    switch ($caseId) {
        'clean-windows11-installer' {
            Assert-ExactProperties -Value $case.details -Expected @('artifactFileName', 'artifactSha256') -Label $caseId
            if ([string]$case.details.artifactFileName -cne $installerName -or
                [string]$case.details.artifactSha256 -cne [string]$installerAsset.sha256) {
                throw "$caseId is not bound to the exact installer."
            }
        }
        'clean-windows11-portable' {
            Assert-ExactProperties -Value $case.details -Expected @('artifactFileName', 'artifactSha256') -Label $caseId
            if ([string]$case.details.artifactFileName -cne $portableName -or
                [string]$case.details.artifactSha256 -cne [string]$portableAsset.sha256) {
                throw "$caseId is not bound to the exact portable archive."
            }
        }
        { $_ -in @('youtube-maintainer-fixture', 'x-maintainer-fixture', 'dedicated-account-cookie-login') } {
            Assert-ExactProperties -Value $case.details -Expected @('artifactFileName', 'artifactSha256', 'fixtureId') -Label $caseId
            Assert-OpaqueFixtureId -Value $case.details.fixtureId -Label "Fixture for $caseId"
            if ([string]$case.details.artifactFileName -cne $installerName -or
                [string]$case.details.artifactSha256 -cne [string]$installerAsset.sha256) {
                throw "$caseId is not bound to the exact installed candidate."
            }
        }
        'signed-app-update' {
            Assert-ExactProperties -Value $case.details -Expected @(
                'fromVersion', 'toVersion', 'manifestSha256', 'installerSha256'
            ) -Label $caseId
            $appFromVersion = Convert-ThreePartVersion -Value $case.details.fromVersion -Label 'App update source version'
            $appToVersion = Convert-ThreePartVersion -Value $case.details.toVersion -Label 'App update target version'
            if ($appFromVersion -ge $appToVersion -or
                [string]$case.details.toVersion -cne $ExpectedVersion -or
                [string]$case.details.manifestSha256 -cne [string]$appManifestAsset.sha256 -or
                [string]$case.details.installerSha256 -cne [string]$installerAsset.sha256) {
                throw 'The signed app-update case does not bind the expected transition and candidate bytes.'
            }
        }
        'signed-runtime-update-rollback' {
            Assert-ExactProperties -Value $case.details -Expected @(
                'fromVersion', 'toVersion', 'rollbackVersion', 'descriptorSha256', 'archiveSha256'
            ) -Label $caseId
            $runtimeFromVersion = Convert-ThreePartVersion -Value $case.details.fromVersion -Label 'Runtime update source version'
            $runtimeToVersion = Convert-ThreePartVersion -Value $case.details.toVersion -Label 'Runtime update target version'
            if ($runtimeFromVersion -ge $runtimeToVersion -or
                [string]$case.details.toVersion -cne [string]$runtimeDescriptor.runtimeVersion -or
                [string]$case.details.rollbackVersion -cne [string]$case.details.fromVersion -or
                [string]$case.details.descriptorSha256 -cne [string]$runtimeDescriptorAsset.sha256 -or
                [string]$case.details.archiveSha256 -cne [string]$runtimeArchiveAsset.sha256) {
                throw 'The signed runtime update/rollback case does not bind the expected transition and candidate bytes.'
            }
        }
    }
}
if ((@($seenCases.Keys | Sort-Object) -join "`n") -cne (@($requiredCaseIds | Sort-Object) -join "`n")) {
    throw 'Manual acceptance evidence is missing a required case.'
}

Write-Output "Verified Windows 11 manual acceptance evidence for candidate run $ExpectedCandidateRunId."
