mod app_error;
#[cfg(test)]
#[path = "../build_config.rs"]
mod build_config;
mod diagnostics;
mod downloader;
mod journal;
mod models;
mod runtime;
mod state;
mod updater;

use app_error::AppError;
use downloader::{
    create_download_manager, DownloadManager, MaintenanceLease as ManagerMaintenanceLease,
};
use futures_util::FutureExt;
use models::{
    AddQueueItemInput, AppSnapshot, BeginInspectionInput, BeginOperationResult, CancelAllResult,
    DownloadProgress, DownloadRequest, OperationKind, OperationState, QueueItemRecord,
    QueuePriority, RuntimeReadiness, UpdateQueueItemInput,
};
use state::StateStore;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, State};

const CANCELLATION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct AppState {
    download_manager: DownloadManager,
    state_store: StateStore,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
}

struct AppMaintenanceLeaseInner {
    manager_lease: ManagerMaintenanceLease,
    store: StateStore,
    app: tauri::AppHandle,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
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
}

impl Drop for AppMaintenanceLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            tauri::async_runtime::spawn(release_app_maintenance(inner));
        }
    }
}

async fn release_app_maintenance(inner: AppMaintenanceLeaseInner) {
    let _lifecycle = inner.lifecycle.lock().await;
    inner.manager_lease.release().await;
    if let Ok(Some(delta)) = inner.store.end_maintenance_operation(&inner.operation_id) {
        inner.store.emit(&inner.app, &delta);
    }
}

async fn acquire_app_maintenance(
    app: &tauri::AppHandle,
    state: &AppState,
    kind: OperationKind,
) -> Result<
    (
        models::OperationSnapshot,
        Vec<models::StateDelta>,
        AppMaintenanceLease,
    ),
    AppError,
> {
    let _lifecycle = state.lifecycle.lock().await;
    let manager_lease = state.download_manager.acquire_maintenance().await?;
    let (operation, deltas) = match state.state_store.begin_maintenance_operation(kind) {
        Ok(result) => result,
        Err(error) => {
            manager_lease.release().await;
            return Err(error);
        }
    };
    let lease = AppMaintenanceLease {
        inner: Some(AppMaintenanceLeaseInner {
            manager_lease,
            store: state.state_store.clone(),
            app: app.clone(),
            lifecycle: state.lifecycle.clone(),
            operation_id: operation.id.clone(),
        }),
    };
    Ok((operation, deltas, lease))
}

fn emit_deltas(app: &tauri::AppHandle, store: &StateStore, deltas: &[models::StateDelta]) {
    for delta in deltas {
        store.emit(app, delta);
    }
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

fn record_runtime_readiness(
    app: &tauri::AppHandle,
    store: &StateStore,
    status: &models::DownloaderRuntimeStatus,
) -> Result<(), AppError> {
    let delta = store.set_runtime_readiness(readiness_from_status(status))?;
    store.emit(app, &delta);
    Ok(())
}

pub(crate) fn record_download_progress(app: &tauri::AppHandle, progress: &DownloadProgress) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if let Some(deltas) = state
        .state_store
        .apply_download_progress(&progress.download_id, progress)
    {
        emit_deltas(app, &state.state_store, &deltas);
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

#[tauri::command]
async fn begin_inspection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: BeginInspectionInput,
) -> Result<BeginOperationResult, AppError> {
    let lifecycle = state.lifecycle.clone();
    let _lifecycle = lifecycle.lock().await;
    downloader::validate_fetch_request(
        &input.url,
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
    )
    .map_err(AppError::invalid)?;
    let (operation, delta) = state
        .state_store
        .begin_operation(OperationKind::Inspection, None)?;
    state.state_store.emit(&app, &delta);

    let operation_id = operation.id.clone();
    let manager = state.download_manager.clone();
    let store = state.state_store.clone();
    let result_id = operation_id.clone();
    tauri::async_runtime::spawn(async move {
        let job = match manager.register(&result_id).await {
            Ok(job) => job,
            Err(summary) => {
                let error = AppError::busy(summary);
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Failed, Some(error))
                {
                    emit_deltas(&app, &store, &deltas);
                }
                return;
            }
        };
        let permit = manager.acquire_inspection(&job).await;
        if store.operation_state(&result_id) == Some(OperationState::Cancelling) {
            let _ = manager.cancel(&result_id).await;
        }
        match permit {
            Ok(Some(_permit)) => {
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Running, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
                match downloader::inspect_url(
                    &input.url,
                    input.cookie_config.as_ref(),
                    input.compat_config_path.as_deref(),
                    &job,
                )
                .await
                {
                    Ok(inspection) => {
                        if let Ok(deltas) = store.complete_inspection(&result_id, inspection) {
                            emit_deltas(&app, &store, &deltas);
                        }
                    }
                    Err(_) if job.is_cancelled() => {
                        if let Ok(deltas) =
                            store.set_operation_state(&result_id, OperationState::Cancelled, None)
                        {
                            emit_deltas(&app, &store, &deltas);
                        }
                    }
                    Err(summary) => {
                        let error = AppError::new("inspection_failed", summary).retryable(true);
                        if let Ok(deltas) = store.set_operation_state(
                            &result_id,
                            OperationState::Failed,
                            Some(error),
                        ) {
                            emit_deltas(&app, &store, &deltas);
                        }
                    }
                }
            }
            Ok(None) => {
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Cancelled, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
            Err(summary) => {
                let error = AppError::internal(summary);
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Failed, Some(error))
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
        }
        manager.finish(&result_id).await;
    });
    Ok(BeginOperationResult { operation_id })
}

#[tauri::command]
async fn cancel_all_downloads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CancelAllResult, AppError> {
    let lifecycle = state.lifecycle.clone();
    let _lifecycle = lifecycle.lock().await;
    state.download_manager.begin_cancel_all().await?;
    let delta = state.state_store.set_maintenance(true, true)?;
    state.state_store.emit(&app, &delta);
    for operation_id in state.state_store.pending_operation_ids() {
        state.state_store.cancel_pending(&operation_id);
        if let Ok(deltas) =
            state
                .state_store
                .set_operation_state(&operation_id, OperationState::Cancelled, None)
        {
            emit_deltas(&app, &state.state_store, &deltas);
        }
    }

    let wait = state
        .download_manager
        .finish_cancel_all(Duration::from_secs(10))
        .await;
    let remaining = state.download_manager.active_ids().await;
    if wait.is_ok() {
        let delta = state.state_store.set_maintenance(false, false)?;
        state.state_store.emit(&app, &delta);
        Ok(CancelAllResult {
            idle: true,
            remaining_operation_ids: Vec::new(),
        })
    } else {
        Ok(CancelAllResult {
            idle: false,
            remaining_operation_ids: remaining,
        })
    }
}

#[tauri::command]
fn get_app_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, AppError> {
    state.state_store.snapshot()
}

#[tauri::command]
fn add_inspection_result_to_queue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    mut input: AddQueueItemInput,
) -> Result<QueueItemRecord, AppError> {
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
    let (item, deltas) = state.state_store.add_queue_item(input)?;
    emit_deltas(&app, &state.state_store, &deltas);
    Ok(item)
}

#[tauri::command]
fn update_queue_item(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    mut input: UpdateQueueItemInput,
) -> Result<(), AppError> {
    let snapshot = state.state_store.snapshot()?;
    let current = snapshot
        .queue
        .into_iter()
        .find(|item| item.id == item_id)
        .ok_or_else(|| AppError::not_found("queue item"))?;
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
    let delta = state.state_store.update_queue_item(&item_id, input)?;
    state.state_store.emit(&app, &delta);
    Ok(())
}

#[tauri::command]
fn remove_queue_items(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Result<(), AppError> {
    let deltas = state.state_store.remove_queue_items(&item_ids)?;
    emit_deltas(&app, &state.state_store, &deltas);
    Ok(())
}

fn spawn_download_workers(app: &tauri::AppHandle, store: &StateStore, manager: &DownloadManager) {
    for _ in 0..5 {
        let app = app.clone();
        let store = store.clone();
        let manager = manager.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let _slot = match manager.acquire_download_slot().await {
                    Ok(permit) => permit,
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
                let work = store.wait_next_pending().await;
                let job = match manager.register(&work.operation_id).await {
                    Ok(job) => job,
                    Err(summary) => {
                        let error = AppError::busy(summary);
                        if let Ok(deltas) = store.set_operation_state(
                            &work.operation_id,
                            OperationState::Failed,
                            Some(error),
                        ) {
                            emit_deltas(&app, &store, &deltas);
                        }
                        continue;
                    }
                };
                if store.operation_state(&work.operation_id) == Some(OperationState::Cancelling) {
                    let _ = manager.cancel(&work.operation_id).await;
                    if let Ok(deltas) = store.set_operation_state(
                        &work.operation_id,
                        OperationState::Cancelled,
                        None,
                    ) {
                        emit_deltas(&app, &store, &deltas);
                    }
                } else {
                    match store.set_operation_state(
                        &work.operation_id,
                        OperationState::Running,
                        None,
                    ) {
                        Ok(deltas) => {
                            emit_deltas(&app, &store, &deltas);
                            let task_app = app.clone();
                            let task_operation_id = work.operation_id.clone();
                            let result = AssertUnwindSafe(downloader::start_download(
                                app.clone(),
                                work.operation_id.clone(),
                                work.queue_item.to_download_request(),
                                manager.clone(),
                                job,
                            ))
                            .catch_unwind()
                            .await;
                            if result.is_err() {
                                downloader::emit_download_task_failure(
                                    &task_app,
                                    &task_operation_id,
                                );
                            }
                        }
                        Err(_)
                            if store.operation_state(&work.operation_id)
                                == Some(OperationState::Cancelling) =>
                        {
                            let _ = manager.cancel(&work.operation_id).await;
                            if let Ok(deltas) = store.set_operation_state(
                                &work.operation_id,
                                OperationState::Cancelled,
                                None,
                            ) {
                                emit_deltas(&app, &store, &deltas);
                            }
                        }
                        Err(error) => {
                            if let Ok(deltas) = store.set_operation_state(
                                &work.operation_id,
                                OperationState::Failed,
                                Some(error),
                            ) {
                                emit_deltas(&app, &store, &deltas);
                            }
                        }
                    }
                }
                manager.finish(&work.operation_id).await;
            }
        });
    }
}

#[tauri::command]
async fn enqueue_queue_items(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
    priority: QueuePriority,
) -> Result<Vec<BeginOperationResult>, AppError> {
    let lifecycle = state.lifecycle.clone();
    let _lifecycle = lifecycle.lock().await;
    let (work, deltas) = state.state_store.enqueue(&item_ids, priority)?;
    emit_deltas(&app, &state.state_store, &deltas);
    let results = work
        .iter()
        .map(|item| BeginOperationResult {
            operation_id: item.operation_id.clone(),
        })
        .collect();
    Ok(results)
}

#[tauri::command]
async fn cancel_operation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    uuid::Uuid::parse_str(operation_id.trim())
        .map_err(|_| AppError::invalid("Invalid operation ID."))?;
    let operation_kind = state
        .state_store
        .operation_kind(&operation_id)
        .ok_or_else(|| AppError::not_found("operation"))?;
    if operation_kind == OperationKind::AppUpdate {
        return Err(AppError::new(
            "not_cancellable",
            "An application update cannot be cancelled after installation begins.",
        ));
    }
    let delta = state.state_store.request_cancellation(&operation_id)?;
    state.state_store.emit(&app, &delta);
    if operation_kind == OperationKind::RuntimeUpdate {
        let _ = runtime::request_runtime_update_cancel();
        state
            .state_store
            .wait_for_terminal(&operation_id, CANCELLATION_WAIT_TIMEOUT)
            .await?;
        return Ok(());
    }
    if state.state_store.cancel_pending(&operation_id) {
        let deltas = state.state_store.set_operation_state(
            &operation_id,
            OperationState::Cancelled,
            None,
        )?;
        emit_deltas(&app, &state.state_store, &deltas);
        return Ok(());
    }
    match state.download_manager.cancel(&operation_id).await {
        Ok(()) => {}
        Err(error) if error.code == "not_found" => {}
        Err(error) => return Err(error),
    }
    state
        .state_store
        .wait_for_terminal(&operation_id, CANCELLATION_WAIT_TIMEOUT)
        .await?;
    Ok(())
}

#[tauri::command]
fn dismiss_operation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    let delta = state.state_store.dismiss_operation(&operation_id)?;
    state.state_store.emit(&app, &delta);
    Ok(())
}

#[tauri::command]
async fn check_downloader_runtime(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeStatus, AppError> {
    let status = runtime::check_downloader_runtime().await;
    record_runtime_readiness(&app, &state.state_store, &status)?;
    Ok(status)
}

#[tauri::command]
async fn check_runtime_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeUpdateCheck, AppError> {
    let result = runtime::check_downloader_runtime_update()
        .await
        .map_err(|summary| AppError::new("runtime_update_check_failed", summary).retryable(true))?;
    let readiness = if result.update_available {
        RuntimeReadiness::UpdateAvailable
    } else {
        readiness_from_status(&runtime::check_downloader_runtime().await)
    };
    let delta = state.state_store.set_runtime_readiness(readiness)?;
    state.state_store.emit(&app, &delta);
    Ok(result)
}

#[tauri::command]
async fn begin_runtime_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<BeginOperationResult, AppError> {
    let (operation, deltas, lease) =
        acquire_app_maintenance(&app, &state, OperationKind::RuntimeUpdate).await?;
    emit_deltas(&app, &state.state_store, &deltas);

    let operation_id = operation.id.clone();
    let result_id = operation_id.clone();
    let store = state.state_store.clone();
    tauri::async_runtime::spawn(async move {
        let deltas = match store.set_operation_state(&result_id, OperationState::Running, None) {
            Ok(deltas) => deltas,
            Err(_) if store.operation_state(&result_id) == Some(OperationState::Cancelling) => {
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Cancelled, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
                lease.release().await;
                return;
            }
            Err(_) => {
                lease.release().await;
                return;
            }
        };
        emit_deltas(&app, &store, &deltas);
        let result = runtime::update_downloader_runtime(app.clone()).await;
        match result {
            Ok(status) => {
                let _ = record_runtime_readiness(&app, &store, &status);
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Completed, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
            Err(summary) if summary.to_ascii_lowercase().contains("cancel") => {
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Cancelled, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
            Err(summary) => {
                let error = AppError::new("runtime_update_failed", summary).retryable(true);
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Failed, Some(error))
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
        }
        lease.release().await;
    });
    Ok(BeginOperationResult { operation_id })
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
async fn check_app_update(app: tauri::AppHandle) -> Result<models::UpdateCheckResult, AppError> {
    check_for_app_update(app).await
}

#[tauri::command]
async fn begin_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    expected_version: String,
) -> Result<BeginOperationResult, AppError> {
    let (operation, deltas, lease) =
        acquire_app_maintenance(&app, &state, OperationKind::AppUpdate).await?;
    emit_deltas(&app, &state.state_store, &deltas);
    let operation_id = operation.id.clone();
    let result_id = operation_id.clone();
    let store = state.state_store.clone();
    tauri::async_runtime::spawn(async move {
        let deltas = match store.set_operation_state(&result_id, OperationState::Running, None) {
            Ok(deltas) => deltas,
            Err(_) => {
                lease.release().await;
                return;
            }
        };
        emit_deltas(&app, &store, &deltas);
        match updater::install_app_update(&app, expected_version).await {
            Ok(()) => {
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Completed, None)
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
            Err(summary) => {
                let error = AppError::new("app_update_failed", summary).retryable(true);
                if let Ok(deltas) =
                    store.set_operation_state(&result_id, OperationState::Failed, Some(error))
                {
                    emit_deltas(&app, &store, &deltas);
                }
            }
        }
        lease.release().await;
    });
    Ok(BeginOperationResult { operation_id })
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
            spawn_download_workers(app.handle(), &store, &download_manager);
            app.manage(AppState {
                download_manager,
                state_store,
                lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            });
            let output_roots = store.journaled_output_roots();
            tauri::async_runtime::spawn(async move {
                let cleanup = tauri::async_runtime::spawn_blocking(move || {
                    downloader::cleanup_abandoned_download_stages(&output_roots)
                })
                .await;
                match cleanup {
                    Ok(failures) => {
                        for failure in failures {
                            store.diagnostics().log(
                                "error",
                                "download_stage_cleanup_failed",
                                &uuid::Uuid::new_v4().to_string(),
                                &failure,
                            );
                        }
                    }
                    Err(_) => store.diagnostics().log(
                        "error",
                        "download_stage_cleanup_failed",
                        &uuid::Uuid::new_v4().to_string(),
                        "The abandoned download-stage cleanup worker stopped unexpectedly.",
                    ),
                }
                if let Err(summary) = runtime::cleanup_abandoned_runtime_updates().await {
                    store.diagnostics().log(
                        "error",
                        "runtime_cleanup_failed",
                        &uuid::Uuid::new_v4().to_string(),
                        &summary,
                    );
                }
            });
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
            let lifecycle = state.lifecycle.clone();
            let handle = app_handle.clone();
            let complete = shutdown_complete.clone();
            tauri::async_runtime::spawn(async move {
                let _lifecycle = lifecycle.lock().await;
                if let Ok(delta) = store.set_maintenance(true, true) {
                    store.emit(&handle, &delta);
                }
                manager.begin_shutdown().await;
                let _ = manager.wait_for_idle(Duration::from_secs(10)).await;
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
        assert_eq!(production.matches("spawn_download_workers(").count(), 2);
        let enqueue = production
            .split("fn enqueue_queue_items(")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("enqueue command body");
        assert!(!enqueue.contains("spawn_download_workers"));
    }
}
