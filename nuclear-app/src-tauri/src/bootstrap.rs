use crate::app_error::AppError;
use crate::downloader;
use crate::models::RuntimeReadiness;
use crate::notifications::DownloadProgressSink;
use crate::services::downloads::finalize_download;
use crate::services::updates::runtime_check_error;
use crate::services::Backend;
use crate::{runtime, updater};
use std::time::Duration;

pub(crate) fn start(backend: &Backend, current_version: String) -> Result<(), AppError> {
    let store = backend.state_store.clone();
    let startup_report_store = store.clone();
    let startup_shutdown = backend.download_manager.shutdown_token();
    backend.download_manager.spawn_startup(
        async move {
            let _delta = store.set_maintenance(true, false).await?;
            runtime::recover_runtime_update_transaction()
                .await
                .map_err(|summary| {
                    AppError::new(
                        "runtime_recovery_required",
                        "The interrupted runtime update needs recovery before work can start.",
                    )
                    .with_detail(summary)
                    .retryable(true)
                })?;
            let _deltas = store.reconcile_pending_app_update(&current_version).await?;
            if let Err(summary) = updater::cleanup_owned_installer_stages().await {
                store.diagnostics().log(
                    "warning",
                    "installer_stage_cleanup_failed",
                    &uuid::Uuid::new_v4().to_string(),
                    &summary,
                );
            }
            if let Err(summary) = runtime::cleanup_abandoned_runtime_updates().await {
                store.diagnostics().log(
                    "warning",
                    "runtime_cleanup_failed",
                    &uuid::Uuid::new_v4().to_string(),
                    &summary,
                );
            }
            let output_roots = store.journaled_output_roots();
            match tauri::async_runtime::spawn_blocking(move || {
                downloader::publication::cleanup_abandoned_download_stages(&output_roots)
            })
            .await
            {
                Ok(failures) => {
                    for failure in failures {
                        store.diagnostics().log(
                            "warning",
                            "download_stage_cleanup_failed",
                            &uuid::Uuid::new_v4().to_string(),
                            &failure,
                        );
                    }
                }
                Err(_) => {
                    let error = AppError::new(
                        "startup_cleanup_failed",
                        "The abandoned-stage cleanup worker stopped unexpectedly.",
                    );
                    store.diagnostics().log(
                        "error",
                        "startup_cleanup_failed",
                        &error.correlation_id,
                        &error.summary,
                    );
                    return Err(error);
                }
            }
            if let Err(summary) = runtime::initialize_runtime_cache(startup_shutdown.clone()).await
            {
                let error = runtime_check_error(
                    "runtime_initialization_failed",
                    summary,
                    &startup_shutdown,
                );
                if startup_shutdown.is_cancelled() {
                    return Err(error);
                }
                store.diagnostics().log(
                    "warning",
                    "runtime_repair_required",
                    &error.correlation_id,
                    &error.summary,
                );
                store
                    .set_runtime_readiness(RuntimeReadiness::RepairRequired)
                    .await?;
            }
            let _delta = store.set_maintenance(false, false).await?;
            Ok(())
        },
        move |result| {
            if let Err(error) = result {
                startup_report_store.diagnostics().log(
                    "error",
                    "startup_admission_closed",
                    &error.correlation_id,
                    &error.summary,
                );
            }
        },
    )
}

// This future is driven by the desktop's untracked shutdown task. Registering
// the driver with the coordinator would make it wait on itself.
pub(crate) async fn shutdown(backend: Backend, publish_progress: DownloadProgressSink) {
    let manager = backend.download_manager;
    let store = backend.state_store;
    let cleanup_store = store.clone();
    let cleanup_manager = manager.clone();
    let cleanup_progress = publish_progress.clone();
    let cleanup_shutdown = manager.shutdown_token();
    if let Err(error) = manager.spawn_cleanup_continuation(async move {
        cleanup_shutdown.cancelled().await;
        if let Err(error) = cleanup_store.set_maintenance(true, true).await {
            cleanup_store.diagnostics().log(
                "warning",
                "shutdown_state_failed",
                &error.correlation_id,
                &error.summary,
            );
        }
        for operation_id in cleanup_store.pending_operation_ids() {
            if cleanup_store.cancel_pending(&operation_id).await {
                finalize_download(
                    &cleanup_progress,
                    &cleanup_store,
                    &operation_id,
                    downloader::DownloadOutcome::Cancelled,
                )
                .await;
                cleanup_manager.finish(&operation_id).await;
            }
        }
    }) {
        store.diagnostics().log(
            "error",
            "shutdown_cleanup_registration_failed",
            &error.correlation_id,
            &error.summary,
        );
    }
    manager.begin_shutdown().await;
    if let Err(timeout) = manager
        .wait_for_shutdown_tasks(Duration::from_secs(15))
        .await
    {
        store.diagnostics().log(
            "warning",
            "shutdown_cleanup_timeout",
            &uuid::Uuid::new_v4().to_string(),
            &format!(
                "Cleanup exceeded the ordinary 15-second grace: {:?}",
                timeout.outstanding
            ),
        );
    }
}
