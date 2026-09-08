use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
#[cfg(test)]
use crate::journal::TestJournalSavePause;
use crate::journal::{
    now_ms, retained_operation_ids, JournalStore, OperationRetentionMetadata, PersistentJournal,
};
use crate::journal_commit::persist_state;
use crate::models::{
    AddQueueItemInput, AppSnapshot, DownloadProgress, IntendedTerminalOutcome, OperationKind,
    OperationSnapshot, OperationState, PendingAppUpdateRecovery, PersistenceHealth,
    PublishedOutput, QueueItemRecord, QueueItemState, QueuePriority, RuntimeReadiness, StateDelta,
    StateDeltaValue, UpdateQueueItemInput, UrlInspection, VideoInfo, APP_SCHEMA_VERSION,
};
#[cfg(test)]
use crate::outbox::StateOutboxStats;
use crate::outbox::{StateOutbox, StateOutboxReader};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::{Deref, DerefMut};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub const MAX_QUEUE_ITEMS: usize = 1_000;
pub const MAX_ACTIVE_OPERATIONS: usize = 1_000;
const MAX_RETAINED_INSPECTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_UI_FIELD_BYTES: usize = 4 * 1024;

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

#[cfg(test)]
pub(crate) struct TestCommitPause {
    entered: Notify,
    release: Notify,
}

#[cfg(test)]
impl TestCommitPause {
    pub(crate) async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Default)]
struct InspectionRetentionBudget {
    retained: HashMap<usize, (Weak<UrlInspection>, usize)>,
}

impl InspectionRetentionBudget {
    fn retain(&mut self, inspection: UrlInspection) -> Result<Arc<UrlInspection>, AppError> {
        self.retained
            .retain(|_, (inspection, _)| inspection.strong_count() != 0);
        let retained_bytes = self
            .retained
            .values()
            .map(|(_, bytes)| *bytes)
            .sum::<usize>();
        let bytes = estimate_inspection_allocation(&inspection);
        if retained_bytes.saturating_add(bytes) > MAX_RETAINED_INSPECTION_BYTES {
            return Err(AppError::new(
                "inspection_retention_limit",
                "Completed inspection results reached the 16 MiB retention limit. Add or dismiss earlier results, then retry.",
            )
            .retryable(true));
        }
        let inspection = Arc::new(inspection);
        self.retained.insert(
            Arc::as_ptr(&inspection) as usize,
            (Arc::downgrade(&inspection), bytes),
        );
        Ok(inspection)
    }
}

#[derive(Clone)]
struct SharedRecord<T>(Arc<T>);

impl<T> SharedRecord<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: Clone> SharedRecord<T> {
    fn snapshot(&self) -> T {
        self.0.as_ref().clone()
    }
}

impl<T> Deref for SharedRecord<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T: Clone> DerefMut for SharedRecord<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<T> From<T> for SharedRecord<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[derive(Clone)]
pub(crate) struct StateData {
    sequence: u64,
    queue_order: Vec<String>,
    queue: HashMap<String, SharedRecord<QueueItemRecord>>,
    operation_order: Vec<String>,
    operations: HashMap<String, SharedRecord<OperationSnapshot>>,
    pending_downloads: VecDeque<String>,
    runtime_readiness: RuntimeReadiness,
    maintenance_active: bool,
    draining: bool,
    maintenance_owner: Option<String>,
    persistence_health: PersistenceHealth,
    persistence_dirty: bool,
    pending_app_update: Option<PendingAppUpdateRecovery>,
}

impl StateData {
    pub(crate) fn persistence_journal(&self) -> PersistentJournal {
        PersistentJournal {
            schema_version: APP_SCHEMA_VERSION,
            revision: self.sequence,
            queue: self
                .queue_order
                .iter()
                .filter_map(|id| self.queue.get(id).map(SharedRecord::snapshot))
                .collect(),
            operations: self
                .operation_order
                .iter()
                .filter_map(|id| {
                    self.operations.get(id).map(|operation| {
                        let mut operation = operation.snapshot();
                        operation.inspection_result = None;
                        operation
                    })
                })
                .collect(),
            pending_app_update: self.pending_app_update.clone(),
        }
    }
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

struct AppliedFinalization {
    deltas: Vec<StateDelta>,
    durability: FinalizationDurability,
    state: OperationState,
    error: Option<AppError>,
    published_output: Option<PublishedOutput>,
}

struct DegradedFinalizationMetadata<'a> {
    candidate_sequence: u64,
    code: &'a str,
    summary: &'a str,
    detail: &'a str,
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

    #[cfg(test)]
    pub fn open_at(journal_path: PathBuf, diagnostics_path: PathBuf) -> Result<Self, AppError> {
        let diagnostics = Diagnostics::open(diagnostics_path)?;
        let (journal, loaded, quarantine) = JournalStore::open(journal_path)?;
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

    #[cfg(test)]
    pub(crate) fn outbox_stats(&self) -> StateOutboxStats {
        self.inner.outbox.stats()
    }

    pub async fn add_queue_item(
        &self,
        input: AddQueueItemInput,
    ) -> Result<(QueueItemRecord, Vec<StateDelta>), AppError> {
        validate_actionable_field("format", &input.format)?;
        validate_actionable_field("quality", &input.quality)?;
        validate_actionable_field("output folder", &input.output_dir)?;
        validate_optional_actionable_field("filename", input.filename_override.as_deref())?;
        validate_optional_actionable_field(
            "compatibility configuration path",
            input.compat_config_path.as_deref(),
        )?;
        validate_cookie_config(input.cookie_config.as_ref())?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.queue.len() >= MAX_QUEUE_ITEMS {
            return Err(AppError::new(
                "queue_limit",
                format!("The queue is limited to {MAX_QUEUE_ITEMS} items."),
            ));
        }
        let inspection = authoritative_inspection_video(&state, &input.inspection_operation_id)?;
        let id = uuid::Uuid::new_v4().to_string();
        let item = QueueItemRecord {
            schema_version: APP_SCHEMA_VERSION,
            id: id.clone(),
            source_url: inspection.url,
            title: inspection.title,
            available_qualities: inspection.available_qualities,
            has_audio: inspection.has_audio,
            cookie_config: input.cookie_config,
            format: input.format,
            quality: input.quality,
            output_dir: input.output_dir,
            filename_override: input.filename_override,
            compat_config_path: input.compat_config_path,
            state: QueueItemState::Inert,
            latest_operation_id: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        state.queue_order.push(id.clone());
        state.queue.insert(id, item.clone().into());
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::QueueItemUpserted(item.clone()),
        ));
        state.operations.remove(&input.inspection_operation_id);
        state
            .operation_order
            .retain(|candidate| candidate != &input.inspection_operation_id);
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::OperationRemoved(input.inspection_operation_id),
        ));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok((item, deltas))
    }

    pub fn completed_inspection_video(&self, operation_id: &str) -> Result<VideoInfo, AppError> {
        let state = self.lock()?;
        authoritative_inspection_video(&state, operation_id)
    }

    pub async fn update_queue_item(
        &self,
        id: &str,
        input: UpdateQueueItemInput,
    ) -> Result<Vec<StateDelta>, AppError> {
        validate_optional_actionable_field("format", input.format.as_deref())?;
        validate_optional_actionable_field("quality", input.quality.as_deref())?;
        validate_optional_actionable_field("output folder", input.output_dir.as_deref())?;
        if let Some(filename) = &input.filename_override {
            validate_optional_actionable_field("filename", filename.as_deref())?;
        }
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut state, mut recovery_deltas, retention_now) = self.durable_candidate()?;
        let item = state
            .queue
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        if !item.state.is_editable() {
            return Err(AppError::new(
                "queue_item_active",
                "A queued or running item cannot be edited.",
            ));
        }
        let item = state
            .queue
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        if let Some(format) = input.format {
            item.format = format;
        }
        if let Some(quality) = input.quality {
            item.quality = quality;
        }
        if let Some(output_dir) = input.output_dir {
            item.output_dir = output_dir;
        }
        if let Some(filename_override) = input.filename_override {
            item.filename_override = filename_override;
        }
        item.updated_at_ms = now_ms();
        let item = item.snapshot();
        let delta = next_delta(&mut state, StateDeltaValue::QueueItemUpserted(item));
        recovery_deltas.push(delta);
        self.commit_candidate(state, mutation, retention_now, recovery_deltas.clone())
            .await?;
        Ok(recovery_deltas)
    }

    pub async fn remove_queue_items(&self, ids: &[String]) -> Result<Vec<StateDelta>, AppError> {
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let removed_ids = ids.iter().map(String::as_str).collect::<HashSet<_>>();
        for id in ids {
            let item = state
                .queue
                .get(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            if !item.state.is_editable() {
                return Err(AppError::new(
                    "queue_item_active",
                    "A queued or running item cannot be removed.",
                ));
            }
        }
        if state.operations.values().any(|operation| {
            operation
                .queue_item_id
                .as_deref()
                .is_some_and(|id| removed_ids.contains(id))
                && !operation.state.is_terminal()
        }) {
            return Err(AppError::new(
                "queue_item_active",
                "A queue item with an active operation cannot be removed.",
            ));
        }
        for id in ids {
            state.queue.remove(id);
            state.queue_order.retain(|candidate| candidate != id);
        }
        let detached_operation_ids = state
            .operation_order
            .iter()
            .filter(|operation_id| {
                state
                    .operations
                    .get(*operation_id)
                    .and_then(|operation| operation.queue_item_id.as_deref())
                    .is_some_and(|item_id| removed_ids.contains(item_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        deltas.reserve(detached_operation_ids.len() + 1);
        for operation_id in detached_operation_ids {
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.queue_item_id = None;
                operation.updated_at_ms = now_ms();
                let operation = operation.snapshot();
                deltas.push(next_operation_delta(&mut state, operation));
            }
        }
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::QueueItemsRemoved(ids.to_vec()),
        ));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok(deltas)
    }

    pub async fn enqueue(
        &self,
        ids: &[String],
        priority: QueuePriority,
    ) -> Result<(Vec<QueuedDownload>, Vec<StateDelta>), AppError> {
        if priority == QueuePriority::Front && ids.len() != 1 {
            return Err(AppError::invalid(
                "Front priority accepts exactly one queue item.",
            ));
        }
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.maintenance_active || state.draining {
            return Err(AppError::busy(
                "New work is paused while maintenance or cancellation is active.",
            ));
        }
        ensure_operation_capacity(&state, ids.len(), MAX_ACTIVE_OPERATIONS)?;

        let mut unique_ids = HashSet::with_capacity(ids.len());
        for id in ids {
            if !unique_ids.insert(id) {
                return Err(AppError::invalid(
                    "A queue item was requested more than once.",
                ));
            }
            let item = state
                .queue
                .get(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            if matches!(item.state, QueueItemState::Queued | QueueItemState::Running) {
                return Err(AppError::new(
                    "queue_item_active",
                    "The queue item already has an active attempt.",
                ));
            }
        }

        let mut work = Vec::with_capacity(ids.len());
        deltas.reserve(ids.len() * 2);
        for id in ids {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let item = state
                .queue
                .get_mut(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            item.state = QueueItemState::Queued;
            item.latest_operation_id = Some(operation_id.clone());
            item.updated_at_ms = now;
            let item = item.snapshot();
            deltas.push(next_delta(
                &mut state,
                StateDeltaValue::QueueItemUpserted(item.clone()),
            ));

            let operation = OperationSnapshot {
                schema_version: APP_SCHEMA_VERSION,
                id: operation_id.clone(),
                queue_item_id: Some(id.clone()),
                kind: OperationKind::Download,
                state: OperationState::Queued,
                progress: 0.0,
                phase: None,
                sequence: 0,
                created_at_ms: now,
                updated_at_ms: now,
                finished_at_ms: None,
                error: None,
                inspection_result: None,
                published_output: None,
                intended_terminal_outcome: None,
                correlation_id: uuid::Uuid::new_v4().to_string(),
            };
            state.operation_order.push(operation_id.clone());
            state
                .operations
                .insert(operation_id.clone(), operation.clone().into());
            if priority == QueuePriority::Front {
                state.pending_downloads.push_front(operation_id.clone());
            } else {
                state.pending_downloads.push_back(operation_id.clone());
            }
            deltas.push(next_operation_delta(&mut state, operation));
            work.push(QueuedDownload {
                operation_id,
                queue_item: item,
            });
        }
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        self.inner.pending_notify.notify_waiters();
        Ok((work, deltas))
    }

    pub async fn begin_operation(
        &self,
        kind: OperationKind,
        queue_item_id: Option<String>,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        self.begin_operation_with_limit(kind, queue_item_id, MAX_ACTIVE_OPERATIONS)
            .await
    }

    async fn begin_operation_with_limit(
        &self,
        kind: OperationKind,
        queue_item_id: Option<String>,
        limit: usize,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.maintenance_active || state.draining {
            return Err(AppError::busy("New work is temporarily paused."));
        }
        ensure_operation_capacity(&state, 1, limit)?;
        let id = uuid::Uuid::new_v4().to_string();
        let operation = OperationSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            id: id.clone(),
            queue_item_id,
            kind,
            state: OperationState::Queued,
            progress: 0.0,
            phase: None,
            sequence: 0,
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
            error: None,
            inspection_result: None,
            published_output: None,
            intended_terminal_outcome: None,
            correlation_id: uuid::Uuid::new_v4().to_string(),
        };
        state.operation_order.push(id.clone());
        state.operations.insert(id, operation.clone().into());
        let delta = next_operation_delta(&mut state, operation.clone());
        deltas.push(delta);
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok((operation, deltas))
    }

    #[cfg(test)]
    pub async fn begin_maintenance_operation(
        &self,
        kind: OperationKind,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        self.begin_maintenance_operation_with_id(kind, uuid::Uuid::new_v4().to_string())
            .await
    }

    pub(crate) async fn begin_maintenance_operation_with_id(
        &self,
        kind: OperationKind,
        operation_id: String,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        if !matches!(
            kind,
            OperationKind::AppUpdate | OperationKind::RuntimeUpdate
        ) {
            return Err(AppError::invalid(
                "Only update operations may acquire maintenance atomically.",
            ));
        }
        uuid::Uuid::parse_str(&operation_id)
            .map_err(|_| AppError::invalid("Invalid operation ID."))?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        if self.lock()?.operations.contains_key(&operation_id) {
            return Err(AppError::new(
                "duplicate_operation",
                "An operation with this ID is already registered.",
            ));
        }
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.maintenance_active || state.draining {
            return Err(AppError::busy("Maintenance is already active."));
        }
        if state
            .operations
            .values()
            .any(|operation| !operation.state.is_terminal())
        {
            return Err(AppError::busy(
                "Finish or cancel queued and active operations before installing updates.",
            ));
        }
        state.maintenance_active = true;
        let operation = OperationSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            id: operation_id.clone(),
            queue_item_id: None,
            kind,
            state: OperationState::Queued,
            progress: 0.0,
            phase: None,
            sequence: 0,
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
            error: None,
            inspection_result: None,
            published_output: None,
            intended_terminal_outcome: None,
            correlation_id: uuid::Uuid::new_v4().to_string(),
        };
        state.operation_order.push(operation_id.clone());
        state
            .operations
            .insert(operation_id, operation.clone().into());
        state.maintenance_owner = Some(operation.id.clone());
        let operation_delta = next_operation_delta(&mut state, operation.clone());
        let maintenance_delta = next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged {
                active: true,
                draining: false,
            },
        );
        deltas.extend([operation_delta, maintenance_delta]);
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok((operation, deltas))
    }

    pub async fn prepare_app_update_handoff(
        &self,
        operation_id: &str,
        expected_version: &str,
    ) -> Result<Vec<StateDelta>, AppError> {
        let expected_version = canonical_app_version(expected_version)?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        if let Some(pending) = &self.lock()?.pending_app_update {
            if pending.operation_id == operation_id && pending.expected_version == expected_version
            {
                return Ok(Vec::new());
            }
            return Err(AppError::busy(
                "Another application update handoff is already pending.",
            ));
        }
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let now = now_ms();
        let operation = state
            .operations
            .get_mut(operation_id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.kind != OperationKind::AppUpdate {
            return Err(AppError::new(
                "invalid_app_update_operation",
                "Only an application update may prepare an installer handoff.",
            ));
        }
        if operation.state.is_terminal() || operation.state == OperationState::Cancelling {
            return Err(AppError::new(
                "app_update_handoff_unavailable",
                "A finished or cancelling application update cannot launch an installer.",
            ));
        }
        operation.state = OperationState::Running;
        operation.phase = Some("installing".to_string());
        operation.updated_at_ms = now;
        operation.error = None;
        let operation = operation.snapshot();
        state.pending_app_update = Some(PendingAppUpdateRecovery {
            operation_id: operation_id.to_string(),
            expected_version,
            prepared_at_ms: now,
        });
        deltas.push(next_operation_delta(&mut state, operation));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok(deltas)
    }

    pub async fn reconcile_pending_app_update(
        &self,
        current_version: &str,
    ) -> Result<Vec<StateDelta>, AppError> {
        let current_version = canonical_app_version(current_version)?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        if self.lock()?.pending_app_update.is_none() {
            return Ok(Vec::new());
        }
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let pending = state
            .pending_app_update
            .clone()
            .ok_or_else(|| AppError::internal("The pending update record disappeared."))?;
        let now = now_ms();
        let operation = state
            .operations
            .get_mut(&pending.operation_id)
            .ok_or_else(|| AppError::not_found("pending application update operation"))?;
        if operation.kind != OperationKind::AppUpdate {
            return Err(AppError::new(
                "invalid_app_update_operation",
                "The pending installer handoff referenced the wrong operation kind.",
            ));
        }
        if current_version == pending.expected_version {
            operation.state = OperationState::Completed;
            operation.progress = 100.0;
            operation.error = None;
        } else {
            operation.state = OperationState::Interrupted;
            operation.error = Some(
                AppError::new(
                    "app_update_handoff_interrupted",
                    "The requested application update was not installed. You can retry the update.",
                )
                .retryable(true)
                .with_detail(format!(
                    "Expected version {}; the running version is {}.",
                    pending.expected_version, current_version
                )),
            );
        }
        operation.phase = None;
        operation.updated_at_ms = now;
        operation.finished_at_ms = Some(now);
        let operation = operation.snapshot();
        state.pending_app_update = None;
        deltas.push(next_operation_delta(&mut state, operation));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok(deltas)
    }

    pub async fn set_operation_state(
        &self,
        id: &str,
        operation_state: OperationState,
        error: Option<AppError>,
    ) -> Result<Vec<StateDelta>, AppError> {
        if operation_state.is_terminal() {
            return self.finalize_operation(id, operation_state, error).await;
        }
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock()?;
        let deltas = apply_operation_transition(
            &mut state,
            id,
            operation_state,
            error,
            None,
            None,
            now_ms(),
        )?;
        let latest_sequence = state.sequence;
        drop(state);
        self.inner.outbox.enqueue(deltas.clone(), latest_sequence);
        self.inner.operation_notify.notify_waiters();
        Ok(deltas)
    }

    pub async fn finalize_operation(
        &self,
        id: &str,
        operation_state: OperationState,
        error: Option<AppError>,
    ) -> Result<Vec<StateDelta>, AppError> {
        if !operation_state.is_terminal() {
            return Err(AppError::invalid(
                "Operation finalization requires a terminal state.",
            ));
        }
        let applied = self
            .finalize_operation_inner(id, operation_state, error, None, None)
            .await?;
        Ok(applied.deltas)
    }

    pub async fn complete_inspection(
        &self,
        id: &str,
        inspection: crate::models::UrlInspection,
    ) -> Result<Vec<StateDelta>, AppError> {
        let inspection = normalize_inspection(inspection)?;
        let inspection = self
            .inner
            .inspection_budget
            .lock()
            .map_err(|_| AppError::internal("The inspection retention budget is unavailable."))?
            .retain(inspection)?;
        let applied = self
            .finalize_operation_inner(id, OperationState::Completed, None, Some(inspection), None)
            .await?;
        Ok(applied.deltas)
    }

    pub async fn request_cancellation(&self, id: &str) -> Result<StateDelta, AppError> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock()?;
        let operation = state
            .operations
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.state.is_terminal() {
            return Err(AppError::new(
                "already_terminal",
                "The operation has already finished.",
            ));
        }
        operation.state = OperationState::Cancelling;
        operation.updated_at_ms = now_ms();
        operation.error = None;
        let operation = operation.snapshot();
        let delta = next_operation_delta(&mut state, operation);
        let latest_sequence = state.sequence;
        drop(state);
        self.inner
            .outbox
            .enqueue(vec![delta.clone()], latest_sequence);
        Ok(delta)
    }

    pub async fn apply_download_progress(
        &self,
        operation_id: &str,
        progress: &DownloadProgress,
    ) -> Result<Option<Vec<StateDelta>>, AppError> {
        if matches!(
            progress.status.as_str(),
            "completed" | "cancelled" | "error"
        ) {
            return Err(AppError::new(
                "terminal_progress_requires_finalization",
                "Terminal download progress must use the durable finalization path.",
            ));
        }
        let operation_state = if progress.status == "queued" {
            OperationState::Queued
        } else {
            OperationState::Running
        };
        let _mutation = self.inner.mutation_gate.lock().await;
        let now = now_ms();
        let mut state = self.lock()?;
        let queue_item_id = {
            let Some(operation) = state.operations.get_mut(operation_id) else {
                return Ok(None);
            };
            if operation.state.is_terminal() || operation.state == OperationState::Cancelling {
                return Ok(None);
            }
            operation.state = operation_state;
            operation.progress = progress.progress.clamp(0.0, 100.0);
            operation.phase = progress.phase.clone();
            if let Some(phase) = &mut operation.phase {
                truncate_display_field(phase);
            }
            operation.error = None;
            operation.updated_at_ms = now;
            operation.queue_item_id.clone()
        };
        let operation = state
            .operations
            .get(operation_id)
            .map(SharedRecord::snapshot)
            .unwrap();
        let mut deltas = vec![next_operation_delta(&mut state, operation)];
        if let Some(queue_item_id) = queue_item_id {
            if let Some(item) = state.queue.get_mut(&queue_item_id) {
                item.state = queue_state_for_operation(operation_state);
                item.updated_at_ms = now;
                let item = item.snapshot();
                deltas.push(next_delta(
                    &mut state,
                    StateDeltaValue::QueueItemUpserted(item),
                ));
            }
        }
        let latest_sequence = state.sequence;
        drop(state);
        self.inner.outbox.enqueue(deltas.clone(), latest_sequence);
        Ok(Some(deltas))
    }

    pub async fn finalize_download(
        &self,
        operation_id: &str,
        outcome: DownloadTerminalOutcome,
    ) -> Result<FinalizationReceipt, AppError> {
        if self.operation_kind(operation_id) != Some(OperationKind::Download) {
            return Err(AppError::new(
                "invalid_download_operation",
                "Only a download operation may use download finalization.",
            ));
        }
        let (terminal_state, error, published_output) = match outcome {
            DownloadTerminalOutcome::Completed { filename } => (
                OperationState::Completed,
                None,
                filename.map(|path| PublishedOutput {
                    path,
                    recorded_at_ms: now_ms(),
                }),
            ),
            DownloadTerminalOutcome::Cancelled => (OperationState::Cancelled, None, None),
            DownloadTerminalOutcome::Failed(error) => (OperationState::Failed, Some(error), None),
        };
        let applied = self
            .finalize_operation_inner(operation_id, terminal_state, error, None, published_output)
            .await?;
        let progress = terminal_progress_from_finalization(operation_id, &applied);
        Ok(FinalizationReceipt {
            progress,
            durability: applied.durability,
        })
    }

    async fn finalize_operation_inner(
        &self,
        id: &str,
        operation_state: OperationState,
        error: Option<AppError>,
        inspection_result: Option<Arc<UrlInspection>>,
        published_output: Option<PublishedOutput>,
    ) -> Result<AppliedFinalization, AppError> {
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut candidate, mut deltas, retention_now) = self.durable_candidate()?;
        if operation_state == OperationState::Completed
            && candidate
                .pending_app_update
                .as_ref()
                .is_some_and(|pending| pending.operation_id == id)
        {
            return Err(AppError::new(
                "app_update_reconciliation_required",
                "An installer handoff can complete only after the installed version is verified on restart.",
            ));
        }
        let now = now_ms();
        let intended_outcome = IntendedTerminalOutcome {
            state: operation_state,
            error: error.clone(),
        };
        let mut transition_deltas = apply_operation_transition(
            &mut candidate,
            id,
            operation_state,
            error,
            inspection_result,
            published_output.clone(),
            now,
        )?;
        clear_pending_app_update(&mut candidate, id, operation_state);
        let operation = candidate
            .operations
            .get(id)
            .map(SharedRecord::snapshot)
            .ok_or_else(|| AppError::not_found("operation"))?;
        let failure = failure_diagnostic(&operation);
        deltas.append(&mut transition_deltas);
        deltas.extend(prune_live_operations(&mut candidate, now));

        let store = self.clone();
        let operation_id = id.to_string();
        let compensation_operation_id = operation_id.clone();
        let compensation_intended_outcome = intended_outcome.clone();
        let compensation_published_output = published_output.clone();
        let compensation_sequence = candidate.sequence;
        let finalizer = tokio::spawn(async move {
            let _mutation = mutation;
            #[cfg(test)]
            if store
                .inner
                .fail_next_finalizer_task
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                panic!("injected finalizer task failure");
            }
            match store
                .persist_candidate_with_retry(&candidate, retention_now)
                .await
            {
                Ok(()) => {
                    #[cfg(test)]
                    if store
                        .inner
                        .fail_next_finalizer_after_save
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        panic!("injected finalizer task failure after save");
                    }
                    let latest_sequence = candidate.sequence;
                    store.install_candidate(candidate)?;
                    store.inner.outbox.enqueue(deltas.clone(), latest_sequence);
                    if let Some((correlation_id, message)) = failure {
                        store.inner.diagnostics.log(
                            "error",
                            "operation_failed",
                            &correlation_id,
                            &message,
                        );
                    }
                    store.inner.operation_notify.notify_waiters();
                    Ok(AppliedFinalization {
                        deltas,
                        durability: FinalizationDurability::Persisted,
                        state: operation_state,
                        error: operation.error,
                        published_output,
                    })
                }
                Err(save_error) => {
                    let applied = store.install_degraded_finalization(
                        &operation_id,
                        intended_outcome,
                        published_output,
                        DegradedFinalizationMetadata {
                            candidate_sequence: candidate.sequence,
                            code: "state_persistence_failed",
                            summary:
                                "The operation finished, but its final state could not be saved.",
                            detail: &save_error.summary,
                        },
                    )?;
                    store.inner.diagnostics.log(
                        "error",
                        "terminal_state_persistence_failed",
                        &save_error.correlation_id,
                        &save_error.summary,
                    );
                    Ok(applied)
                }
            }
        });
        match finalizer.await {
            Ok(result) => result,
            Err(join_error) => {
                let _mutation = self.inner.mutation_gate.lock().await;
                self.inner.diagnostics.log(
                    "error",
                    "terminal_finalizer_task_failed",
                    &uuid::Uuid::new_v4().to_string(),
                    &join_error.to_string(),
                );
                self.install_degraded_finalization(
                    &compensation_operation_id,
                    compensation_intended_outcome,
                    compensation_published_output,
                    DegradedFinalizationMetadata {
                        candidate_sequence: compensation_sequence,
                        code: "state_finalizer_failed",
                        summary:
                            "The operation finished, but its final state worker stopped unexpectedly.",
                        detail: "The final state will be saved by the next durable command.",
                    },
                )
            }
        }
    }

    fn install_degraded_finalization(
        &self,
        operation_id: &str,
        intended_outcome: IntendedTerminalOutcome,
        published_output: Option<PublishedOutput>,
        metadata: DegradedFinalizationMetadata<'_>,
    ) -> Result<AppliedFinalization, AppError> {
        let mut fallback = self.lock()?.clone();
        fallback.sequence = fallback.sequence.max(metadata.candidate_sequence);
        let degraded_error = AppError::new(metadata.code, metadata.summary)
            .retryable(true)
            .with_detail(metadata.detail);
        let operation = fallback
            .operations
            .get_mut(operation_id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        operation.intended_terminal_outcome = Some(Box::new(intended_outcome));
        let mut deltas = apply_operation_transition(
            &mut fallback,
            operation_id,
            OperationState::Failed,
            Some(degraded_error.clone()),
            None,
            published_output.clone(),
            now_ms(),
        )?;
        clear_pending_app_update(&mut fallback, operation_id, OperationState::Failed);
        fallback.persistence_dirty = true;
        fallback.persistence_health = PersistenceHealth {
            degraded: true,
            error: Some(degraded_error.clone()),
        };
        let health = fallback.persistence_health.clone();
        deltas.push(next_delta(
            &mut fallback,
            StateDeltaValue::PersistenceHealthChanged(health),
        ));
        let latest_sequence = fallback.sequence;
        self.install_candidate(fallback)?;
        self.inner.outbox.enqueue(deltas.clone(), latest_sequence);
        self.inner.operation_notify.notify_waiters();
        Ok(AppliedFinalization {
            deltas,
            durability: FinalizationDurability::Degraded,
            state: OperationState::Failed,
            error: Some(degraded_error),
            published_output,
        })
    }

    pub async fn dismiss_operation(&self, id: &str) -> Result<Vec<StateDelta>, AppError> {
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let operation = state
            .operations
            .get(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if !operation.state.is_terminal() {
            return Err(AppError::new(
                "operation_active",
                "An active operation cannot be dismissed.",
            ));
        }
        state.operations.remove(id);
        state.operation_order.retain(|candidate| candidate != id);
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::OperationRemoved(id.to_string()),
        ));
        let affected_items = state
            .queue
            .iter()
            .filter_map(|(item_id, item)| {
                (item.latest_operation_id.as_deref() == Some(id)).then_some(item_id.clone())
            })
            .collect::<Vec<_>>();
        for item_id in affected_items {
            if let Some(item) = state.queue.get_mut(&item_id) {
                item.latest_operation_id = None;
                item.updated_at_ms = now_ms();
                let item = item.snapshot();
                deltas.push(next_delta(
                    &mut state,
                    StateDeltaValue::QueueItemUpserted(item),
                ));
            }
        }
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok(deltas)
    }

    pub async fn set_maintenance(
        &self,
        active: bool,
        draining: bool,
    ) -> Result<StateDelta, AppError> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock()?;
        if state.maintenance_owner.is_some() {
            return Err(AppError::busy(
                "Update maintenance is owned by an active operation.",
            ));
        }
        state.maintenance_active = active;
        state.draining = draining;
        let delta = next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged { active, draining },
        );
        let latest_sequence = state.sequence;
        drop(state);
        self.inner
            .outbox
            .enqueue(vec![delta.clone()], latest_sequence);
        if !active && !draining {
            self.inner.pending_notify.notify_waiters();
        }
        Ok(delta)
    }

    pub async fn end_maintenance_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<StateDelta>, AppError> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock()?;
        if state.maintenance_owner.as_deref() != Some(operation_id) {
            return Ok(None);
        }
        state.maintenance_owner = None;
        state.maintenance_active = false;
        state.draining = false;
        let delta = next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged {
                active: false,
                draining: false,
            },
        );
        let latest_sequence = state.sequence;
        drop(state);
        self.inner
            .outbox
            .enqueue(vec![delta.clone()], latest_sequence);
        self.inner.pending_notify.notify_waiters();
        Ok(Some(delta))
    }

    pub async fn wait_for_terminal(
        &self,
        operation_id: &str,
        timeout: std::time::Duration,
    ) -> Result<OperationSnapshot, AppError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.inner.operation_notify.notified();
            let operation = self
                .lock()?
                .operations
                .get(operation_id)
                .map(SharedRecord::snapshot)
                .ok_or_else(|| AppError::not_found("operation"))?;
            if operation.state.is_terminal() {
                return Ok(operation);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() || tokio::time::timeout(remaining, notified).await.is_err() {
                return Err(AppError::new(
                    "cancellation_timeout",
                    "The operation is still cancelling while process cleanup completes.",
                )
                .retryable(true));
            }
        }
    }

    pub async fn set_runtime_readiness(
        &self,
        readiness: RuntimeReadiness,
    ) -> Result<StateDelta, AppError> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock()?;
        state.runtime_readiness = readiness;
        let delta = next_delta(
            &mut state,
            StateDeltaValue::RuntimeReadinessChanged(readiness),
        );
        let latest_sequence = state.sequence;
        drop(state);
        self.inner
            .outbox
            .enqueue(vec![delta.clone()], latest_sequence);
        Ok(delta)
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.lock()
            .map(|state| state.pending_downloads.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn wait_pending_available(&self) {
        loop {
            let notified = self.inner.pending_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .lock()
                .is_ok_and(|state| pending_download_available(&state))
            {
                return;
            }
            notified.await;
        }
    }

    pub async fn take_next_pending(&self) -> Option<QueuedDownload> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock().ok()?;
        while let Some(operation_id) = state.pending_downloads.pop_front() {
            let Some(operation) = state.operations.get(&operation_id) else {
                continue;
            };
            if operation.state != OperationState::Queued {
                continue;
            }
            let Some(queue_item_id) = operation.queue_item_id.as_ref() else {
                continue;
            };
            let Some(queue_item) = state.queue.get(queue_item_id).map(SharedRecord::snapshot)
            else {
                continue;
            };
            return Some(QueuedDownload {
                operation_id,
                queue_item,
            });
        }
        None
    }

    pub async fn cancel_pending(&self, operation_id: &str) -> bool {
        let _mutation = self.inner.mutation_gate.lock().await;
        let Ok(mut state) = self.lock() else {
            return false;
        };
        let before = state.pending_downloads.len();
        state
            .pending_downloads
            .retain(|candidate| candidate != operation_id);
        before != state.pending_downloads.len()
    }

    pub fn operation_state(&self, operation_id: &str) -> Option<OperationState> {
        self.lock()
            .ok()?
            .operations
            .get(operation_id)
            .map(|operation| operation.state)
    }

    pub fn operation_kind(&self, operation_id: &str) -> Option<OperationKind> {
        self.lock()
            .ok()?
            .operations
            .get(operation_id)
            .map(|operation| operation.kind)
    }

    pub fn journaled_output_roots(&self) -> Vec<String> {
        let Ok(state) = self.lock() else {
            return Vec::new();
        };
        let mut roots = state
            .queue
            .values()
            .map(|item| item.output_dir.clone())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.inner.diagnostics
    }

    #[cfg(test)]
    fn fail_next_persistence_for_test(&self) {
        self.inner.journal.fail_next_save_for_test();
    }

    #[cfg(test)]
    fn fail_persistence_for_test(&self, count: usize) {
        self.inner.journal.fail_saves_for_test(count);
    }

    #[cfg(test)]
    fn fail_next_finalizer_task_for_test(&self) {
        self.inner
            .fail_next_finalizer_task
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    fn fail_next_finalizer_after_save_for_test(&self) {
        self.inner
            .fail_next_finalizer_after_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn pause_next_commit_for_test(&self) -> Arc<TestCommitPause> {
        let pause = Arc::new(TestCommitPause {
            entered: Notify::new(),
            release: Notify::new(),
        });
        *self.inner.commit_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn pause_next_journal_save_for_test(&self) -> Arc<TestJournalSavePause> {
        self.inner.journal.pause_next_save_for_test()
    }

    #[cfg(test)]
    async fn pause_commit_for_test(&self) {
        let pause = self.inner.commit_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, StateData>, AppError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AppError::internal("The application state registry is unavailable."))
    }

    fn durable_candidate(&self) -> Result<(StateData, Vec<StateDelta>, u64), AppError> {
        let mut candidate = self.lock()?.clone();
        let mut deltas = Vec::new();
        let retention_now = now_ms();
        if candidate.persistence_dirty || candidate.persistence_health.degraded {
            candidate.persistence_dirty = false;
            candidate.persistence_health = PersistenceHealth::default();
            deltas.push(next_delta(
                &mut candidate,
                StateDeltaValue::PersistenceHealthChanged(PersistenceHealth::default()),
            ));
        }
        deltas.extend(prune_live_operations(&mut candidate, retention_now));
        Ok((candidate, deltas, retention_now))
    }

    fn install_candidate(&self, candidate: StateData) -> Result<(), AppError> {
        *self.lock()? = candidate;
        Ok(())
    }

    async fn persist_candidate(
        &self,
        candidate: Arc<StateData>,
        retention_now: u64,
    ) -> Result<(), AppError> {
        let store = self.inner.journal.clone();
        let result = persist_state(store, candidate, retention_now).await;
        result.inspect_err(|error| {
            self.inner.diagnostics.log(
                "error",
                "journal_write_failed",
                &error.correlation_id,
                &error.summary,
            );
        })
    }

    async fn commit_candidate(
        &self,
        candidate: StateData,
        mutation: tokio::sync::OwnedMutexGuard<()>,
        retention_now: u64,
        publication: Vec<StateDelta>,
    ) -> Result<(), AppError> {
        let store = self.clone();
        tokio::spawn(async move {
            let _mutation = mutation;
            #[cfg(test)]
            store.pause_commit_for_test().await;
            store
                .persist_candidate(Arc::new(candidate.clone()), retention_now)
                .await?;
            let latest_sequence = candidate.sequence;
            store.install_candidate(candidate)?;
            store.inner.outbox.enqueue(publication, latest_sequence);
            Ok(())
        })
        .await
        .map_err(|_| AppError::internal("The state commit worker stopped unexpectedly."))?
    }

    async fn persist_candidate_with_retry(
        &self,
        candidate: &StateData,
        retention_now: u64,
    ) -> Result<(), AppError> {
        let candidate = Arc::new(candidate.clone());
        let mut last_error = None;
        for delay in [None, Some(250u64), Some(1_000u64)] {
            if let Some(delay_ms) = delay {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            match self
                .persist_candidate(candidate.clone(), retention_now)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| AppError::internal("The application journal could not be saved.")))
    }
}

fn apply_operation_transition(
    state: &mut StateData,
    id: &str,
    operation_state: OperationState,
    error: Option<AppError>,
    inspection_result: Option<Arc<UrlInspection>>,
    published_output: Option<PublishedOutput>,
    now: u64,
) -> Result<Vec<StateDelta>, AppError> {
    let queue_item_id = {
        let operation = state
            .operations
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.state.is_terminal() && operation.state != operation_state {
            return Err(AppError::new(
                "invalid_transition",
                "A terminal operation cannot change state.",
            ));
        }
        let published_completion = operation_state == OperationState::Completed
            && published_output.is_some()
            && operation.kind == OperationKind::Download;
        if operation.state == OperationState::Cancelling
            && !matches!(
                operation_state,
                OperationState::Cancelling | OperationState::Cancelled | OperationState::Failed
            )
            && !published_completion
        {
            return Err(AppError::new(
                "invalid_transition",
                "A cancelling operation cannot be started again.",
            ));
        }
        if inspection_result.is_some() && operation.kind != OperationKind::Inspection {
            return Err(AppError::new(
                "invalid_inspection_operation",
                "Only an inspection operation may store an inspection result.",
            ));
        }
        operation.state = operation_state;
        operation.updated_at_ms = now;
        operation.error = error;
        if let Some(inspection_result) = inspection_result {
            operation.inspection_result = Some(inspection_result);
        }
        if let Some(published_output) = published_output {
            operation.published_output = Some(published_output);
        }
        if operation_state.is_terminal() {
            operation.finished_at_ms = Some(now);
            operation.phase = None;
            if operation_state == OperationState::Completed {
                operation.progress = 100.0;
            }
        }
        operation.queue_item_id.clone()
    };

    let operation = state
        .operations
        .get(id)
        .map(SharedRecord::snapshot)
        .unwrap();
    let mut deltas = vec![next_operation_delta(state, operation)];
    if let Some(queue_item_id) = queue_item_id {
        if let Some(item) = state.queue.get_mut(&queue_item_id) {
            item.state = queue_state_for_operation(operation_state);
            item.updated_at_ms = now;
            let item = item.snapshot();
            deltas.push(next_delta(state, StateDeltaValue::QueueItemUpserted(item)));
        }
    }
    Ok(deltas)
}

fn queue_state_for_operation(state: OperationState) -> QueueItemState {
    match state {
        OperationState::Queued | OperationState::Starting => QueueItemState::Queued,
        OperationState::Running | OperationState::Cancelling => QueueItemState::Running,
        OperationState::Completed => QueueItemState::Completed,
        OperationState::Failed => QueueItemState::Failed,
        OperationState::Cancelled => QueueItemState::Cancelled,
        OperationState::Interrupted => QueueItemState::Interrupted,
    }
}

fn clear_pending_app_update(
    state: &mut StateData,
    operation_id: &str,
    operation_state: OperationState,
) {
    if operation_state.is_terminal()
        && state
            .pending_app_update
            .as_ref()
            .is_some_and(|pending| pending.operation_id == operation_id)
    {
        state.pending_app_update = None;
    }
}

fn canonical_app_version(raw: &str) -> Result<String, AppError> {
    if raw.trim() != raw {
        return Err(AppError::invalid("Invalid application version."));
    }
    let normalized = raw.strip_prefix('v').unwrap_or(raw);
    let version = semver::Version::parse(normalized)
        .map_err(|_| AppError::invalid("Invalid application version."))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(AppError::invalid(
            "Application update versions must be stable semantic versions.",
        ));
    }
    Ok(version.to_string())
}

fn normalize_inspection(mut inspection: UrlInspection) -> Result<UrlInspection, AppError> {
    match &mut inspection {
        UrlInspection::Video { video } => {
            validate_actionable_field("video ID", &video.id)?;
            validate_actionable_field("video URL", &video.url)?;
            shrink_string(&mut video.id);
            shrink_string(&mut video.url);
            if let Some(thumbnail) = &mut video.thumbnail {
                validate_actionable_field("thumbnail URL", thumbnail)?;
                shrink_string(thumbnail);
            }
            for quality in &mut video.available_qualities {
                validate_actionable_field("quality label", quality)?;
                shrink_string(quality);
            }
            compact_vec(&mut video.available_qualities);
            truncate_display_field(&mut video.title);
            if let Some(channel) = &mut video.channel {
                truncate_display_field(channel);
            }
        }
        UrlInspection::Playlist { playlist } => {
            truncate_display_field(&mut playlist.title);
            if let Some(channel) = &mut playlist.channel {
                truncate_display_field(channel);
            }
            for entry in &mut playlist.entries {
                validate_actionable_field("playlist entry ID", &entry.id)?;
                validate_actionable_field("playlist entry URL", &entry.url)?;
                shrink_string(&mut entry.id);
                shrink_string(&mut entry.url);
                if let Some(thumbnail) = &mut entry.thumbnail {
                    validate_actionable_field("thumbnail URL", thumbnail)?;
                    shrink_string(thumbnail);
                }
                if let Some(title) = &mut entry.title {
                    truncate_display_field(title);
                }
            }
            compact_vec(&mut playlist.entries);
        }
    }
    Ok(inspection)
}

fn validate_actionable_field(name: &str, value: &str) -> Result<(), AppError> {
    if value.len() > MAX_UI_FIELD_BYTES {
        Err(AppError::new(
            "field_too_large",
            format!("The {name} exceeds the 4 KiB input limit."),
        ))
    } else {
        Ok(())
    }
}

fn validate_optional_actionable_field(name: &str, value: Option<&str>) -> Result<(), AppError> {
    value.map_or(Ok(()), |value| validate_actionable_field(name, value))
}

fn validate_cookie_config(config: Option<&crate::models::CookieConfig>) -> Result<(), AppError> {
    let Some(config) = config else {
        return Ok(());
    };
    validate_actionable_field("cookie mode", &config.mode)?;
    validate_actionable_field("cookie browser", &config.browser)?;
    validate_optional_actionable_field("cookie file path", config.cookie_file.as_deref())
}

fn truncate_display_field(value: &mut String) {
    if value.len() > MAX_UI_FIELD_BYTES {
        let mut end = MAX_UI_FIELD_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    shrink_string(value);
}

fn shrink_string(value: &mut String) {
    *value = std::mem::take(value).into_boxed_str().into_string();
}

fn compact_vec<T>(value: &mut Vec<T>) {
    *value = std::mem::take(value).into_boxed_slice().into_vec();
}

pub(crate) fn estimate_inspection_allocation(inspection: &UrlInspection) -> usize {
    let fixed =
        std::mem::size_of::<UrlInspection>().saturating_add(2 * std::mem::size_of::<usize>());
    match inspection {
        UrlInspection::Video { video } => [
            video.id.capacity(),
            video.title.capacity(),
            video.channel.as_ref().map_or(0, String::capacity),
            video.thumbnail.as_ref().map_or(0, String::capacity),
            video.url.capacity(),
            video
                .available_qualities
                .capacity()
                .saturating_mul(std::mem::size_of::<String>()),
            video
                .available_qualities
                .iter()
                .map(String::capacity)
                .fold(0usize, usize::saturating_add),
        ]
        .into_iter()
        .fold(fixed, usize::saturating_add),
        UrlInspection::Playlist { playlist } => {
            let entry_heap = playlist.entries.iter().fold(0usize, |total, entry| {
                [
                    entry.id.capacity(),
                    entry.title.as_ref().map_or(0, String::capacity),
                    entry.url.capacity(),
                    entry.thumbnail.as_ref().map_or(0, String::capacity),
                ]
                .into_iter()
                .fold(total, usize::saturating_add)
            });
            [
                playlist.title.capacity(),
                playlist.channel.as_ref().map_or(0, String::capacity),
                playlist
                    .entries
                    .capacity()
                    .saturating_mul(std::mem::size_of::<crate::models::PlaylistEntry>()),
                entry_heap,
            ]
            .into_iter()
            .fold(fixed, usize::saturating_add)
        }
    }
}

fn pending_download_available(state: &StateData) -> bool {
    state.pending_downloads.iter().any(|operation_id| {
        state.operations.get(operation_id).is_some_and(|operation| {
            operation.state == OperationState::Queued
                && operation
                    .queue_item_id
                    .as_ref()
                    .is_some_and(|queue_item_id| state.queue.contains_key(queue_item_id))
        })
    })
}

fn terminal_progress_from_finalization(
    operation_id: &str,
    applied: &AppliedFinalization,
) -> DownloadProgress {
    let (status, progress) = match applied.state {
        OperationState::Completed => ("completed", 100.0),
        OperationState::Cancelled => ("cancelled", 0.0),
        _ => ("error", 0.0),
    };
    DownloadProgress {
        download_id: operation_id.to_string(),
        status: status.to_string(),
        progress,
        phase: None,
        download_progress: (applied.state == OperationState::Completed).then_some(100.0),
        conversion_progress: None,
        speed: None,
        eta: None,
        error: applied.error.as_ref().map(|error| error.summary.clone()),
        error_code: applied.error.as_ref().map(|error| error.code.clone()),
        error_detail: applied
            .error
            .as_ref()
            .and_then(|error| error.detail.clone()),
        filename: applied
            .published_output
            .as_ref()
            .map(|output| output.path.clone()),
    }
}

fn authoritative_inspection_video(
    state: &StateData,
    operation_id: &str,
) -> Result<VideoInfo, AppError> {
    let operation = state
        .operations
        .get(operation_id)
        .ok_or_else(|| AppError::not_found("inspection operation"))?;
    if operation.kind != OperationKind::Inspection {
        return Err(AppError::new(
            "invalid_inspection_operation",
            "The selected operation is not an inspection.",
        ));
    }
    if operation.state != OperationState::Completed {
        return Err(AppError::new(
            "inspection_not_completed",
            "The inspection must complete before adding its result to the queue.",
        )
        .retryable(true));
    }
    match operation.inspection_result.as_deref() {
        Some(UrlInspection::Video { video }) => Ok(video.clone()),
        Some(UrlInspection::Playlist { .. }) => Err(AppError::new(
            "inspection_result_kind",
            "Playlist entries must be inspected individually before queueing.",
        )),
        None => Err(AppError::new(
            "inspection_result_unavailable",
            "The inspection result is no longer available; inspect the item again.",
        )
        .retryable(true)),
    }
}

fn ensure_operation_capacity(
    state: &StateData,
    additional: usize,
    limit: usize,
) -> Result<(), AppError> {
    let active = state
        .operations
        .values()
        .filter(|operation| !operation.state.is_terminal())
        .count();
    if active.saturating_add(additional) > limit {
        Err(AppError::new(
            "operation_limit",
            format!("At most {limit} operations may be queued or active at once."),
        )
        .retryable(true))
    } else {
        Ok(())
    }
}

fn failure_diagnostic(operation: &OperationSnapshot) -> Option<(String, String)> {
    if operation.state != OperationState::Failed {
        return None;
    }
    let error = operation.error.as_ref()?;
    let mut message = format!("{}: {}", error.code, error.summary);
    if let Some(detail) = error.detail.as_deref() {
        message.push_str("; ");
        message.push_str(detail);
    }
    Some((operation.correlation_id.clone(), message))
}

fn prune_live_operations(state: &mut StateData, now: u64) -> Vec<StateDelta> {
    let pending_app_update_id = state
        .pending_app_update
        .as_ref()
        .map(|pending| pending.operation_id.as_str());
    let retained = retained_operation_ids(
        state
            .operations
            .values()
            .map(|operation| OperationRetentionMetadata {
                id: &operation.id,
                state: operation.state,
                finished_at_ms: operation.finished_at_ms,
                updated_at_ms: operation.updated_at_ms,
            }),
        pending_app_update_id,
        now,
    );
    let removed = state
        .operations
        .keys()
        .filter(|id| !retained.contains(*id))
        .cloned()
        .collect::<HashSet<_>>();
    if removed.is_empty() {
        return Vec::new();
    }

    state
        .operation_order
        .retain(|operation_id| !removed.contains(operation_id));
    for operation_id in &removed {
        state.operations.remove(operation_id);
    }

    let mut deltas = removed
        .iter()
        .cloned()
        .map(|operation_id| next_delta(state, StateDeltaValue::OperationRemoved(operation_id)))
        .collect::<Vec<_>>();
    let queue_items_to_clear = state
        .queue
        .iter()
        .filter_map(|(id, item)| {
            item.latest_operation_id
                .as_ref()
                .is_some_and(|operation_id| removed.contains(operation_id))
                .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    for item_id in queue_items_to_clear {
        if let Some(item) = state.queue.get_mut(&item_id) {
            item.latest_operation_id = None;
            let item = item.snapshot();
            deltas.push(next_delta(state, StateDeltaValue::QueueItemUpserted(item)));
        }
    }
    deltas
}

fn next_delta(state: &mut StateData, delta: StateDeltaValue) -> StateDelta {
    state.sequence = state.sequence.saturating_add(1);
    StateDelta {
        schema_version: APP_SCHEMA_VERSION,
        sequence: state.sequence,
        emitted_at_ms: now_ms(),
        delta,
    }
}

fn next_operation_delta(state: &mut StateData, mut operation: OperationSnapshot) -> StateDelta {
    state.sequence = state.sequence.saturating_add(1);
    operation.sequence = state.sequence;
    state
        .operations
        .insert(operation.id.clone(), operation.clone().into());
    StateDelta {
        schema_version: APP_SCHEMA_VERSION,
        sequence: state.sequence,
        emitted_at_ms: now_ms(),
        delta: StateDeltaValue::OperationUpserted(operation),
    }
}

#[cfg(test)]
mod tests {
    use super::StateStore;
    use crate::app_error::AppError;
    use crate::journal::MAX_TERMINAL_ATTEMPTS;
    use crate::models::{
        AddQueueItemInput, OperationState, PlaylistEntry, PlaylistInfo, QueueItemState,
        QueuePriority, RuntimeReadiness, UpdateQueueItemInput, UrlInspection, VideoInfo,
    };

    fn test_store() -> StateStore {
        let root = std::env::temp_dir().join(format!("nuclear-state-{}", uuid::Uuid::new_v4()));
        StateStore::open_at(root.join("journal.dpapi"), root.join("diagnostics")).unwrap()
    }

    fn video(index: usize) -> VideoInfo {
        VideoInfo {
            id: format!("video-{index}"),
            title: format!("Video {index}"),
            duration: None,
            channel: None,
            thumbnail: None,
            url: format!("https://example.com/{index}"),
            available_qualities: vec!["720p".to_string()],
            has_audio: true,
        }
    }

    fn large_inspection() -> UrlInspection {
        let large_id = "i".repeat(4_000);
        let large_url = "u".repeat(4_000);
        let title = "t".repeat(1_000);
        UrlInspection::Playlist {
            playlist: PlaylistInfo {
                title: "Large retained inspection".to_string(),
                channel: None,
                entry_count: 1_000,
                truncated: false,
                entries: (0..1_000)
                    .map(|_| PlaylistEntry {
                        id: large_id.clone(),
                        title: Some(title.clone()),
                        duration: None,
                        url: large_url.clone(),
                        thumbnail: None,
                    })
                    .collect(),
            },
        }
    }

    fn input(inspection_operation_id: String) -> AddQueueItemInput {
        AddQueueItemInput {
            inspection_operation_id,
            format: "mp4".to_string(),
            quality: "720p".to_string(),
            output_dir: "C:\\Downloads".to_string(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        }
    }

    async fn add_item(
        store: &StateStore,
        index: usize,
    ) -> (
        crate::models::QueueItemRecord,
        Vec<crate::models::StateDelta>,
    ) {
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store
            .complete_inspection(
                &operation.id,
                crate::models::UrlInspection::Video {
                    video: video(index),
                },
            )
            .await
            .unwrap();
        store.add_queue_item(input(operation.id)).await.unwrap()
    }

    #[tokio::test]
    async fn sequence_is_monotonic_and_snapshot_reports_latest() {
        let store = test_store();
        let (_, first) = add_item(&store, 1).await;
        let (_, second) = add_item(&store, 2).await;
        let snapshot = store.snapshot().unwrap();

        assert!(second.last().unwrap().sequence > first.last().unwrap().sequence);
        assert_eq!(snapshot.latest_sequence, second.last().unwrap().sequence);
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.runtime_readiness, RuntimeReadiness::RepairRequired);
    }

    #[tokio::test]
    async fn queue_add_consumes_only_authoritative_completed_video_inspections() {
        let store = test_store();
        assert_eq!(
            store
                .add_queue_item(input(uuid::Uuid::new_v4().to_string()))
                .await
                .unwrap_err()
                .code,
            "not_found"
        );

        let (pending, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        assert_eq!(
            store
                .add_queue_item(input(pending.id.clone()))
                .await
                .unwrap_err()
                .code,
            "inspection_not_completed"
        );

        let (wrong_kind, _) = store
            .begin_operation(crate::models::OperationKind::Download, None)
            .await
            .unwrap();
        store
            .set_operation_state(&wrong_kind.id, OperationState::Completed, None)
            .await
            .unwrap();
        assert_eq!(
            store
                .add_queue_item(input(wrong_kind.id))
                .await
                .unwrap_err()
                .code,
            "invalid_inspection_operation"
        );

        store
            .set_operation_state(&pending.id, OperationState::Cancelled, None)
            .await
            .unwrap();
        let (playlist, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store
            .complete_inspection(
                &playlist.id,
                crate::models::UrlInspection::Playlist {
                    playlist: crate::models::PlaylistInfo {
                        title: "Playlist".to_string(),
                        channel: None,
                        entry_count: 0,
                        truncated: false,
                        entries: Vec::new(),
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .add_queue_item(input(playlist.id))
                .await
                .unwrap_err()
                .code,
            "inspection_result_kind"
        );

        let (video_operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store
            .complete_inspection(
                &video_operation.id,
                crate::models::UrlInspection::Video { video: video(42) },
            )
            .await
            .unwrap();
        let (item, deltas) = store
            .add_queue_item(input(video_operation.id.clone()))
            .await
            .unwrap();

        assert_eq!(item.source_url, "https://example.com/42");
        assert!(store.operation_state(&video_operation.id).is_none());
        assert!(deltas.iter().any(|delta| {
            matches!(
                &delta.delta,
                crate::models::StateDeltaValue::OperationRemoved(id) if id == &video_operation.id
            )
        }));
    }

    #[tokio::test]
    async fn retry_preserves_item_identity_and_creates_a_new_attempt() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (first, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        store
            .set_operation_state(&first[0].operation_id, OperationState::Failed, None)
            .await
            .unwrap();
        let (second, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();

        assert_eq!(second[0].queue_item.id, item.id);
        assert_ne!(second[0].operation_id, first[0].operation_id);
    }

    #[tokio::test]
    async fn active_items_cannot_be_edited_or_removed() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();

        assert!(store
            .update_queue_item(&item.id, UpdateQueueItemInput::default())
            .await
            .is_err());
        assert!(store
            .remove_queue_items(std::slice::from_ref(&item.id))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn failed_durable_mutations_roll_back_memory_and_pending_work() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let before = store.snapshot().unwrap();
        store.fail_next_persistence_for_test();

        let error = store
            .update_queue_item(
                &item.id,
                UpdateQueueItemInput {
                    quality: Some("1080p".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.summary.contains("Injected"));
        let after_update = store.snapshot().unwrap();
        assert_eq!(after_update.latest_sequence, before.latest_sequence);
        assert_eq!(after_update.queue[0].quality, before.queue[0].quality);

        store.fail_next_persistence_for_test();
        assert!(store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .is_err());
        let after_enqueue = store.snapshot().unwrap();
        assert_eq!(after_enqueue.latest_sequence, before.latest_sequence);
        assert_eq!(after_enqueue.operations.len(), before.operations.len());
        assert!(store.take_next_pending().await.is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn removal_detaches_terminal_history_and_reopens_without_quarantine() {
        let root =
            std::env::temp_dir().join(format!("nuclear-remove-reopen-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (removed_item, _) = add_item(&store, 1).await;
        let (retained_item, _) = add_item(&store, 2).await;
        let (work, _) = store
            .enqueue(
                std::slice::from_ref(&removed_item.id),
                QueuePriority::Normal,
            )
            .await
            .unwrap();
        store
            .set_operation_state(&work[0].operation_id, OperationState::Completed, None)
            .await
            .unwrap();

        let deltas = store
            .remove_queue_items(std::slice::from_ref(&removed_item.id))
            .await
            .unwrap();
        assert!(deltas.iter().any(|delta| matches!(
            &delta.delta,
            crate::models::StateDeltaValue::OperationUpserted(operation)
                if operation.id == work[0].operation_id && operation.queue_item_id.is_none()
        )));
        drop(store);

        let reopened = StateStore::open_at(journal_path, diagnostics_path).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].id, retained_item.id);
        assert!(snapshot.operations.iter().any(|operation| {
            operation.id == work[0].operation_id && operation.queue_item_id.is_none()
        }));
        drop(reopened);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn first_mutation_after_high_revision_restart_is_persisted() {
        let root =
            std::env::temp_dir().join(format!("nuclear-revision-reopen-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        add_item(&store, 1).await;
        let revision_before_restart = store.snapshot().unwrap().latest_sequence;
        assert!(revision_before_restart > 1);
        drop(store);

        let reopened = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        assert_eq!(
            reopened.snapshot().unwrap().latest_sequence,
            revision_before_restart
        );
        add_item(&reopened, 2).await;
        let expected_revision = reopened.snapshot().unwrap().latest_sequence;
        drop(reopened);

        let verified = StateStore::open_at(journal_path, diagnostics_path).unwrap();
        let snapshot = verified.snapshot().unwrap();
        assert_eq!(snapshot.latest_sequence, expected_revision);
        assert_eq!(snapshot.queue.len(), 2);
        drop(verified);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn front_priority_is_single_row_only() {
        let store = test_store();
        let (first, _) = add_item(&store, 1).await;
        let (second, _) = add_item(&store, 2).await;
        assert!(store
            .enqueue(&[first.id, second.id], QueuePriority::Front)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn front_priority_is_next_after_the_five_admitted_rows() {
        let store = test_store();
        let mut items = Vec::new();
        for index in 0..7 {
            items.push(add_item(&store, index).await.0);
        }
        let normal_ids = items[..6]
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        store
            .enqueue(&normal_ids, QueuePriority::Normal)
            .await
            .unwrap();
        for _ in 0..5 {
            store.take_next_pending().await.unwrap();
        }
        let (front, _) = store
            .enqueue(std::slice::from_ref(&items[6].id), QueuePriority::Front)
            .await
            .unwrap();

        assert_eq!(
            store.take_next_pending().await.unwrap().operation_id,
            front[0].operation_id
        );
    }

    #[tokio::test]
    async fn pending_wait_does_not_dequeue_and_ignores_cancelled_pending_work() {
        let store = test_store();
        let (first, _) = add_item(&store, 1).await;
        let (second, _) = add_item(&store, 2).await;
        let (first_work, _) = store
            .enqueue(std::slice::from_ref(&first.id), QueuePriority::Normal)
            .await
            .unwrap();

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            store.wait_pending_available(),
        )
        .await
        .expect("an already-pending job is visible without another notification");
        assert_eq!(
            store.pending_operation_ids(),
            vec![first_work[0].operation_id.clone()]
        );

        store
            .request_cancellation(&first_work[0].operation_id)
            .await
            .unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(25),
            store.wait_pending_available(),
        )
        .await
        .is_err());
        assert!(store.cancel_pending(&first_work[0].operation_id).await);

        let waiter_store = store.clone();
        let waiter = tokio::spawn(async move {
            waiter_store.wait_pending_available().await;
        });
        tokio::task::yield_now().await;
        let (second_work, _) = store
            .enqueue(std::slice::from_ref(&second.id), QueuePriority::Normal)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(100), waiter)
            .await
            .expect("enqueue notification must wake a registered pending waiter")
            .unwrap();
        assert_eq!(
            store.pending_operation_ids(),
            vec![second_work[0].operation_id.clone()]
        );
    }

    #[tokio::test]
    async fn maintenance_operation_is_registered_in_the_same_state_transition() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (operation, deltas) = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
            .await
            .unwrap();
        let snapshot = store.snapshot().unwrap();

        assert_eq!(deltas.len(), 2);
        assert!(snapshot.maintenance_active);
        assert!(snapshot
            .operations
            .iter()
            .any(|value| value.id == operation.id));
        assert!(store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .is_err());
        assert!(store.set_maintenance(false, false).await.is_err());
        let ended = store
            .end_maintenance_operation(&operation.id)
            .await
            .unwrap()
            .expect("owner releases maintenance");
        assert!(matches!(
            ended.delta,
            crate::models::StateDeltaValue::MaintenanceChanged {
                active: false,
                draining: false
            }
        ));
        assert!(!store.snapshot().unwrap().maintenance_active);
    }

    #[tokio::test]
    async fn maintenance_operation_accepts_a_preregistered_unique_uuid() {
        let store = test_store();
        let invalid = store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::RuntimeUpdate,
                "not-a-uuid".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid.code, "invalid_request");
        assert!(!store.snapshot().unwrap().maintenance_active);

        let operation_id = uuid::Uuid::new_v4().to_string();
        let (operation, _) = store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::RuntimeUpdate,
                operation_id.clone(),
            )
            .await
            .unwrap();
        assert_eq!(operation.id, operation_id);
        store
            .finalize_operation(&operation_id, OperationState::Completed, None)
            .await
            .unwrap();
        store
            .end_maintenance_operation(&operation_id)
            .await
            .unwrap();

        let duplicate = store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::RuntimeUpdate,
                operation_id,
            )
            .await
            .unwrap_err();
        assert_eq!(duplicate.code, "duplicate_operation");
        assert!(!store.snapshot().unwrap().maintenance_active);
    }

    #[tokio::test]
    async fn app_update_handoff_does_not_launch_from_an_unpersisted_marker() {
        let store = test_store();
        let operation_id = uuid::Uuid::new_v4().to_string();
        store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::AppUpdate,
                operation_id.clone(),
            )
            .await
            .unwrap();
        store.fail_next_persistence_for_test();

        let error = store
            .prepare_app_update_handoff(&operation_id, "v0.6.0")
            .await
            .unwrap_err();

        assert!(error.summary.contains("Injected"));
        let snapshot = store.snapshot().unwrap();
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Queued);
        assert!(operation.phase.is_none());
        assert!(store.lock().unwrap().pending_app_update.is_none());
    }

    #[tokio::test]
    async fn degraded_app_update_finalization_clears_and_flushes_the_pending_handoff() {
        let store = test_store();
        let operation_id = uuid::Uuid::new_v4().to_string();
        store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::AppUpdate,
                operation_id.clone(),
            )
            .await
            .unwrap();
        store
            .prepare_app_update_handoff(&operation_id, "0.6.0")
            .await
            .unwrap();
        let premature_completion = store
            .finalize_operation(&operation_id, OperationState::Completed, None)
            .await
            .unwrap_err();
        assert_eq!(
            premature_completion.code,
            "app_update_reconciliation_required"
        );
        assert!(store.lock().unwrap().pending_app_update.is_some());
        store.fail_persistence_for_test(3);

        store
            .finalize_operation(
                &operation_id,
                OperationState::Failed,
                Some(AppError::new(
                    "installer_launch_failed",
                    "The installer could not be launched.",
                )),
            )
            .await
            .unwrap();

        assert!(store.lock().unwrap().pending_app_update.is_none());
        assert_eq!(
            store.operation_state(&operation_id),
            Some(OperationState::Failed)
        );
        assert!(store.snapshot().unwrap().persistence_health.degraded);

        store
            .end_maintenance_operation(&operation_id)
            .await
            .unwrap();
        store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        assert!(!store.snapshot().unwrap().persistence_health.degraded);
        assert!(store.lock().unwrap().pending_app_update.is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn app_update_handoff_completes_only_on_target_version_and_is_restart_idempotent() {
        let root =
            std::env::temp_dir().join(format!("nuclear-app-handoff-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (queue_item, _) = add_item(&store, 1).await;
        let operation_id = uuid::Uuid::new_v4().to_string();
        store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::AppUpdate,
                operation_id.clone(),
            )
            .await
            .unwrap();
        let prepared = store
            .prepare_app_update_handoff(&operation_id, "v0.6.0")
            .await
            .unwrap();
        assert!(prepared.iter().any(|delta| matches!(
            &delta.delta,
            crate::models::StateDeltaValue::OperationUpserted(operation)
                if operation.id == operation_id
                    && operation.state == OperationState::Running
                    && operation.phase.as_deref() == Some("installing")
        )));
        drop(store);

        let restarted =
            StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let before_reconcile = restarted.snapshot().unwrap();
        assert_eq!(before_reconcile.queue[0].id, queue_item.id);
        assert_eq!(
            before_reconcile
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)
                .unwrap()
                .state,
            OperationState::Running
        );
        let deltas = restarted
            .reconcile_pending_app_update("0.6.0")
            .await
            .unwrap();
        assert!(deltas.iter().any(|delta| matches!(
            &delta.delta,
            crate::models::StateDeltaValue::OperationUpserted(operation)
                if operation.id == operation_id && operation.state == OperationState::Completed
        )));
        assert!(restarted.lock().unwrap().pending_app_update.is_none());
        drop(restarted);

        let second_restart =
            StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        assert!(second_restart
            .reconcile_pending_app_update("0.6.0")
            .await
            .unwrap()
            .is_empty());
        let snapshot = second_restart.snapshot().unwrap();
        assert_eq!(snapshot.queue[0].id, queue_item.id);
        assert_eq!(
            snapshot
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)
                .unwrap()
                .state,
            OperationState::Completed
        );
        drop(second_restart);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn app_update_handoff_mismatch_becomes_retryable_interruption() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-app-handoff-mismatch-{}",
            uuid::Uuid::new_v4()
        ));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (queue_item, _) = add_item(&store, 1).await;
        let operation_id = uuid::Uuid::new_v4().to_string();
        store
            .begin_maintenance_operation_with_id(
                crate::models::OperationKind::AppUpdate,
                operation_id.clone(),
            )
            .await
            .unwrap();
        store
            .prepare_app_update_handoff(&operation_id, "0.6.0")
            .await
            .unwrap();
        drop(store);

        let restarted = StateStore::open_at(journal_path, diagnostics_path).unwrap();
        restarted
            .reconcile_pending_app_update("0.5.0")
            .await
            .unwrap();
        let snapshot = restarted.snapshot().unwrap();
        assert_eq!(snapshot.queue[0].id, queue_item.id);
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(
            operation.error.as_ref().map(|error| error.code.as_str()),
            Some("app_update_handoff_interrupted")
        );
        assert!(operation.error.as_ref().unwrap().retryable);
        assert!(restarted.lock().unwrap().pending_app_update.is_none());
        drop(restarted);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn queued_operation_blocks_maintenance_before_worker_registration() {
        let store = test_store();
        store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();

        let error = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
            .await
            .unwrap_err();

        assert_eq!(error.code, "busy");
        assert!(!store.snapshot().unwrap().maintenance_active);
    }

    #[tokio::test]
    async fn cancelled_maintenance_operation_cannot_start_after_cancellation() {
        let store = test_store();
        let (operation, _) = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
            .await
            .unwrap();
        store
            .set_operation_state(&operation.id, OperationState::Cancelled, None)
            .await
            .unwrap();

        let error = store
            .set_operation_state(&operation.id, OperationState::Running, None)
            .await
            .unwrap_err();

        assert_eq!(error.code, "invalid_transition");
        assert_eq!(
            store.operation_state(&operation.id),
            Some(OperationState::Cancelled)
        );
    }

    #[tokio::test]
    async fn cancellation_cannot_regress_or_complete_from_late_progress() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        store.request_cancellation(&operation_id).await.unwrap();

        for status in ["queued", "downloading"] {
            let progress = crate::models::DownloadProgress {
                download_id: operation_id.clone(),
                status: status.to_string(),
                progress: 100.0,
                phase: Some("download".to_string()),
                download_progress: Some(100.0),
                conversion_progress: None,
                speed: None,
                eta: None,
                error: None,
                error_code: None,
                error_detail: None,
                filename: None,
            };
            assert!(store
                .apply_download_progress(&operation_id, &progress)
                .await
                .unwrap()
                .is_none());
            assert_eq!(
                store.operation_state(&operation_id),
                Some(OperationState::Cancelling)
            );
        }

        let receipt = store
            .finalize_download(&operation_id, super::DownloadTerminalOutcome::Cancelled)
            .await
            .unwrap();
        assert_eq!(receipt.progress.status, "cancelled");
        assert_eq!(
            store.operation_state(&operation_id),
            Some(OperationState::Cancelled)
        );
    }

    #[tokio::test]
    async fn cancellation_waiter_resolves_only_after_terminal_transition() {
        let store = test_store();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store.request_cancellation(&operation.id).await.unwrap();
        let transition_store = store.clone();
        let operation_id = operation.id.clone();
        let transition_id = operation_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            transition_store
                .set_operation_state(&transition_id, OperationState::Cancelled, None)
                .await
                .unwrap();
        });

        let terminal = store
            .wait_for_terminal(&operation_id, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(terminal.state, OperationState::Cancelled);
    }

    #[tokio::test]
    async fn cancelling_inspection_cannot_be_completed() {
        let store = test_store();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store.request_cancellation(&operation.id).await.unwrap();

        let error = store
            .complete_inspection(
                &operation.id,
                crate::models::UrlInspection::Video { video: video(1) },
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "invalid_transition");
        assert_eq!(
            store.operation_state(&operation.id),
            Some(OperationState::Cancelling)
        );
    }

    #[tokio::test]
    async fn terminal_operations_are_pruned_from_live_snapshots_with_removal_deltas() {
        let store = test_store();
        let mut removed = 0usize;
        for _ in 0..(MAX_TERMINAL_ATTEMPTS + 7) {
            let (operation, _) = store
                .begin_operation(crate::models::OperationKind::Inspection, None)
                .await
                .unwrap();
            let deltas = store
                .set_operation_state(&operation.id, OperationState::Completed, None)
                .await
                .unwrap();
            removed += deltas
                .iter()
                .filter(|delta| {
                    matches!(
                        delta.delta,
                        crate::models::StateDeltaValue::OperationRemoved(_)
                    )
                })
                .count();
        }

        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.operations.len(), MAX_TERMINAL_ATTEMPTS);
        assert_eq!(removed, 7);
        assert!(snapshot
            .operations
            .iter()
            .all(|operation| operation.state.is_terminal()));
    }

    #[tokio::test]
    async fn concurrent_operation_admission_never_exceeds_the_limit() {
        let store = test_store();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(33));
        let handles = (0..32)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    store
                        .begin_operation_with_limit(
                            crate::models::OperationKind::Inspection,
                            None,
                            16,
                        )
                        .await
                })
            })
            .collect::<Vec<_>>();
        barrier.wait().await;
        let mut admitted = 0usize;
        for handle in handles {
            admitted += usize::from(handle.await.unwrap().is_ok());
        }

        assert_eq!(admitted, 16);
        assert_eq!(store.snapshot().unwrap().operations.len(), 16);
    }

    #[tokio::test]
    async fn published_download_completion_wins_a_late_cancellation() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        store.request_cancellation(&operation_id).await.unwrap();

        let receipt = store
            .finalize_download(
                &operation_id,
                super::DownloadTerminalOutcome::Completed {
                    filename: Some("C:\\Downloads\\published.mp4".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(receipt.durability, super::FinalizationDurability::Persisted);
        assert_eq!(receipt.progress.status, "completed");
        assert_eq!(
            receipt.progress.filename.as_deref(),
            Some("C:\\Downloads\\published.mp4")
        );
        let snapshot = store.snapshot().unwrap();
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Completed);
        assert_eq!(
            operation
                .published_output
                .as_ref()
                .map(|output| output.path.as_str()),
            Some("C:\\Downloads\\published.mp4")
        );
        assert_eq!(
            snapshot.queue[0].state,
            crate::models::QueueItemState::Completed
        );
    }

    #[tokio::test]
    async fn transient_terminal_save_failure_is_retried_before_completion_is_acknowledged() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        store.fail_next_persistence_for_test();

        let receipt = store
            .finalize_download(
                &work[0].operation_id,
                super::DownloadTerminalOutcome::Completed { filename: None },
            )
            .await
            .unwrap();

        assert_eq!(receipt.durability, super::FinalizationDurability::Persisted);
        assert_eq!(receipt.progress.status, "completed");
        assert!(!store.snapshot().unwrap().persistence_health.degraded);
    }

    #[tokio::test]
    async fn exhausted_terminal_save_retries_retain_recovery_intent_until_the_next_commit() {
        let store = test_store();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        store.fail_persistence_for_test(3);

        let receipt = store
            .finalize_download(
                &operation_id,
                super::DownloadTerminalOutcome::Completed {
                    filename: Some("C:\\Downloads\\published.mp4".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
        assert_eq!(receipt.progress.status, "error");
        assert_eq!(
            receipt.progress.error_code.as_deref(),
            Some("state_persistence_failed")
        );
        assert_eq!(
            receipt.progress.filename.as_deref(),
            Some("C:\\Downloads\\published.mp4")
        );
        let degraded = store.snapshot().unwrap();
        assert!(degraded.persistence_health.degraded);
        assert_eq!(
            degraded.queue[0].state,
            crate::models::QueueItemState::Failed
        );
        let operation = degraded
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Failed);
        assert_eq!(
            operation
                .intended_terminal_outcome
                .as_ref()
                .map(|outcome| outcome.state),
            Some(OperationState::Completed)
        );
        assert_eq!(
            operation
                .published_output
                .as_ref()
                .map(|output| output.path.as_str()),
            Some("C:\\Downloads\\published.mp4")
        );

        let deltas = store
            .update_queue_item(
                &item.id,
                UpdateQueueItemInput {
                    quality: Some("1080p".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(deltas.iter().any(|delta| matches!(
            &delta.delta,
            crate::models::StateDeltaValue::PersistenceHealthChanged(health)
                if !health.degraded && health.error.is_none()
        )));
        let recovered = store.snapshot().unwrap();
        assert!(!recovered.persistence_health.degraded);
        assert_eq!(recovered.queue[0].quality, "1080p");
        let operation = recovered
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Failed);
        assert_eq!(
            operation
                .intended_terminal_outcome
                .as_ref()
                .map(|outcome| outcome.state),
            Some(OperationState::Completed)
        );
    }

    #[tokio::test]
    async fn failed_finalizer_task_installs_a_degraded_terminal_compensation() {
        let store = test_store();
        let reader = store.take_outbox_reader().unwrap();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        while reader.try_recv().is_some() {}
        store.fail_next_finalizer_task_for_test();

        let receipt = store
            .finalize_download(
                &operation_id,
                super::DownloadTerminalOutcome::Completed {
                    filename: Some("C:\\Downloads\\published.mp4".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
        assert_eq!(receipt.progress.status, "error");
        assert_eq!(
            receipt.progress.error_code.as_deref(),
            Some("state_finalizer_failed")
        );
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.persistence_health.degraded);
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Failed);
        assert_eq!(
            operation
                .intended_terminal_outcome
                .as_ref()
                .map(|outcome| outcome.state),
            Some(OperationState::Completed)
        );
        assert_eq!(
            operation
                .published_output
                .as_ref()
                .map(|output| output.path.as_str()),
            Some("C:\\Downloads\\published.mp4")
        );
        assert_eq!(snapshot.queue[0].state, QueueItemState::Failed);
        let publications = std::iter::from_fn(|| reader.try_recv()).collect::<Vec<_>>();
        assert!(publications.iter().any(|publication| matches!(
            publication,
            crate::outbox::StatePublication::Deltas(deltas)
                if deltas.iter().any(|delta| matches!(
                    &delta.delta,
                    crate::models::StateDeltaValue::OperationUpserted(operation)
                        if operation.id == operation_id && operation.state == OperationState::Failed
                ))
        )));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn failure_after_save_advances_compensation_past_the_persisted_candidate_revision() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-finalizer-after-save-{}",
            uuid::Uuid::new_v4()
        ));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (item, _) = add_item(&store, 1).await;
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .await
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        let terminal_count = MAX_TERMINAL_ATTEMPTS + 7;
        let sequence_before = store.snapshot().unwrap().latest_sequence;
        {
            let mut state = store.lock().unwrap();
            for _ in 0..terminal_count {
                let id = uuid::Uuid::new_v4().to_string();
                let operation = crate::models::OperationSnapshot {
                    schema_version: crate::models::APP_SCHEMA_VERSION,
                    id: id.clone(),
                    queue_item_id: None,
                    kind: crate::models::OperationKind::Inspection,
                    state: OperationState::Completed,
                    progress: 100.0,
                    phase: None,
                    sequence: 0,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    finished_at_ms: Some(1),
                    error: None,
                    inspection_result: None,
                    published_output: None,
                    intended_terminal_outcome: None,
                    correlation_id: uuid::Uuid::new_v4().to_string(),
                };
                state.operation_order.push(id.clone());
                state.operations.insert(id, operation.into());
            }
        }
        store.fail_next_finalizer_after_save_for_test();

        let receipt = store
            .finalize_download(
                &operation_id,
                super::DownloadTerminalOutcome::Completed { filename: None },
            )
            .await
            .unwrap();

        assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
        assert!(
            store.snapshot().unwrap().latest_sequence
                > sequence_before + u64::try_from(terminal_count).unwrap()
        );
        store
            .update_queue_item(
                &item.id,
                UpdateQueueItemInput {
                    quality: Some("1080p".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        drop(store);

        for _ in 0..2 {
            let reopened =
                StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
            let snapshot = reopened.snapshot().unwrap();
            assert_eq!(snapshot.queue[0].quality, "1080p");
            let operation = snapshot
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)
                .unwrap();
            assert_eq!(operation.state, OperationState::Failed);
            assert_eq!(
                operation
                    .intended_terminal_outcome
                    .as_ref()
                    .map(|outcome| outcome.state),
                Some(OperationState::Completed)
            );
            drop(reopened);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn detached_commit_keeps_the_gate_until_save_and_swap_finish() {
        let root =
            std::env::temp_dir().join(format!("nuclear-detached-commit-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (item, _) = add_item(&store, 1).await;
        let pause = store.pause_next_commit_for_test();
        let first_store = store.clone();
        let first_id = item.id.clone();
        let first = tokio::spawn(async move {
            first_store
                .update_queue_item(
                    &first_id,
                    UpdateQueueItemInput {
                        quality: Some("1080p".to_string()),
                        ..Default::default()
                    },
                )
                .await
        });
        pause.wait_entered().await;
        first.abort();

        let transient_store = store.clone();
        let transient = tokio::spawn(async move {
            transient_store
                .set_runtime_readiness(RuntimeReadiness::Ready)
                .await
        });
        let second_store = store.clone();
        let second_id = item.id.clone();
        let second = tokio::spawn(async move {
            second_store
                .update_queue_item(
                    &second_id,
                    UpdateQueueItemInput {
                        quality: Some("1440p".to_string()),
                        ..Default::default()
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(!transient.is_finished());
        assert!(!second.is_finished());

        pause.release();
        transient.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        drop(store);

        let reopened = StateStore::open_at(journal_path, diagnostics_path).unwrap();
        assert_eq!(reopened.snapshot().unwrap().queue[0].quality, "1440p");
        drop(reopened);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn inspection_budget_counts_payloads_retained_only_by_the_outbox() {
        let store = test_store();
        let reader = store.take_outbox_reader().unwrap();
        let (first, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store
            .complete_inspection(&first.id, large_inspection())
            .await
            .unwrap();
        store.dismiss_operation(&first.id).await.unwrap();

        let (second, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let error = store
            .complete_inspection(&second.id, large_inspection())
            .await
            .unwrap_err();
        assert_eq!(error.code, "inspection_retention_limit");

        while reader.try_recv().is_some() {}
        store
            .complete_inspection(&second.id, large_inspection())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inspection_budget_counts_payloads_retained_by_a_snapshot() {
        let store = test_store();
        let reader = store.take_outbox_reader().unwrap();
        let (first, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        store
            .complete_inspection(&first.id, large_inspection())
            .await
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        store.dismiss_operation(&first.id).await.unwrap();
        while reader.try_recv().is_some() {}

        let (second, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let error = store
            .complete_inspection(&second.id, large_inspection())
            .await
            .unwrap_err();
        assert_eq!(error.code, "inspection_retention_limit");

        drop(snapshot);
        store
            .complete_inspection(&second.id, large_inspection())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inspection_display_fields_are_utf8_safely_truncated_and_action_fields_rejected() {
        let store = test_store();
        let (display_operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let mut display_video = video(1);
        let mut oversized_capacity_title = String::with_capacity(10 * 1024 * 1024);
        oversized_capacity_title.push_str(&"💥".repeat(2_000));
        display_video.title = oversized_capacity_title;
        display_video.channel = Some("channel".repeat(1_000));
        store
            .complete_inspection(
                &display_operation.id,
                UrlInspection::Video {
                    video: display_video,
                },
            )
            .await
            .unwrap();
        let completed = store
            .snapshot()
            .unwrap()
            .operations
            .into_iter()
            .find(|operation| operation.id == display_operation.id)
            .unwrap();
        let inspection = completed.inspection_result.unwrap();
        let UrlInspection::Video {
            video: inspected_video,
        } = inspection.as_ref()
        else {
            panic!("expected video inspection");
        };
        assert!(inspected_video.title.len() <= super::MAX_UI_FIELD_BYTES);
        assert!(inspected_video.title.capacity() <= super::MAX_UI_FIELD_BYTES);
        assert!(inspected_video.channel.as_ref().unwrap().len() <= super::MAX_UI_FIELD_BYTES);

        let (action_operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let mut action_video = video(2);
        action_video.url = "u".repeat(super::MAX_UI_FIELD_BYTES + 1);
        let error = store
            .complete_inspection(
                &action_operation.id,
                UrlInspection::Video {
                    video: action_video,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "field_too_large");
    }

    #[tokio::test]
    async fn many_empty_quality_records_are_counted_by_the_inspection_budget() {
        let store = test_store();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let quality_count = super::MAX_RETAINED_INSPECTION_BYTES
            .checked_div(std::mem::size_of::<String>())
            .unwrap()
            + 1;
        let mut oversized = video(1);
        oversized.available_qualities = vec![String::new(); quality_count];

        let error = store
            .complete_inspection(&operation.id, UrlInspection::Video { video: oversized })
            .await
            .unwrap_err();

        assert_eq!(error.code, "inspection_retention_limit");
        assert_eq!(
            store.operation_state(&operation.id),
            Some(OperationState::Queued)
        );
    }

    #[tokio::test]
    async fn operation_failure_log_uses_snapshot_correlation_and_redacts_detail() {
        let root = std::env::temp_dir().join(format!("nuclear-state-log-{}", uuid::Uuid::new_v4()));
        let diagnostics_path = root.join("diagnostics");
        let store =
            StateStore::open_at(root.join("journal.dpapi"), diagnostics_path.clone()).unwrap();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let error = AppError::new("fixture_failed", "Inspection failed")
            .with_detail("Authorization: Bearer should-not-escape");

        store
            .set_operation_state(&operation.id, OperationState::Failed, Some(error))
            .await
            .unwrap();

        let log = std::fs::read_to_string(diagnostics_path.join("diagnostics.jsonl")).unwrap();
        assert!(log.contains(&operation.correlation_id));
        assert!(log.contains("operation_failed"));
        assert!(log.contains("fixture_failed"));
        assert!(!log.contains("should-not-escape"));
        let _ = std::fs::remove_dir_all(root);
    }
}
