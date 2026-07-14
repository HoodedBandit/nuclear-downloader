use crate::models::{
    DownloaderRuntimeStatus, DownloaderRuntimeUpdateCheck, DownloaderRuntimeUpdateProgress,
    DownloaderToolStatus,
};
use futures_util::{future::join_all, StreamExt};
use reqwest::header::ACCEPT;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use url::Url;
use zip::ZipArchive;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const RUNTIME_UPDATE_PROGRESS_EVENT: &str = "downloader-runtime-update-progress";
const MIN_RECOMMENDED_YTDLP_VERSION: &str = "2026.07.04";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
static RUNTIME_UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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

#[derive(Debug, Clone)]
struct ToolResolution {
    path: PathBuf,
    source: String,
    runtime_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeManifest {
    runtime_version: String,
    platform: String,
    tools: Vec<RuntimeManifestTool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    tag_name: String,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone)]
struct RuntimeAssetSelection {
    version: String,
    archive_name: String,
    archive_url: String,
    archive_size: u64,
    checksum_url: String,
}

pub fn resolve_bin(name: &str) -> PathBuf {
    resolve_tool(name)
        .map(|tool| tool.path)
        .unwrap_or_else(|| missing_tool_path(name))
}

pub fn ytdlp_command_config() -> YtdlpCommandConfig {
    YtdlpCommandConfig {
        ffmpeg_dir: resolve_tool("ffmpeg")
            .and_then(|tool| tool.path.parent().map(Path::to_path_buf)),
        deno_path: resolve_tool("deno").map(|tool| tool.path),
        plugin_dir: app_plugin_dir().exists().then(app_plugin_dir),
    }
}

pub fn diagnostic_summary() -> String {
    REQUIRED_TOOLS
        .iter()
        .map(|tool| {
            resolve_tool(tool.name)
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
            if let Some(resolution) = resolve_tool(spec.name) {
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
    let (state, message) = if missing_required {
        (
            "missing".to_string(),
            Some(format!(
                "Downloader runtime is missing or cannot run required tools: {}.",
                missing_tool_names.join(", ")
            )),
        )
    } else if deno_missing {
        (
            "degraded".to_string(),
            Some("YouTube JavaScript runtime is missing; some public videos may fail.".to_string()),
        )
    } else if stale_ytdlp {
        (
            "degraded".to_string(),
            Some(format!(
                "yt-dlp is older than the recommended baseline {MIN_RECOMMENDED_YTDLP_VERSION}."
            )),
        )
    } else {
        (
            "ready".to_string(),
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
    let update_available = local_status.state != "ready"
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
        RUNTIME_UPDATE_IN_PROGRESS.store(false, Ordering::SeqCst);
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
    let expected_checksum = download_checksum(&client, &selection.checksum_url).await?;
    let archive_name = Path::new(&selection.archive_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name == selection.archive_name)
        .ok_or_else(|| "Runtime release returned an unsafe archive filename.".to_string())?;
    let managed_root = managed_runtime_root();
    fs::create_dir_all(&managed_root)
        .await
        .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
    let update_id = uuid::Uuid::new_v4().to_string();
    let work_root = managed_root.join(".updates").join(&update_id);
    fs::create_dir_all(&work_root)
        .await
        .map_err(|error| format!("Failed to create runtime update staging folder: {error}"))?;
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
        let actual_checksum = download_archive(app, &client, &selection, &archive_path).await?;
        if !actual_checksum.eq_ignore_ascii_case(&expected_checksum) {
            return Err(format!(
                "Runtime bundle checksum mismatch: expected {expected_checksum}, got {actual_checksum}."
            ));
        }

        emit_runtime_progress(
            app,
            "installing",
            Some(selection.version.clone()),
            selection.archive_size,
            Some(selection.archive_size),
            Some("Verifying and installing runtime bundle.".into()),
        );

        let manifest_dir = extract_runtime_zip(&archive_path, &staging_dir)?;
        let manifest = validate_manifest_at(&manifest_dir, true)?;
        validate_runtime_version(&manifest.runtime_version)?;
        if version_sort_key(&manifest.runtime_version) != version_sort_key(&selection.version) {
            return Err(format!(
                "Runtime manifest version {} does not match release asset version {}.",
                manifest.runtime_version, selection.version
            ));
        }

        let final_dir = managed_root.join(&manifest.runtime_version);
        let backup_dir = managed_root.join(format!(
            ".backup-{}-{}",
            manifest.runtime_version, update_id
        ));
        promote_runtime_atomically(&manifest_dir, &final_dir, &backup_dir).await?;
        Ok(manifest.runtime_version)
    }
    .await;

    cleanup_dir_if_exists(&work_root).await;
    let installed_version = install_result?;

    emit_runtime_progress(
        app,
        "complete",
        Some(installed_version),
        selection.archive_size,
        Some(selection.archive_size),
        Some("Downloader runtime is updated.".into()),
    );

    Ok(check_downloader_runtime().await)
}

fn validate_runtime_version(version: &str) -> Result<(), String> {
    let trimmed = version.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        return Err("Runtime manifest contains an unsafe runtime version.".into());
    }
    Ok(())
}

async fn promote_runtime_atomically(
    candidate_dir: &Path,
    final_dir: &Path,
    backup_dir: &Path,
) -> Result<(), String> {
    let had_existing = fs::try_exists(final_dir).await.unwrap_or(false);
    if had_existing {
        cleanup_dir_if_exists(backup_dir).await;
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

    if had_existing {
        cleanup_dir_if_exists(backup_dir).await;
    }
    Ok(())
}

async fn tool_status(spec: ToolSpec) -> DownloaderToolStatus {
    let Some(resolution) = resolve_tool(spec.name) else {
        return DownloaderToolStatus {
            name: spec.name.to_string(),
            required: spec.required,
            available: false,
            version: None,
            path: None,
            source: "missing".into(),
            error: Some("Required runtime tool was not found.".into()),
        };
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

fn resolve_tool(name: &str) -> Option<ToolResolution> {
    if let Some((runtime_dir, manifest)) = discover_managed_runtime() {
        if let Some(tool) = manifest.tools.iter().find(|tool| tool.name == name) {
            let path = runtime_dir.join(&tool.path);
            if path.is_file() {
                return Some(ToolResolution {
                    path,
                    source: "managed".into(),
                    runtime_version: Some(manifest.runtime_version),
                });
            }
        }
    }

    if let Some(path) = bundled_tool_path(name) {
        return Some(ToolResolution {
            path,
            source: "bundled".into(),
            runtime_version: None,
        });
    }

    #[cfg(debug_assertions)]
    {
        Some(ToolResolution {
            path: PathBuf::from(name),
            source: "path".into(),
            runtime_version: None,
        })
    }

    #[cfg(not(debug_assertions))]
    {
        None
    }
}

fn discover_managed_runtime() -> Option<(PathBuf, RuntimeManifest)> {
    let root = managed_runtime_root();
    let mut candidates = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let manifest = validate_manifest_at(&path, false).ok()?;
            Some((path, manifest))
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|(_, left), (_, right)| {
        version_sort_key(&left.runtime_version).cmp(&version_sort_key(&right.runtime_version))
    });
    candidates.pop()
}

fn bundled_tool_path(name: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = exe_dir.join(tool_exe_name(name));
    path.is_file().then_some(path)
}

fn missing_tool_path(name: &str) -> PathBuf {
    local_data_root()
        .join("missing-runtime")
        .join(tool_exe_name(name))
}

fn tool_exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

async fn tool_version(name: &str, path: &Path) -> Result<String, String> {
    let mut cmd = Command::new(path);
    cmd.args(tool_version_args(name));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = tokio::time::timeout(TOOL_PROBE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{} did not respond within 10 seconds", path.display()))?
        .map_err(|error| format!("Failed to run {}: {}", path.display(), error))?;

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
    let manifest_path = dir.join("runtime-manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("Failed to read runtime manifest: {error}"))?;
    let manifest = serde_json::from_str::<RuntimeManifest>(&manifest_text)
        .map_err(|error| format!("Failed to parse runtime manifest: {error}"))?;

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

    for tool in &manifest.tools {
        if tool.version.trim().is_empty() {
            return Err(format!("Runtime tool {} is missing a version.", tool.name));
        }
        validate_relative_manifest_path(&tool.path)?;
        let path = dir.join(&tool.path);
        if !path.is_file() {
            return Err(format!("Runtime tool {} was not found.", tool.name));
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

fn validate_relative_manifest_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err("Runtime manifest contains an unsafe tool path.".into());
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
    if parts.len() < 3
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }

    Some(Version::new(
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

pub fn is_ytdlp_stale(version: &str, minimum: &str) -> bool {
    version_sort_key(version) < version_sort_key(minimum)
}

async fn fetch_latest_runtime_asset() -> Result<Option<RuntimeAssetSelection>, String> {
    let client = build_client()?;
    let release = fetch_latest_release(&client).await?;
    Ok(select_runtime_assets(&release))
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
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read GitHub release response: {error}"))?;

    if !status.is_success() {
        return Err(format!(
            "GitHub runtime update check failed with HTTP {}.",
            status.as_u16()
        ));
    }

    serde_json::from_str::<GitHubRelease>(&body)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {error}"))
}

fn select_runtime_assets(release: &GitHubRelease) -> Option<RuntimeAssetSelection> {
    let archive = release.assets.iter().find(|asset| {
        let lower = asset.name.to_ascii_lowercase();
        lower.ends_with(".zip")
            && lower.contains("runtime")
            && lower.contains("windows")
            && lower.contains("x64")
    })?;
    let checksum = release.assets.iter().find(|asset| {
        let lower = asset.name.to_ascii_lowercase();
        lower == format!("{}.sha256", archive.name.to_ascii_lowercase())
            || (lower.ends_with(".sha256")
                && lower.contains("runtime")
                && lower.contains("windows")
                && lower.contains("x64"))
    })?;

    Some(RuntimeAssetSelection {
        version: runtime_version_from_asset_name(&archive.name)
            .unwrap_or_else(|| release.tag_name.trim_start_matches('v').to_string()),
        archive_name: archive.name.clone(),
        archive_url: archive.browser_download_url.clone(),
        archive_size: archive.size,
        checksum_url: checksum.browser_download_url.clone(),
    })
}

fn runtime_version_from_asset_name(name: &str) -> Option<String> {
    name.split(['-', '_'])
        .find(|part| {
            let pieces = part.split('.').collect::<Vec<_>>();
            pieces.len() == 3
                && pieces
                    .iter()
                    .all(|piece| piece.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
}

async fn download_checksum(client: &Client, url: &str) -> Result<String, String> {
    validate_https_url(url)?;
    let response = client
        .get(url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download runtime checksum: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read runtime checksum: {error}"))?;

    if !status.is_success() {
        return Err(format!(
            "Failed to download runtime checksum: HTTP {}.",
            status.as_u16()
        ));
    }

    parse_sha256_checksum(&body).ok_or_else(|| "Runtime checksum file was invalid.".to_string())
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

    let total_bytes = response.content_length().or(Some(selection.archive_size));
    let mut file = fs::File::create(archive_path)
        .await
        .map_err(|error| format!("Failed to create runtime archive file: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk =
            chunk_result.map_err(|error| format!("Failed while downloading runtime: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Failed to write runtime archive: {error}"))?;
        hasher.update(&chunk);
        downloaded_bytes += chunk.len() as u64;
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

    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_runtime_zip(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    if staging_dir.exists() {
        std::fs::remove_dir_all(staging_dir)
            .map_err(|error| format!("Failed to reset runtime staging folder: {error}"))?;
    }
    std::fs::create_dir_all(staging_dir)
        .map_err(|error| format!("Failed to create runtime staging folder: {error}"))?;

    let file = File::open(archive_path)
        .map_err(|error| format!("Failed to open runtime archive: {error}"))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| format!("Failed to read runtime archive: {error}"))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read runtime archive entry: {error}"))?;
        let Some(enclosed_name) = entry.enclosed_name() else {
            continue;
        };
        let output_path = staging_dir.join(enclosed_name);

        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&output_path)
                .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
        } else {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
            }
            let mut output = File::create(&output_path)
                .map_err(|error| format!("Failed to create runtime file: {error}"))?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|error| format!("Failed to extract runtime file: {error}"))?;
            output
                .flush()
                .map_err(|error| format!("Failed to flush runtime file: {error}"))?;
        }
    }

    find_manifest_dir(staging_dir)
        .ok_or_else(|| "Runtime bundle did not contain runtime-manifest.json.".to_string())
}

fn find_manifest_dir(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join("runtime-manifest.json").is_file() {
            return Some(dir);
        }
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    None
}

fn validate_https_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("Invalid runtime asset URL: {error}"))?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err("Runtime asset URL must use HTTPS.".into())
    }
}

fn parse_sha256_checksum(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|part| part.len() == 64 && part.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|part| part.to_ascii_lowercase())
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

async fn cleanup_dir_if_exists(path: &Path) {
    if fs::try_exists(path).await.unwrap_or(false) {
        let _ = fs::remove_dir_all(path).await;
    }
}

fn emit_runtime_progress(
    app: &AppHandle,
    status: &str,
    version: Option<String>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: Option<String>,
) {
    let _ = app.emit(
        RUNTIME_UPDATE_PROGRESS_EVENT,
        DownloaderRuntimeUpdateProgress {
            status: status.to_string(),
            version,
            downloaded_bytes,
            total_bytes,
            message,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        is_ytdlp_stale, parse_sha256_checksum, promote_runtime_atomically, select_runtime_assets,
        tool_version_args, validate_manifest_at, GitHubRelease, GitHubReleaseAsset, REQUIRED_TOOLS,
    };
    use sha2::{Digest, Sha256};
    use std::fs;

    fn release_with_assets(tag: &str, assets: Vec<&str>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            assets: assets
                .into_iter()
                .map(|name| GitHubReleaseAsset {
                    name: name.into(),
                    browser_download_url: format!("https://example.com/{name}"),
                    size: 100,
                })
                .collect(),
        }
    }

    #[test]
    fn checksum_parser_accepts_hash_sidecar_format() {
        let checksum = "A".repeat(64);
        let expected = checksum.to_ascii_lowercase();
        assert_eq!(
            parse_sha256_checksum(&format!("{checksum}  runtime.zip")),
            Some(expected)
        );
        assert_eq!(parse_sha256_checksum("not-a-checksum"), None);
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
    fn selects_runtime_archive_and_checksum_assets() {
        let release = release_with_assets(
            "v0.5.1",
            vec![
                "Nuclear.Downloader_0.5.1_x64-setup.exe",
                "nuclear-downloader-runtime-2026.06.09-windows-x64.zip",
                "nuclear-downloader-runtime-2026.06.09-windows-x64.zip.sha256",
            ],
        );

        let selected = select_runtime_assets(&release).unwrap();
        assert_eq!(selected.version, "2026.06.09");
        assert!(selected.archive_name.ends_with(".zip"));
        assert!(selected.checksum_url.ends_with(".sha256"));
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
                r#"{{"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
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
                r#"{{"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();

        assert!(validate_manifest_at(&root, true).is_ok());
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
