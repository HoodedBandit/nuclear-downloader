use super::naming::build_output_template;
use super::validation::is_x_or_twitter_url;
use crate::models::{CookieConfig, DownloadRequest};
use crate::runtime::YtdlpCommandConfig;
use std::path::Path;
use tokio::process::Command;

pub(super) const FINAL_OUTPUT_RECORD_NAME: &str = ".nuclear-final-output-v1.jsonl";

pub(super) fn append_final_output_record_args(args: &mut Vec<String>, staging_dir: &Path) {
    let record_path = staging_dir
        .join(FINAL_OUTPUT_RECORD_NAME)
        .to_string_lossy()
        .replace('\\', "/")
        .replace('%', "%%");
    args.push("--print-to-file".to_string());
    args.push(r#"after_move:{"schema_version":1,"filepath":%(filepath)j}"#.to_string());
    args.push(record_path);
}

pub(super) fn append_ytdlp_runtime_args(
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

pub(super) fn append_cookie_args(args: &mut Vec<String>, config: &CookieConfig) {
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

pub(super) fn configure_cookie_args(cmd: &mut Command, cookie_config: Option<&CookieConfig>) {
    if let Some(config) = cookie_config {
        let mut args = Vec::new();
        append_cookie_args(&mut args, config);
        cmd.args(args);
    }
}

pub(super) fn append_twitter_syndication_args(args: &mut Vec<String>, url: &str, enabled: bool) {
    if enabled && is_x_or_twitter_url(url) {
        args.push("--extractor-args".to_string());
        args.push("twitter:api=syndication".to_string());
    }
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

pub(super) fn build_download_args_with_runtime(
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

#[cfg(test)]
mod tests {
    use super::{
        append_final_output_record_args, build_download_args, build_download_args_with_runtime,
        FINAL_OUTPUT_RECORD_NAME,
    };
    use crate::models::DownloadRequest;
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
    fn output_record_args_use_after_move_json_and_escape_percent_in_record_path() {
        let stage = PathBuf::from(r"C:\Downloads\100% Ready");
        let mut args = Vec::new();

        append_final_output_record_args(&mut args, &stage);

        assert_eq!(args[0], "--print-to-file");
        assert_eq!(
            args[1],
            r#"after_move:{"schema_version":1,"filepath":%(filepath)j}"#
        );
        assert_eq!(
            args[2],
            format!("C:/Downloads/100%% Ready/{FINAL_OUTPUT_RECORD_NAME}")
        );
    }
}
