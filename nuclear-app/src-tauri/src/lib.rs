mod app_error;
#[cfg(test)]
mod backend_lifecycle_tests;
#[cfg(test)]
#[path = "../build_config.rs"]
mod build_config;
mod cancellation;
mod diagnostics;
mod downloader;
mod journal;
mod journal_commit;
mod lifecycle;
mod lifecycle_cleanup;
mod models;
mod outbox;
#[cfg(test)]
mod performance_harness;
mod runtime;
mod runtime_transaction;
mod scheduling;
#[cfg(test)]
mod soak_harness;
mod state;
mod state_events;
mod updater;

use app_error::AppError;
use futures_util::FutureExt;
use lifecycle::{
    create_download_manager, DownloadManager, MaintenanceLease as ManagerMaintenanceLease,
};
use lifecycle::{TrackedTaskKind, UpdateRunError};
use lifecycle_cleanup::InspectionAdmissionGuard;
use models::{
    AddQueueItemInput, AppSnapshot, BeginInspectionInput, BeginOperationResult, CancelAllResult,
    DownloadProgress, DownloadRequest, OperationKind, OperationState, QueueItemRecord,
    QueuePriority, RuntimeReadiness, UpdateQueueItemInput,
};
#[cfg(test)]
use scheduling::run_registered_download;
use scheduling::spawn_download_workers;
use state::StateStore;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Manager, State};

const CANCELLATION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct AppState {
    download_manager: DownloadManager,
    state_store: StateStore,
}

struct AppMaintenanceLeaseInner {
    manager_lease: ManagerMaintenanceLease,
    store: StateStore,
    coordinator: DownloadManager,
    operation_id: String,
}

struct AppMaintenanceLease {
    inner: Option<AppMaintenanceLeaseInner>,
}

impl AppMaintenanceLease {
    async fn release(mut self) {
        if let Some(inner) = self.inner.take() {
            release_app_maintenance(inner).await;
        }
    }

    async fn release_for_installer_handoff(mut self) {
        if let Some(inner) = self.inner.take() {
            // The operation remains installing until the next launch reconciles
            // its durable expected version. The coordinator's committed handoff
            // latch prevents this lease release from reopening admission.
            inner.manager_lease.release().await;
        }
    }
}

impl Drop for AppMaintenanceLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let coordinator = inner.coordinator.clone();
            let store = inner.store.clone();
            if let Err(error) =
                coordinator.spawn_cleanup_continuation(release_app_maintenance(inner))
            {
                store.diagnostics().log(
                    "warning",
                    "maintenance_cleanup_registration_failed",
                    &error.correlation_id,
                    &error.summary,
                );
            }
        }
    }
}

async fn release_app_maintenance(inner: AppMaintenanceLeaseInner) {
    match inner
        .store
        .end_maintenance_operation(&inner.operation_id)
        .await
    {
        Ok(Some(_delta)) => {}
        Ok(None) => {}
        Err(error) => inner.store.diagnostics().log(
            "warning",
            "maintenance_state_cleanup_failed",
            &error.correlation_id,
            &error.summary,
        ),
    }
    inner.manager_lease.release().await;
}

struct AdmittedUpdate {
    operation: models::OperationSnapshot,
    lease: AppMaintenanceLease,
    tracked: lifecycle::TrackedUpdate,
    context: lifecycle::UpdateTaskContext,
}

async fn acquire_app_maintenance(
    _app: &tauri::AppHandle,
    state: &AppState,
    kind: OperationKind,
) -> Result<AdmittedUpdate, AppError> {
    let manager_lease = state.download_manager.acquire_maintenance().await?;
    let operation_id = uuid::Uuid::new_v4().to_string();
    let (tracked, context) = match state
        .download_manager
        .register_update(operation_id.clone(), kind)
    {
        Ok(registration) => registration,
        Err(error) => {
            manager_lease.release().await;
            return Err(error);
        }
    };
    let (operation, _deltas) = match state
        .state_store
        .begin_maintenance_operation_with_id(kind, operation_id)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            drop(tracked);
            manager_lease.release().await;
            return Err(error);
        }
    };
    let lease = AppMaintenanceLease {
        inner: Some(AppMaintenanceLeaseInner {
            manager_lease,
            store: state.state_store.clone(),
            coordinator: state.download_manager.clone(),
            operation_id: operation.id.clone(),
        }),
    };
    Ok(AdmittedUpdate {
        operation,
        lease,
        tracked,
        context,
    })
}
// The backend owns admission and cancellation through completion. Dropping an
// IPC response must not abandon a journal commit, registered task, or drain.
async fn run_tracked_command<T, F>(
    coordinator: &DownloadManager,
    kind: TrackedTaskKind,
    future: F,
) -> Result<T, AppError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, AppError>> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task_coordinator = coordinator.clone();
    coordinator.spawn_tracked(kind, async move {
        let result = AssertUnwindSafe(async move {
            task_coordinator.wait_for_startup().await?;
            future.await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(AppError::internal(
                "The backend command stopped unexpectedly.",
            ))
        });
        let _ = sender.send(result);
    })?;
    receiver
        .await
        .map_err(|_| AppError::internal("The backend command could not return its result."))?
}

fn readiness_from_status(status: &models::DownloaderRuntimeStatus) -> RuntimeReadiness {
    if status.update_available {
        RuntimeReadiness::UpdateAvailable
    } else {
        match status.state {
            models::DownloaderRuntimeState::Ready => RuntimeReadiness::Ready,
            models::DownloaderRuntimeState::ReadyWithWarnings => {
                RuntimeReadiness::ReadyWithWarnings
            }
            models::DownloaderRuntimeState::RepairRequired => RuntimeReadiness::RepairRequired,
        }
    }
}

async fn record_runtime_readiness(
    _app: &tauri::AppHandle,
    store: &StateStore,
    status: &models::DownloaderRuntimeStatus,
) -> Result<(), AppError> {
    let _delta = store
        .set_runtime_readiness(readiness_from_status(status))
        .await?;
    Ok(())
}

pub(crate) async fn record_download_progress(
    app: &tauri::AppHandle,
    progress: &DownloadProgress,
) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    match state
        .state_store
        .apply_download_progress(&progress.download_id, progress)
        .await
    {
        Ok(Some(_deltas)) => true,
        Ok(None) => false,
        Err(error) => {
            state.state_store.diagnostics().log(
                "error",
                "download_progress_state_failed",
                &error.correlation_id,
                &error.summary,
            );
            false
        }
    }
}

async fn finalize_download(
    app: &tauri::AppHandle,
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
            if let Err(error) = app.emit("download-progress", &receipt.progress) {
                record_event_delivery_failure(app, "download-progress", &error.to_string());
            }
        }
        Err(error) => store.diagnostics().log(
            "error",
            "download_finalization_failed",
            &error.correlation_id,
            &error.summary,
        ),
    }
}

pub(crate) fn record_event_delivery_failure(app: &tauri::AppHandle, event: &str, error: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    state.state_store.diagnostics().log(
        "error",
        "event_delivery_failed",
        &uuid::Uuid::new_v4().to_string(),
        &format!("Event {event} could not be delivered: {error}"),
    );
}

pub(crate) fn record_download_cleanup_warning(
    app: &tauri::AppHandle,
    operation_id: &str,
    error: &str,
) {
    if let Some(state) = app.try_state::<AppState>() {
        state.state_store.diagnostics().log(
            "warning",
            "download_stage_cleanup_failed",
            operation_id,
            error,
        );
    }
}

#[tauri::command]
async fn begin_inspection(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: BeginInspectionInput,
) -> Result<BeginOperationResult, AppError> {
    downloader::validate_fetch_request(
        &input.url,
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
    )
    .map_err(AppError::invalid)?;
    let backend = state.inner().clone();
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let admission = backend.download_manager.begin_job_admission(1).await?;
        let (operation, _deltas) = backend
            .state_store
            .begin_operation(OperationKind::Inspection, None)
            .await?;
        let operation_id = operation.id;
        let admission_cleanup = InspectionAdmissionGuard::new(
            backend.state_store.clone(),
            backend.download_manager.clone(),
            operation_id.clone(),
        );
        let published = match admission.publish(std::slice::from_ref(&operation_id)).await {
            Ok(jobs) => jobs,
            Err(error) => return Err(admission_cleanup.finalize(error).await),
        };
        let job = match published.into_iter().next() {
            Some(job) => job,
            None => {
                let error = AppError::internal("The inspection was not registered.");
                return Err(admission_cleanup.finalize(error).await);
            }
        };
        let result_id = operation_id.clone();
        let task_store = backend.state_store.clone();
        let manager = backend.download_manager.clone();
        let task = async move {
            let result = AssertUnwindSafe(bounded_inspection(
                &job,
                execute_inspection(&task_store, &manager, &result_id, input, &job),
                downloader::inspection::INSPECTION_TIMEOUT,
            ))
            .catch_unwind()
            .await;
            let result = match result {
                Ok(result) => result,
                Err(_) => {
                    job.terminate_processes();
                    Err(AppError::internal(
                        "The inspection worker stopped unexpectedly.",
                    ))
                }
            };
            let finalized = finalize_inspection_result(&task_store, &result_id, result).await;
            match finalized {
                Ok(_deltas) => {}
                Err(error) => task_store.diagnostics().log(
                    "error",
                    "inspection_finalization_failed",
                    &error.correlation_id,
                    &error.summary,
                ),
            }
            manager.finish(&result_id).await;
        };
        if let Err(error) = backend
            .download_manager
            .spawn_tracked(TrackedTaskKind::Inspection, task)
        {
            return Err(admission_cleanup.finalize(error).await);
        }
        admission_cleanup.disarm();
        Ok(BeginOperationResult { operation_id })
    })
    .await
}

async fn finalize_inspection_result(
    store: &StateStore,
    operation_id: &str,
    result: Result<Option<models::UrlInspection>, AppError>,
) -> Result<Vec<models::StateDelta>, AppError> {
    let error = match result {
        Ok(Some(inspection)) => match store.complete_inspection(operation_id, inspection).await {
            Ok(deltas) => return Ok(deltas),
            Err(_) if store.operation_state(operation_id) == Some(OperationState::Cancelling) => {
                return store
                    .finalize_operation(operation_id, OperationState::Cancelled, None)
                    .await;
            }
            Err(error) => error,
        },
        Ok(None) => {
            return store
                .finalize_operation(operation_id, OperationState::Cancelled, None)
                .await
        }
        Err(error) => error,
    };
    // Rejected inspection metadata or resource limits still require a
    // terminal state before the registered worker can be released.
    store
        .finalize_operation(operation_id, OperationState::Failed, Some(error))
        .await
}

async fn bounded_inspection<F, T>(
    job: &downloader::process::DownloadJob,
    future: F,
    timeout: Duration,
) -> Result<T, AppError>
where
    F: Future<Output = Result<T, AppError>>,
{
    tokio::pin!(future);
    match tokio::time::timeout(timeout, &mut future).await {
        Ok(result) => result,
        Err(_) => {
            job.terminate_processes();
            // Retain the child owner long enough to observe exit and release
            // its pipes/permit, even if the outer inspection deadline expired.
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut future).await;
            Err(AppError::new(
                "inspection_timeout",
                "The complete inspection exceeded its 120-second deadline.",
            )
            .retryable(true))
        }
    }
}
async fn execute_inspection(
    store: &StateStore,
    manager: &DownloadManager,
    operation_id: &str,
    input: BeginInspectionInput,
    job: &downloader::process::DownloadJob,
) -> Result<Option<models::UrlInspection>, AppError> {
    if job.is_cancelled() {
        return Ok(None);
    }
    let Some(_permit) = manager
        .acquire_inspection(job)
        .await
        .map_err(AppError::internal)?
    else {
        return Ok(None);
    };
    if job.is_cancelled() {
        return Ok(None);
    }
    match store
        .set_operation_state(operation_id, OperationState::Running, None)
        .await
    {
        Ok(_deltas) => {}
        Err(_)
            if job.is_cancelled()
                || store.operation_state(operation_id) == Some(OperationState::Cancelling) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    }
    match downloader::inspection::inspect_url(
        &input.url,
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
        job,
    )
    .await
    {
        Ok(inspection) => Ok(Some(inspection)),
        Err(_) if job.is_cancelled() => Ok(None),
        Err(summary) => Err(AppError::new("inspection_failed", summary).retryable(true)),
    }
}
#[tauri::command]
async fn cancel_all_downloads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CancelAllResult, AppError> {
    cancellation::cancel_all(
        state.state_store.clone(),
        state.download_manager.clone(),
        Arc::new(move |progress| {
            if let Err(error) = app.emit("download-progress", progress) {
                record_event_delivery_failure(&app, "download-progress", &error.to_string());
            }
        }),
    )
    .await
}
#[tauri::command]
fn get_app_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, AppError> {
    state.state_store.snapshot()
}

#[tauri::command]
async fn add_inspection_result_to_queue(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    mut input: AddQueueItemInput,
) -> Result<QueueItemRecord, AppError> {
    let state = state.inner().clone();
    let coordinator = state.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        uuid::Uuid::parse_str(input.inspection_operation_id.trim())
            .map_err(|_| AppError::invalid("Invalid inspection operation ID."))?;
        let inspection = state
            .state_store
            .completed_inspection_video(&input.inspection_operation_id)?;
        input.output_dir = downloader::validate_output_directory(&input.output_dir)?;
        let request = DownloadRequest {
            url: inspection.url.clone(),
            quality: input.quality.clone(),
            format: input.format.clone(),
            output_dir: input.output_dir.clone(),
            cookie_config: input.cookie_config.clone(),
            filename_override: input.filename_override.clone(),
            compat_config_path: input.compat_config_path.clone(),
        };
        downloader::validate_download_request(&request).map_err(AppError::invalid)?;
        if !inspection.has_audio
            && matches!(
                input.format.as_str(),
                "mp3" | "flac" | "wav" | "aac" | "opus"
            )
        {
            return Err(AppError::invalid(
                "Audio-only output is unavailable because this item has no audio stream.",
            ));
        }
        let (item, _deltas) = state.state_store.add_queue_item(input).await?;
        Ok(item)
    })
    .await
}

#[tauri::command]
async fn update_queue_item(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    mut input: UpdateQueueItemInput,
) -> Result<(), AppError> {
    let state = state.inner().clone();
    let coordinator = state.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let current = state.state_store.queue_item(&item_id)?;
        let mut request = current.to_download_request();
        if let Some(format) = input.format.as_ref() {
            request.format.clone_from(format);
        }
        if let Some(quality) = input.quality.as_ref() {
            request.quality.clone_from(quality);
        }
        if let Some(output_dir) = input.output_dir.as_ref() {
            request.output_dir.clone_from(output_dir);
        }
        if let Some(filename_override) = input.filename_override.as_ref() {
            request.filename_override.clone_from(filename_override);
        }
        downloader::validate_download_request(&request).map_err(AppError::invalid)?;
        let canonical_output = downloader::validate_output_directory(&request.output_dir)?;
        request.output_dir.clone_from(&canonical_output);
        input.output_dir = Some(canonical_output);
        if !current.has_audio
            && matches!(
                request.format.as_str(),
                "mp3" | "flac" | "wav" | "aac" | "opus"
            )
        {
            return Err(AppError::invalid(
                "Audio-only output is unavailable because this item has no audio stream.",
            ));
        }
        let _deltas = state.state_store.update_queue_item(&item_id, input).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn remove_queue_items(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Result<(), AppError> {
    let state = state.inner().clone();
    let coordinator = state.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let _deltas = state.state_store.remove_queue_items(&item_ids).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn enqueue_queue_items(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
    priority: QueuePriority,
) -> Result<Vec<BeginOperationResult>, AppError> {
    let backend = state.inner().clone();
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let admission = backend
            .download_manager
            .begin_job_admission(item_ids.len())
            .await?;
        let (work, _deltas) = backend.state_store.enqueue(&item_ids, priority).await?;
        let ids: Vec<_> = work.iter().map(|item| item.operation_id.clone()).collect();
        if let Err(error) = admission.publish(&ids).await {
            cancel_unregistered_operations(&app, &backend.state_store, &ids).await;
            return Err(error);
        }
        Ok(ids
            .into_iter()
            .map(|operation_id| BeginOperationResult { operation_id })
            .collect())
    })
    .await
}

async fn cancel_unregistered_operations(
    _app: &tauri::AppHandle,
    store: &StateStore,
    ids: &[String],
) {
    for operation_id in ids {
        store.cancel_pending(operation_id).await;
        match store
            .finalize_operation(operation_id, OperationState::Cancelled, None)
            .await
        {
            Ok(_deltas) => {}
            Err(error) => store.diagnostics().log(
                "error",
                "admission_compensation_failed",
                &error.correlation_id,
                &error.summary,
            ),
        }
    }
}
#[tauri::command]
async fn cancel_operation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    uuid::Uuid::parse_str(operation_id.trim())
        .map_err(|_| AppError::invalid("Invalid operation ID."))?;
    let backend = state.inner().clone();
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Drain, async move {
        let kind = backend
            .state_store
            .operation_kind(&operation_id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if backend
            .state_store
            .operation_state(&operation_id)
            .is_some_and(|state| state.is_terminal())
        {
            return Ok(());
        }
        let deferred = if matches!(
            kind,
            OperationKind::RuntimeUpdate | OperationKind::AppUpdate
        ) {
            backend.download_manager.cancel_update(&operation_id)?
                == lifecycle::UpdateCancellation::DeferredPublication
        } else {
            match backend.download_manager.cancel(&operation_id).await {
                Ok(()) => {}
                Err(error)
                    if error.code == "not_found"
                        && backend
                            .state_store
                            .operation_state(&operation_id)
                            .is_some_and(|state| state.is_terminal()) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
            false
        };
        if !deferred {
            match backend
                .state_store
                .request_cancellation(&operation_id)
                .await
            {
                Ok(_delta) => {}
                Err(_)
                    if backend
                        .state_store
                        .operation_state(&operation_id)
                        .is_some_and(|state| state.is_terminal()) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
        }
        if kind == OperationKind::Download
            && backend.state_store.cancel_pending(&operation_id).await
        {
            finalize_download(
                &app,
                &backend.state_store,
                &operation_id,
                downloader::DownloadOutcome::Cancelled,
            )
            .await;
            backend.download_manager.finish(&operation_id).await;
            return Ok(());
        }
        backend
            .state_store
            .wait_for_terminal(&operation_id, CANCELLATION_WAIT_TIMEOUT)
            .await?;
        Ok(())
    })
    .await
}
#[tauri::command]
async fn dismiss_operation(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    let state = state.inner().clone();
    let coordinator = state.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let _deltas = state.state_store.dismiss_operation(&operation_id).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn check_downloader_runtime(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeStatus, AppError> {
    let state = state.inner().clone();
    let manager = state.download_manager.clone();
    let shutdown = manager.shutdown_token();
    run_tracked_command(&manager, TrackedTaskKind::Health, async move {
        let status = runtime::check_downloader_runtime_cancellable(shutdown.clone())
            .await
            .map_err(|summary| runtime_check_error("runtime_check_failed", summary, &shutdown))?;
        record_runtime_readiness(&app, &state.state_store, &status).await?;
        Ok(status)
    })
    .await
}

#[tauri::command]
async fn check_runtime_update(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeUpdateCheck, AppError> {
    let state = state.inner().clone();
    let manager = state.download_manager.clone();
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
        let _delta = state.state_store.set_runtime_readiness(readiness).await?;
        Ok(result)
    })
    .await
}

fn runtime_check_error(
    code: &str,
    summary: String,
    shutdown: &tokio_util::sync::CancellationToken,
) -> AppError {
    if shutdown.is_cancelled() {
        AppError::new("shutting_down", "The application is shutting down.")
    } else {
        AppError::new(code, summary).retryable(true)
    }
}

#[tauri::command]
async fn begin_runtime_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<BeginOperationResult, AppError> {
    let backend = state.inner().clone();
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let AdmittedUpdate {
            operation,
            lease,
            tracked,
            context,
        } = acquire_app_maintenance(&app, &backend, OperationKind::RuntimeUpdate).await?;
        let operation_id = operation.id;
        let result_id = operation_id.clone();
        let store = backend.state_store.clone();
        // `tracked` was registered before spawning and owns this task until
        // terminal persistence and maintenance cleanup have both completed.
        tauri::async_runtime::spawn(async move {
            let _tracked = tracked;
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
                runtime::update_downloader_runtime(app.clone(), context).await
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
                    if let Err(error) = record_runtime_readiness(&app, &store, &status).await {
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
        });
        Ok(BeginOperationResult { operation_id })
    })
    .await
}
#[tauri::command]
fn default_download_dir() -> Result<String, AppError> {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|dir| dir.join("Downloads")))
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| AppError::internal("Could not determine a default downloads folder."))
}

#[tauri::command]
fn validate_output_directory(path: String) -> Result<String, AppError> {
    downloader::validate_output_directory(&path).map(display_output_directory)
}

fn display_output_directory(path: String) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return rest.to_string();
        }
    }
    path
}

#[tauri::command]
async fn check_for_app_update(
    app: tauri::AppHandle,
) -> Result<models::UpdateCheckResult, AppError> {
    updater::check_for_app_update(&app)
        .await
        .map_err(|summary| AppError::new("app_update_check_failed", summary).retryable(true))
}

#[tauri::command]
async fn check_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::UpdateCheckResult, AppError> {
    let manager = state.download_manager.clone();
    let shutdown = manager.shutdown_token();
    run_tracked_command(&manager, TrackedTaskKind::Network, async move {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => Err(AppError::new("shutting_down", "The application is shutting down.")),
            result = check_for_app_update(app) => result,
        }
    }).await
}

#[tauri::command]
async fn begin_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    expected_version: String,
) -> Result<BeginOperationResult, AppError> {
    let backend = state.inner().clone();
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let AdmittedUpdate {
            operation,
            lease,
            tracked,
            context,
        } = acquire_app_maintenance(&app, &backend, OperationKind::AppUpdate).await?;
        let operation_id = operation.id;
        let result_id = operation_id.clone();
        let store = backend.state_store.clone();
        tauri::async_runtime::spawn(async move {
            let _tracked = tracked;
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
                let handoff = updater::prepare_app_update(&app, expected_version, &context).await?;
                context.check_cancelled()?;
                let _deltas = match store
                    .prepare_app_update_handoff(&result_id, handoff.expected_version())
                    .await
                {
                    Ok(_deltas) => _deltas,
                    Err(_) if context.is_cancelled() => return Err(UpdateRunError::Cancelled),
                    Err(error) => return Err(UpdateRunError::Failed(error)),
                };
                launch_verified_installer(&app, &handoff, &context)?;
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
                    drop(_tracked);
                    app.exit(0);
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
        });
        Ok(BeginOperationResult { operation_id })
    })
    .await
}
fn launch_verified_installer(
    app: &tauri::AppHandle,
    handoff: &updater::InstallerHandoff,
    context: &lifecycle::UpdateTaskContext,
) -> Result<(), UpdateRunError> {
    let publication = context.enter_publication(lifecycle::PublicationKind::InstallerHandoff)?;
    if let Err(error) = app.emit(
        "update-install-progress",
        &models::UpdateInstallProgress {
            status: "launching".into(),
            version: handoff.expected_version().to_string(),
            downloaded_bytes: handoff.installer_size(),
            total_bytes: Some(handoff.installer_size()),
            message: Some(format!(
                "Launching {}. Nuclear Downloader will close and reopen after install.",
                handoff.installer_name()
            )),
        },
    ) {
        record_event_delivery_failure(app, "update-install-progress", &error.to_string());
    }
    std::process::Command::new(handoff.installer_path())
        .args(["/S", "/R"])
        .spawn()
        .map_err(|error| {
            AppError::new(
                "installer_launch_failed",
                "The verified installer could not be started.",
            )
            .with_detail(error.to_string())
            .retryable(true)
        })?;
    publication.commit();
    Ok(())
}
#[tauri::command]
fn export_diagnostics(state: State<'_, AppState>, destination: String) -> Result<(), AppError> {
    state
        .state_store
        .diagnostics()
        .export_to(&PathBuf::from(destination))
}

#[tauri::command]
fn clear_diagnostics(state: State<'_, AppState>) -> Result<(), AppError> {
    state.state_store.diagnostics().clear()
}

#[cfg(windows)]
fn report_startup_failure(error: &AppError) {
    use std::os::windows::ffi::OsStrExt;
    const MB_ICONERROR: u32 = 0x10;
    const MB_OK: u32 = 0;
    #[link(name = "User32")]
    extern "system" {
        fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let text = std::ffi::OsStr::new(&format!(
        "{}\n\nThe existing application data was left unchanged.\nCorrelation ID: {}",
        error.summary, error.correlation_id
    ))
    .encode_wide()
    .chain(Some(0))
    .collect::<Vec<_>>();
    let caption = std::ffi::OsStr::new("Nuclear Downloader could not start")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn report_startup_failure(error: &AppError) {
    eprintln!(
        "Nuclear Downloader could not start: {} ({})",
        error.summary, error.correlation_id
    );
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_complete = Arc::new(AtomicBool::new(false));
    let startup_failure_reported = Arc::new(AtomicBool::new(false));
    let setup_failure_reported = startup_failure_reported.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // The single-instance plugin is deliberately registered before this
            // setup hook. Persistent state must not be opened until that plugin
            // has rejected any secondary process.
            let state_store = StateStore::open_default().map_err(|error| {
                report_startup_failure(&error);
                setup_failure_reported.store(true, Ordering::SeqCst);
                Box::<dyn std::error::Error>::from(error)
            })?;
            let download_manager = create_download_manager();
            let store = state_store.clone();
            app.manage(AppState {
                download_manager: download_manager.clone(),
                state_store,
            });
            state_events::spawn_state_events(app.handle(), &store, &download_manager)
                .map_err(Box::<dyn std::error::Error>::from)?;
            spawn_download_workers(app.handle(), &store, &download_manager)
                .map_err(Box::<dyn std::error::Error>::from)?;
            let startup_app = app.handle().clone();
            let startup_shutdown = download_manager.shutdown_token();
            let startup_report_store = store.clone();
            download_manager
                .spawn_startup(
                    async move {
                        let _delta = store.set_maintenance(true, false).await?;
                        runtime::recover_runtime_update_transaction().await.map_err(|summary| {
                            AppError::new("runtime_recovery_required", "The interrupted runtime update needs recovery before work can start.")
                                .with_detail(summary).retryable(true)
                        })?;
                        let _deltas = store.reconcile_pending_app_update(&startup_app.package_info().version.to_string()).await?;
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
                        if let Err(summary) = runtime::initialize_runtime_cache(startup_shutdown.clone()).await {
                            let error = runtime_check_error("runtime_initialization_failed", summary, &startup_shutdown);
                            if startup_shutdown.is_cancelled() { return Err(error); }
                            store.diagnostics().log("warning", "runtime_repair_required", &error.correlation_id, &error.summary);
                            store.set_runtime_readiness(RuntimeReadiness::RepairRequired).await?;
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
                .map_err(Box::<dyn std::error::Error>::from)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            begin_inspection,
            cancel_operation,
            dismiss_operation,
            cancel_all_downloads,
            get_app_snapshot,
            add_inspection_result_to_queue,
            update_queue_item,
            remove_queue_items,
            enqueue_queue_items,
            check_downloader_runtime,
            check_runtime_update,
            begin_runtime_update,
            default_download_dir,
            validate_output_directory,
            check_app_update,
            begin_app_update,
            export_diagnostics,
            clear_diagnostics,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            if !startup_failure_reported.swap(true, Ordering::SeqCst) {
                report_startup_failure(
                    &AppError::internal("The application shell could not be initialized.")
                        .with_detail(error.to_string()),
                );
            }
            return;
        }
    };

    app.run(move |app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if shutdown_complete.load(Ordering::SeqCst) {
                return;
            }
            api.prevent_exit();
            if shutdown_started.swap(true, Ordering::SeqCst) {
                return;
            }
            let Some(state) = app_handle.try_state::<AppState>() else {
                shutdown_complete.store(true, Ordering::SeqCst);
                app_handle.exit(code.unwrap_or(1));
                return;
            };
            let manager = state.download_manager.clone();
            let store = state.state_store.clone();
            let handle = app_handle.clone();
            let complete = shutdown_complete.clone();
            // This driver waits for tracked work; registering it as work would
            // make shutdown wait on itself.
            tauri::async_runtime::spawn(async move {
                let cleanup_store = store.clone();
                let cleanup_manager = manager.clone();
                let cleanup_app = handle.clone();
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
                                &cleanup_app,
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
                complete.store(true, Ordering::SeqCst);
                handle.exit(code.unwrap_or(0));
            });
        }
    });
}

#[cfg(test)]
mod public_boundary_tests {
    use super::display_output_directory;

    #[test]
    fn output_directory_hides_windows_verbatim_prefixes() {
        assert_eq!(
            display_output_directory(r"\\?\C:\Users\Example\Downloads".into()),
            r"C:\Users\Example\Downloads"
        );
        assert_eq!(
            display_output_directory(r"\\?\UNC\server\share\Downloads".into()),
            r"\\server\share\Downloads"
        );
        assert_eq!(
            display_output_directory(r"\\?\Volume{1234}\Downloads".into()),
            r"\\?\Volume{1234}\Downloads"
        );
    }

    #[test]
    fn legacy_state_bypassing_commands_are_not_registered() {
        let source = include_str!("lib.rs");
        let handler = source
            .split(".invoke_handler(tauri::generate_handler![")
            .nth(1)
            .and_then(|tail| tail.split("])").next())
            .expect("invoke handler command list");

        for legacy in [
            "inspect_url",
            "cancel_inspection",
            "start_download",
            "cancel_download",
            "update_downloader_runtime",
            "install_app_update",
            "check_for_app_update",
        ] {
            assert!(
                !handler
                    .lines()
                    .any(|line| line.trim() == format!("{legacy},")),
                "legacy command {legacy} was publicly registered"
            );
        }
    }

    #[test]
    fn persistent_state_opens_only_after_single_instance_registration() {
        let source = include_str!("lib.rs");
        let production = source
            .split("mod public_boundary_tests")
            .next()
            .expect("production source");
        let plugin = production
            .find(".plugin(tauri_plugin_single_instance::init")
            .expect("single-instance plugin registration");
        let setup = production
            .find(".setup(move |app|")
            .expect("application setup hook");
        let persistent_state = production
            .find("StateStore::open_default()")
            .expect("persistent state initialization");

        assert!(plugin < setup);
        assert!(setup < persistent_state);
    }

    #[test]
    fn download_worker_pool_is_started_once_during_setup() {
        let source = include_str!("lib.rs");
        let production = source
            .split("mod public_boundary_tests")
            .next()
            .expect("production source");
        assert_eq!(production.matches("spawn_download_workers(").count(), 1);
        assert!(include_str!("scheduling.rs").contains("for _ in 0..5"));
        let enqueue = production
            .split("fn enqueue_queue_items(")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("enqueue command body");
        assert!(!enqueue.contains("spawn_download_workers"));
    }
}
