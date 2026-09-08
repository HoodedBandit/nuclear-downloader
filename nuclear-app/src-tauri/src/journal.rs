use crate::app_error::AppError;
use crate::models::{
    OperationKind, OperationSnapshot, OperationState, PendingAppUpdateRecovery, QueueItemRecord,
    QueueItemState, APP_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{Arc, Condvar};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
use tokio::sync::Notify;

pub(crate) const TERMINAL_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub(crate) const MAX_TERMINAL_ATTEMPTS: usize = 200;
pub(crate) const MAX_DECRYPTED_JOURNAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENCRYPTED_JOURNAL_BYTES: u64 = 40 * 1024 * 1024;
const JOURNAL_FILENAME: &str = "state-v1.dpapi";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentJournal {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    pub queue: Vec<QueueItemRecord>,
    pub operations: Vec<OperationSnapshot>,
    #[serde(default)]
    pub pending_app_update: Option<PendingAppUpdateRecovery>,
}

impl Default for PersistentJournal {
    fn default() -> Self {
        Self {
            schema_version: APP_SCHEMA_VERSION,
            revision: 0,
            queue: Vec::new(),
            operations: Vec::new(),
            pending_app_update: None,
        }
    }
}

impl PersistentJournal {
    pub fn normalize_after_restart(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        let pending_app_update_id = self
            .pending_app_update
            .as_ref()
            .map(|pending| pending.operation_id.as_str());
        for item in &mut self.queue {
            if matches!(item.state, QueueItemState::Queued | QueueItemState::Running) {
                item.state = QueueItemState::Interrupted;
                item.updated_at_ms = now_ms;
                changed = true;
            }
        }

        for operation in &mut self.operations {
            if operation.inspection_result.take().is_some() {
                changed = true;
            }
            if !operation.state.is_terminal()
                && Some(operation.id.as_str()) != pending_app_update_id
            {
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
        let retention = self.prune_and_repair_latest_references(now_ms);
        changed || retention.changed()
    }

    pub(crate) fn prune_and_repair_latest_references(
        &mut self,
        now_ms: u64,
    ) -> JournalRetentionChanges {
        let pending_app_update_id = self
            .pending_app_update
            .as_ref()
            .map(|pending| pending.operation_id.as_str());
        let retained_operation_ids = retained_operation_ids(
            self.operations
                .iter()
                .map(|operation| OperationRetentionMetadata {
                    id: &operation.id,
                    state: operation.state,
                    finished_at_ms: operation.finished_at_ms,
                    updated_at_ms: operation.updated_at_ms,
                }),
            pending_app_update_id,
            now_ms,
        );
        let mut removed_operation_ids = Vec::new();
        let mut retained_operations = Vec::new();
        for operation in std::mem::take(&mut self.operations) {
            if retained_operation_ids.contains(&operation.id) {
                retained_operations.push(operation);
            } else {
                removed_operation_ids.push(operation.id);
            }
        }
        retained_operations.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.operations = retained_operations;

        let retained_operation_ids = self
            .operations
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<HashSet<_>>();
        let mut cleared_latest_operation_item_ids = Vec::new();
        for item in &mut self.queue {
            if item
                .latest_operation_id
                .as_deref()
                .is_some_and(|operation_id| !retained_operation_ids.contains(operation_id))
            {
                item.latest_operation_id = None;
                cleared_latest_operation_item_ids.push(item.id.clone());
            }
        }

        JournalRetentionChanges {
            removed_operation_ids,
            cleared_latest_operation_item_ids,
        }
    }

    pub(crate) fn prepare_for_persistence(
        mut self,
        now_ms: u64,
    ) -> Result<PreparedJournal, AppError> {
        // CookieConfig contains only the selected mode/browser or cookies.txt
        // path. Cookie database/file contents are never read into this journal.
        // Inspection payloads are transient capability records. Persisting them
        // would multiply untrusted remote metadata across operation history.
        for operation in &mut self.operations {
            operation.inspection_result = None;
        }
        self.prune_and_repair_latest_references(now_ms);
        validate_journal_structure(&self, LatestReferencePolicy::RequirePresent)?;
        Ok(PreparedJournal { journal: self })
    }
}

pub(crate) struct OperationRetentionMetadata<'a> {
    pub(crate) id: &'a str,
    pub(crate) state: OperationState,
    pub(crate) finished_at_ms: Option<u64>,
    pub(crate) updated_at_ms: u64,
}

pub(crate) fn retained_operation_ids<'a>(
    operations: impl IntoIterator<Item = OperationRetentionMetadata<'a>>,
    pending_app_update_id: Option<&str>,
    now_ms: u64,
) -> HashSet<String> {
    let cutoff = now_ms.saturating_sub(TERMINAL_RETENTION_MS);
    let mut retained = HashSet::new();
    let mut terminal = Vec::new();
    for operation in operations {
        if !operation.state.is_terminal() || Some(operation.id) == pending_app_update_id {
            retained.insert(operation.id.to_string());
        } else if operation.finished_at_ms.unwrap_or(operation.updated_at_ms) >= cutoff {
            terminal.push(operation);
        }
    }
    terminal.sort_by(|left, right| {
        right
            .finished_at_ms
            .unwrap_or(right.updated_at_ms)
            .cmp(&left.finished_at_ms.unwrap_or(left.updated_at_ms))
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.id.cmp(right.id))
    });
    retained.extend(
        terminal
            .into_iter()
            .take(MAX_TERMINAL_ATTEMPTS)
            .map(|operation| operation.id.to_string()),
    );
    retained
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct JournalRetentionChanges {
    pub(crate) removed_operation_ids: Vec<String>,
    pub(crate) cleared_latest_operation_item_ids: Vec<String>,
}

impl JournalRetentionChanges {
    fn changed(&self) -> bool {
        !self.removed_operation_ids.is_empty() || !self.cleared_latest_operation_item_ids.is_empty()
    }
}

#[derive(Debug)]
pub(crate) struct PreparedJournal {
    journal: PersistentJournal,
}

struct TemporaryPublication {
    path: PathBuf,
    published: bool,
}

impl TemporaryPublication {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }

    fn mark_published(&mut self) {
        self.published = true;
    }
}

impl Drop for TemporaryPublication {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
pub struct JournalStore {
    path: PathBuf,
    _lock: fs::File,
    persisted_revision: Mutex<u64>,
    #[cfg(test)]
    fail_saves_remaining: AtomicUsize,
    #[cfg(test)]
    save_pause: Mutex<Option<Arc<TestJournalSavePause>>>,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestJournalSavePause {
    entered: Notify,
    released: Mutex<bool>,
    release_changed: Condvar,
}

#[cfg(test)]
impl TestJournalSavePause {
    pub(crate) async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        if let Ok(mut released) = self.released.lock() {
            *released = true;
            self.release_changed.notify_all();
        }
    }
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
            fail_saves_remaining: AtomicUsize::new(0),
            #[cfg(test)]
            save_pause: Mutex::new(None),
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
        let prepared = journal.clone().prepare_for_persistence(now_ms())?;
        self.save_prepared(prepared)
    }

    pub(crate) fn save_prepared(&self, prepared: PreparedJournal) -> Result<(), AppError> {
        let journal = prepared.journal;
        // Validate the exact representation that will be encrypted and
        // published. PreparedJournal is opaque outside this module, but this
        // second boundary check keeps future internal callers fail-closed.
        validate_journal_structure(&journal, LatestReferencePolicy::RequirePresent)?;
        let mut persisted_revision = self
            .persisted_revision
            .lock()
            .map_err(|_| AppError::internal("The journal writer lock is unavailable."))?;
        if journal.revision <= *persisted_revision {
            return Ok(());
        }
        #[cfg(test)]
        if self
            .fail_saves_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
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

        let json = serde_json::to_vec(&journal)
            .map_err(|_| AppError::internal("Could not serialize the application journal."))?;
        if json.len() > MAX_DECRYPTED_JOURNAL_BYTES {
            return Err(journal_too_large());
        }
        let protected = protect_for_current_user(&json)?;
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let mut temporary_publication = TemporaryPublication::new(temporary.clone());

        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                AppError::internal("Could not create the application journal.")
                    .with_detail(error.kind().to_string())
            })?;
        #[cfg(test)]
        self.pause_save_for_test();
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
            AppError::internal("Could not publish the application journal.")
                .with_detail(error.kind().to_string())
        })?;
        temporary_publication.mark_published();
        *persisted_revision = journal.revision;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_save_for_test(&self) {
        self.fail_saves_for_test(1);
    }

    #[cfg(test)]
    pub(crate) fn fail_saves_for_test(&self, count: usize) {
        self.fail_saves_remaining.store(count, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn pause_next_save_for_test(&self) -> Arc<TestJournalSavePause> {
        let pause = Arc::new(TestJournalSavePause {
            entered: Notify::new(),
            released: Mutex::new(false),
            release_changed: Condvar::new(),
        });
        *self.save_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    fn pause_save_for_test(&self) {
        let pause = self.save_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            let mut released = pause.released.lock().unwrap();
            while !*released {
                released = pause.release_changed.wait(released).unwrap();
            }
        }
    }

    fn read(&self) -> Result<PersistentJournal, AppError> {
        if fs::metadata(&self.path)
            .map_err(|_| AppError::internal("Could not inspect the application journal."))?
            .len()
            > MAX_ENCRYPTED_JOURNAL_BYTES
        {
            return Err(journal_too_large());
        }
        let protected = fs::read(&self.path)
            .map_err(|_| AppError::internal("Could not read the application journal."))?;
        let json = unprotect_for_current_user(&protected)?;
        if json.len() > MAX_DECRYPTED_JOURNAL_BYTES {
            return Err(journal_too_large());
        }
        validate_serialized_schema(&json)?;
        let journal: PersistentJournal = serde_json::from_slice(&json)
            .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
        validate_journal_structure(&journal, LatestReferencePolicy::AllowDangling)?;
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

fn journal_too_large() -> AppError {
    AppError::new(
        "journal_too_large",
        "The saved application state is too large to load safely. The existing journal was preserved.",
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalSchemaHeader {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalRecordSchemaHeaders {
    queue: Vec<RecordSchemaHeader>,
    operations: Vec<RecordSchemaHeader>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordSchemaHeader {
    schema_version: u32,
}

fn validate_serialized_schema(json: &[u8]) -> Result<(), AppError> {
    let header: JournalSchemaHeader = serde_json::from_slice(json)
        .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
    if header.schema_version != APP_SCHEMA_VERSION {
        return Err(journal_migration_required());
    }
    let records: JournalRecordSchemaHeaders = serde_json::from_slice(json)
        .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
    if records
        .queue
        .iter()
        .chain(records.operations.iter())
        .any(|record| record.schema_version != APP_SCHEMA_VERSION)
    {
        return Err(journal_migration_required());
    }
    Ok(())
}

fn journal_migration_required() -> AppError {
    AppError::new(
        "journal_migration_required",
        "The application journal was created by an unsupported version.",
    )
    .retryable(true)
}

#[derive(Clone, Copy)]
enum LatestReferencePolicy {
    AllowDangling,
    RequirePresent,
}

fn validate_journal_structure(
    journal: &PersistentJournal,
    latest_reference_policy: LatestReferencePolicy,
) -> Result<(), AppError> {
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
    if let Some(pending) = &journal.pending_app_update {
        if uuid::Uuid::parse_str(&pending.operation_id).is_err()
            || pending.expected_version.is_empty()
            || pending.expected_version.len() > 128
            || !matches!(
                semver::Version::parse(&pending.expected_version),
                Ok(version)
                    if version.pre.is_empty()
                        && version.build.is_empty()
                        && version.to_string() == pending.expected_version
            )
        {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal contained an invalid pending update record.",
            ));
        }
        let operation = journal
            .operations
            .iter()
            .find(|operation| operation.id == pending.operation_id)
            .ok_or_else(|| {
                AppError::new(
                    "journal_corrupt",
                    "The pending update operation was missing from the application journal.",
                )
            })?;
        if operation.kind != OperationKind::AppUpdate {
            return Err(AppError::new(
                "journal_corrupt",
                "The pending update record referenced the wrong operation kind.",
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
                if matches!(
                    latest_reference_policy,
                    LatestReferencePolicy::AllowDangling
                ) {
                    continue;
                }
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
        protect_for_current_user, JournalStore, LatestReferencePolicy, PersistentJournal,
        PreparedJournal, MAX_DECRYPTED_JOURNAL_BYTES, MAX_ENCRYPTED_JOURNAL_BYTES,
        MAX_TERMINAL_ATTEMPTS, TERMINAL_RETENTION_MS,
    };
    use crate::models::{
        OperationKind, OperationSnapshot, OperationState, PendingAppUpdateRecovery,
        QueueItemRecord, QueueItemState, UrlInspection, VideoInfo, APP_SCHEMA_VERSION,
    };

    fn operation(index: usize, state: OperationState, updated_at_ms: u64) -> OperationSnapshot {
        OperationSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
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
            published_output: None,
            intended_terminal_outcome: None,
            correlation_id: uuid::Uuid::from_u128(index as u128 + 10_000).to_string(),
        }
    }

    fn queue_item(index: usize, latest_operation_id: Option<String>) -> QueueItemRecord {
        QueueItemRecord {
            schema_version: APP_SCHEMA_VERSION,
            id: uuid::Uuid::from_u128(index as u128 + 100_000).to_string(),
            source_url: format!("https://example.com/{index}"),
            title: format!("item-{index}"),
            available_qualities: vec!["720p".to_string()],
            has_audio: true,
            cookie_config: None,
            format: "mp4".to_string(),
            quality: "720p".to_string(),
            output_dir: "C:\\Downloads".to_string(),
            filename_override: None,
            compat_config_path: None,
            state: QueueItemState::Completed,
            latest_operation_id,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn schema_one_journal_defaults_a_missing_pending_update_record() {
        let mut value = serde_json::to_value(PersistentJournal::default()).unwrap();
        value.as_object_mut().unwrap().remove("pendingAppUpdate");

        let journal: PersistentJournal = serde_json::from_value(value).unwrap();

        assert!(journal.pending_app_update.is_none());
        assert_eq!(journal.schema_version, APP_SCHEMA_VERSION);
    }

    #[test]
    fn restart_normalization_and_retention_preserve_pending_app_update_proof() {
        let mut app_update = operation(1, OperationState::Running, 1);
        app_update.kind = OperationKind::AppUpdate;
        app_update.phase = Some("installing".to_string());
        let operation_id = app_update.id.clone();
        let mut journal = PersistentJournal {
            operations: vec![app_update],
            pending_app_update: Some(PendingAppUpdateRecovery {
                operation_id: operation_id.clone(),
                expected_version: "0.6.0".to_string(),
                prepared_at_ms: 1,
            }),
            ..PersistentJournal::default()
        };

        assert!(!journal.normalize_after_restart(TERMINAL_RETENTION_MS + 10));
        assert_eq!(journal.operations[0].state, OperationState::Running);

        journal.operations[0].state = OperationState::Interrupted;
        journal.operations[0].finished_at_ms = Some(1);
        for index in 2..=(MAX_TERMINAL_ATTEMPTS + 5) {
            journal.operations.push(operation(
                index,
                OperationState::Completed,
                TERMINAL_RETENTION_MS + 10,
            ));
        }
        journal.prune_and_repair_latest_references(TERMINAL_RETENTION_MS + 10);

        assert!(journal
            .operations
            .iter()
            .any(|operation| operation.id == operation_id));
        assert!(
            super::validate_journal_structure(&journal, LatestReferencePolicy::RequirePresent)
                .is_ok()
        );
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

        journal.prune_and_repair_latest_references(now);

        assert_eq!(journal.schema_version, APP_SCHEMA_VERSION);
        assert_eq!(journal.operations.len(), MAX_TERMINAL_ATTEMPTS + 1);
        assert!(journal
            .operations
            .iter()
            .any(|item| item.id == operation(0, OperationState::Running, 1).id));
        assert!(!journal
            .operations
            .iter()
            .any(|item| item.id == operation(999, OperationState::Completed, 1).id));
    }

    #[test]
    fn dismiss_repair_clears_a_dangling_latest_operation_reference() {
        let operation = operation(1, OperationState::Completed, 25);
        let mut journal = PersistentJournal {
            queue: vec![queue_item(1, Some(operation.id.clone()))],
            operations: vec![operation.clone()],
            ..PersistentJournal::default()
        };
        journal
            .operations
            .retain(|candidate| candidate.id != operation.id);

        let changes = journal.prune_and_repair_latest_references(25);

        assert_eq!(
            changes.cleared_latest_operation_item_ids,
            vec![journal.queue[0].id.clone()]
        );
        assert!(journal.queue[0].latest_operation_id.is_none());
    }

    #[test]
    fn age_pruning_clears_only_the_reference_to_the_expired_attempt() {
        let now = TERMINAL_RETENTION_MS + 10_000;
        let mut expired = operation(1, OperationState::Completed, 1);
        let mut recent = operation(2, OperationState::Completed, now);
        let expired_item = queue_item(1, Some(expired.id.clone()));
        let recent_item = queue_item(2, Some(recent.id.clone()));
        expired.queue_item_id = Some(expired_item.id.clone());
        recent.queue_item_id = Some(recent_item.id.clone());
        let recent_id = recent.id.clone();
        let mut journal = PersistentJournal {
            queue: vec![expired_item, recent_item],
            operations: vec![expired, recent],
            ..PersistentJournal::default()
        };

        journal.prune_and_repair_latest_references(now);

        assert!(journal.queue[0].latest_operation_id.is_none());
        assert_eq!(
            journal.queue[1].latest_operation_id.as_deref(),
            Some(recent_id.as_str())
        );
    }

    #[test]
    fn count_retention_repairs_references_to_evicted_attempts() {
        let now = 10_000;
        let mut operations = (0..=MAX_TERMINAL_ATTEMPTS)
            .map(|index| operation(index, OperationState::Completed, index as u64 + 1))
            .collect::<Vec<_>>();
        let oldest_id = operations[0].id.clone();
        let newest_id = operations[MAX_TERMINAL_ATTEMPTS].id.clone();
        let mut oldest_item = queue_item(1, Some(oldest_id));
        let mut newest_item = queue_item(2, Some(newest_id.clone()));
        operations[0].queue_item_id = Some(oldest_item.id.clone());
        operations[MAX_TERMINAL_ATTEMPTS].queue_item_id = Some(newest_item.id.clone());
        oldest_item.updated_at_ms = now;
        newest_item.updated_at_ms = now;
        let mut journal = PersistentJournal {
            queue: vec![oldest_item, newest_item],
            operations,
            ..PersistentJournal::default()
        };

        journal.prune_and_repair_latest_references(now);

        assert_eq!(journal.operations.len(), MAX_TERMINAL_ATTEMPTS);
        assert!(journal.queue[0].latest_operation_id.is_none());
        assert_eq!(
            journal.queue[1].latest_operation_id.as_deref(),
            Some(newest_id.as_str())
        );
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
        completed.inspection_result = Some(std::sync::Arc::new(UrlInspection::Video {
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
        }));
        journal.operations.push(completed);

        let persisted = journal.prepare_for_persistence(25).unwrap().journal;
        let serialized = serde_json::to_vec(&persisted).unwrap();

        assert!(persisted.operations[0].inspection_result.is_none());
        assert!(serialized.len() < 16 * 1024);
    }

    #[cfg(windows)]
    #[test]
    fn oversized_save_preserves_the_previous_journal_and_removes_temporary_files() {
        let root =
            std::env::temp_dir().join(format!("nuclear-journal-limit-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.dpapi");
        let (store, _, _) = JournalStore::open(path.clone()).unwrap();
        let previous = PersistentJournal {
            revision: 1,
            ..PersistentJournal::default()
        };
        store.save(&previous).unwrap();

        let mut item = queue_item(1, None);
        item.source_url = "u".repeat(MAX_DECRYPTED_JOURNAL_BYTES + 1);
        let oversized = PersistentJournal {
            revision: 2,
            queue: vec![item],
            ..PersistentJournal::default()
        };
        let error = store.save(&oversized).unwrap_err();

        assert_eq!(error.code, "journal_too_large");
        assert_eq!(store.read().unwrap().revision, 1);
        assert!(std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-")));
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn oversized_existing_journal_is_preserved_without_quarantine() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-journal-load-limit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        let oversized_len = MAX_ENCRYPTED_JOURNAL_BYTES + 1;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(oversized_len).unwrap();

        let error = JournalStore::open(path.clone()).unwrap_err();

        assert_eq!(error.code, "journal_too_large");
        assert!(path.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), oversized_len);
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            2,
            "the oversized journal and its lock should be the only files"
        );
        let _ = std::fs::remove_dir_all(root);
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

    #[cfg(windows)]
    #[test]
    fn dangling_latest_reference_is_repaired_once_and_persisted() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-dangling-reference-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        let dangling_id = uuid::Uuid::new_v4().to_string();
        let journal = PersistentJournal {
            revision: 7,
            queue: vec![queue_item(1, Some(dangling_id))],
            ..PersistentJournal::default()
        };
        let protected = protect_for_current_user(&serde_json::to_vec(&journal).unwrap()).unwrap();
        std::fs::write(&path, protected).unwrap();

        let (first_store, first, first_quarantine) = JournalStore::open(path.clone()).unwrap();
        assert!(first_quarantine.is_none());
        assert!(first.queue[0].latest_operation_id.is_none());
        assert_eq!(first.revision, 8);
        drop(first_store);
        let repaired_bytes = std::fs::read(&path).unwrap();

        let (second_store, second, second_quarantine) = JournalStore::open(path.clone()).unwrap();
        assert!(second_quarantine.is_none());
        assert!(second.queue[0].latest_operation_id.is_none());
        assert_eq!(second.revision, 8);
        assert_eq!(std::fs::read(&path).unwrap(), repaired_bytes);
        drop(second_store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn invalid_prepared_journal_preserves_previous_published_bytes() {
        let root =
            std::env::temp_dir().join(format!("nuclear-invalid-prepared-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.dpapi");
        let (store, _, _) = JournalStore::open(path.clone()).unwrap();
        store
            .save_prepared(
                PersistentJournal {
                    revision: 1,
                    ..PersistentJournal::default()
                }
                .prepare_for_persistence(1)
                .unwrap(),
            )
            .unwrap();
        let previous_bytes = std::fs::read(&path).unwrap();

        let duplicate = operation(1, OperationState::Completed, 2);
        let invalid_candidate = PersistentJournal {
            revision: 2,
            operations: vec![duplicate.clone(), duplicate.clone()],
            ..PersistentJournal::default()
        };
        let prepare_error = invalid_candidate.prepare_for_persistence(2).unwrap_err();
        assert_eq!(prepare_error.code, "journal_corrupt");
        assert_eq!(std::fs::read(&path).unwrap(), previous_bytes);

        let invalid = PreparedJournal {
            journal: PersistentJournal {
                revision: 2,
                operations: vec![duplicate.clone(), duplicate],
                ..PersistentJournal::default()
            },
        };
        let error = store.save_prepared(invalid).unwrap_err();

        assert_eq!(error.code, "journal_corrupt");
        assert_eq!(std::fs::read(&path).unwrap(), previous_bytes);
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
            super::validate_journal_structure(&journal, LatestReferencePolicy::RequirePresent,)
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
            "queue": { "futureLayout": [1, 2, 3] },
            "operations": "future-layout",
        });
        let protected = protect_for_current_user(&serde_json::to_vec(&future).unwrap()).unwrap();
        std::fs::write(&path, &protected).unwrap();

        for _ in 0..2 {
            let error = JournalStore::open(path.clone()).unwrap_err();
            assert_eq!(error.code, "journal_migration_required");
            assert_eq!(std::fs::read(&path).unwrap(), protected);
        }
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
    fn future_record_schema_is_preserved_before_decoding_its_shape() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-journal-record-schema-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        let future = serde_json::json!({
            "schemaVersion": APP_SCHEMA_VERSION,
            "queue": [{
                "schemaVersion": APP_SCHEMA_VERSION + 1,
                "futureRecordLayout": { "nested": true }
            }],
            "operations": [],
        });
        let protected = protect_for_current_user(&serde_json::to_vec(&future).unwrap()).unwrap();
        std::fs::write(&path, &protected).unwrap();

        for _ in 0..2 {
            let error = JournalStore::open(path.clone()).unwrap_err();
            assert_eq!(error.code, "journal_migration_required");
            assert_eq!(std::fs::read(&path).unwrap(), protected);
        }
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
