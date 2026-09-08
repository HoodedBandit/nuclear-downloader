use super::command_args::{
    append_cookie_args, append_twitter_syndication_args, append_ytdlp_runtime_args,
    configure_cookie_args,
};
use super::errors::{error_for_fetch, should_retry_with_twitter_syndication};
use super::process::{
    record_streamed_output_bytes, wait_with_bounded_output, wait_with_streamed_stdout, DownloadJob,
    MAX_STDERR_BYTES,
};
use super::validation::{is_allowed_download_url, validate_fetch_request};
use crate::models::{CookieConfig, PlaylistEntry, PlaylistInfo, UrlInspection, VideoInfo};
use serde::Deserialize;
use serde_json::Deserializer;
use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;
use tokio::process::Command;
use url::Url;

const MAX_INSPECTION_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const INSPECTION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PLAYLIST_ENTRIES: usize = 1_000;

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

fn sanitize_thumbnail_url(raw: Option<&str>) -> Option<String> {
    raw.and_then(|value| {
        Url::parse(value)
            .ok()
            .filter(|url| url.scheme() == "https")
            .map(|_| value.to_string())
    })
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

async fn with_inspection_timeout<T, F>(
    job: &DownloadJob,
    timeout: Duration,
    operation: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => {
            job.terminate_processes();
            Err("process_timeout: inspection timed out".to_string())
        }
    }
}

pub(crate) async fn inspect_url(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    job: &DownloadJob,
) -> Result<UrlInspection, String> {
    with_inspection_timeout(
        job,
        INSPECTION_TIMEOUT,
        inspect_url_inner(url, cookie_config, compat_config_path, job),
    )
    .await
}

async fn inspect_url_inner(
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

    let child = job.spawn(&mut cmd, "yt-dlp", false).await?;

    let mut entries: Vec<PlaylistEntry> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();
    let mut parsed_entry_count = 0usize;
    let mut truncated = false;
    let mut playlist_title = String::from("Playlist");
    let mut playlist_channel: Option<String> = None;
    let mut stdout_bytes = 0usize;

    let output = wait_with_streamed_stdout(child, job, "yt-dlp", |line| {
        let result = record_streamed_output_bytes(
            &mut stdout_bytes,
            line.len(),
            MAX_INSPECTION_OUTPUT_BYTES,
        );
        if result.is_ok() {
            if let Ok(data) = serde_json::from_str::<PlaylistLineRecord>(&line) {
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
        }
        std::future::ready(result)
    })
    .await?;

    if output.cancelled || job.is_cancelled() {
        return Err("Playlist inspection was cancelled.".into());
    }

    if !output.status.success() {
        return Err(error_for_fetch(&output.stderr, output.status.code()));
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

#[cfg(test)]
mod tests {
    use super::super::process::{record_streamed_output_bytes, DownloadJob};
    use super::{
        parse_first_json_value, push_bounded_playlist_entry, sanitize_thumbnail_url,
        with_inspection_timeout, PlaylistLineRecord, MAX_INSPECTION_OUTPUT_BYTES,
        MAX_PLAYLIST_ENTRIES,
    };
    use crate::models::PlaylistEntry;
    use std::collections::HashSet;
    use std::time::Duration;

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
    fn parses_first_json_value_from_multiple_documents() {
        let value = parse_first_json_value("{\"id\":\"one\"}\n{\"id\":\"two\"}").unwrap();
        assert_eq!(value["id"].as_str(), Some("one"));
    }

    #[test]
    fn streamed_inspection_enforces_one_cumulative_output_limit() {
        let legal_line_bytes = 60 * 1024;
        let mut total = 0usize;
        let legal_lines = MAX_INSPECTION_OUTPUT_BYTES / (legal_line_bytes + 1);

        for _ in 0..legal_lines {
            record_streamed_output_bytes(&mut total, legal_line_bytes, MAX_INSPECTION_OUTPUT_BYTES)
                .unwrap();
        }
        let error =
            record_streamed_output_bytes(&mut total, legal_line_bytes, MAX_INSPECTION_OUTPUT_BYTES)
                .unwrap_err();

        assert!(error.starts_with("process_output_limit:"), "{error}");
    }

    #[tokio::test]
    async fn stalled_inspection_hits_deadline_without_becoming_user_cancelled() {
        let job = DownloadJob::new().unwrap();

        let error = with_inspection_timeout(
            &job,
            Duration::from_millis(10),
            std::future::pending::<Result<(), String>>(),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "process_timeout: inspection timed out");
        assert!(!job.is_cancelled());
    }
}
