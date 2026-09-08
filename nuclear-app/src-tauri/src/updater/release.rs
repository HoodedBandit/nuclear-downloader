use super::artifact::INSTALLER_LIMIT;
use super::network::{download_bounded_success_body, GitHubRelease, GitHubReleaseAsset};
use minisign_verify::Signature;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use std::path::Path;

const UPDATE_PUBLIC_KEY: Option<&str> = option_env!("NUCLEAR_UPDATE_PUBLIC_KEY");
const UPDATE_KEY_ID: Option<&str> = option_env!("NUCLEAR_UPDATE_KEY_ID");
const UPDATE_NEXT_PUBLIC_KEY: Option<&str> = option_env!("NUCLEAR_UPDATE_NEXT_PUBLIC_KEY");
const UPDATE_NEXT_KEY_ID: Option<&str> = option_env!("NUCLEAR_UPDATE_NEXT_KEY_ID");
const WINDOWS_PLATFORM: &str = "windows-x86_64";
const MANIFEST_LIMIT: u64 = 64 * 1024;
const SIGNATURE_LIMIT: u64 = 8 * 1024;

pub(super) fn signed_update_minimum() -> Version {
    Version::new(0, 6, 0)
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SignedAppManifest {
    schema_version: u32,
    key_id: String,
    version: String,
    platform: String,
    published_at: String,
    pub(super) installer: SignedInstaller,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SignedInstaller {
    pub(super) file_name: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

pub(super) struct VerifiedUpdate<'a> {
    pub(super) version: Version,
    pub(super) installer_asset: &'a GitHubReleaseAsset,
    pub(super) manifest: SignedAppManifest,
}

pub(super) async fn verify_release_contract<'a>(
    client: &Client,
    release: &'a GitHubRelease,
    release_version: &Version,
) -> Result<VerifiedUpdate<'a>, String> {
    let (manifest_asset, signature_asset, installer_asset) =
        select_signed_update_assets(release, release_version)?;
    let manifest_bytes = download_bounded_success_body(
        client,
        &manifest_asset.browser_download_url,
        MANIFEST_LIMIT,
        "app update manifest",
    )
    .await?;
    let signature_bytes = download_bounded_success_body(
        client,
        &signature_asset.browser_download_url,
        SIGNATURE_LIMIT,
        "app update signature",
    )
    .await?;
    let untrusted_manifest: SignedAppManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Failed to parse signed app update manifest: {error}"))?;
    verify_release_signature_for_key(
        &untrusted_manifest.key_id,
        &manifest_bytes,
        &signature_bytes,
    )?;
    let manifest = validate_manifest(untrusted_manifest, release_version, installer_asset)?;
    Ok(VerifiedUpdate {
        version: release_version.clone(),
        installer_asset,
        manifest,
    })
}

#[cfg(test)]
pub(super) fn parse_and_validate_manifest(
    bytes: &[u8],
    release_version: &Version,
    installer_asset: &GitHubReleaseAsset,
) -> Result<SignedAppManifest, String> {
    let manifest: SignedAppManifest = serde_json::from_slice(bytes)
        .map_err(|error| format!("Failed to parse signed app update manifest: {error}"))?;
    validate_manifest(manifest, release_version, installer_asset)
}

fn validate_manifest(
    manifest: SignedAppManifest,
    release_version: &Version,
    installer_asset: &GitHubReleaseAsset,
) -> Result<SignedAppManifest, String> {
    if manifest.schema_version != 1 {
        return Err(format!(
            "Unsupported app update manifest schema version {}.",
            manifest.schema_version
        ));
    }
    validate_security_text("manifest key ID", &manifest.key_id)?;
    if !is_canonical_update_key_id(&manifest.key_id) {
        return Err("The app update key ID is not in the canonical release-key format.".into());
    }
    trusted_public_key(&manifest.key_id)?;
    if parse_semver(&manifest.version)? != *release_version
        || manifest.version != release_version.to_string()
    {
        return Err(
            "The app update manifest version does not exactly match the release tag.".into(),
        );
    }
    if manifest.platform != WINDOWS_PLATFORM {
        return Err(format!(
            "The app update targets {}, not {WINDOWS_PLATFORM}.",
            manifest.platform
        ));
    }
    validate_timestamp(&manifest.published_at)?;
    let expected_name = expected_installer_name(release_version);
    if manifest.installer.file_name != expected_name || installer_asset.name != expected_name {
        return Err(
            "The signed installer filename does not match the exact release contract.".into(),
        );
    }
    if manifest.installer.size == 0 || manifest.installer.size > INSTALLER_LIMIT {
        return Err("The signed installer size is outside the allowed range.".into());
    }
    if installer_asset.size != manifest.installer.size {
        return Err("GitHub installer metadata does not match the signed installer size.".into());
    }
    validate_sha256(&manifest.installer.sha256)?;
    Ok(manifest)
}

pub(crate) fn verify_release_signature_for_key(
    key_id: &str,
    bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let public_key = trusted_public_key(key_id)?;
    verify_with_public_key(public_key, bytes, signature_bytes)
}

fn trusted_public_key(key_id: &str) -> Result<&'static str, String> {
    select_public_key(
        key_id,
        UPDATE_KEY_ID,
        UPDATE_PUBLIC_KEY,
        UPDATE_NEXT_KEY_ID,
        UPDATE_NEXT_PUBLIC_KEY,
    )
}

pub(super) fn select_public_key<'a>(
    key_id: &str,
    current_id: Option<&'a str>,
    current_key: Option<&'a str>,
    next_id: Option<&'a str>,
    next_key: Option<&'a str>,
) -> Result<&'a str, String> {
    let current = match (current_id, current_key) {
        (Some(id), Some(key)) if is_canonical_update_key_id(id) && !key.is_empty() => {
            Some((id, key))
        }
        (None, None) => None,
        _ => return Err("The embedded current update key pair is missing or invalid.".into()),
    };
    let next = match (next_id, next_key) {
        (Some(id), Some(key)) if is_canonical_update_key_id(id) && !key.is_empty() => {
            Some((id, key))
        }
        (None, None) => None,
        _ => return Err("The embedded next update key pair is missing or invalid.".into()),
    };
    if let (Some((current_id, _)), Some((next_id, _))) = (current, next) {
        if current_id == next_id {
            return Err("Embedded current and next update key IDs must be distinct.".into());
        }
    }
    current
        .into_iter()
        .chain(next)
        .find_map(|(id, key)| (id == key_id).then_some(key))
        .ok_or_else(|| "The release manifest references an untrusted signing key ID.".to_string())
}

pub(crate) fn is_canonical_update_key_id(value: &str) -> bool {
    crate::artifact_contract::is_canonical_update_key_id(value)
}

pub(super) fn verify_with_public_key(
    public_key_text: &str,
    bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let public_key = crate::artifact_contract::parse_tauri_update_public_key(public_key_text)?;
    let decoded_signature = crate::artifact_contract::decode_tauri_base64_wrapper(
        signature_bytes,
        SIGNATURE_LIMIT as usize,
    )
    .map_err(|_| "The Tauri release signature wrapper is invalid.".to_string())?;
    let signature_text = std::str::from_utf8(&decoded_signature)
        .map_err(|_| "The release signature is not valid UTF-8.".to_string())?;
    let signature = Signature::decode(signature_text)
        .map_err(|_| "The release signature has an invalid format.".to_string())?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|_| "The release manifest signature is invalid.".to_string())
}

pub(super) fn select_signed_update_assets<'a>(
    release: &'a GitHubRelease,
    version: &Version,
) -> Result<
    (
        &'a GitHubReleaseAsset,
        &'a GitHubReleaseAsset,
        &'a GitHubReleaseAsset,
    ),
    String,
> {
    let manifest_name = format!("nuclear-downloader-v{version}-update.json");
    let signature_name = format!("{manifest_name}.sig");
    let installer_name = expected_installer_name(version);
    let manifest = select_one_exact_asset(release, &manifest_name)?;
    let signature = select_one_exact_asset(release, &signature_name)?;
    let installer = select_one_exact_asset(release, &installer_name)?;
    let installer_candidates = release
        .assets
        .iter()
        .filter(|asset| is_versioned_installer_candidate(&asset.name))
        .count();
    let manifest_candidates = release
        .assets
        .iter()
        .filter(|asset| is_versioned_app_manifest_candidate(&asset.name))
        .count();
    let signature_candidates = release
        .assets
        .iter()
        .filter(|asset| is_versioned_app_signature_candidate(&asset.name))
        .count();
    if installer_candidates != 1 || manifest_candidates != 1 || signature_candidates != 1 {
        return Err(
            "The release contains ambiguous or extra versioned app update candidates.".into(),
        );
    }
    Ok((manifest, signature, installer))
}

fn select_one_exact_asset<'a>(
    release: &'a GitHubRelease,
    expected_name: &str,
) -> Result<&'a GitHubReleaseAsset, String> {
    let mut matches = release
        .assets
        .iter()
        .filter(|asset| asset.name == expected_name);
    let selected = matches
        .next()
        .ok_or_else(|| format!("Required release asset {expected_name} was not found."))?;
    if matches.next().is_some() {
        return Err(format!("Release asset {expected_name} is ambiguous."));
    }
    Ok(selected)
}

pub(super) fn parse_release_tag(raw: &str) -> Result<Version, String> {
    validate_security_text("release tag", raw)?;
    let version = parse_semver(raw)?;
    if raw != format!("v{version}") {
        return Err("Release tag is not in exact vMAJOR.MINOR.PATCH semantic-version form.".into());
    }
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err("Prerelease and build metadata are not accepted for app updates.".into());
    }
    Ok(version)
}

pub(super) fn parse_semver(raw: &str) -> Result<Version, String> {
    let trimmed = raw.trim();
    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Version::parse(normalized).map_err(|error| format!("Invalid release version '{raw}': {error}"))
}

pub(super) fn expected_installer_name(version: &Version) -> String {
    format!("Nuclear.Downloader_{version}_x64-setup.exe")
}

fn is_versioned_installer_candidate(name: &str) -> bool {
    name.starts_with("Nuclear.Downloader_") && name.ends_with("_x64-setup.exe")
}

fn is_versioned_app_manifest_candidate(name: &str) -> bool {
    name.starts_with("nuclear-downloader-v") && name.ends_with("-update.json")
}

fn is_versioned_app_signature_candidate(name: &str) -> bool {
    name.starts_with("nuclear-downloader-v") && name.ends_with("-update.json.sig")
}

fn validate_security_text(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value.is_ascii()
        || value.chars().any(|character| character.is_control())
        || value.trim() != value
    {
        return Err(format!(
            "The {label} contains invalid or non-canonical text."
        ));
    }
    Ok(())
}

pub(super) fn validate_timestamp(value: &str) -> Result<(), String> {
    validate_security_text("publication timestamp", value)?;
    let bytes = value.as_bytes();
    let canonical_shape = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        });
    if !canonical_shape {
        return Err("The signed publication timestamp is malformed.".into());
    }
    let parse = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&bytes[range])
            .ok()
            .and_then(|part| part.parse::<u32>().ok())
    };
    let year = parse(0..4).unwrap_or(0);
    let month = parse(5..7).unwrap_or(0);
    let day = parse(8..10).unwrap_or(0);
    let hour = parse(11..13).unwrap_or(99);
    let minute = parse(14..16).unwrap_or(99);
    let second = parse(17..19).unwrap_or(99);
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year => 29,
        2 => 28,
        _ => 0,
    };
    if year == 0 || day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return Err("The signed publication timestamp is not a valid UTC time.".into());
    }
    Ok(())
}

pub(super) fn validate_sha256(value: &str) -> Result<(), String> {
    if !crate::artifact_contract::is_canonical_sha256(value) {
        return Err("The signed installer SHA-256 must be 64 lowercase hexadecimal digits.".into());
    }
    Ok(())
}

pub(super) fn sanitize_file_name(name: &str) -> Result<String, String> {
    validate_security_text("installer filename", name)?;
    if name.contains('/')
        || name.contains('\\')
        || Path::new(name).file_name() != Some(name.as_ref())
    {
        return Err("The release returned an unsafe installer filename.".into());
    }
    Ok(name.to_string())
}

pub(super) fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}
