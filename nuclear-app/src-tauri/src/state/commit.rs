use super::data::{SharedRecord, StateData};
use super::reducers::{
    apply_operation_transition, clear_pending_app_update, next_delta, normalize_inspection,
    prune_live_operations,
};
use super::{DownloadTerminalOutcome, FinalizationDurability, FinalizationReceipt, StateStore};
use crate::app_error::AppError;
use crate::journal::{now_ms, JournalStore};
use crate::models::{
    DownloadProgress, IntendedTerminalOutcome, OperationKind, OperationSnapshot, OperationState,
    PersistenceHealth, PublishedOutput, StateDelta, StateDeltaValue, UrlInspection,
};
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

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
        inspection: UrlInspection,
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

    pub(super) fn durable_candidate(&self) -> Result<(StateData, Vec<StateDelta>, u64), AppError> {
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

    pub(super) async fn commit_candidate(
        &self,
        candidate: StateData,
        mutation: OwnedMutexGuard<()>,
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

async fn persist_state(
    store: Arc<JournalStore>,
    state: Arc<StateData>,
    retention_now: u64,
) -> Result<(), AppError> {
    tokio::task::spawn_blocking(move || {
        let journal = state.persistence_journal();
        let prepared = journal.prepare_for_persistence(retention_now)?;
        store.save_prepared(prepared)
    })
    .await
    .map_err(|_| AppError::internal("The application journal writer stopped unexpectedly."))?
}
