use crate::app_error::AppError;
use crate::models::{
    OperationSnapshot, OperationState, QueueItemRecord, QueueItemState, APP_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const TERMINAL_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub(crate) const MAX_TERMINAL_ATTEMPTS: usize = 200;
const JOURNAL_FILENAME: &str = "state-v1.dpapi";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentJournal {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    pub queue: Vec<QueueItemRecord>,
    pub operations: Vec<OperationSnapshot>,
}

impl Default for PersistentJournal {
    fn default() -> Self {
        Self {
            schema_version: APP_SCHEMA_VERSION,
            revision: 0,
            queue: Vec::new(),
            operations: Vec::new(),
        }
    }
}

impl PersistentJournal {
    pub fn normalize_after_restart(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        for item in &mut self.queue {
            if matches!(item.state, QueueItemState::Queued | QueueItemState::Running) {
                item.state = QueueItemState::Interrupted;
                item.updated_at_ms = now_ms;
                changed = true;
            }
        }

        for operation in &mut self.operations {
            if !operation.state.is_terminal() {
                operation.state = OperationState::Interrupted;
                operation.phase = None;
                operation.finished_at_ms = Some(now_ms);
                operation.updated_at_ms = now_ms;
                operation.error = Some(
                    AppError::new(
                        "interrupted",
                        "The application stopped before this operation finished.",
                    )
                    .retryable(true),
                );
                changed = true;
            }
        }
        let operation_count = self.operations.len();
        self.prune(now_ms);
        changed || self.operations.len() != operation_count
    }

    pub fn prune(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(TERMINAL_RETENTION_MS);
        let mut active = self
            .operations
            .iter()
            .filter(|operation| !operation.state.is_terminal())
            .cloned()
            .collect::<Vec<_>>();
        let mut terminal = self
            .operations
            .iter()
            .filter(|operation| {
                operation.state.is_terminal()
                    && operation.finished_at_ms.unwrap_or(operation.updated_at_ms) >= cutoff
            })
            .cloned()
            .collect::<Vec<_>>();
        terminal.sort_by_key(|operation| std::cmp::Reverse(operation.updated_at_ms));
        terminal.truncate(MAX_TERMINAL_ATTEMPTS);
        active.extend(terminal);
        active.sort_by_key(|operation| operation.created_at_ms);
        self.operations = active;
    }

    fn persistence_copy(&self, now_ms: u64) -> Self {
        let mut copy = self.clone();
        // CookieConfig contains only the selected mode/browser or cookies.txt
        // path. Cookie database/file contents are never read into this journal.
        // Inspection payloads are transient capability records. Persisting them
        // would multiply untrusted remote metadata across operation history.
        for operation in &mut copy.operations {
            operation.inspection_result = None;
        }
        copy.prune(now_ms);
        copy
    }
}

#[derive(Debug)]
pub struct JournalStore {
    path: PathBuf,
    _lock: fs::File,
    persisted_revision: Mutex<u64>,
    #[cfg(test)]
    fail_next_save: AtomicBool,
}

impl JournalStore {
    pub fn default_path() -> Result<PathBuf, AppError> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| AppError::internal("Could not locate per-user application data."))?
            .join("Nuclear Downloader");
        Ok(root.join(JOURNAL_FILENAME))
    }

    pub fn open_default() -> Result<(Self, PersistentJournal, Option<PathBuf>), AppError> {
        Self::open(Self::default_path()?)
    }

    pub fn open(path: PathBuf) -> Result<(Self, PersistentJournal, Option<PathBuf>), AppError> {
        let journal_lock = acquire_journal_lock(&path)?;
        let store = Self {
            path,
            _lock: journal_lock,
            persisted_revision: Mutex::new(0),
            #[cfg(test)]
            fail_next_save: AtomicBool::new(false),
        };
        if !store.path.exists() {
            return Ok((store, PersistentJournal::default(), None));
        }

        match store.read() {
            Ok(mut journal) => {
                let persisted_revision = journal.revision;
                let normalized = journal.normalize_after_restart(now_ms());
                *store
                    .persisted_revision
                    .lock()
                    .map_err(|_| AppError::internal("The journal writer lock is unavailable."))? =
                    persisted_revision;
                if normalized {
                    journal.revision = persisted_revision.saturating_add(1);
                    store.save(&journal)?;
                }
                Ok((store, journal, None))
            }
            Err(error) if error.code == "journal_corrupt" => {
                let quarantine = store.quarantine_corrupt()?;
                Ok((store, PersistentJournal::default(), Some(quarantine)))
            }
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, journal: &PersistentJournal) -> Result<(), AppError> {
        let mut persisted_revision = self
            .persisted_revision
            .lock()
            .map_err(|_| AppError::internal("The journal writer lock is unavailable."))?;
        if journal.revision <= *persisted_revision {
            return Ok(());
        }
        #[cfg(test)]
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(AppError::internal(
                "Injected application journal publication failure.",
            ));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| AppError::internal("Journal path did not have a parent folder."))?;
        fs::create_dir_all(parent).map_err(|error| {
            AppError::internal("Could not create the application data folder.")
                .with_detail(error.kind().to_string())
        })?;

        let json = serde_json::to_vec(&journal.persistence_copy(now_ms()))
            .map_err(|_| AppError::internal("Could not serialize the application journal."))?;
        let protected = protect_for_current_user(&json)?;
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));

        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                AppError::internal("Could not create the application journal.")
                    .with_detail(error.kind().to_string())
            })?;
        output.write_all(&protected).map_err(|error| {
            AppError::internal("Could not write the application journal.")
                .with_detail(error.kind().to_string())
        })?;
        output.sync_all().map_err(|error| {
            AppError::internal("Could not flush the application journal.")
                .with_detail(error.kind().to_string())
        })?;
        drop(output);

        atomic_replace(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            AppError::internal("Could not publish the application journal.")
                .with_detail(error.kind().to_string())
        })?;
        *persisted_revision = journal.revision;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_save_for_test(&self) {
        self.fail_next_save.store(true, Ordering::SeqCst);
    }

    fn read(&self) -> Result<PersistentJournal, AppError> {
        let protected = fs::read(&self.path)
            .map_err(|_| AppError::internal("Could not read the application journal."))?;
        let json = unprotect_for_current_user(&protected)?;
        let journal: PersistentJournal = serde_json::from_slice(&json)
            .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
        if journal.schema_version != APP_SCHEMA_VERSION {
            return Err(AppError::new(
                "journal_migration_required",
                "The application journal was created by an unsupported version.",
            )
            .retryable(true));
        }
        if journal
            .queue
            .iter()
            .any(|item| item.schema_version != APP_SCHEMA_VERSION)
            || journal
                .operations
                .iter()
                .any(|operation| operation.schema_version != APP_SCHEMA_VERSION)
        {
            return Err(AppError::new(
                "journal_migration_required",
                "The application journal contains records from an unsupported version.",
            )
            .retryable(true));
        }
        validate_journal_structure(&journal)?;
        Ok(journal)
    }

    fn quarantine_corrupt(&self) -> Result<PathBuf, AppError> {
        let quarantine = self.path.with_extension(format!("corrupt-{}", now_ms()));
        fs::rename(&self.path, &quarantine).map_err(|error| {
            AppError::internal("Could not quarantine the corrupt application journal.")
                .with_detail(error.kind().to_string())
        })?;
        Ok(quarantine)
    }
}

fn validate_journal_structure(journal: &PersistentJournal) -> Result<(), AppError> {
    if journal.queue.len() > 1_000 || journal.operations.len() > 1_200 {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal exceeded its record limits.",
        ));
    }
    let mut queue_ids = HashSet::with_capacity(journal.queue.len());
    for item in &journal.queue {
        if uuid::Uuid::parse_str(&item.id).is_err() || !queue_ids.insert(item.id.as_str()) {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal contained invalid or duplicate queue IDs.",
            ));
        }
    }
    let mut operation_ids = HashSet::with_capacity(journal.operations.len());
    for operation in &journal.operations {
        if uuid::Uuid::parse_str(&operation.id).is_err()
            || uuid::Uuid::parse_str(&operation.correlation_id).is_err()
            || !operation_ids.insert(operation.id.as_str())
            || operation
                .queue_item_id
                .as_deref()
                .is_some_and(|id| !queue_ids.contains(id))
        {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal contained invalid operation references.",
            ));
        }
    }
    for item in &journal.queue {
        if let Some(operation_id) = item.latest_operation_id.as_deref() {
            let Some(operation) = journal
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)
            else {
                return Err(AppError::new(
                    "journal_corrupt",
                    "The application journal contained a missing latest operation reference.",
                ));
            };
            if operation.kind != crate::models::OperationKind::Download
                || operation.queue_item_id.as_deref() != Some(item.id.as_str())
            {
                return Err(AppError::new(
                    "journal_corrupt",
                    "The application journal contained an inconsistent latest operation reference.",
                ));
            }
        }
    }
    Ok(())
}

fn acquire_journal_lock(journal_path: &Path) -> Result<fs::File, AppError> {
    let parent = journal_path
        .parent()
        .ok_or_else(|| AppError::internal("Journal path did not have a parent folder."))?;
    fs::create_dir_all(parent).map_err(|error| {
        AppError::internal("Could not create the application data folder.")
            .with_detail(error.kind().to_string())
    })?;
    let lock_path = journal_path.with_extension("lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(lock_path).map_err(|error| {
        AppError::new(
            "already_running",
            "Another Nuclear Downloader instance is already using this application data.",
        )
        .retryable(true)
        .with_detail(error.kind().to_string())
    })
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let new = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
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
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn protect_for_current_user(plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
    crypt_protect(plaintext, false)
}

#[cfg(windows)]
fn unprotect_for_current_user(ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
    crypt_protect(ciphertext, true)
}

#[cfg(windows)]
fn crypt_protect(input: &[u8], decrypt: bool) -> Result<Vec<u8>, AppError> {
    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    #[link(name = "Crypt32")]
    extern "system" {
        fn CryptProtectData(
            input: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut core::ffi::c_void,
            prompt: *const core::ffi::c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut core::ffi::c_void,
            prompt: *const core::ffi::c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let size = u32::try_from(input.len())
        .map_err(|_| AppError::internal("Journal data exceeded the supported size."))?;
    let input_blob = DataBlob {
        size,
        data: input.as_ptr() as *mut u8,
    };
    let mut output_blob = DataBlob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if decrypt {
            CryptUnprotectData(
                &input_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output_blob,
            )
        } else {
            CryptProtectData(
                &input_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output_blob,
            )
        }
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        if decrypt && error.raw_os_error() == Some(13) {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal could not be decrypted because it is corrupt.",
            ));
        }
        return Err(AppError::new(
            "journal_crypto_failed",
            if decrypt {
                "Windows could not decrypt the application journal."
            } else {
                "Windows could not protect the application journal."
            },
        )
        .with_detail(error.kind().to_string()));
    }

    let result = unsafe {
        let bytes =
            std::slice::from_raw_parts(output_blob.data, output_blob.size as usize).to_vec();
        let _ = LocalFree(output_blob.data.cast());
        bytes
    };
    Ok(result)
}

#[cfg(not(windows))]
fn protect_for_current_user(_plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
    Err(AppError::new(
        "unsupported_platform",
        "Encrypted queue persistence is supported only on Windows.",
    ))
}

#[cfg(not(windows))]
fn unprotect_for_current_user(_ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
    Err(AppError::new(
        "unsupported_platform",
        "Encrypted queue persistence is supported only on Windows.",
    ))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        protect_for_current_user, JournalStore, PersistentJournal, MAX_TERMINAL_ATTEMPTS,
        TERMINAL_RETENTION_MS,
    };
    use crate::models::{
        OperationKind, OperationSnapshot, OperationState, UrlInspection, VideoInfo,
        APP_SCHEMA_VERSION,
    };

    fn operation(index: usize, state: OperationState, updated_at_ms: u64) -> OperationSnapshot {
        OperationSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            id: format!("operation-{index}"),
            queue_item_id: None,
            kind: OperationKind::Download,
            state,
            progress: 0.0,
            phase: None,
            sequence: 0,
            created_at_ms: updated_at_ms,
            updated_at_ms,
            finished_at_ms: state.is_terminal().then_some(updated_at_ms),
            error: None,
            inspection_result: None,
            correlation_id: format!("correlation-{index}"),
        }
    }

    #[test]
    fn pruning_retains_active_and_only_recent_bounded_terminal_attempts() {
        let now = TERMINAL_RETENTION_MS + 10_000;
        let mut journal = PersistentJournal::default();
        journal
            .operations
            .push(operation(0, OperationState::Running, 1));
        for index in 1..=(MAX_TERMINAL_ATTEMPTS + 20) {
            journal
                .operations
                .push(operation(index, OperationState::Completed, now));
        }
        journal
            .operations
            .push(operation(999, OperationState::Completed, 1));

        journal.prune(now);

        assert_eq!(journal.schema_version, APP_SCHEMA_VERSION);
        assert_eq!(journal.operations.len(), MAX_TERMINAL_ATTEMPTS + 1);
        assert!(journal
            .operations
            .iter()
            .any(|item| item.id == "operation-0"));
        assert!(!journal
            .operations
            .iter()
            .any(|item| item.id == "operation-999"));
    }

    #[test]
    fn restart_converts_nonterminal_operations_to_interrupted() {
        let mut journal = PersistentJournal::default();
        journal
            .operations
            .push(operation(1, OperationState::Running, 1));

        journal.normalize_after_restart(25);

        let operation = &journal.operations[0];
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(operation.finished_at_ms, Some(25));
        assert_eq!(operation.error.as_ref().unwrap().code, "interrupted");
    }

    #[test]
    fn persistence_strips_large_transient_inspection_results() {
        let mut journal = PersistentJournal::default();
        let mut completed = operation(1, OperationState::Completed, 25);
        completed.kind = OperationKind::Inspection;
        completed.inspection_result = Some(UrlInspection::Video {
            video: VideoInfo {
                id: "video".to_string(),
                title: "x".repeat(2 * 1024 * 1024),
                duration: None,
                channel: None,
                thumbnail: None,
                url: "https://example.com/video".to_string(),
                available_qualities: vec!["720p".to_string()],
                has_audio: true,
            },
        });
        journal.operations.push(completed);

        let persisted = journal.persistence_copy(25);
        let serialized = serde_json::to_vec(&persisted).unwrap();

        assert!(persisted.operations[0].inspection_result.is_none());
        assert!(serialized.len() < 16 * 1024);
    }

    #[cfg(windows)]
    #[test]
    fn journal_lock_rejects_a_concurrent_instance_and_releases_on_drop() {
        let root = std::env::temp_dir().join(format!("nuclear-lock-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.dpapi");
        let (first, _, _) = JournalStore::open(path.clone()).unwrap();

        let error = JournalStore::open(path.clone()).unwrap_err();
        assert_eq!(error.code, "already_running");

        drop(first);
        assert!(JournalStore::open(path).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn damaged_dpapi_ciphertext_is_quarantined_as_corrupt() {
        for damage in ["truncate", "bitflip"] {
            let root = std::env::temp_dir().join(format!(
                "nuclear-journal-damage-{damage}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("state.dpapi");
            let json = serde_json::to_vec(&PersistentJournal::default()).unwrap();
            let mut protected = protect_for_current_user(&json).unwrap();
            if damage == "truncate" {
                protected.truncate(protected.len() / 2);
            } else {
                let middle = protected.len() / 2;
                protected[middle] ^= 0x5a;
            }
            std::fs::write(&path, protected).unwrap();

            let (_, journal, quarantine) = JournalStore::open(path.clone()).unwrap();

            assert!(journal.queue.is_empty());
            assert!(journal.operations.is_empty());
            assert!(!path.exists());
            assert!(quarantine.unwrap().is_file());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[cfg(windows)]
    #[test]
    fn stale_journal_revision_cannot_overwrite_a_newer_snapshot() {
        let root = std::env::temp_dir().join(format!("nuclear-revision-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.dpapi");
        let (store, _, _) = JournalStore::open(path).unwrap();
        let newer = PersistentJournal {
            revision: 2,
            ..PersistentJournal::default()
        };
        let older = PersistentJournal {
            revision: 1,
            ..PersistentJournal::default()
        };

        store.save(&newer).unwrap();
        store.save(&older).unwrap();

        assert_eq!(store.read().unwrap().revision, 2);
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn structural_validation_rejects_duplicate_operation_ids() {
        let id = uuid::Uuid::new_v4().to_string();
        let correlation = uuid::Uuid::new_v4().to_string();
        let mut first = operation(1, OperationState::Completed, 1);
        first.id.clone_from(&id);
        first.correlation_id.clone_from(&correlation);
        let mut second = first.clone();
        second.correlation_id = uuid::Uuid::new_v4().to_string();
        let journal = PersistentJournal {
            operations: vec![first, second],
            ..PersistentJournal::default()
        };

        assert_eq!(
            super::validate_journal_structure(&journal)
                .unwrap_err()
                .code,
            "journal_corrupt"
        );
    }

    #[cfg(windows)]
    #[test]
    fn future_schema_fails_closed_without_quarantining_the_journal() {
        let root = std::env::temp_dir().join(format!("nuclear-journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        let future = serde_json::json!({
            "schemaVersion": 2,
            "queue": [],
            "operations": [],
        });
        let protected = protect_for_current_user(&serde_json::to_vec(&future).unwrap()).unwrap();
        std::fs::write(&path, protected).unwrap();

        let error = JournalStore::open(path.clone()).unwrap_err();

        assert_eq!(error.code, "journal_migration_required");
        assert!(path.is_file());
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.contains("corrupt-"))
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn malformed_encrypted_json_is_quarantined() {
        let root = std::env::temp_dir().join(format!("nuclear-journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        std::fs::write(&path, protect_for_current_user(b"not-json").unwrap()).unwrap();

        let (_, journal, quarantine) = JournalStore::open(path.clone()).unwrap();

        assert!(journal.queue.is_empty());
        assert!(!path.exists());
        assert!(quarantine.unwrap().is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
