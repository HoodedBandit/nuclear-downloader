use crate::app_error::AppError;
use crate::models::{
    OperationKind, OperationSnapshot, OperationState, PendingAppUpdateRecovery, QueueItemRecord,
    QueueItemState, APP_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
pub(crate) use tests::TestJournalSavePause;

mod platform;
mod validation;

use platform::{
    acquire_journal_lock, atomic_replace, journal_too_large, protect_for_current_user,
    read_encrypted_journal, unprotect_for_current_user,
};
use validation::{validate_journal_structure, validate_serialized_schema, LatestReferencePolicy};

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
        let interrupted_preparations = self
            .operations
            .iter()
            .filter(|operation| {
                operation.kind == OperationKind::Inspection
                    && operation.state == OperationState::Interrupted
                    && operation.queue_item_id.is_some()
            })
            .filter_map(|operation| operation.queue_item_id.as_deref())
            .collect::<HashSet<_>>();
        for item in &mut self.queue {
            if item.preparation == Some(crate::models::QueuePreparation::Pending)
                && interrupted_preparations.contains(item.id.as_str())
                && item.state != QueueItemState::Interrupted
            {
                item.state = QueueItemState::Interrupted;
                item.updated_at_ms = now_ms;
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
        let mut retained_operation_ids = retained_operation_ids(
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
        let queue_ids = self
            .queue
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        retained_operation_ids.extend(
            self.queue
                .iter()
                .filter_map(|item| item.preparation_operation_id.clone()),
        );
        retained_operation_ids.extend(self.operations.iter().filter_map(|operation| {
            operation
                .playlist_admission
                .as_ref()
                .is_some_and(|receipt| {
                    receipt
                        .item_ids
                        .iter()
                        .any(|id| queue_ids.contains(id.as_str()))
                })
                .then_some(operation.id.clone())
        }));
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
    save_attempts: AtomicUsize,
    #[cfg(test)]
    save_pause: Mutex<Option<Arc<TestJournalSavePause>>>,
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
            save_attempts: AtomicUsize::new(0),
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
        self.save_attempts.fetch_add(1, Ordering::SeqCst);
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

    fn read(&self) -> Result<PersistentJournal, AppError> {
        let mut input = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|_| AppError::internal("Could not read the application journal."))?;
        if input
            .metadata()
            .map_err(|_| AppError::internal("Could not inspect the application journal."))?
            .len()
            > MAX_ENCRYPTED_JOURNAL_BYTES
        {
            return Err(journal_too_large());
        }
        let protected = read_encrypted_journal(&mut input)?;
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

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
