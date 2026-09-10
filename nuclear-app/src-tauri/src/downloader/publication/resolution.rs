use super::staging::is_reparse_metadata;
use crate::downloader::command_args::FINAL_OUTPUT_RECORD_NAME;
use crate::models::MediaSelection;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) const MAX_FINAL_OUTPUT_RECORD_BYTES: u64 = 64 * 1024;

#[cfg(test)]
thread_local! {
    pub(super) static TEST_FINAL_OUTPUT_RECORD_READ_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalOutputRecord {
    schema_version: u8,
    filepath: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    extractor_key: Option<String>,
}

#[derive(Debug)]
pub(in crate::downloader) struct StagedOutputError {
    pub(in crate::downloader) code: &'static str,
    pub(in crate::downloader) message: String,
}

pub(super) fn final_output_record_path(staging_dir: &Path) -> PathBuf {
    staging_dir.join(FINAL_OUTPUT_RECORD_NAME)
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

pub(in crate::downloader) fn resolve_staged_output(
    staging_dir: &Path,
    selection: Option<&MediaSelection>,
) -> Result<PathBuf, StagedOutputError> {
    let record_path = final_output_record_path(staging_dir);
    match std::fs::symlink_metadata(&record_path) {
        Ok(metadata) => resolve_recorded_output(&record_path, &metadata, staging_dir, selection),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if selection.is_some() {
                Err(StagedOutputError {
                    code: "staging_output_record_invalid",
                    message: "Selected media completed without an output identity record."
                        .to_string(),
                })
            } else {
                resolve_unambiguous_fallback(staging_dir)
            }
        }
        Err(error) => Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!("Failed to inspect downloader output record: {error}"),
        }),
    }
}

pub(super) fn resolve_recorded_output(
    record_path: &Path,
    metadata: &std::fs::Metadata,
    staging_dir: &Path,
    selection: Option<&MediaSelection>,
) -> Result<PathBuf, StagedOutputError> {
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || is_reparse_metadata(metadata)
    {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: "Downloader output record was not a regular non-reparse file.".to_string(),
        });
    }
    if metadata.len() > MAX_FINAL_OUTPUT_RECORD_BYTES {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!(
                "Downloader output record exceeded the {MAX_FINAL_OUTPUT_RECORD_BYTES}-byte limit."
            ),
        });
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut record_file = options
        .open(record_path)
        .map_err(|error| StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!("Failed to read downloader output record: {error}"),
        })?;
    let opened_metadata = record_file.metadata().map_err(|error| StagedOutputError {
        code: "staging_output_record_invalid",
        message: format!("Failed to inspect opened downloader output record: {error}"),
    })?;
    if !opened_metadata.is_file()
        || is_reparse_metadata(&opened_metadata)
        || opened_metadata.len() > MAX_FINAL_OUTPUT_RECORD_BYTES
    {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!(
                "Downloader output record was not a regular non-reparse file within the {MAX_FINAL_OUTPUT_RECORD_BYTES}-byte limit."
            ),
        });
    }
    let mut contents = Vec::with_capacity(opened_metadata.len() as usize);
    (&mut record_file)
        .take(MAX_FINAL_OUTPUT_RECORD_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!("Failed to read downloader output record: {error}"),
        })?;
    #[cfg(test)]
    TEST_FINAL_OUTPUT_RECORD_READ_BYTES.with(|bytes| bytes.set(contents.len()));
    if contents.len() as u64 > MAX_FINAL_OUTPUT_RECORD_BYTES {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!(
                "Downloader output record exceeded the {MAX_FINAL_OUTPUT_RECORD_BYTES}-byte limit."
            ),
        });
    }
    let text = std::str::from_utf8(&contents).map_err(|_| StagedOutputError {
        code: "staging_output_record_invalid",
        message: "Downloader output record was not valid UTF-8.".to_string(),
    })?;
    let mut records = text.lines().filter(|line| !line.trim().is_empty());
    let line = records.next().ok_or_else(|| StagedOutputError {
        code: "staging_output_record_invalid",
        message: "Downloader output record was empty.".to_string(),
    })?;
    if records.next().is_some() {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: "Downloader output record contained multiple entries.".to_string(),
        });
    }
    let record: FinalOutputRecord =
        serde_json::from_str(line).map_err(|error| StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!("Downloader output record was malformed: {error}"),
        })?;
    if record.schema_version != 1 {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: format!(
                "Downloader output record used unsupported schema version {}.",
                record.schema_version
            ),
        });
    }
    if record.filepath.trim().is_empty() {
        return Err(StagedOutputError {
            code: "staging_output_record_invalid",
            message: "Downloader output record did not contain a filepath.".to_string(),
        });
    }
    if let Some(selection) = selection {
        if record.id.as_deref() != Some(selection.entry_id.as_str())
            || record.extractor_key.as_deref() != Some(selection.extractor_key.as_str())
        {
            return Err(StagedOutputError {
                code: "staging_output_identity_mismatch",
                message: "Downloader output identity did not match the selected media.".to_string(),
            });
        }
    }

    validate_staged_file(Path::new(&record.filepath), staging_dir).map_err(|message| {
        StagedOutputError {
            code: "path_escape",
            message,
        }
    })
}

fn resolve_unambiguous_fallback(staging_dir: &Path) -> Result<PathBuf, StagedOutputError> {
    let entries = std::fs::read_dir(staging_dir).map_err(|error| StagedOutputError {
        code: "staging_failed",
        message: format!("Failed to inspect staged downloader output: {error}"),
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| StagedOutputError {
            code: "staging_failed",
            message: format!("Failed to inspect a staged downloader entry: {error}"),
        })?;
        let path = entry.path();
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
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| StagedOutputError {
            code: "staging_failed",
            message: format!("Failed to inspect staged media candidate: {error}"),
        })?;
        if metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && !is_reparse_metadata(&metadata)
        {
            candidates.push(path);
        }
    }

    match candidates.len() {
        0 => Err(StagedOutputError {
            code: "staging_failed",
            message: "Download completed but no staged media file was found.".to_string(),
        }),
        1 => {
            validate_staged_file(&candidates[0], staging_dir).map_err(|message| StagedOutputError {
                code: "path_escape",
                message,
            })
        }
        count => Err(StagedOutputError {
            code: "staging_output_ambiguous",
            message: format!(
                "Download completed without an output record and left {count} staged media files."
            ),
        }),
    }
}
