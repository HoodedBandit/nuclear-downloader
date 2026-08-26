use crate::models::{
    DownloaderRuntimeState, DownloaderRuntimeStatus, DownloaderRuntimeUpdateCheck,
    DownloaderRuntimeUpdateProgress, DownloaderToolStatus,
};
use futures_util::{future::join_all, StreamExt};
use reqwest::header::ACCEPT;
use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;
use zip::ZipArchive;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const RUNTIME_UPDATE_PROGRESS_EVENT: &str = "downloader-runtime-update-progress";
const MIN_RECOMMENDED_YTDLP_VERSION: &str = "2026.07.04";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
// First launch can require Windows Defender to inspect the large, freshly
// unpacked yt-dlp and FFmpeg executables. Keep the probes bounded, but allow
// enough time for that cold-start scan instead of reporting a false repair.
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_DESCRIPTOR_LIMIT: u64 = 64 * 1024;
const RUNTIME_MANIFEST_LIMIT: u64 = 64 * 1024;
const RUNTIME_SIGNATURE_LIMIT: u64 = 8 * 1024;
const RELEASE_METADATA_LIMIT: u64 = 1024 * 1024;
const RUNTIME_ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;
const RUNTIME_EXPANDED_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const RUNTIME_ENTRY_LIMIT: usize = 128;
const RUNTIME_ENTRY_SIZE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const RUNTIME_DEPTH_LIMIT: usize = 4;
const RUNTIME_COMPRESSION_RATIO_LIMIT: u64 = 200;
const RUNTIME_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RUNTIME_CURRENT_POINTER: &str = "current.json";
const RUNTIME_UPDATE_OWNER_MARKER: &str = ".nuclear-runtime-update-v1";
const RUNTIME_INSTALL_OWNER_MARKER: &str = ".nuclear-runtime-install-v1";
const RUNTIME_AUTH_DESCRIPTOR: &str = ".nuclear-runtime-descriptor-v1.json";
const RUNTIME_AUTH_SIGNATURE: &str = ".nuclear-runtime-descriptor-v1.json.sig";
static RUNTIME_UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static RUNTIME_UPDATE_CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct ToolSpec {
    name: &'static str,
    required: bool,
}

const REQUIRED_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "yt-dlp",
        required: true,
    },
    ToolSpec {
        name: "ffmpeg",
        required: true,
    },
    ToolSpec {
        name: "ffprobe",
        required: true,
    },
    ToolSpec {
        name: "deno",
        required: false,
    },
];

#[derive(Debug, Clone)]
pub struct YtdlpCommandConfig {
    pub ffmpeg_dir: Option<PathBuf>,
    pub deno_path: Option<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct RuntimeToolLease {
    path: PathBuf,
    source: String,
    runtime_version: Option<String>,
    _read_lease: Option<std::fs::File>,
}

impl RuntimeToolLease {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeManifest {
    #[serde(default)]
    schema_version: u32,
    runtime_version: String,
    platform: String,
    tools: Vec<RuntimeManifestTool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeManifestTool {
    name: String,
    version: String,
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedRuntimeDescriptor {
    schema_version: u32,
    key_id: String,
    runtime_version: String,
    platform: String,
    archive_name: String,
    compressed_size: u64,
    sha256: String,
    manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeCurrentPointer {
    schema_version: u32,
    runtime_version: String,
}

#[derive(Debug, Clone)]
struct RuntimeAssetSelection {
    version: String,
    archive_name: String,
    archive_url: String,
    archive_size: u64,
    archive_sha256: String,
    manifest_sha256: String,
    descriptor_bytes: Vec<u8>,
    signature_bytes: Vec<u8>,
}

pub(crate) fn plugin_dir() -> Option<PathBuf> {
    app_plugin_dir().exists().then(app_plugin_dir)
}

pub fn diagnostic_summary() -> String {
    REQUIRED_TOOLS
        .iter()
        .map(|tool| {
            resolve_tool_lease(tool.name)
                .ok()
                .flatten()
                .map(|resolution| {
                    format!(
                        "{}={} ({})",
                        tool.name,
                        resolution.path.display(),
                        resolution.source
                    )
                })
                .unwrap_or_else(|| format!("{}=missing", tool.name))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub async fn check_downloader_runtime() -> DownloaderRuntimeStatus {
    let mut tools = Vec::new();
    let mut missing_required = false;
    let mut deno_missing = false;
    let mut stale_ytdlp = false;
    let mut runtime_version: Option<String> = None;
    let mut source = "missing".to_string();
    let mut runtime_dir: Option<String> = None;
    let managed_validation_error = match discover_managed_runtime_at(&managed_runtime_root(), true)
    {
        Ok(Some(_)) => None,
        Ok(None) => None,
        Err(error) => Some(error),
    };

    let statuses = join_all(REQUIRED_TOOLS.iter().copied().map(tool_status)).await;
    for (spec, status) in REQUIRED_TOOLS.iter().zip(statuses) {
        if spec.required && !status.available {
            missing_required = true;
        }
        if spec.name == "deno" && !status.available {
            deno_missing = true;
        }
        if spec.name == "yt-dlp" {
            stale_ytdlp = status
                .version
                .as_deref()
                .map(|version| is_ytdlp_stale(version, MIN_RECOMMENDED_YTDLP_VERSION))
                .unwrap_or(false);
        }
        if status.source == "managed" {
            if let Ok(Some(resolution)) = resolve_tool_lease(spec.name) {
                runtime_version = resolution.runtime_version;
                runtime_dir = resolution
                    .path
                    .parent()
                    .map(|path| path.display().to_string());
            }
        }
        if source == "missing" && status.available {
            source = status.source.clone();
        }
        tools.push(status);
    }

    let missing_tool_names = missing_required_tool_names(&tools);
    let (state, message) = if managed_validation_error.is_some() {
        (
            DownloaderRuntimeState::RepairRequired,
            Some(
                "The managed runtime pointer or installed files failed integrity validation. Repair the downloader runtime."
                    .to_string(),
            ),
        )
    } else if missing_required {
        (
            DownloaderRuntimeState::RepairRequired,
            Some(format!(
                "Downloader runtime is missing or cannot run required tools: {}.",
                missing_tool_names.join(", ")
            )),
        )
    } else if deno_missing {
        (
            DownloaderRuntimeState::ReadyWithWarnings,
            Some("YouTube JavaScript runtime is missing; some public videos may fail.".to_string()),
        )
    } else if stale_ytdlp {
        (
            DownloaderRuntimeState::ReadyWithWarnings,
            Some(format!(
                "yt-dlp is older than the recommended baseline {MIN_RECOMMENDED_YTDLP_VERSION}."
            )),
        )
    } else {
        (
            DownloaderRuntimeState::Ready,
            Some("Downloader runtime is ready.".to_string()),
        )
    };

    DownloaderRuntimeStatus {
        state,
        runtime_version,
        source,
        update_available: false,
        latest_runtime_version: None,
        runtime_dir,
        plugin_dir: app_plugin_dir().display().to_string(),
        message,
        tools,
    }
}

pub async fn check_downloader_runtime_update() -> Result<DownloaderRuntimeUpdateCheck, String> {
    let latest = fetch_latest_runtime_asset().await?.ok_or_else(|| {
        "No downloader runtime bundle was found on the latest GitHub Release.".to_string()
    })?;

    let local_status = check_downloader_runtime().await;
    let local_version = local_status.runtime_version.clone().or_else(|| {
        local_status
            .tools
            .iter()
            .find(|tool| tool.name == "yt-dlp" && tool.available)
            .and_then(|tool| tool.version.clone())
    });
    let update_available = local_status.state == DownloaderRuntimeState::RepairRequired
        || local_version
            .as_deref()
            .map(|version| version_sort_key(version) < version_sort_key(&latest.version))
            .unwrap_or(true);

    Ok(DownloaderRuntimeUpdateCheck {
        update_available,
        latest_runtime_version: Some(latest.version.clone()),
        message: Some(if update_available {
            format!("Downloader runtime {} is available.", latest.version)
        } else {
            "Downloader runtime is current.".to_string()
        }),
    })
}

struct RuntimeUpdateGuard;

impl Drop for RuntimeUpdateGuard {
    fn drop(&mut self) {
        RUNTIME_UPDATE_CANCEL_REQUESTED.store(false, Ordering::SeqCst);
        RUNTIME_UPDATE_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

pub fn request_runtime_update_cancel() -> bool {
    if RUNTIME_UPDATE_IN_PROGRESS.load(Ordering::SeqCst) {
        RUNTIME_UPDATE_CANCEL_REQUESTED.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

pub async fn update_downloader_runtime(app: AppHandle) -> Result<DownloaderRuntimeStatus, String> {
    if RUNTIME_UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A downloader runtime update is already in progress.".into());
    }
    let _guard = RuntimeUpdateGuard;
    RUNTIME_UPDATE_CANCEL_REQUESTED.store(false, Ordering::SeqCst);

    let result = update_downloader_runtime_inner(&app).await;
    if let Err(error) = &result {
        emit_runtime_progress(&app, "error", None, 0, None, Some(error.clone()));
    }
    result
}

async fn update_downloader_runtime_inner(
    app: &AppHandle,
) -> Result<DownloaderRuntimeStatus, String> {
    emit_runtime_progress(
        app,
        "checking",
        None,
        0,
        None,
        Some("Checking GitHub Releases for a downloader runtime bundle.".into()),
    );

    let selection = fetch_latest_runtime_asset().await?.ok_or_else(|| {
        "No downloader runtime bundle was found on the latest GitHub Release.".to_string()
    })?;

    let client = build_client()?;
    let archive_name = Path::new(&selection.archive_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name == selection.archive_name)
        .ok_or_else(|| "Runtime release returned an unsafe archive filename.".to_string())?;
    let managed_root = managed_runtime_root();
    fs::create_dir_all(&managed_root)
        .await
        .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
    cleanup_abandoned_runtime_updates().await?;
    let update_id = uuid::Uuid::new_v4().to_string();
    let work_root = managed_root.join(".updates").join(&update_id);
    fs::create_dir_all(&work_root)
        .await
        .map_err(|error| format!("Failed to create runtime update staging folder: {error}"))?;
    fs::write(
        work_root.join(RUNTIME_UPDATE_OWNER_MARKER),
        b"schemaVersion=1\n",
    )
    .await
    .map_err(|error| format!("Failed to mark runtime update staging folder: {error}"))?;
    let archive_path = work_root.join(archive_name);

    emit_runtime_progress(
        app,
        "downloading",
        Some(selection.version.clone()),
        0,
        Some(selection.archive_size),
        Some(format!("Downloading {}.", selection.archive_name)),
    );

    let staging_dir = work_root.join("extracted");
    let install_result = async {
        let actual_checksum = tokio::time::timeout(
            RUNTIME_DOWNLOAD_TIMEOUT,
            download_archive(app, &client, &selection, &archive_path),
        )
        .await
        .map_err(|_| "Runtime download exceeded the 30-minute limit.".to_string())??;
        if actual_checksum != selection.archive_sha256 {
            return Err("Runtime bundle SHA-256 does not match the signed descriptor.".into());
        }

        emit_runtime_progress(
            app,
            "installing",
            Some(selection.version.clone()),
            selection.archive_size,
            Some(selection.archive_size),
            Some("Verifying and installing runtime bundle.".into()),
        );

        let archive_path_for_extract = archive_path.clone();
        let staging_dir_for_extract = staging_dir.clone();
        let manifest_dir = tokio::task::spawn_blocking(move || {
            extract_runtime_zip(&archive_path_for_extract, &staging_dir_for_extract)
        })
        .await
        .map_err(|error| format!("Runtime extraction worker failed: {error}"))??;
        let manifest_hash = sha256_file_sync(&manifest_dir.join("runtime-manifest.json"))?;
        if manifest_hash != selection.manifest_sha256 {
            return Err("Runtime manifest SHA-256 does not match the signed descriptor.".into());
        }
        let manifest = validate_manifest_at(&manifest_dir, true)?;
        ensure_runtime_update_not_cancelled()?;
        if manifest.schema_version != 1 {
            return Err("Signed runtime bundles must use runtime manifest schemaVersion 1.".into());
        }
        validate_runtime_version(&manifest.runtime_version)?;
        if manifest.runtime_version != selection.version {
            return Err(format!(
                "Runtime manifest version {} does not match release asset version {}.",
                manifest.runtime_version, selection.version
            ));
        }
        write_runtime_auth_contract(
            &manifest_dir,
            &selection.descriptor_bytes,
            &selection.signature_bytes,
        )?;
        write_runtime_install_marker(&manifest_dir, &manifest.runtime_version)?;

        let final_dir = managed_root.join(&manifest.runtime_version);
        let backup_dir = managed_root.join(format!(
            ".backup-{}-{}",
            manifest.runtime_version, update_id
        ));
        let mut warnings = Vec::new();
        if let Some(warning) =
            promote_runtime_atomically(&manifest_dir, &final_dir, &backup_dir).await?
        {
            warnings.push(warning);
        }
        write_current_pointer(&managed_root, &manifest.runtime_version).await?;
        if let Err(warning) =
            cleanup_old_runtime_versions(&managed_root, &manifest.runtime_version).await
        {
            warnings.push(warning);
        }
        Ok((manifest.runtime_version, warnings))
    }
    .await;

    let cleanup_warning = remove_owned_runtime_update_dir(&work_root).await.err();
    let (installed_version, mut warnings) = match install_result {
        Ok(result) => result,
        Err(error) => {
            return Err(match cleanup_warning {
                Some(cleanup) => format!("{error} {cleanup}"),
                None => error,
            });
        }
    };
    if let Some(cleanup) = cleanup_warning {
        warnings.push(cleanup);
    }

    emit_runtime_progress(
        app,
        "complete",
        Some(installed_version),
        selection.archive_size,
        Some(selection.archive_size),
        Some("Downloader runtime is updated.".into()),
    );

    let mut status = check_downloader_runtime().await;
    if !warnings.is_empty() {
        if status.state == DownloaderRuntimeState::Ready {
            status.state = DownloaderRuntimeState::ReadyWithWarnings;
        }
        let cleanup_message = warnings.join(" ");
        status.message = Some(match status.message.take() {
            Some(message) => format!("{message} {cleanup_message}"),
            None => cleanup_message,
        });
    }
    Ok(status)
}

fn ensure_runtime_update_not_cancelled() -> Result<(), String> {
    if RUNTIME_UPDATE_CANCEL_REQUESTED.load(Ordering::SeqCst) {
        Err("Runtime update was cancelled.".into())
    } else {
        Ok(())
    }
}

pub async fn cleanup_abandoned_runtime_updates() -> Result<(), String> {
    let updates_root = managed_runtime_root().join(".updates");
    cleanup_abandoned_runtime_updates_at(&updates_root).await
}

async fn cleanup_abandoned_runtime_updates_at(updates_root: &Path) -> Result<(), String> {
    if !fs::try_exists(&updates_root).await.unwrap_or(false) {
        return Ok(());
    }
    ensure_no_reparse_components(updates_root)?;
    if is_reparse_or_symlink(updates_root)? {
        return Err("Runtime update staging root is a reparse point; cleanup was refused.".into());
    }
    let mut entries = fs::read_dir(&updates_root)
        .await
        .map_err(|error| format!("Failed to inspect runtime update staging: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate runtime update staging: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if uuid::Uuid::parse_str(&name).is_err()
            || !entry
                .file_type()
                .await
                .map_err(|error| format!("Failed to inspect runtime staging entry: {error}"))?
                .is_dir()
            || is_reparse_or_symlink(&path)?
        {
            continue;
        }
        let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
        if !marker.is_file()
            || is_reparse_or_symlink(&marker)?
            || fs::read(&marker).await.ok().as_deref() != Some(b"schemaVersion=1\n")
        {
            continue;
        }
        remove_owned_runtime_update_dir(&path).await?;
    }
    Ok(())
}

fn validate_runtime_version(version: &str) -> Result<(), String> {
    if version.trim() != version || parse_dotted_numeric_version(version).is_none() {
        Err("Runtime version must contain exactly three numeric dotted components.".into())
    } else {
        Ok(())
    }
}

async fn promote_runtime_atomically(
    candidate_dir: &Path,
    final_dir: &Path,
    backup_dir: &Path,
) -> Result<Option<String>, String> {
    let had_existing = fs::try_exists(final_dir).await.unwrap_or(false);
    let final_version = final_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Runtime destination has an invalid version name.".to_string())?;
    validate_runtime_version(final_version)?;
    let existing_was_owned = if had_existing {
        installed_runtime_is_owned(final_dir, final_version)?
            && validate_manifest_at(final_dir, true)
                .map(|manifest| manifest.runtime_version == final_version)
                .unwrap_or(false)
    } else {
        false
    };
    if had_existing && !existing_was_owned {
        return Err("Refusing to replace an unowned or invalid managed runtime directory.".into());
    }
    if had_existing {
        if fs::try_exists(backup_dir).await.unwrap_or(false) {
            return Err("Refusing to overwrite an unexpected runtime backup path.".into());
        }
        fs::rename(final_dir, backup_dir).await.map_err(|error| {
            format!("Failed to stage the existing runtime for replacement: {error}")
        })?;
    }

    if let Err(error) = fs::rename(candidate_dir, final_dir).await {
        if had_existing {
            let _ = fs::rename(backup_dir, final_dir).await;
        }
        return Err(format!("Failed to publish runtime bundle: {error}"));
    }

    let warning = if had_existing && existing_was_owned {
        if !installed_runtime_is_owned(backup_dir, final_version)? {
            Some(format!(
                "Runtime is ready, but backup ownership changed; cleanup of {} was refused.",
                backup_dir.display()
            ))
        } else {
            fs::remove_dir_all(backup_dir).await.err().map(|error| {
                format!(
                    "Runtime is ready, but cleanup of backup {} failed: {error}",
                    backup_dir.display()
                )
            })
        }
    } else {
        None
    };
    Ok(warning)
}

async fn write_current_pointer(root: &Path, runtime_version: &str) -> Result<(), String> {
    validate_runtime_version(runtime_version)?;
    ensure_no_reparse_components(root)?;
    let pointer = RuntimeCurrentPointer {
        schema_version: 1,
        runtime_version: runtime_version.to_string(),
    };
    let bytes = serde_json::to_vec(&pointer)
        .map_err(|error| format!("Failed to serialize runtime pointer: {error}"))?;
    let temporary = root.join(format!(".current-{}.json.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|error| format!("Failed to create runtime pointer: {error}"))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| format!("Failed to write runtime pointer: {error}"))?;
    file.flush()
        .await
        .map_err(|error| format!("Failed to flush runtime pointer: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync runtime pointer: {error}"))?;
    drop(file);
    let destination = root.join(RUNTIME_CURRENT_POINTER);
    if destination.exists() {
        ensure_no_reparse_components(&destination)?;
        if is_reparse_or_symlink(&destination)? {
            let _ = std::fs::remove_file(&temporary);
            return Err("Runtime pointer destination is a reparse point.".into());
        }
    }
    replace_file_atomically(&temporary, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Failed to publish runtime pointer: {error}")
    })
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are valid, nul-terminated UTF-16 buffers for the
    // duration of this call. Flags request same-volume replacement and flush.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

async fn cleanup_old_runtime_versions(root: &Path, current: &str) -> Result<(), String> {
    let mut candidates = Vec::new();
    let mut stale_backups = Vec::new();
    let mut entries = fs::read_dir(root)
        .await
        .map_err(|error| format!("Failed to inspect managed runtimes: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate managed runtimes: {error}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| format!("Failed to inspect a managed runtime: {error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() && name.starts_with(".backup-") {
            if let Ok(manifest) = validate_manifest_at(&entry.path(), false) {
                if installed_runtime_is_owned(&entry.path(), &manifest.runtime_version)? {
                    stale_backups.push((entry.path(), manifest.runtime_version));
                }
            }
            continue;
        }
        if !file_type.is_dir()
            || name.starts_with('.')
            || name == current
            || validate_runtime_version(&name).is_err()
        {
            continue;
        }
        if validate_manifest_at(&entry.path(), false).is_ok()
            && installed_runtime_is_owned(&entry.path(), &name)?
        {
            candidates.push((version_sort_key(&name), entry.path(), name));
        }
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path, version) in candidates.into_iter().skip(1) {
        if !installed_runtime_is_owned(&path, &version)? {
            return Err(format!(
                "Refusing to remove runtime {} because ownership changed.",
                path.display()
            ));
        }
        fs::remove_dir_all(&path).await.map_err(|error| {
            format!(
                "Runtime was installed, but cleanup of {} failed: {error}",
                path.display()
            )
        })?;
    }
    for (path, version) in stale_backups {
        if !installed_runtime_is_owned(&path, &version)? {
            return Err(format!(
                "Refusing to remove backup {} because ownership changed.",
                path.display()
            ));
        }
        fs::remove_dir_all(&path).await.map_err(|error| {
            format!(
                "Runtime is ready, but cleanup of stale backup {} failed: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

async fn tool_status(spec: ToolSpec) -> DownloaderToolStatus {
    let resolution = match resolve_tool_lease(spec.name) {
        Ok(Some(resolution)) => resolution,
        Ok(None) => {
            return DownloaderToolStatus {
                name: spec.name.to_string(),
                required: spec.required,
                available: false,
                version: None,
                path: None,
                source: "missing".into(),
                error: Some("Required runtime tool was not found.".into()),
            };
        }
        Err(error) => {
            return DownloaderToolStatus {
                name: spec.name.to_string(),
                required: spec.required,
                available: false,
                version: None,
                path: None,
                source: "managed".into(),
                error: Some(error),
            };
        }
    };
    match tool_version(spec.name, &resolution.path).await {
        Ok(version) => DownloaderToolStatus {
            name: spec.name.to_string(),
            required: spec.required,
            available: true,
            version: Some(version),
            path: Some(resolution.path.display().to_string()),
            source: resolution.source,
            error: None,
        },
        Err(error) => DownloaderToolStatus {
            name: spec.name.to_string(),
            required: spec.required,
            available: false,
            version: None,
            path: Some(resolution.path.display().to_string()),
            source: resolution.source,
            error: Some(error),
        },
    }
}

pub(crate) fn resolve_tool_lease(name: &str) -> Result<Option<RuntimeToolLease>, String> {
    if let Some((runtime_dir, manifest)) =
        discover_managed_runtime_at(&managed_runtime_root(), false)?
    {
        let tool = manifest
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| format!("Managed runtime manifest does not contain {name}."))?;
        let path = runtime_dir.join(&tool.path);
        let file = open_verified_tool_file(&path, Some(&tool.sha256))?;
        return Ok(Some(RuntimeToolLease {
            path,
            source: "managed".into(),
            runtime_version: Some(manifest.runtime_version),
            _read_lease: Some(file),
        }));
    }

    if let Some(path) = bundled_tool_path(name) {
        let file = open_verified_tool_file(&path, None)?;
        return Ok(Some(RuntimeToolLease {
            path,
            source: "bundled".into(),
            runtime_version: None,
            _read_lease: Some(file),
        }));
    }

    #[cfg(debug_assertions)]
    {
        Ok(Some(RuntimeToolLease {
            path: PathBuf::from(name),
            source: "path".into(),
            runtime_version: None,
            _read_lease: None,
        }))
    }

    #[cfg(not(debug_assertions))]
    {
        Ok(None)
    }
}

fn open_verified_tool_file(path: &Path, expected_sha256: Option<&str>) -> Result<File, String> {
    ensure_no_reparse_components(path)?;
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect runtime executable: {error}"))?;
    if !path_metadata.is_file()
        || is_reparse_or_symlink(path)?
        || path_metadata.len() == 0
        || path_metadata.len() > RUNTIME_ENTRY_SIZE_LIMIT
    {
        return Err("Runtime executable must be a bounded regular non-reparse file.".into());
    }

    let mut options = std::fs::OpenOptions::new();
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
        .map_err(|error| format!("Failed to acquire runtime executable lease: {error}"))?;
    ensure_no_reparse_components(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect leased runtime executable: {error}"))?;
    if !opened_metadata.is_file()
        || opened_metadata.len() != path_metadata.len()
        || metadata_is_reparse(&opened_metadata)
    {
        return Err("Runtime executable identity changed while acquiring its lease.".into());
    }

    if let Some(expected_sha256) = expected_sha256 {
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("Failed to hash leased runtime executable: {error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("{:x}", hasher.finalize()) != expected_sha256 {
            return Err("Runtime executable failed signed integrity validation.".into());
        }
    }

    let final_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to re-inspect leased runtime executable: {error}"))?;
    if final_metadata.len() != opened_metadata.len() || metadata_is_reparse(&final_metadata) {
        return Err("Runtime executable changed during integrity validation.".into());
    }
    Ok(file)
}

fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
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

fn discover_managed_runtime_at(
    root: &Path,
    verify_hashes: bool,
) -> Result<Option<(PathBuf, RuntimeManifest)>, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to inspect managed runtime root: {error}")),
    };
    ensure_no_reparse_components(root)?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(root)? {
        return Err("Managed runtime root must be a non-reparse directory.".into());
    }
    let pointer_path = root.join(RUNTIME_CURRENT_POINTER);
    let pointer_metadata = match std::fs::symlink_metadata(&pointer_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "Failed to inspect managed runtime pointer: {error}"
            ))
        }
    };
    if let Some(metadata) = pointer_metadata {
        ensure_no_reparse_components(&pointer_path)?;
        if !metadata.is_file() || is_reparse_or_symlink(&pointer_path)? {
            return Err("Managed runtime pointer must be a regular non-reparse file.".into());
        }
        if metadata.len() > RUNTIME_MANIFEST_LIMIT {
            return Err("Managed runtime pointer exceeds the 64 KiB limit.".into());
        }
        let pointer_bytes = std::fs::read(&pointer_path)
            .map_err(|error| format!("Failed to read managed runtime pointer: {error}"))?;
        let pointer: RuntimeCurrentPointer = serde_json::from_slice(&pointer_bytes)
            .map_err(|error| format!("Failed to parse managed runtime pointer: {error}"))?;
        if pointer.schema_version != 1
            || validate_runtime_version(&pointer.runtime_version).is_err()
        {
            return Err("Managed runtime pointer has an unsupported schema or version.".into());
        }
        let runtime_dir = root.join(&pointer.runtime_version);
        let manifest = validate_installed_runtime_at(&runtime_dir, verify_hashes)?;
        if manifest.runtime_version != pointer.runtime_version {
            return Err("Managed runtime pointer and manifest versions do not match.".into());
        }
        return Ok(Some((runtime_dir, manifest)));
    }

    // Compatibility is limited to unmarked pre-0.6 runtime directories. Once
    // a marker-owned 0.6 runtime exists, current.json is authoritative and its
    // absence is a repair condition rather than an invitation to scan/select.
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("Failed to inspect managed runtime root: {error}"))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Failed to enumerate managed runtime root: {error}"))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "Managed runtime contains a non-UTF-8 entry name.".to_string())?
            .to_string();
        if name.starts_with('.') || validate_runtime_version(&name).is_err() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("Failed to inspect managed runtime entry: {error}"))?;
        if !metadata.is_dir() || is_reparse_or_symlink(&path)? {
            return Err("Managed runtime version entry must be a non-reparse directory.".into());
        }
        if installed_runtime_is_owned(&path, &name)? {
            return Err(
                "Managed runtime current.json is missing for a marker-owned runtime.".into(),
            );
        }
        let manifest = validate_manifest_at(&path, verify_hashes)?;
        if manifest.runtime_version != name {
            return Err(
                "Legacy managed runtime directory and manifest versions do not match.".into(),
            );
        }
        candidates.push((path, manifest));
    }

    candidates.sort_by(|(_, left), (_, right)| {
        version_sort_key(&left.runtime_version).cmp(&version_sort_key(&right.runtime_version))
    });
    Ok(candidates.pop())
}

fn bundled_tool_path(name: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = exe_dir.join(tool_exe_name(name));
    path.is_file().then_some(path)
}

fn tool_exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

async fn tool_version(name: &str, path: &Path) -> Result<String, String> {
    let output = crate::downloader::run_supervised_probe(
        path,
        tool_version_args(name),
        TOOL_PROBE_TIMEOUT,
        64 * 1024,
        64 * 1024,
    )
    .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "{name} exited with code {}",
                output.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_line = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown");

    Ok(first_line.to_string())
}

fn tool_version_args(name: &str) -> &'static [&'static str] {
    match name {
        "ffmpeg" | "ffprobe" => &["-version"],
        _ => &["--version"],
    }
}

fn missing_required_tool_names(tools: &[DownloaderToolStatus]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| tool.required && !tool.available)
        .map(|tool| tool.name.clone())
        .collect()
}

fn validate_manifest_at(dir: &Path, verify_hashes: bool) -> Result<RuntimeManifest, String> {
    ensure_no_reparse_components(dir)?;
    let root_metadata = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("Failed to inspect runtime root: {error}"))?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(dir)? {
        return Err("Runtime root must be a regular non-reparse directory.".into());
    }
    let canonical_root = std::fs::canonicalize(dir)
        .map_err(|error| format!("Failed to canonicalize runtime root: {error}"))?;
    let manifest_path = dir.join("runtime-manifest.json");
    ensure_no_reparse_components(&manifest_path)?;
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("Failed to inspect runtime manifest: {error}"))?;
    if !metadata.is_file()
        || is_reparse_or_symlink(&manifest_path)?
        || metadata.len() > RUNTIME_MANIFEST_LIMIT
    {
        return Err(
            "Runtime manifest must be a regular non-reparse file no larger than 64 KiB.".into(),
        );
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("Failed to read runtime manifest: {error}"))?;
    let manifest = serde_json::from_str::<RuntimeManifest>(&manifest_text)
        .map_err(|error| format!("Failed to parse runtime manifest: {error}"))?;

    if manifest.schema_version != 1 {
        return Err(format!(
            "Unsupported runtime manifest schema version {}.",
            manifest.schema_version
        ));
    }
    validate_runtime_version(&manifest.runtime_version)?;

    if manifest.platform != runtime_platform() {
        return Err(format!(
            "Runtime bundle platform {} does not match {}.",
            manifest.platform,
            runtime_platform()
        ));
    }

    for required in REQUIRED_TOOLS.iter().filter(|tool| tool.required) {
        if !manifest.tools.iter().any(|tool| tool.name == required.name) {
            return Err(format!("Runtime manifest is missing {}.", required.name));
        }
    }

    let mut seen_names = std::collections::HashSet::new();
    for tool in &manifest.tools {
        if !REQUIRED_TOOLS.iter().any(|spec| spec.name == tool.name)
            || !seen_names.insert(tool.name.as_str())
        {
            return Err("Runtime manifest contains an unknown or duplicate tool.".into());
        }
        if tool.version.trim().is_empty()
            || tool.version.len() > 256
            || tool.version.chars().any(char::is_control)
        {
            return Err(format!("Runtime tool {} is missing a version.", tool.name));
        }
        validate_relative_manifest_path(&tool.path)?;
        validate_canonical_sha256(&tool.sha256)?;
        let path = dir.join(&tool.path);
        ensure_no_reparse_components(&path)?;
        if !path.is_file() {
            return Err(format!("Runtime tool {} was not found.", tool.name));
        }
        if is_reparse_or_symlink(&path)? {
            return Err(format!(
                "Runtime tool {} is a symbolic link or reparse point.",
                tool.name
            ));
        }
        let canonical_tool = std::fs::canonicalize(&path).map_err(|error| {
            format!("Failed to canonicalize runtime tool {}: {error}", tool.name)
        })?;
        let relative = canonical_tool.strip_prefix(&canonical_root).map_err(|_| {
            format!(
                "Runtime tool {} resolves outside the runtime root.",
                tool.name
            )
        })?;
        if relative.as_os_str().is_empty() {
            return Err(format!(
                "Runtime tool {} does not resolve to a file beneath the runtime root.",
                tool.name
            ));
        }
        if verify_hashes {
            let actual = sha256_file_sync(&path)?;
            if !actual.eq_ignore_ascii_case(&tool.sha256) {
                return Err(format!(
                    "Runtime tool {} checksum mismatch: expected {}, got {}.",
                    tool.name, tool.sha256, actual
                ));
            }
        }
    }

    Ok(manifest)
}

fn validate_installed_runtime_at(
    dir: &Path,
    verify_hashes: bool,
) -> Result<RuntimeManifest, String> {
    let manifest = validate_manifest_at(dir, verify_hashes)?;
    if installed_runtime_is_owned(dir, &manifest.runtime_version)? {
        validate_runtime_auth_contract(dir, &manifest)?;
    }
    Ok(manifest)
}

fn validate_runtime_auth_contract(
    dir: &Path,
    manifest: &RuntimeManifest,
) -> Result<SignedRuntimeDescriptor, String> {
    let descriptor_path = dir.join(RUNTIME_AUTH_DESCRIPTOR);
    let signature_path = dir.join(RUNTIME_AUTH_SIGNATURE);
    let descriptor_bytes = read_regular_bounded_file(
        &descriptor_path,
        RUNTIME_DESCRIPTOR_LIMIT,
        "installed runtime descriptor",
    )?;
    let signature_bytes = read_regular_bounded_file(
        &signature_path,
        RUNTIME_SIGNATURE_LIMIT,
        "installed runtime descriptor signature",
    )?;
    let descriptor = parse_runtime_descriptor(&descriptor_bytes)?;
    crate::updater::verify_release_signature_for_key(
        &descriptor.key_id,
        &descriptor_bytes,
        &signature_bytes,
    )?;
    if descriptor.runtime_version != manifest.runtime_version {
        return Err("Installed runtime descriptor and manifest versions do not match.".into());
    }
    let actual_manifest_hash = sha256_file_sync(&dir.join("runtime-manifest.json"))?;
    if actual_manifest_hash != descriptor.manifest_sha256 {
        return Err("Installed runtime manifest failed signed integrity validation.".into());
    }
    Ok(descriptor)
}

fn read_regular_bounded_file(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, String> {
    ensure_no_reparse_components(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {label}: {error}"))?;
    if !metadata.is_file() || is_reparse_or_symlink(path)? || metadata.len() > limit {
        return Err(format!(
            "{label} must be a regular non-reparse file no larger than {limit} bytes."
        ));
    }
    std::fs::read(path).map_err(|error| format!("Failed to read {label}: {error}"))
}

fn validate_relative_manifest_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.as_os_str().len() > 512 || path.is_absolute() {
        return Err("Runtime manifest contains an unsafe tool path.".into());
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err("Runtime manifest contains an unsafe tool path.".into());
        };
        let Some(name) = name.to_str() else {
            return Err("Runtime manifest tool paths must be UTF-8.".into());
        };
        validate_runtime_path_component(name)?;
    }
    Ok(())
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
        if !component_path.exists() {
            continue;
        }
        if is_reparse_or_symlink(component_path)? {
            return Err(format!(
                "Path traverses a symbolic link or reparse point: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

fn local_data_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("NuclearDownloader")
}

fn managed_runtime_root() -> PathBuf {
    local_data_root().join("runtime")
}

fn app_plugin_dir() -> PathBuf {
    local_data_root().join("plugins")
}

fn runtime_platform() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x64"
    } else if cfg!(windows) {
        "windows"
    } else {
        "unknown"
    }
}

fn version_sort_key(raw: &str) -> Version {
    let normalized = raw.trim().trim_start_matches('v');
    if let Some(version) = parse_dotted_numeric_version(normalized) {
        return version;
    }

    Version::parse(normalized).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn parse_dotted_numeric_version(raw: &str) -> Option<Version> {
    let parts = raw.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| part.is_empty())
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }

    Some(Version::new(
        parts[0].parse::<u64>().ok()?,
        parts[1].parse::<u64>().ok()?,
        parts[2].parse::<u64>().ok()?,
    ))
}

pub fn is_ytdlp_stale(version: &str, minimum: &str) -> bool {
    version_sort_key(version) < version_sort_key(minimum)
}

async fn fetch_latest_runtime_asset() -> Result<Option<RuntimeAssetSelection>, String> {
    let client = build_client()?;
    let release = fetch_latest_release(&client).await?;
    let (descriptor_asset, signature_asset) = select_runtime_descriptor_assets(&release)?;
    let descriptor_bytes = download_bounded_body(
        &client,
        &descriptor_asset.browser_download_url,
        RUNTIME_DESCRIPTOR_LIMIT,
        "runtime descriptor",
    )
    .await?;
    let signature_bytes = download_bounded_body(
        &client,
        &signature_asset.browser_download_url,
        RUNTIME_SIGNATURE_LIMIT,
        "runtime descriptor signature",
    )
    .await?;
    let descriptor = parse_runtime_descriptor(&descriptor_bytes)?;
    crate::updater::verify_release_signature_for_key(
        &descriptor.key_id,
        &descriptor_bytes,
        &signature_bytes,
    )?;
    Ok(Some(select_runtime_archive(
        &release,
        descriptor,
        descriptor_bytes,
        signature_bytes,
    )?))
}

fn build_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(
            "NuclearDownloaderRuntime/1 (+https://github.com/HoodedBandit/nuclear-downloader)",
        )
        .connect_timeout(NETWORK_CONNECT_TIMEOUT)
        .read_timeout(NETWORK_READ_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to prepare runtime update client: {error}"))
}

async fn fetch_latest_release(client: &Client) -> Result<GitHubRelease, String> {
    let response = client
        .get(GITHUB_RELEASES_LATEST_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to reach GitHub Releases: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "GitHub runtime update check failed with HTTP {}.",
            status.as_u16()
        ));
    }
    let body =
        read_response_limited(response, RELEASE_METADATA_LIMIT, "GitHub release metadata").await?;
    serde_json::from_slice::<GitHubRelease>(&body)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {error}"))
}

async fn read_response_limited(
    response: reqwest::Response,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        if body.len().saturating_add(chunk.len()) as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn select_runtime_descriptor_assets(
    release: &GitHubRelease,
) -> Result<(&GitHubReleaseAsset, &GitHubReleaseAsset), String> {
    let descriptor =
        select_exact_runtime_asset(release, "nuclear-downloader-runtime-windows-x64.json")?;
    let signature =
        select_exact_runtime_asset(release, "nuclear-downloader-runtime-windows-x64.json.sig")?;
    Ok((descriptor, signature))
}

fn select_exact_runtime_asset<'a>(
    release: &'a GitHubRelease,
    expected: &str,
) -> Result<&'a GitHubReleaseAsset, String> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == expected);
    let selected = matches
        .next()
        .ok_or_else(|| format!("Required runtime release asset {expected} was not found."))?;
    if matches.next().is_some() {
        return Err(format!("Runtime release asset {expected} is ambiguous."));
    }
    Ok(selected)
}

fn parse_runtime_descriptor(bytes: &[u8]) -> Result<SignedRuntimeDescriptor, String> {
    let descriptor: SignedRuntimeDescriptor = serde_json::from_slice(bytes)
        .map_err(|error| format!("Failed to parse signed runtime descriptor: {error}"))?;
    if descriptor.schema_version != 1 {
        return Err(format!(
            "Unsupported runtime descriptor schema version {}.",
            descriptor.schema_version
        ));
    }
    if !crate::updater::is_canonical_update_key_id(&descriptor.key_id) {
        return Err("Runtime descriptor key ID is not in the canonical release-key format.".into());
    }
    validate_runtime_version(&descriptor.runtime_version)?;
    if descriptor.platform != "windows-x64" {
        return Err("Runtime descriptor platform must be exactly windows-x64.".into());
    }
    let expected_name = format!(
        "nuclear-downloader-runtime-{}-windows-x64.zip",
        descriptor.runtime_version
    );
    if descriptor.archive_name != expected_name {
        return Err("Runtime descriptor archive name does not match its version.".into());
    }
    if descriptor.compressed_size == 0 || descriptor.compressed_size > RUNTIME_ARCHIVE_LIMIT {
        return Err("Runtime descriptor compressed size is outside the allowed range.".into());
    }
    validate_canonical_sha256(&descriptor.sha256)?;
    validate_canonical_sha256(&descriptor.manifest_sha256)?;
    Ok(descriptor)
}

fn select_runtime_archive(
    release: &GitHubRelease,
    descriptor: SignedRuntimeDescriptor,
    descriptor_bytes: Vec<u8>,
    signature_bytes: Vec<u8>,
) -> Result<RuntimeAssetSelection, String> {
    let archive = select_exact_runtime_asset(release, &descriptor.archive_name)?;
    let candidates = release
        .assets
        .iter()
        .filter(|asset| {
            asset.name.starts_with("nuclear-downloader-runtime-")
                && asset.name.ends_with("-windows-x64.zip")
        })
        .count();
    if candidates != 1 {
        return Err("The release contains ambiguous or extra runtime archives.".into());
    }
    if archive.size != descriptor.compressed_size {
        return Err("GitHub runtime archive size does not match the signed descriptor.".into());
    }
    Ok(RuntimeAssetSelection {
        version: descriptor.runtime_version,
        archive_name: descriptor.archive_name,
        archive_url: archive.browser_download_url.clone(),
        archive_size: descriptor.compressed_size,
        archive_sha256: descriptor.sha256,
        manifest_sha256: descriptor.manifest_sha256,
        descriptor_bytes,
        signature_bytes,
    })
}

fn validate_canonical_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Runtime SHA-256 must be 64 lowercase hexadecimal digits.".into());
    }
    Ok(())
}

async fn download_bounded_body(
    client: &Client,
    url: &str,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    validate_https_url(url)?;
    let response = client
        .get(url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download {label}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download {label}: HTTP {}.",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        if output.len().saturating_add(chunk.len()) as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn download_archive(
    app: &AppHandle,
    client: &Client,
    selection: &RuntimeAssetSelection,
    archive_path: &Path,
) -> Result<String, String> {
    validate_https_url(&selection.archive_url)?;
    let response = client
        .get(&selection.archive_url)
        .send()
        .await
        .map_err(|error| format!("Failed to download runtime bundle: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Failed to download runtime bundle: HTTP {}.",
            status.as_u16()
        ));
    }

    if response
        .content_length()
        .is_some_and(|size| size != selection.archive_size || size > RUNTIME_ARCHIVE_LIMIT)
    {
        return Err("Runtime Content-Length does not match the signed descriptor.".into());
    }

    let total_bytes = response.content_length().or(Some(selection.archive_size));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(archive_path)
        .await
        .map_err(|error| format!("Failed to create runtime archive file: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();

    while let Some(chunk_result) = stream.next().await {
        ensure_runtime_update_not_cancelled()?;
        let chunk =
            chunk_result.map_err(|error| format!("Failed while downloading runtime: {error}"))?;
        downloaded_bytes = downloaded_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "Runtime download byte count overflowed.".to_string())?;
        if downloaded_bytes > selection.archive_size || downloaded_bytes > RUNTIME_ARCHIVE_LIMIT {
            return Err("Runtime archive exceeded its signed size or the 1 GiB limit.".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Failed to write runtime archive: {error}"))?;
        hasher.update(&chunk);
        emit_runtime_progress(
            app,
            "downloading",
            Some(selection.version.clone()),
            downloaded_bytes,
            total_bytes,
            Some(format!("Downloading {}.", selection.archive_name)),
        );
    }

    file.flush()
        .await
        .map_err(|error| format!("Failed to finalize runtime archive: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync runtime archive: {error}"))?;
    if downloaded_bytes != selection.archive_size {
        return Err(format!(
            "Runtime archive size mismatch: expected {} bytes, got {downloaded_bytes} bytes.",
            selection.archive_size
        ));
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_runtime_zip(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    if staging_dir.exists() {
        return Err("Runtime extraction staging already exists; refusing to overwrite it.".into());
    }
    let staging_parent = staging_dir
        .parent()
        .ok_or_else(|| "Runtime staging path has no parent.".to_string())?;
    ensure_no_reparse_components(staging_parent)?;
    std::fs::create_dir(staging_dir)
        .map_err(|error| format!("Failed to create runtime staging folder: {error}"))?;
    ensure_no_reparse_components(staging_dir)?;
    ensure_no_reparse_components(archive_path)?;

    let file = File::open(archive_path)
        .map_err(|error| format!("Failed to open runtime archive: {error}"))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| format!("Failed to read runtime archive: {error}"))?;

    if archive.len() > RUNTIME_ENTRY_LIMIT {
        return Err(format!(
            "Runtime archive contains {} entries; the limit is {RUNTIME_ENTRY_LIMIT}.",
            archive.len()
        ));
    }
    let mut expanded_total = 0u64;
    let mut normalized_paths = HashMap::<String, bool>::new();
    let mut manifest_count = 0usize;
    for index in 0..archive.len() {
        ensure_runtime_update_not_cancelled()?;
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to inspect runtime archive entry: {error}"))?;
        let enclosed_name = entry
            .enclosed_name()
            .ok_or_else(|| "Runtime archive contains an unsafe path.".to_string())?;
        let depth = enclosed_name.components().count();
        if depth == 0 || depth > RUNTIME_DEPTH_LIMIT {
            return Err(format!(
                "Runtime archive entry {} exceeds depth {RUNTIME_DEPTH_LIMIT}.",
                entry.name()
            ));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Runtime archive contains a symbolic-link entry.".into());
        }
        let is_directory = entry.is_dir();
        let normalized = normalize_runtime_archive_path(&enclosed_name)?;
        if normalized
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "runtime-manifest.json")
            && !is_directory
        {
            manifest_count += 1;
        }
        if normalized_paths
            .insert(normalized.clone(), is_directory)
            .is_some()
        {
            return Err(format!(
                "Runtime archive contains a duplicate or case-colliding path: {}.",
                entry.name()
            ));
        }
        let components = normalized.split('/').collect::<Vec<_>>();
        for parent_depth in 1..components.len() {
            let parent = components[..parent_depth].join("/");
            if normalized_paths.get(&parent) == Some(&false) {
                return Err("Runtime archive contains a file/directory path conflict.".into());
            }
        }
        if !is_directory
            && normalized_paths
                .iter()
                .any(|(path, _)| path.starts_with(&format!("{normalized}/")))
        {
            return Err("Runtime archive contains a file/directory path conflict.".into());
        }
        if entry.size() > RUNTIME_ENTRY_SIZE_LIMIT {
            return Err(format!(
                "Runtime archive entry {} exceeds the 2 GiB limit.",
                entry.name()
            ));
        }
        if entry.size() > 0
            && (entry.compressed_size() == 0
                || entry.size()
                    > entry
                        .compressed_size()
                        .saturating_mul(RUNTIME_COMPRESSION_RATIO_LIMIT))
        {
            return Err(format!(
                "Runtime archive entry {} exceeds the 200:1 compression-ratio limit.",
                entry.name()
            ));
        }
        expanded_total = expanded_total
            .checked_add(entry.size())
            .ok_or_else(|| "Runtime archive expanded size overflowed.".to_string())?;
        if expanded_total > RUNTIME_EXPANDED_LIMIT {
            return Err("Runtime archive exceeds the 4 GiB expanded-size limit.".into());
        }
    }
    if manifest_count != 1 || !normalized_paths.contains_key("runtime-manifest.json") {
        return Err("Runtime archive must contain exactly one root runtime-manifest.json.".into());
    }
    preflight_free_space(staging_dir, expanded_total)?;

    for index in 0..archive.len() {
        ensure_runtime_update_not_cancelled()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read runtime archive entry: {error}"))?;
        let enclosed_name = entry
            .enclosed_name()
            .ok_or_else(|| "Runtime archive contains an unsafe path.".to_string())?;
        let output_path = staging_dir.join(enclosed_name);

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
            ensure_no_reparse_components(&output_path)?;
        } else {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
                ensure_no_reparse_components(parent)?;
            }
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output_path)
                .map_err(|error| format!("Failed to create runtime file: {error}"))?;
            let mut copied = 0u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                ensure_runtime_update_not_cancelled()?;
                let read = entry
                    .read(&mut buffer)
                    .map_err(|error| format!("Failed to read runtime archive entry: {error}"))?;
                if read == 0 {
                    break;
                }
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| "Runtime extraction byte count overflowed.".to_string())?;
                if copied > entry.size() || copied > RUNTIME_ENTRY_SIZE_LIMIT {
                    return Err("Runtime archive entry exceeded its declared size.".into());
                }
                ensure_runtime_update_not_cancelled()?;
                output
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("Failed to extract runtime file: {error}"))?;
            }
            if copied != entry.size() {
                return Err("Runtime archive entry ended before its declared size.".into());
            }
            output
                .flush()
                .map_err(|error| format!("Failed to flush runtime file: {error}"))?;
            output
                .sync_all()
                .map_err(|error| format!("Failed to sync runtime file: {error}"))?;
        }
    }

    Ok(staging_dir.to_path_buf())
}

fn normalize_runtime_archive_path(path: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err("Runtime archive contains a non-normal path component.".into());
        };
        let name = name
            .to_str()
            .ok_or_else(|| "Runtime archive paths must be UTF-8.".to_string())?;
        validate_runtime_path_component(name)?;
        components.push(name.to_ascii_lowercase());
    }
    if components.is_empty() {
        return Err("Runtime archive contains an empty path.".into());
    }
    Ok(components.join("/"))
}

fn validate_runtime_path_component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || name.ends_with(['.', ' '])
        || is_windows_reserved_device_name(name)
    {
        return Err("Runtime archive contains a non-canonical Windows path.".into());
    }
    Ok(())
}

fn is_windows_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(windows)]
fn preflight_free_space(path: &Path, required: u64) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available = 0u64;
    // SAFETY: `wide` is a valid nul-terminated UTF-16 path and the output
    // pointer is valid for the duration of the call.
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(format!(
            "Failed to determine free space for runtime extraction: {}",
            std::io::Error::last_os_error()
        ));
    }
    if available < required {
        return Err(format!(
            "Runtime extraction requires {required} bytes, but only {available} bytes are available."
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn preflight_free_space(_path: &Path, _required: u64) -> Result<(), String> {
    Ok(())
}

fn validate_https_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("Invalid runtime asset URL: {error}"))?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err("Runtime asset URL must use HTTPS.".into())
    }
}

fn sha256_file_sync(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_runtime_install_marker(dir: &Path, version: &str) -> Result<(), String> {
    validate_runtime_version(version)?;
    ensure_no_reparse_components(dir)?;
    let marker = dir.join(RUNTIME_INSTALL_OWNER_MARKER);
    let contents = format!("schemaVersion=1\nruntimeVersion={version}\n");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker)
        .map_err(|error| format!("Failed to create runtime ownership marker: {error}"))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("Failed to write runtime ownership marker: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Failed to sync runtime ownership marker: {error}"))
}

fn write_runtime_auth_contract(
    dir: &Path,
    descriptor_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    if descriptor_bytes.is_empty()
        || descriptor_bytes.len() as u64 > RUNTIME_DESCRIPTOR_LIMIT
        || signature_bytes.is_empty()
        || signature_bytes.len() as u64 > RUNTIME_SIGNATURE_LIMIT
    {
        return Err("Runtime authentication contract exceeds its size limits.".into());
    }
    let descriptor = parse_runtime_descriptor(descriptor_bytes)?;
    crate::updater::verify_release_signature_for_key(
        &descriptor.key_id,
        descriptor_bytes,
        signature_bytes,
    )?;
    ensure_no_reparse_components(dir)?;
    for (name, bytes) in [
        (RUNTIME_AUTH_DESCRIPTOR, descriptor_bytes),
        (RUNTIME_AUTH_SIGNATURE, signature_bytes),
    ] {
        let path = dir.join(name);
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("Failed to create {name}: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("Failed to write {name}: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Failed to sync {name}: {error}"))?;
    }
    Ok(())
}

fn installed_runtime_is_owned(dir: &Path, version: &str) -> Result<bool, String> {
    if validate_runtime_version(version).is_err()
        || !dir.is_dir()
        || ensure_no_reparse_components(dir).is_err()
    {
        return Ok(false);
    }
    let marker = dir.join(RUNTIME_INSTALL_OWNER_MARKER);
    if !marker.is_file() || is_reparse_or_symlink(&marker)? {
        return Ok(false);
    }
    let expected = format!("schemaVersion=1\nruntimeVersion={version}\n");
    Ok(std::fs::read(&marker)
        .map(|contents| contents == expected.as_bytes())
        .unwrap_or(false))
}

async fn remove_owned_runtime_update_dir(path: &Path) -> Result<(), String> {
    if !fs::try_exists(path).await.unwrap_or(false) {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Runtime update staging has an invalid name.".to_string())?;
    if uuid::Uuid::parse_str(name).is_err() || ensure_no_reparse_components(path).is_err() {
        return Err("Refusing to remove an unowned or reparse runtime staging path.".into());
    }
    let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
    if !marker.is_file()
        || is_reparse_or_symlink(&marker)?
        || fs::read(&marker).await.ok().as_deref() != Some(b"schemaVersion=1\n")
    {
        return Err(
            "Refusing to remove runtime staging without its exact ownership marker.".into(),
        );
    }
    ensure_no_reparse_components(path)?;
    fs::remove_dir_all(path).await.map_err(|error| {
        format!(
            "Cleanup of owned runtime staging directory {} failed: {error}",
            path.display()
        )
    })
}

fn emit_runtime_progress(
    app: &AppHandle,
    status: &str,
    version: Option<String>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: Option<String>,
) {
    if let Err(error) = app.emit(
        RUNTIME_UPDATE_PROGRESS_EVENT,
        DownloaderRuntimeUpdateProgress {
            status: status.to_string(),
            version,
            downloaded_bytes,
            total_bytes,
            message,
        },
    ) {
        eprintln!("Failed to emit runtime update progress: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use zip::write::SimpleFileOptions;

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

    fn release_with_assets(assets: Vec<(&str, u64)>) -> GitHubRelease {
        GitHubRelease {
            assets: assets
                .into_iter()
                .map(|(name, size)| GitHubReleaseAsset {
                    name: name.into(),
                    browser_download_url: format!("https://example.com/{name}"),
                    size,
                })
                .collect(),
        }
    }

    #[test]
    fn runtime_versions_require_three_numeric_components() {
        for valid in ["2026.06.09", "1.0.0"] {
            assert!(validate_runtime_version(valid).is_ok());
        }
        for invalid in [
            "2026.06",
            "2026.06.09.1",
            "v2026.06.09",
            "2026.06.beta",
            "２０２６.０６.０９",
        ] {
            assert!(
                validate_runtime_version(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn runtime_http_reader_rejects_streamed_overflow() {
        let url = start_http_fixture(vec![(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nabcde\r\n0\r\n\r\n"
                .to_vec(),
            Duration::ZERO,
        )]);
        let response = Client::new().get(url).send().await.unwrap();
        let error = read_response_limited(response, 4, "fixture")
            .await
            .unwrap_err();
        assert!(error.contains("exceeds the 4-byte limit"));
    }

    #[tokio::test]
    async fn runtime_http_reader_enforces_idle_read_timeout() {
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
        let error = read_response_limited(response, 5, "fixture")
            .await
            .unwrap_err();
        assert!(error.contains("Failed while reading fixture"));
    }

    #[test]
    fn stale_ytdlp_detection_uses_recommended_baseline() {
        assert!(is_ytdlp_stale("2026.03.17", "2026.06.09"));
        assert!(!is_ytdlp_stale("2026.06.09", "2026.06.09"));
        assert!(!is_ytdlp_stale("2026.07.01", "2026.06.09"));
    }

    #[test]
    fn version_probe_args_match_tool_cli_contracts() {
        assert_eq!(tool_version_args("yt-dlp"), &["--version"]);
        assert_eq!(tool_version_args("deno"), &["--version"]);
        assert_eq!(tool_version_args("ffmpeg"), &["-version"]);
        assert_eq!(tool_version_args("ffprobe"), &["-version"]);
    }

    #[test]
    fn deno_absence_degrades_instead_of_blocking_downloads() {
        let deno = REQUIRED_TOOLS
            .iter()
            .find(|tool| tool.name == "deno")
            .expect("deno tool spec");

        assert!(!deno.required);
    }

    #[test]
    fn signed_descriptor_selects_one_exact_runtime_archive() {
        let release = release_with_assets(vec![
            ("nuclear-downloader-runtime-windows-x64.json", 100),
            ("nuclear-downloader-runtime-windows-x64.json.sig", 100),
            ("nuclear-downloader-runtime-2026.06.09-windows-x64.zip", 123),
            (
                "nuclear-downloader-runtime-2026.06.09-windows-x64.zip.sha256",
                100,
            ),
        ]);
        let (descriptor, signature) = select_runtime_descriptor_assets(&release).unwrap();
        assert!(descriptor.name.ends_with(".json"));
        assert!(signature.name.ends_with(".json.sig"));
        let selected = select_runtime_archive(
            &release,
            SignedRuntimeDescriptor {
                schema_version: 1,
                key_id: "key-1".into(),
                runtime_version: "2026.06.09".into(),
                platform: "windows-x64".into(),
                archive_name: "nuclear-downloader-runtime-2026.06.09-windows-x64.zip".into(),
                compressed_size: 123,
                sha256: "a".repeat(64),
                manifest_sha256: "b".repeat(64),
            },
            b"descriptor".to_vec(),
            b"signature".to_vec(),
        )
        .unwrap();
        assert_eq!(selected.version, "2026.06.09");
        assert!(selected.archive_name.ends_with(".zip"));
        assert_eq!(selected.archive_sha256, "a".repeat(64));
    }

    #[test]
    fn runtime_descriptor_is_strict_and_binds_exact_archive_name() {
        let valid = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"nuclear-downloader-runtime-2026.06.09-windows-x64.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;
        let descriptor = parse_runtime_descriptor(valid).unwrap();
        assert_eq!(descriptor.runtime_version, "2026.06.09");

        let wrong_name = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"other.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;
        assert!(parse_runtime_descriptor(wrong_name).is_err());
        let unknown_field = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"nuclear-downloader-runtime-2026.06.09-windows-x64.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","extra":true}"#;
        assert!(parse_runtime_descriptor(unknown_field).is_err());
    }

    #[test]
    fn runtime_zip_rejects_excessive_depth() {
        let root = unique_test_root("runtime-zip-depth");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("runtime.zip");
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "one/two/three/four/five.exe",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let error = extract_runtime_zip(&archive_path, &root.join("extracted")).unwrap_err();
        assert!(error.contains("depth"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_zip_rejects_extreme_compression_ratio() {
        let root = unique_test_root("runtime-zip-ratio");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("runtime.zip");
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "payload.exe",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&vec![0_u8; 1024 * 1024]).unwrap();
        zip.finish().unwrap();

        let error = extract_runtime_zip(&archive_path, &root.join("extracted")).unwrap_err();
        assert!(error.contains("compression-ratio"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_zip_rejects_case_collisions_and_file_directory_conflicts() {
        let root = unique_test_root("runtime-zip-collisions");
        fs::create_dir_all(&root).unwrap();

        let case_archive = root.join("case-collision.zip");
        let file = File::create(&case_archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for name in ["runtime-manifest.json", "TOOLS/tool.exe", "tools/TOOL.exe"] {
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
        let error = extract_runtime_zip(&case_archive, &root.join("case-output")).unwrap_err();
        assert!(error.contains("duplicate or case-colliding"));

        let conflict_archive = root.join("file-directory-conflict.zip");
        let file = File::create(&conflict_archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for name in ["runtime-manifest.json", "tools", "tools/tool.exe"] {
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
        let error =
            extract_runtime_zip(&conflict_archive, &root.join("conflict-output")).unwrap_err();
        assert!(error.contains("file/directory path conflict"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_zip_requires_one_exact_root_manifest_and_new_staging() {
        let root = unique_test_root("runtime-zip-manifest");
        fs::create_dir_all(&root).unwrap();

        let nested_archive = root.join("nested-manifest.zip");
        let file = File::create(&nested_archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "nested/runtime-manifest.json",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        let error = extract_runtime_zip(&nested_archive, &root.join("nested-output")).unwrap_err();
        assert!(error.contains("exactly one root runtime-manifest.json"));

        let extra_manifest_archive = root.join("extra-manifest.zip");
        let file = File::create(&extra_manifest_archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for name in ["runtime-manifest.json", "nested/runtime-manifest.json"] {
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(b"{}").unwrap();
        }
        zip.finish().unwrap();
        let error =
            extract_runtime_zip(&extra_manifest_archive, &root.join("extra-output")).unwrap_err();
        assert!(error.contains("exactly one root runtime-manifest.json"));

        let valid_archive = root.join("valid.zip");
        let file = File::create(&valid_archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "runtime-manifest.json",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        let staging = root.join("existing-staging");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("sentinel.txt"), b"keep").unwrap();
        let error = extract_runtime_zip(&valid_archive, &staging).unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        assert_eq!(fs::read(staging.join("sentinel.txt")).unwrap(), b"keep");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_paths_reject_windows_reserved_device_stems() {
        for reserved in [
            "CON",
            "con.txt",
            "PRN.exe",
            "AUX",
            "NUL.json",
            "COM1.exe",
            "com9.data",
            "LPT1",
            "lpt9.exe",
        ] {
            assert!(
                normalize_runtime_archive_path(Path::new(reserved)).is_err(),
                "accepted {reserved}"
            );
            assert!(
                validate_relative_manifest_path(reserved).is_err(),
                "accepted manifest path {reserved}"
            );
        }
        for allowed in ["console.exe", "com0.exe", "com10.exe", "lpt0.exe"] {
            assert!(normalize_runtime_archive_path(Path::new(allowed)).is_ok());
        }
    }

    #[tokio::test]
    async fn abandoned_runtime_cleanup_requires_uuid_and_owner_marker() {
        let root = unique_test_root("runtime-cleanup");
        let updates = root.join(".updates");
        let owned = updates.join("550e8400-e29b-41d4-a716-446655440000");
        let unowned = updates.join("not-owned");
        fs::create_dir_all(&owned).unwrap();
        fs::create_dir_all(&unowned).unwrap();
        fs::write(
            owned.join(RUNTIME_UPDATE_OWNER_MARKER),
            b"schemaVersion=1\n",
        )
        .unwrap();
        fs::write(unowned.join("data.bin"), b"keep").unwrap();

        cleanup_abandoned_runtime_updates_at(&updates)
            .await
            .unwrap();
        assert!(!owned.exists());
        assert!(unowned.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_runtime_integrity_is_verified_before_trust() {
        let root = unique_test_root("runtime-integrity");
        let runtime_dir = write_test_runtime_tree(&root, "2026.06.09", false, true);
        assert!(discover_managed_runtime_at(&root, true).unwrap().is_some());

        fs::write(runtime_dir.join("yt-dlp.exe"), b"tampered").unwrap();
        let error = discover_managed_runtime_at(&root, true).unwrap_err();
        assert!(error.contains("checksum mismatch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn marker_owned_runtime_requires_signed_auth_contract() {
        let root = unique_test_root("runtime-auth-required");
        write_test_runtime_tree(&root, "2026.06.09", true, true);
        let error = discover_managed_runtime_at(&root, true).unwrap_err();
        assert!(error.contains("installed runtime descriptor"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn runtime_tool_lease_denies_mutation_and_rehashes_before_use() {
        let root = unique_test_root("runtime-tool-lease");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("yt-dlp.exe");
        let replacement = root.join("replacement.exe");
        fs::write(&path, b"trusted-runtime-tool").unwrap();
        fs::write(&replacement, b"malicious-runtime!!").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"trusted-runtime-tool"));

        let lease = open_verified_tool_file(&path, Some(&hash)).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::rename(&replacement, &path).is_err());
        drop(lease);

        fs::write(&path, b"tampered-runtime-tool").unwrap();
        assert!(open_verified_tool_file(&path, Some(&hash)).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn marker_owned_runtime_requires_current_pointer() {
        let root = unique_test_root("runtime-pointer-required");
        write_test_runtime_tree(&root, "2026.06.09", true, false);
        let error = discover_managed_runtime_at(&root, true).unwrap_err();
        assert!(error.contains("current.json is missing"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unmarked_legacy_runtime_has_a_bounded_migration_path() {
        let root = unique_test_root("runtime-legacy-migration");
        write_test_runtime_tree(&root, "2026.06.09", false, false);
        let discovered = discover_managed_runtime_at(&root, true)
            .unwrap()
            .expect("legacy runtime");
        assert_eq!(discovered.1.runtime_version, "2026.06.09");
        let _ = fs::remove_dir_all(root);
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

    fn write_test_runtime_tree(
        root: &Path,
        version: &str,
        write_marker: bool,
        write_pointer: bool,
    ) -> PathBuf {
        let runtime_dir = root.join(version);
        fs::create_dir_all(&runtime_dir).unwrap();
        let mut manifest_tools = Vec::new();
        for tool in ["yt-dlp", "ffmpeg", "ffprobe"] {
            let bytes = format!("{tool}-fixture");
            fs::write(runtime_dir.join(format!("{tool}.exe")), bytes.as_bytes()).unwrap();
            manifest_tools.push(format!(
                r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{:x}"}}"#,
                Sha256::digest(bytes.as_bytes())
            ));
        }
        fs::write(
            runtime_dir.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"{version}","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();
        if write_marker {
            write_runtime_install_marker(&runtime_dir, version).unwrap();
        }
        if write_pointer {
            fs::write(
                root.join(RUNTIME_CURRENT_POINTER),
                serde_json::to_vec(&RuntimeCurrentPointer {
                    schema_version: 1,
                    runtime_version: version.to_string(),
                })
                .unwrap(),
            )
            .unwrap();
        }
        runtime_dir
    }

    #[test]
    fn validates_runtime_manifest_and_tool_hashes() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-runtime-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let tools = ["yt-dlp", "ffmpeg", "ffprobe", "deno"];
        let mut manifest_tools = Vec::new();
        for tool in tools {
            let path = root.join(format!("{tool}.exe"));
            fs::write(&path, tool.as_bytes()).unwrap();
            let hash = format!("{:x}", Sha256::digest(tool.as_bytes()));
            manifest_tools.push(format!(
                r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{hash}"}}"#
            ));
        }

        fs::write(
            root.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();

        let manifest = validate_manifest_at(&root, true).unwrap();
        assert_eq!(manifest.runtime_version, "2026.06.09");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_allows_optional_deno_to_be_absent() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-runtime-no-deno-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let mut manifest_tools = Vec::new();
        for tool in ["yt-dlp", "ffmpeg", "ffprobe"] {
            let path = root.join(format!("{tool}.exe"));
            fs::write(&path, tool.as_bytes()).unwrap();
            let hash = format!("{:x}", Sha256::digest(tool.as_bytes()));
            manifest_tools.push(format!(
                r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{hash}"}}"#
            ));
        }
        fs::write(
            root.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();

        assert!(validate_manifest_at(&root, true).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_rejects_missing_schema_version() {
        let root = unique_test_root("runtime-no-schema");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("runtime-manifest.json"),
            r#"{"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[]}"#,
        )
        .unwrap();
        let error = validate_manifest_at(&root, false).unwrap_err();
        assert!(error.contains("schema version 0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_rejects_unknown_manifest_and_tool_fields() {
        let unknown_manifest = r#"{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[],"unexpected":true}"#;
        assert!(serde_json::from_str::<RuntimeManifest>(unknown_manifest).is_err());

        let unknown_tool = r#"{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{"name":"yt-dlp","version":"1.0.0","path":"yt-dlp.exe","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","unexpected":true}]}"#;
        assert!(serde_json::from_str::<RuntimeManifest>(unknown_tool).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn runtime_manifest_rejects_a_reparse_runtime_root() {
        use std::os::windows::fs::symlink_dir;

        let root = unique_test_root("runtime-reparse-root");
        let actual = root.join("actual");
        let linked = root.join("linked");
        fs::create_dir_all(&actual).unwrap();
        if symlink_dir(&actual, &linked).is_err() {
            let _ = fs::remove_dir_all(root);
            return;
        }

        let error = validate_manifest_at(&linked, false).unwrap_err();
        assert!(error.contains("reparse point"));
        fs::remove_dir(&linked).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn atomic_runtime_promotion_rolls_back_on_publish_failure() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-runtime-promote-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let final_dir = root.join("current");
        let candidate_dir = root.join("missing-candidate");
        let backup_dir = root.join("backup");
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("marker.txt"), "original").unwrap();

        let result = promote_runtime_atomically(&candidate_dir, &final_dir, &backup_dir).await;

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(final_dir.join("marker.txt")).unwrap(),
            "original"
        );
        assert!(!backup_dir.exists());
        let _ = fs::remove_dir_all(root);
    }
}
