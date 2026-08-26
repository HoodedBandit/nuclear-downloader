use serde::{Deserialize, Serialize};

use crate::app_error::AppError;
use serde::Deserializer;
use ts_rs::TS;

pub const APP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct CookieConfig {
    pub enabled: bool,
    pub mode: String, // "browser" or "file"
    pub browser: String,
    pub cookie_file: Option<String>, // path to cookies.txt
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistEntry {
    pub id: String,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub url: String,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistInfo {
    pub title: String,
    pub channel: Option<String>,
    pub entry_count: usize,
    pub truncated: bool,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum UrlInspection {
    Video { video: VideoInfo },
    Playlist { playlist: PlaylistInfo },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloadRequest {
    pub url: String,
    pub quality: String,
    pub format: String,
    pub output_dir: String,
    #[ts(optional = nullable)]
    pub cookie_config: Option<CookieConfig>,
    #[ts(optional = nullable)]
    pub filename_override: Option<String>,
    #[ts(optional = nullable)]
    pub compat_config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloadProgress {
    pub download_id: String,
    pub status: String,
    pub progress: f64,
    pub phase: Option<String>,
    pub download_progress: Option<f64>,
    pub conversion_progress: Option<f64>,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloaderToolStatus {
    pub name: String,
    pub required: bool,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub source: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum DownloaderRuntimeState {
    Ready,
    ReadyWithWarnings,
    RepairRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloaderRuntimeStatus {
    pub state: DownloaderRuntimeState,
    pub runtime_version: Option<String>,
    pub source: String,
    pub update_available: bool,
    pub latest_runtime_version: Option<String>,
    pub runtime_dir: Option<String>,
    pub plugin_dir: String,
    pub message: Option<String>,
    pub tools: Vec<DownloaderToolStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloaderRuntimeUpdateCheck {
    pub update_available: bool,
    pub latest_runtime_version: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct DownloaderRuntimeUpdateProgress {
    pub status: String,
    pub version: Option<String>,
    #[ts(type = "number")]
    pub downloaded_bytes: u64,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub has_update: bool,
    pub latest_version: Option<String>,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub installer_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct UpdateInstallProgress {
    pub status: String,
    pub version: String,
    #[ts(type = "number")]
    pub downloaded_bytes: u64,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum QueueItemState {
    Inert,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl QueueItemState {
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Inert | Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum OperationKind {
    Inspection,
    Download,
    Conversion,
    AppUpdate,
    RuntimeUpdate,
    DiagnosticsExport,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum OperationState {
    Queued,
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum RuntimeReadiness {
    Ready,
    ReadyWithWarnings,
    RepairRequired,
    UpdateAvailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum QueuePriority {
    Normal,
    Front,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
/// Persisted queue configuration. `cookie_config` is an immutable per-item
/// authentication selection containing metadata only; cookie contents are never stored.
pub struct QueueItemRecord {
    pub schema_version: u32,
    pub id: String,
    pub source_url: String,
    pub title: String,
    pub available_qualities: Vec<String>,
    pub has_audio: bool,
    pub cookie_config: Option<CookieConfig>,
    pub format: String,
    pub quality: String,
    pub output_dir: String,
    pub filename_override: Option<String>,
    pub compat_config_path: Option<String>,
    pub state: QueueItemState,
    pub latest_operation_id: Option<String>,
    #[ts(type = "number")]
    pub created_at_ms: u64,
    #[ts(type = "number")]
    pub updated_at_ms: u64,
}

impl QueueItemRecord {
    pub fn to_download_request(&self) -> DownloadRequest {
        DownloadRequest {
            url: self.source_url.clone(),
            quality: self.quality.clone(),
            format: self.format.clone(),
            output_dir: self.output_dir.clone(),
            cookie_config: self.cookie_config.clone(),
            filename_override: self.filename_override.clone(),
            compat_config_path: self.compat_config_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct OperationSnapshot {
    pub schema_version: u32,
    pub id: String,
    pub queue_item_id: Option<String>,
    pub kind: OperationKind,
    pub state: OperationState,
    pub progress: f64,
    pub phase: Option<String>,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub created_at_ms: u64,
    #[ts(type = "number")]
    pub updated_at_ms: u64,
    #[ts(type = "number | null")]
    pub finished_at_ms: Option<u64>,
    pub error: Option<AppError>,
    pub inspection_result: Option<UrlInspection>,
    pub correlation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct AppSnapshot {
    pub schema_version: u32,
    pub queue: Vec<QueueItemRecord>,
    pub operations: Vec<OperationSnapshot>,
    pub runtime_readiness: RuntimeReadiness,
    pub maintenance_active: bool,
    pub draining: bool,
    #[ts(type = "number")]
    pub latest_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum StateDeltaValue {
    QueueItemUpserted(QueueItemRecord),
    QueueItemsRemoved(Vec<String>),
    OperationUpserted(OperationSnapshot),
    OperationRemoved(String),
    RuntimeReadinessChanged(RuntimeReadiness),
    MaintenanceChanged { active: bool, draining: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct StateDelta {
    pub schema_version: u32,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub emitted_at_ms: u64,
    #[serde(flatten)]
    pub delta: StateDeltaValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct AddQueueItemInput {
    pub inspection_operation_id: String,
    pub format: String,
    pub quality: String,
    pub output_dir: String,
    #[ts(optional = nullable)]
    pub cookie_config: Option<CookieConfig>,
    #[ts(optional = nullable)]
    pub filename_override: Option<String>,
    #[ts(optional = nullable)]
    pub compat_config_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct UpdateQueueItemInput {
    #[ts(optional = nullable)]
    pub format: Option<String>,
    #[ts(optional = nullable)]
    pub quality: Option<String>,
    #[ts(optional = nullable)]
    pub output_dir: Option<String>,
    #[ts(optional, type = "string | null")]
    pub filename_override: Option<Option<String>>,
}

impl<'de> Deserialize<'de> for UpdateQueueItemInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireInput {
            format: Option<String>,
            quality: Option<String>,
            output_dir: Option<String>,
            #[serde(default, deserialize_with = "deserialize_double_option")]
            filename_override: Option<Option<String>>,
        }

        let wire = WireInput::deserialize(deserializer)?;
        Ok(Self {
            format: wire.format,
            quality: wire.quality,
            output_dir: wire.output_dir,
            filename_override: wire.filename_override,
        })
    }
}

fn deserialize_double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct BeginInspectionInput {
    pub url: String,
    #[ts(optional = nullable)]
    pub cookie_config: Option<CookieConfig>,
    #[ts(optional = nullable)]
    pub compat_config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct BeginOperationResult {
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct CancelAllResult {
    pub idle: bool,
    pub remaining_operation_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_rs::{Config, TS};

    #[test]
    fn update_filename_distinguishes_missing_null_and_value() {
        let missing = serde_json::from_str::<UpdateQueueItemInput>("{}").unwrap();
        let cleared =
            serde_json::from_str::<UpdateQueueItemInput>(r#"{"filenameOverride":null}"#).unwrap();
        let value =
            serde_json::from_str::<UpdateQueueItemInput>(r#"{"filenameOverride":"renamed"}"#)
                .unwrap();

        assert_eq!(missing.filename_override, None);
        assert_eq!(cleared.filename_override, Some(None));
        assert_eq!(value.filename_override, Some(Some("renamed".to_string())));
    }

    #[test]
    fn serialized_optional_output_fields_are_present_as_null() {
        let progress = DownloadProgress {
            download_id: "operation".to_string(),
            status: "queued".to_string(),
            progress: 0.0,
            phase: None,
            download_progress: None,
            conversion_progress: None,
            speed: None,
            eta: None,
            error: None,
            error_code: None,
            error_detail: None,
            filename: None,
        };
        let value = serde_json::to_value(progress).unwrap();
        for key in [
            "phase",
            "download_progress",
            "conversion_progress",
            "speed",
            "eta",
            "error",
            "error_code",
            "error_detail",
            "filename",
        ] {
            assert!(value.get(key).is_some_and(serde_json::Value::is_null));
        }

        let binding = UpdateQueueItemInput::export_to_string(&Config::default()).unwrap();
        assert!(binding.contains("filenameOverride?: string | null"));
        assert!(!binding.contains("null | null"));
    }

    fn assert_committed_binding<T: TS + 'static>() {
        let generated = T::export_to_string(&Config::default()).unwrap();
        let relative = T::output_path().expect("exported binding path");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(relative);
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
        assert_eq!(committed, generated, "binding drifted: {}", path.display());
    }

    #[test]
    fn committed_typescript_bindings_match_every_public_contract() {
        assert_committed_binding::<crate::app_error::AppError>();
        assert_committed_binding::<VideoInfo>();
        assert_committed_binding::<CookieConfig>();
        assert_committed_binding::<PlaylistEntry>();
        assert_committed_binding::<PlaylistInfo>();
        assert_committed_binding::<UrlInspection>();
        assert_committed_binding::<DownloadRequest>();
        assert_committed_binding::<DownloadProgress>();
        assert_committed_binding::<DownloaderToolStatus>();
        assert_committed_binding::<DownloaderRuntimeStatus>();
        assert_committed_binding::<DownloaderRuntimeState>();
        assert_committed_binding::<DownloaderRuntimeUpdateCheck>();
        assert_committed_binding::<DownloaderRuntimeUpdateProgress>();
        assert_committed_binding::<UpdateCheckResult>();
        assert_committed_binding::<UpdateInstallProgress>();
        assert_committed_binding::<QueueItemState>();
        assert_committed_binding::<OperationKind>();
        assert_committed_binding::<OperationState>();
        assert_committed_binding::<RuntimeReadiness>();
        assert_committed_binding::<QueuePriority>();
        assert_committed_binding::<QueueItemRecord>();
        assert_committed_binding::<OperationSnapshot>();
        assert_committed_binding::<AppSnapshot>();
        assert_committed_binding::<StateDeltaValue>();
        assert_committed_binding::<StateDelta>();
        assert_committed_binding::<AddQueueItemInput>();
        assert_committed_binding::<UpdateQueueItemInput>();
        assert_committed_binding::<BeginInspectionInput>();
        assert_committed_binding::<BeginOperationResult>();
        assert_committed_binding::<CancelAllResult>();
    }
}
