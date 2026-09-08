use super::super::process::{record_streamed_output_bytes, DownloadJob};
use super::{
    parse_first_json_value, push_bounded_playlist_entry, sanitize_thumbnail_url,
    with_inspection_timeout, PlaylistLineRecord, MAX_INSPECTION_OUTPUT_BYTES, MAX_PLAYLIST_ENTRIES,
};
use crate::models::PlaylistEntry;
use std::collections::HashSet;
use std::time::Duration;

fn playlist_entry(index: usize, url: String) -> PlaylistEntry {
    PlaylistEntry {
        id: format!("video-{index}"),
        title: Some(format!("Video {index}")),
        duration: None,
        url,
        thumbnail: None,
    }
}

#[test]
fn playlist_entries_are_deduplicated_and_bounded() {
    let mut entries = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut parsed_count = 0;
    let mut truncated = false;

    push_bounded_playlist_entry(
        &mut entries,
        &mut seen_urls,
        &mut parsed_count,
        &mut truncated,
        playlist_entry(0, "https://example.com/video/0".into()),
    );
    push_bounded_playlist_entry(
        &mut entries,
        &mut seen_urls,
        &mut parsed_count,
        &mut truncated,
        playlist_entry(1, "https://example.com/video/0".into()),
    );
    assert_eq!(entries.len(), 1);

    entries.clear();
    seen_urls.clear();
    parsed_count = 0;
    truncated = false;
    for index in 0..=MAX_PLAYLIST_ENTRIES {
        push_bounded_playlist_entry(
            &mut entries,
            &mut seen_urls,
            &mut parsed_count,
            &mut truncated,
            playlist_entry(index, format!("https://example.com/video/{index}")),
        );
    }

    assert_eq!(entries.len(), MAX_PLAYLIST_ENTRIES);
    assert!(truncated);
}

#[test]
fn keeps_only_https_thumbnail_urls() {
    assert_eq!(
        sanitize_thumbnail_url(Some("https://example.com/thumb.jpg")),
        Some("https://example.com/thumb.jpg".into())
    );
    assert_eq!(
        sanitize_thumbnail_url(Some("http://example.com/thumb.jpg")),
        None
    );
    assert_eq!(sanitize_thumbnail_url(Some("file:///C:/thumb.jpg")), None);
}

#[test]
fn playlist_line_prefers_last_thumbnail_and_keeps_metadata_hints() {
    let line = serde_json::from_str::<PlaylistLineRecord>(
        r#"{
            "id":"abc123",
            "title":"Example Clip",
            "duration":42,
            "url":"https://example.com/watch/abc123",
            "thumbnail":"http://example.com/thumb-low.jpg",
            "thumbnails":[
                {"url":"http://example.com/thumb-low.jpg"},
                {"url":"https://example.com/thumb-hi.jpg"}
            ],
            "playlist_title":"Example Playlist",
            "playlist_uploader":"Example Channel"
        }"#,
    )
    .unwrap();

    assert_eq!(line.playlist_title_hint(), Some("Example Playlist"));
    assert_eq!(line.playlist_channel_hint(), Some("Example Channel"));

    let entry = line.into_playlist_entry().unwrap();
    assert_eq!(entry.id, "abc123");
    assert_eq!(entry.title.as_deref(), Some("Example Clip"));
    assert_eq!(entry.duration, Some(42.0));
    assert_eq!(entry.url, "https://example.com/watch/abc123");
    assert_eq!(
        entry.thumbnail.as_deref(),
        Some("https://example.com/thumb-hi.jpg")
    );
}

#[test]
fn youtube_playlist_line_falls_back_to_watch_url_when_missing_urls() {
    let line = serde_json::from_str::<PlaylistLineRecord>(
        r#"{"id":"fallback-id","title":"Fallback","extractor_key":"Youtube"}"#,
    )
    .unwrap();

    let entry = line.into_playlist_entry().unwrap();
    assert_eq!(entry.url, "https://www.youtube.com/watch?v=fallback-id");
}

#[test]
fn generic_playlist_line_without_a_url_is_rejected() {
    let line = serde_json::from_str::<PlaylistLineRecord>(
        r#"{"id":"opaque-id","title":"Unavailable","extractor_key":"Generic"}"#,
    )
    .unwrap();

    assert!(line.into_playlist_entry().is_none());
}

#[test]
fn parses_first_json_value_from_multiple_documents() {
    let value = parse_first_json_value("{\"id\":\"one\"}\n{\"id\":\"two\"}").unwrap();
    assert_eq!(value["id"].as_str(), Some("one"));
}

#[test]
fn streamed_inspection_enforces_one_cumulative_output_limit() {
    let legal_line_bytes = 60 * 1024;
    let mut total = 0usize;
    let legal_lines = MAX_INSPECTION_OUTPUT_BYTES / (legal_line_bytes + 1);

    for _ in 0..legal_lines {
        record_streamed_output_bytes(&mut total, legal_line_bytes, MAX_INSPECTION_OUTPUT_BYTES)
            .unwrap();
    }
    let error =
        record_streamed_output_bytes(&mut total, legal_line_bytes, MAX_INSPECTION_OUTPUT_BYTES)
            .unwrap_err();

    assert!(error.starts_with("process_output_limit:"), "{error}");
}

#[tokio::test]
async fn stalled_inspection_hits_deadline_without_becoming_user_cancelled() {
    let job = DownloadJob::new().unwrap();

    let error = with_inspection_timeout(
        &job,
        Duration::from_millis(10),
        std::future::pending::<Result<(), String>>(),
    )
    .await
    .unwrap_err();

    assert_eq!(error, "process_timeout: inspection timed out");
    assert!(!job.is_cancelled());
}
