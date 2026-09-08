pub(crate) mod inspection;
pub(crate) mod process;
pub(crate) mod publication;

use crate::app_error::AppError;
use crate::lifecycle::DownloadManager;
use crate::models::{CookieConfig, DownloadProgress, DownloadRequest};
use crate::runtime::{self, YtdlpCommandConfig};
use process::{
    wait_with_bounded_output, wait_with_streamed_stdout, DownloadJob, MAX_PROCESS_LINE_BYTES,
    MAX_STDERR_BYTES,
};
use publication::{
    append_final_output_record_args, build_final_output_path, build_staged_webm_output_path,
    build_staging_dir, build_webm_final_path, cleanup_staging_with_warning, path_to_string,
    publish_staged_output, reset_staging_dir, resolve_staged_output,
};
use regex::Regex;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use url::Url;

const MAX_CUSTOM_FILENAME_UTF16_UNITS: usize = 180;
pub(super) const MAX_ACTIONABLE_FIELD_BYTES: usize = 4 * 1024;
const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
const AUDIO_FORMATS: &[&str] = &["mp3", "flac", "wav", "aac", "opus"];
const COOKIE_BROWSERS: &[&str] = &["firefox", "chrome", "edge", "brave", "opera", "chromium"];

static DOWNLOAD_PROGRESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\]\s+([\d.]+)%\s+of").unwrap());
static DOWNLOAD_SPEED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"at\s+([\d.]+\w+/s)").unwrap());
static DOWNLOAD_ETA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ETA\s+(\S+)").unwrap());
static DOWNLOAD_MERGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[Merger\]|\[VideoConvertor\]|\[VideoRemuxer\]|\[ExtractAudio\]|post-?process|converting|remuxing").unwrap()
});
static QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{3,4}p$").unwrap());

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadOutcome {
    Completed {
        filename: Option<String>,
    },
    Cancelled,
    Failed {
        code: String,
        message: String,
        detail: String,
    },
}

impl From<DownloadErrorInfo> for DownloadOutcome {
    fn from(error: DownloadErrorInfo) -> Self {
        Self::Failed {
            code: error.code,
            message: error.message,
            detail: error.detail,
        }
    }
}

async fn emit_progress(
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
    if !crate::record_download_progress(app, &event).await {
        return;
    }
    if let Err(error) = app.emit("download-progress", &event) {
        crate::record_event_delivery_failure(app, "download-progress", &error.to_string());
    }
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
    validate_actionable_input("URL", url)?;
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

    validate_actionable_input("output format", &request.format)?;
    validate_actionable_input("quality", &request.quality)?;
    validate_actionable_input("output folder", &request.output_dir)?;
    if let Some(filename) = request.filename_override.as_deref() {
        validate_actionable_input("custom filename", filename)?;
    }

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
    validate_actionable_input("output folder", path).map_err(AppError::invalid)?;
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

fn validate_cookie_config(config: &CookieConfig) -> Result<(), String> {
    validate_actionable_input("cookie mode", &config.mode)?;
    validate_actionable_input("cookie browser", &config.browser)?;
    if let Some(path) = config.cookie_file.as_deref() {
        validate_actionable_input("cookie file path", path)?;
    }
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
    if let Some(path) = path {
        validate_actionable_input("compatibility config path", path)?;
    }
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(());
    };

    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err("Compatibility config file was not found.".into())
    }
}

fn validate_actionable_input(name: &str, value: &str) -> Result<(), String> {
    if value.len() > MAX_ACTIONABLE_FIELD_BYTES {
        Err(format!("The {name} exceeds the 4 KiB input limit."))
    } else {
        Ok(())
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
    append_final_output_record_args(&mut args, Path::new(&request.output_dir));

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

fn terminal_outcome_from_attempt(result: DownloadAttemptResult) -> DownloadOutcome {
    match result {
        DownloadAttemptResult::Completed(filename) => DownloadOutcome::Completed { filename },
        DownloadAttemptResult::Cancelled => DownloadOutcome::Cancelled,
        DownloadAttemptResult::Error(error) => error.into(),
        DownloadAttemptResult::RetryWithTwitterSyndication => simple_error(
            "download_failed",
            "Download failed before retry could complete.",
        )
        .into(),
    }
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

    let child = match job.spawn(&mut cmd, "yt-dlp", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start yt-dlp: {}", error),
            ));
        }
    };

    let output = match wait_with_streamed_stdout(child, job, "yt-dlp", |line| {
        let update = if let Some(caps) = DOWNLOAD_PROGRESS_RE.captures(&line) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            let speed = DOWNLOAD_SPEED_RE.captures(&line).map(|c| c[1].to_string());
            let eta = DOWNLOAD_ETA_RE.captures(&line).map(|c| c[1].to_string());
            Some((
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
            ))
        } else if DOWNLOAD_MERGE_RE.is_match(&line) {
            Some((
                "postprocessing",
                100.0,
                ProgressFields {
                    filename: None,
                    phase: Some("postprocess"),
                    download_progress: Some(100.0),
                    ..Default::default()
                },
            ))
        } else {
            None
        };

        async move {
            if let Some((status, progress, fields)) = update {
                emit_progress(app, download_id, status, progress, fields).await;
            }
            Ok(())
        }
    })
    .await
    {
        Ok(output) => output,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };

    if output.cancelled || job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    match output.status {
        s if s.success() => DownloadAttemptResult::Completed(None),
        s => {
            if !use_twitter_syndication
                && should_retry_with_twitter_syndication(&request.url, &output.stderr)
            {
                DownloadAttemptResult::RetryWithTwitterSyndication
            } else {
                DownloadAttemptResult::Error(classify_process_error(
                    &output.stderr,
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
    )
    .await;

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

    let child = match job.spawn(&mut cmd, "ffmpeg", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start ffmpeg: {error}"),
            ));
        }
    };

    let mut last_progress: f64 = 0.0;
    let output = match wait_with_streamed_stdout(child, job, "FFmpeg", |line| {
        let progress = if line.trim() == "progress=end" {
            Some(100.0)
        } else {
            parse_ffmpeg_progress_percent(&line, duration_seconds)
        };
        let update = progress.map(|progress| {
            last_progress = last_progress.max(progress);
            last_progress
        });

        async move {
            if let Some(progress) = update {
                emit_progress(
                    app,
                    download_id,
                    "postprocessing",
                    progress,
                    ProgressFields {
                        filename: None,
                        phase: Some("conversion"),
                        download_progress: Some(100.0),
                        conversion_progress: Some(progress),
                        ..Default::default()
                    },
                )
                .await;
            }
            Ok(())
        }
    })
    .await
    {
        Ok(output) => output,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };

    if output.cancelled || job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    match output.status {
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
            &output.stderr,
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
            DownloadAttemptResult::Completed(_) => {
                let intermediate_path = match resolve_staged_output(&staging_dir) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(app, &staging_dir, output_dir, download_id);
                        return DownloadAttemptResult::Error(simple_error(
                            error.code,
                            error.message,
                        ));
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
                )
                .await;
                let _conversion_permit = match manager.acquire_conversion(job).await {
                    Ok(Some(permit)) => permit,
                    Ok(None) => {
                        cleanup_staging_with_warning(app, &staging_dir, output_dir, download_id);
                        return DownloadAttemptResult::Cancelled;
                    }
                    Err(error) => {
                        cleanup_staging_with_warning(app, &staging_dir, output_dir, download_id);
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

                cleanup_staging_with_warning(app, &staging_dir, output_dir, download_id);
                return result;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                use_twitter_syndication = true;
            }
            other => {
                cleanup_staging_with_warning(app, &staging_dir, output_dir, download_id);
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
) -> DownloadOutcome {
    if job.is_cancelled() {
        return DownloadOutcome::Cancelled;
    }

    if let Err(error) = validate_download_request(&request) {
        return simple_error("invalid_request", error).into();
    }

    match validate_output_directory(&request.output_dir) {
        Ok(output_dir) => request.output_dir = output_dir,
        Err(error) => {
            return DownloadOutcome::Failed {
                code: error.code,
                message: error.summary,
                detail: error.detail.unwrap_or_default(),
            };
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
    )
    .await;

    if request.format == "webm" {
        return terminal_outcome_from_attempt(
            run_webm_download(&app, &download_id, &request, &manager, &job).await,
        );
    }

    let output_dir = Path::new(&request.output_dir);
    let staging_dir = build_staging_dir(output_dir, &download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir, output_dir, &download_id) {
            return simple_error("staging_failed", error).into();
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
            DownloadAttemptResult::Completed(_) => {
                let staged_path = match resolve_staged_output(&staging_dir) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                        return simple_error(error.code, error.message).into();
                    }
                };

                let desired_path = match build_final_output_path(&request, &staged_path) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                        return simple_error("invalid_filename", error).into();
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
                )
                .await;

                let outcome =
                    match publish_staged_output(&staged_path, &desired_path, Some(&job)).await {
                        Ok(published_path) => DownloadOutcome::Completed {
                            filename: Some(path_to_string(&published_path)),
                        },
                        Err(_) if job.is_cancelled() => DownloadOutcome::Cancelled,
                        Err(error) => simple_error("publish_failed", error).into(),
                    };
                cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                return outcome;
            }
            DownloadAttemptResult::Cancelled => {
                cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                return DownloadOutcome::Cancelled;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                use_twitter_syndication = true;
            }
            DownloadAttemptResult::Error(error) => {
                cleanup_staging_with_warning(&app, &staging_dir, output_dir, &download_id);
                return error.into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_download_args, build_download_args_with_runtime, build_error_message,
        build_output_template, classify_process_error, is_twitter_api_auth_error,
        is_twitter_missing_video_error, is_x_or_twitter_url, normalize_filename_override,
        parse_ffmpeg_progress_percent, should_retry_with_twitter_syndication,
        validate_download_request, validate_fetch_request, DownloadAttemptResult,
        DownloadErrorInfo, DownloadOutcome, MAX_ACTIONABLE_FIELD_BYTES,
    };
    use crate::models::{CookieConfig, DownloadRequest};
    use crate::runtime::YtdlpCommandConfig;
    use std::path::PathBuf;

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

    #[test]
    fn terminal_outcome_preserves_published_filename() {
        let filename = r"C:\Users\Mr.W\Downloads\clip (2).mp4".to_string();

        let outcome = super::terminal_outcome_from_attempt(DownloadAttemptResult::Completed(Some(
            filename.clone(),
        )));

        assert_eq!(
            outcome,
            DownloadOutcome::Completed {
                filename: Some(filename)
            }
        );
    }

    #[test]
    fn terminal_outcome_preserves_structured_failure() {
        let error = DownloadErrorInfo {
            code: "publish_failed".to_string(),
            message: "Could not publish the download.".to_string(),
            detail: "The destination volume rejected the atomic move.".to_string(),
        };

        let outcome = super::terminal_outcome_from_attempt(DownloadAttemptResult::Error(error));

        assert_eq!(
            outcome,
            DownloadOutcome::Failed {
                code: "publish_failed".to_string(),
                message: "Could not publish the download.".to_string(),
                detail: "The destination volume rejected the atomic move.".to_string(),
            }
        );
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
        assert_eq!(
            arg_value_after(&args, "--print-to-file"),
            Some(r#"after_move:{"schema_version":1,"filepath":%(filepath)j}"#)
        );
        assert!(args.iter().any(|argument| {
            argument.ends_with("/.nuclear-final-output-v1.jsonl")
                && argument.starts_with("C:/Users/Mr.W/Downloads")
        }));
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
    fn rejects_actionable_request_fields_over_four_kibibytes() {
        let oversized = "x".repeat(MAX_ACTIONABLE_FIELD_BYTES + 1);
        let mut request = download_request("mp4", "best");

        request.url = format!("https://example.com/{oversized}");
        assert!(validate_download_request(&request)
            .unwrap_err()
            .contains("URL exceeds the 4 KiB"));

        request.url = "https://example.com/video".to_string();
        request.output_dir.clone_from(&oversized);
        assert!(validate_download_request(&request)
            .unwrap_err()
            .contains("output folder exceeds the 4 KiB"));

        request.output_dir = "C:\\Downloads".to_string();
        request.filename_override = Some(oversized.clone());
        assert!(validate_download_request(&request)
            .unwrap_err()
            .contains("custom filename exceeds the 4 KiB"));

        request.filename_override = None;
        request.cookie_config = Some(CookieConfig {
            enabled: false,
            mode: oversized.clone(),
            browser: String::new(),
            cookie_file: None,
        });
        assert!(validate_download_request(&request)
            .unwrap_err()
            .contains("cookie mode exceeds the 4 KiB"));

        request.cookie_config = None;
        request.compat_config_path = Some(oversized);
        assert!(validate_download_request(&request)
            .unwrap_err()
            .contains("compatibility config path exceeds the 4 KiB"));
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
}
