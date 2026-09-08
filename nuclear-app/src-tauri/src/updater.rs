mod artifact;

use crate::lifecycle::{PublicationKind, UpdateRunError, UpdateTaskContext};
use crate::models::{UpdateCheckResult, UpdateInstallProgress};
use futures_util::StreamExt;
use minisign_verify::Signature;
use reqwest::header::ACCEPT;
use reqwest::{Client, Response};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

#[cfg(test)]
use artifact::UPDATE_LOCK_FILE_NAME;
use artifact::{
    cleanup_current_artifact, cleanup_file_if_exists, cleanup_owned_old_installers,
    cleanup_owned_partial_installers, cleanup_owned_prepared_directories,
    create_prepared_directory, ensure_no_reparse_components, open_or_quarantine_cached_installer,
    open_verified_installer, owner_record_path, updater_directory, write_owner_record,
    CachedInstaller, UpdateDirectoryLock, VerifiedInstaller, INSTALLER_LIMIT,
};
pub(crate) use artifact::{cleanup_owned_installer_stages, InstallerHandoff};

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const UPDATE_PROGRESS_EVENT: &str = "update-install-progress";
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

fn signed_update_minimum() -> Version {
    Version::new(0, 6, 0)
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

pub(crate) async fn prepare_app_update(
    app: &AppHandle,
    expected_version: String,
    context: &UpdateTaskContext,
) -> Result<InstallerHandoff, UpdateRunError> {
    prepare_app_update_inner(app, expected_version.clone(), context)
        .await
        .inspect_err(|error| {
            let (status, message) = match error {
                UpdateRunError::Cancelled => ("cancelled", "App update was cancelled.".to_string()),
                UpdateRunError::Failed(error) => ("error", error.summary.clone()),
            };
            emit_install_progress(
                app,
                UpdateInstallProgress {
                    status: status.into(),
                    version: normalize_version_label(&expected_version),
                    downloaded_bytes: 0,
                    total_bytes: None,
                    message: Some(message),
                },
            );
        })
}

async fn prepare_app_update_inner(
    app: &AppHandle,
    expected_version: String,
    context: &UpdateTaskContext,
) -> Result<InstallerHandoff, UpdateRunError> {
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    {
        let _ = app;
        let _ = expected_version;
        let _ = context;
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
            )
            .into());
        }
        let client = build_client(updater_user_agent(&current_semver.to_string()))?;
        let release = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = fetch_latest_release(&client) => result,
        }?;
        let latest_semver = parse_release_tag(&release.tag_name)?;
        if latest_semver != expected_semver {
            return Err(format!(
                "The latest GitHub release changed from {expected_semver} to {latest_semver}. Please check for updates again."
            )
            .into());
        }
        let verified = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = verify_release_contract(&client, &release, &latest_semver) => result,
        }?;
        context.check_cancelled()?;
        let target_dir = updater_directory();
        let directory_lock = UpdateDirectoryLock::acquire(&target_dir)?;
        cleanup_owned_prepared_directories(&target_dir).await?;
        cleanup_owned_partial_installers(&target_dir).await?;
        cleanup_owned_old_installers(&target_dir, &verified.manifest.installer.file_name).await?;
        let installer = tokio::time::timeout(
            INSTALLER_OVERALL_TIMEOUT,
            download_installer(app, &client, &target_dir, &verified, context),
        )
        .await
        .map_err(|_| "Update installer download exceeded the 30-minute limit.".to_string())??;
        context.check_cancelled()?;
        Ok(InstallerHandoff {
            expected_version: latest_semver.to_string(),
            installer_name: verified.manifest.installer.file_name.clone(),
            installer_size: verified.manifest.installer.size,
            installer,
            _directory_lock: directory_lock,
        })
    }
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
    crate::artifact_contract::is_canonical_update_key_id(value)
}

fn verify_with_public_key(
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
    context: &UpdateTaskContext,
) -> Result<VerifiedInstaller, UpdateRunError> {
    ensure_no_reparse_components(target_dir)?;
    let installer = verified.installer_asset;
    validate_download_url(&installer.browser_download_url)?;
    let file_name = sanitize_file_name(&verified.manifest.installer.file_name)?;
    let canonical_path = target_dir.join(&file_name);
    let publication_dir = match open_or_quarantine_cached_installer(
        target_dir,
        &canonical_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await?
    {
        CachedInstaller::Verified(installer) => {
            context.check_cancelled()?;
            return Ok(installer);
        }
        CachedInstaller::Vacant => target_dir.to_path_buf(),
        CachedInstaller::UnownedCollision => {
            create_prepared_directory(
                target_dir,
                verified.manifest.installer.size,
                &verified.manifest.installer.sha256,
            )
            .await?
        }
    };
    let final_path = publication_dir.join(&file_name);
    let part_path = publication_dir.join(format!("{file_name}.{}.part", uuid::Uuid::new_v4()));
    let response = tokio::select! {
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        result = client.get(&installer.browser_download_url).send() => result,
    }
    .map_err(|error| format!("Failed to download update installer: {error}"))?;
    if !response.status().is_success() {
        let error = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            error = read_http_error(response, "update installer") => error,
        };
        return Err(error.into());
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
    if let Err(error) = write_owner_record(
        &part_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await
    {
        drop(file);
        cleanup_file_if_exists(&part_path).await;
        return Err(error.into());
    }
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

    loop {
        let chunk_result = tokio::select! {
            _ = context.cancelled() => {
                cleanup_current_artifact(&part_path).await;
                return Err(UpdateRunError::Cancelled);
            }
            result = stream.next() => result,
        };
        let Some(chunk_result) = chunk_result else {
            break;
        };
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                cleanup_current_artifact(&part_path).await;
                return Err(format!("Failed while downloading update installer: {error}").into());
            }
        };
        downloaded_bytes = downloaded_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "Installer byte count overflowed.".to_string())?;
        if downloaded_bytes > verified.manifest.installer.size || downloaded_bytes > INSTALLER_LIMIT
        {
            cleanup_current_artifact(&part_path).await;
            return Err("Installer exceeded its signed size or the 1 GiB limit.".into());
        }
        if let Err(error) = file.write_all(&chunk).await {
            cleanup_current_artifact(&part_path).await;
            return Err(format!("Failed to write installer download: {error}").into());
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
        cleanup_current_artifact(&part_path).await;
        return Err(format!(
            "Downloaded installer size mismatch: expected {} bytes, got {downloaded_bytes} bytes.",
            verified.manifest.installer.size
        )
        .into());
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
        cleanup_current_artifact(&part_path).await;
        return Err("Downloaded installer SHA-256 does not match the signed manifest.".into());
    }
    let publication = match context.enter_publication(PublicationKind::InstallerCache) {
        Ok(publication) => publication,
        Err(error) => {
            cleanup_current_artifact(&part_path).await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_no_reparse_components(target_dir) {
        cleanup_current_artifact(&part_path).await;
        return Err(error.into());
    }
    if fs::try_exists(&final_path).await.unwrap_or(false) {
        cleanup_current_artifact(&part_path).await;
        return Err("The updater destination appeared while publishing the installer.".into());
    }
    if let Err(error) = fs::hard_link(&part_path, &final_path).await {
        cleanup_current_artifact(&part_path).await;
        return Err(format!(
            "Failed to publish the verified installer without replacement: {error}"
        )
        .into());
    }
    let installer = open_verified_installer(
        &final_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await?
    .ok_or_else(|| {
        "The published installer changed before its execution lease was acquired.".to_string()
    })?;
    if let Err(error) = write_owner_record(
        &final_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await
    {
        cleanup_current_artifact(&part_path).await;
        return Err(error.into());
    }
    if let Err(error) = fs::remove_file(&part_path).await {
        eprintln!(
            "Published the verified installer, but failed to remove its partial link: {error}"
        );
    }
    if let Ok(record_path) = owner_record_path(&part_path) {
        cleanup_file_if_exists(&record_path).await;
    }
    drop(publication);
    context.check_cancelled()?;
    Ok(installer)
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
    if !crate::artifact_contract::is_canonical_sha256(value) {
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
mod tests;
