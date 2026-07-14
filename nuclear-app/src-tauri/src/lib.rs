mod downloader;
mod models;
mod runtime;
mod updater;

use downloader::{create_download_manager, DownloadManager};
use futures_util::FutureExt;
use models::DownloadRequest;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

struct AppState {
    download_manager: DownloadManager,
}

#[tauri::command]
fn validate_url(url: &str) -> bool {
    downloader::is_allowed_download_url(url)
}

#[tauri::command]
async fn inspect_url(
    state: State<'_, AppState>,
    url: String,
    cookie_config: Option<models::CookieConfig>,
    compat_config_path: Option<String>,
) -> Result<models::UrlInspection, String> {
    let operation_id = format!("inspect-{}", uuid::Uuid::new_v4());
    let job = state.download_manager.register(&operation_id).await?;
    let result = downloader::inspect_url(
        &url,
        cookie_config.as_ref(),
        compat_config_path.as_deref(),
        &job,
    )
    .await;
    state.download_manager.finish(&operation_id).await;
    result
}

#[tauri::command]
async fn start_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    download_id: String,
    request: DownloadRequest,
) -> Result<(), String> {
    downloader::validate_download_request(&request)?;
    let manager = state.download_manager.clone();
    let job = manager.register(&download_id).await?;
    tauri::async_runtime::spawn(async move {
        let task_app = app.clone();
        let task_download_id = download_id.clone();
        let result = AssertUnwindSafe(downloader::start_download(
            app,
            download_id.clone(),
            request,
            manager.clone(),
            job,
        ))
        .catch_unwind()
        .await;
        if result.is_err() {
            downloader::emit_download_task_failure(&task_app, &task_download_id);
        }
        manager.finish(&download_id).await;
    });
    Ok(())
}

#[tauri::command]
async fn cancel_download(state: State<'_, AppState>, download_id: String) -> Result<(), String> {
    downloader::cancel_download(&download_id, state.download_manager.clone()).await
}

#[tauri::command]
async fn cancel_all_downloads(state: State<'_, AppState>) -> Result<(), String> {
    downloader::cancel_all_downloads(state.download_manager.clone()).await
}

#[tauri::command]
async fn check_downloader_runtime() -> Result<models::DownloaderRuntimeStatus, String> {
    Ok(runtime::check_downloader_runtime().await)
}

#[tauri::command]
async fn check_downloader_runtime_update() -> Result<models::DownloaderRuntimeUpdateCheck, String> {
    runtime::check_downloader_runtime_update().await
}

#[tauri::command]
async fn update_downloader_runtime(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<models::DownloaderRuntimeStatus, String> {
    state.download_manager.begin_maintenance().await?;
    let result = runtime::update_downloader_runtime(app).await;
    state.download_manager.end_maintenance().await;
    result
}

#[tauri::command]
fn default_download_dir() -> Result<String, String> {
    if let Some(path) =
        dirs::download_dir().or_else(|| dirs::home_dir().map(|dir| dir.join("Downloads")))
    {
        Ok(path.to_string_lossy().to_string())
    } else {
        Err("Could not determine a default downloads folder".into())
    }
}

#[tauri::command]
async fn check_for_app_update(app: tauri::AppHandle) -> Result<models::UpdateCheckResult, String> {
    updater::check_for_app_update(&app).await
}

#[tauri::command]
async fn install_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    expected_version: String,
) -> Result<(), String> {
    state.download_manager.begin_maintenance().await?;
    let result = updater::install_app_update(&app, expected_version).await;
    if result.is_err() {
        state.download_manager.end_maintenance().await;
    }
    result
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let download_manager = create_download_manager();
    let shutdown_manager = download_manager.clone();
    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_complete = Arc::new(AtomicBool::new(false));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { download_manager })
        .invoke_handler(tauri::generate_handler![
            validate_url,
            inspect_url,
            start_download,
            cancel_download,
            cancel_all_downloads,
            check_downloader_runtime,
            check_downloader_runtime_update,
            update_downloader_runtime,
            default_download_dir,
            check_for_app_update,
            install_app_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if shutdown_complete.load(Ordering::SeqCst) {
                return;
            }

            api.prevent_exit();
            if shutdown_started.swap(true, Ordering::SeqCst) {
                return;
            }

            let manager = shutdown_manager.clone();
            let handle = app_handle.clone();
            let complete = shutdown_complete.clone();
            tauri::async_runtime::spawn(async move {
                manager.begin_shutdown().await;
                let _ = manager.wait_for_idle(Duration::from_secs(10)).await;
                complete.store(true, Ordering::SeqCst);
                handle.exit(code.unwrap_or(0));
            });
        }
    });
}
