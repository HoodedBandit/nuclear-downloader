use crate::app_error::AppError;
use crate::lifecycle::{DownloadManager, DrainCompletion, DrainTicket, TrackedTaskKind};
use crate::models::{CancelAllResult, DownloadProgress};
use crate::state::{DownloadTerminalOutcome, StateStore};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

pub(crate) type ProgressPublisher = Arc<dyn Fn(&DownloadProgress) + Send + Sync>;

struct DrainCleanup {
    manager: DownloadManager,
    store: StateStore,
    publish_progress: ProgressPublisher,
    ticket: DrainTicket,
}

struct DrainGuard(Option<DrainCleanup>);

impl DrainGuard {
    async fn finish(mut self) -> Result<CancelAllResult, AppError> {
        let result = complete_drain(self.0.as_ref().expect("active drain guard")).await;
        if result.is_ok() {
            self.0.take();
        }
        result
    }
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        let Some(cleanup) = self.0.take() else {
            return;
        };
        let manager = cleanup.manager.clone();
        let diagnostics = cleanup.store.diagnostics().clone();
        if let Err(error) = manager.spawn_cleanup_continuation(async move {
            let result = AssertUnwindSafe(complete_drain(&cleanup))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(AppError::internal(
                        "Cancellation recovery stopped unexpectedly.",
                    ))
                });
            if let Err(error) = result {
                cleanup.store.diagnostics().log(
                    "error",
                    "cancel_all_recovery_failed",
                    &error.correlation_id,
                    &error.summary,
                );
            }
        }) {
            diagnostics.log(
                "error",
                "cancel_all_recovery_registration_failed",
                &error.correlation_id,
                &error.summary,
            );
        }
    }
}

async fn complete_drain(cleanup: &DrainCleanup) -> Result<CancelAllResult, AppError> {
    let DrainCleanup {
        manager,
        store,
        publish_progress,
        ticket,
    } = cleanup;
    // A dropped guard from a completed generation must never pause newer work.
    if !manager.owns_drain(*ticket).await {
        return Ok(CancelAllResult {
            idle: false,
            remaining_operation_ids: manager.active_ids().await,
        });
    }
    store.set_maintenance(true, true).await?;
    for operation_id in manager.active_ids().await {
        match store.request_cancellation(&operation_id).await {
            Ok(_) => {}
            Err(_)
                if store
                    .operation_state(&operation_id)
                    .is_some_and(|state| state.is_terminal()) => {}
            Err(error) => store.diagnostics().log(
                "warning",
                "cancellation_state_failed",
                &error.correlation_id,
                &error.summary,
            ),
        }
    }
    // Admission and worker claims are closed for this drain generation. Keep
    // each pending ID until both finalization and deregistration have completed,
    // so an unwind can repeat cleanup without losing ownership of a pending job.
    for operation_id in store.pending_operation_ids() {
        if store
            .operation_state(&operation_id)
            .is_some_and(|state| !state.is_terminal())
        {
            let receipt = store
                .finalize_download(&operation_id, DownloadTerminalOutcome::Cancelled)
                .await?;
            publish_progress(&receipt.progress);
        }
        manager.finish(&operation_id).await;
        store.cancel_pending(&operation_id).await;
    }
    match manager.wait_for_drain(*ticket).await {
        DrainCompletion::Idle => {
            store.set_maintenance(false, false).await?;
            let resumed = manager.resume_after_drain(*ticket).await;
            Ok(CancelAllResult {
                idle: resumed,
                remaining_operation_ids: manager.active_ids().await,
            })
        }
        DrainCompletion::SupersededByShutdown => Ok(CancelAllResult {
            idle: false,
            remaining_operation_ids: manager.active_ids().await,
        }),
    }
}

pub(crate) async fn cancel_all(
    store: StateStore,
    manager: DownloadManager,
    publish_progress: ProgressPublisher,
) -> Result<CancelAllResult, AppError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task_manager = manager.clone();
    manager.spawn_tracked(TrackedTaskKind::Drain, async move {
        let result = AssertUnwindSafe(async {
            task_manager.wait_for_startup().await?;
            let ticket = task_manager.begin_cancel_all().await?;
            DrainGuard(Some(DrainCleanup {
                manager: task_manager,
                store: store.clone(),
                publish_progress,
                ticket,
            }))
            .finish()
            .await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(AppError::internal(
                "The cancellation task stopped unexpectedly; cleanup continues.",
            ))
        });
        if let Err(error) = &result {
            store.diagnostics().log(
                "error",
                "cancel_all_drain_failed",
                &error.correlation_id,
                &error.summary,
            );
        }
        let _ = sender.send(result);
    })?;
    match tokio::time::timeout(crate::CANCELLATION_WAIT_TIMEOUT, receiver).await {
        Ok(result) => {
            result.map_err(|_| AppError::internal("The cancellation result was unavailable."))?
        }
        Err(_) => Ok(CancelAllResult {
            idle: false,
            remaining_operation_ids: manager.active_ids().await,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{OperationState, QueuePriority};
    use std::time::Duration;

    async fn unwind_drain(with_pending: bool, finalize_before_panic: bool) {
        let root =
            std::env::temp_dir().join(format!("nuclear-drain-unwind-{}", uuid::Uuid::new_v4()));
        let store =
            StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
        let manager = DownloadManager::new(5, 1, 1);
        manager.open_for_test().await;
        let operation_id = if with_pending {
            let item = crate::backend_lifecycle_tests::fixture(&store, &root).await;
            let admission = manager.begin_job_admission(1).await.unwrap();
            let (work, _) = store
                .enqueue(&[item.id], QueuePriority::Normal)
                .await
                .unwrap();
            let id = work[0].operation_id.clone();
            admission.publish(std::slice::from_ref(&id)).await.unwrap();
            Some(id)
        } else {
            None
        };
        let task_manager = manager.clone();
        let task_store = store.clone();
        let task_operation_id = operation_id.clone();
        let task = tokio::spawn(async move {
            let ticket = task_manager.begin_cancel_all().await.unwrap();
            let _guard = DrainGuard(Some(DrainCleanup {
                manager: task_manager,
                store: task_store.clone(),
                publish_progress: Arc::new(|_| {}),
                ticket,
            }));
            if finalize_before_panic {
                task_store
                    .finalize_download(
                        task_operation_id.as_deref().unwrap(),
                        DownloadTerminalOutcome::Cancelled,
                    )
                    .await
                    .unwrap();
            }
            panic!("injected panic after acquiring the drain ticket");
        });
        assert!(task.await.unwrap_err().is_panic());
        let claim = tokio::time::timeout(Duration::from_secs(5), manager.wait_worker_claim())
            .await
            .unwrap()
            .unwrap()
            .expect("cleanup must reopen admission");
        drop(claim);
        assert!(manager.active_ids().await.is_empty());
        assert!(store.pending_operation_ids().is_empty());
        if let Some(id) = &operation_id {
            assert_eq!(store.operation_state(id), Some(OperationState::Cancelled));
        }
        manager.begin_shutdown().await;
        manager
            .wait_for_shutdown_tasks(Duration::from_secs(2))
            .await
            .unwrap();
        drop(store);
        let reopened =
            StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
        if let Some(id) = &operation_id {
            assert_eq!(
                reopened.operation_state(id),
                Some(OperationState::Cancelled)
            );
        }
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn abandoned_empty_drain_reopens_admission() {
        unwind_drain(false, false).await;
    }

    #[tokio::test]
    async fn abandoned_drain_finalizes_pending_work_before_reopening() {
        unwind_drain(true, false).await;
    }

    #[tokio::test]
    async fn abandoned_drain_releases_already_finalized_pending_registration() {
        unwind_drain(true, true).await;
    }
}
