use super::commands::run_tracked_command;
use super::maintenance::{acquire_app_maintenance, AdmittedUpdate};
use super::Backend;
use crate::app_error::AppError;
use crate::lifecycle::{PublicationKind, TrackedTaskKind, UpdateRunError, UpdateTaskContext};
use crate::models::{
    BeginOperationResult, DownloaderRuntimeState, DownloaderRuntimeStatus,
    DownloaderRuntimeUpdateCheck, OperationKind, OperationState, RuntimeReadiness,
    UpdateCheckResult, UpdateInstallProgress,
};
use crate::notifications::{RuntimeProgressSink, UpdateProgressSink};
use crate::runtime;
use crate::state::StateStore;
use crate::updater::{self, InstallerHandoff};
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

type InstallerLauncher = Arc<dyn Fn(&InstallerHandoff) -> Result<(), AppError> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct InstallerActions {
    pub(crate) launch: InstallerLauncher,
    pub(crate) exit: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) fn readiness_from_status(status: &DownloaderRuntimeStatus) -> RuntimeReadiness {
    if status.update_available {
        RuntimeReadiness::UpdateAvailable
    } else {
        match status.state {
            DownloaderRuntimeState::Ready => RuntimeReadiness::Ready,
            DownloaderRuntimeState::ReadyWithWarnings => RuntimeReadiness::ReadyWithWarnings,
            DownloaderRuntimeState::RepairRequired => RuntimeReadiness::RepairRequired,
        }
    }
}

async fn record_runtime_readiness(
    store: &StateStore,
    status: &DownloaderRuntimeStatus,
) -> Result<(), AppError> {
    let _delta = store
        .set_runtime_readiness(readiness_from_status(status))
        .await?;
    Ok(())
}

pub(crate) async fn check_downloader_runtime(
    backend: Backend,
) -> Result<DownloaderRuntimeStatus, AppError> {
    let manager = backend.download_manager.clone();
    let shutdown = manager.shutdown_token();
    run_tracked_command(&manager, TrackedTaskKind::Health, async move {
        let status = runtime::check_downloader_runtime_cancellable(shutdown.clone())
            .await
            .map_err(|summary| runtime_check_error("runtime_check_failed", summary, &shutdown))?;
        record_runtime_readiness(&backend.state_store, &status).await?;
        Ok(status)
    })
    .await
}

pub(crate) async fn check_runtime_update(
    backend: Backend,
) -> Result<DownloaderRuntimeUpdateCheck, AppError> {
    let manager = backend.download_manager.clone();
    let shutdown = manager.shutdown_token();
    run_tracked_command(&manager, TrackedTaskKind::Network, async move {
        let (result, status) =
            runtime::check_downloader_runtime_update_with_status_cancellable(shutdown.clone())
                .await
                .map_err(|summary| {
                    runtime_check_error("runtime_update_check_failed", summary, &shutdown)
                })?;
        let readiness = if result.update_available {
            RuntimeReadiness::UpdateAvailable
        } else {
            readiness_from_status(&status)
        };
        let _delta = backend.state_store.set_runtime_readiness(readiness).await?;
        Ok(result)
    })
    .await
}

pub(crate) fn runtime_check_error(
    code: &str,
    summary: String,
    shutdown: &CancellationToken,
) -> AppError {
    if shutdown.is_cancelled() {
        AppError::new("shutting_down", "The application is shutting down.")
    } else {
        AppError::new(code, summary).retryable(true)
    }
}

pub(crate) async fn begin_runtime_update(
    backend: Backend,
    progress: RuntimeProgressSink,
) -> Result<BeginOperationResult, AppError> {
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let AdmittedUpdate {
            operation,
            lease,
            tracked,
            context,
        } = acquire_app_maintenance(&backend, OperationKind::RuntimeUpdate).await?;
        let operation_id = operation.id;
        let result_id = operation_id.clone();
        let store = backend.state_store.clone();
        // `tracked` was registered before spawning and owns this task until
        // terminal persistence and maintenance cleanup have both completed.
        tauri::async_runtime::spawn(async move {
            let initiating_update = tracked;
            let result = AssertUnwindSafe(async {
                context.check_cancelled()?;
                let _deltas = match store
                    .set_operation_state(&result_id, OperationState::Running, None)
                    .await
                {
                    Ok(_deltas) => _deltas,
                    Err(_) if context.is_cancelled() => return Err(UpdateRunError::Cancelled),
                    Err(error) => return Err(UpdateRunError::Failed(error)),
                };
                runtime::update_downloader_runtime(progress, context).await
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(UpdateRunError::Failed(AppError::internal(
                    "The runtime updater stopped unexpectedly.",
                )))
            });
            let (state, error) = match result {
                Ok(status) => {
                    if let Err(error) = record_runtime_readiness(&store, &status).await {
                        store.diagnostics().log(
                            "warning",
                            "runtime_readiness_publish_failed",
                            &error.correlation_id,
                            &error.summary,
                        );
                    }
                    (OperationState::Completed, None)
                }
                Err(UpdateRunError::Cancelled) => (OperationState::Cancelled, None),
                Err(UpdateRunError::Failed(error)) => (OperationState::Failed, Some(error)),
            };
            match store.finalize_operation(&result_id, state, error).await {
                Ok(_deltas) => {}
                Err(error) => store.diagnostics().log(
                    "error",
                    "runtime_update_finalization_failed",
                    &error.correlation_id,
                    &error.summary,
                ),
            }
            lease.release().await;
            drop(initiating_update);
        });
        Ok(BeginOperationResult { operation_id })
    })
    .await
}

pub(crate) async fn check_app_update(
    backend: Backend,
    current_version: String,
) -> Result<UpdateCheckResult, AppError> {
    let manager = backend.download_manager.clone();
    let shutdown = manager.shutdown_token();
    run_tracked_command(&manager, TrackedTaskKind::Network, async move {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => Err(AppError::new("shutting_down", "The application is shutting down.")),
            result = updater::check_for_app_update(&current_version) => {
                result.map_err(|summary| AppError::new("app_update_check_failed", summary).retryable(true))
            },
        }
    })
    .await
}

pub(crate) async fn begin_app_update(
    backend: Backend,
    current_version: String,
    expected_version: String,
    progress: UpdateProgressSink,
    installer_actions: InstallerActions,
) -> Result<BeginOperationResult, AppError> {
    begin_app_update_with_prepare(
        backend,
        current_version,
        expected_version,
        progress,
        installer_actions,
        |current_version, progress, expected_version, context| async move {
            updater::prepare_app_update(&current_version, progress, expected_version, &context)
                .await
        },
    )
    .await
}

async fn begin_app_update_with_prepare<P, F>(
    backend: Backend,
    current_version: String,
    expected_version: String,
    progress: UpdateProgressSink,
    installer_actions: InstallerActions,
    prepare: P,
) -> Result<BeginOperationResult, AppError>
where
    P: FnOnce(String, UpdateProgressSink, String, UpdateTaskContext) -> F + Send + 'static,
    F: Future<Output = Result<InstallerHandoff, UpdateRunError>> + Send + 'static,
{
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let AdmittedUpdate {
            operation,
            lease,
            tracked,
            context,
        } = acquire_app_maintenance(&backend, OperationKind::AppUpdate).await?;
        let operation_id = operation.id;
        let result_id = operation_id.clone();
        let store = backend.state_store.clone();
        tauri::async_runtime::spawn(async move {
            let initiating_update = tracked;
            let result = AssertUnwindSafe(async {
                context.check_cancelled()?;
                let _deltas = match store
                    .set_operation_state(&result_id, OperationState::Running, None)
                    .await
                {
                    Ok(_deltas) => _deltas,
                    Err(_) if context.is_cancelled() => return Err(UpdateRunError::Cancelled),
                    Err(error) => return Err(UpdateRunError::Failed(error)),
                };
                let handoff = prepare(
                    current_version,
                    progress.clone(),
                    expected_version,
                    context.clone(),
                )
                .await?;
                context.check_cancelled()?;
                let _deltas = match store
                    .prepare_app_update_handoff(&result_id, handoff.expected_version())
                    .await
                {
                    Ok(_deltas) => _deltas,
                    Err(_) if context.is_cancelled() => return Err(UpdateRunError::Cancelled),
                    Err(error) => return Err(UpdateRunError::Failed(error)),
                };
                launch_verified_installer(&handoff, &context, &progress, &installer_actions)?;
                Ok(handoff)
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(UpdateRunError::Failed(AppError::internal(
                    "The application updater stopped unexpectedly.",
                )))
            });
            let (state, error) = match result {
                Ok(handoff) => {
                    lease.release_for_installer_handoff().await;
                    // Unregister the initiating update before requesting app
                    // shutdown; its pending journal record reconciles next run.
                    drop(initiating_update);
                    (installer_actions.exit)();
                    drop(handoff);
                    return;
                }
                Err(UpdateRunError::Cancelled) => (OperationState::Cancelled, None),
                Err(UpdateRunError::Failed(error)) => (OperationState::Failed, Some(error)),
            };
            match store.finalize_operation(&result_id, state, error).await {
                Ok(_deltas) => {}
                Err(error) => store.diagnostics().log(
                    "error",
                    "app_update_finalization_failed",
                    &error.correlation_id,
                    &error.summary,
                ),
            }
            lease.release().await;
            drop(initiating_update);
        });
        Ok(BeginOperationResult { operation_id })
    })
    .await
}

fn launch_verified_installer(
    handoff: &InstallerHandoff,
    context: &UpdateTaskContext,
    progress: &UpdateProgressSink,
    installer_actions: &InstallerActions,
) -> Result<(), UpdateRunError> {
    let publication = context.enter_publication(PublicationKind::InstallerHandoff)?;
    progress(UpdateInstallProgress {
        status: "launching".into(),
        version: handoff.expected_version().to_string(),
        downloaded_bytes: handoff.installer_size(),
        total_bytes: Some(handoff.installer_size()),
        message: Some(format!(
            "Launching {}. Nuclear Downloader will close and reopen after install.",
            handoff.installer_name()
        )),
    });
    (installer_actions.launch)(handoff)?;
    publication.commit();
    Ok(())
}

#[cfg(all(test, windows))]
mod tests;
