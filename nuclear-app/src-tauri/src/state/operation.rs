use super::data::SharedRecord;
use super::reducers::{
    apply_operation_transition, ensure_operation_capacity, next_delta, next_operation_delta,
    queue_state_for_operation, truncate_display_field,
};
use super::{StateStore, MAX_ACTIVE_OPERATIONS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    DownloadProgress, OperationKind, OperationSnapshot, OperationState, RuntimeReadiness,
    StateDelta, StateDeltaValue, APP_SCHEMA_VERSION,
};

impl StateStore {
    pub async fn begin_operation(
        &self,
        kind: OperationKind,
        queue_item_id: Option<String>,
    ) -> Result<(OperationSnapshot, Vec<StateDelta>), AppError> {
        self.begin_operation_with_limit(kind, queue_item_id, MAX_ACTIVE_OPERATIONS)
            .await
    }

    pub(super) async fn begin_operation_with_limit(
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
}
