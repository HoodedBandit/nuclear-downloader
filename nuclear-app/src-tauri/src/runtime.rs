mod cache;
mod verified;

use crate::lifecycle::{PublicationKind, UpdateRunError, UpdateTaskContext};
use crate::models::{
    DownloaderRuntimeState, DownloaderRuntimeStatus, DownloaderRuntimeUpdateCheck,
    DownloaderRuntimeUpdateProgress, DownloaderToolStatus,
};
use crate::notifications::RuntimeProgressSink;
use crate::runtime_transaction::{
    self, RuntimeMutationLock, RuntimeTransaction, RuntimeTransactionCheckpoint,
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
#[cfg(test)]
use std::sync::LazyLock;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;
use zip::ZipArchive;

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Mutex;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
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

type RuntimeSignatureVerifier = fn(&str, &[u8], &[u8]) -> Result<(), String>;

#[cfg(test)]
use cache::RuntimeCache;
#[cfg(test)]
use verified::{
    build_verified_bundled_snapshot_from_lock_at, initialize_test_runtime_cache_at,
    open_verified_tool_file, resolve_tool_lease_from_cache, resolve_tool_lease_uncached_at,
    validate_bundled_sidecar_lock, BundledSidecarLock, VerifiedRuntimeSnapshot,
    VerifiedRuntimeTool, BUNDLED_SIDECAR_LOCK,
};
use verified::{
    build_verified_runtime_snapshot_async, initialize_runtime_cache_at, VERIFIED_RUNTIME_CACHE,
};
pub(crate) use verified::{resolve_tool_lease, RuntimeToolLease};

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
struct RuntimeHashCounters {
    total_invocations: u64,
    total_bytes: u64,
    manifest_invocations: u64,
    manifest_bytes: u64,
    tool_invocations: u64,
    tool_bytes: u64,
    resolution_calls: u64,
    successful_resolutions: u64,
}

#[cfg(test)]
static TEST_MANIFEST_HASH_INVOCATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_MANIFEST_HASH_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_TOOL_HASH_INVOCATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_TOOL_HASH_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_RUNTIME_RESOLUTION_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_SCOPED_HASH_COUNTERS: LazyLock<Mutex<HashMap<PathBuf, RuntimeHashCounters>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
fn reset_runtime_hash_counters() {
    for counter in [
        &TEST_MANIFEST_HASH_INVOCATIONS,
        &TEST_MANIFEST_HASH_BYTES,
        &TEST_TOOL_HASH_INVOCATIONS,
        &TEST_TOOL_HASH_BYTES,
        &TEST_RUNTIME_RESOLUTION_CALLS,
        &TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS,
    ] {
        counter.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn runtime_hash_counters() -> RuntimeHashCounters {
    let manifest_invocations = TEST_MANIFEST_HASH_INVOCATIONS.load(Ordering::SeqCst);
    let manifest_bytes = TEST_MANIFEST_HASH_BYTES.load(Ordering::SeqCst);
    let tool_invocations = TEST_TOOL_HASH_INVOCATIONS.load(Ordering::SeqCst);
    let tool_bytes = TEST_TOOL_HASH_BYTES.load(Ordering::SeqCst);
    RuntimeHashCounters {
        total_invocations: manifest_invocations + tool_invocations,
        total_bytes: manifest_bytes + tool_bytes,
        manifest_invocations,
        manifest_bytes,
        tool_invocations,
        tool_bytes,
        resolution_calls: TEST_RUNTIME_RESOLUTION_CALLS.load(Ordering::SeqCst),
        successful_resolutions: TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS.load(Ordering::SeqCst),
    }
}

#[cfg(test)]
fn reset_runtime_hash_counters_for_root(root: &Path) {
    TEST_SCOPED_HASH_COUNTERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(root.to_path_buf(), RuntimeHashCounters::default());
}

#[cfg(test)]
fn runtime_hash_counters_for_root(root: &Path) -> RuntimeHashCounters {
    TEST_SCOPED_HASH_COUNTERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(root)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
fn record_scoped_hash_invocation(path: &Path, is_runtime_tool: bool) {
    let mut scopes = TEST_SCOPED_HASH_COUNTERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (root, counters) in scopes.iter_mut() {
        if path.starts_with(root) {
            counters.total_invocations += 1;
            if is_runtime_tool {
                counters.tool_invocations += 1;
            } else {
                counters.manifest_invocations += 1;
            }
        }
    }
}

#[cfg(test)]
fn record_scoped_hash_bytes(path: &Path, is_runtime_tool: bool, bytes: u64) {
    let mut scopes = TEST_SCOPED_HASH_COUNTERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (root, counters) in scopes.iter_mut() {
        if path.starts_with(root) {
            counters.total_bytes += bytes;
            if is_runtime_tool {
                counters.tool_bytes += bytes;
            } else {
                counters.manifest_bytes += bytes;
            }
        }
    }
}

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

pub(crate) async fn initialize_runtime_cache(
    cancellation: CancellationToken,
) -> Result<(), String> {
    initialize_runtime_cache_at(
        &VERIFIED_RUNTIME_CACHE,
        managed_runtime_root(),
        bundled_executable_root(),
        crate::updater::verify_release_signature_for_key,
        cancellation,
    )
    .await
    .map(drop)
}

pub async fn check_downloader_runtime() -> DownloaderRuntimeStatus {
    check_downloader_runtime_cancellable(CancellationToken::new())
        .await
        .unwrap_or_else(|error| DownloaderRuntimeStatus {
            state: DownloaderRuntimeState::RepairRequired,
            runtime_version: None,
            source: "missing".into(),
            update_available: false,
            latest_runtime_version: None,
            runtime_dir: None,
            plugin_dir: app_plugin_dir().display().to_string(),
            message: Some(error),
            tools: Vec::new(),
        })
}

pub(crate) async fn check_downloader_runtime_cancellable(
    cancellation: CancellationToken,
) -> Result<DownloaderRuntimeStatus, String> {
    let mut tools = Vec::new();
    let mut missing_required = false;
    let mut deno_missing = false;
    let mut stale_ytdlp = false;
    let mut runtime_version: Option<String> = None;
    let mut source = "missing".to_string();
    let mut runtime_dir: Option<String> = None;
    let health_snapshot = initialize_runtime_cache_at(
        &VERIFIED_RUNTIME_CACHE,
        managed_runtime_root(),
        bundled_executable_root(),
        crate::updater::verify_release_signature_for_key,
        cancellation.clone(),
    )
    .await;
    if cancellation.is_cancelled() {
        return Err("Runtime health check was cancelled during shutdown.".into());
    }
    let managed_validation_error = match &health_snapshot {
        Ok(_) => None,
        Err(error) => Some(error.clone()),
    };
    if let Ok(Some(snapshot)) = &health_snapshot {
        runtime_version.clone_from(&snapshot.runtime_version);
        runtime_dir = snapshot
            .runtime_dir
            .as_ref()
            .map(|path| path.display().to_string());
    }

    let statuses = join_all(
        REQUIRED_TOOLS
            .iter()
            .copied()
            .map(|spec| tool_status(spec, cancellation.clone())),
    )
    .await;
    for (spec, status) in REQUIRED_TOOLS.iter().zip(statuses) {
        let status = status?;
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

    Ok(DownloaderRuntimeStatus {
        state,
        runtime_version,
        source,
        update_available: false,
        latest_runtime_version: None,
        runtime_dir,
        plugin_dir: app_plugin_dir().display().to_string(),
        message,
        tools,
    })
}

pub(crate) async fn check_downloader_runtime_update_with_status_cancellable(
    cancellation: CancellationToken,
) -> Result<(DownloaderRuntimeUpdateCheck, DownloaderRuntimeStatus), String> {
    let latest = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            return Err("Runtime update check was cancelled during shutdown.".into());
        }
        result = fetch_latest_runtime_asset() => result,
    }?
    .ok_or_else(|| {
        "No downloader runtime bundle was found on the latest GitHub Release.".to_string()
    })?;

    let local_status = check_downloader_runtime_cancellable(cancellation).await?;
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

    let update = DownloaderRuntimeUpdateCheck {
        update_available,
        latest_runtime_version: Some(latest.version.clone()),
        message: Some(if update_available {
            format!("Downloader runtime {} is available.", latest.version)
        } else {
            "Downloader runtime is current.".to_string()
        }),
    };
    Ok((update, local_status))
}

pub async fn update_downloader_runtime(
    progress: RuntimeProgressSink,
    context: UpdateTaskContext,
) -> Result<DownloaderRuntimeStatus, UpdateRunError> {
    let result = update_downloader_runtime_inner(&progress, &context).await;
    if let Err(error) = &result {
        let (status, message) = match error {
            UpdateRunError::Cancelled => ("cancelled", "Runtime update was cancelled.".to_string()),
            UpdateRunError::Failed(error) => ("error", error.summary.clone()),
        };
        emit_runtime_progress(&progress, status, None, 0, None, Some(message));
    }
    result
}

async fn update_downloader_runtime_inner(
    progress: &RuntimeProgressSink,
    context: &UpdateTaskContext,
) -> Result<DownloaderRuntimeStatus, UpdateRunError> {
    emit_runtime_progress(
        progress,
        "checking",
        None,
        0,
        None,
        Some("Checking GitHub Releases for a downloader runtime bundle.".into()),
    );

    let selection = tokio::select! {
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        result = fetch_latest_runtime_asset() => result,
    }?
    .ok_or_else(|| {
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
    recover_runtime_update_transaction().await?;
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
        progress,
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
            download_archive(progress, &client, &selection, &archive_path, context),
        )
        .await
        .map_err(|_| "Runtime download exceeded the 30-minute limit.".to_string())??;
        if actual_checksum != selection.archive_sha256 {
            return Err("Runtime bundle SHA-256 does not match the signed descriptor.".into());
        }

        emit_runtime_progress(
            progress,
            "installing",
            Some(selection.version.clone()),
            selection.archive_size,
            Some(selection.archive_size),
            Some("Verifying and installing runtime bundle.".into()),
        );

        let archive_path_for_extract = archive_path.clone();
        let staging_dir_for_extract = staging_dir.clone();
        let extraction_context = context.clone();
        let manifest_dir = tokio::task::spawn_blocking(move || {
            extract_runtime_zip(&archive_path_for_extract, &staging_dir_for_extract, &|| {
                extraction_context.check_cancelled()
            })
        })
        .await
        .map_err(|error| format!("Runtime extraction worker failed: {error}"))??;
        let manifest_hash = sha256_file_sync(&manifest_dir.join("runtime-manifest.json"))?;
        if manifest_hash != selection.manifest_sha256 {
            return Err("Runtime manifest SHA-256 does not match the signed descriptor.".into());
        }
        let manifest = validate_manifest_at(&manifest_dir, true)?;
        context.check_cancelled()?;
        if manifest.schema_version != 1 {
            return Err("Signed runtime bundles must use runtime manifest schemaVersion 1.".into());
        }
        validate_runtime_version(&manifest.runtime_version)?;
        if manifest.runtime_version != selection.version {
            return Err(format!(
                "Runtime manifest version {} does not match release asset version {}.",
                manifest.runtime_version, selection.version
            )
            .into());
        }
        write_runtime_auth_contract(
            &manifest_dir,
            &selection.descriptor_bytes,
            &selection.signature_bytes,
        )?;
        write_runtime_install_marker(&manifest_dir, &manifest.runtime_version)?;

        let _mutation_lock = RuntimeMutationLock::acquire(&managed_root)?;
        let final_dir = managed_root.join(&manifest.runtime_version);
        let had_existing = authorize_runtime_replacement(&final_dir, &manifest.runtime_version)?;
        let mut transaction = RuntimeTransaction::new(
            update_id.clone(),
            manifest.runtime_version.clone(),
            had_existing,
        )?;
        let cache_mutation = tokio::select! {
            biased;
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            mutation = VERIFIED_RUNTIME_CACHE.begin_mutation() => mutation,
        };
        context.check_cancelled()?;
        let publication = context.enter_publication(PublicationKind::RuntimeCommit)?;
        runtime_transaction::store(&managed_root, &transaction)?;
        let paths = transaction.paths(&managed_root);
        let mut warnings = Vec::new();
        let promotion =
            promote_runtime_atomically(&managed_root, &mut transaction, &manifest_dir).await?;
        if let Err(pointer_error) =
            write_current_pointer(&managed_root, &manifest.runtime_version).await
        {
            let rollback_error = rollback_runtime_promotion(
                &manifest_dir,
                &paths.final_dir,
                &paths.backup,
                promotion.had_existing,
            )
            .await
            .err();
            if rollback_error.is_none() {
                let _ = runtime_transaction::clear(&managed_root);
            }
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "{pointer_error} Runtime publication rollback also failed: {rollback_error}"
                )
                .into(),
                None => pointer_error.into(),
            });
        }
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::CurrentPointerCommitted);
        let pointer_checkpoint_error =
            runtime_transaction::store(&managed_root, &transaction).err();
        publication.commit();
        if let Some(error) = pointer_checkpoint_error {
            warnings.push(format!(
                "Runtime is active, but its pointer checkpoint could not be persisted: {error}"
            ));
        } else {
            if let Some(warning) = cleanup_runtime_promotion_backup(
                &paths.backup,
                &manifest.runtime_version,
                promotion,
            )
            .await?
            {
                warnings.push(warning);
            }
            transaction.set_checkpoint(RuntimeTransactionCheckpoint::BackupCleaned);
            if let Err(error) = runtime_transaction::store(&managed_root, &transaction) {
                warnings.push(format!(
                    "Runtime is active, but its cleanup checkpoint could not be persisted: {error}"
                ));
            } else if let Err(error) = runtime_transaction::clear(&managed_root) {
                warnings.push(format!(
                    "Runtime is active, but its completed transaction record remains: {error}"
                ));
            }
        }
        if let Err(warning) =
            cleanup_old_runtime_versions(&managed_root, &manifest.runtime_version).await
        {
            warnings.push(warning);
        }
        let refreshed_cache = build_verified_runtime_snapshot_async(
            managed_root.clone(),
            bundled_executable_root(),
            crate::updater::verify_release_signature_for_key,
            CancellationToken::new(),
        )
        .await;
        if let Err(error) = &refreshed_cache {
            warnings.push(format!(
                "Runtime was published, but its verified runtime cache could not be refreshed: {error}"
            ));
        }
        cache_mutation.publish(refreshed_cache);
        Ok((manifest.runtime_version, warnings))
    }
    .await;

    let cleanup_lock = RuntimeMutationLock::acquire(&managed_root);
    let preserve_work_root = match &cleanup_lock {
        Ok(_) => match runtime_transaction::protected_update_ids(&managed_root) {
            Ok(protected) => protected.contains(&update_id),
            Err(error) => {
                eprintln!(
                    "Runtime staging was retained because transaction protection could not be read: {error}"
                );
                true
            }
        },
        Err(error) => {
            eprintln!(
                "Runtime staging was retained because the mutation lock was unavailable: {error}"
            );
            true
        }
    };
    let cleanup_warning = if preserve_work_root {
        None
    } else {
        remove_owned_runtime_update_dir(&work_root).await.err()
    };
    drop(cleanup_lock);
    let (installed_version, mut warnings) = match install_result {
        Ok(result) => result,
        Err(UpdateRunError::Cancelled) => {
            if let Some(cleanup) = &cleanup_warning {
                eprintln!("Runtime update was cancelled; staging cleanup failed: {cleanup}");
            }
            return Err(UpdateRunError::Cancelled);
        }
        Err(UpdateRunError::Failed(error)) => {
            if let Some(cleanup) = &cleanup_warning {
                eprintln!("Runtime update failed; staging cleanup also failed: {cleanup}");
            }
            return Err(UpdateRunError::Failed(error));
        }
    };
    if let Some(cleanup) = cleanup_warning {
        warnings.push(cleanup);
    }

    emit_runtime_progress(
        progress,
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

pub async fn cleanup_abandoned_runtime_updates() -> Result<(), String> {
    let managed_root = managed_runtime_root();
    let _mutation_lock = RuntimeMutationLock::acquire(&managed_root)?;
    let protected = runtime_transaction::protected_update_ids(&managed_root)?;
    let updates_root = managed_root.join(".updates");
    cleanup_abandoned_runtime_updates_at(&updates_root, &protected).await
}

pub async fn recover_runtime_update_transaction() -> Result<(), String> {
    let managed_root = managed_runtime_root();
    recover_runtime_update_transaction_at(&managed_root, &authenticated_owned_runtime).await
}

async fn recover_runtime_update_transaction_at<F>(
    managed_root: &Path,
    validate: &F,
) -> Result<(), String>
where
    F: Fn(&Path, &str) -> bool,
{
    let _mutation_lock = RuntimeMutationLock::acquire(managed_root)?;
    let active = runtime_transaction::load(managed_root)?;
    let protected = runtime_transaction::protected_update_ids(managed_root)?;
    if protected.iter().any(|update_id| {
        active
            .as_ref()
            .is_none_or(|active| update_id != &active.update_id)
    }) {
        return Err(
            "A quarantined runtime transaction requires manual recovery before updates can continue."
                .into(),
        );
    }
    let Some(mut transaction) = active else {
        if !protected.is_empty() {
            return Err(
                "A quarantined runtime transaction requires manual recovery before updates can continue."
                    .into(),
            );
        }
        return Ok(());
    };
    let paths = transaction.paths(managed_root);
    let candidate_valid = runtime_update_work_root_is_owned(&paths.work_root)
        && validate(&paths.candidate, &transaction.runtime_version);
    let final_valid = validate(&paths.final_dir, &transaction.runtime_version);
    let backup_valid = validate(&paths.backup, &transaction.runtime_version);
    let candidate_exists = paths.candidate.exists();
    let final_exists = paths.final_dir.exists();
    let backup_exists = paths.backup.exists();

    let should_finish_candidate = candidate_valid
        && (transaction.checkpoint == RuntimeTransactionCheckpoint::CandidateVerified
            || !final_valid
            || (final_exists && !backup_exists));
    if should_finish_candidate {
        if final_exists {
            if !installed_runtime_is_owned(&paths.final_dir, &transaction.runtime_version)? {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "An unowned directory occupies the runtime destination.",
                );
            }
            if backup_exists {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "Both the runtime destination and backup exist before recovery can stage the old runtime.",
                );
            }
            fs::rename(&paths.final_dir, &paths.backup)
                .await
                .map_err(|error| {
                    format!("Failed to stage the old runtime during recovery: {error}")
                })?;
            transaction.set_checkpoint(RuntimeTransactionCheckpoint::OldMoved);
            runtime_transaction::store(managed_root, &transaction)?;
        } else if transaction.had_existing && !backup_exists {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The previous runtime is missing during transaction recovery.",
            );
        }
        if paths.final_dir.exists() {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The runtime destination reappeared during transaction recovery.",
            );
        }
        fs::rename(&paths.candidate, &paths.final_dir)
            .await
            .map_err(|error| {
                format!("Failed to publish the recovered runtime candidate: {error}")
            })?;
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
        runtime_transaction::store(managed_root, &transaction)?;
        return finish_recovered_runtime(managed_root, transaction, validate).await;
    }

    if final_valid && !candidate_exists {
        if transaction.checkpoint == RuntimeTransactionCheckpoint::CandidateVerified
            && transaction.had_existing
            && !backup_exists
        {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The verified candidate and previous-runtime backup are both missing.",
            );
        }
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
        runtime_transaction::store(managed_root, &transaction)?;
        return finish_recovered_runtime(managed_root, transaction, validate).await;
    }

    if backup_valid {
        if final_exists {
            if !installed_runtime_is_owned(&paths.final_dir, &transaction.runtime_version)? {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "Recovery cannot replace an unowned runtime destination with the authenticated backup.",
                );
            }
            let quarantine = managed_root.join(format!(
                ".runtime-quarantine-{}-{}",
                transaction.runtime_version, transaction.update_id
            ));
            if quarantine.exists() {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "The runtime quarantine destination already exists.",
                );
            }
            fs::rename(&paths.final_dir, &quarantine)
                .await
                .map_err(|error| format!("Failed to quarantine the invalid runtime: {error}"))?;
        }
        fs::rename(&paths.backup, &paths.final_dir)
            .await
            .map_err(|error| {
                format!("Failed to restore the authenticated runtime backup: {error}")
            })?;
        write_current_pointer(managed_root, &transaction.runtime_version).await?;
        runtime_transaction::clear(managed_root)?;
        return Ok(());
    }

    let reason = if candidate_exists || final_exists || backup_exists {
        "Runtime transaction artifacts exist, but none is an authenticated, integrity-valid recovery source."
    } else {
        "All runtime transaction artifacts are missing."
    };
    quarantine_runtime_transaction(managed_root, &transaction, reason)
}

async fn finish_recovered_runtime<F>(
    managed_root: &Path,
    mut transaction: RuntimeTransaction,
    validate: &F,
) -> Result<(), String>
where
    F: Fn(&Path, &str) -> bool,
{
    let paths = transaction.paths(managed_root);
    if !validate(&paths.final_dir, &transaction.runtime_version) {
        return quarantine_runtime_transaction(
            managed_root,
            &transaction,
            "The published runtime failed authenticated integrity validation during recovery.",
        );
    }
    write_current_pointer(managed_root, &transaction.runtime_version).await?;
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::CurrentPointerCommitted);
    runtime_transaction::store(managed_root, &transaction)?;
    if paths.backup.exists() {
        if !installed_runtime_is_owned(&paths.backup, &transaction.runtime_version)? {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "Recovery retained an unowned runtime backup.",
            );
        }
        fs::remove_dir_all(&paths.backup)
            .await
            .map_err(|error| format!("Failed to remove the recovered runtime backup: {error}"))?;
    }
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::BackupCleaned);
    runtime_transaction::store(managed_root, &transaction)?;
    runtime_transaction::clear(managed_root)
}

fn authenticated_owned_runtime(path: &Path, version: &str) -> bool {
    let Ok(true) = installed_runtime_is_owned(path, version) else {
        return false;
    };
    let Ok(manifest) = validate_manifest_at(path, true) else {
        return false;
    };
    manifest.runtime_version == version && validate_runtime_auth_contract(path, &manifest).is_ok()
}

fn runtime_update_work_root_is_owned(path: &Path) -> bool {
    if !path.is_dir() || ensure_no_reparse_components(path).is_err() {
        return false;
    }
    let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
    marker.is_file()
        && is_reparse_or_symlink(&marker).ok() == Some(false)
        && std::fs::read(marker).ok().as_deref() == Some(b"schemaVersion=1\n")
}

fn quarantine_runtime_transaction(
    managed_root: &Path,
    transaction: &RuntimeTransaction,
    reason: &str,
) -> Result<(), String> {
    runtime_transaction::quarantine(managed_root, transaction)?;
    Err(format!(
        "Runtime transaction was quarantined without deleting its artifacts: {reason}"
    ))
}

async fn cleanup_abandoned_runtime_updates_at(
    updates_root: &Path,
    protected: &std::collections::HashSet<String>,
) -> Result<(), String> {
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
        if protected.contains(&name) {
            continue;
        }
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

fn authorize_runtime_replacement(final_dir: &Path, version: &str) -> Result<bool, String> {
    let had_existing = final_dir.exists();
    if had_existing && !installed_runtime_is_owned(final_dir, version)? {
        return Err("Refusing to replace an unowned managed runtime directory.".into());
    }
    Ok(had_existing)
}

#[derive(Clone, Copy)]
struct RuntimePromotion {
    had_existing: bool,
}

async fn promote_runtime_atomically(
    managed_root: &Path,
    transaction: &mut RuntimeTransaction,
    candidate_dir: &Path,
) -> Result<RuntimePromotion, String> {
    let paths = transaction.paths(managed_root);
    if candidate_dir != paths.candidate {
        return Err("Runtime candidate does not match its durable transaction path.".into());
    }
    if transaction.had_existing {
        if fs::try_exists(&paths.backup).await.unwrap_or(false) {
            return Err("Refusing to overwrite an unexpected runtime backup path.".into());
        }
        fs::rename(&paths.final_dir, &paths.backup)
            .await
            .map_err(|error| {
                format!("Failed to stage the existing runtime for replacement: {error}")
            })?;
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::OldMoved);
        if let Err(checkpoint_error) = runtime_transaction::store(managed_root, transaction) {
            let rollback_error = fs::rename(&paths.backup, &paths.final_dir).await.err();
            if rollback_error.is_none() {
                let _ = runtime_transaction::clear(managed_root);
            }
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "Failed to persist the old-runtime checkpoint: {checkpoint_error}. Restoring the previous runtime also failed: {rollback_error}"
                ),
                None => format!(
                    "Failed to persist the old-runtime checkpoint: {checkpoint_error}"
                ),
            });
        }
    }

    if let Err(error) = fs::rename(candidate_dir, &paths.final_dir).await {
        if transaction.had_existing {
            if let Err(rollback_error) = fs::rename(&paths.backup, &paths.final_dir).await {
                return Err(format!(
                    "Failed to publish runtime bundle: {error}. Restoring the previous runtime also failed: {rollback_error}"
                ));
            }
        }
        let _ = runtime_transaction::clear(managed_root);
        return Err(format!("Failed to publish runtime bundle: {error}"));
    }
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
    if let Err(checkpoint_error) = runtime_transaction::store(managed_root, transaction) {
        let rollback_error = rollback_runtime_promotion(
            candidate_dir,
            &paths.final_dir,
            &paths.backup,
            transaction.had_existing,
        )
        .await
        .err();
        if rollback_error.is_none() {
            let _ = runtime_transaction::clear(managed_root);
        }
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "Failed to persist the new-runtime checkpoint: {checkpoint_error}. Runtime rollback also failed: {rollback_error}"
            ),
            None => format!("Failed to persist the new-runtime checkpoint: {checkpoint_error}"),
        });
    }

    Ok(RuntimePromotion {
        had_existing: transaction.had_existing,
    })
}

async fn rollback_runtime_promotion(
    candidate_dir: &Path,
    final_dir: &Path,
    backup_dir: &Path,
    had_existing: bool,
) -> Result<(), String> {
    fs::rename(final_dir, candidate_dir)
        .await
        .map_err(|error| format!("Failed to withdraw the newly published runtime: {error}"))?;
    if had_existing {
        if let Err(error) = fs::rename(backup_dir, final_dir).await {
            let restore_new_error = fs::rename(candidate_dir, final_dir).await.err();
            return Err(match restore_new_error {
                Some(restore_new_error) => format!(
                    "Failed to restore the previous runtime: {error}. Restoring the new runtime also failed: {restore_new_error}"
                ),
                None => format!("Failed to restore the previous runtime: {error}"),
            });
        }
    }
    Ok(())
}

async fn cleanup_runtime_promotion_backup(
    backup_dir: &Path,
    final_version: &str,
    promotion: RuntimePromotion,
) -> Result<Option<String>, String> {
    if !promotion.had_existing {
        return Ok(None);
    }
    let warning = match installed_runtime_is_owned(backup_dir, final_version) {
        Ok(true) => fs::remove_dir_all(backup_dir).await.err().map(|error| {
            format!(
                "Runtime is ready, but cleanup of backup {} failed: {error}",
                backup_dir.display()
            )
        }),
        Ok(false) => Some(format!(
            "Runtime is ready, but backup ownership changed; cleanup of {} was refused.",
            backup_dir.display()
        )),
        Err(error) => Some(format!(
            "Runtime is ready, but backup validation failed and cleanup was refused: {error}"
        )),
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

async fn tool_status(
    spec: ToolSpec,
    cancellation: CancellationToken,
) -> Result<DownloaderToolStatus, String> {
    if cancellation.is_cancelled() {
        return Err("Runtime health check was cancelled during shutdown.".into());
    }
    let resolution = match resolve_tool_lease(spec.name) {
        Ok(Some(resolution)) => resolution,
        Ok(None) => {
            return Ok(DownloaderToolStatus {
                name: spec.name.to_string(),
                required: spec.required,
                available: false,
                version: None,
                path: None,
                source: "missing".into(),
                error: Some("Required runtime tool was not found.".into()),
            });
        }
        Err(error) => {
            return Ok(DownloaderToolStatus {
                name: spec.name.to_string(),
                required: spec.required,
                available: false,
                version: None,
                path: None,
                source: "managed".into(),
                error: Some(error),
            });
        }
    };
    let version = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            return Err("Runtime health check was cancelled during shutdown.".into());
        }
        result = tool_version(spec.name, &resolution.path) => result,
    };
    Ok(match version {
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
    })
}

#[cfg(test)]
fn discover_managed_runtime_at(
    root: &Path,
    verify_hashes: bool,
) -> Result<Option<(PathBuf, RuntimeManifest)>, String> {
    discover_managed_runtime_at_with_verifier(
        root,
        verify_hashes,
        crate::updater::verify_release_signature_for_key,
    )
}

fn discover_managed_runtime_at_with_verifier(
    root: &Path,
    verify_hashes: bool,
    signature_verifier: RuntimeSignatureVerifier,
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
        let manifest = validate_installed_runtime_at_with_verifier(
            &runtime_dir,
            verify_hashes,
            signature_verifier,
        )?;
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

fn bundled_executable_root() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

fn tool_exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

async fn tool_version(name: &str, path: &Path) -> Result<String, String> {
    let output = crate::downloader::process::run_supervised_probe(
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
            let actual = sha256_runtime_tool_file_sync(&path)?;
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

fn validate_installed_runtime_at_with_verifier(
    dir: &Path,
    verify_hashes: bool,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<RuntimeManifest, String> {
    let manifest = validate_manifest_at(dir, verify_hashes)?;
    if installed_runtime_is_owned(dir, &manifest.runtime_version)? {
        validate_runtime_auth_contract_with_verifier(dir, &manifest, signature_verifier)?;
    }
    Ok(manifest)
}

fn validate_runtime_auth_contract(
    dir: &Path,
    manifest: &RuntimeManifest,
) -> Result<SignedRuntimeDescriptor, String> {
    validate_runtime_auth_contract_with_verifier(
        dir,
        manifest,
        crate::updater::verify_release_signature_for_key,
    )
}

fn validate_runtime_auth_contract_with_verifier(
    dir: &Path,
    manifest: &RuntimeManifest,
    signature_verifier: RuntimeSignatureVerifier,
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
    signature_verifier(&descriptor.key_id, &descriptor_bytes, &signature_bytes)?;
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
    if !crate::artifact_contract::is_canonical_sha256(value) {
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
    progress: &RuntimeProgressSink,
    client: &Client,
    selection: &RuntimeAssetSelection,
    archive_path: &Path,
    context: &UpdateTaskContext,
) -> Result<String, UpdateRunError> {
    validate_https_url(&selection.archive_url)?;
    let response = tokio::select! {
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        result = client.get(&selection.archive_url).send() => result,
    }
    .map_err(|error| format!("Failed to download runtime bundle: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Failed to download runtime bundle: HTTP {}.",
            status.as_u16()
        )
        .into());
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

    loop {
        let chunk_result = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = stream.next() => result,
        };
        let Some(chunk_result) = chunk_result else {
            break;
        };
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
            progress,
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
        )
        .into());
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_runtime_zip(
    archive_path: &Path,
    staging_dir: &Path,
    check_cancelled: &dyn Fn() -> Result<(), UpdateRunError>,
) -> Result<PathBuf, UpdateRunError> {
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
        )
        .into());
    }
    let mut expanded_total = 0u64;
    let mut normalized_paths = HashMap::<String, bool>::new();
    let mut manifest_count = 0usize;
    for index in 0..archive.len() {
        check_cancelled()?;
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
            )
            .into());
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
            )
            .into());
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
            )
            .into());
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
            )
            .into());
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
        check_cancelled()?;
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
                check_cancelled()?;
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
                check_cancelled()?;
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
    sha256_file_sync_with_kind(path, false)
}

fn sha256_runtime_tool_file_sync(path: &Path) -> Result<String, String> {
    sha256_file_sync_with_kind(path, true)
}

fn sha256_file_sync_with_kind(path: &Path, is_runtime_tool: bool) -> Result<String, String> {
    #[cfg(not(test))]
    let _ = is_runtime_tool;
    let mut file =
        File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    #[cfg(test)]
    if is_runtime_tool {
        TEST_TOOL_HASH_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    } else {
        TEST_MANIFEST_HASH_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    }
    #[cfg(test)]
    record_scoped_hash_invocation(path, is_runtime_tool);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        #[cfg(test)]
        if is_runtime_tool {
            TEST_TOOL_HASH_BYTES.fetch_add(read as u64, Ordering::SeqCst);
        } else {
            TEST_MANIFEST_HASH_BYTES.fetch_add(read as u64, Ordering::SeqCst);
        }
        #[cfg(test)]
        record_scoped_hash_bytes(path, is_runtime_tool, read as u64);
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
    progress: &RuntimeProgressSink,
    status: &str,
    version: Option<String>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: Option<String>,
) {
    progress(DownloaderRuntimeUpdateProgress {
        status: status.to_string(),
        version,
        downloaded_bytes,
        total_bytes,
        message,
    });
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
