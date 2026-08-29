use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
use crate::journal::{
    now_ms, JournalStore, PersistentJournal, MAX_TERMINAL_ATTEMPTS, TERMINAL_RETENTION_MS,
};
use crate::models::{
    AddQueueItemInput, AppSnapshot, DownloadProgress, OperationKind, OperationSnapshot,
    OperationState, QueueItemRecord, QueueItemState, QueuePriority, RuntimeReadiness, StateDelta,
    StateDeltaValue, UpdateQueueItemInput, UrlInspection, VideoInfo, APP_SCHEMA_VERSION,
};
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub const MAX_QUEUE_ITEMS: usize = 1_000;
pub const MAX_ACTIVE_OPERATIONS: usize = 1_000;

#[derive(Clone)]
pub struct StateStore {
    inner: Arc<StateStoreInner>,
}

struct StateStoreInner {
    state: Mutex<StateData>,
    journal: JournalStore,
    diagnostics: Diagnostics,
    pending_notify: Notify,
    operation_notify: Notify,
}

#[derive(Clone)]
struct StateData {
    sequence: u64,
    queue_order: Vec<String>,
    queue: HashMap<String, QueueItemRecord>,
    operation_order: Vec<String>,
    operations: HashMap<String, OperationSnapshot>,
    pending_downloads: VecDeque<String>,
    runtime_readiness: RuntimeReadiness,
    maintenance_active: bool,
    draining: bool,
    maintenance_owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueuedDownload {
    pub operation_id: String,
    pub queue_item: QueueItemRecord,
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
        let queue_order = loaded.queue.iter().map(|item| item.id.clone()).collect();
        let queue = loaded
            .queue
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect();
        let operation_order = loaded
            .operations
            .iter()
            .map(|operation| operation.id.clone())
            .collect();
        let operations = loaded
            .operations
            .into_iter()
            .map(|operation| (operation.id.clone(), operation))
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
                }),
                journal,
                diagnostics,
                pending_notify: Notify::new(),
                operation_notify: Notify::new(),
            }),
        }
    }

    pub fn snapshot(&self) -> Result<AppSnapshot, AppError> {
        let state = self.lock()?;
        Ok(AppSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            queue: state
                .queue_order
                .iter()
                .filter_map(|id| state.queue.get(id).cloned())
                .collect(),
            operations: state
                .operation_order
                .iter()
                .filter_map(|id| state.operations.get(id).cloned())
                .collect(),
            runtime_readiness: state.runtime_readiness,
            maintenance_active: state.maintenance_active,
            draining: state.draining,
            latest_sequence: state.sequence,
        })
    }

    pub fn add_queue_item(
        &self,
        input: AddQueueItemInput,
    ) -> Result<(QueueItemRecord, Vec<StateDelta>), AppError> {
        let now = now_ms();
        let mut state = self.lock()?;
        if state.queue.len() >= MAX_QUEUE_ITEMS {
            return Err(AppError::new(
                "queue_limit",
                format!("The queue is limited to {MAX_QUEUE_ITEMS} items."),
            ));
        }
        let inspection = authoritative_inspection_video(&state, &input.inspection_operation_id)?;
        let previous = state.clone();
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
        state.queue.insert(id, item.clone());
        let mut deltas = vec![next_delta(
            &mut state,
            StateDeltaValue::QueueItemUpserted(item.clone()),
        )];
        state.operations.remove(&input.inspection_operation_id);
        state
            .operation_order
            .retain(|candidate| candidate != &input.inspection_operation_id);
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::OperationRemoved(input.inspection_operation_id),
        ));
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok((item, deltas))
    }

    pub fn completed_inspection_video(&self, operation_id: &str) -> Result<VideoInfo, AppError> {
        let state = self.lock()?;
        authoritative_inspection_video(&state, operation_id)
    }

    pub fn update_queue_item(
        &self,
        id: &str,
        input: UpdateQueueItemInput,
    ) -> Result<StateDelta, AppError> {
        let mut state = self.lock()?;
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
        let previous = state.clone();
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
        let item = item.clone();
        let delta = next_delta(&mut state, StateDeltaValue::QueueItemUpserted(item));
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok(delta)
    }

    pub fn remove_queue_items(&self, ids: &[String]) -> Result<Vec<StateDelta>, AppError> {
        let mut state = self.lock()?;
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
        let previous = state.clone();
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
        let mut deltas = Vec::with_capacity(detached_operation_ids.len() + 1);
        for operation_id in detached_operation_ids {
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.queue_item_id = None;
                operation.updated_at_ms = now_ms();
                let operation = operation.clone();
                deltas.push(next_operation_delta(&mut state, operation));
            }
        }
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::QueueItemsRemoved(ids.to_vec()),
        ));
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok(deltas)
    }

    pub fn enqueue(
        &self,
        ids: &[String],
        priority: QueuePriority,
    ) -> Result<(Vec<QueuedDownload>, Vec<StateDelta>), AppError> {
        if priority == QueuePriority::Front && ids.len() != 1 {
            return Err(AppError::invalid(
                "Front priority accepts exactly one queue item.",
            ));
        }
        let now = now_ms();
        let mut state = self.lock()?;
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

        let previous = state.clone();

        let mut work = Vec::with_capacity(ids.len());
        let mut deltas = Vec::with_capacity(ids.len() * 2);
        for id in ids {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let item = state
                .queue
                .get_mut(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            item.state = QueueItemState::Queued;
            item.latest_operation_id = Some(operation_id.clone());
            item.updated_at_ms = now;
            let item = item.clone();
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
                correlation_id: uuid::Uuid::new_v4().to_string(),
            };
            state.operation_order.push(operation_id.clone());
            state
                .operations
                .insert(operation_id.clone(), operation.clone());
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
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        self.inner.pending_notify.notify_waiters();
        Ok((work, deltas))
    }

    pub fn begin_operation(
        &self,
        kind: OperationKind,
        queue_item_id: Option<String>,
    ) -> Result<(OperationSnapshot, StateDelta), AppError> {
        self.begin_operation_with_limit(kind, queue_item_id, MAX_ACTIVE_OPERATIONS)
    }

    fn begin_operation_with_limit(
        &self,
        kind: OperationKind,
        queue_item_id: Option<String>,
        limit: usize,
    ) -> Result<(OperationSnapshot, StateDelta), AppError> {
        let now = now_ms();
        let mut state = self.lock()?;
        if state.maintenance_active || state.draining {
            return Err(AppError::busy("New work is temporarily paused."));
        }
        ensure_operation_capacity(&state, 1, limit)?;
        let previous = state.clone();
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
            correlation_id: uuid::Uuid::new_v4().to_string(),
        };
        state.operation_order.push(id.clone());
        state.operations.insert(id, operation.clone());
        let delta = next_operation_delta(&mut state, operation.clone());
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok((operation, delta))
    }

    pub fn begin_maintenance_operation(
        &self,
        kind: OperationKind,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        if !matches!(
            kind,
            OperationKind::AppUpdate | OperationKind::RuntimeUpdate
        ) {
            return Err(AppError::invalid(
                "Only update operations may acquire maintenance atomically.",
            ));
        }
        let now = now_ms();
        let mut state = self.lock()?;
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
        let previous = state.clone();
        state.maintenance_active = true;
        let id = uuid::Uuid::new_v4().to_string();
        let operation = OperationSnapshot {
            schema_version: APP_SCHEMA_VERSION,
            id: id.clone(),
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
            correlation_id: uuid::Uuid::new_v4().to_string(),
        };
        state.operation_order.push(id.clone());
        state.operations.insert(id, operation.clone());
        state.maintenance_owner = Some(operation.id.clone());
        let operation_delta = next_operation_delta(&mut state, operation.clone());
        let maintenance_delta = next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged {
                active: true,
                draining: false,
            },
        );
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok((operation, vec![operation_delta, maintenance_delta]))
    }

    pub fn set_operation_state(
        &self,
        id: &str,
        operation_state: OperationState,
        error: Option<AppError>,
    ) -> Result<Vec<StateDelta>, AppError> {
        let now = now_ms();
        let mut state = self.lock()?;
        let previous = operation_state.is_terminal().then(|| state.clone());
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
            if operation.state == OperationState::Cancelling
                && !matches!(
                    operation_state,
                    OperationState::Cancelling | OperationState::Cancelled | OperationState::Failed
                )
            {
                return Err(AppError::new(
                    "invalid_transition",
                    "A cancelling operation cannot be started again.",
                ));
            }
            operation.state = operation_state;
            operation.updated_at_ms = now;
            operation.error = error;
            if operation_state.is_terminal() {
                operation.finished_at_ms = Some(now);
                operation.phase = None;
                if operation_state == OperationState::Completed {
                    operation.progress = 100.0;
                }
            }
            operation.queue_item_id.clone()
        };

        let mut deltas = Vec::with_capacity(2);
        let operation = state.operations.get(id).cloned().unwrap();
        let failure_diagnostic = failure_diagnostic(&operation);
        deltas.push(next_operation_delta(&mut state, operation));

        if let Some(queue_item_id) = queue_item_id {
            if let Some(item) = state.queue.get_mut(&queue_item_id) {
                item.state = match operation_state {
                    OperationState::Queued | OperationState::Starting => QueueItemState::Queued,
                    OperationState::Running | OperationState::Cancelling => QueueItemState::Running,
                    OperationState::Completed => QueueItemState::Completed,
                    OperationState::Failed => QueueItemState::Failed,
                    OperationState::Cancelled => QueueItemState::Cancelled,
                    OperationState::Interrupted => QueueItemState::Interrupted,
                };
                item.updated_at_ms = now;
                let item = item.clone();
                deltas.push(next_delta(
                    &mut state,
                    StateDeltaValue::QueueItemUpserted(item),
                ));
            }
        }
        let should_persist = operation_state.is_terminal();
        if should_persist {
            deltas.extend(prune_live_operations(&mut state, now));
        }
        if let Some(previous) = previous {
            self.persist_or_rollback(&mut state, previous)?;
        }
        drop(state);
        if let Some((correlation_id, message)) = failure_diagnostic {
            self.inner
                .diagnostics
                .log("error", "operation_failed", &correlation_id, &message);
        }
        self.inner.operation_notify.notify_waiters();
        Ok(deltas)
    }

    pub fn complete_inspection(
        &self,
        id: &str,
        inspection: crate::models::UrlInspection,
    ) -> Result<Vec<StateDelta>, AppError> {
        let mut state = self.lock()?;
        let previous = state.clone();
        let now = now_ms();
        let operation = state
            .operations
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.state.is_terminal() {
            return Err(AppError::new(
                "invalid_transition",
                "A terminal inspection cannot be completed again.",
            ));
        }
        if operation.state == OperationState::Cancelling {
            return Err(AppError::new(
                "invalid_transition",
                "A cancelling inspection cannot complete.",
            ));
        }
        operation.state = OperationState::Completed;
        operation.progress = 100.0;
        operation.phase = None;
        operation.updated_at_ms = now;
        operation.finished_at_ms = Some(now);
        operation.inspection_result = Some(inspection);
        operation.error = None;
        let operation = operation.clone();
        let mut deltas = vec![next_operation_delta(&mut state, operation)];
        deltas.extend(prune_live_operations(&mut state, now));
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        self.inner.operation_notify.notify_waiters();
        Ok(deltas)
    }

    pub fn request_cancellation(&self, id: &str) -> Result<StateDelta, AppError> {
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
        let operation = operation.clone();
        Ok(next_operation_delta(&mut state, operation))
    }

    pub fn apply_download_progress(
        &self,
        operation_id: &str,
        progress: &DownloadProgress,
    ) -> Option<Vec<StateDelta>> {
        let operation_state = match progress.status.as_str() {
            "completed" => OperationState::Completed,
            "cancelled" => OperationState::Cancelled,
            "error" => OperationState::Failed,
            "queued" => OperationState::Queued,
            _ => OperationState::Running,
        };
        let error = (operation_state == OperationState::Failed).then(|| {
            let mut error = AppError::new(
                progress.error_code.as_deref().unwrap_or("download_failed"),
                progress
                    .error
                    .as_deref()
                    .unwrap_or("The download operation failed."),
            )
            .retryable(true);
            if let Some(detail) = progress.error_detail.as_deref() {
                error = error.with_detail(detail);
            }
            error
        });
        let now = now_ms();
        let mut state = self.lock().ok()?;
        let previous = operation_state.is_terminal().then(|| state.clone());
        let queue_item_id = {
            let operation = state.operations.get_mut(operation_id)?;
            if operation.state.is_terminal() {
                return None;
            }
            if operation.state == OperationState::Cancelling
                && !matches!(
                    operation_state,
                    OperationState::Cancelled | OperationState::Failed
                )
            {
                return None;
            }
            operation.state = operation_state;
            operation.progress = progress.progress.clamp(0.0, 100.0);
            operation.phase = if operation_state.is_terminal() {
                None
            } else {
                progress.phase.clone()
            };
            operation.error = error;
            operation.updated_at_ms = now;
            if operation_state.is_terminal() {
                operation.finished_at_ms = Some(now);
            }
            operation.queue_item_id.clone()
        };
        let operation = state.operations.get(operation_id).cloned()?;
        let failure_diagnostic = failure_diagnostic(&operation);
        let mut deltas = vec![next_operation_delta(&mut state, operation)];
        if let Some(queue_item_id) = queue_item_id {
            if let Some(item) = state.queue.get_mut(&queue_item_id) {
                item.state = match operation_state {
                    OperationState::Queued | OperationState::Starting => QueueItemState::Queued,
                    OperationState::Running | OperationState::Cancelling => QueueItemState::Running,
                    OperationState::Completed => QueueItemState::Completed,
                    OperationState::Failed => QueueItemState::Failed,
                    OperationState::Cancelled => QueueItemState::Cancelled,
                    OperationState::Interrupted => QueueItemState::Interrupted,
                };
                item.updated_at_ms = now;
                let item = item.clone();
                deltas.push(next_delta(
                    &mut state,
                    StateDeltaValue::QueueItemUpserted(item),
                ));
            }
        }
        if operation_state.is_terminal() {
            deltas.extend(prune_live_operations(&mut state, now));
        }
        if let Some(previous) = previous {
            if let Err(error) = self.persist_or_rollback(&mut state, previous) {
                self.inner.diagnostics.log(
                    "error",
                    "terminal_state_persistence_failed",
                    &error.correlation_id,
                    &error.summary,
                );
                return None;
            }
        }
        drop(state);
        if let Some((correlation_id, message)) = failure_diagnostic {
            self.inner
                .diagnostics
                .log("error", "operation_failed", &correlation_id, &message);
        }
        if operation_state.is_terminal() {
            self.inner.operation_notify.notify_waiters();
        }
        Some(deltas)
    }

    pub fn dismiss_operation(&self, id: &str) -> Result<StateDelta, AppError> {
        let mut state = self.lock()?;
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
        let previous = state.clone();
        state.operations.remove(id);
        state.operation_order.retain(|candidate| candidate != id);
        let delta = next_delta(
            &mut state,
            StateDeltaValue::OperationRemoved(id.to_string()),
        );
        self.persist_or_rollback(&mut state, previous)?;
        drop(state);
        Ok(delta)
    }

    pub fn set_maintenance(&self, active: bool, draining: bool) -> Result<StateDelta, AppError> {
        let mut state = self.lock()?;
        if state.maintenance_owner.is_some() {
            return Err(AppError::busy(
                "Update maintenance is owned by an active operation.",
            ));
        }
        state.maintenance_active = active;
        state.draining = draining;
        Ok(next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged { active, draining },
        ))
    }

    pub fn end_maintenance_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<StateDelta>, AppError> {
        let mut state = self.lock()?;
        if state.maintenance_owner.as_deref() != Some(operation_id) {
            return Ok(None);
        }
        state.maintenance_owner = None;
        state.maintenance_active = false;
        state.draining = false;
        Ok(Some(next_delta(
            &mut state,
            StateDeltaValue::MaintenanceChanged {
                active: false,
                draining: false,
            },
        )))
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
                .cloned()
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

    pub fn set_runtime_readiness(
        &self,
        readiness: RuntimeReadiness,
    ) -> Result<StateDelta, AppError> {
        let mut state = self.lock()?;
        state.runtime_readiness = readiness;
        Ok(next_delta(
            &mut state,
            StateDeltaValue::RuntimeReadinessChanged(readiness),
        ))
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.lock()
            .map(|state| state.pending_downloads.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn take_next_pending(&self) -> Option<QueuedDownload> {
        let mut state = self.lock().ok()?;
        while let Some(operation_id) = state.pending_downloads.pop_front() {
            let operation = state.operations.get(&operation_id)?;
            if operation.state != OperationState::Queued {
                continue;
            }
            let queue_item_id = operation.queue_item_id.as_ref()?;
            let queue_item = state.queue.get(queue_item_id)?.clone();
            return Some(QueuedDownload {
                operation_id,
                queue_item,
            });
        }
        None
    }

    pub async fn wait_next_pending(&self) -> QueuedDownload {
        loop {
            let notified = self.inner.pending_notify.notified();
            if let Some(work) = self.take_next_pending() {
                return work;
            }
            notified.await;
        }
    }

    pub fn cancel_pending(&self, operation_id: &str) -> bool {
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

    pub fn emit(&self, app: &AppHandle, delta: &StateDelta) {
        if let Err(error) = app.emit("app-state-changed", delta) {
            self.inner.diagnostics.log(
                "error",
                "state_event_delivery_failed",
                &uuid::Uuid::new_v4().to_string(),
                &error.to_string(),
            );
        }
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.inner.diagnostics
    }

    #[cfg(test)]
    fn fail_next_persistence_for_test(&self) {
        self.inner.journal.fail_next_save_for_test();
    }

    fn lock(&self) -> Result<MutexGuard<'_, StateData>, AppError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AppError::internal("The application state registry is unavailable."))
    }

    fn persist_or_rollback(
        &self,
        state: &mut MutexGuard<'_, StateData>,
        previous: StateData,
    ) -> Result<(), AppError> {
        let journal = journal_from_state(state);
        if let Err(error) = self.persist(&journal) {
            **state = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self, journal: &PersistentJournal) -> Result<(), AppError> {
        self.inner.journal.save(journal).inspect_err(|error| {
            self.inner.diagnostics.log(
                "error",
                "journal_write_failed",
                &error.correlation_id,
                &error.summary,
            );
        })
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
    match operation.inspection_result.as_ref() {
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
    let cutoff = now.saturating_sub(TERMINAL_RETENTION_MS);
    let mut terminal = state
        .operations
        .values()
        .filter(|operation| operation.state.is_terminal())
        .map(|operation| {
            (
                operation.id.clone(),
                operation.finished_at_ms.unwrap_or(operation.updated_at_ms),
            )
        })
        .collect::<Vec<_>>();
    terminal.sort_by_key(|(_, timestamp)| std::cmp::Reverse(*timestamp));
    let retained = terminal
        .iter()
        .filter(|(_, timestamp)| *timestamp >= cutoff)
        .take(MAX_TERMINAL_ATTEMPTS)
        .map(|(id, _)| id.clone())
        .collect::<HashSet<_>>();
    let removed = terminal
        .into_iter()
        .filter_map(|(id, _)| (!retained.contains(&id)).then_some(id))
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
            let item = item.clone();
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
        .insert(operation.id.clone(), operation.clone());
    StateDelta {
        schema_version: APP_SCHEMA_VERSION,
        sequence: state.sequence,
        emitted_at_ms: now_ms(),
        delta: StateDeltaValue::OperationUpserted(operation),
    }
}

fn journal_from_state(state: &StateData) -> PersistentJournal {
    PersistentJournal {
        schema_version: APP_SCHEMA_VERSION,
        revision: state.sequence,
        queue: state
            .queue_order
            .iter()
            .filter_map(|id| state.queue.get(id).cloned())
            .collect(),
        operations: state
            .operation_order
            .iter()
            .filter_map(|id| state.operations.get(id).cloned())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{StateStore, MAX_TERMINAL_ATTEMPTS};
    use crate::app_error::AppError;
    use crate::models::{
        AddQueueItemInput, OperationState, QueuePriority, RuntimeReadiness, UpdateQueueItemInput,
        VideoInfo,
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

    fn add_item(
        store: &StateStore,
        index: usize,
    ) -> (
        crate::models::QueueItemRecord,
        Vec<crate::models::StateDelta>,
    ) {
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();
        store
            .complete_inspection(
                &operation.id,
                crate::models::UrlInspection::Video {
                    video: video(index),
                },
            )
            .unwrap();
        store.add_queue_item(input(operation.id)).unwrap()
    }

    #[test]
    fn sequence_is_monotonic_and_snapshot_reports_latest() {
        let store = test_store();
        let (_, first) = add_item(&store, 1);
        let (_, second) = add_item(&store, 2);
        let snapshot = store.snapshot().unwrap();

        assert!(second.last().unwrap().sequence > first.last().unwrap().sequence);
        assert_eq!(snapshot.latest_sequence, second.last().unwrap().sequence);
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.runtime_readiness, RuntimeReadiness::RepairRequired);
    }

    #[test]
    fn queue_add_consumes_only_authoritative_completed_video_inspections() {
        let store = test_store();
        assert_eq!(
            store
                .add_queue_item(input(uuid::Uuid::new_v4().to_string()))
                .unwrap_err()
                .code,
            "not_found"
        );

        let (pending, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();
        assert_eq!(
            store
                .add_queue_item(input(pending.id.clone()))
                .unwrap_err()
                .code,
            "inspection_not_completed"
        );

        let (wrong_kind, _) = store
            .begin_operation(crate::models::OperationKind::Download, None)
            .unwrap();
        store
            .set_operation_state(&wrong_kind.id, OperationState::Completed, None)
            .unwrap();
        assert_eq!(
            store.add_queue_item(input(wrong_kind.id)).unwrap_err().code,
            "invalid_inspection_operation"
        );

        store
            .set_operation_state(&pending.id, OperationState::Cancelled, None)
            .unwrap();
        let (playlist, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
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
            .unwrap();
        assert_eq!(
            store.add_queue_item(input(playlist.id)).unwrap_err().code,
            "inspection_result_kind"
        );

        let (video_operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();
        store
            .complete_inspection(
                &video_operation.id,
                crate::models::UrlInspection::Video { video: video(42) },
            )
            .unwrap();
        let (item, deltas) = store
            .add_queue_item(input(video_operation.id.clone()))
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

    #[test]
    fn retry_preserves_item_identity_and_creates_a_new_attempt() {
        let store = test_store();
        let (item, _) = add_item(&store, 1);
        let (first, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .unwrap();
        store
            .set_operation_state(&first[0].operation_id, OperationState::Failed, None)
            .unwrap();
        let (second, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .unwrap();

        assert_eq!(second[0].queue_item.id, item.id);
        assert_ne!(second[0].operation_id, first[0].operation_id);
    }

    #[test]
    fn active_items_cannot_be_edited_or_removed() {
        let store = test_store();
        let (item, _) = add_item(&store, 1);
        store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .unwrap();

        assert!(store
            .update_queue_item(&item.id, UpdateQueueItemInput::default())
            .is_err());
        assert!(store
            .remove_queue_items(std::slice::from_ref(&item.id))
            .is_err());
    }

    #[test]
    fn failed_durable_mutations_roll_back_memory_and_pending_work() {
        let store = test_store();
        let (item, _) = add_item(&store, 1);
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
            .unwrap_err();
        assert!(error.summary.contains("Injected"));
        let after_update = store.snapshot().unwrap();
        assert_eq!(after_update.latest_sequence, before.latest_sequence);
        assert_eq!(after_update.queue[0].quality, before.queue[0].quality);

        store.fail_next_persistence_for_test();
        assert!(store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .is_err());
        let after_enqueue = store.snapshot().unwrap();
        assert_eq!(after_enqueue.latest_sequence, before.latest_sequence);
        assert_eq!(after_enqueue.operations.len(), before.operations.len());
        assert!(store.take_next_pending().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn removal_detaches_terminal_history_and_reopens_without_quarantine() {
        let root =
            std::env::temp_dir().join(format!("nuclear-remove-reopen-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let (removed_item, _) = add_item(&store, 1);
        let (retained_item, _) = add_item(&store, 2);
        let (work, _) = store
            .enqueue(
                std::slice::from_ref(&removed_item.id),
                QueuePriority::Normal,
            )
            .unwrap();
        store
            .set_operation_state(&work[0].operation_id, OperationState::Completed, None)
            .unwrap();

        let deltas = store
            .remove_queue_items(std::slice::from_ref(&removed_item.id))
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
    #[test]
    fn first_mutation_after_high_revision_restart_is_persisted() {
        let root =
            std::env::temp_dir().join(format!("nuclear-revision-reopen-{}", uuid::Uuid::new_v4()));
        let journal_path = root.join("journal.dpapi");
        let diagnostics_path = root.join("diagnostics");
        let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        add_item(&store, 1);
        let revision_before_restart = store.snapshot().unwrap().latest_sequence;
        assert!(revision_before_restart > 1);
        drop(store);

        let reopened = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        assert_eq!(
            reopened.snapshot().unwrap().latest_sequence,
            revision_before_restart
        );
        add_item(&reopened, 2);
        let expected_revision = reopened.snapshot().unwrap().latest_sequence;
        drop(reopened);

        let verified = StateStore::open_at(journal_path, diagnostics_path).unwrap();
        let snapshot = verified.snapshot().unwrap();
        assert_eq!(snapshot.latest_sequence, expected_revision);
        assert_eq!(snapshot.queue.len(), 2);
        drop(verified);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn front_priority_is_single_row_only() {
        let store = test_store();
        let (first, _) = add_item(&store, 1);
        let (second, _) = add_item(&store, 2);
        assert!(store
            .enqueue(&[first.id, second.id], QueuePriority::Front)
            .is_err());
    }

    #[test]
    fn front_priority_is_next_after_the_five_admitted_rows() {
        let store = test_store();
        let items = (0..7)
            .map(|index| add_item(&store, index).0)
            .collect::<Vec<_>>();
        let normal_ids = items[..6]
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        store.enqueue(&normal_ids, QueuePriority::Normal).unwrap();
        for _ in 0..5 {
            store.take_next_pending().unwrap();
        }
        let (front, _) = store
            .enqueue(std::slice::from_ref(&items[6].id), QueuePriority::Front)
            .unwrap();

        assert_eq!(
            store.take_next_pending().unwrap().operation_id,
            front[0].operation_id
        );
    }

    #[test]
    fn maintenance_operation_is_registered_in_the_same_state_transition() {
        let store = test_store();
        let (item, _) = add_item(&store, 1);
        let (operation, deltas) = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
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
            .is_err());
        assert!(store.set_maintenance(false, false).is_err());
        let ended = store
            .end_maintenance_operation(&operation.id)
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

    #[test]
    fn queued_operation_blocks_maintenance_before_worker_registration() {
        let store = test_store();
        store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();

        let error = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
            .unwrap_err();

        assert_eq!(error.code, "busy");
        assert!(!store.snapshot().unwrap().maintenance_active);
    }

    #[test]
    fn cancelled_maintenance_operation_cannot_start_after_cancellation() {
        let store = test_store();
        let (operation, _) = store
            .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
            .unwrap();
        store
            .set_operation_state(&operation.id, OperationState::Cancelled, None)
            .unwrap();

        let error = store
            .set_operation_state(&operation.id, OperationState::Running, None)
            .unwrap_err();

        assert_eq!(error.code, "invalid_transition");
        assert_eq!(
            store.operation_state(&operation.id),
            Some(OperationState::Cancelled)
        );
    }

    #[test]
    fn cancellation_cannot_regress_or_complete_from_late_progress() {
        let store = test_store();
        let (item, _) = add_item(&store, 1);
        let (work, _) = store
            .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
            .unwrap();
        let operation_id = work[0].operation_id.clone();
        store.request_cancellation(&operation_id).unwrap();

        for status in ["queued", "downloading", "completed"] {
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
                .is_none());
            assert_eq!(
                store.operation_state(&operation_id),
                Some(OperationState::Cancelling)
            );
        }

        let cancelled = crate::models::DownloadProgress {
            download_id: operation_id.clone(),
            status: "cancelled".to_string(),
            progress: 0.0,
            phase: None,
            download_progress: None,
            conversion_progress: None,
            speed: None,
            eta: None,
            error: None,
            error_code: None,
            error_detail: None,
            filename: None,
        };
        assert!(store
            .apply_download_progress(&operation_id, &cancelled)
            .is_some());
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
            .unwrap();
        store.request_cancellation(&operation.id).unwrap();
        let transition_store = store.clone();
        let operation_id = operation.id.clone();
        let transition_id = operation_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            transition_store
                .set_operation_state(&transition_id, OperationState::Cancelled, None)
                .unwrap();
        });

        let terminal = store
            .wait_for_terminal(&operation_id, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(terminal.state, OperationState::Cancelled);
    }

    #[test]
    fn cancelling_inspection_cannot_be_completed() {
        let store = test_store();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();
        store.request_cancellation(&operation.id).unwrap();

        let error = store
            .complete_inspection(
                &operation.id,
                crate::models::UrlInspection::Video { video: video(1) },
            )
            .unwrap_err();

        assert_eq!(error.code, "invalid_transition");
        assert_eq!(
            store.operation_state(&operation.id),
            Some(OperationState::Cancelling)
        );
    }

    #[test]
    fn terminal_operations_are_pruned_from_live_snapshots_with_removal_deltas() {
        let store = test_store();
        let mut removed = 0usize;
        for _ in 0..(MAX_TERMINAL_ATTEMPTS + 7) {
            let (operation, _) = store
                .begin_operation(crate::models::OperationKind::Inspection, None)
                .unwrap();
            let deltas = store
                .set_operation_state(&operation.id, OperationState::Completed, None)
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

    #[test]
    fn concurrent_operation_admission_never_exceeds_the_limit() {
        let store = test_store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(33));
        let handles = (0..32)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.begin_operation_with_limit(
                        crate::models::OperationKind::Inspection,
                        None,
                        16,
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let admitted = handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap().is_ok()))
            .sum::<usize>();

        assert_eq!(admitted, 16);
        assert_eq!(store.snapshot().unwrap().operations.len(), 16);
    }

    #[test]
    fn operation_failure_log_uses_snapshot_correlation_and_redacts_detail() {
        let root = std::env::temp_dir().join(format!("nuclear-state-log-{}", uuid::Uuid::new_v4()));
        let diagnostics_path = root.join("diagnostics");
        let store =
            StateStore::open_at(root.join("journal.dpapi"), diagnostics_path.clone()).unwrap();
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .unwrap();
        let error = AppError::new("fixture_failed", "Inspection failed")
            .with_detail("Authorization: Bearer should-not-escape");

        store
            .set_operation_state(&operation.id, OperationState::Failed, Some(error))
            .unwrap();

        let log = std::fs::read_to_string(diagnostics_path.join("diagnostics.jsonl")).unwrap();
        assert!(log.contains(&operation.correlation_id));
        assert!(log.contains("operation_failed"));
        assert!(log.contains("fixture_failed"));
        assert!(!log.contains("should-not-escape"));
        let _ = std::fs::remove_dir_all(root);
    }
}
