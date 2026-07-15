use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
    pub channel: Option<String>,
    pub thumbnail: Option<String>,
    pub url: String,
    pub available_qualities: Vec<String>,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieConfig {
    pub enabled: bool,
    pub mode: String, // "browser" or "file"
    pub browser: String,
    pub cookie_file: Option<String>, // path to cookies.txt
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub url: String,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistInfo {
    pub title: String,
    pub channel: Option<String>,
    pub entry_count: usize,
    pub truncated: bool,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UrlInspection {
    Video { video: VideoInfo },
    Playlist { playlist: PlaylistInfo },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub url: String,
    pub quality: String,
    pub format: String,
    pub output_dir: String,
    pub cookie_config: Option<CookieConfig>,
    pub filename_override: Option<String>,
    pub compat_config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub download_id: String,
    pub status: String,
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion_progress: Option<f64>,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloaderToolStatus {
    pub name: String,
    pub required: bool,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub source: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloaderRuntimeStatus {
    pub state: String,
    pub runtime_version: Option<String>,
    pub source: String,
    pub update_available: bool,
    pub latest_runtime_version: Option<String>,
    pub runtime_dir: Option<String>,
    pub plugin_dir: String,
    pub message: Option<String>,
    pub tools: Vec<DownloaderToolStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloaderRuntimeUpdateCheck {
    pub update_available: bool,
    pub latest_runtime_version: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloaderRuntimeUpdateProgress {
    pub status: String,
    pub version: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub has_update: bool,
    pub latest_version: Option<String>,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub installer_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstallProgress {
    pub status: String,
    pub version: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}
