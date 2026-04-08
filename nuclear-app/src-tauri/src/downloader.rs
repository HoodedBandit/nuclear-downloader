use crate::models::{CookieConfig, DownloadProgress, DownloadRequest, PlaylistEntry, PlaylistInfo, VideoInfo};
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

pub type ActiveDownloads = Arc<Mutex<HashMap<String, tokio::process::Child>>>;

pub fn create_active_downloads() -> ActiveDownloads {
    Arc::new(Mutex::new(HashMap::new()))
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

pub async fn fetch_info(url: &str, cookie_config: Option<&CookieConfig>) -> Result<VideoInfo, String> {
    let bin = ytdlp_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(["--dump-json", "--no-download", "--no-playlist", url]);

    if let Some(config) = cookie_config {
        if config.enabled {
            match config.mode.as_str() {
                "file" => {
                    if let Some(ref path) = config.cookie_file {
                        cmd.arg("--cookies").arg(path);
                    }
                }
                _ => {
                    cmd.arg("--cookies-from-browser").arg(&config.browser);
                }
            }
        }
    }

    // Point yt-dlp at our bundled ffmpeg if available
    let ffmpeg = ffmpeg_bin();
    if ffmpeg.exists() {
        if let Some(dir) = ffmpeg.parent() {
            cmd.arg("--ffmpeg-location").arg(dir);
        }
    }

    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}. Is yt-dlp installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp error: {}", stderr.trim()));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let data: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse info: {}", e))?;

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
        thumbnail: data["thumbnail"].as_str().map(|s| s.to_string()),
        url: url.to_string(),
        available_qualities: qualities,
        has_audio,
    })
}

pub async fn fetch_playlist(url: &str, cookie_config: Option<&CookieConfig>) -> Result<PlaylistInfo, String> {
    let bin = ytdlp_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(["--flat-playlist", "--dump-json", "--no-download", url]);

    if let Some(config) = cookie_config {
        if config.enabled {
            match config.mode.as_str() {
                "file" => {
                    if let Some(ref path) = config.cookie_file {
                        cmd.arg("--cookies").arg(path);
                    }
                }
                _ => {
                    cmd.arg("--cookies-from-browser").arg(&config.browser);
                }
            }
        }
    }

    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    if lines.is_empty() {
        return Err("No entries found in playlist".into());
    }

    let mut entries: Vec<PlaylistEntry> = Vec::new();
    let mut playlist_title = String::from("Playlist");
    let mut playlist_channel: Option<String> = None;

    for line in &lines {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(line) {
            // Extract playlist-level info from first entry
            if entries.is_empty() {
                if let Some(t) = data["playlist_title"].as_str() {
                    playlist_title = t.to_string();
                } else if let Some(t) = data["playlist"].as_str() {
                    playlist_title = t.to_string();
                }
                playlist_channel = data["playlist_uploader"].as_str()
                    .or(data["channel"].as_str())
                    .map(|s| s.to_string());
            }

            let id = data["id"].as_str().unwrap_or("unknown").to_string();
            let video_url = data["url"].as_str()
                .or(data["webpage_url"].as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={}", id));

            entries.push(PlaylistEntry {
                id,
                title: data["title"].as_str().map(|s| s.to_string()),
                duration: data["duration"].as_f64(),
                url: video_url,
                thumbnail: data["thumbnails"]
                    .as_array()
                    .and_then(|t| t.last())
                    .and_then(|t| t["url"].as_str())
                    .or(data["thumbnail"].as_str())
                    .map(|s| s.to_string()),
            });
        }
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

    let mut args: Vec<String> = Vec::new();

    // Point yt-dlp at our bundled ffmpeg
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
            format!("bestvideo[ext={}]+bestaudio/best[ext={}]/bestvideo+bestaudio/best", request.format, request.format)
        } else {
            let height = request.quality.replace("p", "");
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
    args.push("-o".to_string());
    args.push(build_output_template(&request));
    if let Some(ref config) = request.cookie_config {
        if config.enabled {
            match config.mode.as_str() {
                "file" => {
                    if let Some(ref path) = config.cookie_file {
                        args.push("--cookies".to_string());
                        args.push(path.clone());
                    }
                }
                _ => {
                    args.push("--cookies-from-browser".to_string());
                    args.push(config.browser.clone());
                }
            }
        }
    }

    args.push(request.url.clone());

    let bin = ytdlp_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

    let child_result = cmd.spawn();

    let mut child = match child_result {
        Ok(c) => c,
        Err(e) => {
            emit_progress("error", 0.0, None, None, Some(format!("Failed to start yt-dlp: {}", e)), None);
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    {
        let mut downloads = active.lock().await;
        downloads.insert(download_id.clone(), child);
    }

    // Collect stderr in background for error reporting
    let stderr_handle = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut collected = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if !collected.is_empty() {
                collected.push('\n');
            }
            collected.push_str(&line);
        }
        collected
    });

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let progress_re = Regex::new(r"\[download\]\s+([\d.]+)%\s+of").unwrap();
    let speed_re = Regex::new(r"at\s+([\d.]+\w+/s)").unwrap();
    let eta_re = Regex::new(r"ETA\s+(\S+)").unwrap();
    let dest_re = Regex::new(r"\[download\] Destination:\s+(.+)").unwrap();
    let merge_re = Regex::new(r"\[Merger\]|post-?process|\[ExtractAudio\]|converting").unwrap();

    let mut last_filename: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(caps) = dest_re.captures(&line) {
            last_filename = Some(caps[1].trim().to_string());
        }

        if let Some(caps) = progress_re.captures(&line) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            let speed = speed_re.captures(&line).map(|c| c[1].to_string());
            let eta = eta_re.captures(&line).map(|c| c[1].to_string());
            emit_progress("downloading", pct, speed, eta, None, last_filename.clone());
        } else if merge_re.is_match(&line) {
            emit_progress("postprocessing", 100.0, None, None, None, last_filename.clone());
        }
    }

    let stderr_output = stderr_handle.await.unwrap_or_default();

    let status = {
        let mut downloads = active.lock().await;
        if let Some(mut child) = downloads.remove(&download_id) {
            child.wait().await.ok()
        } else {
            emit_progress("cancelled", 0.0, None, None, None, None);
            return;
        }
    };

    match status {
        Some(s) if s.success() => {
            emit_progress("completed", 100.0, None, None, None, last_filename);
        }
        Some(s) => {
            let err_msg = if stderr_output.is_empty() {
                format!("yt-dlp exited with code {}", s.code().unwrap_or(-1))
            } else {
                // Take last meaningful lines from stderr
                let last_lines: Vec<&str> = stderr_output.lines().rev().take(3).collect();
                last_lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
            };
            emit_progress("error", 0.0, None, None, Some(err_msg), None);
        }
        None => {
            emit_progress("error", 0.0, None, None, Some("Process terminated unexpectedly".into()), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_output_template;
    use crate::models::DownloadRequest;

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
}

pub async fn cancel_download(download_id: &str, active: ActiveDownloads) -> Result<(), String> {
    let mut downloads = active.lock().await;
    if let Some(mut child) = downloads.remove(download_id) {
        child.kill().await.map_err(|e| format!("Failed to cancel: {}", e))?;
        Ok(())
    } else {
        Err("Download not found or already finished".into())
    }
}
