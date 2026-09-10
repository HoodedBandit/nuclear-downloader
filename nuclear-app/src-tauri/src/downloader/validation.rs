use super::naming::normalize_filename_override;
use crate::app_error::AppError;
use crate::models::{CookieConfig, DownloadRequest};
use regex::Regex;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use url::Url;

pub(super) const MAX_ACTIONABLE_FIELD_BYTES: usize = 4 * 1024;
pub(super) const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
pub(super) const AUDIO_FORMATS: &[&str] = &["mp3", "flac", "wav", "aac", "opus"];
const COOKIE_BROWSERS: &[&str] = &["firefox", "chrome", "edge", "brave", "opera", "chromium"];

static QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{3,4}p$").unwrap());

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
    if let Some(selection) = &request.selection {
        selection.validate()?;
    }

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

pub(super) fn validate_actionable_input(name: &str, value: &str) -> Result<(), String> {
    if value.len() > MAX_ACTIONABLE_FIELD_BYTES {
        Err(format!("The {name} exceeds the 4 KiB input limit."))
    } else {
        Ok(())
    }
}

fn is_allowed_format(format: &str) -> bool {
    VIDEO_FORMATS.contains(&format) || AUDIO_FORMATS.contains(&format)
}

fn is_allowed_quality(quality: &str) -> bool {
    quality == "best" || QUALITY_RE.is_match(quality)
}

#[cfg(windows)]
pub(super) fn is_x_or_twitter_url(raw: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{validate_download_request, validate_fetch_request, MAX_ACTIONABLE_FIELD_BYTES};
    use crate::models::{CookieConfig, DownloadRequest, MediaSelection};

    fn download_request(format: &str, quality: &str) -> DownloadRequest {
        DownloadRequest {
            url: "https://example.com/video".into(),
            quality: quality.into(),
            format: format.into(),
            output_dir: "C:\\Users\\Mr.W\\Downloads".into(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
            selection: None,
        }
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
            selection: None,
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
            selection: None,
        };

        assert!(validate_download_request(&request).is_err());
    }

    #[test]
    fn rejects_invalid_media_selection() {
        let mut request = download_request("mp4", "best");
        request.selection = Some(MediaSelection {
            entry_id: "video-id".into(),
            extractor_key: "Twitter\n".into(),
            playlist_index: 1,
        });

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
}
