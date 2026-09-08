use crate::app_error::AppError;
use crate::downloader;
use crate::lifecycle::{DownloadManager, TrackedTaskKind, DOWNLOAD_CAPACITY};
use crate::models::DownloadProgress;
use crate::notifications::{DownloadNotifications, DownloadProgressSink};
use crate::scheduling::run_registered_download;
use crate::state::{self, StateStore};
use futures_util::FutureExt;
use std::sync::Arc;

pub(crate) fn download_notifications(
    store: &StateStore,
    publish_progress: DownloadProgressSink,
) -> DownloadNotifications {
    let progress_store = store.clone();
    let diagnostics = store.diagnostics().clone();
    DownloadNotifications {
        progress: Arc::new(move |progress| {
            let store = progress_store.clone();
            let publish_progress = publish_progress.clone();
            async move {
                if record_download_progress(&store, &progress).await {
                    publish_progress(&progress);
                }
            }
            .boxed()
        }),
        cleanup_warning: Arc::new(move |operation_id, error| {
            diagnostics.log(
                "warning",
                "download_stage_cleanup_failed",
                operation_id,
                error,
            );
        }),
    }
}
pub(crate) async fn record_download_progress(
    store: &StateStore,
    progress: &DownloadProgress,
) -> bool {
    match store
        .apply_download_progress(&progress.download_id, progress)
        .await
    {
        Ok(Some(_deltas)) => true,
        Ok(None) => false,
        Err(error) => {
            store.diagnostics().log(
                "error",
                "download_progress_state_failed",
                &error.correlation_id,
                &error.summary,
            );
            false
        }
    }
}

pub(crate) async fn finalize_download(
    publish_progress: &DownloadProgressSink,
    store: &StateStore,
    operation_id: &str,
    outcome: downloader::DownloadOutcome,
) {
    let outcome = match outcome {
        downloader::DownloadOutcome::Completed { filename } => {
            state::DownloadTerminalOutcome::Completed { filename }
        }
        downloader::DownloadOutcome::Cancelled => state::DownloadTerminalOutcome::Cancelled,
        downloader::DownloadOutcome::Failed {
            code,
            message,
            detail,
        } => state::DownloadTerminalOutcome::Failed(
            AppError::new(code, message)
                .with_detail(detail)
                .retryable(true),
        ),
    };
    match store.finalize_download(operation_id, outcome).await {
        Ok(receipt) => {
            if receipt.durability == state::FinalizationDurability::Degraded {
                store.diagnostics().log(
                    "error",
                    "download_finished_with_degraded_persistence",
                    operation_id,
                    "The worker finished, but its outcome could not be saved. New durable work must first recover persistence.",
                );
            }
            publish_progress(&receipt.progress);
        }
        Err(error) => store.diagnostics().log(
            "error",
            "download_finalization_failed",
            &error.correlation_id,
            &error.summary,
        ),
    }
}

pub(crate) fn spawn_download_workers(
    publish_progress: DownloadProgressSink,
    store: &StateStore,
    manager: &DownloadManager,
) -> Result<(), AppError> {
    for _ in 0..DOWNLOAD_CAPACITY {
        let publish_progress = publish_progress.clone();
        let notifications = download_notifications(store, publish_progress.clone());
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
                            &publish_progress,
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
                            notifications.clone(),
                            work.operation_id.clone(),
                            work.queue_item.to_download_request(),
                            manager.clone(),
                            job,
                        )
                    })
                    .await;
                finalize_download(&publish_progress, &store, &work.operation_id, outcome).await;
                manager.finish(&work.operation_id).await;
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
