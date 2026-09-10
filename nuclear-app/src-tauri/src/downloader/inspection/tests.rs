use super::metadata::parse_inspection;
use super::{append_inspection_args, with_inspection_timeout, MAX_INSPECTION_OUTPUT_BYTES};
use crate::downloader::process::DownloadJob;
use crate::models::{MediaSelection, UrlInspection};
use serde_json::{json, Value};
use std::time::Duration;

const PARENT_URL: &str = "https://example.com/playlist";

fn parse(value: &Value, selection: Option<&MediaSelection>) -> Result<UrlInspection, String> {
    parse_inspection(PARENT_URL, &serde_json::to_vec(value).unwrap(), selection)
}

fn selection(id: &str, key: &str, index: u32) -> MediaSelection {
    MediaSelection {
        entry_id: id.into(),
        extractor_key: key.into(),
        playlist_index: index,
    }
}

#[test]
fn playlist_entries_are_deduplicated_and_bounded_at_one_thousand() {
    let mut entries = Vec::new();
    for index in 0..1_001 {
        entries.push(json!({
            "_type": "url",
            "id": format!("video-{index}"),
            "url": format!("https://example.com/video/{index}"),
        }));
    }
    entries.insert(1, entries[0].clone());

    let UrlInspection::Playlist { playlist } = parse(
        &json!({"_type":"playlist", "title":"Large", "entries":entries}),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    // The discovery budget counts source records, including duplicates.
    assert_eq!(playlist.entries.len(), 999);
    assert_eq!(playlist.entry_count, 999);
    assert!(playlist.truncated);
    assert_eq!(playlist.entries[0].id, "video-0");
    assert_eq!(playlist.entries[998].id, "video-998");
}

#[test]
fn playlist_keeps_https_thumbnail_and_parent_metadata_hints() {
    let UrlInspection::Playlist { playlist } = parse(
        &json!({
            "_type":"playlist", "title":"Example Playlist", "uploader":"Example Channel",
            "entries":[{
                "_type":"url", "id":"abc123", "title":"Example Clip", "duration":42,
                "url":"https://example.com/watch/abc123",
                "thumbnail":"http://example.com/thumb-low.jpg",
                "thumbnails":[
                    {"url":"http://example.com/thumb-low.jpg"},
                    {"url":"https://example.com/thumb-hi.jpg"}
                ]
            }]
        }),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    assert_eq!(playlist.title, "Example Playlist");
    assert_eq!(playlist.channel.as_deref(), Some("Example Channel"));
    assert_eq!(playlist.entries[0].title.as_deref(), Some("Example Clip"));
    assert_eq!(playlist.entries[0].duration, Some(42.0));
    assert_eq!(
        playlist.entries[0].thumbnail.as_deref(),
        Some("https://example.com/thumb-hi.jpg")
    );
}

#[test]
fn youtube_entry_falls_back_to_watch_url_but_generic_missing_url_is_rejected() {
    let UrlInspection::Playlist { playlist } = parse(
        &json!({"_type":"playlist", "entries":[{
            "_type":"url", "id":"fallback-id", "extractor_key":"Youtube"
        }]}),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    assert_eq!(
        playlist.entries[0].url,
        "https://www.youtube.com/watch?v=fallback-id"
    );

    let error = parse(
        &json!({"_type":"playlist", "entries":[{
            "_type":"url", "id":"opaque-id", "extractor_key":"Generic"
        }]}),
        None,
    )
    .unwrap_err();
    assert_eq!(error, "Failed to parse playlist entries");
}

#[test]
fn x_multi_video_children_sharing_parent_are_retained_with_ordinal_selectors() {
    let parent = "https://x.com/LLMenjoyer/status/2097804593132671192";
    let metadata = json!({
        "_type":"multi_video", "webpage_url":parent, "entries":[
            {"_type":"video", "id":"2049184998117588992", "extractor_key":"Twitter",
             "webpage_url":parent, "playlist_index":1, "formats":[{"height":720}]},
            {"_type":"video", "id":"2097430424205295616", "extractor_key":"Twitter",
             "webpage_url":parent, "playlist_index":2, "formats":[{"height":1080}]}
        ]
    });
    let bytes = serde_json::to_vec(&metadata).unwrap();
    let UrlInspection::Playlist { playlist } = parse_inspection(parent, &bytes, None).unwrap()
    else {
        panic!("expected playlist");
    };
    assert_eq!(playlist.entries.len(), 2);
    assert_eq!(playlist.entries[0].url, parent);
    assert_eq!(playlist.entries[1].url, parent);
    assert_eq!(
        playlist.entries[0].selection.as_ref().unwrap().entry_id,
        "2049184998117588992"
    );
    assert_eq!(
        playlist.entries[0]
            .selection
            .as_ref()
            .unwrap()
            .extractor_key,
        "Twitter"
    );
    assert_eq!(
        playlist.entries[0]
            .selection
            .as_ref()
            .unwrap()
            .playlist_index,
        1
    );
    assert_eq!(
        playlist.entries[1].selection.as_ref().unwrap().entry_id,
        "2097430424205295616"
    );
    assert_eq!(
        playlist.entries[1]
            .selection
            .as_ref()
            .unwrap()
            .playlist_index,
        2
    );
}

#[test]
fn shared_parent_embedded_video_requires_authoritative_playlist_index() {
    let error = parse(
        &json!({"_type":"multi_video", "webpage_url":PARENT_URL, "entries":[{
            "_type":"video", "id":"embedded", "extractor_key":"Twitter",
            "webpage_url":PARENT_URL, "formats":[{"height":720}]
        }]}),
        None,
    )
    .unwrap_err();
    assert!(error.contains("selection index"), "{error}");
}

#[test]
fn shared_parent_flat_record_prefers_child_url_and_skips_one_without_it() {
    let UrlInspection::Playlist { playlist } = parse(
        &json!({"_type":"playlist", "webpage_url":PARENT_URL, "entries":[
            {"_type":"url", "id":"child", "webpage_url":PARENT_URL,
             "url":"https://example.com/watch/child"},
            {"_type":"url", "id":"missing", "webpage_url":PARENT_URL,
             "extractor_key":"Generic"}
        ]}),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    assert_eq!(playlist.entries.len(), 1);
    assert_eq!(playlist.entries[0].id, "child");
    assert_eq!(playlist.entries[0].url, "https://example.com/watch/child");
    assert!(playlist.entries[0].selection.is_none());
}

#[test]
fn formats_bearing_record_without_type_uses_verified_selector() {
    let UrlInspection::Playlist { playlist } = parse(
        &json!({"_type":"multi_video", "webpage_url":PARENT_URL, "entries":[{
            "id":"embedded", "extractor_key":"Generic", "webpage_url":PARENT_URL,
            "playlist_index":1, "formats":[{"height":720}]
        }]}),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    let selected = playlist.entries[0].selection.as_ref().unwrap();
    assert_eq!(selected.entry_id, "embedded");
    assert_eq!(selected.extractor_key, "Generic");
    assert_eq!(selected.playlist_index, 1);
}

#[test]
fn selected_single_wrapper_returns_exact_video_identity() {
    let selected = selection("wanted", "Twitter", 2);
    let UrlInspection::Video { video } = parse(
        &json!({"_type":"playlist", "entries":[{
            "_type":"video", "id":"wanted", "extractor_key":"Twitter",
            "title":"Selected", "formats":[{"height":1080},{"height":720}], "acodec":"aac"
        }]}),
        Some(&selected),
    )
    .unwrap() else {
        panic!("expected video");
    };
    assert_eq!(video.id, "wanted");
    assert_eq!(video.title, "Selected");
    assert_eq!(video.available_qualities, ["1080p", "720p"]);
    assert_eq!(video.selection.as_ref().unwrap().playlist_index, 2);
}

#[test]
fn selected_identity_rejects_wrong_id_or_extractor_key() {
    let selected = selection("wanted", "Twitter", 1);
    for record in [
        json!({"_type":"video", "id":"wrong", "extractor_key":"Twitter"}),
        json!({"_type":"video", "id":"wanted", "extractor_key":"Wrong"}),
    ] {
        let error = parse(
            &json!({"_type":"playlist", "entries":[record]}),
            Some(&selected),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "The selected playlist entry changed. Inspect the URL again before adding it."
        );
    }
}

#[test]
fn selected_wrapper_rejects_zero_multiple_nested_and_null_entries() {
    let selected = selection("wanted", "Twitter", 1);
    let cases = [
        json!({"_type":"playlist", "entries":[]}),
        json!({"_type":"playlist", "entries":[
            {"_type":"video", "id":"wanted", "extractor_key":"Twitter"},
            {"_type":"video", "id":"other", "extractor_key":"Twitter"}
        ]}),
        json!({"_type":"playlist", "entries":[
            {"_type":"playlist", "id":"wanted", "extractor_key":"Twitter", "entries":[]}
        ]}),
        json!({"_type":"playlist", "entries":[null]}),
    ];
    for value in cases {
        assert!(parse(&value, Some(&selected)).is_err(), "accepted {value}");
    }
}

#[test]
fn ordinary_video_and_generic_flat_playlist_are_preserved() {
    let UrlInspection::Video { video } = parse(
        &json!({"_type":"video", "id":"single", "title":"Single", "formats":[]}),
        None,
    )
    .unwrap() else {
        panic!("expected video");
    };
    assert_eq!(video.id, "single");
    assert!(video.selection.is_none());

    let UrlInspection::Playlist { playlist } = parse(
        &json!({"_type":"playlist", "entries":[{
            "_type":"url", "id":"flat", "extractor_key":"Generic",
            "url":"https://example.com/watch/flat"
        }]}),
        None,
    )
    .unwrap() else {
        panic!("expected playlist");
    };
    assert_eq!(playlist.entries[0].url, "https://example.com/watch/flat");
    assert!(playlist.entries[0].selection.is_none());
}

#[test]
fn inspection_metadata_is_one_json_value_and_bounded_to_eight_mib() {
    let too_large = vec![b' '; MAX_INSPECTION_OUTPUT_BYTES + 1];
    assert!(parse_inspection(PARENT_URL, &too_large, None)
        .unwrap_err()
        .starts_with("process_output_limit:"));
    assert!(parse_inspection(PARENT_URL, b"null", None).is_err());
    assert!(parse_inspection(PARENT_URL, b"[]", None).is_err());
    assert!(parse_inspection(PARENT_URL, b"{}\n{}", None).is_err());
}

#[test]
fn inspection_command_uses_one_bounded_pass_and_selected_ordinal() {
    let mut discovery = Vec::new();
    append_inspection_args(&mut discovery, None);
    assert_eq!(
        discovery
            .iter()
            .filter(|arg| *arg == "--dump-single-json")
            .count(),
        1
    );
    assert!(discovery
        .windows(2)
        .any(|pair| pair == ["--playlist-end", "1001"]));
    assert!(discovery.contains(&"--flat-playlist".into()));
    assert!(discovery.contains(&"--lazy-playlist".into()));

    let mut selected_args = Vec::new();
    append_inspection_args(&mut selected_args, Some(&selection("id", "Twitter", 2)));
    assert_eq!(
        selected_args
            .iter()
            .filter(|arg| *arg == "--dump-single-json")
            .count(),
        1
    );
    assert!(selected_args
        .windows(2)
        .any(|pair| pair == ["--playlist-items", "2"]));
    assert!(selected_args.contains(&"--yes-playlist".into()));
    assert!(selected_args.contains(&"--no-flat-playlist".into()));
    assert!(!selected_args.contains(&"--flat-playlist".into()));
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
