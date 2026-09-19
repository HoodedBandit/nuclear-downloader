use super::inspection::{bounded_inspection, execute_inspection, finalize_inspection_result};
use crate::app_error::AppError;
use crate::downloader;
use crate::downloader::process::DownloadJob;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::models::{BeginInspectionInput, OperationKind, OperationState, UrlInspection};
use crate::state::StateStore;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

// One lifecycle-owned worker consumes the bounded durable preparation queue.
// It shares the ordinary inspection permit, so playlist preparation cannot
// increase process concurrency or consume the five download workers.
pub(crate) fn spawn_preparation_worker(
    store: &StateStore,
    manager: &DownloadManager,
) -> Result<(), AppError> {
    let store = store.clone();
    let worker_manager = manager.clone();
    manager.spawn_tracked(TrackedTaskKind::PreparationWorker, async move {
        let manager = worker_manager;
        let shutdown = manager.shutdown_token();
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = store.wait_preparation_available() => {},
            }
            let claim = match manager.wait_worker_claim().await {
                Ok(Some(claim)) => claim,
                Ok(None) => return,
                Err(summary) => {
                    store.diagnostics().log(
                        "error",
                        "preparation_scheduler_failed",
                        &uuid::Uuid::new_v4().to_string(),
                        &summary,
                    );
                    return;
                }
            };
            let Some(work) = store.take_next_preparation().await else {
                continue;
            };
            let registered = claim.registered_job(&work.operation_id).await;
            drop(claim);
            let result = match registered {
                Ok(job) => {
                    let input = BeginInspectionInput {
                        url: work.queue_item.source_url,
                        cookie_config: work.queue_item.cookie_config,
                        compat_config_path: work.queue_item.compat_config_path,
                        selection: work.queue_item.selection,
                    };
                    prepare(&store, &manager, &work.operation_id, input, &job).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = finalize_inspection_result(&store, &work.operation_id, result).await
            {
                store.diagnostics().log(
                    "error",
                    "preparation_finalization_failed",
                    &error.correlation_id,
                    &error.summary,
                );
            }
            manager.finish(&work.operation_id).await;
        }
    })
}

async fn prepare(
    store: &StateStore,
    manager: &DownloadManager,
    operation_id: &str,
    input: BeginInspectionInput,
    job: &DownloadJob,
) -> Result<Option<UrlInspection>, AppError> {
    match AssertUnwindSafe(bounded_inspection(
        job,
        execute_inspection(store, manager, operation_id, input, job),
        downloader::inspection::INSPECTION_TIMEOUT,
    ))
    .catch_unwind()
    .await
    {
        Ok(result) => result,
        Err(_) => {
            job.terminate_processes();
            Err(AppError::internal(
                "The inspection worker stopped unexpectedly.",
            ))
        }
    }
}

// Used by cancellation and shutdown for work that has not been claimed. A
// preparation operation has no download progress event or published output.
pub(crate) async fn finalize_pending_cancellation(
    store: &StateStore,
    operation_id: &str,
    publish_progress: &crate::notifications::DownloadProgressSink,
) -> Result<(), AppError> {
    if store.operation_kind(operation_id) == Some(OperationKind::Inspection) {
        store
            .finalize_operation(operation_id, OperationState::Cancelled, None)
            .await?;
    } else {
        let receipt = store
            .finalize_download(
                operation_id,
                crate::state::DownloadTerminalOutcome::Cancelled,
            )
            .await?;
        publish_progress(&receipt.progress);
    }
    Ok(())
}

pub(crate) async fn finalize_pending_cancellations(
    store: &StateStore,
    ids: &[String],
    publish_progress: &crate::notifications::DownloadProgressSink,
) -> Result<(), AppError> {
    let deltas = store
        .finalize_pending_batch(ids, OperationState::Cancelled, None)
        .await?;
    for delta in deltas {
        let crate::models::StateDeltaValue::OperationUpserted(operation) = delta.delta else {
            continue;
        };
        if operation.kind != OperationKind::Download || !operation.state.is_terminal() {
            continue;
        }
        publish_progress(&crate::models::DownloadProgress {
            download_id: operation.id,
            status: if operation.state == OperationState::Cancelled {
                "cancelled"
            } else {
                "error"
            }
            .into(),
            progress: 0.0,
            phase: None,
            download_progress: None,
            conversion_progress: None,
            speed: None,
            eta: None,
            error: operation.error.as_ref().map(|error| error.summary.clone()),
            error_code: operation.error.as_ref().map(|error| error.code.clone()),
            error_detail: operation.error.and_then(|error| error.detail),
            filename: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
