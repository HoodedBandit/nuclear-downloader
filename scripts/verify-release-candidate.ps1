[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $CandidateDirectory,

    [ValidatePattern('^0\.6\.0$')]
    [string] $ExpectedVersion = '0.6.0',

    [ValidatePattern('^$|^[0-9a-f]{40}$')]
    [string] $ExpectedCommitSha = '',

    [string] $CurrentKeyId = $env:NUCLEAR_UPDATE_KEY_ID,
    [string] $CurrentPublicKey = $env:NUCLEAR_UPDATE_PUBLIC_KEY,
    [string] $NextKeyId = $env:NUCLEAR_UPDATE_NEXT_KEY_ID,
    [string] $NextPublicKey = $env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

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
        throw "Candidate path escapes its root: $candidateFull"
    }
    return $candidateFull
}

function Get-CandidateFile {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $Root
    )

    if ($Name -cnotmatch '^[A-Za-z0-9._-]+$') {
        throw "Unsafe release asset name: $Name"
    }
    $path = Get-CanonicalContainedPath -Candidate (Join-Path $Root $Name) -Root $Root
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required candidate asset is missing: $Name"
    }
    $item = Get-Item -LiteralPath $path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Candidate assets must not be reparse points: $Name"
    }
    return $item
}

function Get-LowerSha256 {
    param([Parameter(Mandatory)] [string] $Path)
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-RuntimeArchiveManifestSha256 {
    param([Parameter(Mandatory)] [System.IO.FileInfo] $Archive)

    $stream = [System.IO.File]::Open(
        $Archive.FullName,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        $zip = [System.IO.Compression.ZipArchive]::new(
            $stream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        try {
            $manifestCandidates = @(
                $zip.Entries | Where-Object {
                    [System.IO.Path]::GetFileName($_.FullName) -ieq 'runtime-manifest.json'
                }
            )
            $exact = @(
                $manifestCandidates | Where-Object { $_.FullName -ceq 'runtime-manifest.json' }
            )
            if ($manifestCandidates.Count -ne 1 -or $exact.Count -ne 1) {
                throw 'The runtime archive must contain exactly one root runtime-manifest.json.'
            }
            $entry = $exact[0]
            if ($entry.Length -le 0 -or $entry.Length -gt 64KB) {
                throw 'The runtime archive manifest is empty or exceeds 64 KiB.'
            }
            $entryStream = $entry.Open()
            try {
                $sha256 = [System.Security.Cryptography.SHA256]::Create()
                try {
                    $hash = $sha256.ComputeHash($entryStream)
                    if ($entryStream.ReadByte() -ne -1) {
                        throw 'The runtime archive manifest exceeded its declared ZIP length.'
                    }
                    return ([Convert]::ToHexString($hash)).ToLowerInvariant()
                } finally {
                    $sha256.Dispose()
                }
            } finally {
                $entryStream.Dispose()
            }
        } finally {
            $zip.Dispose()
        }
    } catch {
        throw "The runtime archive could not be validated: $($_.Exception.Message)"
    } finally {
        $stream.Dispose()
    }
}

function Assert-ExactProperties {
    param(
        [Parameter(Mandatory)] [object] $Value,
        [Parameter(Mandatory)] [string[]] $Expected,
        [Parameter(Mandatory)] [string] $Label
    )

    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $wanted = @($Expected | Sort-Object)
    if (($actual -join "`n") -cne ($wanted -join "`n")) {
        throw "$Label has an unexpected field set. Expected [$($wanted -join ', ')], found [$($actual -join ', ')]."
    }
}

function Read-StrictJson {
    param(
        [Parameter(Mandatory)] [System.IO.FileInfo] $File,
        [Parameter(Mandatory)] [long] $MaximumBytes,
        [Parameter(Mandatory)] [string] $Label
    )

    if ($File.Length -le 0 -or $File.Length -gt $MaximumBytes) {
        throw "$Label is empty or exceeds its $MaximumBytes-byte limit."
    }
    $bytes = [System.IO.File]::ReadAllBytes($File.FullName)
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf) {
        throw "$Label must use UTF-8 without a byte-order mark."
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    try {
        $text = $utf8.GetString($bytes)
        $convertArguments = @{ InputObject = $text }
        if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) {
            $convertArguments.DateKind = 'String'
        }
        return ConvertFrom-Json @convertArguments
    } catch {
        throw "$Label is not strict UTF-8 JSON: $($_.Exception.Message)"
    }
}

function Assert-CanonicalSha256 {
    param(
        [Parameter(Mandatory)] [string] $Value,
        [Parameter(Mandatory)] [string] $Label
    )
    if ($Value -cnotmatch '^[0-9a-f]{64}$') {
        throw "$Label must contain exactly 64 lowercase hexadecimal digits."
    }
}

function Assert-SignatureFile {
    param(
        [Parameter(Mandatory)] [System.IO.FileInfo] $File,
        [Parameter(Mandatory)] [string] $Label
    )
    if ($File.Length -le 0 -or $File.Length -gt 8KB) {
        throw "$Label is empty or exceeds the 8 KiB client limit."
    }
    $bytes = [System.IO.File]::ReadAllBytes($File.FullName)
    if ($bytes -contains 0) {
        throw "$Label contains an invalid NUL byte."
    }
}

function Assert-KeyPairConfiguration {
    param(
        [string] $ConfiguredCurrentKeyId,
        [string] $ConfiguredCurrentPublicKey,
        [string] $ConfiguredNextKeyId,
        [string] $ConfiguredNextPublicKey
    )

    if ($ConfiguredCurrentKeyId -cnotmatch '^[A-Za-z0-9._-]{1,64}$' -or
        [string]::IsNullOrWhiteSpace($ConfiguredCurrentPublicKey)) {
        throw 'Current updater key verification requires a canonical key ID and non-empty public key.'
    }
    $nextIdMissing = [string]::IsNullOrWhiteSpace($ConfiguredNextKeyId)
    $nextKeyMissing = [string]::IsNullOrWhiteSpace($ConfiguredNextPublicKey)
    if ($nextIdMissing -ne $nextKeyMissing) {
        throw 'The next updater key ID and public key must be configured together.'
    }
    if (-not $nextIdMissing -and
        ($ConfiguredNextKeyId -cnotmatch '^[A-Za-z0-9._-]{1,64}$' -or
         $ConfiguredNextKeyId -ceq $ConfiguredCurrentKeyId)) {
        throw 'The next updater key ID must be canonical and distinct from the current key ID.'
    }
}

function Select-TrustedPublicKey {
    param(
        [Parameter(Mandatory)] [string] $ManifestKeyId,
        [Parameter(Mandatory)] [string] $ConfiguredCurrentKeyId,
        [Parameter(Mandatory)] [string] $ConfiguredCurrentPublicKey,
        [string] $ConfiguredNextKeyId,
        [string] $ConfiguredNextPublicKey
    )

    if ($ManifestKeyId -ceq $ConfiguredCurrentKeyId) {
        return $ConfiguredCurrentPublicKey
    }
    if (-not [string]::IsNullOrWhiteSpace($ConfiguredNextKeyId) -and
        $ManifestKeyId -ceq $ConfiguredNextKeyId) {
        return $ConfiguredNextPublicKey
    }
    throw "Manifest key ID '$ManifestKeyId' is not in the configured current/next trust set."
}

function ConvertFrom-TauriMinisignWrapper {
    param(
        [Parameter(Mandatory)] [string] $Value,
        [Parameter(Mandatory)] [string] $Label
    )

    $trimmed = $Value.Trim()
    if ($trimmed -cnotmatch '^[A-Za-z0-9+/]+={0,2}$') {
        throw "$Label is not canonical Tauri base64."
    }
    try {
        $decodedBytes = [Convert]::FromBase64String($trimmed)
        $decoded = [System.Text.UTF8Encoding]::new($false, $true).GetString($decodedBytes).Trim()
    } catch {
        throw "$Label is not valid Tauri base64-wrapped UTF-8: $($_.Exception.Message)"
    }
    if (-not $decoded.StartsWith('untrusted comment: ', [System.StringComparison]::Ordinal)) {
        throw "$Label did not decode to Minisign text."
    }
    return $decoded
}

function Invoke-MinisignVerification {
    param(
        [Parameter(Mandatory)] [string] $PublicKey,
        [Parameter(Mandatory)] [string] $MessagePath,
        [Parameter(Mandatory)] [string] $SignaturePath,
        [Parameter(Mandatory)] [string] $RepositoryRoot
    )

    $manifestPath = Join-Path $RepositoryRoot 'nuclear-app\src-tauri\Cargo.toml'
    $lockPath = Join-Path $RepositoryRoot 'nuclear-app\src-tauri\Cargo.lock'
    $lockText = Get-Content -Raw -LiteralPath $lockPath
    $lockedVerifierPattern = '(?ms)\[\[package\]\]\s+name = "minisign-verify"\s+version = "0\.2\.5"\s+source = "registry\+https://github\.com/rust-lang/crates\.io-index"\s+checksum = "22f9645cb765ea72b8111f36c522475d2daa0d22c957a9826437e97534bc4e9e"'
    $lockedVerifierEntries = [regex]::Matches($lockText, $lockedVerifierPattern)
    if ($lockedVerifierEntries.Count -ne 1) {
        throw 'Cargo.lock must contain one exact minisign-verify 0.2.5 registry package with the reviewed checksum.'
    }

    $metadataJson = @(
        & cargo metadata `
            --manifest-path $manifestPath `
            --format-version 1 `
            --locked `
            --offline
    ) -join "`n"
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($metadataJson)) {
        throw 'Could not resolve the locked Minisign verifier source. Run cargo fetch --locked first.'
    }
    $metadata = $metadataJson | ConvertFrom-Json
    $packages = @(
        $metadata.packages |
            Where-Object { [string]$_.name -ceq 'minisign-verify' -and [string]$_.version -ceq '0.2.5' }
    )
    if ($packages.Count -ne 1) {
        throw "Cargo metadata must resolve one exact minisign-verify 0.2.5 package; found $($packages.Count)."
    }
    if ([string]$packages[0].source -cne 'registry+https://github.com/rust-lang/crates.io-index') {
        throw 'The Minisign verifier must resolve from the Cargo.lock-pinned crates.io registry source.'
    }
    $crateRoot = Split-Path -Parent ([string]$packages[0].manifest_path)
    $crateSource = Join-Path $crateRoot 'src\lib.rs'
    if (-not (Test-Path -LiteralPath $crateSource -PathType Leaf)) {
        throw 'The locked Minisign verifier source is unavailable.'
    }

    $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-signature-verifier-$([Guid]::NewGuid().ToString('N'))"
    $markerPath = Join-Path $tempRoot '.nuclear-signature-verifier-owned'
    New-Item -ItemType Directory -Path $tempRoot | Out-Null
    [System.IO.File]::WriteAllText($markerPath, 'owned', [System.Text.UTF8Encoding]::new($false))
    try {
        $normalizedPublicKey = ConvertFrom-TauriMinisignWrapper `
            -Value $PublicKey `
            -Label 'Updater public key'
        $signatureText = [System.IO.File]::ReadAllText(
            $SignaturePath,
            [System.Text.UTF8Encoding]::new($false, $true)
        )
        $normalizedSignature = ConvertFrom-TauriMinisignWrapper `
            -Value $signatureText `
            -Label 'Detached signature'
        $normalizedSignaturePath = Join-Path $tempRoot 'decoded.sig'
        [System.IO.File]::WriteAllText(
            $normalizedSignaturePath,
            $normalizedSignature,
            [System.Text.UTF8Encoding]::new($false)
        )

        & rustc `
            --crate-name minisign_verify `
            --edition=2018 `
            --crate-type=rlib `
            --out-dir $tempRoot `
            $crateSource
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not compile the locked Minisign verifier library.'
        }
        $rlibs = @(Get-ChildItem -LiteralPath $tempRoot -File -Filter '*minisign_verify*.rlib')
        if ($rlibs.Count -ne 1) {
            throw 'The locked Minisign verifier compilation produced an unexpected output set.'
        }

        $helperSource = Join-Path $tempRoot 'verify.rs'
        $helperProgram = @'
use minisign_verify::{PublicKey, Signature};
use std::{env, fs, process};

fn main() {
    let arguments: Vec<String> = env::args().collect();
    if arguments.len() != 3 {
        eprintln!("signature verifier requires message and signature paths");
        process::exit(2);
    }
    let public_key_text = env::var("NUCLEAR_VERIFY_PUBLIC_KEY")
        .unwrap_or_else(|_| {
            eprintln!("signature verifier public key is missing");
            process::exit(2);
        });
    let public_key = PublicKey::from_base64(public_key_text.trim())
        .or_else(|_| PublicKey::decode(&public_key_text))
        .unwrap_or_else(|_| {
            eprintln!("signature verifier public key is invalid");
            process::exit(2);
        });
    let message = fs::read(&arguments[1]).unwrap_or_else(|_| {
        eprintln!("signature verifier could not read the message");
        process::exit(2);
    });
    let signature_text = fs::read_to_string(&arguments[2]).unwrap_or_else(|_| {
        eprintln!("signature verifier could not read the signature");
        process::exit(2);
    });
    let signature = Signature::decode(signature_text.trim()).unwrap_or_else(|_| {
        eprintln!("signature verifier could not decode the signature");
        process::exit(2);
    });
    if public_key.verify(&message, &signature, false).is_err() {
        eprintln!("signature verification failed");
        process::exit(1);
    }
}
'@
        [System.IO.File]::WriteAllText(
            $helperSource,
            $helperProgram,
            [System.Text.UTF8Encoding]::new($false)
        )
        $helperPath = Join-Path $tempRoot 'minisign-contract-verifier.exe'
        & rustc `
            --edition=2021 `
            --extern "minisign_verify=$($rlibs[0].FullName)" `
            -o $helperPath `
            $helperSource
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not compile the candidate signature-verification helper.'
        }

        $previousPublicKey = $env:NUCLEAR_VERIFY_PUBLIC_KEY
        try {
            $env:NUCLEAR_VERIFY_PUBLIC_KEY = $normalizedPublicKey
            & $helperPath $MessagePath $normalizedSignaturePath
            if ($LASTEXITCODE -ne 0) {
                throw "Detached signature verification failed for $([System.IO.Path]::GetFileName($MessagePath))."
            }
        } finally {
            $env:NUCLEAR_VERIFY_PUBLIC_KEY = $previousPublicKey
        }
    } finally {
        $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $canonicalVerifier = [System.IO.Path]::GetFullPath($tempRoot)
        $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalVerifier)
        $verifierItem = Get-Item -LiteralPath $canonicalVerifier -Force -ErrorAction SilentlyContinue
        $markerItem = Get-Item -LiteralPath $markerPath -Force -ErrorAction SilentlyContinue
        $isOwnedRegularTree = (
            $null -ne $verifierItem -and
            $verifierItem.PSIsContainer -and
            ($verifierItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0 -and
            $null -ne $markerItem -and
            -not $markerItem.PSIsContainer -and
            ($markerItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
        )
        if (-not [System.IO.Path]::IsPathRooted($relative) -and
            $relative -notmatch '^\.\.' -and
            (Split-Path -Leaf $canonicalVerifier) -cmatch '^nuclear-signature-verifier-[0-9a-f]{32}$' -and
            $isOwnedRegularTree -and
            (Get-Content -Raw -LiteralPath $markerPath) -ceq 'owned') {
            Remove-Item -LiteralPath $canonicalVerifier -Recurse -Force
        } else {
            Write-Warning "Temporary signature-verifier cleanup was skipped: $canonicalVerifier"
        }
    }
}

function Assert-VersionParity {
    param(
        [Parameter(Mandatory)] [string] $RepositoryRoot,
        [Parameter(Mandatory)] [string] $Version
    )

    $package = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\package.json') | ConvertFrom-Json
    $tauri = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\src-tauri\tauri.conf.json') | ConvertFrom-Json
    $cargoText = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\src-tauri\Cargo.toml')
    $cargoMatch = [regex]::Match($cargoText, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
    if (-not $cargoMatch.Success -or
        [string]$package.version -cne $Version -or
        [string]$tauri.version -cne $Version -or
        $cargoMatch.Groups[1].Value -cne $Version) {
        throw 'The candidate version does not match package.json, tauri.conf.json, and Cargo.toml.'
    }
}

if ($ExpectedVersion -cne '0.6.0') {
    throw 'This verification contract is intentionally pinned to release 0.6.0.'
}
Assert-KeyPairConfiguration `
    -ConfiguredCurrentKeyId $CurrentKeyId `
    -ConfiguredCurrentPublicKey $CurrentPublicKey `
    -ConfiguredNextKeyId $NextKeyId `
    -ConfiguredNextPublicKey $NextPublicKey

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Assert-VersionParity -RepositoryRoot $repositoryRoot -Version $ExpectedVersion

$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$candidateRootItem = Get-Item -LiteralPath $candidateRoot -Force
if (-not $candidateRootItem.PSIsContainer -or
    ($candidateRootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'The candidate root must be a real directory, not a reparse point.'
}
if (@(Get-ChildItem -LiteralPath $candidateRoot -Directory -Force).Count -ne 0) {
    throw 'The candidate directory must be flat and contain no subdirectories.'
}

$installerName = "Nuclear.Downloader_${ExpectedVersion}_x64-setup.exe"
$portableName = "Nuclear.Downloader_${ExpectedVersion}_x64-portable.zip"
$manifestName = "nuclear-downloader-v${ExpectedVersion}-update.json"
$legacyName = "nuclear-downloader-v${ExpectedVersion}-sha256.txt"
$runtimeDescriptorName = 'nuclear-downloader-runtime-windows-x64.json'
$inventoryName = 'release-candidate-inventory.json'

$installer = Get-CandidateFile -Name $installerName -Root $candidateRoot
if ($installer.Length -le 0 -or $installer.Length -gt 1GB) {
    throw 'The installer is empty or exceeds the 1 GiB updater limit.'
}
$installerHash = Get-LowerSha256 -Path $installer.FullName

$manifestFile = Get-CandidateFile -Name $manifestName -Root $candidateRoot
$manifest = Read-StrictJson -File $manifestFile -MaximumBytes 64KB -Label 'App update manifest'
Assert-ExactProperties -Value $manifest `
    -Expected @('schemaVersion', 'keyId', 'version', 'platform', 'publishedAt', 'installer') `
    -Label 'App update manifest'
Assert-ExactProperties -Value $manifest.installer `
    -Expected @('fileName', 'size', 'sha256') `
    -Label 'App update installer record'
if ($manifest.schemaVersion -ne 1 -or
    [string]$manifest.version -cne $ExpectedVersion -or
    [string]$manifest.platform -cne 'windows-x86_64' -or
    [string]$manifest.installer.fileName -cne $installerName -or
    [long]$manifest.installer.size -ne [long]$installer.Length -or
    [string]$manifest.installer.sha256 -cne $installerHash) {
    throw 'The signed app manifest does not exactly bind the expected installer metadata.'
}
if ([string]::IsNullOrWhiteSpace([string]$manifest.keyId) -or
    [string]$manifest.keyId -cnotmatch '^[\x21-\x7e]{1,128}$') {
    throw 'The app manifest key ID is invalid.'
}
Assert-CanonicalSha256 -Value ([string]$manifest.installer.sha256) -Label 'Installer SHA-256'
if ([string]$manifest.publishedAt -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$') {
    throw 'The app manifest publication time is not canonical UTC.'
}
$publishedAt = [DateTimeOffset]::MinValue
if (-not [DateTimeOffset]::TryParseExact(
    [string]$manifest.publishedAt,
    'yyyy-MM-ddTHH:mm:ssZ',
    [System.Globalization.CultureInfo]::InvariantCulture,
    [System.Globalization.DateTimeStyles]::AssumeUniversal,
    [ref]$publishedAt
)) {
    throw 'The app manifest publication time is invalid.'
}

$manifestSignature = Get-CandidateFile -Name "$manifestName.sig" -Root $candidateRoot
Assert-SignatureFile -File $manifestSignature -Label 'App manifest signature'
$manifestPublicKey = Select-TrustedPublicKey `
    -ManifestKeyId ([string]$manifest.keyId) `
    -ConfiguredCurrentKeyId $CurrentKeyId `
    -ConfiguredCurrentPublicKey $CurrentPublicKey `
    -ConfiguredNextKeyId $NextKeyId `
    -ConfiguredNextPublicKey $NextPublicKey
Invoke-MinisignVerification `
    -PublicKey $manifestPublicKey `
    -MessagePath $manifestFile.FullName `
    -SignaturePath $manifestSignature.FullName `
    -RepositoryRoot $repositoryRoot

$legacyFile = Get-CandidateFile -Name $legacyName -Root $candidateRoot
$legacyExpected = "$installerHash  $installerName`n"
$legacyActual = [System.IO.File]::ReadAllText($legacyFile.FullName, [System.Text.Encoding]::ASCII)
if ($legacyActual -cne $legacyExpected) {
    throw 'The v0.5.4 bridge checksum does not exactly bind the 0.6.0 installer.'
}

$portable = Get-CandidateFile -Name $portableName -Root $candidateRoot
if ($portable.Length -le 0 -or $portable.Length -gt 4GB) {
    throw 'The portable ZIP is empty or exceeds the 4 GiB release limit.'
}

$runtimeDescriptorFile = Get-CandidateFile -Name $runtimeDescriptorName -Root $candidateRoot
$runtime = Read-StrictJson -File $runtimeDescriptorFile -MaximumBytes 64KB -Label 'Runtime descriptor'
Assert-ExactProperties -Value $runtime `
    -Expected @('schemaVersion', 'keyId', 'runtimeVersion', 'platform', 'archiveName', 'compressedSize', 'sha256', 'manifestSha256') `
    -Label 'Runtime descriptor'
if ($runtime.schemaVersion -ne 1 -or
    [string]$runtime.keyId -cne [string]$manifest.keyId -or
    [string]$runtime.platform -cne 'windows-x64' -or
    [string]$runtime.runtimeVersion -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw 'The runtime descriptor has an invalid schema, key, platform, or version.'
}
$runtimeArchiveName = "nuclear-downloader-runtime-$([string]$runtime.runtimeVersion)-windows-x64.zip"
if ([string]$runtime.archiveName -cne $runtimeArchiveName) {
    throw 'The runtime descriptor archive name is not exact.'
}
Assert-CanonicalSha256 -Value ([string]$runtime.sha256) -Label 'Runtime archive SHA-256'
Assert-CanonicalSha256 -Value ([string]$runtime.manifestSha256) -Label 'Runtime manifest SHA-256'
$runtimeArchive = Get-CandidateFile -Name $runtimeArchiveName -Root $candidateRoot
if ($runtimeArchive.Length -le 0 -or
    $runtimeArchive.Length -gt 1GB -or
    [long]$runtime.compressedSize -ne [long]$runtimeArchive.Length -or
    (Get-LowerSha256 -Path $runtimeArchive.FullName) -cne [string]$runtime.sha256) {
    throw 'The runtime archive does not match its signed descriptor.'
}
if ((Get-RuntimeArchiveManifestSha256 -Archive $runtimeArchive) -cne [string]$runtime.manifestSha256) {
    throw 'The runtime archive manifest does not match its signed descriptor.'
}
$runtimeSignature = Get-CandidateFile -Name "$runtimeDescriptorName.sig" -Root $candidateRoot
Assert-SignatureFile -File $runtimeSignature -Label 'Runtime descriptor signature'
$runtimePublicKey = Select-TrustedPublicKey `
    -ManifestKeyId ([string]$runtime.keyId) `
    -ConfiguredCurrentKeyId $CurrentKeyId `
    -ConfiguredCurrentPublicKey $CurrentPublicKey `
    -ConfiguredNextKeyId $NextKeyId `
    -ConfiguredNextPublicKey $NextPublicKey
Invoke-MinisignVerification `
    -PublicKey $runtimePublicKey `
    -MessagePath $runtimeDescriptorFile.FullName `
    -SignaturePath $runtimeSignature.FullName `
    -RepositoryRoot $repositoryRoot
$runtimeLegacy = Get-CandidateFile -Name "$runtimeArchiveName.sha256" -Root $candidateRoot
$runtimeLegacyExpected = "$([string]$runtime.sha256)  $runtimeArchiveName`n"
$runtimeLegacyActual = [System.IO.File]::ReadAllText($runtimeLegacy.FullName, [System.Text.Encoding]::ASCII)
if ($runtimeLegacyActual -cne $runtimeLegacyExpected) {
    throw 'The runtime bridge checksum does not exactly bind the runtime archive.'
}

$shaSums = Get-CandidateFile -Name 'SHA256SUMS' -Root $candidateRoot
$inventoryFile = Get-CandidateFile -Name $inventoryName -Root $candidateRoot
$expectedNames = @(
    $installerName,
    $portableName,
    $manifestName,
    "$manifestName.sig",
    $legacyName,
    $runtimeDescriptorName,
    "$runtimeDescriptorName.sig",
    $runtimeArchiveName,
    "$runtimeArchiveName.sha256",
    'SHA256SUMS',
    $inventoryName
) | Sort-Object
$actualFiles = @(Get-ChildItem -LiteralPath $candidateRoot -File -Force | Sort-Object -Property Name)
$actualNames = @($actualFiles.Name | Sort-Object)
if (($actualNames -join "`n") -cne ($expectedNames -join "`n")) {
    throw "Candidate file set is not exact. Expected [$($expectedNames -join ', ')], found [$($actualNames -join ', ')]."
}

$sumEntries = [ordered]@{}
foreach ($line in [System.IO.File]::ReadAllLines($shaSums.FullName, [System.Text.Encoding]::ASCII)) {
    if ($line -cnotmatch '^([0-9a-f]{64})  ([A-Za-z0-9._-]+)$') {
        throw "SHA256SUMS contains a malformed entry: $line"
    }
    if ($sumEntries.Contains($Matches[2])) {
        throw "SHA256SUMS contains a duplicate entry for $($Matches[2])."
    }
    $sumEntries[$Matches[2]] = $Matches[1]
}
$sumExpectedNames = @($expectedNames | Where-Object { $_ -cne 'SHA256SUMS' -and $_ -cne $inventoryName })
if ((@($sumEntries.Keys | Sort-Object) -join "`n") -cne ($sumExpectedNames -join "`n")) {
    throw 'SHA256SUMS must contain every public artifact except itself, and no private inventory.'
}
foreach ($name in $sumExpectedNames) {
    $file = Get-CandidateFile -Name $name -Root $candidateRoot
    if ([string]$sumEntries[$name] -cne (Get-LowerSha256 -Path $file.FullName)) {
        throw "SHA256SUMS does not match $name."
    }
}

$inventory = Read-StrictJson -File $inventoryFile -MaximumBytes 1MB -Label 'Candidate inventory'
Assert-ExactProperties -Value $inventory `
    -Expected @('schemaVersion', 'releaseVersion', 'releaseTag', 'platform', 'keyId', 'sourceCommit', 'createdAt', 'toolchains', 'assets') `
    -Label 'Candidate inventory'
Assert-ExactProperties -Value $inventory.toolchains `
    -Expected @('node', 'npm', 'rustc', 'cargo') `
    -Label 'Candidate toolchain inventory'
if ($inventory.schemaVersion -ne 1 -or
    [string]$inventory.releaseVersion -cne $ExpectedVersion -or
    [string]$inventory.releaseTag -cne "v$ExpectedVersion" -or
    [string]$inventory.platform -cne 'windows-x86_64' -or
    [string]$inventory.keyId -cne [string]$manifest.keyId -or
    [string]$inventory.sourceCommit -cnotmatch '^[0-9a-f]{40}$') {
    throw 'The candidate inventory identity is invalid.'
}
if ($ExpectedCommitSha -and [string]$inventory.sourceCommit -cne $ExpectedCommitSha) {
    throw 'The candidate inventory does not match the expected source commit.'
}
if ([string]$inventory.toolchains.node -cne 'v22.23.1' -or
    [string]$inventory.toolchains.npm -cne '10.9.9' -or
    [string]$inventory.toolchains.rustc -cnotmatch '^rustc 1\.94\.1 ' -or
    [string]$inventory.toolchains.cargo -cnotmatch '^cargo 1\.94\.1 ') {
    throw 'The candidate was not built with the pinned Node, npm, Rust, and Cargo toolchains.'
}

$inventoryAssets = @($inventory.assets)
$inventoryExpectedNames = @($expectedNames | Where-Object { $_ -cne $inventoryName })
if ($inventoryAssets.Count -ne $inventoryExpectedNames.Count) {
    throw 'The candidate inventory has the wrong number of assets.'
}
$seenInventoryNames = @{}
foreach ($asset in $inventoryAssets) {
    Assert-ExactProperties -Value $asset -Expected @('fileName', 'size', 'sha256') -Label 'Candidate inventory asset'
    $name = [string]$asset.fileName
    if ($name -notin $inventoryExpectedNames -or $seenInventoryNames.ContainsKey($name)) {
        throw "The candidate inventory contains an unexpected or duplicate asset: $name"
    }
    $seenInventoryNames[$name] = $true
    Assert-CanonicalSha256 -Value ([string]$asset.sha256) -Label "Inventory SHA-256 for $name"
    $file = Get-CandidateFile -Name $name -Root $candidateRoot
    if ([long]$asset.size -ne [long]$file.Length -or
        [string]$asset.sha256 -cne (Get-LowerSha256 -Path $file.FullName)) {
        throw "The candidate inventory does not match $name."
    }
}
if ((@($seenInventoryNames.Keys | Sort-Object) -join "`n") -cne ($inventoryExpectedNames -join "`n")) {
    throw 'The candidate inventory is incomplete.'
}

Write-Output "Verified Nuclear Downloader $ExpectedVersion candidate from commit $([string]$inventory.sourceCommit)."
