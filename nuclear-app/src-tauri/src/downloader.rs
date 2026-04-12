use crate::models::{CookieConfig, DownloadProgress, DownloadRequest, PlaylistEntry, PlaylistInfo, VideoInfo};
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

static DOWNLOAD_PROGRESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\]\s+([\d.]+)%\s+of").unwrap());
static DOWNLOAD_SPEED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"at\s+([\d.]+\w+/s)").unwrap());
static DOWNLOAD_ETA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ETA\s+(\S+)").unwrap());
static DOWNLOAD_DEST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\] Destination:\s+(.+)").unwrap());
static DOWNLOAD_MERGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[Merger\]|post-?process|\[ExtractAudio\]|converting").unwrap());
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
            .or(webpage_url)
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
) -> Result<(), String> {
    if !is_allowed_download_url(url) {
        return Err("Only http:// and https:// URLs are allowed.".into());
    }

    if let Some(config) = cookie_config {
        validate_cookie_config(config)?;
    }

    Ok(())
}

pub fn validate_download_request(request: &DownloadRequest) -> Result<(), String> {
    validate_fetch_request(&request.url, request.cookie_config.as_ref())?;

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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar = dir.join(format!("{}.exe", name));
            if sidecar.exists() {
                return sidecar;
            }
        }
    }
    PathBuf::from(name)
}

fn ytdlp_bin() -> PathBuf {
    resolve_bin("yt-dlp")
}

fn ffmpeg_bin() -> PathBuf {
    resolve_bin("ffmpeg")
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
    is_x_or_twitter_url(url) && is_twitter_api_auth_error(message)
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

fn build_error_message(stderr_output: &str, exit_code: Option<i32>) -> String {
    if stderr_output.is_empty() {
        return format!("yt-dlp exited with code {}", exit_code.unwrap_or(-1));
    }

    stderr_output
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
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

fn spawn_stderr_tail_reader(stderr: tokio::process::ChildStderr) -> tokio::task::JoinHandle<String> {
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

pub fn ffmpeg_available() -> bool {
    let path = ffmpeg_bin();
    if path.exists() {
        return true;
    }
    // Check system PATH
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn run_fetch_info_command(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    use_twitter_syndication: bool,
) -> Result<std::process::Output, String> {
    let bin = ytdlp_bin();
    let mut args = vec![
        "--dump-single-json".to_string(),
        "--no-download".to_string(),
        "--no-playlist".to_string(),
    ];

    append_twitter_syndication_args(&mut args, url, use_twitter_syndication);

    let ffmpeg = ffmpeg_bin();
    if ffmpeg.exists() {
        if let Some(dir) = ffmpeg.parent() {
            args.push("--ffmpeg-location".to_string());
            args.push(dir.to_string_lossy().to_string());
        }
    }

    if let Some(config) = cookie_config {
        append_cookie_args(&mut args, config);
    }

    args.push(url.to_string());

    let mut cmd = Command::new(&bin);
    cmd.args(&args);

    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

    cmd.output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}. Is yt-dlp installed?", e))
}

pub async fn fetch_info(url: &str, cookie_config: Option<&CookieConfig>) -> Result<VideoInfo, String> {
    validate_fetch_request(url, cookie_config)?;

    let mut output = run_fetch_info_command(url, cookie_config, false).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if should_retry_with_twitter_syndication(url, &stderr) {
            output = run_fetch_info_command(url, cookie_config, true).await?;
        }
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp error: {}", stderr.trim()));
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

    let has_audio = data["acodec"]
        .as_str()
        .map(|a| a != "none")
        .unwrap_or(true);

    Ok(VideoInfo {
        id: data["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
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

pub async fn fetch_playlist(url: &str, cookie_config: Option<&CookieConfig>) -> Result<PlaylistInfo, String> {
    validate_fetch_request(url, cookie_config)?;

    let bin = ytdlp_bin();
    let mut cmd = Command::new(&bin);
    cmd.args([
        "--flat-playlist",
        "--dump-json",
        "--lazy-playlist",
        "--no-download",
        url,
    ]);
    configure_cookie_args(&mut cmd, cookie_config);

    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

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
        return Err(format!(
            "yt-dlp error: {}",
            build_error_message(&stderr_output, status.code())
        ));
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

fn build_download_args(request: &DownloadRequest, use_twitter_syndication: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    let ffmpeg = ffmpeg_bin();
    if ffmpeg.exists() {
        if let Some(dir) = ffmpeg.parent() {
            args.push("--ffmpeg-location".to_string());
            args.push(dir.to_string_lossy().to_string());
        }
    }

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
        let format_selector = if request.quality == "best" {
            format!(
                "bestvideo[ext={}]+bestaudio/best[ext={}]/bestvideo+bestaudio/best",
                request.format, request.format
            )
        } else {
            let height = request.quality.replace('p', "");
            format!(
                "bestvideo[height<={}][ext={}]+bestaudio/bestvideo[height<={}]+bestaudio/best[height<={}]/best",
                height, request.format, height, height
            )
        };
        args.push("-f".to_string());
        args.push(format_selector);
        args.push("--merge-output-format".to_string());
        args.push(request.format.clone());
    }

    args.push("--newline".to_string());
    args.push("--progress".to_string());
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

enum DownloadAttemptResult {
    Completed(Option<String>),
    Cancelled,
    RetryWithTwitterSyndication,
    Error(String),
}

async fn run_download_attempt(
    emit_progress: &impl Fn(&str, f64, Option<String>, Option<String>, Option<String>, Option<String>),
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
    cmd.creation_flags(0x08000000);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(format!("Failed to start yt-dlp: {}", error));
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
        }

        if let Some(caps) = DOWNLOAD_PROGRESS_RE.captures(&line) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            let speed = DOWNLOAD_SPEED_RE.captures(&line).map(|c| c[1].to_string());
            let eta = DOWNLOAD_ETA_RE.captures(&line).map(|c| c[1].to_string());
            emit_progress("downloading", pct, speed, eta, None, last_filename.clone());
        } else if DOWNLOAD_MERGE_RE.is_match(&line) {
            emit_progress("postprocessing", 100.0, None, None, None, last_filename.clone());
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
                DownloadAttemptResult::Error(build_error_message(&stderr_output, s.code()))
            }
        }
        None => DownloadAttemptResult::Error("Process terminated unexpectedly".into()),
    }
}

pub async fn start_download(
    app: AppHandle,
    download_id: String,
    request: DownloadRequest,
    active: ActiveDownloads,
) {
    let emit_progress = |status: &str, progress: f64, speed: Option<String>, eta: Option<String>, error: Option<String>, filename: Option<String>| {
        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                download_id: download_id.clone(),
                status: status.to_string(),
                progress,
                speed,
                eta,
                error,
                filename,
            },
        );
    };

    if request.output_dir.trim().is_empty() {
        emit_progress(
            "error",
            0.0,
            None,
            None,
            Some("Output folder is not set.".into()),
            None,
        );
        return;
    }

    if let Err(error) = std::fs::create_dir_all(&request.output_dir) {
        emit_progress(
            "error",
            0.0,
            None,
            None,
            Some(format!("Failed to create output folder: {}", error)),
            None,
        );
        return;
    }

    emit_progress("downloading", 0.0, None, None, None, None);

    let mut use_twitter_syndication = false;

    loop {
        match run_download_attempt(
            &emit_progress,
            &download_id,
            &request,
            active.clone(),
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(filename) => {
                emit_progress("completed", 100.0, None, None, None, filename);
                return;
            }
            DownloadAttemptResult::Cancelled => {
                emit_progress("cancelled", 0.0, None, None, None, None);
                return;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                use_twitter_syndication = true;
            }
            DownloadAttemptResult::Error(error) => {
                emit_progress("error", 0.0, None, None, Some(error), None);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_output_template, is_twitter_api_auth_error, is_x_or_twitter_url,
        parse_first_json_value, sanitize_thumbnail_url, should_retry_with_twitter_syndication,
        validate_download_request, validate_fetch_request, PlaylistLineRecord,
    };
    use crate::models::{CookieConfig, DownloadRequest};

    #[test]
    fn uses_default_template_without_override() {
        let request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "mp4".into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
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
        };

        assert_eq!(
            build_output_template(&request),
            "C:/Users/Mr.W/100%%Downloads/CON_ 100%%_.%(ext)s"
        );
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

        assert!(validate_fetch_request("https://example.com/video", Some(&cookie_config)).is_err());
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

        assert!(validate_fetch_request("https://example.com/video", Some(&cookie_config)).is_err());
    }

    #[test]
    fn keeps_only_https_thumbnail_urls() {
        assert_eq!(
            sanitize_thumbnail_url(Some("https://example.com/thumb.jpg")),
            Some("https://example.com/thumb.jpg".into())
        );
        assert_eq!(sanitize_thumbnail_url(Some("http://example.com/thumb.jpg")), None);
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
        let line =
            serde_json::from_str::<PlaylistLineRecord>(r#"{"id":"fallback-id","title":"Fallback"}"#)
                .unwrap();

        let entry = line.into_playlist_entry();
        assert_eq!(entry.url, "https://www.youtube.com/watch?v=fallback-id");
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
        assert!(is_x_or_twitter_url("https://mobile.twitter.com/user/status/1"));
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
}

pub async fn cancel_download(download_id: &str, active: ActiveDownloads) -> Result<(), String> {
    let child = {
        let mut downloads = active.lock().await;
        downloads.remove(download_id)
    };

    if let Some(mut child) = child {
        child.kill().await.map_err(|e| format!("Failed to cancel: {}", e))?;
        let _ = child.wait().await;
        Ok(())
    } else {
        Err("Download not found or already finished".into())
    }
}
