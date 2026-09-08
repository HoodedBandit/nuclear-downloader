use super::emit_runtime_progress;
use super::manifest::{ensure_no_reparse_components, RUNTIME_ENTRY_SIZE_LIMIT};
use super::release::{validate_https_url, RuntimeAssetSelection, RUNTIME_ARCHIVE_LIMIT};
use crate::lifecycle::{UpdateRunError, UpdateTaskContext};
use crate::notifications::RuntimeProgressSink;
use futures_util::StreamExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

const RUNTIME_EXPANDED_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const RUNTIME_ENTRY_LIMIT: usize = 128;
const RUNTIME_DEPTH_LIMIT: usize = 4;
const RUNTIME_COMPRESSION_RATIO_LIMIT: u64 = 200;

pub(super) async fn download_archive(
    progress: &RuntimeProgressSink,
    client: &Client,
    selection: &RuntimeAssetSelection,
    archive_path: &Path,
    context: &UpdateTaskContext,
) -> Result<String, UpdateRunError> {
    validate_https_url(&selection.archive_url)?;
    let response = tokio::select! {
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        result = client.get(&selection.archive_url).send() => result,
    }
    .map_err(|error| format!("Failed to download runtime bundle: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Failed to download runtime bundle: HTTP {}.",
            status.as_u16()
        )
        .into());
    }

    if response
        .content_length()
        .is_some_and(|size| size != selection.archive_size || size > RUNTIME_ARCHIVE_LIMIT)
    {
        return Err("Runtime Content-Length does not match the signed descriptor.".into());
    }

    let total_bytes = response.content_length().or(Some(selection.archive_size));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(archive_path)
        .await
        .map_err(|error| format!("Failed to create runtime archive file: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();

    loop {
        let chunk_result = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = stream.next() => result,
        };
        let Some(chunk_result) = chunk_result else {
            break;
        };
        let chunk =
            chunk_result.map_err(|error| format!("Failed while downloading runtime: {error}"))?;
        downloaded_bytes = downloaded_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "Runtime download byte count overflowed.".to_string())?;
        if downloaded_bytes > selection.archive_size || downloaded_bytes > RUNTIME_ARCHIVE_LIMIT {
            return Err("Runtime archive exceeded its signed size or the 1 GiB limit.".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Failed to write runtime archive: {error}"))?;
        hasher.update(&chunk);
        emit_runtime_progress(
            progress,
            "downloading",
            Some(selection.version.clone()),
            downloaded_bytes,
            total_bytes,
            Some(format!("Downloading {}.", selection.archive_name)),
        );
    }

    file.flush()
        .await
        .map_err(|error| format!("Failed to finalize runtime archive: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync runtime archive: {error}"))?;
    if downloaded_bytes != selection.archive_size {
        return Err(format!(
            "Runtime archive size mismatch: expected {} bytes, got {downloaded_bytes} bytes.",
            selection.archive_size
        )
        .into());
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn extract_runtime_zip(
    archive_path: &Path,
    staging_dir: &Path,
    check_cancelled: &dyn Fn() -> Result<(), UpdateRunError>,
) -> Result<PathBuf, UpdateRunError> {
    if staging_dir.exists() {
        return Err("Runtime extraction staging already exists; refusing to overwrite it.".into());
    }
    let staging_parent = staging_dir
        .parent()
        .ok_or_else(|| "Runtime staging path has no parent.".to_string())?;
    ensure_no_reparse_components(staging_parent)?;
    std::fs::create_dir(staging_dir)
        .map_err(|error| format!("Failed to create runtime staging folder: {error}"))?;
    ensure_no_reparse_components(staging_dir)?;
    ensure_no_reparse_components(archive_path)?;

    let file = File::open(archive_path)
        .map_err(|error| format!("Failed to open runtime archive: {error}"))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| format!("Failed to read runtime archive: {error}"))?;

    if archive.len() > RUNTIME_ENTRY_LIMIT {
        return Err(format!(
            "Runtime archive contains {} entries; the limit is {RUNTIME_ENTRY_LIMIT}.",
            archive.len()
        )
        .into());
    }
    let mut expanded_total = 0u64;
    let mut normalized_paths = HashMap::<String, bool>::new();
    let mut manifest_count = 0usize;
    for index in 0..archive.len() {
        check_cancelled()?;
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to inspect runtime archive entry: {error}"))?;
        let enclosed_name = entry
            .enclosed_name()
            .ok_or_else(|| "Runtime archive contains an unsafe path.".to_string())?;
        let depth = enclosed_name.components().count();
        if depth == 0 || depth > RUNTIME_DEPTH_LIMIT {
            return Err(format!(
                "Runtime archive entry {} exceeds depth {RUNTIME_DEPTH_LIMIT}.",
                entry.name()
            )
            .into());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Runtime archive contains a symbolic-link entry.".into());
        }
        let is_directory = entry.is_dir();
        let normalized = normalize_runtime_archive_path(&enclosed_name)?;
        if normalized
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "runtime-manifest.json")
            && !is_directory
        {
            manifest_count += 1;
        }
        if normalized_paths
            .insert(normalized.clone(), is_directory)
            .is_some()
        {
            return Err(format!(
                "Runtime archive contains a duplicate or case-colliding path: {}.",
                entry.name()
            )
            .into());
        }
        let components = normalized.split('/').collect::<Vec<_>>();
        for parent_depth in 1..components.len() {
            let parent = components[..parent_depth].join("/");
            if normalized_paths.get(&parent) == Some(&false) {
                return Err("Runtime archive contains a file/directory path conflict.".into());
            }
        }
        if !is_directory
            && normalized_paths
                .iter()
                .any(|(path, _)| path.starts_with(&format!("{normalized}/")))
        {
            return Err("Runtime archive contains a file/directory path conflict.".into());
        }
        if entry.size() > RUNTIME_ENTRY_SIZE_LIMIT {
            return Err(format!(
                "Runtime archive entry {} exceeds the 2 GiB limit.",
                entry.name()
            )
            .into());
        }
        if entry.size() > 0
            && (entry.compressed_size() == 0
                || entry.size()
                    > entry
                        .compressed_size()
                        .saturating_mul(RUNTIME_COMPRESSION_RATIO_LIMIT))
        {
            return Err(format!(
                "Runtime archive entry {} exceeds the 200:1 compression-ratio limit.",
                entry.name()
            )
            .into());
        }
        expanded_total = expanded_total
            .checked_add(entry.size())
            .ok_or_else(|| "Runtime archive expanded size overflowed.".to_string())?;
        if expanded_total > RUNTIME_EXPANDED_LIMIT {
            return Err("Runtime archive exceeds the 4 GiB expanded-size limit.".into());
        }
    }
    if manifest_count != 1 || !normalized_paths.contains_key("runtime-manifest.json") {
        return Err("Runtime archive must contain exactly one root runtime-manifest.json.".into());
    }
    preflight_free_space(staging_dir, expanded_total)?;

    for index in 0..archive.len() {
        check_cancelled()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read runtime archive entry: {error}"))?;
        let enclosed_name = entry
            .enclosed_name()
            .ok_or_else(|| "Runtime archive contains an unsafe path.".to_string())?;
        let output_path = staging_dir.join(enclosed_name);

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
            ensure_no_reparse_components(&output_path)?;
        } else {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("Failed to create runtime folder: {error}"))?;
                ensure_no_reparse_components(parent)?;
            }
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output_path)
                .map_err(|error| format!("Failed to create runtime file: {error}"))?;
            let mut copied = 0u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                check_cancelled()?;
                let read = entry
                    .read(&mut buffer)
                    .map_err(|error| format!("Failed to read runtime archive entry: {error}"))?;
                if read == 0 {
                    break;
                }
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| "Runtime extraction byte count overflowed.".to_string())?;
                if copied > entry.size() || copied > RUNTIME_ENTRY_SIZE_LIMIT {
                    return Err("Runtime archive entry exceeded its declared size.".into());
                }
                check_cancelled()?;
                output
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("Failed to extract runtime file: {error}"))?;
            }
            if copied != entry.size() {
                return Err("Runtime archive entry ended before its declared size.".into());
            }
            output
                .flush()
                .map_err(|error| format!("Failed to flush runtime file: {error}"))?;
            output
                .sync_all()
                .map_err(|error| format!("Failed to sync runtime file: {error}"))?;
        }
    }

    Ok(staging_dir.to_path_buf())
}

pub(super) fn normalize_runtime_archive_path(path: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err("Runtime archive contains a non-normal path component.".into());
        };
        let name = name
            .to_str()
            .ok_or_else(|| "Runtime archive paths must be UTF-8.".to_string())?;
        validate_runtime_path_component(name)?;
        components.push(name.to_ascii_lowercase());
    }
    if components.is_empty() {
        return Err("Runtime archive contains an empty path.".into());
    }
    Ok(components.join("/"))
}

pub(super) fn validate_runtime_path_component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || name.ends_with(['.', ' '])
        || is_windows_reserved_device_name(name)
    {
        return Err("Runtime archive contains a non-canonical Windows path.".into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn preflight_free_space(_path: &Path, _required: u64) -> Result<(), String> {
    Ok(())
}

pub(super) fn is_windows_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(windows)]
pub(super) fn preflight_free_space(path: &Path, required: u64) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available = 0u64;
    // SAFETY: `wide` is a valid nul-terminated UTF-16 path and the output
    // pointer is valid for the duration of the call.
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(format!(
            "Failed to determine free space for runtime extraction: {}",
            std::io::Error::last_os_error()
        ));
    }
    if available < required {
        return Err(format!(
            "Runtime extraction requires {required} bytes, but only {available} bytes are available."
        ));
    }
    Ok(())
}
