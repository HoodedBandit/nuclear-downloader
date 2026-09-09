use super::data::SharedRecord;
use super::reducers::{
    authoritative_inspection_video, ensure_operation_capacity, next_delta, next_operation_delta,
    pending_download_available, validate_actionable_field, validate_cookie_config,
    validate_optional_actionable_field,
};
use super::{QueuedDownload, StateStore, MAX_ACTIVE_OPERATIONS, MAX_QUEUE_ITEMS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    AddQueueItemInput, OperationKind, OperationSnapshot, OperationState, QueueItemRecord,
    QueueItemState, QueuePriority, StateDelta, StateDeltaValue, UpdateQueueItemInput, VideoInfo,
    APP_SCHEMA_VERSION,
};
use std::collections::HashSet;

impl StateStore {
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
}
