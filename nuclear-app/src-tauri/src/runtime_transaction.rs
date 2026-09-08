use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const JOURNAL_FILE: &str = ".runtime-transaction-v1.json";
const LOCK_FILE: &str = ".runtime-mutation-v1.lock";
const JOURNAL_LIMIT: u64 = 64 * 1024;
const QUARANTINE_PREFIX: &str = ".runtime-transaction-v1.quarantine-";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RuntimeTransactionCheckpoint {
    CandidateVerified,
    OldMoved,
    NewPublished,
    CurrentPointerCommitted,
    BackupCleaned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuntimeTransaction {
    schema_version: u32,
    pub(crate) update_id: String,
    pub(crate) runtime_version: String,
    pub(crate) had_existing: bool,
    pub(crate) checkpoint: RuntimeTransactionCheckpoint,
}

impl RuntimeTransaction {
    pub(crate) fn new(
        update_id: String,
        runtime_version: String,
        had_existing: bool,
    ) -> Result<Self, String> {
        validate_update_id(&update_id)?;
        validate_runtime_version(&runtime_version)?;
        Ok(Self {
            schema_version: 1,
            update_id,
            runtime_version,
            had_existing,
            checkpoint: RuntimeTransactionCheckpoint::CandidateVerified,
        })
    }

    pub(crate) fn set_checkpoint(&mut self, checkpoint: RuntimeTransactionCheckpoint) {
        self.checkpoint = checkpoint;
    }

    pub(crate) fn paths(&self, root: &Path) -> RuntimeTransactionPaths {
        let work_root = root.join(".updates").join(&self.update_id);
        RuntimeTransactionPaths {
            candidate: work_root.join("extracted"),
            work_root,
            final_dir: root.join(&self.runtime_version),
            backup: root.join(format!(
                ".backup-{}-{}",
                self.runtime_version, self.update_id
            )),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "Unsupported runtime transaction schema version {}.",
                self.schema_version
            ));
        }
        validate_update_id(&self.update_id)?;
        validate_runtime_version(&self.runtime_version)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeTransactionPaths {
    pub(crate) work_root: PathBuf,
    pub(crate) candidate: PathBuf,
    pub(crate) final_dir: PathBuf,
    pub(crate) backup: PathBuf,
}

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

pub(crate) fn load(root: &Path) -> Result<Option<RuntimeTransaction>, String> {
    let path = root.join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(None);
    }
    ensure_regular_path(&path, false, "runtime transaction journal")?;
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Failed to inspect runtime transaction journal: {error}"))?;
    if metadata.len() > JOURNAL_LIMIT {
        return Err("Runtime transaction journal exceeds the 64 KiB limit.".into());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("Failed to read runtime transaction journal: {error}"))?;
    let transaction = serde_json::from_slice::<RuntimeTransaction>(&bytes)
        .map_err(|error| format!("Failed to parse runtime transaction journal: {error}"))?;
    transaction.validate()?;
    Ok(Some(transaction))
}

pub(crate) fn store(root: &Path, transaction: &RuntimeTransaction) -> Result<(), String> {
    transaction.validate()?;
    ensure_regular_path(root, true, "managed runtime root")?;
    let bytes = serde_json::to_vec(transaction)
        .map_err(|error| format!("Failed to serialize runtime transaction journal: {error}"))?;
    let persisted = serde_json::from_slice::<RuntimeTransaction>(&bytes)
        .map_err(|error| format!("Failed to validate runtime transaction journal: {error}"))?;
    if &persisted != transaction {
        return Err("Runtime transaction journal did not round-trip exactly.".into());
    }
    let destination = root.join(JOURNAL_FILE);
    if destination.exists() {
        ensure_regular_path(&destination, false, "runtime transaction journal")?;
    }
    let temporary = root.join(format!(".runtime-transaction-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("Failed to create runtime transaction journal: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "Failed to persist runtime transaction journal: {error}"
        ));
    }
    drop(file);
    replace_file_atomically(&temporary, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Failed to publish runtime transaction journal: {error}")
    })
}

pub(crate) fn clear(root: &Path) -> Result<(), String> {
    let path = root.join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(());
    }
    ensure_regular_path(&path, false, "runtime transaction journal")?;
    std::fs::remove_file(path)
        .map_err(|error| format!("Failed to clear runtime transaction journal: {error}"))
}

pub(crate) fn quarantine(root: &Path, transaction: &RuntimeTransaction) -> Result<(), String> {
    transaction.validate()?;
    let source = root.join(JOURNAL_FILE);
    ensure_regular_path(&source, false, "runtime transaction journal")?;
    let current = load(root)?
        .ok_or_else(|| "Runtime transaction journal disappeared before quarantine.".to_string())?;
    if &current != transaction {
        return Err(
            "Runtime transaction journal changed before quarantine; all artifacts were retained."
                .into(),
        );
    }
    let destination = root.join(format!("{QUARANTINE_PREFIX}{}.json", transaction.update_id));
    if destination.exists() {
        return Err("A runtime transaction quarantine record already exists.".into());
    }
    std::fs::rename(source, destination)
        .map_err(|error| format!("Failed to quarantine runtime transaction journal: {error}"))
}

pub(crate) fn protected_update_ids(root: &Path) -> Result<HashSet<String>, String> {
    let mut protected = HashSet::new();
    if let Some(transaction) = load(root)? {
        protected.insert(transaction.update_id);
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("Failed to inspect runtime transaction records: {error}"))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Failed to enumerate runtime transaction records: {error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(QUARANTINE_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        ensure_regular_path(&path, false, "quarantined runtime transaction")?;
        let mut file = File::open(&path)
            .map_err(|error| format!("Failed to open quarantined runtime transaction: {error}"))?;
        let metadata = file.metadata().map_err(|error| {
            format!("Failed to inspect quarantined runtime transaction: {error}")
        })?;
        if !metadata.is_file() || is_reparse(&metadata) || metadata.len() > JOURNAL_LIMIT {
            return Err(
                "Quarantined runtime transaction exceeds the 64 KiB limit or is not a regular file."
                    .into(),
            );
        }
        let mut bytes = Vec::new();
        Read::take(&mut file, JOURNAL_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Failed to read quarantined runtime transaction: {error}"))?;
        if bytes.len() as u64 > JOURNAL_LIMIT {
            return Err("Quarantined runtime transaction exceeds the 64 KiB limit.".into());
        }
        let transaction = serde_json::from_slice::<RuntimeTransaction>(&bytes)
            .map_err(|error| format!("Failed to parse quarantined runtime transaction: {error}"))?;
        transaction.validate()?;
        let name_update_id = name
            .strip_prefix(QUARANTINE_PREFIX)
            .and_then(|value| value.strip_suffix(".json"))
            .ok_or_else(|| "Quarantined runtime transaction name is invalid.".to_string())?;
        if name_update_id != transaction.update_id {
            return Err(
                "Quarantined runtime transaction name does not match its update ID.".into(),
            );
        }
        protected.insert(transaction.update_id);
    }
    Ok(protected)
}

fn validate_update_id(update_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(update_id)
        .map(|_| ())
        .map_err(|_| "Runtime transaction update ID is invalid.".to_string())
}

fn validate_runtime_version(version: &str) -> Result<(), String> {
    let mut parts = version.split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none();
    if valid && version.trim() == version {
        Ok(())
    } else {
        Err("Runtime transaction version is invalid.".into())
    }
}

fn ensure_regular_path(path: &Path, directory: bool, label: &str) -> Result<(), String> {
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

fn ensure_no_reparse_components(path: &Path) -> Result<(), String> {
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
fn verify_opened_lock_identity(opened: &File, path: &Path) -> Result<(), String> {
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
fn verify_opened_lock_identity(opened: &File, path: &Path) -> Result<(), String> {
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
fn verify_opened_lock_identity(_opened: &File, _path: &Path) -> Result<(), String> {
    Ok(())
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
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
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
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
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nuclear-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn transaction_round_trips_every_checkpoint() {
        let root = root("runtime-transaction-roundtrip");
        std::fs::create_dir_all(&root).unwrap();
        let mut transaction = RuntimeTransaction::new(
            uuid::Uuid::new_v4().to_string(),
            "2026.06.09".to_string(),
            true,
        )
        .unwrap();
        for checkpoint in [
            RuntimeTransactionCheckpoint::CandidateVerified,
            RuntimeTransactionCheckpoint::OldMoved,
            RuntimeTransactionCheckpoint::NewPublished,
            RuntimeTransactionCheckpoint::CurrentPointerCommitted,
            RuntimeTransactionCheckpoint::BackupCleaned,
        ] {
            transaction.set_checkpoint(checkpoint);
            store(&root, &transaction).unwrap();
            assert_eq!(load(&root).unwrap(), Some(transaction.clone()));
        }
        clear(&root).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn operating_system_lock_rejects_a_second_owner() {
        let root = root("runtime-mutation-lock");
        let first = RuntimeMutationLock::acquire(&root).unwrap();
        assert!(RuntimeMutationLock::acquire(&root).is_err());
        drop(first);
        RuntimeMutationLock::acquire(&root).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn changed_journal_is_preserved_instead_of_quarantining_stale_state() {
        let root = root("runtime-quarantine-identity");
        std::fs::create_dir_all(&root).unwrap();
        let stored = RuntimeTransaction::new(
            uuid::Uuid::new_v4().to_string(),
            "2026.06.09".to_string(),
            true,
        )
        .unwrap();
        let stale = RuntimeTransaction::new(
            uuid::Uuid::new_v4().to_string(),
            "2026.06.10".to_string(),
            false,
        )
        .unwrap();
        store(&root, &stored).unwrap();

        assert!(quarantine(&root, &stale).is_err());
        assert_eq!(load(&root).unwrap(), Some(stored));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protected_update_ids_rejects_oversized_quarantine_and_preserves_it() {
        let root = root("runtime-quarantine-bound");
        std::fs::create_dir_all(&root).unwrap();
        let transaction = RuntimeTransaction::new(
            uuid::Uuid::new_v4().to_string(),
            "2026.06.09".to_string(),
            true,
        )
        .unwrap();
        let quarantine = root.join(format!("{QUARANTINE_PREFIX}{}.json", transaction.update_id));
        let mut bytes = serde_json::to_vec(&transaction).unwrap();
        bytes.resize(JOURNAL_LIMIT as usize + 1, b' ');
        std::fs::write(&quarantine, &bytes).unwrap();

        let error = protected_update_ids(&root).unwrap_err();
        assert!(error.contains("64 KiB limit"));
        assert_eq!(std::fs::read(&quarantine).unwrap(), bytes);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn mutation_lock_rejects_a_reparse_ancestor_without_creating_the_leaf() {
        use std::os::windows::fs::symlink_dir;

        let base = root("runtime-lock-reparse");
        let actual = base.join("actual");
        let linked = base.join("linked");
        let managed = linked.join("managed");
        std::fs::create_dir_all(&actual).unwrap();
        if symlink_dir(&actual, &linked).is_err() {
            let _ = std::fs::remove_dir_all(base);
            return;
        }

        assert!(RuntimeMutationLock::acquire(&managed).is_err());
        assert!(!actual.join("managed").exists());
        std::fs::remove_dir(&linked).unwrap();
        let _ = std::fs::remove_dir_all(base);
    }
}
