mod archive;
mod cache;
mod manifest;
mod promotion;
mod release;
mod verified;

#[cfg(test)]
use archive::normalize_runtime_archive_path;
use archive::{download_archive, extract_runtime_zip};
use manifest::{
    app_plugin_dir, bundled_executable_root, managed_runtime_root, sha256_file_sync,
    validate_manifest_at, validate_runtime_version, version_sort_key, write_runtime_auth_contract,
    write_runtime_install_marker, ToolSpec, REQUIRED_TOOLS, RUNTIME_UPDATE_OWNER_MARKER,
};
use promotion::{
    authenticated_owned_runtime, cleanup_abandoned_runtime_updates_at, finalize_update_workspace,
    publish_verified_candidate, recover_runtime_update_transaction_at, RuntimePublicationRequest,
};
use release::{build_client, fetch_latest_runtime_asset};

#[cfg(test)]
use manifest::{
    discover_managed_runtime_at, discover_managed_runtime_at_with_verifier,
    ensure_no_reparse_components, installed_runtime_is_owned, tool_exe_name,
    validate_relative_manifest_path, RuntimeCurrentPointer, RuntimeManifest,
    RUNTIME_AUTH_DESCRIPTOR, RUNTIME_AUTH_SIGNATURE, RUNTIME_CURRENT_POINTER,
};
#[cfg(test)]
use promotion::{
    authorize_runtime_replacement, promote_runtime_atomically,
    publish_verified_candidate_with_cache, rollback_runtime_promotion,
};
#[cfg(test)]
use release::{
    parse_runtime_descriptor, read_response_limited, select_runtime_archive,
    select_runtime_descriptor_assets, GitHubRelease, GitHubReleaseAsset, SignedRuntimeDescriptor,
};

use crate::lifecycle::{UpdateRunError, UpdateTaskContext};
use crate::models::{
    DownloaderRuntimeState, DownloaderRuntimeStatus, DownloaderRuntimeUpdateCheck,
    DownloaderRuntimeUpdateProgress, DownloaderToolStatus,
};
use crate::notifications::RuntimeProgressSink;
use crate::runtime_transaction::{self, RuntimeMutationLock};
#[cfg(test)]
use crate::runtime_transaction::{RuntimeTransaction, RuntimeTransactionCheckpoint};
use futures_util::future::join_all;
#[cfg(test)]
use reqwest::Client;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::LazyLock;
use std::time::Duration;
use tokio::fs;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Mutex;

const MIN_RECOMMENDED_YTDLP_VERSION: &str = "2026.08.19";
// First launch can require Windows Defender to inspect the large, freshly
// unpacked yt-dlp and FFmpeg executables. Keep the probes bounded, but allow
// enough time for that cold-start scan instead of reporting a false repair.
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
#[cfg(test)]
use cache::RuntimeCache;
#[cfg(test)]
use verified::{
    build_verified_bundled_snapshot_from_lock_at, build_verified_runtime_snapshot_async,
    initialize_test_runtime_cache_at, open_verified_tool_file, resolve_tool_lease_from_cache,
    resolve_tool_lease_uncached_at, validate_bundled_sidecar_lock, BundledSidecarLock,
    VerifiedRuntimeSnapshot, VerifiedRuntimeTool, BUNDLED_SIDECAR_LOCK,
};
use verified::{initialize_runtime_cache_at, VERIFIED_RUNTIME_CACHE};
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

#[derive(Debug, Clone)]
pub struct YtdlpCommandConfig {
    pub ffmpeg_dir: Option<PathBuf>,
    pub deno_path: Option<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
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

        let publication = publish_verified_candidate(
            RuntimePublicationRequest {
                managed_root: managed_root.clone(),
                candidate_dir: manifest_dir,
                update_id: update_id.clone(),
                runtime_version: manifest.runtime_version,
                bundled_root: bundled_executable_root(),
                signature_verifier: crate::updater::verify_release_signature_for_key,
            },
            context,
        )
        .await?;
        Ok((publication.installed_version, publication.warnings))
    }
    .await;

    let cleanup_warning = finalize_update_workspace(&managed_root, &work_root, &update_id).await;
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

pub fn is_ytdlp_stale(version: &str, minimum: &str) -> bool {
    version_sort_key(version) < version_sort_key(minimum)
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
