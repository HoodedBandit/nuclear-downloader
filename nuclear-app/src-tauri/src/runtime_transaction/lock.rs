use super::filesystem::{
    ensure_no_reparse_components, ensure_regular_path, is_reparse, verify_opened_lock_identity,
};
use std::fs::{File, OpenOptions};
use std::path::Path;

const LOCK_FILE: &str = ".runtime-mutation-v1.lock";

pub(crate) struct RuntimeMutationLock {
    file: File,
}

impl RuntimeMutationLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self, String> {
        let existing_parent = root
            .ancestors()
            .find(|path| path.exists())
            .ok_or_else(|| "Managed runtime root has no existing ancestor.".to_string())?;
        ensure_no_reparse_components(existing_parent)?;
        std::fs::create_dir_all(root)
            .map_err(|error| format!("Failed to create managed runtime root: {error}"))?;
        ensure_no_reparse_components(root)?;
        ensure_regular_path(root, true, "managed runtime root")?;
        let path = root.join(LOCK_FILE);
        if path.exists() {
            ensure_regular_path(&path, false, "runtime mutation lock")?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            const FILE_SHARE_READ: u32 = 0x0000_0001;
            const FILE_SHARE_WRITE: u32 = 0x0000_0002;
            options
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options
            .open(&path)
            .map_err(|error| format!("Failed to open runtime mutation lock: {error}"))?;
        ensure_regular_path(&path, false, "runtime mutation lock")?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| format!("Failed to inspect opened runtime mutation lock: {error}"))?;
        if !opened_metadata.is_file() || is_reparse(&opened_metadata) {
            return Err("The opened runtime mutation lock is not a regular file.".into());
        }
        file.try_lock()
            .map_err(|error| format!("Another process is mutating the managed runtime: {error}"))?;
        verify_opened_lock_identity(&file, &path)?;
        Ok(Self { file })
    }
}

impl Drop for RuntimeMutationLock {
    fn drop(&mut self) {
        let _ = File::unlock(&self.file);
    }
}
