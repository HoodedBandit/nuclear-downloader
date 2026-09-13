use super::data::{SharedRecord, StateData};
use super::reducers::{
    ensure_operation_capacity, next_delta, next_operation_delta, pending_download_available,
};
use super::{QueuedDownload, StateStore, MAX_ACTIVE_OPERATIONS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    OperationKind, OperationSnapshot, OperationState, QueueItemState, QueuePreparation,
    QueuePriority, StateDelta, StateDeltaValue, APP_SCHEMA_VERSION,
};
use std::collections::{HashSet, VecDeque};

impl StateStore {
    pub async fn enqueue(
        &self,
        ids: &[String],
        priority: QueuePriority,
    ) -> Result<(Vec<QueuedDownload>, Vec<StateDelta>), AppError> {
        validate_priority(priority, ids.len())?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        validate_enqueue(&state, ids)?;
        let work = append_attempts(&mut state, &mut deltas, ids, priority, now)?;
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        self.inner.pending_notify.notify_waiters();
        self.inner.preparation_notify.notify_waiters();
        Ok((work, deltas))
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.lock()
            .map(|state| {
                state
                    .pending_downloads
                    .iter()
                    .chain(state.pending_preparations.iter())
                    .cloned()
                    .collect()
            })
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
        take_next(&mut state, false)
    }

    pub async fn wait_preparation_available(&self) {
        loop {
            let notified = self.inner.preparation_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock().is_ok_and(|state| preparation_available(&state)) {
                return;
            }
            notified.await;
        }
    }

    pub async fn take_next_preparation(&self) -> Option<QueuedDownload> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock().ok()?;
        take_next(&mut state, true)
    }

    pub async fn cancel_pending(&self, operation_id: &str) -> bool {
        let _mutation = self.inner.mutation_gate.lock().await;
        let Ok(mut state) = self.lock() else {
            return false;
        };
        remove_pending(&mut state.pending_downloads, operation_id)
            | remove_pending(&mut state.pending_preparations, operation_id)
    }
}

fn validate_priority(priority: QueuePriority, count: usize) -> Result<(), AppError> {
    if priority == QueuePriority::Front && count != 1 {
        Err(AppError::invalid(
            "Front priority accepts exactly one queue item.",
        ))
    } else {
        Ok(())
    }
}

fn validate_enqueue(state: &StateData, ids: &[String]) -> Result<(), AppError> {
    if state.maintenance_active || state.draining {
        return Err(AppError::busy(
            "New work is paused while maintenance or cancellation is active.",
        ));
    }
    ensure_operation_capacity(state, ids.len(), MAX_ACTIVE_OPERATIONS)?;
    let mut unique = HashSet::with_capacity(ids.len());
    for id in ids {
        if !unique.insert(id) {
            return Err(AppError::invalid(
                "A queue item was requested more than once.",
            ));
        }
        let item = state
            .queue
            .get(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        let active = matches!(item.state, QueueItemState::Queued | QueueItemState::Running)
            || item
                .latest_operation_id
                .as_ref()
                .is_some_and(|operation_id| {
                    state
                        .operations
                        .get(operation_id)
                        .is_some_and(|operation| !operation.state.is_terminal())
                });
        if active {
            return Err(AppError::new(
                "queue_item_active",
                "The queue item already has an active attempt.",
            ));
        }
    }
    Ok(())
}

fn append_attempts(
    state: &mut StateData,
    deltas: &mut Vec<StateDelta>,
    ids: &[String],
    priority: QueuePriority,
    now: u64,
) -> Result<Vec<QueuedDownload>, AppError> {
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
            state,
            StateDeltaValue::QueueItemUpserted(item.clone()),
        ));
        let is_preparation = item.preparation == Some(QueuePreparation::Pending);
        let operation = new_operation(operation_id.clone(), id.clone(), is_preparation, now);
        state.operation_order.push(operation_id.clone());
        state
            .operations
            .insert(operation_id.clone(), operation.clone().into());
        push_pending(state, operation_id.clone(), is_preparation, priority);
        deltas.push(next_operation_delta(state, operation));
        work.push(QueuedDownload {
            operation_id,
            queue_item: item,
        });
    }
    Ok(work)
}

fn new_operation(
    id: String,
    queue_item_id: String,
    preparation: bool,
    now: u64,
) -> OperationSnapshot {
    OperationSnapshot {
        schema_version: APP_SCHEMA_VERSION,
        id,
        queue_item_id: Some(queue_item_id),
        kind: if preparation {
            OperationKind::Inspection
        } else {
            OperationKind::Download
        },
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
        playlist_admission: None,
        correlation_id: uuid::Uuid::new_v4().to_string(),
    }
}

fn push_pending(state: &mut StateData, id: String, preparation: bool, priority: QueuePriority) {
    let pending = if preparation {
        &mut state.pending_preparations
    } else {
        &mut state.pending_downloads
    };
    if priority == QueuePriority::Front {
        pending.push_front(id);
    } else {
        pending.push_back(id);
    }
}

fn preparation_available(state: &StateData) -> bool {
    state.pending_preparations.iter().any(|id| {
        state.operations.get(id).is_some_and(|operation| {
            operation.state == OperationState::Queued
                && operation
                    .queue_item_id
                    .as_ref()
                    .is_some_and(|id| state.queue.contains_key(id))
        })
    })
}

fn take_next(state: &mut StateData, preparation: bool) -> Option<QueuedDownload> {
    loop {
        let operation_id = if preparation {
            state.pending_preparations.pop_front()?
        } else {
            state.pending_downloads.pop_front()?
        };
        let Some(operation) = state.operations.get(&operation_id) else {
            continue;
        };
        if operation.state != OperationState::Queued
            || (preparation && operation.kind != OperationKind::Inspection)
        {
            continue;
        }
        let Some(queue_item_id) = operation.queue_item_id.as_ref() else {
            continue;
        };
        let Some(queue_item) = state.queue.get(queue_item_id).map(SharedRecord::snapshot) else {
            continue;
        };
        return Some(QueuedDownload {
            operation_id,
            queue_item,
        });
    }
}

fn remove_pending(pending: &mut VecDeque<String>, operation_id: &str) -> bool {
    let before = pending.len();
    pending.retain(|candidate| candidate != operation_id);
    before != pending.len()
}
