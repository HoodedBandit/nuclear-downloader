use super::{MAX_INSPECTION_OUTPUT_BYTES, MAX_PLAYLIST_ENTRIES};
use crate::downloader::validation::is_allowed_download_url;
use crate::models::{MediaSelection, PlaylistEntry, PlaylistInfo, UrlInspection, VideoInfo};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

#[derive(Debug, Deserialize)]
struct ThumbnailRecord {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistRecord {
    #[serde(rename = "_type")]
    kind: Option<String>,
    id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    url: Option<String>,
    webpage_url: Option<String>,
    original_url: Option<String>,
    extractor_key: Option<String>,
    ie_key: Option<String>,
    playlist_index: Option<u32>,
    thumbnail: Option<String>,
    thumbnails: Option<Vec<ThumbnailRecord>>,
}

fn thumbnail(raw: Option<&str>) -> Option<String> {
    raw.and_then(|value| {
        Url::parse(value)
            .ok()
            .filter(|url| url.scheme() == "https")
            .map(|_| value.to_string())
    })
}

fn is_playlist(data: &Value) -> bool {
    matches!(data["_type"].as_str(), Some("playlist" | "multi_video"))
}

fn same_url(left: &str, right: &str) -> bool {
    match (Url::parse(left), Url::parse(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

impl PlaylistRecord {
    fn into_entry(
        self,
        parent_url: &str,
        has_formats: bool,
    ) -> Result<Option<PlaylistEntry>, String> {
        let thumbnail = thumbnail(
            self.thumbnails
                .as_ref()
                .and_then(|values| values.iter().rev().find_map(|value| value.url.as_deref()))
                .or(self.thumbnail.as_deref()),
        );
        let id = self.id.filter(|id| !id.trim().is_empty());
        let extractor = self.extractor_key.or(self.ie_key);
        let webpage = self
            .webpage_url
            .filter(|value| is_allowed_download_url(value))
            .or_else(|| {
                self.original_url
                    .filter(|value| is_allowed_download_url(value))
            });
        let resolved_video =
            self.kind.as_deref() == Some("video") || (self.kind.is_none() && has_formats);
        // Embedded videos may share a post URL. Replaying the parent needs both
        // an ordinal and a verified identity, not a guessed /video/N URL or an
        // expiring direct media URL.
        let needs_selection = resolved_video
            && webpage
                .as_deref()
                .is_none_or(|value| same_url(value, parent_url));
        let (url, selection) = if needs_selection {
            let selection = MediaSelection {
                entry_id: id
                    .clone()
                    .ok_or("An embedded playlist video has no media identity.")?,
                extractor_key: extractor
                    .clone()
                    .ok_or("An embedded playlist video has no extractor identity.")?,
                playlist_index: self
                    .playlist_index
                    .ok_or("An embedded playlist video has no selection index.")?,
            };
            selection.validate()?;
            (parent_url.to_string(), Some(selection))
        } else {
            let is_youtube = extractor
                .as_deref()
                .is_some_and(|key| key.to_ascii_lowercase().contains("youtube"));
            let Some(url) = webpage
                .filter(|value| !same_url(value, parent_url))
                .or_else(|| {
                    self.url.filter(|value| {
                        is_allowed_download_url(value) && !same_url(value, parent_url)
                    })
                })
                .or_else(|| {
                    (is_youtube && id.is_some()).then(|| {
                        format!(
                            "https://www.youtube.com/watch?v={}",
                            id.as_deref().unwrap_or_default()
                        )
                    })
                })
            else {
                return Ok(None);
            };
            (url, None)
        };
        Ok(Some(PlaylistEntry {
            id: id.unwrap_or_else(|| url.clone()),
            title: self.title,
            duration: self.duration,
            url,
            thumbnail,
            selection,
        }))
    }
}

fn video_info(
    url: &str,
    data: &Value,
    selection: Option<&MediaSelection>,
) -> Result<VideoInfo, String> {
    if !data.is_object()
        || is_playlist(data)
        || matches!(data["_type"].as_str(), Some("url" | "url_transparent"))
    {
        return Err("The selected entry did not resolve to a single video.".into());
    }
    if let Some(selection) = selection {
        if data["id"].as_str() != Some(selection.entry_id.as_str())
            || data["extractor_key"].as_str() != Some(selection.extractor_key.as_str())
        {
            return Err(
                "The selected playlist entry changed. Inspect the URL again before adding it."
                    .into(),
            );
        }
    }
    let mut heights: Vec<u64> = data["formats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|format| format["height"].as_u64())
        .filter(|height| *height > 0)
        .collect();
    heights.sort_unstable();
    heights.dedup();
    heights.reverse();
    Ok(VideoInfo {
        id: data["id"].as_str().unwrap_or("unknown").to_string(),
        title: data["title"]
            .as_str()
            .unwrap_or("Unknown Title")
            .to_string(),
        duration: data["duration"].as_f64(),
        channel: data["channel"].as_str().map(str::to_string),
        thumbnail: thumbnail(data["thumbnail"].as_str()),
        url: url.to_string(),
        available_qualities: heights.iter().map(|height| format!("{height}p")).collect(),
        has_audio: data["acodec"]
            .as_str()
            .map(|codec| codec != "none")
            .unwrap_or(true),
        selection: selection.cloned(),
    })
}

pub(super) fn parse_inspection(
    url: &str,
    bytes: &[u8],
    selection: Option<&MediaSelection>,
) -> Result<UrlInspection, String> {
    if bytes.len() > MAX_INSPECTION_OUTPUT_BYTES {
        return Err("process_output_limit: inspection metadata exceeded its output limit".into());
    }
    let data: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("Failed to parse info: {error}"))?;
    if let Some(selection) = selection {
        selection.validate()?;
        let selected = if is_playlist(&data) {
            let entries = data["entries"]
                .as_array()
                .ok_or("Selected playlist metadata has no entries.")?;
            if entries.len() != 1 {
                return Err(
                    "The selected playlist entry did not resolve to exactly one video.".into(),
                );
            }
            &entries[0]
        } else {
            &data
        };
        return Ok(UrlInspection::Video {
            video: video_info(url, selected, Some(selection))?,
        });
    }
    if !is_playlist(&data) {
        return Ok(UrlInspection::Video {
            video: video_info(url, &data, None)?,
        });
    }
    let records = data["entries"]
        .as_array()
        .ok_or("Failed to parse playlist entries")?;
    let parent_url = data["webpage_url"]
        .as_str()
        .filter(|value| is_allowed_download_url(value))
        .unwrap_or(url);
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for record in records.iter().take(MAX_PLAYLIST_ENTRIES) {
        if record.is_null() {
            continue;
        }
        let has_formats = record["formats"].is_array();
        let record = PlaylistRecord::deserialize(record)
            .map_err(|error| format!("Failed to parse playlist entry: {error}"))?;
        if let Some(entry) = record.into_entry(parent_url, has_formats)? {
            let identity = (
                entry.url.clone(),
                entry
                    .selection
                    .as_ref()
                    .map(|selection| (selection.entry_id.clone(), selection.extractor_key.clone())),
            );
            if seen.insert(identity) {
                entries.push(entry);
            }
        }
    }
    if entries.is_empty() {
        return Err("Failed to parse playlist entries".into());
    }
    Ok(UrlInspection::Playlist {
        playlist: PlaylistInfo {
            title: data["title"].as_str().unwrap_or("Playlist").to_string(),
            channel: data["uploader"]
                .as_str()
                .or_else(|| data["channel"].as_str())
                .map(str::to_string),
            entry_count: entries.len(),
            truncated: records.len() > MAX_PLAYLIST_ENTRIES,
            entries,
        },
    })
}
