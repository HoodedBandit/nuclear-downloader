use crate::downloader::naming::suffixed_output_path;
use crate::downloader::process::DownloadJob;
use crate::downloader::validation::MAX_ACTIONABLE_FIELD_BYTES;
use std::path::{Path, PathBuf};

const MAX_OUTPUT_SUFFIX: usize = 9_999;

pub(in crate::downloader) async fn publish_staged_output(
    staged_output: &Path,
    desired_path: &Path,
    job: Option<&DownloadJob>,
) -> Result<PathBuf, String> {
    validate_publishable_output_path(desired_path)?;
    if let Some(parent) = desired_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Failed to create output folder: {error}"))?;
    }

    let staged_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(staged_output)
        .await
        .map_err(|error| format!("Failed to open staged output: {error}"))?;
    staged_file
        .sync_all()
        .await
        .map_err(|error| format!("Failed to flush staged output: {error}"))?;
    drop(staged_file);

    for suffix in 1..=MAX_OUTPUT_SUFFIX {
        if job.is_some_and(DownloadJob::is_cancelled) {
            return Err("Publishing was cancelled.".into());
        }

        let candidate = suffixed_output_path(desired_path, suffix);
        validate_publishable_output_path(&candidate)?;
        if candidate.exists() {
            continue;
        }

        match atomic_move_no_replace(staged_output, &candidate).await {
            Ok(()) => {
                return Ok(candidate);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists || candidate.exists() =>
            {
                continue
            }
            Err(error) => {
                return Err(format!("Failed to publish output: {error}"));
            }
        }
    }

    Err("Could not allocate a unique output filename.".into())
}

fn validate_publishable_output_path(path: &Path) -> Result<(), String> {
    if path.to_string_lossy().len() > MAX_ACTIONABLE_FIELD_BYTES {
        Err("Published output path exceeds the 4 KiB metadata limit.".to_string())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
async fn atomic_move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
async fn atomic_move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    tokio::fs::hard_link(source, destination).await?;
    tokio::fs::remove_file(source).await
}
