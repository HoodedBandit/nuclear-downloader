[CmdletBinding()]
param(
    [string]$RuntimeVersion,
    [string]$OutputDirectory,
    [string]$KeyId = $env:NUCLEAR_UPDATE_KEY_ID,
    [string]$SignaturePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-CanonicalContainedPath {
    param(
        [Parameter(Mandatory)] [string]$Candidate,
        [Parameter(Mandatory)] [string]$Root
    )

    $rootFull = [System.IO.Path]::TrimEndingDirectorySeparator(
        [System.IO.Path]::GetFullPath($Root)
    )
    $candidateFull = [System.IO.Path]::GetFullPath($Candidate)
    $relative = [System.IO.Path]::GetRelativePath($rootFull, $candidateFull)
    if ([System.IO.Path]::IsPathRooted($relative) -or
        $relative -eq '..' -or
        $relative.StartsWith("..$([System.IO.Path]::DirectorySeparatorChar)", [System.StringComparison]::Ordinal) -or
        $relative.StartsWith("..$([System.IO.Path]::AltDirectorySeparatorChar)", [System.StringComparison]::Ordinal)) {
        throw "Release path must stay inside $rootFull"
    }

    $cursor = $rootFull
    if (Test-Path -LiteralPath $cursor) {
        $rootItem = Get-Item -LiteralPath $cursor -Force
        if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release root must not be a reparse point: $rootFull"
        }
    }
    foreach ($component in $relative.Split(
        @([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar),
        [System.StringSplitOptions]::RemoveEmptyEntries
    )) {
        $cursor = Join-Path $cursor $component
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Release path must not traverse a reparse point: $cursor"
            }
        }
    }
    return $candidateFull
}

function Assert-CanonicalSha256 {
    param([Parameter(Mandatory)] [string]$Value, [Parameter(Mandatory)] [string]$Label)
    if ($Value -cnotmatch '^[0-9a-f]{64}$') {
        throw "$Label must be 64 lowercase hexadecimal digits."
    }
}

function Remove-OwnedRuntimeStaging {
    param(
        [Parameter(Mandatory)] [string]$Candidate,
        [Parameter(Mandatory)] [string]$Root,
        [Parameter(Mandatory)] [string]$StagingId
    )

    $expectedName = "runtime-bundle-staging-$StagingId"
    $stagingPath = Get-CanonicalContainedPath -Candidate $Candidate -Root $Root
    if ([System.IO.Path]::GetFileName($stagingPath) -cne $expectedName) {
        throw "Refusing to remove a runtime staging directory with an unexpected name: $stagingPath"
    }
    if (-not (Test-Path -LiteralPath $stagingPath)) {
        return
    }

    $markerPath = Get-CanonicalContainedPath `
        -Candidate (Join-Path $stagingPath '.nuclear-runtime-package-v1') `
        -Root $Root
    $expectedMarker = "schemaVersion=1`nstagingId=$StagingId`n"

    # Revalidate the exact directory and ownership marker immediately before
    # recursive deletion. A missing, altered, or reparse marker fails closed.
    $stagingPath = Get-CanonicalContainedPath -Candidate $stagingPath -Root $Root
    $stagingItem = Get-Item -LiteralPath $stagingPath -Force
    if (-not $stagingItem.PSIsContainer -or
        ($stagingItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to remove a non-directory or reparse runtime staging path: $stagingPath"
    }
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw "Refusing to remove runtime staging without its ownership marker: $stagingPath"
    }
    $markerItem = Get-Item -LiteralPath $markerPath -Force
    if (($markerItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [System.IO.File]::ReadAllText($markerPath) -cne $expectedMarker) {
        throw "Refusing to remove runtime staging with an invalid ownership marker: $stagingPath"
    }

    Remove-Item -LiteralPath $stagingPath -Recurse -Force
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$tauriRoot = Join-Path $repositoryRoot 'nuclear-app\src-tauri'
$binaryRoot = Join-Path $tauriRoot 'binaries'
$targetRoot = Join-Path $tauriRoot 'target'
$lockPath = Join-Path $tauriRoot 'sidecars.lock.json'

if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
    throw "Missing checked sidecar lock manifest: $lockPath"
}
$lock = Get-Content -Raw -LiteralPath $lockPath | ConvertFrom-Json
if ($lock.schemaVersion -ne 1 -or $lock.platform -cne 'windows-x86_64') {
    throw 'sidecars.lock.json must use schemaVersion 1 and platform windows-x86_64.'
}
if (-not $lock.sidecars -or @($lock.sidecars).Count -ne 4) {
    throw 'sidecars.lock.json must contain exactly four sidecars.'
}

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $targetRoot 'release-artifacts'
}
New-Item -ItemType Directory -Force -Path $targetRoot | Out-Null
$targetRoot = (Resolve-Path -LiteralPath $targetRoot).Path
$outputRoot = Get-CanonicalContainedPath -Candidate $OutputDirectory -Root $targetRoot
$stagingId = [System.Guid]::NewGuid().ToString('D')
$stagingRoot = Get-CanonicalContainedPath `
    -Candidate (Join-Path $targetRoot "runtime-bundle-staging-$stagingId") `
    -Root $targetRoot

$expectedTools = [ordered]@{
    'yt-dlp' = [pscustomobject]@{ Target = 'yt-dlp.exe'; VersionArgs = @('--version') }
    'ffmpeg' = [pscustomobject]@{ Target = 'ffmpeg.exe'; VersionArgs = @('-version') }
    'ffprobe' = [pscustomobject]@{ Target = 'ffprobe.exe'; VersionArgs = @('-version') }
    'deno' = [pscustomobject]@{ Target = 'deno.exe'; VersionArgs = @('--version') }
}

$tools = foreach ($name in $expectedTools.Keys) {
    $entries = @($lock.sidecars | Where-Object { $_.name -ceq $name })
    if ($entries.Count -ne 1) {
        throw "sidecars.lock.json must contain one exact entry for $name."
    }
    $entry = $entries[0]
    foreach ($requiredField in @('sourceUrl', 'version', 'license', 'architecture', 'filename', 'sha256')) {
        $value = [string]$entry.$requiredField
        if ([string]::IsNullOrWhiteSpace($value)) {
            throw "Sidecar $name is missing $requiredField."
        }
    }
    $sourceUri = [System.Uri]$entry.sourceUrl
    if (-not $sourceUri.IsAbsoluteUri -or $sourceUri.Scheme -cne 'https' -or -not [string]::IsNullOrEmpty($sourceUri.UserInfo)) {
        throw "Sidecar $name must use an absolute HTTPS source URL without credentials."
    }
    if ($entry.architecture -cne 'x86_64-pc-windows-msvc') {
        throw "Sidecar $name has unsupported architecture $($entry.architecture)."
    }
    if ($entry.filename -cnotmatch '^[A-Za-z0-9._-]+\.exe$') {
        throw "Sidecar $name has an unsafe filename."
    }
    Assert-CanonicalSha256 -Value ([string]$entry.sha256) -Label "Sidecar $name SHA-256"
    $sourcePath = Get-CanonicalContainedPath `
        -Candidate (Join-Path $binaryRoot ([string]$entry.filename)) `
        -Root $binaryRoot
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
        throw "Missing runtime binary: $sourcePath"
    }
    $sourceItem = Get-Item -LiteralPath $sourcePath -Force
    if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Runtime binary must not be a reparse point: $sourcePath"
    }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $sourcePath).Hash.ToLowerInvariant()
    if ($actualHash -cne [string]$entry.sha256) {
        throw "Runtime binary $name does not match sidecars.lock.json."
    }
    [pscustomobject]@{
        Name = $name
        SourcePath = $sourcePath
        Target = $expectedTools[$name].Target
        VersionArgs = $expectedTools[$name].VersionArgs
        LockedVersion = [string]$entry.version
        License = [string]$entry.license
        SourceUrl = [string]$entry.sourceUrl
        Sha256 = [string]$entry.sha256
    }
}

if (-not $RuntimeVersion) {
    $RuntimeVersion = $tools[0].LockedVersion
}
if ($RuntimeVersion -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "Runtime version must contain exactly three numeric dotted components: $RuntimeVersion"
}
if ([string]::IsNullOrWhiteSpace($KeyId) -or $KeyId -cnotmatch '^[A-Za-z0-9._-]{1,64}$') {
    throw 'NUCLEAR_UPDATE_KEY_ID (or -KeyId) must use 1-64 ASCII letters, digits, dot, underscore, or hyphen.'
}

$stagingOwned = $false
$packageError = $null
$cleanupError = $null
$packagedOutputs = @()
$signatureCreated = $false
$descriptorName = 'nuclear-downloader-runtime-windows-x64.json'
try {
    if (Test-Path -LiteralPath $stagingRoot) {
        throw "Refusing to reuse an existing runtime staging directory: $stagingRoot"
    }
    New-Item -ItemType Directory -Path $stagingRoot | Out-Null
    $stagingMarkerPath = Get-CanonicalContainedPath `
        -Candidate (Join-Path $stagingRoot '.nuclear-runtime-package-v1') `
        -Root $targetRoot
    $stagingMarker = "schemaVersion=1`nstagingId=$stagingId`n"
    $markerBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($stagingMarker)
    $markerStream = [System.IO.FileStream]::new(
        $stagingMarkerPath,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        $markerStream.Write($markerBytes, 0, $markerBytes.Length)
        $markerStream.Flush($true)
    } finally {
        $markerStream.Dispose()
    }
    $stagingOwned = $true
    New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null

    $manifestTools = foreach ($tool in $tools) {
        $targetPath = Join-Path $stagingRoot $tool.Target
        Copy-Item -LiteralPath $tool.SourcePath -Destination $targetPath
        $version = (& $tool.SourcePath @($tool.VersionArgs) | Select-Object -First 1).Trim()
        if ([string]::IsNullOrWhiteSpace($version)) {
            throw "Could not probe $($tool.Name) version."
        }
        [ordered]@{
            name = $tool.Name
            version = $version
            path = $tool.Target
            sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $targetPath).Hash.ToLowerInvariant()
        }
    }

    $manifest = [ordered]@{
        schemaVersion = 1
        runtimeVersion = $RuntimeVersion
        platform = 'windows-x64'
        tools = @($manifestTools)
    }
    $manifestPath = Join-Path $stagingRoot 'runtime-manifest.json'
    $manifestJson = $manifest | ConvertTo-Json -Depth 5
    [System.IO.File]::WriteAllText($manifestPath, $manifestJson, [System.Text.UTF8Encoding]::new($false))
    $manifestHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $manifestPath).Hash.ToLowerInvariant()

    $archiveName = "nuclear-downloader-runtime-$RuntimeVersion-windows-x64.zip"
    $archivePath = Get-CanonicalContainedPath -Candidate (Join-Path $outputRoot $archiveName) -Root $targetRoot
    $checksumPath = Get-CanonicalContainedPath -Candidate "$archivePath.sha256" -Root $targetRoot
    Remove-Item -LiteralPath $archivePath, $checksumPath -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path (Join-Path $stagingRoot '*') -DestinationPath $archivePath -CompressionLevel Optimal
    $archiveItem = Get-Item -LiteralPath $archivePath
    if ($archiveItem.Length -le 0 -or $archiveItem.Length -gt 1GB) {
        throw 'Runtime archive is empty or exceeds the 1 GiB compressed-size limit.'
    }
    $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText(
        $checksumPath,
        "$archiveHash  $archiveName`n",
        [System.Text.Encoding]::ASCII
    )

    $descriptorPath = Get-CanonicalContainedPath -Candidate (Join-Path $outputRoot $descriptorName) -Root $targetRoot
    $descriptor = [ordered]@{
        schemaVersion = 1
        keyId = $KeyId
        runtimeVersion = $RuntimeVersion
        platform = 'windows-x64'
        archiveName = $archiveName
        compressedSize = [long]$archiveItem.Length
        sha256 = $archiveHash
        manifestSha256 = $manifestHash
    }
    $descriptorJson = $descriptor | ConvertTo-Json -Depth 4 -Compress
    [System.IO.File]::WriteAllText($descriptorPath, $descriptorJson, [System.Text.UTF8Encoding]::new($false))

    $expectedSignaturePath = "$descriptorPath.sig"
    if ($SignaturePath) {
        $sourceSignature = (Resolve-Path -LiteralPath $SignaturePath).Path
        $signatureItem = Get-Item -LiteralPath $sourceSignature -Force
        if (($signatureItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or $signatureItem.Length -le 0 -or $signatureItem.Length -gt 8KB) {
            throw 'Runtime descriptor signature must be a regular non-reparse file no larger than 8 KiB.'
        }
        Copy-Item -LiteralPath $sourceSignature -Destination $expectedSignaturePath -Force
    } else {
        Remove-Item -LiteralPath $expectedSignaturePath -Force -ErrorAction SilentlyContinue
    }

    $packagedOutputs = @($archivePath, $checksumPath, $descriptorPath)
    $signatureCreated = Test-Path -LiteralPath $expectedSignaturePath -PathType Leaf
    if ($signatureCreated) {
        $packagedOutputs += $expectedSignaturePath
    }
} catch {
    $packageError = $_
} finally {
    if ($stagingOwned) {
        try {
            Remove-OwnedRuntimeStaging -Candidate $stagingRoot -Root $targetRoot -StagingId $stagingId
        } catch {
            $cleanupError = $_
        }
    }
}

if ($packageError) {
    if ($cleanupError) {
        throw "$($packageError.Exception.Message) Cleanup also failed: $($cleanupError.Exception.Message)"
    }
    throw $packageError
}
if ($cleanupError) {
    throw $cleanupError
}
$packagedOutputs | Write-Output
if (-not $signatureCreated) {
    Write-Warning "Descriptor created but not signed. Sign its exact bytes with the protected Tauri key and publish as $descriptorName.sig."
}
