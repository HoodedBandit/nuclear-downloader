mod downloader;
mod models;

use downloader::{ActiveDownloads, create_active_downloads};
use models::DownloadRequest;
use tauri::State;

struct AppState {
    active_downloads: ActiveDownloads,
}

#[tauri::command]
fn validate_url(url: &str) -> bool {
    downloader::is_allowed_download_url(url)
}

#[tauri::command]
async fn fetch_video_info(url: String, cookie_config: Option<models::CookieConfig>) -> Result<models::VideoInfo, String> {
    downloader::validate_fetch_request(&url, cookie_config.as_ref())?;
    downloader::fetch_info(&url, cookie_config.as_ref()).await
}

#[tauri::command]
async fn fetch_playlist_info(url: String, cookie_config: Option<models::CookieConfig>) -> Result<models::PlaylistInfo, String> {
    downloader::validate_fetch_request(&url, cookie_config.as_ref())?;
    downloader::fetch_playlist(&url, cookie_config.as_ref()).await
}

#[tauri::command]
async fn start_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    download_id: String,
    request: DownloadRequest,
) -> Result<(), String> {
    downloader::validate_download_request(&request)?;
    let active = state.active_downloads.clone();
    tauri::async_runtime::spawn(async move {
        downloader::start_download(app, download_id, request, active).await;
    });
    Ok(())
}

#[tauri::command]
async fn cancel_download(
    state: State<'_, AppState>,
    download_id: String,
) -> Result<(), String> {
    downloader::cancel_download(&download_id, state.active_downloads.clone()).await
}

#[tauri::command]
async fn check_ytdlp() -> Result<String, String> {
    let bin = downloader::resolve_bin("yt-dlp");
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("--version");
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);

    let output = cmd
        .output()
        .await
        .map_err(|_| "yt-dlp not found.".to_string())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("yt-dlp not working properly".into())
    }
}

#[tauri::command]
async fn check_ffmpeg() -> Result<bool, String> {
    Ok(downloader::ffmpeg_available())
}

#[tauri::command]
fn default_download_dir() -> Result<String, String> {
    if let Some(path) = dirs::download_dir().or_else(|| dirs::home_dir().map(|dir| dir.join("Downloads"))) {
        Ok(path.to_string_lossy().to_string())
    } else {
        Err("Could not determine a default downloads folder".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let active_downloads = create_active_downloads();
    let cleanup_downloads = active_downloads.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            active_downloads,
        })
        .invoke_handler(tauri::generate_handler![
            validate_url,
            fetch_video_info,
            fetch_playlist_info,
            start_download,
            cancel_download,
            check_ytdlp,
            check_ffmpeg,
            default_download_dir,
        ])
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let downloads = cleanup_downloads.clone();
                tauri::async_runtime::spawn(async move {
                    let children = {
                        let mut map = downloads.lock().await;
                        map.drain().map(|(_, child)| child).collect::<Vec<_>>()
                    };

                    for mut child in children {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
