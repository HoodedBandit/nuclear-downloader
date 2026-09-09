use super::reducers::{canonical_app_version, next_delta, next_operation_delta};
use super::StateStore;
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    OperationKind, OperationSnapshot, OperationState, PendingAppUpdateRecovery, StateDelta,
    StateDeltaValue, APP_SCHEMA_VERSION,
};

impl StateStore {
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
}
