mod app_error;
mod artifact_contract;
#[cfg(test)]
mod backend_lifecycle_tests;
mod bootstrap;
mod bounded_read;
#[cfg(test)]
#[path = "../build_config.rs"]
mod build_config;
mod cancellation;
mod desktop;
mod diagnostics;
mod downloader;
mod journal;
mod lifecycle;
mod lifecycle_cleanup;
mod models;
mod notifications;
mod outbox;
#[cfg(test)]
mod performance_harness;
mod runtime;
mod runtime_transaction;
mod scheduling;
mod services;
#[cfg(test)]
mod soak_harness;
mod state;
mod state_events;
mod updater;
#[cfg(windows)]
mod windows_file;

use app_error::AppError;
use lifecycle::create_download_manager;
use models::{
    AddQueueItemInput, AppSnapshot, BeginInspectionInput, BeginOperationResult, CancelAllResult,
    QueueItemRecord, QueuePriority, UpdateQueueItemInput,
};
use services::Backend as AppState;
use state::StateStore;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Manager, State};

#[tauri::command]
async fn begin_inspection(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: BeginInspectionInput,
) -> Result<BeginOperationResult, AppError> {
    services::inspection::begin_inspection(state.inner().clone(), input).await
}

#[tauri::command]
async fn cancel_operation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    services::operations::cancel_operation(
        desktop::download_progress_sink(&app, &state.state_store),
        state.inner().clone(),
        operation_id,
    )
    .await
}

#[tauri::command]
async fn dismiss_operation(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), AppError> {
    services::operations::dismiss_operation(state.inner().clone(), operation_id).await
}

#[tauri::command]
async fn cancel_all_downloads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CancelAllResult, AppError> {
    services::operations::cancel_all_downloads(
        state.inner().clone(),
        desktop::download_progress_sink(&app, &state.state_store),
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
    input: AddQueueItemInput,
) -> Result<QueueItemRecord, AppError> {
    services::queue::add_inspection_result_to_queue(state.inner().clone(), input).await
}

#[tauri::command]
async fn update_queue_item(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    input: UpdateQueueItemInput,
) -> Result<(), AppError> {
    services::queue::update_queue_item(state.inner().clone(), item_id, input).await
}

#[tauri::command]
async fn remove_queue_items(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Result<(), AppError> {
    services::queue::remove_queue_items(state.inner().clone(), item_ids).await
}

#[tauri::command]
async fn enqueue_queue_items(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    item_ids: Vec<String>,
    priority: QueuePriority,
) -> Result<Vec<BeginOperationResult>, AppError> {
    services::queue::enqueue_queue_items(state.inner().clone(), item_ids, priority).await
}

#[tauri::command]
async fn check_downloader_runtime(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeStatus, AppError> {
    services::updates::check_downloader_runtime(state.inner().clone()).await
}

#[tauri::command]
async fn check_runtime_update(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeUpdateCheck, AppError> {
    services::updates::check_runtime_update(state.inner().clone()).await
}

#[tauri::command]
async fn begin_runtime_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<BeginOperationResult, AppError> {
    services::updates::begin_runtime_update(
        state.inner().clone(),
        desktop::runtime_progress_sink(&app, &state.state_store),
    )
    .await
}

#[tauri::command]
fn default_download_dir() -> Result<String, AppError> {
    services::queue::default_download_dir()
}

#[tauri::command]
fn validate_output_directory(path: String) -> Result<String, AppError> {
    services::queue::validate_output_directory(path)
}

#[tauri::command]
async fn check_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::UpdateCheckResult, AppError> {
    services::updates::check_app_update(
        state.inner().clone(),
        app.package_info().version.to_string(),
    )
    .await
}

#[tauri::command]
async fn begin_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    expected_version: String,
) -> Result<BeginOperationResult, AppError> {
    services::updates::begin_app_update(
        state.inner().clone(),
        app.package_info().version.to_string(),
        expected_version,
        desktop::update_progress_sink(&app, &state.state_store),
        desktop::installer_actions(&app),
    )
    .await
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
                desktop::report_startup_failure(&error);
                setup_failure_reported.store(true, Ordering::SeqCst);
                Box::<dyn std::error::Error>::from(error)
            })?;
            let download_manager = create_download_manager();
            let store = state_store.clone();
            let backend = AppState {
                download_manager: download_manager.clone(),
                state_store,
            };
            app.manage(backend.clone());
            desktop::spawn_state_events(app.handle(), &store, &download_manager)
                .map_err(Box::<dyn std::error::Error>::from)?;
            services::downloads::spawn_download_workers(
                desktop::download_progress_sink(app.handle(), &store),
                &store,
                &download_manager,
            )
            .map_err(Box::<dyn std::error::Error>::from)?;
            bootstrap::start(&backend, app.package_info().version.to_string())
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
                desktop::report_startup_failure(
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
            let backend = state.inner().clone();
            let publish_progress =
                desktop::download_progress_sink(app_handle, &backend.state_store);
            let handle = app_handle.clone();
            let complete = shutdown_complete.clone();
            tauri::async_runtime::spawn(async move {
                bootstrap::shutdown(backend, publish_progress).await;
                complete.store(true, Ordering::SeqCst);
                handle.exit(code.unwrap_or(0));
            });
        }
    });
}

#[cfg(test)]
mod public_boundary_tests;
