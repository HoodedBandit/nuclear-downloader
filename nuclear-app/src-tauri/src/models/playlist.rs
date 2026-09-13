use super::{CookieConfig, QueueItemRecord, VideoInfo};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

const MAX_MEDIA_SELECTION_FIELD_BYTES: usize = 4 * 1024;
const MAX_MEDIA_SELECTION_INDEX: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct MediaSelection {
    pub entry_id: String,
    pub extractor_key: String,
    pub playlist_index: u32,
}

impl MediaSelection {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("entry ID", self.entry_id.as_str()),
            ("extractor key", self.extractor_key.as_str()),
        ] {
            if value.is_empty() || value.trim() != value {
                return Err(format!(
                    "Media selection {name} must be non-empty and trimmed."
                ));
            }
            if value.len() > MAX_MEDIA_SELECTION_FIELD_BYTES {
                return Err(format!("Media selection {name} exceeds the 4 KiB limit."));
            }
            if value.chars().any(char::is_control) {
                return Err(format!(
                    "Media selection {name} contains control characters."
                ));
            }
        }
        if !(1..=MAX_MEDIA_SELECTION_INDEX).contains(&self.playlist_index) {
            return Err("Media selection playlist index must be between 1 and 1,000.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistEntry {
    pub id: String,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub url: String,
    pub thumbnail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub selection: Option<MediaSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub video: Option<Box<VideoInfo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistInfo {
    pub title: String,
    pub channel: Option<String>,
    pub entry_count: usize,
    pub truncated: bool,
    pub entries: Vec<PlaylistEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub inspection_settings_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum UrlInspection {
    Video { video: VideoInfo },
    Playlist { playlist: PlaylistInfo },
}

pub fn inspection_settings_fingerprint(
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};

    let canonical = serde_json::to_vec(&(cookie_config, compat_config_path))
        .expect("inspection settings contain only infallibly serializable values");
    format!("{:x}", Sha256::digest(canonical))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum QueuePreparation {
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistAdmissionReceipt {
    pub request_id: String,
    pub fingerprint: String,
    pub item_ids: Vec<String>,
    pub skipped_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistAdmissionInput {
    pub request_id: String,
    pub entry_indices: Vec<usize>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub playlist: Option<PlaylistAdmissionInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct PlaylistAdmissionResult {
    pub kind: PlaylistAdmissionKind,
    pub request_id: String,
    pub item_ids: Vec<String>,
    pub skipped_count: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum PlaylistAdmissionKind {
    Playlist,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub enum AddQueueItemResult {
    Single(Box<QueueItemRecord>),
    Playlist(PlaylistAdmissionResult),
}
