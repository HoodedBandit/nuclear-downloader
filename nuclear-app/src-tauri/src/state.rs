mod commit;
mod data;
mod maintenance;
mod operation;
mod queue;
mod reducers;

pub(crate) use self::data::estimate_inspection_allocation;
#[cfg(test)]
use self::data::MAX_RETAINED_INSPECTION_BYTES;
use self::data::{InspectionRetentionBudget, SharedRecord, StateData};
#[cfg(test)]
use self::reducers::MAX_UI_FIELD_BYTES;
#[cfg(test)]
use self::tests::TestCommitPause;
use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
use crate::journal::{JournalStore, PersistentJournal};
use crate::models::{
    AppSnapshot, DownloadProgress, PersistenceHealth, QueueItemRecord, RuntimeReadiness,
    APP_SCHEMA_VERSION,
};
use crate::outbox::{StateOutbox, StateOutboxReader};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub const MAX_QUEUE_ITEMS: usize = 1_000;
pub const MAX_ACTIVE_OPERATIONS: usize = 1_000;

#[derive(Clone)]
pub struct StateStore {
    inner: Arc<StateStoreInner>,
}

struct StateStoreInner {
    state: Mutex<StateData>,
    mutation_gate: Arc<AsyncMutex<()>>,
    journal: Arc<JournalStore>,
    diagnostics: Diagnostics,
    pending_notify: Notify,
    operation_notify: Notify,
    outbox: StateOutbox,
    inspection_budget: Mutex<InspectionRetentionBudget>,
    #[cfg(test)]
    commit_pause: Mutex<Option<Arc<TestCommitPause>>>,
    #[cfg(test)]
    fail_next_finalizer_task: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_finalizer_after_save: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone)]
pub struct QueuedDownload {
    pub operation_id: String,
    pub queue_item: QueueItemRecord,
}

#[derive(Debug, Clone)]
pub enum DownloadTerminalOutcome {
    Completed { filename: Option<String> },
    Cancelled,
    Failed(AppError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizationDurability {
    Persisted,
    Degraded,
}

#[derive(Debug, Clone)]
pub struct FinalizationReceipt {
    pub progress: DownloadProgress,
    pub durability: FinalizationDurability,
}

impl StateStore {
    pub fn open_default() -> Result<Self, AppError> {
        let diagnostics = Diagnostics::open_default()?;
        let (journal, loaded, quarantine) = JournalStore::open_default()?;
        if quarantine.is_some() {
            diagnostics.log(
                "error",
                "journal_quarantined",
                &uuid::Uuid::new_v4().to_string(),
                "A corrupt journal was quarantined and replaced with an empty journal.",
            );
        }
        Ok(Self::from_parts(journal, loaded, diagnostics))
    }

    fn from_parts(
        journal: JournalStore,
        loaded: PersistentJournal,
        diagnostics: Diagnostics,
    ) -> Self {
        let sequence = loaded.revision;
        let pending_app_update = loaded.pending_app_update;
        let queue_order = loaded.queue.iter().map(|item| item.id.clone()).collect();
        let queue = loaded
            .queue
            .into_iter()
            .map(|item| (item.id.clone(), item.into()))
            .collect();
        let operation_order = loaded
            .operations
            .iter()
            .map(|operation| operation.id.clone())
            .collect();
        let operations = loaded
            .operations
            .into_iter()
            .map(|operation| (operation.id.clone(), operation.into()))
            .collect();
        Self {
            inner: Arc::new(StateStoreInner {
                state: Mutex::new(StateData {
                    sequence,
                    queue_order,
                    queue,
                    operation_order,
                    operations,
                    pending_downloads: VecDeque::new(),
                    runtime_readiness: RuntimeReadiness::RepairRequired,
                    maintenance_active: false,
                    draining: false,
                    maintenance_owner: None,
                    persistence_health: PersistenceHealth::default(),
                    persistence_dirty: false,
                    pending_app_update,
                }),
                mutation_gate: Arc::new(AsyncMutex::new(())),
                journal: Arc::new(journal),
                diagnostics,
                pending_notify: Notify::new(),
                operation_notify: Notify::new(),
                outbox: StateOutbox::new(sequence),
                inspection_budget: Mutex::new(InspectionRetentionBudget::default()),
                #[cfg(test)]
                commit_pause: Mutex::new(None),
                #[cfg(test)]
                fail_next_finalizer_task: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                fail_next_finalizer_after_save: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    pub fn snapshot(&self) -> Result<AppSnapshot, AppError> {
        let (
            queue,
            operations,
            runtime_readiness,
            maintenance_active,
            draining,
            persistence_health,
            latest_sequence,
        ) = {
            let state = self.lock()?;
            (
                state
                    .queue_order
                    .iter()
                    .filter_map(|id| state.queue.get(id).cloned())
                    .collect::<Vec<_>>(),
                state
                    .operation_order
                    .iter()
                    .filter_map(|id| state.operations.get(id).cloned())
                    .collect::<Vec<_>>(),
                state.runtime_readiness,
                state.maintenance_active,
                state.draining,
                state.persistence_health.clone(),
                state.sequence,
            )
        };
        Ok(AppSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            queue: queue.into_iter().map(|item| item.snapshot()).collect(),
            operations: operations
                .into_iter()
                .map(|operation| operation.snapshot())
                .collect(),
            runtime_readiness,
            maintenance_active,
            draining,
            persistence_health,
            latest_sequence,
        })
    }

    pub fn queue_item(&self, id: &str) -> Result<QueueItemRecord, AppError> {
        self.lock()?
            .queue
            .get(id)
            .map(SharedRecord::snapshot)
            .ok_or_else(|| AppError::not_found("queue item"))
    }

    pub(crate) fn take_outbox_reader(&self) -> Result<StateOutboxReader, AppError> {
        self.inner.outbox.take_reader()
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.inner.diagnostics
    }

    fn lock(&self) -> Result<MutexGuard<'_, StateData>, AppError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AppError::internal("The application state registry is unavailable."))
    }
}

#[cfg(test)]
mod tests;
