use super::validation::is_x_or_twitter_url;
use crate::runtime;

#[derive(Debug, Clone)]
pub(super) struct DownloadErrorInfo {
    pub(super) code: String,
    pub(super) message: String,
    pub(super) detail: String,
}

fn is_twitter_api_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("guest token")
        || lower.contains("bad guest token")
        || lower.contains("failed to query api")
        || (lower.contains("[twitter]") && lower.contains("unauthorized"))
}

pub(super) fn should_retry_with_twitter_syndication(url: &str, message: &str) -> bool {
    is_x_or_twitter_url(url)
        && (is_twitter_api_auth_error(message) || is_twitter_missing_video_error(message))
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

pub(super) fn simple_error(code: &str, message: impl Into<String>) -> DownloadErrorInfo {
    let message = message.into();
    DownloadErrorInfo {
        code: code.to_string(),
        message: message.clone(),
        detail: message,
    }
}

pub(super) fn classify_process_error(
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

pub(super) fn error_for_fetch(stderr_output: &str, exit_code: Option<i32>) -> String {
    let error = classify_process_error(stderr_output, exit_code, None);
    error.message
}

#[cfg(test)]
mod tests {
    use super::{
        build_error_message, classify_process_error, is_twitter_api_auth_error,
        is_twitter_missing_video_error, is_x_or_twitter_url, should_retry_with_twitter_syndication,
    };

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
