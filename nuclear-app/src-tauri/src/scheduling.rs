use crate::downloader;
use crate::lifecycle::DownloadManager;
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
