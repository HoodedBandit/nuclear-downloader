use crate::app_error::AppError;
use crate::models::{
    CookieConfig, DownloadProgress, DownloadRequest, PlaylistEntry, PlaylistInfo, UrlInspection,
    VideoInfo,
};
use crate::runtime::{self, YtdlpCommandConfig};
use regex::Regex;
use serde::Deserialize;
use serde_json::Deserializer;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

const MAX_STDERR_LINES: usize = 256;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_PROCESS_LINE_BYTES: usize = 64 * 1024;
const MAX_INSPECTION_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PLAYLIST_ENTRIES: usize = 1_000;
const MAX_CUSTOM_FILENAME_UTF16_UNITS: usize = 180;
const MAX_OUTPUT_SUFFIX: usize = 9_999;
const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
const AUDIO_FORMATS: &[&str] = &["mp3", "flac", "wav", "aac", "opus"];
const COOKIE_BROWSERS: &[&str] = &["firefox", "chrome", "edge", "brave", "opera", "chromium"];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x00000004;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

static DOWNLOAD_PROGRESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\]\s+([\d.]+)%\s+of").unwrap());
static DOWNLOAD_SPEED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"at\s+([\d.]+\w+/s)").unwrap());
static DOWNLOAD_ETA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ETA\s+(\S+)").unwrap());
static DOWNLOAD_DEST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\] Destination:\s+(.+)").unwrap());
static DOWNLOAD_FINAL_DEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\[(?:Merger|VideoConvertor|VideoRemuxer|ExtractAudio)\].*(?:into|Destination:)\s+"?(.+?)"?\s*$"#,
    )
    .unwrap()
});
static DOWNLOAD_MERGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[Merger\]|\[VideoConvertor\]|\[VideoRemuxer\]|\[ExtractAudio\]|post-?process|converting|remuxing").unwrap()
});
static QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{3,4}p$").unwrap());
const STAGING_ROOT_NAME: &str = ".nuclear-downloader-staging";
const STAGING_MARKER_NAME: &str = ".nuclear-downloader-owner-v1.json";
const STAGING_CLEANUP_PREFIX: &str = ".cleanup-";

struct TailBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

#[derive(Debug, Deserialize)]
struct PlaylistThumbnailRecord {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistLineRecord {
    id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    url: Option<String>,
    webpage_url: Option<String>,
    original_url: Option<String>,
    extractor_key: Option<String>,
    ie_key: Option<String>,
    thumbnail: Option<String>,
    thumbnails: Option<Vec<PlaylistThumbnailRecord>>,
    playlist_title: Option<String>,
    playlist: Option<String>,
    playlist_uploader: Option<String>,
    channel: Option<String>,
}

impl PlaylistLineRecord {
    fn playlist_title_hint(&self) -> Option<&str> {
        self.playlist_title.as_deref().or(self.playlist.as_deref())
    }

    fn playlist_channel_hint(&self) -> Option<&str> {
        self.playlist_uploader
            .as_deref()
            .or(self.channel.as_deref())
    }

    fn preferred_thumbnail_url(&self) -> Option<&str> {
        self.thumbnails
            .as_ref()
            .and_then(|thumbnails| {
                thumbnails
                    .iter()
                    .rev()
                    .find_map(|thumbnail| thumbnail.url.as_deref())
            })
            .or(self.thumbnail.as_deref())
    }

    fn into_playlist_entry(self) -> Option<PlaylistEntry> {
        let thumbnail = sanitize_thumbnail_url(self.preferred_thumbnail_url());
        let PlaylistLineRecord {
            id,
            title,
            duration,
            url,
            webpage_url,
            original_url,
            extractor_key,
            ie_key,
            ..
        } = self;
        let normalized_id = id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let is_youtube = extractor_key
            .as_deref()
            .or(ie_key.as_deref())
            .map(|key| key.to_ascii_lowercase().contains("youtube"))
            .unwrap_or(false);
        let video_url = webpage_url
            .filter(|value| is_allowed_download_url(value))
            .or_else(|| original_url.filter(|value| is_allowed_download_url(value)))
            .or_else(|| url.filter(|value| is_allowed_download_url(value)))
            .or_else(|| {
                (is_youtube && normalized_id.is_some()).then(|| {
                    format!(
                        "https://www.youtube.com/watch?v={}",
                        normalized_id.as_deref().unwrap_or_default()
                    )
                })
            })?;
        let id = normalized_id.unwrap_or_else(|| video_url.clone());

        Some(PlaylistEntry {
            id,
            title,
            duration,
            url: video_url,
            thumbnail,
        })
    }
}

impl TailBuffer {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);

        while self.lines.len() > MAX_STDERR_LINES || self.bytes > MAX_STDERR_BYTES {
            if let Some(removed) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                break;
            }
        }
    }

    fn into_string(self) -> String {
        self.lines.into_iter().collect::<Vec<_>>().join("\n")
    }
}

#[derive(Default)]
struct ProgressFields {
    speed: Option<String>,
    eta: Option<String>,
    error: Option<String>,
    error_code: Option<String>,
    error_detail: Option<String>,
    filename: Option<String>,
    phase: Option<&'static str>,
    download_progress: Option<f64>,
    conversion_progress: Option<f64>,
}

#[derive(Debug, Clone)]
struct DownloadErrorInfo {
    code: String,
    message: String,
    detail: String,
}

fn emit_progress(
    app: &AppHandle,
    download_id: &str,
    status: &str,
    progress: f64,
    fields: ProgressFields,
) {
    let event = DownloadProgress {
        download_id: download_id.to_string(),
        status: status.to_string(),
        progress,
        phase: fields.phase.map(str::to_string),
        download_progress: fields.download_progress,
        conversion_progress: fields.conversion_progress,
        speed: fields.speed,
        eta: fields.eta,
        error: fields.error,
        error_code: fields.error_code,
        error_detail: fields.error_detail,
        filename: fields.filename,
    };
    crate::record_download_progress(app, &event);
    if let Err(error) = app.emit("download-progress", &event) {
        crate::record_event_delivery_failure(app, "download-progress", &error.to_string());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DownloadManagerMode {
    Accepting,
    Paused,
    Draining,
    ShuttingDown,
}

struct DownloadManagerState {
    mode: DownloadManagerMode,
    jobs: HashMap<String, DownloadJob>,
}

struct DownloadManagerInner {
    state: Mutex<DownloadManagerState>,
    idle: Notify,
    download_slots: Arc<Semaphore>,
    inspection_slots: Arc<Semaphore>,
    conversion_slots: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadManagerInner>,
}

#[derive(Clone)]
pub struct DownloadJob {
    supervisor: Arc<ProcessSupervisor>,
}

struct ProcessSupervisor {
    cancellation: CancellationToken,
    runtime_tools: std::sync::Mutex<HashMap<String, runtime::RuntimeToolLease>>,
    #[cfg(windows)]
    process_job: Arc<WindowsProcessJob>,
}

pub struct MaintenanceLease {
    manager: DownloadManager,
    active: bool,
}

impl MaintenanceLease {
    pub async fn release(mut self) {
        self.manager.end_maintenance().await;
        self.active = false;
    }
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let manager = self.manager.clone();
        tauri::async_runtime::spawn(async move {
            manager.end_maintenance().await;
        });
    }
}

#[cfg(windows)]
struct WindowsProcessJob {
    handle: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsProcessJob {}
#[cfg(windows)]
unsafe impl Sync for WindowsProcessJob {}

#[cfg(windows)]
impl WindowsProcessJob {
    fn new() -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "Failed to create Windows process job: {}",
                std::io::Error::last_os_error()
            ));
        }

        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &information as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };

        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(format!("Failed to configure Windows process job: {error}"));
        }

        Ok(Self { handle })
    }

    fn assign(&self, child: &tokio::process::Child) -> Result<(), String> {
        let process_handle = child
            .raw_handle()
            .ok_or_else(|| "Downloader process did not expose a Windows handle.".to_string())?;
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process_handle as HANDLE) };
        if assigned == 0 {
            Err(format!(
                "Failed to attach downloader process to its Windows job: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    fn resume(&self, child: &tokio::process::Child) -> Result<(), String> {
        let process_id = child
            .id()
            .ok_or_else(|| "Downloader process did not expose a process ID.".to_string())?;
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Failed to enumerate the suspended downloader process: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut found = false;
        let mut next = unsafe { Thread32First(snapshot, &mut entry) };
        while next != 0 {
            if entry.th32OwnerProcessID == process_id {
                found = true;
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    unsafe { CloseHandle(snapshot) };
                    return Err(format!(
                        "Failed to open the suspended downloader thread: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                let previous_count = unsafe { ResumeThread(thread) };
                unsafe { CloseHandle(thread) };
                if previous_count == u32::MAX {
                    unsafe { CloseHandle(snapshot) };
                    return Err(format!(
                        "Failed to resume the supervised downloader process: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            next = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        if found {
            Ok(())
        } else {
            Err("The suspended downloader process exposed no resumable thread.".into())
        }
    }

    fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

impl DownloadJob {
    fn new() -> Result<Self, String> {
        Ok(Self {
            supervisor: Arc::new(ProcessSupervisor {
                cancellation: CancellationToken::new(),
                runtime_tools: std::sync::Mutex::new(HashMap::new()),
                #[cfg(windows)]
                process_job: Arc::new(WindowsProcessJob::new()?),
            }),
        })
    }

    pub fn is_cancelled(&self) -> bool {
        self.supervisor.cancellation.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.supervisor.cancellation.cancelled().await;
    }

    pub(crate) fn cancel(&self) {
        self.supervisor.cancellation.cancel();
        #[cfg(windows)]
        self.supervisor.process_job.terminate();
    }

    fn runtime_tool(&self, name: &str, required: bool) -> Result<Option<PathBuf>, String> {
        let mut leases = self
            .supervisor
            .runtime_tools
            .lock()
            .map_err(|_| "Runtime executable lease registry was poisoned.".to_string())?;
        if let Some(lease) = leases.get(name) {
            return Ok(Some(lease.path().to_path_buf()));
        }
        match runtime::resolve_tool_lease(name)? {
            Some(lease) => {
                let path = lease.path().to_path_buf();
                leases.insert(name.to_string(), lease);
                Ok(Some(path))
            }
            None if required => Err(format!("Required runtime executable {name} was not found.")),
            None => Ok(None),
        }
    }

    fn required_runtime_tool(&self, name: &str) -> Result<PathBuf, String> {
        self.runtime_tool(name, true)?
            .ok_or_else(|| format!("Required runtime executable {name} was not found."))
    }

    fn ytdlp_runtime_config(&self) -> Result<YtdlpCommandConfig, String> {
        let ffmpeg = self.runtime_tool("ffmpeg", true)?;
        let deno = self.runtime_tool("deno", false)?;
        Ok(YtdlpCommandConfig {
            ffmpeg_dir: ffmpeg.and_then(|path| path.parent().map(Path::to_path_buf)),
            deno_path: deno,
            plugin_dir: runtime::plugin_dir(),
        })
    }

    fn attach_process(&self, child: &tokio::process::Child) -> Result<bool, String> {
        #[cfg(windows)]
        {
            self.supervisor.process_job.assign(child)?;
            if self.is_cancelled() {
                return Ok(false);
            }
            self.supervisor.process_job.resume(child)?;
        }
        Ok(!self.is_cancelled())
    }

    async fn spawn(
        &self,
        command: &mut Command,
        process_name: &str,
        below_normal_priority: bool,
    ) -> Result<tokio::process::Child, String> {
        #[cfg(windows)]
        command.creation_flags(supervised_process_flags(below_normal_priority));
        #[cfg(not(windows))]
        let _ = below_normal_priority;
        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to start {process_name}: {error}"))?;
        match self.attach_process(&child) {
            Ok(true) => Ok(child),
            Ok(false) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(format!("{process_name} operation was cancelled."))
            }
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(error)
            }
        }
    }
}

impl DownloadManager {
    pub fn new(max_downloads: usize, max_inspections: usize, max_conversions: usize) -> Self {
        Self {
            inner: Arc::new(DownloadManagerInner {
                state: Mutex::new(DownloadManagerState {
                    mode: DownloadManagerMode::Accepting,
                    jobs: HashMap::new(),
                }),
                idle: Notify::new(),
                download_slots: Arc::new(Semaphore::new(max_downloads.max(1))),
                inspection_slots: Arc::new(Semaphore::new(max_inspections.max(1))),
                conversion_slots: Arc::new(Semaphore::new(max_conversions.max(1))),
            }),
        }
    }

    pub async fn register(&self, download_id: &str) -> Result<DownloadJob, String> {
        let job = DownloadJob::new()?;
        let mut state = self.inner.state.lock().await;
        if state.mode != DownloadManagerMode::Accepting {
            return Err("Downloads are temporarily paused for shutdown or maintenance.".into());
        }
        if state.jobs.contains_key(download_id) {
            return Err("A download with this ID is already active.".into());
        }
        if state.jobs.len() >= MAX_PLAYLIST_ENTRIES {
            return Err("The operation queue is limited to 1,000 items.".into());
        }
        state.jobs.insert(download_id.to_string(), job.clone());
        Ok(job)
    }

    pub async fn finish(&self, download_id: &str) {
        let removed = self
            .inner
            .state
            .lock()
            .await
            .jobs
            .remove(download_id)
            .is_some();
        if removed {
            self.inner.idle.notify_waiters();
        }
    }

    pub async fn cancel(&self, download_id: &str) -> Result<(), AppError> {
        let job = self.inner.state.lock().await.jobs.get(download_id).cloned();
        if let Some(job) = job {
            job.cancel();
            Ok(())
        } else {
            Err(AppError::not_found("operation"))
        }
    }

    pub async fn active_ids(&self) -> Vec<String> {
        self.inner.state.lock().await.jobs.keys().cloned().collect()
    }

    pub async fn active_count(&self) -> usize {
        self.inner.state.lock().await.jobs.len()
    }

    pub async fn acquire_maintenance(&self) -> Result<MaintenanceLease, AppError> {
        let mut state = self.inner.state.lock().await;
        if state.mode != DownloadManagerMode::Accepting {
            return Err(AppError::busy(
                "Downloader maintenance is already in progress.",
            ));
        }
        if !state.jobs.is_empty() {
            return Err(AppError::busy(
                "Finish or cancel active operations before installing updates.",
            ));
        }
        state.mode = DownloadManagerMode::Paused;
        Ok(MaintenanceLease {
            manager: self.clone(),
            active: true,
        })
    }

    pub async fn end_maintenance(&self) {
        let mut state = self.inner.state.lock().await;
        if state.mode == DownloadManagerMode::Paused {
            state.mode = DownloadManagerMode::Accepting;
        }
    }

    pub async fn begin_cancel_all(&self) -> Result<(), AppError> {
        let jobs = {
            let mut state = self.inner.state.lock().await;
            if state.mode == DownloadManagerMode::ShuttingDown {
                return Err(AppError::busy("The application is shutting down."));
            }
            if state.mode == DownloadManagerMode::Paused {
                return Err(AppError::busy(
                    "Cancellation cannot start while update maintenance is active.",
                ));
            }
            state.mode = DownloadManagerMode::Draining;
            state.jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in jobs {
            job.cancel();
        }
        Ok(())
    }

    pub async fn finish_cancel_all(&self, timeout: Duration) -> Result<(), String> {
        let result = self.wait_for_idle(timeout).await;
        if result.is_ok() {
            let mut state = self.inner.state.lock().await;
            if state.mode == DownloadManagerMode::Draining {
                state.mode = DownloadManagerMode::Accepting;
            }
        }
        result
    }

    pub async fn begin_shutdown(&self) {
        let jobs = {
            let mut state = self.inner.state.lock().await;
            state.mode = DownloadManagerMode::ShuttingDown;
            state.jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in jobs {
            job.cancel();
        }
    }

    pub async fn wait_for_idle(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.inner.idle.notified();
            if self.active_count().await == 0 {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || tokio::time::timeout(remaining, notified).await.is_err() {
                return Err("Timed out while waiting for downloader processes to exit.".into());
            }
        }
    }

    async fn acquire_conversion(
        &self,
        job: &DownloadJob,
    ) -> Result<Option<OwnedSemaphorePermit>, String> {
        tokio::select! {
            permit = self.inner.conversion_slots.clone().acquire_owned() => {
                permit.map(Some).map_err(|_| "WebM conversion scheduler is unavailable.".to_string())
            }
            _ = job.cancelled() => Ok(None),
        }
    }

    pub async fn acquire_download_slot(&self) -> Result<OwnedSemaphorePermit, String> {
        self.inner
            .download_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Download scheduler is unavailable.".to_string())
    }

    pub async fn acquire_inspection(
        &self,
        job: &DownloadJob,
    ) -> Result<Option<OwnedSemaphorePermit>, String> {
        tokio::select! {
            permit = self.inner.inspection_slots.clone().acquire_owned() => {
                permit.map(Some).map_err(|_| "Inspection scheduler is unavailable.".to_string())
            }
            _ = job.cancelled() => Ok(None),
        }
    }
}

pub fn create_download_manager() -> DownloadManager {
    DownloadManager::new(5, 1, 1)
}

pub fn is_allowed_download_url(raw: &str) -> bool {
    Url::parse(raw)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

pub fn validate_fetch_request(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
) -> Result<(), String> {
    if !is_allowed_download_url(url) {
        return Err("Only http:// and https:// URLs are allowed.".into());
    }

    if let Some(config) = cookie_config {
        validate_cookie_config(config)?;
    }

    validate_compat_config_path(compat_config_path)?;

    Ok(())
}

pub fn validate_download_request(request: &DownloadRequest) -> Result<(), String> {
    validate_fetch_request(
        &request.url,
        request.cookie_config.as_ref(),
        request.compat_config_path.as_deref(),
    )?;

    if !is_allowed_format(&request.format) {
        return Err("Unsupported output format.".into());
    }

    if !is_allowed_quality(&request.quality) {
        return Err("Unsupported quality selection.".into());
    }

    if request.output_dir.trim().is_empty() {
        return Err("Output folder is not set.".into());
    }

    if request
        .filename_override
        .as_deref()
        .is_some_and(|value| normalize_filename_override(value).is_none())
    {
        return Err("Custom filename must contain at least one valid character.".into());
    }

    Ok(())
}

pub fn validate_output_directory(path: &str) -> Result<String, AppError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid("Output folder is not set."));
    }
    let path = PathBuf::from(trimmed);
    std::fs::create_dir_all(&path).map_err(|error| {
        AppError::new(
            "output_directory_unavailable",
            "The selected output folder could not be created.",
        )
        .with_detail(error.kind().to_string())
    })?;
    let input_metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        AppError::new(
            "output_directory_unavailable",
            "The selected output folder could not be inspected.",
        )
        .with_detail(error.kind().to_string())
    })?;
    if !input_metadata.is_dir() || input_metadata.file_type().is_symlink() {
        return Err(AppError::new(
            "output_directory_unsafe",
            "The selected output folder must be a regular local directory.",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if input_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::new(
                "output_directory_unsafe",
                "The selected output folder cannot be a reparse point.",
            ));
        }
    }
    let canonical = path.canonicalize().map_err(|error| {
        AppError::new(
            "output_directory_unavailable",
            "The selected output folder could not be resolved.",
        )
        .with_detail(error.kind().to_string())
    })?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        AppError::new(
            "output_directory_unavailable",
            "The selected output folder could not be inspected.",
        )
        .with_detail(error.kind().to_string())
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::new(
            "output_directory_unsafe",
            "The selected output folder must be a regular local directory.",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::new(
                "output_directory_unsafe",
                "The selected output folder cannot be a reparse point.",
            ));
        }
    }

    let probe = canonical.join(format!(".nuclear-write-probe-{}", uuid::Uuid::new_v4()));
    let mut probe_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| {
            AppError::new(
                "output_directory_read_only",
                "The selected output folder is not writable.",
            )
            .with_detail(error.kind().to_string())
        })?;
    let probe_result = probe_file.write_all(b"nuclear-downloader-write-probe");
    let sync_result = probe_file.sync_all();
    drop(probe_file);
    let _ = std::fs::remove_file(&probe);
    probe_result.and(sync_result).map_err(|error| {
        AppError::new(
            "output_directory_read_only",
            "The selected output folder is not writable.",
        )
        .with_detail(error.kind().to_string())
    })?;

    #[cfg(windows)]
    if free_space_bytes(&canonical)? == 0 {
        return Err(AppError::new(
            "output_directory_full",
            "The selected output volume has no available space.",
        ));
    }

    Ok(canonical.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn free_space_bytes(path: &Path) -> Result<u64, AppError> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut available = 0_u64;
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(AppError::new(
            "output_directory_unavailable",
            "Available output disk space could not be determined.",
        ))
    } else {
        Ok(available)
    }
}

pub async fn run_supervised_probe(
    binary: &Path,
    arguments: &[&str],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<std::process::Output, String> {
    let job = DownloadJob::new()?;
    let mut command = Command::new(binary);
    command
        .kill_on_drop(true)
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job.spawn(&mut command, "runtime probe", false).await?;
    wait_with_bounded_output(
        child,
        &job,
        stdout_limit.min(MAX_PROCESS_LINE_BYTES),
        stderr_limit.min(MAX_PROCESS_LINE_BYTES),
        timeout,
    )
    .await
}

fn validate_cookie_config(config: &CookieConfig) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }

    match config.mode.as_str() {
        "browser" => {
            if COOKIE_BROWSERS.contains(&config.browser.as_str()) {
                Ok(())
            } else {
                Err("Unsupported browser for cookie import.".into())
            }
        }
        "file" => {
            if let Some(path) = config
                .cookie_file
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                if Path::new(path).is_file() {
                    Ok(())
                } else {
                    Err("Cookie file was not found.".into())
                }
            } else {
                Err("Cookie file mode requires a cookies.txt path.".into())
            }
        }
        _ => Err("Unsupported cookie mode.".into()),
    }
}

fn validate_compat_config_path(path: Option<&str>) -> Result<(), String> {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(());
    };

    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err("Compatibility config file was not found.".into())
    }
}

fn append_ytdlp_runtime_args(
    args: &mut Vec<String>,
    runtime_config: &YtdlpCommandConfig,
    compat_config_path: Option<&str>,
) {
    args.push("--ignore-config".to_string());

    if let Some(path) = compat_config_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        args.push("--config-locations".to_string());
        args.push(path.to_string());
    }

    args.push("--no-plugin-dirs".to_string());
    if let Some(plugin_dir) = runtime_config.plugin_dir.as_ref() {
        args.push("--plugin-dirs".to_string());
        args.push(plugin_dir.to_string_lossy().to_string());
    }

    args.push("--no-js-runtimes".to_string());
    if let Some(deno_path) = runtime_config.deno_path.as_ref() {
        args.push("--js-runtimes".to_string());
        args.push(format!("deno:{}", deno_path.to_string_lossy()));
    }

    if let Some(ffmpeg_dir) = runtime_config.ffmpeg_dir.as_ref() {
        args.push("--ffmpeg-location".to_string());
        args.push(ffmpeg_dir.to_string_lossy().to_string());
    }
}

fn append_cookie_args(args: &mut Vec<String>, config: &CookieConfig) {
    if !config.enabled {
        return;
    }

    match config.mode.as_str() {
        "file" => {
            if let Some(path) = config.cookie_file.as_deref() {
                args.push("--cookies".to_string());
                args.push(path.to_string());
            }
        }
        "browser" => {
            args.push("--cookies-from-browser".to_string());
            args.push(config.browser.clone());
        }
        _ => {}
    }
}

fn configure_cookie_args(cmd: &mut Command, cookie_config: Option<&CookieConfig>) {
    if let Some(config) = cookie_config {
        let mut args = Vec::new();
        append_cookie_args(&mut args, config);
        cmd.args(args);
    }
}

fn is_allowed_format(format: &str) -> bool {
    VIDEO_FORMATS.contains(&format) || AUDIO_FORMATS.contains(&format)
}

fn is_allowed_quality(quality: &str) -> bool {
    quality == "best" || QUALITY_RE.is_match(quality)
}

#[cfg(windows)]
fn supervised_process_flags(below_normal_priority: bool) -> u32 {
    CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | if below_normal_priority {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            0
        }
}

fn is_x_or_twitter_url(raw: &str) -> bool {
    Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .map(|host| {
            host == "x.com"
                || host.ends_with(".x.com")
                || host == "twitter.com"
                || host.ends_with(".twitter.com")
        })
        .unwrap_or(false)
}

fn is_twitter_api_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("guest token")
        || lower.contains("bad guest token")
        || lower.contains("failed to query api")
        || (lower.contains("[twitter]") && lower.contains("unauthorized"))
}

fn should_retry_with_twitter_syndication(url: &str, message: &str) -> bool {
    is_x_or_twitter_url(url)
        && (is_twitter_api_auth_error(message) || is_twitter_missing_video_error(message))
}

fn append_twitter_syndication_args(args: &mut Vec<String>, url: &str, enabled: bool) {
    if enabled && is_x_or_twitter_url(url) {
        args.push("--extractor-args".to_string());
        args.push("twitter:api=syndication".to_string());
    }
}

fn sanitize_thumbnail_url(raw: Option<&str>) -> Option<String> {
    raw.and_then(|value| {
        Url::parse(value)
            .ok()
            .filter(|url| url.scheme() == "https")
            .map(|_| value.to_string())
    })
}

fn is_twitter_missing_video_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no video")
        || lower.contains("does not contain a video")
        || lower.contains("no video could be found")
        || lower.contains("no media")
        || lower.contains("requested format is not available")
}

fn is_non_actionable_error_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed
            .to_ascii_lowercase()
            .contains("drm protected stream detected, decoding will likely fail")
}

fn build_error_message(stderr_output: &str, exit_code: Option<i32>) -> String {
    if stderr_output.is_empty() {
        return format!("yt-dlp exited with code {}", exit_code.unwrap_or(-1));
    }

    let lines = stderr_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    let summary_lines = {
        let actionable_lines = lines
            .iter()
            .copied()
            .filter(|line| !is_non_actionable_error_line(line))
            .collect::<Vec<_>>();

        if actionable_lines.is_empty() {
            lines
        } else {
            actionable_lines
        }
    };

    summary_lines
        .into_iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

fn simple_error(code: &str, message: impl Into<String>) -> DownloadErrorInfo {
    let message = message.into();
    DownloadErrorInfo {
        code: code.to_string(),
        message: message.clone(),
        detail: message,
    }
}

fn classify_process_error(
    stderr_output: &str,
    exit_code: Option<i32>,
    format_hint: Option<&str>,
) -> DownloadErrorInfo {
    let summary = build_error_message(stderr_output, exit_code);
    let lower = stderr_output.to_ascii_lowercase();
    let format_hint = format_hint.unwrap_or_default();

    let (code, message) = if lower.contains("no supported javascript runtime")
        || lower.contains("js runtime")
        || lower.contains("ejs")
    {
        (
            "youtube_missing_js_runtime",
            "YouTube extraction needs the bundled JavaScript runtime. Update the downloader runtime and retry.".to_string(),
        )
    } else if lower.contains("po token")
        || lower.contains("potoken")
        || lower.contains("proof of origin")
        || lower.contains("confirm you")
        || lower.contains("not a bot")
        || lower.contains("bot verification")
    {
        (
            "youtube_bot_verification",
            "YouTube asked for bot or PO-token verification for this public video. Update the downloader runtime first; if it still fails, use the advanced compatibility config/plugin hook for that network.".to_string(),
        )
    } else if lower.contains("login required")
        || lower.contains("authentication required")
        || lower.contains("private video")
        || lower.contains("members-only")
        || lower.contains("age-restricted")
        || lower.contains("sign in to confirm your age")
    {
        (
            "login_required",
            "This video requires an account that can access it. Enable cookies or provide a cookies.txt file, then retry.".to_string(),
        )
    } else if lower.contains("could not copy") && lower.contains("cookie")
        || lower.contains("cookie database")
        || lower.contains("cookies-from-browser")
        || lower.contains("decrypt") && lower.contains("cookie")
        || lower.contains("cookie") && lower.contains("locked")
        || lower.contains("cookie") && lower.contains("expired")
    {
        (
            "cookie_failure",
            "The selected cookies could not be used. Refresh the cookies.txt file or close the browser before importing cookies.".to_string(),
        )
    } else if lower.contains("requested format is not available")
        || lower.contains("no video formats found")
        || lower.contains("no compatible formats")
    {
        if format_hint == "mp4" {
            (
                "format_unavailable",
                "No compatible MP4 stream is available for this video. Choose MKV for best quality or WebM conversion and retry.".to_string(),
            )
        } else {
            (
                "format_unavailable",
                "The requested format is not available for this video. Choose another format or quality and retry.".to_string(),
            )
        }
    } else if lower.contains("not available in your country")
        || lower.contains("geo")
        || lower.contains("region")
    {
        (
            "region_unavailable",
            "This video is not available from the current region or network.".to_string(),
        )
    } else if lower.contains("video unavailable")
        || lower.contains("this video is unavailable")
        || lower.contains("removed")
        || lower.contains("unsupported url")
    {
        (
            "unavailable",
            "This URL is unavailable or unsupported by the downloader runtime.".to_string(),
        )
    } else if lower.contains("ffmpeg")
        || lower.contains("ffprobe")
        || lower.contains("post-process")
        || lower.contains("postprocess")
    {
        (
            "postprocess_failed",
            "The media downloaded but post-processing failed. Update the downloader runtime and retry.".to_string(),
        )
    } else {
        ("download_failed", summary.clone())
    };

    let detail = format!(
        "{}\n\nRuntime: {}",
        if stderr_output.trim().is_empty() {
            summary
        } else {
            stderr_output.trim().to_string()
        },
        runtime::diagnostic_summary()
    );

    DownloadErrorInfo {
        code: code.to_string(),
        message,
        detail,
    }
}

fn error_for_fetch(stderr_output: &str, exit_code: Option<i32>) -> String {
    let error = classify_process_error(stderr_output, exit_code, None);
    error.message
}

fn emit_error_progress(app: &AppHandle, download_id: &str, error: DownloadErrorInfo) {
    emit_progress(
        app,
        download_id,
        "error",
        0.0,
        ProgressFields {
            error: Some(error.message),
            error_code: Some(error.code),
            error_detail: Some(error.detail),
            ..Default::default()
        },
    );
}

pub fn emit_download_task_failure(app: &AppHandle, download_id: &str) {
    emit_error_progress(
        app,
        download_id,
        simple_error(
            "internal_task_failed",
            "The download task stopped unexpectedly. Retry the item and copy diagnostics if it happens again.",
        ),
    );
}

fn parse_first_json_value(stdout: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(stdout).or_else(|primary_error| {
        let mut stream = Deserializer::from_str(stdout).into_iter::<serde_json::Value>();
        match stream.next() {
            Some(Ok(value)) => Ok(value),
            Some(Err(_)) | None => Err(format!("Failed to parse info: {}", primary_error)),
        }
    })
}

fn spawn_stderr_tail_reader(
    mut stderr: tokio::process::ChildStderr,
) -> tokio::task::JoinHandle<Result<String, String>> {
    tokio::spawn(async move {
        let mut tail = TailBuffer::new();
        let mut buffer = [0_u8; 8 * 1024];
        let mut line = Vec::new();
        loop {
            let read = stderr.read(&mut buffer).await.map_err(|error| {
                format!("process_output_failed: could not read stderr: {error}")
            })?;
            if read == 0 {
                break;
            }
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    tail.push(String::from_utf8_lossy(&line).into_owned());
                    line.clear();
                } else if line.len() < MAX_PROCESS_LINE_BYTES {
                    line.push(*byte);
                } else {
                    return Err(
                        "process_output_limit: stderr contained a line larger than 64 KiB"
                            .to_string(),
                    );
                }
            }
        }
        if !line.is_empty() {
            tail.push(String::from_utf8_lossy(&line).into_owned());
        }

        Ok(tail.into_string())
    })
}

fn flatten_stderr_result(
    result: Result<Result<String, String>, tokio::task::JoinError>,
) -> Result<String, String> {
    result.map_err(|_| "process_output_failed: stderr reader stopped unexpectedly".to_string())?
}

async fn wait_child_monitor_stderr(
    child: &mut tokio::process::Child,
    job: &DownloadJob,
    stderr_handle: &mut tokio::task::JoinHandle<Result<String, String>>,
    stderr_output: &mut Option<String>,
    process_name: &str,
) -> Result<(std::process::ExitStatus, bool), String> {
    loop {
        tokio::select! {
            status = child.wait() => {
                return status
                    .map(|status| (status, false))
                    .map_err(|error| format!("process_wait_failed: failed to wait for {process_name}: {error}"));
            }
            result = &mut *stderr_handle, if stderr_output.is_none() => {
                match flatten_stderr_result(result) {
                    Ok(output) => *stderr_output = Some(output),
                    Err(error) => {
                        job.cancel();
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(error);
                    }
                }
            }
            _ = job.cancelled() => {
                job.cancel();
                let _ = child.kill().await;
                let status = child.wait().await
                    .map_err(|error| format!("process_reap_failed: failed to reap {process_name}: {error}"))?;
                return Ok((status, true));
            }
        }
    }
}

async fn await_stderr_tail_bounded(
    stderr_handle: &mut tokio::task::JoinHandle<Result<String, String>>,
    job: &DownloadJob,
    timeout: Duration,
) -> Result<String, String> {
    tokio::select! {
        result = &mut *stderr_handle => flatten_stderr_result(result),
        _ = job.cancelled() => {
            job.cancel();
            match tokio::time::timeout(timeout, &mut *stderr_handle).await {
                Ok(result) => flatten_stderr_result(result),
                Err(_) => {
                    stderr_handle.abort();
                    Err("process_drain_timeout: stderr remained open after cancellation".to_string())
                }
            }
        }
        _ = tokio::time::sleep(timeout) => {
            job.cancel();
            if tokio::time::timeout(Duration::from_secs(1), &mut *stderr_handle).await.is_err() {
                stderr_handle.abort();
            }
            Err("process_drain_timeout: a descendant kept stderr open".to_string())
        }
    }
}

async fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .unwrap_or(available.len());
        if bytes.len().saturating_add(take) > max_bytes {
            reader.consume(take);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "process output line exceeded 64 KiB",
            ));
        }
        bytes.extend_from_slice(&available[..take]);
        let ended = available[take - 1] == b'\n';
        reader.consume(take);
        if ended {
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
}

fn record_streamed_output_bytes(
    total: &mut usize,
    line_bytes: usize,
    limit: usize,
) -> Result<(), String> {
    *total = total.saturating_add(line_bytes.saturating_add(1));
    if *total > limit {
        Err(format!(
            "process_output_limit: inspection output exceeded the {limit}-byte limit"
        ))
    } else {
        Ok(())
    }
}

async fn read_stream_bounded<R>(mut reader: R, max_bytes: usize) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut total = 0usize;
    let mut current_line = 0usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("Failed to read process output: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        if total > max_bytes {
            return Err(format!(
                "process_output_limit: output exceeded the {max_bytes}-byte limit"
            ));
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                current_line = 0;
            } else {
                current_line = current_line.saturating_add(1);
                if current_line > MAX_PROCESS_LINE_BYTES {
                    return Err(
                        "process_output_limit: output contained a line larger than 64 KiB"
                            .to_string(),
                    );
                }
            }
        }
        retained.extend_from_slice(&buffer[..read]);
    }
    Ok(retained)
}

async fn wait_with_bounded_output(
    child: tokio::process::Child,
    job: &DownloadJob,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    wait_with_bounded_output_and_drain(
        child,
        job,
        stdout_limit,
        stderr_limit,
        timeout,
        PROCESS_DRAIN_TIMEOUT,
    )
    .await
}

async fn wait_with_bounded_output_and_drain(
    mut child: tokio::process::Child,
    job: &DownloadJob,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
    drain_timeout: Duration,
) -> Result<std::process::Output, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture process output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture process errors.".to_string())?;
    let mut stdout_reader = tokio::spawn(read_stream_bounded(stdout, stdout_limit));
    let mut stderr_reader = tokio::spawn(read_stream_bounded(stderr, stderr_limit));
    let mut stdout_result: Option<Vec<u8>> = None;
    let mut stderr_result: Option<Vec<u8>> = None;
    let mut terminal_error: Option<String> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    let status = loop {
        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(status) => break status,
                    Err(error) => {
                        job.cancel();
                        stdout_reader.abort();
                        stderr_reader.abort();
                        return Err(format!("process_wait_failed: {error}"));
                    }
                }
            }
            result = &mut stdout_reader, if stdout_result.is_none() => {
                match result {
                    Ok(Ok(output)) => stdout_result = Some(output),
                    Ok(Err(error)) => {
                        stdout_result = Some(Vec::new());
                        terminal_error = Some(error);
                        job.cancel();
                        let _ = child.kill().await;
                        break child.wait().await
                            .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
                    }
                    Err(_) => {
                        stdout_result = Some(Vec::new());
                        terminal_error = Some("process_output_failed: stdout reader stopped unexpectedly".to_string());
                        job.cancel();
                        let _ = child.kill().await;
                        break child.wait().await
                            .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
                    }
                }
            }
            result = &mut stderr_reader, if stderr_result.is_none() => {
                match result {
                    Ok(Ok(output)) => stderr_result = Some(output),
                    Ok(Err(error)) => {
                        stderr_result = Some(Vec::new());
                        terminal_error = Some(error);
                        job.cancel();
                        let _ = child.kill().await;
                        break child.wait().await
                            .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
                    }
                    Err(_) => {
                        stderr_result = Some(Vec::new());
                        terminal_error = Some("process_output_failed: stderr reader stopped unexpectedly".to_string());
                        job.cancel();
                        let _ = child.kill().await;
                        break child.wait().await
                            .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
                    }
                }
            }
            _ = job.cancelled() => {
                terminal_error = Some("process_cancelled: operation was cancelled".to_string());
                job.cancel();
                let _ = child.kill().await;
                break child.wait().await
                    .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
            }
            _ = tokio::time::sleep_until(deadline) => {
                terminal_error = Some("process_timeout: operation timed out".to_string());
                job.cancel();
                let _ = child.kill().await;
                break child.wait().await
                    .map_err(|wait_error| format!("process_reap_failed: {wait_error}"))?;
            }
        }
    };

    let drain_deadline = tokio::time::Instant::now() + drain_timeout;
    while stdout_result.is_none() || stderr_result.is_none() {
        tokio::select! {
            result = &mut stdout_reader, if stdout_result.is_none() => {
                stdout_result = Some(match result {
                    Ok(Ok(output)) => output,
                    Ok(Err(error)) => {
                        terminal_error.get_or_insert(error);
                        job.cancel();
                        Vec::new()
                    }
                    Err(_) => {
                        terminal_error.get_or_insert_with(|| "process_output_failed: stdout reader stopped unexpectedly".to_string());
                        job.cancel();
                        Vec::new()
                    }
                });
            }
            result = &mut stderr_reader, if stderr_result.is_none() => {
                stderr_result = Some(match result {
                    Ok(Ok(output)) => output,
                    Ok(Err(error)) => {
                        terminal_error.get_or_insert(error);
                        job.cancel();
                        Vec::new()
                    }
                    Err(_) => {
                        terminal_error.get_or_insert_with(|| "process_output_failed: stderr reader stopped unexpectedly".to_string());
                        job.cancel();
                        Vec::new()
                    }
                });
            }
            _ = job.cancelled(), if terminal_error.is_none() => {
                terminal_error = Some("process_cancelled: operation was cancelled during output drain".to_string());
                job.cancel();
            }
            _ = tokio::time::sleep_until(drain_deadline) => {
                job.cancel();
                if stdout_result.is_none() {
                    stdout_reader.abort();
                }
                if stderr_result.is_none() {
                    stderr_reader.abort();
                }
                return Err("process_drain_timeout: a descendant kept a process output pipe open".to_string());
            }
        }
    }
    if let Some(error) = terminal_error {
        return Err(error);
    }
    Ok(std::process::Output {
        status,
        stdout: stdout_result.unwrap_or_default(),
        stderr: stderr_result.unwrap_or_default(),
    })
}

fn is_windows_reserved_filename_stem(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn truncate_utf16(value: &str, max_units: usize) -> String {
    let mut used_units = 0usize;
    value
        .chars()
        .take_while(|character| {
            let character_units = character.len_utf16();
            if used_units + character_units > max_units {
                false
            } else {
                used_units += character_units;
                true
            }
        })
        .collect()
}

fn sanitize_filename_component(raw: &str) -> Option<String> {
    let mut cleaned: String = raw
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_control()
                || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();

    cleaned = cleaned
        .trim_matches(|ch: char| ch == ' ' || ch == '.')
        .to_string();

    if cleaned.is_empty() {
        return None;
    }

    let stem_end = cleaned.find('.').unwrap_or(cleaned.len());
    if is_windows_reserved_filename_stem(&cleaned[..stem_end]) {
        cleaned.insert(stem_end, '_');
    }

    cleaned = truncate_utf16(&cleaned, MAX_CUSTOM_FILENAME_UTF16_UNITS);
    cleaned = cleaned
        .trim_matches(|ch: char| ch == ' ' || ch == '.')
        .to_string();

    if cleaned.is_empty() {
        return None;
    }

    Some(cleaned)
}

fn normalize_filename_override(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_string();
    let lower = value.to_ascii_lowercase();
    if let Some(extension) = VIDEO_FORMATS
        .iter()
        .chain(AUDIO_FORMATS.iter())
        .find(|extension| lower.ends_with(&format!(".{extension}")))
    {
        value.truncate(value.len().saturating_sub(extension.len() + 1));
    }

    sanitize_filename_component(&value)
}

fn escape_output_template_literal(value: &str) -> String {
    value.replace('%', "%%")
}

fn build_output_template(request: &DownloadRequest) -> String {
    let output_dir = escape_output_template_literal(&request.output_dir.replace('\\', "/"));

    if let Some(filename_override) = request
        .filename_override
        .as_deref()
        .and_then(normalize_filename_override)
    {
        return format!(
            "{}/{}.%(ext)s",
            output_dir,
            escape_output_template_literal(&filename_override)
        );
    }

    format!("{}/%(title)s [%(id)s].%(ext)s", output_dir)
}

fn format_selector_for_video(request: &DownloadRequest) -> String {
    let height_limit = (request.quality != "best").then(|| request.quality.replace('p', ""));

    match (request.format.as_str(), height_limit.as_deref()) {
        ("mp4", None) => {
            "bestvideo[ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best".to_string()
        }
        ("mp4", Some(height)) => format!(
            "bestvideo[height<={height}][ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo[height<={height}][ext=mp4]+bestaudio[ext=m4a]/best[height<={height}][ext=mp4]/best[height<={height}]"
        ),
        ("mkv", None) => "bestvideo+bestaudio/best".to_string(),
        ("mkv", Some(height)) => {
            format!("bestvideo[height<={height}]+bestaudio/best[height<={height}]")
        }
        ("webm", None) => {
            "bestvideo[ext=webm]+bestaudio[ext=webm]/best[ext=webm]/bestvideo+bestaudio/best"
                .to_string()
        }
        ("webm", Some(height)) => format!(
            "bestvideo[height<={height}][ext=webm]+bestaudio[ext=webm]/best[height<={height}][ext=webm]/bestvideo[height<={height}]+bestaudio/best[height<={height}]"
        ),
        _ => "bestvideo+bestaudio/best".to_string(),
    }
}

fn append_video_postprocess_args(args: &mut Vec<String>, format: &str) {
    match format {
        "mp4" => {
            args.push("--merge-output-format".to_string());
            args.push("mp4".to_string());
            args.push("--remux-video".to_string());
            args.push("mp4".to_string());
        }
        "mkv" => {
            args.push("--merge-output-format".to_string());
            args.push("mkv".to_string());
            args.push("--remux-video".to_string());
            args.push("mkv".to_string());
        }
        "webm" => {
            args.push("--merge-output-format".to_string());
            args.push("mkv".to_string());
        }
        _ => {}
    }
}

async fn run_fetch_info_command(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    use_twitter_syndication: bool,
    allow_playlist: bool,
    job: &DownloadJob,
) -> Result<std::process::Output, String> {
    let bin = job.required_runtime_tool("yt-dlp")?;
    let runtime_config = job.ytdlp_runtime_config()?;
    let mut args = Vec::new();
    append_ytdlp_runtime_args(&mut args, &runtime_config, compat_config_path);
    args.extend([
        "--dump-single-json".to_string(),
        "--no-download".to_string(),
    ]);
    if allow_playlist {
        args.extend(["--playlist-items".to_string(), "1".to_string()]);
    } else {
        args.push("--no-playlist".to_string());
    }

    append_twitter_syndication_args(&mut args, url, use_twitter_syndication);

    if let Some(config) = cookie_config {
        append_cookie_args(&mut args, config);
    }

    args.push(url.to_string());

    let mut cmd = Command::new(&bin);
    cmd.kill_on_drop(true);
    cmd.args(&args);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job.spawn(&mut cmd, "yt-dlp", false).await?;
    wait_with_bounded_output(
        child,
        job,
        MAX_INSPECTION_OUTPUT_BYTES,
        MAX_STDERR_BYTES,
        INSPECTION_TIMEOUT,
    )
    .await
}

fn video_info_from_json(url: &str, data: &serde_json::Value) -> Result<VideoInfo, String> {
    if data
        .get("_type")
        .and_then(|value| value.as_str())
        .is_some_and(|kind| matches!(kind, "playlist" | "multi_video"))
    {
        return Err("URL resolved to a playlist instead of a single video.".into());
    }

    let mut qualities: Vec<String> = Vec::new();
    if let Some(formats) = data["formats"].as_array() {
        let mut heights: Vec<u64> = formats
            .iter()
            .filter_map(|f| f["height"].as_u64())
            .filter(|h| *h > 0)
            .collect();
        heights.sort_unstable();
        heights.dedup();
        heights.reverse();
        qualities = heights.iter().map(|h| format!("{}p", h)).collect();
    }

    let has_audio = data["acodec"].as_str().map(|a| a != "none").unwrap_or(true);

    Ok(VideoInfo {
        id: data["id"].as_str().unwrap_or("unknown").to_string(),
        title: data["title"]
            .as_str()
            .unwrap_or("Unknown Title")
            .to_string(),
        duration: data["duration"].as_f64(),
        channel: data["channel"].as_str().map(|s| s.to_string()),
        thumbnail: sanitize_thumbnail_url(data["thumbnail"].as_str()),
        url: url.to_string(),
        available_qualities: qualities,
        has_audio,
    })
}

pub async fn inspect_url(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    job: &DownloadJob,
) -> Result<UrlInspection, String> {
    validate_fetch_request(url, cookie_config, compat_config_path)?;
    let mut output =
        run_fetch_info_command(url, cookie_config, compat_config_path, false, true, job).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if should_retry_with_twitter_syndication(url, &stderr) {
            output =
                run_fetch_info_command(url, cookie_config, compat_config_path, true, true, job)
                    .await?;
        }
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(error_for_fetch(&stderr, output.status.code()));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let data = parse_first_json_value(&json_str)?;
    let is_playlist = data
        .get("_type")
        .and_then(|value| value.as_str())
        .is_some_and(|kind| matches!(kind, "playlist" | "multi_video"));
    if is_playlist {
        Ok(UrlInspection::Playlist {
            playlist: fetch_playlist(url, cookie_config, compat_config_path, job).await?,
        })
    } else {
        Ok(UrlInspection::Video {
            video: video_info_from_json(url, &data)?,
        })
    }
}

async fn fetch_playlist(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    job: &DownloadJob,
) -> Result<PlaylistInfo, String> {
    validate_fetch_request(url, cookie_config, compat_config_path)?;

    let bin = job.required_runtime_tool("yt-dlp")?;
    let runtime_config = job.ytdlp_runtime_config()?;
    let mut args = Vec::new();
    append_ytdlp_runtime_args(&mut args, &runtime_config, compat_config_path);
    args.extend([
        "--flat-playlist".to_string(),
        "--dump-json".to_string(),
        "--lazy-playlist".to_string(),
        "--playlist-end".to_string(),
        (MAX_PLAYLIST_ENTRIES + 1).to_string(),
        "--no-download".to_string(),
        url.to_string(),
    ]);
    let mut cmd = Command::new(&bin);
    cmd.kill_on_drop(true);
    cmd.args(&args);
    configure_cookie_args(&mut cmd, cookie_config);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = job.spawn(&mut cmd, "yt-dlp", false).await?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture yt-dlp playlist output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture yt-dlp playlist errors.".to_string())?;
    let mut stderr_handle = spawn_stderr_tail_reader(stderr);
    let mut stderr_output: Option<String> = None;
    let mut reader = BufReader::new(stdout);

    let mut entries: Vec<PlaylistEntry> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();
    let mut parsed_entry_count = 0usize;
    let mut truncated = false;
    let mut playlist_title = String::from("Playlist");
    let mut playlist_channel: Option<String> = None;
    let mut cancelled = false;
    let mut output_error: Option<String> = None;
    let mut stdout_bytes = 0usize;

    loop {
        let next_line = tokio::select! {
            line = read_bounded_line(&mut reader, MAX_PROCESS_LINE_BYTES) => line,
            result = &mut stderr_handle, if stderr_output.is_none() => {
                match flatten_stderr_result(result) {
                    Ok(output) => {
                        stderr_output = Some(output);
                        continue;
                    }
                    Err(error) => {
                        output_error = Some(error);
                        job.cancel();
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            _ = job.cancelled() => {
                cancelled = true;
                let _ = child.kill().await;
                break;
            },
        };
        let line = match next_line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                output_error = Some(format!("Playlist output limit was exceeded: {error}"));
                job.cancel();
                let _ = child.kill().await;
                break;
            }
        };
        if let Err(error) =
            record_streamed_output_bytes(&mut stdout_bytes, line.len(), MAX_INSPECTION_OUTPUT_BYTES)
        {
            output_error = Some(error);
            job.cancel();
            let _ = child.kill().await;
            break;
        }
        let Ok(data) = serde_json::from_str::<PlaylistLineRecord>(&line) else {
            continue;
        };

        if entries.is_empty() {
            if let Some(title) = data.playlist_title_hint() {
                playlist_title = title.to_string();
            }

            playlist_channel = data
                .playlist_channel_hint()
                .map(|channel| channel.to_string());
        }

        if let Some(entry) = data.into_playlist_entry() {
            push_bounded_playlist_entry(
                &mut entries,
                &mut seen_urls,
                &mut parsed_entry_count,
                &mut truncated,
                entry,
            );
        }
    }

    let (status, wait_cancelled) = wait_child_monitor_stderr(
        &mut child,
        job,
        &mut stderr_handle,
        &mut stderr_output,
        "yt-dlp",
    )
    .await?;
    cancelled |= wait_cancelled;
    let stderr_output = match stderr_output {
        Some(output) => output,
        None => {
            match await_stderr_tail_bounded(&mut stderr_handle, job, PROCESS_DRAIN_TIMEOUT).await {
                Ok(output) => output,
                Err(error) => {
                    output_error.get_or_insert(error);
                    String::new()
                }
            }
        }
    };

    if (cancelled || job.is_cancelled()) && output_error.is_none() {
        return Err("Playlist inspection was cancelled.".into());
    }
    if let Some(error) = output_error {
        return Err(error);
    }

    if !status.success() {
        return Err(error_for_fetch(&stderr_output, status.code()));
    }

    if entries.is_empty() {
        return Err("Failed to parse playlist entries".into());
    }

    Ok(PlaylistInfo {
        title: playlist_title,
        channel: playlist_channel,
        entry_count: entries.len(),
        truncated,
        entries,
    })
}

fn push_bounded_playlist_entry(
    entries: &mut Vec<PlaylistEntry>,
    seen_urls: &mut HashSet<String>,
    parsed_entry_count: &mut usize,
    truncated: &mut bool,
    entry: PlaylistEntry,
) {
    *parsed_entry_count += 1;
    if *parsed_entry_count > MAX_PLAYLIST_ENTRIES {
        *truncated = true;
        return;
    }

    if seen_urls.insert(entry.url.clone()) {
        entries.push(entry);
    }
}

fn build_download_args_with_runtime(
    request: &DownloadRequest,
    use_twitter_syndication: bool,
    runtime_config: &YtdlpCommandConfig,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    append_ytdlp_runtime_args(
        &mut args,
        runtime_config,
        request.compat_config_path.as_deref(),
    );

    let is_audio_only = matches!(
        request.format.as_str(),
        "mp3" | "flac" | "wav" | "aac" | "opus"
    );

    if is_audio_only {
        args.push("-x".to_string());
        args.push("--audio-format".to_string());
        args.push(request.format.clone());
        args.push("--audio-quality".to_string());
        args.push("0".to_string());
    } else {
        args.push("-f".to_string());
        args.push(format_selector_for_video(request));
        append_video_postprocess_args(&mut args, &request.format);
    }

    args.push("--newline".to_string());
    args.push("--progress".to_string());
    args.push("--progress-delta".to_string());
    args.push("0.5".to_string());
    args.push("--no-playlist".to_string());
    append_twitter_syndication_args(&mut args, &request.url, use_twitter_syndication);
    args.push("-o".to_string());
    args.push(build_output_template(request));

    if let Some(config) = request.cookie_config.as_ref() {
        append_cookie_args(&mut args, config);
    }

    args.push(request.url.clone());
    args
}

#[cfg(test)]
fn build_download_args(request: &DownloadRequest, use_twitter_syndication: bool) -> Vec<String> {
    let runtime_config = YtdlpCommandConfig {
        ffmpeg_dir: None,
        deno_path: None,
        plugin_dir: None,
    };
    build_download_args_with_runtime(request, use_twitter_syndication, &runtime_config)
}

fn staging_root(output_dir: &Path) -> PathBuf {
    output_dir.join(STAGING_ROOT_NAME)
}

fn build_staging_dir(output_dir: &Path, download_id: &str) -> PathBuf {
    // Operation IDs are generated by the backend, but retain a strict UUID
    // fallback for legacy renderer-generated IDs during the 0.6.0 bridge.
    let safe_id = uuid::Uuid::parse_str(download_id.trim())
        .map(|id| id.to_string())
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    staging_root(output_dir).join(safe_id)
}

fn reset_staging_dir(path: &Path, output_dir: &Path, operation_id: &str) -> Result<(), String> {
    let (root, operation_path, normalized_id) =
        validate_staging_layout(path, output_dir, operation_id, false)?;
    if operation_path.exists() {
        quarantine_and_remove_staging_dir(&root, &operation_path, &normalized_id, true)
            .map_err(|error| format!("Failed to reset staging folder: {error}"))?;
    }

    mark_hidden(&root)?;
    std::fs::create_dir(&operation_path)
        .map_err(|error| format!("Failed to create staging folder: {error}"))?;
    let marker = serde_json::json!({
        "schemaVersion": 1,
        "owner": "nuclear-downloader",
        "operationId": normalized_id,
    });
    let marker_bytes = serde_json::to_vec(&marker).map_err(|error| error.to_string())?;
    let mut marker_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(operation_path.join(STAGING_MARKER_NAME))
        .map_err(|error| format!("Failed to create staging ownership marker: {error}"))?;
    marker_file
        .write_all(&marker_bytes)
        .and_then(|()| marker_file.sync_all())
        .map_err(|error| format!("Failed to write staging ownership marker: {error}"))?;
    drop(marker_file);
    verify_staging_marker(&operation_path, &normalized_id)
}

fn cleanup_staging_dir(path: &Path, output_dir: &Path, operation_id: &str) -> Result<(), String> {
    let (root, operation_path, normalized_id) =
        validate_staging_layout(path, output_dir, operation_id, true)?;
    quarantine_and_remove_staging_dir(&root, &operation_path, &normalized_id, true)
        .map_err(|error| format!("Failed to clean staging folder: {error}"))
}

fn quarantine_and_remove_staging_dir(
    root: &Path,
    source: &Path,
    operation_id: &str,
    require_operation_name: bool,
) -> Result<(), String> {
    validate_owned_staging_directory(root, source, operation_id, require_operation_name)?;
    let quarantine = root.join(format!(
        "{STAGING_CLEANUP_PREFIX}{operation_id}.{}",
        uuid::Uuid::new_v4()
    ));
    if quarantine.exists() {
        return Err("A staging quarantine destination unexpectedly already exists.".into());
    }
    std::fs::rename(source, &quarantine)
        .map_err(|error| format!("Could not atomically quarantine staging data: {error}"))?;
    validate_owned_staging_directory(root, &quarantine, operation_id, false)?;
    // Rust's recursive removal does not traverse directory symlinks. Combined
    // with the same-volume quarantine rename and the second identity check,
    // a replaced source path can never redirect deletion outside this root.
    std::fs::remove_dir_all(&quarantine)
        .map_err(|error| format!("Could not remove quarantined staging data: {error}"))
}

fn validate_owned_staging_directory(
    root: &Path,
    path: &Path,
    operation_id: &str,
    require_operation_name: bool,
) -> Result<(), String> {
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("Failed to inspect staging root: {error}"))?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect staging operation: {error}"))?;
    if !root_metadata.is_dir()
        || is_reparse_metadata(&root_metadata)
        || !metadata.is_dir()
        || is_reparse_metadata(&metadata)
    {
        return Err("Staging cleanup requires regular non-reparse directories.".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("Failed to resolve staging root: {error}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("Failed to resolve staging operation: {error}"))?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return Err("Staging cleanup target escaped its staging root.".into());
    }
    verify_staging_marker_with_name_policy(path, operation_id, require_operation_name)
}

fn validate_staging_layout(
    path: &Path,
    output_dir: &Path,
    operation_id: &str,
    require_operation: bool,
) -> Result<(PathBuf, PathBuf, String), String> {
    let normalized_id = uuid::Uuid::parse_str(operation_id.trim())
        .map_err(|_| "Staging operation ID was not a UUID.".to_string())?
        .to_string();
    let expected_lexical = staging_root(output_dir).join(&normalized_id);
    if path != expected_lexical {
        return Err("Refusing to use a staging folder outside the expected operation path.".into());
    }
    let output_metadata = std::fs::symlink_metadata(output_dir)
        .map_err(|error| format!("Failed to inspect output folder: {error}"))?;
    if !output_metadata.is_dir() || is_reparse_metadata(&output_metadata) {
        return Err("Output folder was not a regular non-reparse directory.".into());
    }
    let canonical_output = output_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve output folder: {error}"))?;
    let root = staging_root(&canonical_output);
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse_metadata(&metadata) {
                return Err("Staging root was not a regular non-reparse directory.".into());
            }
            let canonical_root = root
                .canonicalize()
                .map_err(|error| format!("Failed to resolve staging root: {error}"))?;
            if canonical_root.parent() != Some(canonical_output.as_path()) {
                return Err("Staging root escaped the output folder.".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !require_operation => {
            std::fs::create_dir(&root)
                .map_err(|error| format!("Failed to create staging root: {error}"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("Staging root did not exist.".into());
        }
        Err(error) => return Err(format!("Failed to inspect staging root: {error}")),
    }
    let operation_path = root.join(&normalized_id);
    match std::fs::symlink_metadata(&operation_path) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse_metadata(&metadata) {
                return Err("Staging operation was not a regular non-reparse directory.".into());
            }
            let canonical_operation = operation_path
                .canonicalize()
                .map_err(|error| format!("Failed to resolve staging operation: {error}"))?;
            if canonical_operation.parent() != Some(root.as_path()) {
                return Err("Staging operation escaped the staging root.".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !require_operation => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("Staging operation did not exist.".into());
        }
        Err(error) => return Err(format!("Failed to inspect staging operation: {error}")),
    }
    Ok((root, operation_path, normalized_id))
}

fn verify_staging_marker(path: &Path, expected_operation_id: &str) -> Result<(), String> {
    verify_staging_marker_with_name_policy(path, expected_operation_id, true)
}

fn verify_staging_marker_with_name_policy(
    path: &Path,
    expected_operation_id: &str,
    require_operation_name: bool,
) -> Result<(), String> {
    let marker_path = path.join(STAGING_MARKER_NAME);
    let marker_metadata = std::fs::symlink_metadata(&marker_path)
        .map_err(|_| "Staging folder did not contain an ownership marker.".to_string())?;
    if !marker_metadata.is_file() || is_reparse_metadata(&marker_metadata) {
        return Err("Staging ownership marker was not a regular non-reparse file.".into());
    }
    let marker: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&marker_path)
            .map_err(|_| "Staging folder did not contain an ownership marker.".to_string())?,
    )
    .map_err(|_| "Staging ownership marker was invalid.".to_string())?;
    let directory_id = path.file_name().and_then(|name| name.to_str());
    if marker["schemaVersion"] != 1
        || marker["owner"] != "nuclear-downloader"
        || marker["operationId"] != expected_operation_id
        || (require_operation_name && directory_id != Some(expected_operation_id))
    {
        return Err("Staging ownership marker was not recognized.".to_string());
    }
    Ok(())
}

pub fn cleanup_abandoned_download_stages(output_roots: &[String]) -> Vec<String> {
    let mut failures = Vec::new();
    for (root_index, output_root) in output_roots.iter().enumerate() {
        if let Err(error) = cleanup_abandoned_download_stages_at(Path::new(output_root)) {
            failures.push(format!("Output root {root_index}: {error}"));
        }
    }
    failures
}

fn cleanup_abandoned_download_stages_at(output_root: &Path) -> Result<(), String> {
    let output_metadata = match std::fs::symlink_metadata(output_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect output root ({})", error.kind())),
    };
    if !output_metadata.is_dir() || is_reparse_metadata(&output_metadata) {
        return Err("output root is not a regular non-reparse directory".to_string());
    }
    let output_root = output_root
        .canonicalize()
        .map_err(|error| format!("could not resolve output root ({})", error.kind()))?;
    let staging_root = staging_root(&output_root);
    let staging_metadata = match std::fs::symlink_metadata(&staging_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect staging root ({})", error.kind())),
    };
    if !staging_metadata.is_dir() || is_reparse_metadata(&staging_metadata) {
        return Err("staging root is not a regular non-reparse directory".to_string());
    }

    let entries = std::fs::read_dir(&staging_root)
        .map_err(|error| format!("could not enumerate staging root ({})", error.kind()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not inspect staging entry ({})", error.kind()))?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let operation_id = if uuid::Uuid::parse_str(name).is_ok() {
            name.to_string()
        } else if let Some(rest) = name.strip_prefix(STAGING_CLEANUP_PREFIX) {
            let Some((operation_id, quarantine_id)) = rest.split_once('.') else {
                continue;
            };
            if uuid::Uuid::parse_str(operation_id).is_err()
                || uuid::Uuid::parse_str(quarantine_id).is_err()
            {
                continue;
            }
            operation_id.to_string()
        } else {
            continue;
        };
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect staging operation ({})", error.kind()))?;
        if !metadata.is_dir() || is_reparse_metadata(&metadata) {
            continue;
        }
        if verify_staging_marker_with_name_policy(
            &path,
            &operation_id,
            uuid::Uuid::parse_str(name).is_ok(),
        )
        .is_err()
        {
            continue;
        }
        quarantine_and_remove_staging_dir(&staging_root, &path, &operation_id, false)?;
    }
    Ok(())
}

fn is_reparse_metadata(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

#[cfg(windows)]
fn mark_hidden(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    #[link(name = "Kernel32")]
    extern "system" {
        fn GetFileAttributesW(path: *const u16) -> u32;
        fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32;
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == u32::MAX
        || unsafe { SetFileAttributesW(wide.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) } == 0
    {
        return Err(format!(
            "Failed to hide staging root: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn mark_hidden(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn validate_staged_file(path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    let reported_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect reported output: {error}"))?;
    if !reported_metadata.file_type().is_file() || reported_metadata.file_type().is_symlink() {
        return Err("Downloader output was not a regular non-reparse file.".to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if reported_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("Downloader output was a reparse point.".to_string());
        }
    }
    let canonical_stage = staging_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve staging folder: {error}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("Failed to resolve staged output: {error}"))?;
    if canonical_path == canonical_stage || !canonical_path.starts_with(&canonical_stage) {
        return Err("Downloader reported a path outside its staging folder.".to_string());
    }
    let metadata = std::fs::symlink_metadata(&canonical_path)
        .map_err(|error| format!("Failed to inspect staged output: {error}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("Downloader output was not a regular non-reparse file.".to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("Downloader output was a reparse point.".to_string());
        }
    }
    Ok(canonical_path)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn build_webm_final_path(request: &DownloadRequest, intermediate_path: &Path) -> PathBuf {
    let filename = request
        .filename_override
        .as_deref()
        .and_then(normalize_filename_override)
        .or_else(|| {
            intermediate_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(sanitize_filename_component)
        })
        .unwrap_or_else(|| "download".to_string());

    PathBuf::from(&request.output_dir).join(format!("{filename}.webm"))
}

fn build_staged_webm_output_path(staging_dir: &Path, final_path: &Path) -> PathBuf {
    let stem = final_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(sanitize_filename_component)
        .unwrap_or_else(|| "download".to_string());

    staging_dir.join(format!("{stem}.converted.webm"))
}

fn build_final_output_path(
    request: &DownloadRequest,
    staged_path: &Path,
) -> Result<PathBuf, String> {
    let file_name = if let Some(filename) = request
        .filename_override
        .as_deref()
        .and_then(normalize_filename_override)
    {
        let extension = staged_path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(request.format.as_str());
        format!("{filename}.{extension}").into()
    } else {
        staged_path
            .file_name()
            .ok_or_else(|| "Downloaded file did not have a valid filename.".to_string())?
            .to_os_string()
    };

    Ok(PathBuf::from(&request.output_dir).join(file_name))
}

fn suffixed_output_path(base_path: &Path, suffix: usize) -> PathBuf {
    if suffix <= 1 {
        return base_path.to_path_buf();
    }

    let stem = base_path
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(sanitize_filename_component)
        .unwrap_or_else(|| "download".to_string());
    let extension = base_path.extension().and_then(|value| value.to_str());
    let filename = match extension {
        Some(extension) if !extension.is_empty() => format!("{stem} ({suffix}).{extension}"),
        _ => format!("{stem} ({suffix})"),
    };

    base_path.with_file_name(filename)
}

async fn publish_staged_output(
    staged_output: &Path,
    desired_path: &Path,
    job: Option<&DownloadJob>,
) -> Result<PathBuf, String> {
    if let Some(parent) = desired_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Failed to create output folder: {error}"))?;
    }

    let staged_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(staged_output)
        .await
        .map_err(|error| format!("Failed to open staged output: {error}"))?;
    staged_file
        .sync_all()
        .await
        .map_err(|error| format!("Failed to flush staged output: {error}"))?;
    drop(staged_file);

    for suffix in 1..=MAX_OUTPUT_SUFFIX {
        if job.is_some_and(DownloadJob::is_cancelled) {
            return Err("Publishing was cancelled.".into());
        }

        let candidate = suffixed_output_path(desired_path, suffix);
        if candidate.exists() {
            continue;
        }

        match atomic_move_no_replace(staged_output, &candidate).await {
            Ok(()) => {
                return Ok(candidate);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists || candidate.exists() =>
            {
                continue
            }
            Err(error) => {
                return Err(format!("Failed to publish output: {error}"));
            }
        }
    }

    Err("Could not allocate a unique output filename.".into())
}

#[cfg(windows)]
async fn atomic_move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
async fn atomic_move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    tokio::fs::hard_link(source, destination).await?;
    tokio::fs::remove_file(source).await
}

fn find_latest_media_file(dir: &Path) -> Option<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if !matches!(
            extension.as_str(),
            "mp4"
                | "mkv"
                | "webm"
                | "mov"
                | "m4a"
                | "mp3"
                | "flac"
                | "wav"
                | "aac"
                | "opus"
                | "ogg"
                | "ts"
        ) {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        if latest
            .as_ref()
            .map(|(latest_modified, _)| modified > *latest_modified)
            .unwrap_or(true)
        {
            latest = Some((modified, path));
        }
    }

    latest.map(|(_, path)| path)
}

fn parse_ffmpeg_time_value(value: &str) -> Option<f64> {
    let value = value.trim();

    if let Ok(microseconds) = value.parse::<f64>() {
        return (microseconds >= 0.0).then_some(microseconds / 1_000_000.0);
    }

    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let hours = parts[0].parse::<f64>().ok()?;
    let minutes = parts[1].parse::<f64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;

    Some((hours * 3600.0) + (minutes * 60.0) + seconds)
}

fn parse_ffmpeg_progress_percent(line: &str, duration_seconds: f64) -> Option<f64> {
    if duration_seconds <= 0.0 || !duration_seconds.is_finite() {
        return None;
    }

    let value = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))
        .or_else(|| line.strip_prefix("out_time="))?;
    let seconds = parse_ffmpeg_time_value(value)?;

    Some(((seconds / duration_seconds) * 100.0).clamp(0.0, 100.0))
}

async fn probe_media_duration_seconds(path: &Path, job: &DownloadJob) -> Result<f64, String> {
    if job.is_cancelled() {
        return Err("Conversion was cancelled.".into());
    }

    let ffprobe = job.required_runtime_tool("ffprobe")?;
    let mut cmd = Command::new(ffprobe);
    cmd.kill_on_drop(true);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ]);
    cmd.arg(path);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job.spawn(&mut cmd, "ffprobe", false).await?;

    let output = wait_with_bounded_output(
        child,
        job,
        MAX_PROCESS_LINE_BYTES,
        MAX_STDERR_BYTES,
        Duration::from_secs(30),
    )
    .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let duration = stdout
        .trim()
        .parse::<f64>()
        .map_err(|_| "ffprobe did not return a valid duration.".to_string())?;

    if duration.is_finite() && duration > 0.0 {
        Ok(duration)
    } else {
        Err("ffprobe could not determine media duration.".into())
    }
}

async fn publish_converted_output(
    staged_output: &Path,
    final_path: &Path,
    job: &DownloadJob,
) -> Result<PathBuf, String> {
    publish_staged_output(staged_output, final_path, Some(job)).await
}

enum DownloadAttemptResult {
    Completed(Option<String>),
    Cancelled,
    RetryWithTwitterSyndication,
    Error(DownloadErrorInfo),
}

async fn run_download_attempt(
    app: &AppHandle,
    download_id: &str,
    request: &DownloadRequest,
    job: &DownloadJob,
    use_twitter_syndication: bool,
) -> DownloadAttemptResult {
    if job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    let runtime_config = match job.ytdlp_runtime_config() {
        Ok(config) => config,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let args = build_download_args_with_runtime(request, use_twitter_syndication, &runtime_config);

    let bin = match job.required_runtime_tool("yt-dlp") {
        Ok(path) => path,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let mut cmd = Command::new(&bin);
    cmd.kill_on_drop(true);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match job.spawn(&mut cmd, "yt-dlp", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start yt-dlp: {}", error),
            ));
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut stderr_handle = spawn_stderr_tail_reader(stderr);
    let mut stderr_output: Option<String> = None;
    let mut reader = BufReader::new(stdout);
    let mut last_filename: Option<String> = None;
    let mut cancelled = false;
    let mut stdout_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            line = read_bounded_line(&mut reader, MAX_PROCESS_LINE_BYTES) => match line {
                Ok(Some(line)) => Some(line),
                Ok(None) => None,
                Err(error) => {
                    stdout_error = Some(format!("Failed to read yt-dlp progress: {error}"));
                    job.cancel();
                    let _ = child.kill().await;
                    None
                }
            },
            result = &mut stderr_handle, if stderr_output.is_none() => {
                match flatten_stderr_result(result) {
                    Ok(output) => {
                        stderr_output = Some(output);
                        continue;
                    }
                    Err(error) => {
                        stdout_error = Some(error);
                        job.cancel();
                        let _ = child.kill().await;
                        None
                    }
                }
            }
            _ = job.cancelled() => {
                cancelled = true;
                let _ = child.kill().await;
                None
            }
        };
        let Some(line) = line else {
            break;
        };

        if let Some(caps) = DOWNLOAD_DEST_RE.captures(&line) {
            last_filename = Some(caps[1].trim().to_string());
        } else if let Some(caps) = DOWNLOAD_FINAL_DEST_RE.captures(&line) {
            last_filename = Some(caps[1].trim().trim_matches('"').to_string());
        }

        if let Some(caps) = DOWNLOAD_PROGRESS_RE.captures(&line) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            let speed = DOWNLOAD_SPEED_RE.captures(&line).map(|c| c[1].to_string());
            let eta = DOWNLOAD_ETA_RE.captures(&line).map(|c| c[1].to_string());
            emit_progress(
                app,
                download_id,
                "downloading",
                pct,
                ProgressFields {
                    speed,
                    eta,
                    filename: None,
                    phase: Some("download"),
                    download_progress: Some(pct),
                    ..Default::default()
                },
            );
        } else if DOWNLOAD_MERGE_RE.is_match(&line) {
            emit_progress(
                app,
                download_id,
                "postprocessing",
                100.0,
                ProgressFields {
                    filename: None,
                    phase: Some("postprocess"),
                    download_progress: Some(100.0),
                    ..Default::default()
                },
            );
        }
    }

    let (status, wait_cancelled) = match wait_child_monitor_stderr(
        &mut child,
        job,
        &mut stderr_handle,
        &mut stderr_output,
        "yt-dlp",
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };
    cancelled |= wait_cancelled;
    let stderr_output = match stderr_output {
        Some(output) => output,
        None => {
            match await_stderr_tail_bounded(&mut stderr_handle, job, PROCESS_DRAIN_TIMEOUT).await {
                Ok(output) => output,
                Err(error) => {
                    stdout_error.get_or_insert(error);
                    String::new()
                }
            }
        }
    };

    if (cancelled || job.is_cancelled()) && stdout_error.is_none() {
        return DownloadAttemptResult::Cancelled;
    }

    if let Some(error) = stdout_error {
        return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
    }

    match status {
        s if s.success() => DownloadAttemptResult::Completed(last_filename),
        s => {
            if !use_twitter_syndication
                && should_retry_with_twitter_syndication(&request.url, &stderr_output)
            {
                DownloadAttemptResult::RetryWithTwitterSyndication
            } else {
                DownloadAttemptResult::Error(classify_process_error(
                    &stderr_output,
                    s.code(),
                    Some(&request.format),
                ))
            }
        }
    }
}

async fn run_webm_conversion(
    app: &AppHandle,
    download_id: &str,
    input_path: &Path,
    staged_output: &Path,
    final_path: &Path,
    job: &DownloadJob,
) -> DownloadAttemptResult {
    let duration_seconds = match probe_media_duration_seconds(input_path, job).await {
        Ok(duration) => duration,
        Err(_) if job.is_cancelled() => return DownloadAttemptResult::Cancelled,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("postprocess_failed", error))
        }
    };

    emit_progress(
        app,
        download_id,
        "postprocessing",
        0.0,
        ProgressFields {
            filename: None,
            phase: Some("conversion"),
            download_progress: Some(100.0),
            conversion_progress: Some(0.0),
            ..Default::default()
        },
    );

    let ffmpeg = match job.required_runtime_tool("ffmpeg") {
        Ok(path) => path,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true);
    cmd.args([
        "-y",
        "-hide_banner",
        "-nostats",
        "-stats_period",
        "0.5",
        "-i",
    ]);
    cmd.arg(input_path);
    cmd.args([
        "-map",
        "0:v:0",
        "-map",
        "0:a?",
        "-c:v",
        "libvpx-vp9",
        "-row-mt",
        "1",
        "-cpu-used",
        "4",
        "-crf",
        "32",
        "-b:v",
        "0",
        "-c:a",
        "libopus",
        "-b:a",
        "128k",
        "-f",
        "webm",
        "-progress",
        "pipe:1",
    ]);
    cmd.arg(staged_output);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match job.spawn(&mut cmd, "ffmpeg", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start ffmpeg: {error}"),
            ));
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut stderr_handle = spawn_stderr_tail_reader(stderr);
    let mut stderr_output: Option<String> = None;
    let mut reader = BufReader::new(stdout);
    let mut last_progress: f64 = 0.0;
    let mut cancelled = false;
    let mut stdout_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            line = read_bounded_line(&mut reader, MAX_PROCESS_LINE_BYTES) => match line {
                Ok(Some(line)) => Some(line),
                Ok(None) => None,
                Err(error) => {
                    stdout_error = Some(format!("Failed to read FFmpeg progress: {error}"));
                    job.cancel();
                    let _ = child.kill().await;
                    None
                }
            },
            result = &mut stderr_handle, if stderr_output.is_none() => {
                match flatten_stderr_result(result) {
                    Ok(output) => {
                        stderr_output = Some(output);
                        continue;
                    }
                    Err(error) => {
                        stdout_error = Some(error);
                        job.cancel();
                        let _ = child.kill().await;
                        None
                    }
                }
            }
            _ = job.cancelled() => {
                cancelled = true;
                let _ = child.kill().await;
                None
            }
        };
        let Some(line) = line else {
            break;
        };

        if line.trim() == "progress=end" {
            last_progress = 100.0;
        } else if let Some(progress) = parse_ffmpeg_progress_percent(&line, duration_seconds) {
            last_progress = last_progress.max(progress);
        } else {
            continue;
        }

        emit_progress(
            app,
            download_id,
            "postprocessing",
            last_progress,
            ProgressFields {
                filename: None,
                phase: Some("conversion"),
                download_progress: Some(100.0),
                conversion_progress: Some(last_progress),
                ..Default::default()
            },
        );
    }

    let (status, wait_cancelled) = match wait_child_monitor_stderr(
        &mut child,
        job,
        &mut stderr_handle,
        &mut stderr_output,
        "FFmpeg",
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };
    cancelled |= wait_cancelled;
    let stderr_output = match stderr_output {
        Some(output) => output,
        None => {
            match await_stderr_tail_bounded(&mut stderr_handle, job, PROCESS_DRAIN_TIMEOUT).await {
                Ok(output) => output,
                Err(error) => {
                    stdout_error.get_or_insert(error);
                    String::new()
                }
            }
        }
    };

    if (cancelled || job.is_cancelled()) && stdout_error.is_none() {
        return DownloadAttemptResult::Cancelled;
    }

    if let Some(error) = stdout_error {
        return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
    }

    match status {
        status if status.success() => {
            match publish_converted_output(staged_output, final_path, job).await {
                Ok(published_path) => {
                    DownloadAttemptResult::Completed(Some(path_to_string(&published_path)))
                }
                Err(_) if job.is_cancelled() => DownloadAttemptResult::Cancelled,
                Err(error) => {
                    DownloadAttemptResult::Error(simple_error("postprocess_failed", error))
                }
            }
        }
        status => DownloadAttemptResult::Error(classify_process_error(
            &stderr_output,
            status.code(),
            Some("webm"),
        )),
    }
}

async fn run_webm_download(
    app: &AppHandle,
    download_id: &str,
    request: &DownloadRequest,
    manager: &DownloadManager,
    job: &DownloadJob,
) -> DownloadAttemptResult {
    let output_dir = Path::new(&request.output_dir);
    let staging_dir = build_staging_dir(output_dir, download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir, output_dir, download_id) {
            return DownloadAttemptResult::Error(simple_error("staging_failed", error));
        }

        let mut staged_request = request.clone();
        staged_request.output_dir = path_to_string(&staging_dir);

        match run_download_attempt(
            app,
            download_id,
            &staged_request,
            job,
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(filename) => {
                let intermediate_path = filename
                    .map(PathBuf::from)
                    .or_else(|| find_latest_media_file(&staging_dir));
                let Some(intermediate_path) = intermediate_path else {
                    let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                    return DownloadAttemptResult::Error(simple_error(
                        "staging_failed",
                        "Download completed but no staged media file was found.",
                    ));
                };
                let intermediate_path = match validate_staged_file(&intermediate_path, &staging_dir)
                {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                        return DownloadAttemptResult::Error(simple_error("path_escape", error));
                    }
                };

                let final_path = build_webm_final_path(request, &intermediate_path);
                let staged_output = build_staged_webm_output_path(&staging_dir, &final_path);
                emit_progress(
                    app,
                    download_id,
                    "postprocessing",
                    0.0,
                    ProgressFields {
                        filename: None,
                        phase: Some("waiting_conversion"),
                        download_progress: Some(100.0),
                        conversion_progress: Some(0.0),
                        ..Default::default()
                    },
                );
                let _conversion_permit = match manager.acquire_conversion(job).await {
                    Ok(Some(permit)) => permit,
                    Ok(None) => {
                        let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                        return DownloadAttemptResult::Cancelled;
                    }
                    Err(error) => {
                        let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                        return DownloadAttemptResult::Error(simple_error(
                            "conversion_scheduler_failed",
                            error,
                        ));
                    }
                };
                let result = run_webm_conversion(
                    app,
                    download_id,
                    &intermediate_path,
                    &staged_output,
                    &final_path,
                    job,
                )
                .await;

                let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                return result;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                use_twitter_syndication = true;
            }
            other => {
                let _ = cleanup_staging_dir(&staging_dir, output_dir, download_id);
                return other;
            }
        }
    }
}

pub async fn start_download(
    app: AppHandle,
    download_id: String,
    mut request: DownloadRequest,
    manager: DownloadManager,
    job: DownloadJob,
) {
    if job.is_cancelled() {
        emit_progress(
            &app,
            &download_id,
            "cancelled",
            0.0,
            ProgressFields::default(),
        );
        return;
    }

    match validate_output_directory(&request.output_dir) {
        Ok(output_dir) => request.output_dir = output_dir,
        Err(error) => {
            emit_error_progress(
                &app,
                &download_id,
                DownloadErrorInfo {
                    code: error.code,
                    message: error.summary,
                    detail: error.detail.unwrap_or_default(),
                },
            );
            return;
        }
    }

    emit_progress(
        &app,
        &download_id,
        "downloading",
        0.0,
        ProgressFields {
            phase: Some("download"),
            download_progress: Some(0.0),
            ..Default::default()
        },
    );

    if request.format == "webm" {
        match run_webm_download(&app, &download_id, &request, &manager, &job).await {
            DownloadAttemptResult::Completed(filename) => {
                emit_progress(
                    &app,
                    &download_id,
                    "completed",
                    100.0,
                    ProgressFields {
                        filename,
                        phase: Some("complete"),
                        download_progress: Some(100.0),
                        conversion_progress: Some(100.0),
                        ..Default::default()
                    },
                );
            }
            DownloadAttemptResult::Cancelled => {
                emit_progress(
                    &app,
                    &download_id,
                    "cancelled",
                    0.0,
                    ProgressFields::default(),
                );
            }
            DownloadAttemptResult::Error(error) => {
                emit_error_progress(&app, &download_id, error);
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                emit_error_progress(
                    &app,
                    &download_id,
                    simple_error(
                        "download_failed",
                        "Download failed before retry could complete.",
                    ),
                );
            }
        }
        return;
    }

    let output_dir = Path::new(&request.output_dir);
    let staging_dir = build_staging_dir(output_dir, &download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir, output_dir, &download_id) {
            emit_error_progress(&app, &download_id, simple_error("staging_failed", error));
            return;
        }

        let mut staged_request = request.clone();
        staged_request.output_dir = path_to_string(&staging_dir);

        match run_download_attempt(
            &app,
            &download_id,
            &staged_request,
            &job,
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(filename) => {
                let staged_path = filename
                    .map(PathBuf::from)
                    .filter(|path| path.is_file())
                    .or_else(|| find_latest_media_file(&staging_dir));
                let Some(staged_path) = staged_path else {
                    let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                    emit_error_progress(
                        &app,
                        &download_id,
                        simple_error(
                            "staging_failed",
                            "Download completed but no staged media file was found.",
                        ),
                    );
                    return;
                };
                let staged_path = match validate_staged_file(&staged_path, &staging_dir) {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                        emit_error_progress(&app, &download_id, simple_error("path_escape", error));
                        return;
                    }
                };

                let desired_path = match build_final_output_path(&request, &staged_path) {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                        emit_error_progress(
                            &app,
                            &download_id,
                            simple_error("invalid_filename", error),
                        );
                        return;
                    }
                };

                emit_progress(
                    &app,
                    &download_id,
                    "postprocessing",
                    100.0,
                    ProgressFields {
                        filename: None,
                        phase: Some("postprocess"),
                        download_progress: Some(100.0),
                        ..Default::default()
                    },
                );

                match publish_staged_output(&staged_path, &desired_path, Some(&job)).await {
                    Ok(published_path) => emit_progress(
                        &app,
                        &download_id,
                        "completed",
                        100.0,
                        ProgressFields {
                            filename: Some(path_to_string(&published_path)),
                            phase: Some("complete"),
                            download_progress: Some(100.0),
                            ..Default::default()
                        },
                    ),
                    Err(_) if job.is_cancelled() => emit_progress(
                        &app,
                        &download_id,
                        "cancelled",
                        0.0,
                        ProgressFields::default(),
                    ),
                    Err(error) => emit_error_progress(
                        &app,
                        &download_id,
                        simple_error("publish_failed", error),
                    ),
                }
                let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                return;
            }
            DownloadAttemptResult::Cancelled => {
                let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                emit_progress(
                    &app,
                    &download_id,
                    "cancelled",
                    0.0,
                    ProgressFields::default(),
                );
                return;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                use_twitter_syndication = true;
            }
            DownloadAttemptResult::Error(error) => {
                let _ = cleanup_staging_dir(&staging_dir, output_dir, &download_id);
                emit_error_progress(&app, &download_id, error);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_download_args, build_download_args_with_runtime, build_error_message,
        build_output_template, build_staging_dir, build_webm_final_path, classify_process_error,
        cleanup_abandoned_download_stages, cleanup_staging_dir, is_twitter_api_auth_error,
        is_twitter_missing_video_error, is_x_or_twitter_url, normalize_filename_override,
        parse_ffmpeg_progress_percent, parse_first_json_value, publish_staged_output,
        push_bounded_playlist_entry, reset_staging_dir, sanitize_thumbnail_url,
        should_retry_with_twitter_syndication, validate_download_request, validate_fetch_request,
        wait_with_bounded_output_and_drain, DownloadJob, DownloadManager, PlaylistLineRecord,
        MAX_INSPECTION_OUTPUT_BYTES, MAX_PLAYLIST_ENTRIES,
    };
    use crate::models::{CookieConfig, DownloadRequest, PlaylistEntry};
    use crate::runtime::YtdlpCommandConfig;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[cfg(windows)]
    async fn spawn_powershell_fixture(job: &DownloadJob, script: &str) -> tokio::process::Child {
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .kill_on_drop(true)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        job.spawn(&mut command, "test fixture", false)
            .await
            .unwrap()
    }

    fn arg_value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    }

    fn download_request(format: &str, quality: &str) -> DownloadRequest {
        DownloadRequest {
            url: "https://example.com/video".into(),
            quality: quality.into(),
            format: format.into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        }
    }

    fn deterministic_runtime_config() -> YtdlpCommandConfig {
        YtdlpCommandConfig {
            ffmpeg_dir: Some(PathBuf::from("C:\\NuclearRuntime")),
            deno_path: Some(PathBuf::from("C:\\NuclearRuntime\\deno.exe")),
            plugin_dir: Some(PathBuf::from("C:\\NuclearRuntime\\plugins")),
        }
    }

    fn playlist_entry(index: usize, url: String) -> PlaylistEntry {
        PlaylistEntry {
            id: format!("video-{index}"),
            title: Some(format!("Video {index}")),
            duration: None,
            url,
            thumbnail: None,
        }
    }

    #[test]
    fn uses_default_template_without_override() {
        let request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "mp4".into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        };

        assert_eq!(
            build_output_template(&request),
            "C:/Users/Mr.W/Downloads/%(title)s [%(id)s].%(ext)s"
        );
    }

    #[test]
    fn uses_custom_filename_override_when_present() {
        let request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "mp4".into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: Some("My custom clip".into()),
            compat_config_path: None,
        };

        assert_eq!(
            build_output_template(&request),
            "C:/Users/Mr.W/Downloads/My custom clip.%(ext)s"
        );
    }

    #[test]
    fn sanitizes_invalid_filename_characters_and_percent_signs() {
        let request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "mp4".into(),
            output_dir: "C:\\Users\\Mr.W\\100%Downloads".into(),
            cookie_config: None,
            filename_override: Some("CON: 100%?".into()),
            compat_config_path: None,
        };

        assert_eq!(
            build_output_template(&request),
            "C:/Users/Mr.W/100%%Downloads/CON_ 100%%_.%(ext)s"
        );
    }

    #[test]
    fn normalizes_reserved_extensions_and_utf16_length() {
        assert_eq!(
            normalize_filename_override("NUL.txt"),
            Some("NUL_.txt".into())
        );
        assert_eq!(normalize_filename_override("clip.MP4"), Some("clip".into()));

        let long_name = format!("{}😀", "a".repeat(179));
        let normalized = normalize_filename_override(&long_name).expect("name should remain valid");
        assert_eq!(normalized.encode_utf16().count(), 179);
    }

    #[test]
    fn playlist_entries_are_deduplicated_and_bounded() {
        let mut entries = Vec::new();
        let mut seen_urls = HashSet::new();
        let mut parsed_count = 0;
        let mut truncated = false;

        push_bounded_playlist_entry(
            &mut entries,
            &mut seen_urls,
            &mut parsed_count,
            &mut truncated,
            playlist_entry(0, "https://example.com/video/0".into()),
        );
        push_bounded_playlist_entry(
            &mut entries,
            &mut seen_urls,
            &mut parsed_count,
            &mut truncated,
            playlist_entry(1, "https://example.com/video/0".into()),
        );
        assert_eq!(entries.len(), 1);

        entries.clear();
        seen_urls.clear();
        parsed_count = 0;
        truncated = false;
        for index in 0..=MAX_PLAYLIST_ENTRIES {
            push_bounded_playlist_entry(
                &mut entries,
                &mut seen_urls,
                &mut parsed_count,
                &mut truncated,
                playlist_entry(index, format!("https://example.com/video/{index}")),
            );
        }

        assert_eq!(entries.len(), MAX_PLAYLIST_ENTRIES);
        assert!(truncated);
    }

    #[tokio::test]
    async fn publishing_never_overwrites_and_adds_a_suffix() {
        let root =
            std::env::temp_dir().join(format!("nuclear-publish-test-{}", uuid::Uuid::new_v4()));
        let staging_dir = root.join("staging");
        let output_dir = root.join("output");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::create_dir_all(&output_dir).unwrap();

        let staged = staging_dir.join("Clip.mp4");
        let desired = output_dir.join("Clip.mp4");
        std::fs::write(&staged, b"new bytes").unwrap();
        std::fs::write(&desired, b"old bytes").unwrap();

        let published = publish_staged_output(&staged, &desired, None)
            .await
            .unwrap();
        assert_eq!(published, output_dir.join("Clip (2).mp4"));
        assert_eq!(std::fs::read(&desired).unwrap(), b"old bytes");
        assert_eq!(std::fs::read(&published).unwrap(), b"new bytes");

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn concurrent_publication_allocates_distinct_names() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-concurrent-publish-test-{}",
            uuid::Uuid::new_v4()
        ));
        let staging_dir = root.join("staging");
        let output_dir = root.join("output");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::create_dir_all(&output_dir).unwrap();

        let first_staged = staging_dir.join("first.mp4");
        let second_staged = staging_dir.join("second.mp4");
        let desired = output_dir.join("Clip.mp4");
        std::fs::write(&first_staged, b"first").unwrap();
        std::fs::write(&second_staged, b"second").unwrap();

        let (first, second) = tokio::join!(
            publish_staged_output(&first_staged, &desired, None),
            publish_staged_output(&second_staged, &desired, None),
        );
        let mut published = vec![first.unwrap(), second.unwrap()];
        published.sort();

        assert_eq!(
            published,
            vec![output_dir.join("Clip (2).mp4"), output_dir.join("Clip.mp4")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mp4_download_uses_compatible_selector_and_remuxes_final_video() {
        let request = download_request("mp4", "best");
        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo[ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mp4"));
        assert_eq!(arg_value_after(&args, "--remux-video"), Some("mp4"));
        assert!(arg_value_after(&args, "--recode-video").is_none());
    }

    #[test]
    fn mp4_quality_download_keeps_requested_height_before_remux() {
        let request = download_request("mp4", "720p");

        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo[height<=720][ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo[height<=720][ext=mp4]+bestaudio[ext=m4a]/best[height<=720][ext=mp4]/best[height<=720]")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mp4"));
        assert_eq!(arg_value_after(&args, "--remux-video"), Some("mp4"));
        assert!(arg_value_after(&args, "--recode-video").is_none());
    }

    #[test]
    fn download_args_include_deterministic_runtime_flags() {
        let mut request = download_request("mp4", "best");
        request.compat_config_path = Some("C:\\Users\\Mr.W\\yt-dlp-compat.conf".into());
        let runtime_config = deterministic_runtime_config();

        let args = build_download_args_with_runtime(&request, false, &runtime_config);

        assert!(args.iter().any(|arg| arg == "--ignore-config"));
        assert_eq!(
            arg_value_after(&args, "--config-locations"),
            Some("C:\\Users\\Mr.W\\yt-dlp-compat.conf")
        );
        assert!(args.iter().any(|arg| arg == "--no-plugin-dirs"));
        assert_eq!(
            arg_value_after(&args, "--plugin-dirs"),
            Some("C:\\NuclearRuntime\\plugins")
        );
        assert!(args.iter().any(|arg| arg == "--no-js-runtimes"));
        assert_eq!(
            arg_value_after(&args, "--js-runtimes"),
            Some("deno:C:\\NuclearRuntime\\deno.exe")
        );
        assert_eq!(
            arg_value_after(&args, "--ffmpeg-location"),
            Some("C:\\NuclearRuntime")
        );
    }

    #[test]
    fn mkv_download_remuxes_final_video_to_mkv() {
        let request = download_request("mkv", "best");

        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo+bestaudio/best")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mkv"));
        assert_eq!(arg_value_after(&args, "--remux-video"), Some("mkv"));
        assert!(arg_value_after(&args, "--recode-video").is_none());
    }

    #[test]
    fn mkv_quality_download_keeps_requested_height_before_remux() {
        let request = download_request("mkv", "720p");
        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo[height<=720]+bestaudio/best[height<=720]")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mkv"));
        assert_eq!(arg_value_after(&args, "--remux-video"), Some("mkv"));
    }

    #[test]
    fn webm_download_prefers_webm_streams_and_leaves_conversion_to_ffmpeg() {
        let request = download_request("webm", "best");
        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo[ext=webm]+bestaudio[ext=webm]/best[ext=webm]/bestvideo+bestaudio/best")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mkv"));
        assert!(arg_value_after(&args, "--recode-video").is_none());
        assert!(arg_value_after(&args, "--remux-video").is_none());
    }

    #[test]
    fn webm_quality_download_keeps_requested_height_before_ffmpeg_conversion() {
        let request = download_request("webm", "720p");
        let args = build_download_args(&request, false);

        assert_eq!(
            arg_value_after(&args, "-f"),
            Some("bestvideo[height<=720][ext=webm]+bestaudio[ext=webm]/best[height<=720][ext=webm]/bestvideo[height<=720]+bestaudio/best[height<=720]")
        );
        assert_eq!(arg_value_after(&args, "--merge-output-format"), Some("mkv"));
        assert!(arg_value_after(&args, "--recode-video").is_none());
    }

    #[test]
    fn every_audio_output_uses_extract_audio_conversion() {
        for format in ["mp3", "flac", "wav", "aac", "opus"] {
            let request = download_request(format, "best");
            let args = build_download_args(&request, false);

            assert!(args.iter().any(|arg| arg == "-x"));
            assert_eq!(arg_value_after(&args, "--audio-format"), Some(format));
            assert_eq!(arg_value_after(&args, "--audio-quality"), Some("0"));
            assert!(arg_value_after(&args, "--merge-output-format").is_none());
            assert!(arg_value_after(&args, "--recode-video").is_none());
            assert!(arg_value_after(&args, "--remux-video").is_none());
        }
    }

    #[test]
    fn parses_ffmpeg_progress_from_microseconds_and_timestamps() {
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time_us=5000000", 20.0),
            Some(25.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time_ms=10000000", 20.0),
            Some(50.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time=00:00:15.000000", 20.0),
            Some(75.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time=00:00:30.000000", 20.0),
            Some(100.0)
        );
    }

    #[test]
    fn rejects_invalid_ffmpeg_progress_inputs() {
        assert_eq!(
            parse_ffmpeg_progress_percent("progress=continue", 20.0),
            None
        );
        assert_eq!(parse_ffmpeg_progress_percent("out_time_us=N/A", 20.0), None);
        assert_eq!(parse_ffmpeg_progress_percent("out_time_us=1000", 0.0), None);
    }

    #[test]
    fn webm_final_path_uses_custom_filename_or_staged_stem() {
        let mut request = download_request("webm", "best");
        request.output_dir = "C:\\Users\\Mr.W\\Desktop".into();
        request.filename_override = Some("Clip: 100%?".into());

        assert_eq!(
            build_webm_final_path(&request, Path::new("C:\\Temp\\ignored.mkv")),
            PathBuf::from("C:\\Users\\Mr.W\\Desktop").join("Clip_ 100%_.webm")
        );

        request.filename_override = None;
        assert_eq!(
            build_webm_final_path(&request, Path::new("C:\\Temp\\Title [abc123].mkv")),
            PathBuf::from("C:\\Users\\Mr.W\\Desktop").join("Title [abc123].webm")
        );
    }

    #[test]
    fn abandoned_stage_cleanup_deletes_only_marker_owned_uuid_directories() {
        let output =
            std::env::temp_dir().join(format!("nuclear-stage-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).unwrap();
        let valid_id = uuid::Uuid::new_v4().to_string();
        let valid = build_staging_dir(&output, &valid_id);
        reset_staging_dir(&valid, &output, &valid_id).unwrap();
        std::fs::write(valid.join("partial.bin"), b"owned").unwrap();
        let unowned = output
            .join(super::STAGING_ROOT_NAME)
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&unowned).unwrap();
        std::fs::write(unowned.join("user-file.txt"), b"preserve").unwrap();

        let failures = cleanup_abandoned_download_stages(&[output.to_string_lossy().into_owned()]);

        assert!(failures.is_empty());
        assert!(!valid.exists());
        assert!(unowned.join("user-file.txt").is_file());
        let _ = std::fs::remove_dir_all(output);
    }

    #[test]
    fn active_stage_cleanup_refuses_a_wrong_operation_marker() {
        let output =
            std::env::temp_dir().join(format!("nuclear-stage-marker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).unwrap();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let stage = build_staging_dir(&output, &operation_id);
        reset_staging_dir(&stage, &output, &operation_id).unwrap();
        std::fs::write(stage.join("keep.txt"), b"preserve").unwrap();
        let wrong_marker = serde_json::json!({
            "schemaVersion": 1,
            "owner": "nuclear-downloader",
            "operationId": uuid::Uuid::new_v4().to_string(),
        });
        std::fs::write(
            stage.join(super::STAGING_MARKER_NAME),
            serde_json::to_vec(&wrong_marker).unwrap(),
        )
        .unwrap();

        assert!(cleanup_staging_dir(&stage, &output, &operation_id).is_err());
        assert!(stage.join("keep.txt").is_file());
        let _ = std::fs::remove_dir_all(output);
    }

    #[cfg(windows)]
    #[test]
    fn active_stage_reset_refuses_a_reparse_staging_root() {
        use std::os::windows::fs::symlink_dir;

        let base =
            std::env::temp_dir().join(format!("nuclear-stage-reparse-{}", uuid::Uuid::new_v4()));
        let output = base.join("output");
        let target = base.join("outside");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let root = output.join(super::STAGING_ROOT_NAME);
        if symlink_dir(&target, &root).is_err() {
            let _ = std::fs::remove_dir_all(base);
            return;
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let stage = root.join(&operation_id);

        assert!(reset_staging_dir(&stage, &output, &operation_id).is_err());
        assert!(target.read_dir().unwrap().next().is_none());

        let _ = std::fs::remove_dir(&root);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_non_http_download_urls() {
        let request = DownloadRequest {
            url: "file:///C:/Users/Mr.W/video.mp4".into(),
            quality: "best".into(),
            format: "mp4".into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        };

        assert!(validate_download_request(&request).is_err());
    }

    #[test]
    fn rejects_invalid_output_format() {
        let request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "avi".into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        };

        assert!(validate_download_request(&request).is_err());
    }

    #[test]
    fn rejects_cookie_file_mode_without_path() {
        let cookie_config = CookieConfig {
            enabled: true,
            mode: "file".into(),
            browser: "firefox".into(),
            cookie_file: Some("   ".into()),
        };

        assert!(
            validate_fetch_request("https://example.com/video", Some(&cookie_config), None)
                .is_err()
        );
    }

    #[test]
    fn rejects_missing_cookie_file() {
        let missing_path = std::env::temp_dir().join(format!(
            "nuclear-missing-cookie-{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let cookie_config = CookieConfig {
            enabled: true,
            mode: "file".into(),
            browser: "firefox".into(),
            cookie_file: Some(missing_path.to_string_lossy().to_string()),
        };

        assert!(
            validate_fetch_request("https://example.com/video", Some(&cookie_config), None)
                .is_err()
        );
    }

    #[test]
    fn classifies_youtube_runtime_and_format_errors() {
        let js = classify_process_error(
            "WARNING: [youtube] No supported JavaScript runtime could be found",
            Some(1),
            Some("mp4"),
        );
        assert_eq!(js.code, "youtube_missing_js_runtime");

        let bot = classify_process_error(
            "ERROR: [youtube] Sign in to confirm you're not a bot",
            Some(1),
            Some("mp4"),
        );
        assert_eq!(bot.code, "youtube_bot_verification");

        let format = classify_process_error(
            "ERROR: requested format is not available",
            Some(1),
            Some("mp4"),
        );
        assert_eq!(format.code, "format_unavailable");
        assert!(format.message.contains("MP4"));
    }

    #[test]
    fn keeps_only_https_thumbnail_urls() {
        assert_eq!(
            sanitize_thumbnail_url(Some("https://example.com/thumb.jpg")),
            Some("https://example.com/thumb.jpg".into())
        );
        assert_eq!(
            sanitize_thumbnail_url(Some("http://example.com/thumb.jpg")),
            None
        );
        assert_eq!(sanitize_thumbnail_url(Some("file:///C:/thumb.jpg")), None);
    }

    #[test]
    fn playlist_line_prefers_last_thumbnail_and_keeps_metadata_hints() {
        let line = serde_json::from_str::<PlaylistLineRecord>(
            r#"{
                "id":"abc123",
                "title":"Example Clip",
                "duration":42,
                "url":"https://example.com/watch/abc123",
                "thumbnail":"http://example.com/thumb-low.jpg",
                "thumbnails":[
                    {"url":"http://example.com/thumb-low.jpg"},
                    {"url":"https://example.com/thumb-hi.jpg"}
                ],
                "playlist_title":"Example Playlist",
                "playlist_uploader":"Example Channel"
            }"#,
        )
        .unwrap();

        assert_eq!(line.playlist_title_hint(), Some("Example Playlist"));
        assert_eq!(line.playlist_channel_hint(), Some("Example Channel"));

        let entry = line.into_playlist_entry().unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.title.as_deref(), Some("Example Clip"));
        assert_eq!(entry.duration, Some(42.0));
        assert_eq!(entry.url, "https://example.com/watch/abc123");
        assert_eq!(
            entry.thumbnail.as_deref(),
            Some("https://example.com/thumb-hi.jpg")
        );
    }

    #[test]
    fn youtube_playlist_line_falls_back_to_watch_url_when_missing_urls() {
        let line = serde_json::from_str::<PlaylistLineRecord>(
            r#"{"id":"fallback-id","title":"Fallback","extractor_key":"Youtube"}"#,
        )
        .unwrap();

        let entry = line.into_playlist_entry().unwrap();
        assert_eq!(entry.url, "https://www.youtube.com/watch?v=fallback-id");
    }

    #[test]
    fn generic_playlist_line_without_a_url_is_rejected() {
        let line = serde_json::from_str::<PlaylistLineRecord>(
            r#"{"id":"opaque-id","title":"Unavailable","extractor_key":"Generic"}"#,
        )
        .unwrap();

        assert!(line.into_playlist_entry().is_none());
    }

    #[test]
    fn capped_video_selectors_never_contain_an_uncapped_fallback() {
        for format in ["mp4", "mkv", "webm"] {
            let request = download_request(format, "720p");
            let args = build_download_args(&request, false);
            let selector = arg_value_after(&args, "-f").unwrap();

            assert!(selector
                .split('/')
                .all(|branch| branch.contains("height<=720")));
        }
    }

    #[test]
    fn build_error_message_skips_drm_warning_when_real_error_exists() {
        let stderr = "\
[hls @ 000001] DRM protected stream detected, decoding will likely fail!\n\
ERROR: unable to open segment 3\n\
ffmpeg exited with code 1";

        assert_eq!(
            build_error_message(stderr, Some(1)),
            "ERROR: unable to open segment 3 | ffmpeg exited with code 1"
        );
    }

    #[test]
    fn build_error_message_keeps_drm_warning_when_it_is_all_we_have() {
        let stderr = "[hls @ 000001] DRM protected stream detected, decoding will likely fail!";

        assert_eq!(
            build_error_message(stderr, Some(1)),
            "[hls @ 000001] DRM protected stream detected, decoding will likely fail!"
        );
    }

    #[test]
    fn parses_first_json_value_from_multiple_documents() {
        let value = parse_first_json_value("{\"id\":\"one\"}\n{\"id\":\"two\"}").unwrap();
        assert_eq!(value["id"].as_str(), Some("one"));
    }

    #[test]
    fn identifies_x_and_twitter_hosts() {
        assert!(is_x_or_twitter_url("https://x.com/user/status/1"));
        assert!(is_x_or_twitter_url("https://twitter.com/user/status/1"));
        assert!(is_x_or_twitter_url(
            "https://mobile.twitter.com/user/status/1"
        ));
        assert!(!is_x_or_twitter_url("https://example.com/video"));
    }

    #[test]
    fn detects_twitter_guest_auth_failures() {
        assert!(is_twitter_api_auth_error(
            "ERROR: [twitter] 12345: Failed to query API: Bad guest token"
        ));
        assert!(should_retry_with_twitter_syndication(
            "https://x.com/user/status/1",
            "ERROR: [twitter] 12345: Failed to query API: Bad guest token"
        ));
        assert!(!should_retry_with_twitter_syndication(
            "https://example.com/video",
            "ERROR: [twitter] 12345: Failed to query API: Bad guest token"
        ));
    }

    #[test]
    fn retries_x_missing_video_errors_with_syndication() {
        assert!(is_twitter_missing_video_error(
            "ERROR: [twitter] 12345: No video could be found in this post"
        ));
        assert!(should_retry_with_twitter_syndication(
            "https://x.com/user/status/1",
            "ERROR: [twitter] 12345: No video could be found in this post"
        ));
        assert!(should_retry_with_twitter_syndication(
            "https://x.com/user/status/1",
            "ERROR: requested format is not available"
        ));
        assert!(!should_retry_with_twitter_syndication(
            "https://example.com/video",
            "ERROR: requested format is not available"
        ));
    }

    #[tokio::test]
    async fn download_manager_cancellation_is_idempotent_before_spawn() {
        let manager = DownloadManager::new(5, 1, 1);
        let job = manager.register("cancel-before-spawn").await.unwrap();

        manager.cancel("cancel-before-spawn").await.unwrap();
        manager.cancel("cancel-before-spawn").await.unwrap();
        assert!(manager.cancel("already-finished").await.is_err());

        assert!(job.is_cancelled());
        manager.finish("cancel-before-spawn").await;
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn download_manager_rejects_duplicate_ids_and_active_maintenance() {
        let manager = DownloadManager::new(5, 1, 1);
        manager.register("same-id").await.unwrap();

        assert!(manager.register("same-id").await.is_err());
        assert!(manager.acquire_maintenance().await.is_err());

        manager.finish("same-id").await;
        let lease = manager.acquire_maintenance().await.unwrap();
        assert!(manager.register("paused").await.is_err());
        lease.release().await;
        assert!(manager.register("resumed").await.is_ok());
        manager.finish("resumed").await;
    }

    #[tokio::test]
    async fn webm_conversion_slots_apply_backpressure() {
        let manager = DownloadManager::new(5, 1, 1);
        let first_job = manager.register("first").await.unwrap();
        let second_job = manager.register("second").await.unwrap();
        let first_permit = manager
            .acquire_conversion(&first_job)
            .await
            .unwrap()
            .unwrap();

        assert!(tokio::time::timeout(
            Duration::from_millis(25),
            manager.acquire_conversion(&second_job),
        )
        .await
        .is_err());

        drop(first_permit);
        assert!(manager
            .acquire_conversion(&second_job)
            .await
            .unwrap()
            .is_some());
        manager.finish("first").await;
        manager.finish("second").await;
    }

    #[tokio::test]
    async fn download_scheduler_exposes_exactly_five_concurrent_slots() {
        let manager = DownloadManager::new(5, 1, 1);
        let mut permits = Vec::new();
        for _ in 0..5 {
            permits.push(manager.acquire_download_slot().await.unwrap());
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(25), manager.acquire_download_slot(),)
                .await
                .is_err()
        );

        permits.pop();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), manager.acquire_download_slot(),)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn cancel_all_cannot_release_an_update_maintenance_lease() {
        let manager = DownloadManager::new(5, 1, 1);
        let lease = manager.acquire_maintenance().await.unwrap();

        let error = manager.begin_cancel_all().await.unwrap_err();
        assert_eq!(error.code, "busy");
        assert!(manager.register("must-remain-paused").await.is_err());

        lease.release().await;
        assert!(manager.register("after-update").await.is_ok());
        manager.finish("after-update").await;
    }

    #[test]
    fn streamed_inspection_enforces_one_cumulative_output_limit() {
        let legal_line_bytes = 60 * 1024;
        let mut total = 0usize;
        let legal_lines = MAX_INSPECTION_OUTPUT_BYTES / (legal_line_bytes + 1);

        for _ in 0..legal_lines {
            super::record_streamed_output_bytes(
                &mut total,
                legal_line_bytes,
                MAX_INSPECTION_OUTPUT_BYTES,
            )
            .unwrap();
        }
        let error = super::record_streamed_output_bytes(
            &mut total,
            legal_line_bytes,
            MAX_INSPECTION_OUTPUT_BYTES,
        )
        .unwrap_err();

        assert!(error.starts_with("process_output_limit:"), "{error}");
    }

    #[tokio::test]
    async fn cancel_all_timeout_reports_ids_and_keeps_manager_paused() {
        let manager = DownloadManager::new(5, 1, 1);
        manager.register("still-reaping").await.unwrap();

        manager.begin_cancel_all().await.unwrap();
        assert!(manager
            .finish_cancel_all(Duration::from_millis(10))
            .await
            .is_err());
        assert_eq!(manager.active_ids().await, vec!["still-reaping"]);
        assert!(manager.register("new-work").await.is_err());

        manager.finish("still-reaping").await;
        manager.begin_cancel_all().await.unwrap();
        assert!(manager
            .finish_cancel_all(Duration::from_millis(10))
            .await
            .is_ok());
        assert!(manager.register("resumed").await.is_ok());
        manager.finish("resumed").await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_cancels_and_reaps_promptly() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(&job, "Start-Sleep -Seconds 30").await;
        let cancellation = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancellation.cancel();
        });

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(500),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_cancelled:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelled_job_never_runs_suspended_child_first_instruction() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-suspended-child-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sentinel = root.join("child-ran.txt");
        let escaped = sentinel.to_string_lossy().replace('\'', "''");
        let script = format!("[IO.File]::WriteAllText('{escaped}', 'ran')");
        let job = DownloadJob::new().unwrap();
        job.cancel();

        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .kill_on_drop(true)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let error = job
            .spawn(&mut command, "cancelled fixture", false)
            .await
            .unwrap_err();

        assert!(error.contains("cancelled"), "{error}");
        assert!(!sentinel.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_stops_oversized_output_without_draining_producer() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(
            &job,
            "[Console]::Out.Write('x' * 70000); Start-Sleep -Seconds 30",
        )
        .await;

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            128 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(500),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_output_limit:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_bounds_inherited_stderr_after_parent_exit() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(
            &job,
            "Start-Process powershell.exe -ArgumentList '-NoLogo -NoProfile -NonInteractive -Command Start-Sleep -Seconds 30' -NoNewWindow",
        )
        .await;

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(250),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_drain_timeout:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
