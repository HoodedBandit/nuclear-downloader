use super::ownership::owner_record_path;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use tokio::fs;

const UPDATE_DIRECTORY_NAME: &str = "updater";
pub(in crate::updater) const UPDATE_LOCK_FILE_NAME: &str = "update.lock";

/// Holds an open, non-shareable file on Windows. A crashed process releases the
/// OS handle, so a stale marker cannot permanently wedge the updater.
pub(in crate::updater) struct UpdateDirectoryLock {
    file: Option<std::fs::File>,
}

impl UpdateDirectoryLock {
    pub(in crate::updater) fn acquire(directory: &Path) -> Result<Self, String> {
        if let Some(parent) = directory.parent() {
            ensure_no_reparse_components(parent)?;
        }
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Failed to create update temp folder: {error}"))?;
        ensure_no_reparse_components(directory)?;
        let directory_metadata = std::fs::symlink_metadata(directory)
            .map_err(|error| format!("Failed to inspect update temp folder: {error}"))?;
        if !directory_metadata.is_dir() || is_reparse_or_symlink(directory)? {
            return Err("The updater directory must be a regular non-reparse directory.".into());
        }
        let path = directory.join(UPDATE_LOCK_FILE_NAME);
        if path.exists() {
            ensure_no_reparse_components(&path)?;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("Failed to inspect updater lock: {error}"))?;
            if !metadata.is_file() || is_reparse_or_symlink(&path)? {
                return Err("The updater lock must be a regular non-reparse file.".into());
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(&path).map_err(|error| {
            format!("Another updater process owns the update directory: {error}")
        })?;
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            return Err("The updater lock became a reparse point.".into());
        }
        Ok(Self { file: Some(file) })
    }
}

impl Drop for UpdateDirectoryLock {
    fn drop(&mut self) {
        drop(self.file.take());
    }
}

pub(super) fn opened_file_is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(in crate::updater) fn updater_directory() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("NuclearDownloader")
        .join(UPDATE_DIRECTORY_NAME)
}

pub(in crate::updater) fn is_reparse_or_symlink(path: &Path) -> Result<bool, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    Ok(false)
}

pub(in crate::updater) fn ensure_no_reparse_components(path: &Path) -> Result<(), String> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for component_path in ancestors {
        if component_path.exists() && is_reparse_or_symlink(component_path)? {
            return Err(format!(
                "Updater path traverses a symbolic link or reparse point: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

pub(in crate::updater) async fn cleanup_file_if_exists(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path).await {
        if metadata.is_file()
            && is_reparse_or_symlink(path).ok() == Some(false)
            && ensure_no_reparse_components(path).is_ok()
        {
            let _ = fs::remove_file(path).await;
        }
    }
}

pub(in crate::updater) async fn cleanup_current_artifact(path: &Path) {
    cleanup_file_if_exists(path).await;
    // Keep ownership proof while the artifact remains or cannot be inspected,
    // so a later cleanup pass can retry without treating it as unrelated data.
    if !matches!(
        fs::symlink_metadata(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return;
    }
    if let Ok(record_path) = owner_record_path(path) {
        cleanup_file_if_exists(&record_path).await;
    }
}
