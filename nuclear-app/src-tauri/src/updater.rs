use crate::models::{UpdateCheckResult, UpdateInstallProgress};
use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use reqwest::header::ACCEPT;
use reqwest::{Client, Response};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const UPDATE_PROGRESS_EVENT: &str = "update-install-progress";
const UPDATE_DIRECTORY_NAME: &str = "updater";
const UPDATE_LOCK_FILE_NAME: &str = "update.lock";
const UPDATE_PUBLIC_KEY: Option<&str> = option_env!("NUCLEAR_UPDATE_PUBLIC_KEY");
const UPDATE_KEY_ID: Option<&str> = option_env!("NUCLEAR_UPDATE_KEY_ID");
const UPDATE_NEXT_PUBLIC_KEY: Option<&str> = option_env!("NUCLEAR_UPDATE_NEXT_PUBLIC_KEY");
const UPDATE_NEXT_KEY_ID: Option<&str> = option_env!("NUCLEAR_UPDATE_NEXT_KEY_ID");
const WINDOWS_PLATFORM: &str = "windows-x86_64";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALLER_OVERALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RELEASE_METADATA_LIMIT: u64 = 1024 * 1024;
const MANIFEST_LIMIT: u64 = 64 * 1024;
const SIGNATURE_LIMIT: u64 = 8 * 1024;
const ERROR_BODY_LIMIT: u64 = 8 * 1024;
const INSTALLER_LIMIT: u64 = 1024 * 1024 * 1024;
static UPDATE_INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn signed_update_minimum() -> Version {
    Version::new(0, 6, 0)
}

struct UpdateInstallGuard;

impl Drop for UpdateInstallGuard {
    fn drop(&mut self) {
        UPDATE_INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// A cryptographically verified installer whose open file handle prevents
/// write/delete sharing until Windows has successfully created the process.
/// The path alone is never treated as the authorization to execute.
struct VerifiedInstaller {
    path: PathBuf,
    _read_lease: std::fs::File,
}

impl VerifiedInstaller {
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Holds an open, non-shareable file on Windows. A crashed process releases the
/// OS handle, so a stale marker cannot permanently wedge the updater.
struct UpdateDirectoryLock {
    file: Option<std::fs::File>,
}

impl UpdateDirectoryLock {
    fn acquire(directory: &Path) -> Result<Self, String> {
        if let Some(parent) = directory.parent() {
            ensure_no_reparse_components(parent)?;
        }
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Failed to create update temp folder: {error}"))?;
        ensure_no_reparse_components(directory)?;
        let directory_metadata = std::fs::symlink_metadata(directory)
            .map_err(|error| format!("Failed to inspect update temp folder: {error}"))?;
        if !directory_metadata.is_dir() || is_reparse_or_symlink(directory)? {
            return Err("The updater directory must be a regular non-reparse directory.".into());
        }
        let path = directory.join(UPDATE_LOCK_FILE_NAME);
        if path.exists() {
            ensure_no_reparse_components(&path)?;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("Failed to inspect updater lock: {error}"))?;
            if !metadata.is_file() || is_reparse_or_symlink(&path)? {
                return Err("The updater lock must be a regular non-reparse file.".into());
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(&path).map_err(|error| {
            format!("Another updater process owns the update directory: {error}")
        })?;
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            return Err("The updater lock became a reparse point.".into());
        }
        Ok(Self { file: Some(file) })
    }
}

impl Drop for UpdateDirectoryLock {
    fn drop(&mut self) {
        drop(self.file.take());
    }
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    published_at: Option<String>,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedAppManifest {
    schema_version: u32,
    key_id: String,
    version: String,
    platform: String,
    published_at: String,
    installer: SignedInstaller,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedInstaller {
    file_name: String,
    size: u64,
    sha256: String,
}

struct VerifiedUpdate<'a> {
    version: Version,
    installer_asset: &'a GitHubReleaseAsset,
    manifest: SignedAppManifest,
}

pub async fn check_for_app_update(app: &AppHandle) -> Result<UpdateCheckResult, String> {
    let current_version = app.package_info().version.to_string();
    let current_semver = parse_semver(&current_version)?;
    let client = build_client(updater_user_agent(&current_version))?;
    let release = fetch_latest_release(&client).await?;
    let latest_semver = parse_release_tag(&release.tag_name)?;
    let has_update = latest_semver > current_semver;
    let installer_name = if has_update {
        if latest_semver < signed_update_minimum() {
            return Err("Unsigned app releases are not accepted by this updater.".into());
        }
        Some(
            verify_release_contract(&client, &release, &latest_semver)
                .await?
                .installer_asset
                .name
                .clone(),
        )
    } else {
        None
    };

    Ok(UpdateCheckResult {
        current_version,
        has_update,
        latest_version: Some(latest_semver.to_string()),
        notes: normalize_optional_text(release.body),
        published_at: normalize_optional_text(release.published_at),
        installer_name,
    })
}

pub async fn install_app_update(app: &AppHandle, expected_version: String) -> Result<(), String> {
    if UPDATE_INSTALL_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("An app update is already in progress.".into());
    }
    let _guard = UpdateInstallGuard;
    install_app_update_inner(app, expected_version.clone())
        .await
        .inspect_err(|error| {
            emit_install_progress(
                app,
                UpdateInstallProgress {
                    status: "error".into(),
                    version: normalize_version_label(&expected_version),
                    downloaded_bytes: 0,
                    total_bytes: None,
                    message: Some(error.clone()),
                },
            );
        })
}

async fn install_app_update_inner(app: &AppHandle, expected_version: String) -> Result<(), String> {
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    {
        let _ = app;
        let _ = expected_version;
        return Err("Automatic updates are supported only on Windows x64 builds.".into());
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        let expected_semver = parse_semver(&expected_version)?;
        let current_semver = parse_semver(&app.package_info().version.to_string())?;
        if expected_semver < signed_update_minimum() {
            return Err("Unsigned app releases are not accepted by this updater.".into());
        }
        if expected_semver <= current_semver {
            return Err(format!(
                "No newer update is available. Current version is {current_semver}."
            ));
        }
        let client = build_client(updater_user_agent(&current_semver.to_string()))?;
        let release = fetch_latest_release(&client).await?;
        let latest_semver = parse_release_tag(&release.tag_name)?;
        if latest_semver != expected_semver {
            return Err(format!(
                "The latest GitHub release changed from {expected_semver} to {latest_semver}. Please check for updates again."
            ));
        }
        let verified = verify_release_contract(&client, &release, &latest_semver).await?;
        let target_dir = updater_directory();
        let _directory_lock = UpdateDirectoryLock::acquire(&target_dir)?;
        cleanup_owned_partial_installers(&target_dir).await?;
        cleanup_owned_old_installers(&target_dir, &verified.manifest.installer.file_name).await?;
        let installer = tokio::time::timeout(
            INSTALLER_OVERALL_TIMEOUT,
            download_installer(app, &client, &target_dir, &verified),
        )
        .await
        .map_err(|_| "Update installer download exceeded the 30-minute limit.".to_string())??;

        emit_install_progress(
            app,
            UpdateInstallProgress {
                status: "launching".into(),
                version: latest_semver.to_string(),
                downloaded_bytes: verified.manifest.installer.size,
                total_bytes: Some(verified.manifest.installer.size),
                message: Some(format!(
                    "Launching {}. Nuclear Downloader will close and reopen after install.",
                    verified.manifest.installer.file_name
                )),
            },
        );
        std::process::Command::new(installer.path())
            .args(["/S", "/R"])
            .spawn()
            .map_err(|error| format!("Failed to launch the verified installer: {error}"))?;
        app.exit(0);
        Ok(())
    }
}

async fn cleanup_owned_partial_installers(target_dir: &Path) -> Result<(), String> {
    ensure_no_reparse_components(target_dir)?;
    let mut entries = fs::read_dir(target_dir)
        .await
        .map_err(|error| format!("Failed to inspect the updater directory: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate the updater directory: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let owned_name = name
            .strip_prefix("Nuclear.Downloader_")
            .and_then(|rest| rest.split_once("_x64-setup.exe."))
            .and_then(|(version, suffix)| {
                let operation_id = suffix.strip_suffix(".part")?;
                (parse_semver(version).is_ok() && uuid::Uuid::parse_str(operation_id).is_ok())
                    .then_some(())
            })
            .is_some();
        if !owned_name {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("Failed to inspect abandoned updater data: {error}"))?;
        if !metadata.is_file() || is_reparse_or_symlink(&path)? {
            continue;
        }
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            continue;
        }
        fs::remove_file(path).await.map_err(|error| {
            format!("Failed to remove an abandoned updater partial file: {error}")
        })?;
    }
    Ok(())
}

async fn cleanup_owned_old_installers(
    target_dir: &Path,
    current_installer_name: &str,
) -> Result<(), String> {
    ensure_no_reparse_components(target_dir)?;
    let mut entries = fs::read_dir(target_dir)
        .await
        .map_err(|error| format!("Failed to inspect the updater directory: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate the updater directory: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == current_installer_name || !is_exact_owned_installer_name(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("Failed to inspect an old updater artifact: {error}"))?;
        if !metadata.is_file() || is_reparse_or_symlink(&path)? {
            continue;
        }
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            continue;
        }
        fs::remove_file(&path)
            .await
            .map_err(|error| format!("Failed to remove an old updater installer: {error}"))?;
    }
    Ok(())
}

fn is_exact_owned_installer_name(name: &str) -> bool {
    let Some(version_text) = name
        .strip_prefix("Nuclear.Downloader_")
        .and_then(|rest| rest.strip_suffix("_x64-setup.exe"))
    else {
        return false;
    };
    let Ok(version) = Version::parse(version_text) else {
        return false;
    };
    version.pre.is_empty() && version.build.is_empty() && version.to_string() == version_text
}

async fn verify_release_contract<'a>(
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
fn parse_and_validate_manifest(
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

fn select_public_key<'a>(
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
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn verify_with_public_key(
    public_key_text: &str,
    bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let decoded_key = decode_tauri_base64_wrapper(public_key_text.as_bytes(), 8 * 1024)
        .map_err(|_| "The embedded Tauri update public key wrapper is invalid.".to_string())?;
    let decoded_signature = decode_tauri_base64_wrapper(signature_bytes, SIGNATURE_LIMIT as usize)
        .map_err(|_| "The Tauri release signature wrapper is invalid.".to_string())?;
    let public_key_text = std::str::from_utf8(&decoded_key)
        .map_err(|_| "The embedded update public key is not valid UTF-8.".to_string())?;
    let signature_text = std::str::from_utf8(&decoded_signature)
        .map_err(|_| "The release signature is not valid UTF-8.".to_string())?;
    let public_key = PublicKey::decode(public_key_text)
        .map_err(|_| "The embedded update public key is invalid.".to_string())?;
    let signature = Signature::decode(signature_text)
        .map_err(|_| "The release signature has an invalid format.".to_string())?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|_| "The release manifest signature is invalid.".to_string())
}

fn decode_tauri_base64_wrapper(input: &[u8], decoded_limit: usize) -> Result<Vec<u8>, ()> {
    let input = input
        .strip_suffix(b"\r\n")
        .or_else(|| input.strip_suffix(b"\n"))
        .unwrap_or(input);
    if input.is_empty()
        || !input.len().is_multiple_of(4)
        || input.iter().any(u8::is_ascii_whitespace)
    {
        return Err(());
    }
    let padding = input.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 || input[..input.len() - padding].contains(&b'=') {
        return Err(());
    }
    let decoded_length = input
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|length| length.checked_sub(padding))
        .ok_or(())?;
    if decoded_length > decoded_limit {
        return Err(());
    }
    let mut output = Vec::with_capacity(decoded_length);
    for (group_index, group) in input.chunks_exact(4).enumerate() {
        let last = group_index + 1 == input.len() / 4;
        let a = decode_base64_digit(group[0]).ok_or(())? as u32;
        let b = decode_base64_digit(group[1]).ok_or(())? as u32;
        let c = if group[2] == b'=' {
            if !last || group[3] != b'=' {
                return Err(());
            }
            if b & 0x0f != 0 {
                return Err(());
            }
            0
        } else {
            decode_base64_digit(group[2]).ok_or(())? as u32
        };
        let d = if group[3] == b'=' {
            if !last {
                return Err(());
            }
            if c & 0x03 != 0 {
                return Err(());
            }
            0
        } else {
            decode_base64_digit(group[3]).ok_or(())? as u32
        };
        let value = (a << 18) | (b << 12) | (c << 6) | d;
        output.push((value >> 16) as u8);
        if group[2] != b'=' {
            output.push((value >> 8) as u8);
        }
        if group[3] != b'=' {
            output.push(value as u8);
        }
    }
    (output.len() == decoded_length).then_some(output).ok_or(())
}

fn decode_base64_digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn select_signed_update_assets<'a>(
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

async fn download_installer(
    app: &AppHandle,
    client: &Client,
    target_dir: &Path,
    verified: &VerifiedUpdate<'_>,
) -> Result<VerifiedInstaller, String> {
    ensure_no_reparse_components(target_dir)?;
    let installer = verified.installer_asset;
    validate_download_url(&installer.browser_download_url)?;
    let file_name = sanitize_file_name(&verified.manifest.installer.file_name)?;
    let final_path = target_dir.join(&file_name);
    let part_path = target_dir.join(format!("{file_name}.{}.part", uuid::Uuid::new_v4()));
    if fs::try_exists(&final_path).await.unwrap_or(false) {
        ensure_no_reparse_components(&final_path)?;
        if let Some(installer) = open_verified_installer(
            &final_path,
            verified.manifest.installer.size,
            &verified.manifest.installer.sha256,
        )
        .await?
        {
            return Ok(installer);
        }
        return Err("An unverified file already occupies the updater destination.".into());
    }
    let response = client
        .get(&installer.browser_download_url)
        .send()
        .await
        .map_err(|error| format!("Failed to download update installer: {error}"))?;
    if !response.status().is_success() {
        return Err(read_http_error(response, "update installer").await);
    }
    if response
        .content_length()
        .is_some_and(|length| length != verified.manifest.installer.size)
    {
        return Err("Installer Content-Length does not match the signed size.".into());
    }

    let total_bytes = Some(verified.manifest.installer.size);
    let mut stream = response.bytes_stream();
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&part_path)
        .await
        .map_err(|error| format!("Failed to create installer temp file: {error}"))?;
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();
    emit_install_progress(
        app,
        UpdateInstallProgress {
            status: "downloading".into(),
            version: verified.version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Downloading {file_name}...")),
        },
    );

    while let Some(chunk_result) = stream.next().await {
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                cleanup_file_if_exists(&part_path).await;
                return Err(format!(
                    "Failed while downloading update installer: {error}"
                ));
            }
        };
        downloaded_bytes = downloaded_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "Installer byte count overflowed.".to_string())?;
        if downloaded_bytes > verified.manifest.installer.size || downloaded_bytes > INSTALLER_LIMIT
        {
            cleanup_file_if_exists(&part_path).await;
            return Err("Installer exceeded its signed size or the 1 GiB limit.".into());
        }
        if let Err(error) = file.write_all(&chunk).await {
            cleanup_file_if_exists(&part_path).await;
            return Err(format!("Failed to write installer download: {error}"));
        }
        hasher.update(&chunk);
        emit_install_progress(
            app,
            UpdateInstallProgress {
                status: "downloading".into(),
                version: verified.version.to_string(),
                downloaded_bytes,
                total_bytes,
                message: Some(format!("Downloading {file_name}...")),
            },
        );
    }

    file.flush()
        .await
        .map_err(|error| format!("Failed to finalize installer download: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync installer download: {error}"))?;
    drop(file);
    if downloaded_bytes != verified.manifest.installer.size {
        cleanup_file_if_exists(&part_path).await;
        return Err(format!(
            "Downloaded installer size mismatch: expected {} bytes, got {downloaded_bytes} bytes.",
            verified.manifest.installer.size
        ));
    }
    let actual_checksum = format!("{:x}", hasher.finalize());
    emit_install_progress(
        app,
        UpdateInstallProgress {
            status: "verifying".into(),
            version: verified.version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Verifying {file_name}...")),
        },
    );
    if actual_checksum != verified.manifest.installer.sha256 {
        cleanup_file_if_exists(&part_path).await;
        return Err("Downloaded installer SHA-256 does not match the signed manifest.".into());
    }
    ensure_no_reparse_components(target_dir)?;
    if fs::try_exists(&final_path).await.unwrap_or(false) {
        cleanup_file_if_exists(&part_path).await;
        return Err("The updater destination appeared while publishing the installer.".into());
    }
    if let Err(error) = fs::hard_link(&part_path, &final_path).await {
        cleanup_file_if_exists(&part_path).await;
        return Err(format!(
            "Failed to publish the verified installer without replacement: {error}"
        ));
    }
    if let Err(error) = fs::remove_file(&part_path).await {
        eprintln!(
            "Published the verified installer, but failed to remove its partial link: {error}"
        );
    }
    open_verified_installer(
        &final_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await?
    .ok_or_else(|| {
        "The published installer changed before its execution lease was acquired.".to_string()
    })
}

async fn open_verified_installer(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Option<VerifiedInstaller>, String> {
    let path = path.to_path_buf();
    let expected_sha256 = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        open_verified_installer_sync(&path, expected_size, &expected_sha256)
    })
    .await
    .map_err(|error| format!("Installer verification worker failed: {error}"))?
}

fn open_verified_installer_sync(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Option<VerifiedInstaller>, String> {
    ensure_no_reparse_components(path)?;
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect an existing updater artifact: {error}"))?;
    if !path_metadata.is_file()
        || is_reparse_or_symlink(path)?
        || path_metadata.len() != expected_size
        || path_metadata.len() > INSTALLER_LIMIT
    {
        return Ok(None);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x00000001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("Failed to open an existing updater artifact: {error}"))?;

    ensure_no_reparse_components(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect the opened updater artifact: {error}"))?;
    if !opened_metadata.is_file()
        || opened_metadata.len() != expected_size
        || opened_metadata.len() > INSTALLER_LIMIT
        || opened_file_is_reparse(&opened_metadata)
    {
        return Ok(None);
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to verify an existing updater artifact: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to re-inspect the opened updater artifact: {error}"))?;
    if final_metadata.len() != expected_size
        || opened_file_is_reparse(&final_metadata)
        || format!("{:x}", hasher.finalize()) != expected_sha256
    {
        return Ok(None);
    }

    Ok(Some(VerifiedInstaller {
        path: path.to_path_buf(),
        _read_lease: file,
    }))
}

fn opened_file_is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn updater_directory() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("NuclearDownloader")
        .join(UPDATE_DIRECTORY_NAME)
}

fn is_reparse_or_symlink(path: &Path) -> Result<bool, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    Ok(false)
}

fn ensure_no_reparse_components(path: &Path) -> Result<(), String> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for component_path in ancestors {
        if component_path.exists() && is_reparse_or_symlink(component_path)? {
            return Err(format!(
                "Updater path traverses a symbolic link or reparse point: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

fn build_client(user_agent: String) -> Result<Client, String> {
    Client::builder()
        .user_agent(user_agent)
        .connect_timeout(NETWORK_CONNECT_TIMEOUT)
        .read_timeout(NETWORK_READ_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to prepare update client: {error}"))
}

async fn fetch_latest_release(client: &Client) -> Result<GitHubRelease, String> {
    let response = client
        .get(GITHUB_RELEASES_LATEST_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to reach GitHub Releases: {error}"))?;
    let bytes =
        read_bounded_response(response, RELEASE_METADATA_LIMIT, "GitHub release metadata").await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {error}"))
}

async fn download_bounded_success_body(
    client: &Client,
    raw_url: &str,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    validate_download_url(raw_url)?;
    let response = client
        .get(raw_url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download {label}: {error}"))?;
    read_bounded_response(response, limit, label).await
}

async fn read_bounded_response(
    response: Response,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(read_http_error(response, label).await);
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        let next_len = output
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| format!("{label} length overflowed."))?;
        if next_len as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn read_http_error(response: Response, label: &str) -> String {
    let status = response.status();
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        let remaining = ERROR_BODY_LIMIT.saturating_sub(output.len() as u64) as usize;
        if remaining == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let detail = summarize_error_body(&String::from_utf8_lossy(&output));
    if detail.is_empty() {
        format!("Failed to download {label}: HTTP {}.", status.as_u16())
    } else {
        format!(
            "Failed to download {label}: HTTP {}: {detail}",
            status.as_u16()
        )
    }
}

fn parse_release_tag(raw: &str) -> Result<Version, String> {
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

fn parse_semver(raw: &str) -> Result<Version, String> {
    let trimmed = raw.trim();
    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Version::parse(normalized).map_err(|error| format!("Invalid release version '{raw}': {error}"))
}

fn expected_installer_name(version: &Version) -> String {
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

fn validate_timestamp(value: &str) -> Result<(), String> {
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

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("The signed installer SHA-256 must be 64 lowercase hexadecimal digits.".into());
    }
    Ok(())
}

fn validate_download_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("Invalid release asset URL: {error}"))?;
    if url.scheme() != "https" {
        return Err("Release asset URLs must use HTTPS.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Release asset URLs must not contain credentials.".into());
    }
    Ok(())
}

fn sanitize_file_name(name: &str) -> Result<String, String> {
    validate_security_text("installer filename", name)?;
    if name.contains('/')
        || name.contains('\\')
        || Path::new(name).file_name() != Some(name.as_ref())
    {
        return Err("The release returned an unsafe installer filename.".into());
    }
    Ok(name.to_string())
}

async fn cleanup_file_if_exists(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path).await {
        if metadata.is_file()
            && is_reparse_or_symlink(path).ok() == Some(false)
            && ensure_no_reparse_components(path).is_ok()
        {
            let _ = fs::remove_file(path).await;
        }
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}

fn summarize_error_body(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(240).collect())
        .unwrap_or_default()
}

fn normalize_version_label(raw: &str) -> String {
    parse_semver(raw)
        .map(|version| version.to_string())
        .unwrap_or_else(|_| raw.trim().trim_start_matches('v').to_string())
}

fn updater_user_agent(version: &str) -> String {
    format!("NuclearDownloader/{version} (+https://github.com/HoodedBandit/nuclear-downloader)")
}

fn emit_install_progress(app: &AppHandle, payload: UpdateInstallProgress) {
    if let Err(error) = app.emit(UPDATE_PROGRESS_EVENT, payload) {
        eprintln!("Failed to emit updater progress: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn start_http_fixture(parts: Vec<(Vec<u8>, Duration)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            for (bytes, delay_after) in parts {
                stream.write_all(&bytes).unwrap();
                stream.flush().unwrap();
                if !delay_after.is_zero() {
                    std::thread::sleep(delay_after);
                }
            }
        });
        format!("http://{address}/fixture")
    }

    fn release_with_assets(tag_name: &str, assets: Vec<(&str, u64)>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag_name.to_string(),
            body: Some("notes".to_string()),
            published_at: Some("2026-08-17T00:00:00Z".to_string()),
            assets: assets
                .into_iter()
                .map(|(name, size)| GitHubReleaseAsset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.com/{name}"),
                    size,
                })
                .collect(),
        }
    }

    #[test]
    fn release_tags_are_exact_and_canonical() {
        assert_eq!(parse_release_tag("v0.6.0").unwrap(), Version::new(0, 6, 0));
        for invalid in [
            "0.6.0",
            "v00.6.0",
            "v0.6.0+build",
            "v0.6.0-beta",
            "v０.６.０",
        ] {
            assert!(parse_release_tag(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[tokio::test]
    async fn bounded_http_reader_rejects_streamed_overflow_without_content_length() {
        let url = start_http_fixture(vec![(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nabcde\r\n0\r\n\r\n"
                .to_vec(),
            Duration::ZERO,
        )]);
        let response = Client::new().get(url).send().await.unwrap();
        let error = read_bounded_response(response, 4, "fixture")
            .await
            .unwrap_err();
        assert!(error.contains("exceeds the 4-byte limit"));
    }

    #[tokio::test]
    async fn bounded_http_reader_enforces_idle_read_timeout() {
        let url = start_http_fixture(vec![
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\na".to_vec(),
                Duration::from_millis(250),
            ),
            (b"bcde".to_vec(), Duration::ZERO),
        ]);
        let client = Client::builder()
            .read_timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let response = client.get(url).send().await.unwrap();
        let error = read_bounded_response(response, 5, "fixture")
            .await
            .unwrap_err();
        assert!(error.contains("Failed while reading fixture"));
    }

    #[test]
    fn signed_assets_are_exact_and_unambiguous() {
        let version = Version::new(0, 6, 0);
        let release = release_with_assets(
            "v0.6.0",
            vec![
                ("nuclear-downloader-v0.6.0-update.json", 100),
                ("nuclear-downloader-v0.6.0-update.json.sig", 100),
                ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
                ("nuclear-downloader-v0.6.0-sha256.txt", 100),
            ],
        );
        let (_, _, installer) = select_signed_update_assets(&release, &version).unwrap();
        assert_eq!(installer.name, expected_installer_name(&version));

        let ambiguous = release_with_assets(
            "v0.6.0",
            vec![
                ("nuclear-downloader-v0.6.0-update.json", 100),
                ("nuclear-downloader-v0.6.0-update.json.sig", 100),
                ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
                ("Nuclear.Downloader_0.5.9_x64-setup.exe", 42),
            ],
        );
        assert!(select_signed_update_assets(&ambiguous, &version).is_err());

        let extra_manifest = release_with_assets(
            "v0.6.0",
            vec![
                ("nuclear-downloader-v0.6.0-update.json", 100),
                ("nuclear-downloader-v0.6.0-update.json.sig", 100),
                ("nuclear-downloader-v0.5.9-update.json", 100),
                ("nuclear-downloader-v0.5.9-update.json.sig", 100),
                ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
            ],
        );
        assert!(select_signed_update_assets(&extra_manifest, &version).is_err());
    }

    #[test]
    fn manifest_rejects_metadata_mismatch_and_unknown_fields() {
        let version = Version::new(0, 6, 0);
        let installer = GitHubReleaseAsset {
            name: expected_installer_name(&version),
            browser_download_url: "https://example.com/setup.exe".into(),
            size: 43,
        };
        let bytes = br#"{"schemaVersion":1,"keyId":"x","version":"0.6.0","platform":"windows-x86_64","publishedAt":"2026-08-17T00:00:00Z","installer":{"fileName":"Nuclear.Downloader_0.6.0_x64-setup.exe","size":42,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"unexpected":true}"#;
        assert!(parse_and_validate_manifest(bytes, &version, &installer).is_err());
    }

    #[test]
    fn installer_hash_requires_canonical_lowercase_hex() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());
    }

    #[test]
    fn publication_timestamp_requires_a_real_canonical_utc_time() {
        assert!(validate_timestamp("2026-08-17T23:59:59Z").is_ok());
        assert!(validate_timestamp("2026-02-30T00:00:00Z").is_err());
        assert!(validate_timestamp("2026-08-17T24:00:00Z").is_err());
        assert!(validate_timestamp("2026-08-17t23:59:59z").is_err());
    }

    #[test]
    fn download_urls_require_https_without_credentials() {
        assert!(validate_download_url("https://github.com/example/setup.exe").is_ok());
        assert!(validate_download_url("http://github.com/example/setup.exe").is_err());
        assert!(validate_download_url("https://user:pass@example.com/setup.exe").is_err());
    }

    #[test]
    fn minisign_verification_covers_exact_manifest_bytes() {
        let public_key = b"untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let signature = b"untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
        let wrapped_key = base64_encode(public_key);
        let wrapped_signature = base64_encode(signature);
        assert!(
            verify_with_public_key(&wrapped_key, b"test", wrapped_signature.as_bytes()).is_ok()
        );
        assert!(
            verify_with_public_key(&wrapped_key, b"Test", wrapped_signature.as_bytes()).is_err()
        );
        assert!(verify_with_public_key(
            std::str::from_utf8(public_key).unwrap(),
            b"test",
            wrapped_signature.as_bytes()
        )
        .is_err());
    }

    #[test]
    fn key_rotation_selects_by_id_and_rejects_bad_configuration() {
        assert_eq!(
            select_public_key(
                "next",
                Some("current"),
                Some("current-key"),
                Some("next"),
                Some("next-key")
            )
            .unwrap(),
            "next-key"
        );
        assert!(
            select_public_key("current", Some("same"), Some("a"), Some("same"), Some("b")).is_err()
        );
        assert!(select_public_key("current", Some("current"), None, None, None).is_err());
        assert!(select_public_key("unknown", Some("current"), Some("a"), None, None).is_err());
    }

    #[tokio::test]
    async fn existing_installer_must_match_signed_size_and_hash() {
        let root = unique_test_root("updater-existing");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
        std::fs::write(&path, b"verified bytes").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
        assert!(open_verified_installer(&path, 14, &hash)
            .await
            .unwrap()
            .is_some());
        assert!(open_verified_installer(&path, 13, &hash)
            .await
            .unwrap()
            .is_none());
        assert!(open_verified_installer(&path, 14, &"a".repeat(64))
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn verified_installer_lease_denies_mutation_and_replacement() {
        let root = unique_test_root("updater-installer-lease");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
        let replacement = root.join("replacement.exe");
        std::fs::write(&path, b"verified bytes").unwrap();
        std::fs::write(&replacement, b"replacement!!!").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
        let installer = open_verified_installer(&path, 14, &hash)
            .await
            .unwrap()
            .unwrap();

        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::rename(&replacement, &path).is_err());
        assert_eq!(installer.path(), path);

        drop(installer);
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn partial_cleanup_removes_only_owned_regular_files() {
        let root = unique_test_root("updater-partials");
        std::fs::create_dir_all(&root).unwrap();
        let owned = root.join(
            "Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part",
        );
        let unrelated = root.join("someone-elses-download.part");
        std::fs::write(&owned, b"partial").unwrap();
        std::fs::write(&unrelated, b"keep").unwrap();
        cleanup_owned_partial_installers(&root).await.unwrap();
        assert!(!owned.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn old_installer_cleanup_keeps_current_and_unrelated_files() {
        let root = unique_test_root("updater-old-installers");
        std::fs::create_dir_all(&root).unwrap();
        let old = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
        let current = root.join("Nuclear.Downloader_0.6.1_x64-setup.exe");
        let noncanonical = root.join("Nuclear.Downloader_00.6.0_x64-setup.exe");
        let unrelated = root.join("keep.exe");
        for path in [&old, &current, &noncanonical, &unrelated] {
            std::fs::write(path, b"data").unwrap();
        }

        cleanup_owned_old_installers(&root, current.file_name().unwrap().to_str().unwrap())
            .await
            .unwrap();
        assert!(!old.exists());
        assert!(current.exists());
        assert!(noncanonical.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn updater_lock_is_retained_and_reused_after_release() {
        let root = unique_test_root("updater-lock");
        {
            let _lock = UpdateDirectoryLock::acquire(&root).unwrap();
        }
        assert!(root.join(UPDATE_LOCK_FILE_NAME).is_file());
        {
            let _lock = UpdateDirectoryLock::acquire(&root).unwrap();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn updater_rejects_reparse_root_and_preserves_reparse_partial() {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let root = unique_test_root("updater-reparse");
        let actual = root.join("actual");
        let linked = root.join("linked");
        std::fs::create_dir_all(&actual).unwrap();
        if symlink_dir(&actual, &linked).is_ok() {
            assert!(UpdateDirectoryLock::acquire(&linked).is_err());
            std::fs::remove_dir(&linked).unwrap();
        }

        let target = root.join("target.txt");
        std::fs::write(&target, b"keep").unwrap();
        let partial = actual.join(
            "Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part",
        );
        if symlink_file(&target, &partial).is_ok() {
            cleanup_owned_partial_installers(&actual).await.unwrap();
            assert!(partial.exists());
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
            std::fs::remove_file(&partial).unwrap();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    fn unique_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nuclear-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::new();
        for chunk in bytes.chunks(3) {
            let a = chunk[0] as u32;
            let b = chunk.get(1).copied().unwrap_or(0) as u32;
            let c = chunk.get(2).copied().unwrap_or(0) as u32;
            let value = (a << 16) | (b << 8) | c;
            output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
            output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
            output.push(if chunk.len() > 1 {
                ALPHABET[((value >> 6) & 0x3f) as usize] as char
            } else {
                '='
            });
            output.push(if chunk.len() > 2 {
                ALPHABET[(value & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        output
    }
}
