[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $TrustedRepositoryRoot,
    [Parameter(Mandatory)] [string] $ArchivedRepositoryRoot,
    [Parameter(Mandatory)] [string[]] $ArtifactDirectories,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommit,
    [Parameter(Mandatory)] [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$utf8NoBom = [Text.UTF8Encoding]::new($false)

function Contained([string] $Candidate, [string] $Root) {
    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $full = [IO.Path]::GetFullPath($Candidate)
    $relative = [IO.Path]::GetRelativePath($rootFull, $full)
    if ([IO.Path]::IsPathRooted($relative) -or $relative -eq '..' -or $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)")) {
        throw "Path escapes its required root: $full"
    }
    $full
}
function FileIdentity([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Expected a regular non-reparse file: $Path"
    }
    [ordered]@{ size=[long]$item.Length; sha256=(Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
}
function ReadJson([string] $Path, [long] $Limit) {
    $identity = FileIdentity $Path
    if ($identity.size -gt $Limit) { throw "JSON input exceeds its limit: $Path" }
    Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
}
function RelativeSlash([string] $Path, [string] $Root) {
    [IO.Path]::GetRelativePath($Root, [IO.Path]::GetFullPath($Path)).Replace('\','/')
}

$trustedRoot = [IO.Path]::GetFullPath($TrustedRepositoryRoot)
$archiveRoot = [IO.Path]::GetFullPath($ArchivedRepositoryRoot)
$targetRoot = Join-Path $trustedRoot 'target'
$outputRoot = Contained $OutputDirectory $targetRoot
if (Test-Path -LiteralPath $outputRoot) { throw "Refusing to reuse correction output: $outputRoot" }
$gitRoot = ([string](& git -C $trustedRoot rev-parse --show-toplevel)).Trim()
if ($LASTEXITCODE -ne 0 -or -not [IO.Path]::GetFullPath($gitRoot).Equals($trustedRoot.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Trusted repository root is not an exact Git worktree root.'
}
$resolvedCommit = ([string](& git -C $trustedRoot rev-parse "$ExpectedCommit^{commit}")).Trim()
if ($LASTEXITCODE -ne 0 -or $resolvedCommit -cne $ExpectedCommit) { throw 'Expected source commit did not resolve exactly.' }
$tree = ([string](& git -C $trustedRoot rev-parse "$ExpectedCommit^{tree}")).Trim()
if ($LASTEXITCODE -ne 0 -or $tree -cnotmatch '^[0-9a-f]{40}$') { throw 'Could not resolve the expected source tree.' }

$artifactRecords = @()
$canonicalManifestHash = $null
$canonicalProductionHash = $null
$manifest = $null
$suite = $null
$correctionCount = 0
foreach ($directory in $ArtifactDirectories) {
    $artifactRoot = Contained $directory $archiveRoot
    $receiptPath = Join-Path $artifactRoot 'run-receipt.json'
    $manifestPath = Join-Path $artifactRoot 'production-manifest.json'
    $receiptIdentity = FileIdentity $receiptPath
    $manifestIdentity = FileIdentity $manifestPath
    $receipt = ReadJson $receiptPath 1MB
    if ([string]$receipt.suite -notin @('visual','performance')) {
        throw "Only visual and performance renderer evidence can be corrected: $receiptPath"
    }
    if ($null -eq $suite) { $suite = [string]$receipt.suite }
    if ([string]$receipt.suite -cne $suite) { throw 'A correction group must contain one renderer suite.' }
    $resultFiles = @(if ($suite -ceq 'visual') {
        Get-ChildItem -LiteralPath $artifactRoot -File | Where-Object Name -match '^visual-(100|150)\.json$'
    } else {
        Get-ChildItem -LiteralPath $artifactRoot -File | Where-Object Name -ceq 'performance.json'
    })
    if ($resultFiles.Count -ne 1) { throw "Expected one $suite result artifact in $artifactRoot" }
    $resultIdentity = FileIdentity $resultFiles[0].FullName
    $currentManifest = ReadJson $manifestPath 1MB
    $result = ReadJson $resultFiles[0].FullName 8MB
    $manifestBytes = $utf8NoBom.GetBytes(($currentManifest.files | ConvertTo-Json -Depth 4 -Compress))
    $calculatedProductionHash = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData($manifestBytes)
    ).ToLowerInvariant()
    if ([string]$receipt.schemaVersion -cne 'renderer-check-receipt/v1' -or [int]$receipt.exitCode -ne 0) {
        throw "Original renderer receipt is not a successful v1 receipt: $receiptPath"
    }
    if ([string]$currentManifest.schemaVersion -cne 'renderer-input-manifest/v1' -or
        [string]$currentManifest.kind -cne 'production' -or
        [string]$currentManifest.aggregateHash -cne $calculatedProductionHash -or
        [string]$receipt.source.manifest.sha256 -cne $manifestIdentity.sha256 -or
        [string]$receipt.source.productionHash -cne [string]$currentManifest.aggregateHash) {
        throw "Original receipt and production manifest links do not match: $artifactRoot"
    }
    if ($suite -ceq 'visual') {
        if ([string]$result.source.productionHash -cne [string]$currentManifest.aggregateHash -or
            [string]$result.source.commit -cne [string]$receipt.source.commit) {
            throw "Original visual result links do not match its receipt: $artifactRoot"
        }
    } elseif ([string]$result.schemaVersion -cne 'renderer-performance/v1' -or
        [int]$result.measured.queueSize -ne [int]$receipt.queueSize) {
        throw "Original performance result does not match its receipt queue: $artifactRoot"
    }
    $commitCorrectionRequired = [string]$receipt.source.commit -cne $ExpectedCommit
    if ($commitCorrectionRequired) { $correctionCount++ }
    if ($null -eq $manifest) {
        $manifest = $currentManifest
        $canonicalManifestHash = $manifestIdentity.sha256
        $canonicalProductionHash = [string]$currentManifest.aggregateHash
    } elseif ($manifestIdentity.sha256 -cne $canonicalManifestHash -or [string]$currentManifest.aggregateHash -cne $canonicalProductionHash) {
        throw 'Visual artifacts do not share the same frozen production manifest.'
    }
    $artifactRecords += [ordered]@{
        directory = RelativeSlash $artifactRoot $archiveRoot
        originalReportedCommit = [string]$receipt.source.commit
        commitCorrectionRequired = $commitCorrectionRequired
        suite = $suite
        queueSize = [int]$receipt.queueSize
        runReceipt = [ordered]@{ path='run-receipt.json'; size=$receiptIdentity.size; sha256=$receiptIdentity.sha256 }
        productionManifest = [ordered]@{ path='production-manifest.json'; size=$manifestIdentity.size; sha256=$manifestIdentity.sha256 }
        resultArtifact = [ordered]@{ path=$resultFiles[0].Name; size=$resultIdentity.size; sha256=$resultIdentity.sha256 }
    }
}
if ($correctionCount -eq 0) { throw 'Correction group contains no incorrectly reported commits.' }

$paths = @($manifest.files | ForEach-Object { [string]$_.path })
if ($paths.Count -eq 0 -or @($paths | Where-Object { $_ -notmatch '^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.@+-]+)*$' }).Count -ne 0) {
    throw 'Production manifest contains an empty or unsafe path set.'
}
New-Item -ItemType Directory -Path $outputRoot | Out-Null
[IO.File]::WriteAllText((Join-Path $outputRoot '.nuclear-provenance-correction-owned'), $ExpectedCommit, $utf8NoBom)
$zipPath = Join-Path $outputRoot 'verified-source.zip'
$extractRoot = Join-Path $outputRoot 'verified-source'
$gitArgs = @('-C',$trustedRoot,'archive','--format=zip',"--output=$zipPath",$ExpectedCommit,'--') + $paths
& git @gitArgs
if ($LASTEXITCODE -ne 0) { throw 'Could not archive the expected production inputs from Git.' }
Expand-Archive -LiteralPath $zipPath -DestinationPath $extractRoot
foreach ($entry in $manifest.files) {
    $archivedPath = Contained (Join-Path $extractRoot ([string]$entry.path)) $extractRoot
    $identity = FileIdentity $archivedPath
    if ($identity.size -ne [long]$entry.size -or $identity.sha256 -cne [string]$entry.sha256) {
        throw "Frozen production input does not match commit $ExpectedCommit`: $($entry.path)"
    }
}

$before = @($artifactRecords | ForEach-Object { $_.runReceipt.sha256; $_.productionManifest.sha256; $_.resultArtifact.sha256 })
$after = @()
foreach ($record in $artifactRecords) {
    $root = Join-Path $archiveRoot $record.directory
    $after += (FileIdentity (Join-Path $root $record.runReceipt.path)).sha256
    $after += (FileIdentity (Join-Path $root $record.productionManifest.path)).sha256
    $after += (FileIdentity (Join-Path $root $record.resultArtifact.path)).sha256
}
if (($before -join "`n") -cne ($after -join "`n")) { throw 'Original baseline evidence changed during provenance verification.' }

$correction = [ordered]@{
    schemaVersion = 'renderer-baseline-provenance-correction/v1'
    createdAtUtc = [DateTimeOffset]::UtcNow.ToString('o')
    correctionOnly = $true
    testsRerun = $false
    originalEvidenceModified = $false
    verifiedActualCommit = $ExpectedCommit
    verifiedActualTree = $tree
    suite = $suite
    productionHash = $canonicalProductionHash
    verification = [ordered]@{
        method = 'git-archive-file-by-file-sha256'
        archive = [ordered]@{ path='verified-source.zip'; size=(FileIdentity $zipPath).size; sha256=(FileIdentity $zipPath).sha256 }
        fileCount = $paths.Count
    }
    correctedArtifacts = @($artifactRecords)
    statement = 'One or more original source.commit fields were inherited from a parent worktree and are wrong. This companion verifies the shared frozen production bytes against verifiedActualCommit; it is not a test rerun.'
}
$receiptPath = Join-Path $outputRoot 'provenance-correction-receipt.json'
[IO.File]::WriteAllText($receiptPath, (($correction | ConvertTo-Json -Depth 10) + "`n"), $utf8NoBom)
Write-Output $receiptPath
