use super::validation::{AUDIO_FORMATS, VIDEO_FORMATS};
use crate::models::DownloadRequest;
use std::path::{Path, PathBuf};

const MAX_CUSTOM_FILENAME_UTF16_UNITS: usize = 180;

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

pub(super) fn sanitize_filename_component(raw: &str) -> Option<String> {
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

pub(super) fn normalize_filename_override(raw: &str) -> Option<String> {
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

pub(super) fn build_output_template(request: &DownloadRequest) -> String {
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

pub(super) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

pub(super) fn build_webm_final_path(
    request: &DownloadRequest,
    intermediate_path: &Path,
) -> PathBuf {
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

pub(super) fn build_staged_webm_output_path(staging_dir: &Path, final_path: &Path) -> PathBuf {
    let stem = final_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(sanitize_filename_component)
        .unwrap_or_else(|| "download".to_string());

    staging_dir.join(format!("{stem}.converted.webm"))
}

pub(super) fn build_final_output_path(
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

pub(super) fn suffixed_output_path(base_path: &Path, suffix: usize) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::{build_output_template, build_webm_final_path, normalize_filename_override};
    use crate::models::DownloadRequest;
    use std::path::{Path, PathBuf};

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
    fn webm_final_path_uses_custom_filename_or_staged_stem() {
        let mut request = DownloadRequest {
            url: "https://example.com/video".into(),
            quality: "best".into(),
            format: "webm".into(),
            output_dir: r"C:\Users\Mr.W\Desktop".into(),
            cookie_config: None,
            filename_override: Some("Clip: 100%?".into()),
            compat_config_path: None,
        };

        assert_eq!(
            build_webm_final_path(&request, Path::new(r"C:\Temp\ignored.mkv")),
            PathBuf::from(r"C:\Users\Mr.W\Desktop").join("Clip_ 100%_.webm")
        );

        request.filename_override = None;
        assert_eq!(
            build_webm_final_path(&request, Path::new(r"C:\Temp\Title [abc123].mkv")),
            PathBuf::from(r"C:\Users\Mr.W\Desktop").join("Title [abc123].webm")
        );
    }
}
