[CmdletBinding()]
param(
    [ValidatePattern('^0\.6\.0$')]
    [string] $Version = '0.6.0',

    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $RuntimeVersion = '2026.07.04',

    [string] $OutputDirectory,
    [string] $PublishedAt,
    [string] $KeyId = $env:NUCLEAR_UPDATE_KEY_ID
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:ExpectedVersion = '0.6.0'
$script:AppPlatform = 'windows-x86_64'
$script:RuntimePlatform = 'windows-x64'
$script:Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Get-CanonicalContainedPath {
    param(
        [Parameter(Mandatory)] [string] $Candidate,
        [Parameter(Mandatory)] [string] $Root
    )

    $rootFull = [System.IO.Path]::TrimEndingDirectorySeparator(
        [System.IO.Path]::GetFullPath($Root)
    )
    $candidateFull = [System.IO.Path]::GetFullPath($Candidate)
    $relative = [System.IO.Path]::GetRelativePath($rootFull, $candidateFull)
    $parentPrefix = "..$([System.IO.Path]::DirectorySeparatorChar)"
    $alternateParentPrefix = "..$([System.IO.Path]::AltDirectorySeparatorChar)"
    if ([System.IO.Path]::IsPathRooted($relative) -or
        $relative -eq '..' -or
        $relative.StartsWith($parentPrefix, [System.StringComparison]::Ordinal) -or
        $relative.StartsWith($alternateParentPrefix, [System.StringComparison]::Ordinal)) {
        throw "Path must stay inside $rootFull"
    }

    $cursor = $rootFull
    foreach ($component in $relative.Split(
        @([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar),
        [System.StringSplitOptions]::RemoveEmptyEntries
    )) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Path must not traverse a reparse point: $cursor"
            }
        }
        $cursor = Join-Path $cursor $component
    }
    if (Test-Path -LiteralPath $cursor) {
        $item = Get-Item -LiteralPath $cursor -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Path must not be a reparse point: $cursor"
        }
    }
    return $candidateFull
}

function Assert-RegularFile {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Root
    )

    $canonical = Get-CanonicalContainedPath -Candidate $Path -Root $Root
    if (-not (Test-Path -LiteralPath $canonical -PathType Leaf)) {
        throw "Required file is missing: $canonical"
    }
    $item = Get-Item -LiteralPath $canonical -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Required file must not be a reparse point: $canonical"
    }
    return $item
}

function Get-CommandLineVersion {
    param(
        [Parameter(Mandatory)] [string] $Executable,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    $lines = @(& $Executable @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0 -or $lines.Count -eq 0) {
        throw "Could not determine the $Executable version."
    }
    return ([string]$lines[0]).Trim()
}

function Invoke-TauriSignature {
    param(
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string] $AppRoot
    )

    if ([string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY)) {
        throw 'TAURI_SIGNING_PRIVATE_KEY must be supplied by the protected release-candidate environment.'
    }
    if ([string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD)) {
        throw 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD must be supplied by the protected release-candidate environment.'
    }

    $signaturePath = "$FilePath.sig"
    if (Test-Path -LiteralPath $signaturePath) {
        throw "Refusing to overwrite an existing signature: $signaturePath"
    }

    Push-Location $AppRoot
    try {
        # The key and password are read from Tauri's documented environment
        # variables. They are deliberately never placed on the command line or
        # copied into pipeline output.
        & npm.cmd exec tauri signer sign -- $FilePath *> $null
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri failed to sign $([System.IO.Path]::GetFileName($FilePath))."
        }
    } finally {
        Pop-Location
    }

    $signature = Assert-RegularFile -Path $signaturePath -Root ([System.IO.Path]::GetDirectoryName($FilePath))
    if ($signature.Length -le 0 -or $signature.Length -gt 8KB) {
        throw "Generated signature has an invalid size: $signaturePath"
    }
    return $signature.FullName
}

function Write-CanonicalJson {
    param(
        [Parameter(Mandatory)] [object] $Value,
        [Parameter(Mandatory)] [string] $Path,
        [int] $Depth = 8
    )

    $json = $Value | ConvertTo-Json -Depth $Depth -Compress
    [System.IO.File]::WriteAllText($Path, $json, $script:Utf8NoBom)
}

function Get-LowerSha256 {
    param([Parameter(Mandatory)] [string] $Path)
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Assert-VersionParity {
    param(
        [Parameter(Mandatory)] [string] $RepositoryRoot,
        [Parameter(Mandatory)] [string] $ExpectedVersion
    )

    $packagePath = Join-Path $RepositoryRoot 'nuclear-app\package.json'
    $tauriPath = Join-Path $RepositoryRoot 'nuclear-app\src-tauri\tauri.conf.json'
    $cargoPath = Join-Path $RepositoryRoot 'nuclear-app\src-tauri\Cargo.toml'
    $packageVersion = [string](Get-Content -Raw -LiteralPath $packagePath | ConvertFrom-Json).version
    $tauriVersion = [string](Get-Content -Raw -LiteralPath $tauriPath | ConvertFrom-Json).version
    $cargoVersionMatch = [regex]::Match(
        (Get-Content -Raw -LiteralPath $cargoPath),
        '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"'
    )
    if (-not $cargoVersionMatch.Success) {
        throw 'Could not read the Cargo package version.'
    }
    $versions = @($packageVersion, $tauriVersion, $cargoVersionMatch.Groups[1].Value)
    if (@($versions | Where-Object { $_ -cne $ExpectedVersion }).Count -ne 0) {
        throw "Version parity failed. package.json=$packageVersion, tauri.conf.json=$tauriVersion, Cargo.toml=$($cargoVersionMatch.Groups[1].Value), expected=$ExpectedVersion"
    }
}

if ($Version -cne $script:ExpectedVersion) {
    throw "This release pipeline is intentionally pinned to $script:ExpectedVersion."
}
if ([string]::IsNullOrWhiteSpace($KeyId) -or $KeyId -cnotmatch '^[A-Za-z0-9._-]{1,64}$') {
    throw "NUCLEAR_UPDATE_KEY_ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
}
if ($KeyId -cne $env:NUCLEAR_UPDATE_KEY_ID) {
    throw 'The manifest KeyId must exactly match the NUCLEAR_UPDATE_KEY_ID embedded by build.rs.'
}
if ([string]::IsNullOrWhiteSpace($env:NUCLEAR_UPDATE_PUBLIC_KEY)) {
    throw 'NUCLEAR_UPDATE_PUBLIC_KEY must contain the public key embedded by build.rs.'
}
$nextKeyIdMissing = [string]::IsNullOrWhiteSpace($env:NUCLEAR_UPDATE_NEXT_KEY_ID)
$nextPublicKeyMissing = [string]::IsNullOrWhiteSpace($env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY)
if ($nextKeyIdMissing -ne $nextPublicKeyMissing) {
    throw 'NUCLEAR_UPDATE_NEXT_KEY_ID and NUCLEAR_UPDATE_NEXT_PUBLIC_KEY must be configured together.'
}
if (-not $nextKeyIdMissing) {
    if ($env:NUCLEAR_UPDATE_NEXT_KEY_ID -cnotmatch '^[A-Za-z0-9._-]{1,64}$') {
        throw "NUCLEAR_UPDATE_NEXT_KEY_ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
    }
    if ($env:NUCLEAR_UPDATE_NEXT_KEY_ID -ceq $KeyId) {
        throw 'The current and next updater key IDs must be different.'
    }
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$tauriRoot = Join-Path $appRoot 'src-tauri'
$targetRoot = Join-Path $tauriRoot 'target'
New-Item -ItemType Directory -Force -Path $targetRoot | Out-Null
$targetRoot = (Resolve-Path -LiteralPath $targetRoot).Path

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $targetRoot "release-candidate-$Version"
}
$outputRoot = Get-CanonicalContainedPath -Candidate $OutputDirectory -Root $targetRoot
if (Test-Path -LiteralPath $outputRoot) {
    throw "Candidate output already exists; use a new empty path: $outputRoot"
}

$gitStatus = @(& git -C $repositoryRoot status --porcelain)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not inspect the Git worktree.'
}
if ($gitStatus.Count -ne 0) {
    throw 'Release candidates must be built from a clean worktree.'
}
$commitSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim()
if ($LASTEXITCODE -ne 0 -or $commitSha -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Could not determine the exact source commit.'
}

Assert-VersionParity -RepositoryRoot $repositoryRoot -ExpectedVersion $Version

if (-not $PublishedAt) {
    $PublishedAt = [DateTimeOffset]::UtcNow.ToString(
        'yyyy-MM-ddTHH:mm:ssZ',
        [System.Globalization.CultureInfo]::InvariantCulture
    )
}
if ($PublishedAt -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$') {
    throw 'PublishedAt must use canonical UTC form yyyy-MM-ddTHH:mm:ssZ.'
}
$parsedPublishedAt = [DateTimeOffset]::MinValue
if (-not [DateTimeOffset]::TryParseExact(
    $PublishedAt,
    'yyyy-MM-ddTHH:mm:ssZ',
    [System.Globalization.CultureInfo]::InvariantCulture,
    [System.Globalization.DateTimeStyles]::AssumeUniversal,
    [ref]$parsedPublishedAt
)) {
    throw 'PublishedAt is not a valid UTC timestamp.'
}

$stagingRoot = Get-CanonicalContainedPath `
    -Candidate (Join-Path $targetRoot "release-candidate-staging-$([Guid]::NewGuid().ToString('N'))") `
    -Root $targetRoot
$stagingMarker = Join-Path $stagingRoot '.nuclear-release-staging'

New-Item -ItemType Directory -Path $outputRoot, $stagingRoot | Out-Null
[System.IO.File]::WriteAllText($stagingMarker, $Version, $script:Utf8NoBom)

try {
    Push-Location $appRoot
    try {
        & npm.cmd exec tauri build -- --no-sign -b nsis
        if ($LASTEXITCODE -ne 0) {
            throw 'The Tauri NSIS build failed.'
        }
    } finally {
        Pop-Location
    }

    $bundleRoot = Join-Path $tauriRoot 'target\release\bundle\nsis'
    $installerCandidates = @(
        Get-ChildItem -LiteralPath $bundleRoot -File |
            Where-Object { $_.Name -ceq "Nuclear Downloader_${Version}_x64-setup.exe" }
    )
    if ($installerCandidates.Count -ne 1) {
        throw "Expected one exact Tauri NSIS output for $Version; found $($installerCandidates.Count)."
    }

    $installerName = "Nuclear.Downloader_${Version}_x64-setup.exe"
    $installerPath = Join-Path $outputRoot $installerName
    Copy-Item -LiteralPath $installerCandidates[0].FullName -Destination $installerPath
    $installerItem = Assert-RegularFile -Path $installerPath -Root $outputRoot
    if ($installerItem.Length -le 0 -or $installerItem.Length -gt 1GB) {
        throw 'The NSIS installer is empty or exceeds the 1 GiB updater limit.'
    }
    $installerHash = Get-LowerSha256 -Path $installerPath

    $portableStage = Join-Path $stagingRoot 'portable'
    New-Item -ItemType Directory -Path $portableStage | Out-Null
    $releaseRoot = Join-Path $tauriRoot 'target\release'
    foreach ($portableFile in @('nuclear.exe', 'yt-dlp.exe', 'ffmpeg.exe', 'ffprobe.exe', 'deno.exe')) {
        $sourceItem = Assert-RegularFile -Path (Join-Path $releaseRoot $portableFile) -Root $releaseRoot
        Copy-Item -LiteralPath $sourceItem.FullName -Destination (Join-Path $portableStage $portableFile)
    }
    $portableName = "Nuclear.Downloader_${Version}_x64-portable.zip"
    $portablePath = Join-Path $outputRoot $portableName
    Compress-Archive -LiteralPath @(
        Join-Path $portableStage 'nuclear.exe'
        Join-Path $portableStage 'yt-dlp.exe'
        Join-Path $portableStage 'ffmpeg.exe'
        Join-Path $portableStage 'ffprobe.exe'
        Join-Path $portableStage 'deno.exe'
    ) -DestinationPath $portablePath -CompressionLevel Optimal
    $portableItem = Assert-RegularFile -Path $portablePath -Root $outputRoot
    if ($portableItem.Length -le 0 -or $portableItem.Length -gt 4GB) {
        throw 'The portable ZIP is empty or exceeds the 4 GiB release limit.'
    }

    $manifestName = "nuclear-downloader-v${Version}-update.json"
    $manifestPath = Join-Path $outputRoot $manifestName
    $manifest = [ordered]@{
        schemaVersion = 1
        keyId = $KeyId
        version = $Version
        platform = $script:AppPlatform
        publishedAt = $PublishedAt
        installer = [ordered]@{
            fileName = $installerName
            size = [long]$installerItem.Length
            sha256 = $installerHash
        }
    }
    Write-CanonicalJson -Value $manifest -Path $manifestPath
    if ((Get-Item -LiteralPath $manifestPath).Length -gt 64KB) {
        throw 'The app update manifest exceeds the 64 KiB client limit.'
    }
    Invoke-TauriSignature -FilePath $manifestPath -AppRoot $appRoot | Out-Null

    $legacyName = "nuclear-downloader-v${Version}-sha256.txt"
    $legacyPath = Join-Path $outputRoot $legacyName
    [System.IO.File]::WriteAllText(
        $legacyPath,
        "$installerHash  $installerName`n",
        [System.Text.Encoding]::ASCII
    )

    & (Join-Path $repositoryRoot 'scripts\package-runtime.ps1') `
        -RuntimeVersion $RuntimeVersion `
        -OutputDirectory $outputRoot `
        -KeyId $KeyId | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw 'Runtime packaging failed.'
    }
    $runtimeDescriptorPath = Join-Path $outputRoot 'nuclear-downloader-runtime-windows-x64.json'
    Assert-RegularFile -Path $runtimeDescriptorPath -Root $outputRoot | Out-Null
    Invoke-TauriSignature -FilePath $runtimeDescriptorPath -AppRoot $appRoot | Out-Null

    $publicFilesBeforeChecksums = @(
        Get-ChildItem -LiteralPath $outputRoot -File |
            Sort-Object -Property Name
    )
    $shaSumsPath = Join-Path $outputRoot 'SHA256SUMS'
    $shaLines = foreach ($file in $publicFilesBeforeChecksums) {
        "$(Get-LowerSha256 -Path $file.FullName)  $($file.Name)"
    }
    [System.IO.File]::WriteAllText(
        $shaSumsPath,
        (($shaLines -join "`n") + "`n"),
        [System.Text.Encoding]::ASCII
    )

    $nodeVersion = Get-CommandLineVersion -Executable 'node.exe' -Arguments @('--version')
    $npmVersion = Get-CommandLineVersion -Executable 'npm.cmd' -Arguments @('--version')
    $rustcVersion = Get-CommandLineVersion -Executable 'rustc.exe' -Arguments @('--version')
    $cargoVersion = Get-CommandLineVersion -Executable 'cargo.exe' -Arguments @('--version')
    $publicFiles = @(Get-ChildItem -LiteralPath $outputRoot -File | Sort-Object -Property Name)
    $inventoryAssets = foreach ($file in $publicFiles) {
        [ordered]@{
            fileName = $file.Name
            size = [long]$file.Length
            sha256 = Get-LowerSha256 -Path $file.FullName
        }
    }
    $inventory = [ordered]@{
        schemaVersion = 1
        releaseVersion = $Version
        releaseTag = "v$Version"
        platform = $script:AppPlatform
        keyId = $KeyId
        sourceCommit = $commitSha
        createdAt = [DateTimeOffset]::UtcNow.ToString(
            'yyyy-MM-ddTHH:mm:ssZ',
            [System.Globalization.CultureInfo]::InvariantCulture
        )
        toolchains = [ordered]@{
            node = $nodeVersion
            npm = $npmVersion
            rustc = $rustcVersion
            cargo = $cargoVersion
        }
        assets = @($inventoryAssets)
    }
    Write-CanonicalJson -Value $inventory -Path (Join-Path $outputRoot 'release-candidate-inventory.json') -Depth 10

    & (Join-Path $repositoryRoot 'scripts\verify-release-candidate.ps1') `
        -CandidateDirectory $outputRoot `
        -ExpectedVersion $Version `
        -ExpectedCommitSha $commitSha
    if ($LASTEXITCODE -ne 0) {
        throw 'Release-candidate verification failed.'
    }

    Write-Output $outputRoot
} finally {
    if (Test-Path -LiteralPath $stagingRoot) {
        $canonicalStaging = Get-CanonicalContainedPath -Candidate $stagingRoot -Root $targetRoot
        $marker = Join-Path $canonicalStaging '.nuclear-release-staging'
        if ((Test-Path -LiteralPath $marker -PathType Leaf) -and
            (Get-Content -Raw -LiteralPath $marker) -ceq $Version) {
            Remove-Item -LiteralPath $canonicalStaging -Recurse -Force
        } else {
            Write-Warning "Staging cleanup skipped because the ownership marker was missing or invalid: $canonicalStaging"
        }
    }
}
