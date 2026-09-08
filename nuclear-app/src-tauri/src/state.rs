#[cfg(test)]
use self::tests::TestCommitPause;
use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
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
use crate::outbox::{StateOutbox, StateOutboxReader};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::{Deref, DerefMut};
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
mod tests;
