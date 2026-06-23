use crate::models::{
    CookieConfig, DownloadProgress, DownloadRequest, PlaylistEntry, PlaylistInfo, VideoInfo,
};
use crate::runtime::{self, YtdlpCommandConfig};
use regex::Regex;
use serde::Deserialize;
use serde_json::Deserializer;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use url::Url;

const MAX_STDERR_LINES: usize = 32;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
const AUDIO_FORMATS: &[&str] = &["mp3", "flac", "wav", "aac", "opus"];
const COOKIE_BROWSERS: &[&str] = &["firefox", "chrome", "edge", "brave", "opera", "chromium"];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
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
        r#"(?i)\[(?:Merger|VideoConvertor|VideoRemuxer)\].*(?:into|Destination:)\s+"?(.+?)"?\s*$"#,
    )
    .unwrap()
});
static DOWNLOAD_MERGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[Merger\]|\[VideoConvertor\]|\[VideoRemuxer\]|\[ExtractAudio\]|post-?process|converting|remuxing").unwrap()
});
static QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{3,4}p$").unwrap());

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

    fn into_playlist_entry(self) -> PlaylistEntry {
        let thumbnail = sanitize_thumbnail_url(self.preferred_thumbnail_url());
        let PlaylistLineRecord {
            id,
            title,
            duration,
            url,
            webpage_url,
            ..
        } = self;
        let id = id.unwrap_or_else(|| "unknown".to_string());
        let video_url = url
            .filter(|value| is_allowed_download_url(value))
            .or_else(|| webpage_url.filter(|value| is_allowed_download_url(value)))
            .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));

        PlaylistEntry {
            id,
            title,
            duration,
            url: video_url,
            thumbnail,
        }
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
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
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
        },
    );
}

pub type ActiveDownloads = Arc<Mutex<HashMap<String, tokio::process::Child>>>;

pub fn create_active_downloads() -> ActiveDownloads {
    Arc::new(Mutex::new(HashMap::new()))
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

    Ok(())
}

/// Resolve a binary name to the bundled sidecar path if it exists,
/// otherwise fall back to system PATH (for dev mode).
pub fn resolve_bin(name: &str) -> PathBuf {
    runtime::resolve_bin(name)
}

fn ytdlp_bin() -> PathBuf {
    resolve_bin("yt-dlp")
}

fn ffmpeg_bin() -> PathBuf {
    resolve_bin("ffmpeg")
}

fn ffprobe_bin() -> PathBuf {
    resolve_bin("ffprobe")
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
fn hidden_process_flags() -> u32 {
    CREATE_NO_WINDOW
}

#[cfg(windows)]
fn download_process_flags() -> u32 {
    CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
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
    stderr: tokio::process::ChildStderr,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut tail = TailBuffer::new();

        while let Ok(Some(line)) = lines.next_line().await {
            tail.push(line);
        }

        tail.into_string()
    })
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

    let reserved_name = cleaned.to_ascii_uppercase();
    if matches!(
        reserved_name.as_str(),
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
    ) {
        cleaned.push('_');
    }

    Some(cleaned)
}

fn escape_output_template_literal(value: &str) -> String {
    value.replace('%', "%%")
}

fn build_output_template(request: &DownloadRequest) -> String {
    let output_dir = escape_output_template_literal(&request.output_dir.replace('\\', "/"));

    if let Some(filename_override) = request
        .filename_override
        .as_deref()
        .and_then(sanitize_filename_component)
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
            format!("bestvideo[height<={height}]+bestaudio/best[height<={height}]/bestvideo+bestaudio/best")
        }
        ("webm", None) => {
            "bestvideo[ext=webm]+bestaudio[ext=webm]/best[ext=webm]/bestvideo+bestaudio/best"
                .to_string()
        }
        ("webm", Some(height)) => format!(
            "bestvideo[height<={height}][ext=webm]+bestaudio[ext=webm]/best[height<={height}][ext=webm]/bestvideo[height<={height}]+bestaudio/best[height<={height}]/bestvideo+bestaudio/best"
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
) -> Result<std::process::Output, String> {
    let bin = ytdlp_bin();
    let runtime_config = runtime::ytdlp_command_config();
    let mut args = Vec::new();
    append_ytdlp_runtime_args(&mut args, &runtime_config, compat_config_path);
    args.extend([
        "--dump-single-json".to_string(),
        "--no-download".to_string(),
        "--no-playlist".to_string(),
    ]);

    append_twitter_syndication_args(&mut args, url, use_twitter_syndication);

    if let Some(config) = cookie_config {
        append_cookie_args(&mut args, config);
    }

    args.push(url.to_string());

    let mut cmd = Command::new(&bin);
    cmd.args(&args);

    #[cfg(windows)]
    cmd.creation_flags(hidden_process_flags());

    cmd.output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}. Is yt-dlp installed?", e))
}

pub async fn fetch_info(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
) -> Result<VideoInfo, String> {
    validate_fetch_request(url, cookie_config, compat_config_path)?;

    let mut output = run_fetch_info_command(url, cookie_config, compat_config_path, false).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if should_retry_with_twitter_syndication(url, &stderr) {
            output = run_fetch_info_command(url, cookie_config, compat_config_path, true).await?;
        }
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(error_for_fetch(&stderr, output.status.code()));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let data = parse_first_json_value(&json_str)?;

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

pub async fn fetch_playlist(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
) -> Result<PlaylistInfo, String> {
    validate_fetch_request(url, cookie_config, compat_config_path)?;

    let bin = ytdlp_bin();
    let runtime_config = runtime::ytdlp_command_config();
    let mut args = Vec::new();
    append_ytdlp_runtime_args(&mut args, &runtime_config, compat_config_path);
    args.extend([
        "--flat-playlist".to_string(),
        "--dump-json".to_string(),
        "--lazy-playlist".to_string(),
        "--no-download".to_string(),
        url.to_string(),
    ]);
    let mut cmd = Command::new(&bin);
    cmd.args(&args);
    configure_cookie_args(&mut cmd, cookie_config);

    #[cfg(windows)]
    cmd.creation_flags(hidden_process_flags());

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture yt-dlp playlist output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture yt-dlp playlist errors.".to_string())?;
    let stderr_handle = spawn_stderr_tail_reader(stderr);
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let mut entries: Vec<PlaylistEntry> = Vec::new();
    let mut playlist_title = String::from("Playlist");
    let mut playlist_channel: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
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

        entries.push(data.into_playlist_entry());
    }

    let stderr_output = stderr_handle.await.unwrap_or_default();
    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for yt-dlp: {}", e))?;

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
        entries,
    })
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

fn build_download_args(request: &DownloadRequest, use_twitter_syndication: bool) -> Vec<String> {
    let runtime_config = runtime::ytdlp_command_config();
    build_download_args_with_runtime(request, use_twitter_syndication, &runtime_config)
}

fn staging_root() -> PathBuf {
    std::env::temp_dir()
        .join("nuclear-downloader")
        .join("downloads")
}

fn build_staging_dir(download_id: &str) -> PathBuf {
    let safe_id =
        sanitize_filename_component(download_id).unwrap_or_else(|| "download".to_string());
    staging_root().join(safe_id)
}

fn is_safe_staging_dir(path: &Path) -> bool {
    path.starts_with(staging_root())
}

fn reset_staging_dir(path: &Path) -> Result<(), String> {
    if !is_safe_staging_dir(path) {
        return Err("Refusing to use unsafe staging folder.".into());
    }

    if path.exists() {
        std::fs::remove_dir_all(path)
            .map_err(|error| format!("Failed to reset staging folder: {error}"))?;
    }

    std::fs::create_dir_all(path)
        .map_err(|error| format!("Failed to create staging folder: {error}"))
}

fn cleanup_staging_dir(path: &Path) {
    if is_safe_staging_dir(path) {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn build_webm_final_path(request: &DownloadRequest, intermediate_path: &Path) -> PathBuf {
    let filename = request
        .filename_override
        .as_deref()
        .and_then(sanitize_filename_component)
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

fn build_publish_temp_path(final_path: &Path) -> PathBuf {
    let stem = final_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(sanitize_filename_component)
        .unwrap_or_else(|| "download".to_string());
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    final_path.with_file_name(format!(".{stem}.nuclear-publish-{unique}.webm"))
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

        if matches!(extension.as_str(), "part" | "ytdl" | "temp") {
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

async fn probe_media_duration_seconds(path: &Path) -> Result<f64, String> {
    let mut cmd = Command::new(ffprobe_bin());
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ]);
    cmd.arg(path);

    #[cfg(windows)]
    cmd.creation_flags(hidden_process_flags());

    let output = cmd
        .output()
        .await
        .map_err(|error| format!("Failed to run ffprobe: {error}"))?;

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

async fn publish_converted_output(staged_output: &Path, final_path: &Path) -> Result<(), String> {
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Failed to create output folder: {error}"))?;
    }

    match tokio::fs::rename(staged_output, final_path).await {
        Ok(()) => Ok(()),
        Err(_) => {
            let publish_temp = build_publish_temp_path(final_path);
            tokio::fs::copy(staged_output, &publish_temp)
                .await
                .map_err(|error| format!("Failed to stage converted output: {error}"))?;

            if final_path.exists() {
                if let Err(error) = tokio::fs::remove_file(final_path).await {
                    let _ = tokio::fs::remove_file(&publish_temp).await;
                    return Err(format!("Failed to replace existing output file: {error}"));
                }
            }

            if let Err(error) = tokio::fs::rename(&publish_temp, final_path).await {
                let _ = tokio::fs::remove_file(&publish_temp).await;
                return Err(format!("Failed to publish converted output: {error}"));
            }

            let _ = tokio::fs::remove_file(staged_output).await;
            Ok(())
        }
    }
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
    active: ActiveDownloads,
    use_twitter_syndication: bool,
) -> DownloadAttemptResult {
    let args = build_download_args(request, use_twitter_syndication);

    let bin = ytdlp_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(download_process_flags());

    let mut child = match cmd.spawn() {
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

    {
        let mut downloads = active.lock().await;
        downloads.insert(download_id.to_string(), child);
    }

    let stderr_handle = spawn_stderr_tail_reader(stderr);
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut last_filename: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
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
                    filename: last_filename.clone(),
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
                    filename: last_filename.clone(),
                    phase: Some("postprocess"),
                    download_progress: Some(100.0),
                    ..Default::default()
                },
            );
        }
    }

    let stderr_output = stderr_handle.await.unwrap_or_default();

    let maybe_child = {
        let mut downloads = active.lock().await;
        downloads.remove(download_id)
    };

    let status = if let Some(mut child) = maybe_child {
        child.wait().await.ok()
    } else {
        return DownloadAttemptResult::Cancelled;
    };

    match status {
        Some(s) if s.success() => DownloadAttemptResult::Completed(last_filename),
        Some(s) => {
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
        None => DownloadAttemptResult::Error(simple_error(
            "process_terminated",
            "Process terminated unexpectedly",
        )),
    }
}

async fn run_webm_conversion(
    app: &AppHandle,
    download_id: &str,
    input_path: &Path,
    staged_output: &Path,
    final_path: &Path,
    active: ActiveDownloads,
) -> DownloadAttemptResult {
    let duration_seconds = match probe_media_duration_seconds(input_path).await {
        Ok(duration) => duration,
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
            filename: Some(path_to_string(final_path)),
            phase: Some("conversion"),
            download_progress: Some(100.0),
            conversion_progress: Some(0.0),
            ..Default::default()
        },
    );

    let mut cmd = Command::new(ffmpeg_bin());
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

    #[cfg(windows)]
    cmd.creation_flags(download_process_flags());

    let mut child = match cmd.spawn() {
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

    {
        let mut downloads = active.lock().await;
        downloads.insert(download_id.to_string(), child);
    }

    let stderr_handle = spawn_stderr_tail_reader(stderr);
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut last_progress: f64 = 0.0;

    while let Ok(Some(line)) = lines.next_line().await {
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
                filename: Some(path_to_string(final_path)),
                phase: Some("conversion"),
                download_progress: Some(100.0),
                conversion_progress: Some(last_progress),
                ..Default::default()
            },
        );
    }

    let stderr_output = stderr_handle.await.unwrap_or_default();

    let maybe_child = {
        let mut downloads = active.lock().await;
        downloads.remove(download_id)
    };

    let status = if let Some(mut child) = maybe_child {
        child.wait().await.ok()
    } else {
        return DownloadAttemptResult::Cancelled;
    };

    match status {
        Some(status) if status.success() => {
            if let Err(error) = publish_converted_output(staged_output, final_path).await {
                DownloadAttemptResult::Error(simple_error("postprocess_failed", error))
            } else {
                DownloadAttemptResult::Completed(Some(path_to_string(final_path)))
            }
        }
        Some(status) => DownloadAttemptResult::Error(classify_process_error(
            &stderr_output,
            status.code(),
            Some("webm"),
        )),
        None => DownloadAttemptResult::Error(simple_error(
            "process_terminated",
            "FFmpeg terminated unexpectedly",
        )),
    }
}

async fn run_webm_download(
    app: &AppHandle,
    download_id: &str,
    request: &DownloadRequest,
    active: ActiveDownloads,
) -> DownloadAttemptResult {
    let staging_dir = build_staging_dir(download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir) {
            return DownloadAttemptResult::Error(simple_error("staging_failed", error));
        }

        let mut staged_request = request.clone();
        staged_request.output_dir = path_to_string(&staging_dir);

        match run_download_attempt(
            app,
            download_id,
            &staged_request,
            active.clone(),
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(filename) => {
                let intermediate_path = filename
                    .map(PathBuf::from)
                    .or_else(|| find_latest_media_file(&staging_dir));
                let Some(intermediate_path) = intermediate_path else {
                    cleanup_staging_dir(&staging_dir);
                    return DownloadAttemptResult::Error(simple_error(
                        "staging_failed",
                        "Download completed but no staged media file was found.",
                    ));
                };

                let final_path = build_webm_final_path(request, &intermediate_path);
                let staged_output = build_staged_webm_output_path(&staging_dir, &final_path);
                let result = run_webm_conversion(
                    app,
                    download_id,
                    &intermediate_path,
                    &staged_output,
                    &final_path,
                    active.clone(),
                )
                .await;

                cleanup_staging_dir(&staging_dir);
                return result;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                use_twitter_syndication = true;
            }
            other => {
                cleanup_staging_dir(&staging_dir);
                return other;
            }
        }
    }
}

pub async fn start_download(
    app: AppHandle,
    download_id: String,
    request: DownloadRequest,
    active: ActiveDownloads,
) {
    if request.output_dir.trim().is_empty() {
        emit_error_progress(
            &app,
            &download_id,
            simple_error("output_folder", "Output folder is not set."),
        );
        return;
    }

    if let Err(error) = std::fs::create_dir_all(&request.output_dir) {
        emit_error_progress(
            &app,
            &download_id,
            simple_error(
                "output_folder",
                format!("Failed to create output folder: {}", error),
            ),
        );
        return;
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
        match run_webm_download(&app, &download_id, &request, active).await {
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

    let mut use_twitter_syndication = false;

    loop {
        match run_download_attempt(
            &app,
            &download_id,
            &request,
            active.clone(),
            use_twitter_syndication,
        )
        .await
        {
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
                        ..Default::default()
                    },
                );
                return;
            }
            DownloadAttemptResult::Cancelled => {
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
                use_twitter_syndication = true;
            }
            DownloadAttemptResult::Error(error) => {
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
        is_safe_staging_dir, is_twitter_api_auth_error, is_twitter_missing_video_error,
        is_x_or_twitter_url, parse_ffmpeg_progress_percent, parse_first_json_value,
        sanitize_thumbnail_url, should_retry_with_twitter_syndication, validate_download_request,
        validate_fetch_request, PlaylistLineRecord,
    };
    use crate::models::{CookieConfig, DownloadRequest};
    use crate::runtime::YtdlpCommandConfig;
    use std::path::{Path, PathBuf};

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
            Some("bestvideo[height<=720]+bestaudio/best[height<=720]/bestvideo+bestaudio/best")
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
            Some("bestvideo[height<=720][ext=webm]+bestaudio[ext=webm]/best[height<=720][ext=webm]/bestvideo[height<=720]+bestaudio/best[height<=720]/bestvideo+bestaudio/best")
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
    fn staging_paths_are_scoped_to_temp_download_folder() {
        let staging_dir = build_staging_dir("abc-123");
        assert!(is_safe_staging_dir(&staging_dir));
        assert!(!is_safe_staging_dir(Path::new("C:\\Users\\Mr.W\\Desktop")));
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

        let entry = line.into_playlist_entry();
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
    fn playlist_line_falls_back_to_watch_url_when_missing_urls() {
        let line = serde_json::from_str::<PlaylistLineRecord>(
            r#"{"id":"fallback-id","title":"Fallback"}"#,
        )
        .unwrap();

        let entry = line.into_playlist_entry();
        assert_eq!(entry.url, "https://www.youtube.com/watch?v=fallback-id");
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
}

pub async fn cancel_download(download_id: &str, active: ActiveDownloads) -> Result<(), String> {
    let child = {
        let mut downloads = active.lock().await;
        downloads.remove(download_id)
    };

    if let Some(mut child) = child {
        child
            .kill()
            .await
            .map_err(|e| format!("Failed to cancel: {}", e))?;
        let _ = child.wait().await;
        Ok(())
    } else {
        Err("Download not found or already finished".into())
    }
}
