use crate::app_error::AppError;
use crate::downloader;
use crate::finalize_download;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::models::OperationState;
use crate::state::StateStore;
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;

pub(crate) async fn run_registered_download<F, Fut>(
    store: &StateStore,
    manager: &DownloadManager,
    operation_id: &str,
    job: downloader::process::DownloadJob,
    run: F,
) -> downloader::DownloadOutcome
where
    F: FnOnce(downloader::process::DownloadJob) -> Fut,
    Fut: Future<Output = downloader::DownloadOutcome>,
{
    if job.is_cancelled() {
        return downloader::DownloadOutcome::Cancelled;
    }
    let permit = tokio::select! {
        biased;
        _ = job.cancelled() => Ok(None),
        permit = manager.acquire_download_slot() => permit.map(Some),
    };
    let _permit = match permit {
        Ok(Some(permit)) if !job.is_cancelled() => permit,
        Ok(_) => return downloader::DownloadOutcome::Cancelled,
        Err(summary) => {
            return downloader::DownloadOutcome::Failed {
                code: "download_scheduler_failed".into(),
                message: summary,
                detail: String::new(),
            }
        }
    };
    let _deltas = match store
        .set_operation_state(operation_id, OperationState::Running, None)
        .await
    {
        Ok(deltas) => deltas,
        Err(_)
            if job.is_cancelled()
                || store.operation_state(operation_id) == Some(OperationState::Cancelling) =>
        {
            return downloader::DownloadOutcome::Cancelled
        }
        Err(error) => {
            return downloader::DownloadOutcome::Failed {
                code: error.code,
                message: error.summary,
                detail: error.detail.unwrap_or_default(),
            }
        }
    };
    if job.is_cancelled() {
        return downloader::DownloadOutcome::Cancelled;
    }
    AssertUnwindSafe(run(job))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| downloader::DownloadOutcome::Failed {
            code: "download_task_failed".into(),
            message: "The download worker stopped unexpectedly.".into(),
            detail: String::new(),
        })
}
pub(crate) fn spawn_download_workers(
    app: &tauri::AppHandle,
    store: &StateStore,
    manager: &DownloadManager,
) -> Result<(), AppError> {
    for _ in 0..5 {
        let app = app.clone();
        let store = store.clone();
        let worker_manager = manager.clone();
        manager.spawn_tracked(TrackedTaskKind::Worker, async move {
            let manager = worker_manager;
            let shutdown = manager.shutdown_token();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return,
                    _ = store.wait_pending_available() => {},
                }
                let claim = match manager.wait_worker_claim().await {
                    Ok(Some(claim)) => claim,
                    Ok(None) => return,
                    Err(summary) => {
                        store.diagnostics().log(
                            "error",
                            "download_scheduler_failed",
                            &uuid::Uuid::new_v4().to_string(),
                            &summary,
                        );
                        return;
                    }
                };
                let Some(work) = store.take_next_pending().await else {
                    continue;
                };
                let registered = claim.registered_job(&work.operation_id).await;
                drop(claim);
                let job = match registered {
                    Ok(job) => job,
                    Err(error) => {
                        finalize_download(
                            &app,
                            &store,
                            &work.operation_id,
                            downloader::DownloadOutcome::Failed {
                                code: "download_registration_missing".into(),
                                message: error.summary,
                                detail: String::new(),
                            },
                        )
                        .await;
                        manager.finish(&work.operation_id).await;
                        continue;
                    }
                };
                let outcome =
                    run_registered_download(&store, &manager, &work.operation_id, job, |job| {
                        downloader::start_download(
                            app.clone(),
                            work.operation_id.clone(),
                            work.queue_item.to_download_request(),
                            manager.clone(),
                            job,
                        )
                    })
                    .await;
                finalize_download(&app, &store, &work.operation_id, outcome).await;
                manager.finish(&work.operation_id).await;
            }
        })?;
    }
    Ok(())
}
