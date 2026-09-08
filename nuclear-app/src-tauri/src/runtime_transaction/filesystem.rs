use std::fs::{File, OpenOptions};
use std::path::Path;

pub(super) fn ensure_regular_path(path: &Path, directory: bool, label: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {label}: {error}"))?;
    if metadata.file_type().is_symlink() || is_reparse(&metadata) {
        return Err(format!("The {label} is a symbolic link or reparse point."));
    }
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(format!("The {label} has an unexpected file type."));
    }
    Ok(())
}

pub(super) fn ensure_no_reparse_components(path: &Path) -> Result<(), String> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for component in ancestors {
        if !component.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(component)
            .map_err(|error| format!("Failed to inspect runtime path component: {error}"))?;
        if metadata.file_type().is_symlink() || is_reparse(&metadata) {
            return Err("Runtime path traverses a symbolic link or reparse point.".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn verify_opened_lock_identity(opened: &File, path: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;

    fn identity(file: &File) -> Result<crate::windows_file::FileIdentity, String> {
        crate::windows_file::identity(file)
            .map_err(|error| format!("Failed to identify runtime mutation lock: {error}"))
    }

    let mut options = OpenOptions::new();
    options.read(true);
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let checked = options
        .open(path)
        .map_err(|error| format!("Failed to verify runtime mutation lock path: {error}"))?;
    let checked_metadata = checked
        .metadata()
        .map_err(|error| format!("Failed to inspect verified runtime mutation lock: {error}"))?;
    if !checked_metadata.is_file() || is_reparse(&checked_metadata) {
        return Err("The verified runtime mutation lock is not a regular file.".into());
    }
    if identity(opened)? != identity(&checked)? {
        return Err("The runtime mutation lock path changed while it was opened.".into());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn verify_opened_lock_identity(opened: &File, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let left = opened
        .metadata()
        .map_err(|error| format!("Failed to inspect opened runtime mutation lock: {error}"))?;
    let right = std::fs::metadata(path)
        .map_err(|error| format!("Failed to verify runtime mutation lock path: {error}"))?;
    if left.dev() == right.dev() && left.ino() == right.ino() {
        Ok(())
    } else {
        Err("The runtime mutation lock path changed while it was opened.".into())
    }
}

#[cfg(not(any(windows, unix)))]
pub(super) fn verify_opened_lock_identity(_opened: &File, _path: &Path) -> Result<(), String> {
    Ok(())
}

pub(super) fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
pub(super) fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}
