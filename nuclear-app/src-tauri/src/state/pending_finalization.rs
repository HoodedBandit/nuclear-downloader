use super::reducers::{apply_operation_transition, next_delta};
use super::StateStore;
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    IntendedTerminalOutcome, OperationState, PersistenceHealth, StateDelta, StateDeltaValue,
};
use futures_util::FutureExt;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;

impl StateStore {
    pub async fn finalize_pending_batch(
        &self,
        ids: &[String],
        operation_state: OperationState,
        error: Option<AppError>,
    ) -> Result<Vec<StateDelta>, AppError> {
        validate_terminal_state(operation_state)?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut candidate, mut deltas, retention_now) = self.durable_candidate()?;
        let transition_ids = collect_transition_ids(&candidate, ids)?;
        if transition_ids.is_empty() {
            return Ok(Vec::new());
        }
        apply_batch_outcome(
            &mut candidate,
            &mut deltas,
            &transition_ids,
            operation_state,
            error.clone(),
        )?;
        let candidate_sequence = candidate.sequence;
        let store = self.clone();
        let finalizer = tokio::spawn(async move {
            let _mutation = mutation;
            let result = AssertUnwindSafe(async {
                #[cfg(test)]
                store.pause_commit_for_test().await;
                match store
                    .persist_candidate_with_retry(&candidate, retention_now)
                    .await
                {
                    Ok(()) => publish_candidate(&store, candidate, deltas),
                    Err(save_error) => store.install_degraded_pending_batch(
                        &transition_ids,
                        operation_state,
                        error.clone(),
                        candidate_sequence,
                        &save_error,
                    ),
                }
            })
            .catch_unwind()
            .await;
            result.unwrap_or_else(|_| {
                store.install_degraded_pending_batch(
                    &transition_ids,
                    operation_state,
                    error,
                    candidate_sequence,
                    &AppError::new(
                        "state_finalizer_failed",
                        "The pending batch finalizer stopped unexpectedly.",
                    ),
                )
            })
        });
        finalizer.await.map_err(|error| {
            AppError::new(
                "state_finalizer_failed",
                "The pending batch finalizer stopped unexpectedly.",
            )
            .retryable(true)
            .with_detail(error.to_string())
        })?
    }

    fn install_degraded_pending_batch(
        &self,
        ids: &[String],
        intended_state: OperationState,
        intended_error: Option<AppError>,
        candidate_sequence: u64,
        cause: &AppError,
    ) -> Result<Vec<StateDelta>, AppError> {
        let mut fallback = self.lock()?.clone();
        fallback.sequence = fallback.sequence.max(candidate_sequence);
        let degraded_error = AppError::new(
            "state_persistence_failed",
            "The operations finished, but their final states could not be saved.",
        )
        .retryable(true)
        .with_detail(cause.summary.clone());
        for id in ids {
            fallback
                .operations
                .get_mut(id)
                .ok_or_else(|| AppError::not_found("operation"))?
                .intended_terminal_outcome = Some(Box::new(IntendedTerminalOutcome {
                state: intended_state,
                error: intended_error.clone(),
            }));
        }
        let mut deltas = Vec::with_capacity(ids.len() * 2 + 1);
        apply_batch_outcome(
            &mut fallback,
            &mut deltas,
            ids,
            OperationState::Failed,
            Some(degraded_error.clone()),
        )?;
        fallback.persistence_dirty = true;
        fallback.persistence_health = PersistenceHealth {
            degraded: true,
            error: Some(degraded_error),
        };
        let health = fallback.persistence_health.clone();
        deltas.push(next_delta(
            &mut fallback,
            StateDeltaValue::PersistenceHealthChanged(health),
        ));
        publish_candidate(self, fallback, deltas)
    }
}

fn validate_terminal_state(state: OperationState) -> Result<(), AppError> {
    matches!(state, OperationState::Cancelled | OperationState::Failed)
        .then_some(())
        .ok_or_else(|| {
            AppError::invalid(
                "Pending batch finalization accepts only cancelled or failed outcomes.",
            )
        })
}

fn collect_transition_ids(
    state: &super::data::StateData,
    ids: &[String],
) -> Result<Vec<String>, AppError> {
    let mut unique = HashSet::with_capacity(ids.len());
    let mut transitions = Vec::with_capacity(ids.len());
    for id in ids {
        if !unique.insert(id.as_str()) {
            return Err(AppError::invalid(
                "A pending operation was requested more than once.",
            ));
        }
        let operation = state
            .operations
            .get(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.state.is_terminal() {
            continue;
        }
        if operation.state == OperationState::Running {
            return Err(AppError::new(
                "operation_active",
                "Only pending, unregistered operations may be finalized as a batch.",
            ));
        }
        transitions.push(id.clone());
    }
    Ok(transitions)
}

fn apply_batch_outcome(
    state: &mut super::data::StateData,
    deltas: &mut Vec<StateDelta>,
    ids: &[String],
    outcome: OperationState,
    error: Option<AppError>,
) -> Result<(), AppError> {
    let now = now_ms();
    for id in ids {
        deltas.extend(apply_operation_transition(
            state,
            id,
            outcome,
            error.clone(),
            None,
            None,
            now,
        )?);
    }
    Ok(())
}

fn publish_candidate(
    store: &StateStore,
    candidate: super::data::StateData,
    deltas: Vec<StateDelta>,
) -> Result<Vec<StateDelta>, AppError> {
    let latest_sequence = candidate.sequence;
    store.install_candidate(candidate)?;
    store.inner.outbox.enqueue(deltas.clone(), latest_sequence);
    store.inner.operation_notify.notify_waiters();
    Ok(deltas)
}
