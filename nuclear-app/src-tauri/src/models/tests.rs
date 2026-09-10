use super::*;
use ts_rs::{Config, TS};

fn valid_media_selection() -> MediaSelection {
    MediaSelection {
        entry_id: "2097430424205295616".to_string(),
        extractor_key: "Twitter".to_string(),
        playlist_index: 1,
    }
}

#[test]
fn media_selection_accepts_the_complete_valid_range() {
    let first = valid_media_selection();
    let mut last = first.clone();
    last.playlist_index = 1_000;
    last.entry_id = "i".repeat(4 * 1024);
    last.extractor_key = "e".repeat(4 * 1024);

    assert!(first.validate().is_ok());
    assert!(last.validate().is_ok());
}

#[test]
fn media_selection_rejects_out_of_range_indexes() {
    for playlist_index in [0, 1_001] {
        let mut selection = valid_media_selection();
        selection.playlist_index = playlist_index;

        assert!(selection.validate().is_err());
    }
}

#[test]
fn media_selection_rejects_invalid_entry_ids_and_extractor_keys() {
    for entry_id in ["", " ", " media-id", "media-id ", "media\nid"] {
        let mut selection = valid_media_selection();
        selection.entry_id = entry_id.to_string();

        assert!(
            selection.validate().is_err(),
            "accepted entry ID {entry_id:?}"
        );
    }
    for extractor_key in ["", " ", " Twitter", "Twitter ", "Twitter\r"] {
        let mut selection = valid_media_selection();
        selection.extractor_key = extractor_key.to_string();

        assert!(
            selection.validate().is_err(),
            "accepted extractor key {extractor_key:?}"
        );
    }
}

#[test]
fn media_selection_rejects_fields_over_four_kibibytes() {
    let mut oversized_id = valid_media_selection();
    oversized_id.entry_id = "i".repeat(4 * 1024 + 1);
    assert!(oversized_id.validate().is_err());

    let mut oversized_extractor = valid_media_selection();
    oversized_extractor.extractor_key = "e".repeat(4 * 1024 + 1);
    assert!(oversized_extractor.validate().is_err());
}

#[test]
fn begin_inspection_defaults_missing_or_null_selection_to_none() {
    let missing: BeginInspectionInput = serde_json::from_value(serde_json::json!({
        "url": "https://example.com/video",
        "cookieConfig": null,
        "compatConfigPath": null
    }))
    .unwrap();
    let explicit_null: BeginInspectionInput = serde_json::from_value(serde_json::json!({
        "url": "https://example.com/video",
        "cookieConfig": null,
        "compatConfigPath": null,
        "selection": null
    }))
    .unwrap();

    assert!(missing.selection.is_none());
    assert!(explicit_null.selection.is_none());
}

#[test]
fn update_filename_distinguishes_missing_null_and_value() {
    let missing = serde_json::from_str::<UpdateQueueItemInput>("{}").unwrap();
    let cleared =
        serde_json::from_str::<UpdateQueueItemInput>(r#"{"filenameOverride":null}"#).unwrap();
    let value =
        serde_json::from_str::<UpdateQueueItemInput>(r#"{"filenameOverride":"renamed"}"#).unwrap();

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

#[test]
fn schema_one_snapshots_default_new_persistence_fields() {
    let operation = serde_json::json!({
        "schemaVersion": APP_SCHEMA_VERSION,
        "id": "operation",
        "queueItemId": null,
        "kind": "inspection",
        "state": "completed",
        "progress": 100.0,
        "phase": null,
        "sequence": 1,
        "createdAtMs": 1,
        "updatedAtMs": 2,
        "finishedAtMs": 2,
        "error": null,
        "inspectionResult": null,
        "correlationId": "correlation"
    });
    let operation: OperationSnapshot = serde_json::from_value(operation).unwrap();
    assert!(operation.published_output.is_none());
    assert!(operation.intended_terminal_outcome.is_none());

    let snapshot = serde_json::json!({
        "schemaVersion": APP_SCHEMA_VERSION,
        "queue": [],
        "operations": [],
        "runtimeReadiness": "ready",
        "maintenanceActive": false,
        "draining": false,
        "latestSequence": 0
    });
    let snapshot: AppSnapshot = serde_json::from_value(snapshot).unwrap();
    assert_eq!(snapshot.persistence_health, PersistenceHealth::default());
}

fn assert_committed_binding<T: TS + 'static>(committed: &str) {
    let generated = T::export_to_string(&Config::default()).unwrap();
    let relative = T::output_path().expect("exported binding path");
    assert_eq!(
        committed,
        generated,
        "binding drifted: {}",
        relative.display()
    );
}

// ts-rs export tests write these files concurrently. Embed the pre-test
// contents so checking never races a truncating write or masks real drift.
macro_rules! check_binding {
    ($ty:ty, $file:literal) => {
        assert_committed_binding::<$ty>(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/bindings/",
            $file,
            ".ts"
        )));
    };
}

#[test]
fn committed_typescript_bindings_match_every_public_contract() {
    check_binding!(crate::app_error::AppError, "AppError");
    check_binding!(VideoInfo, "VideoInfo");
    check_binding!(MediaSelection, "MediaSelection");
    check_binding!(CookieConfig, "CookieConfig");
    check_binding!(PlaylistEntry, "PlaylistEntry");
    check_binding!(PlaylistInfo, "PlaylistInfo");
    check_binding!(UrlInspection, "UrlInspection");
    check_binding!(DownloadRequest, "DownloadRequest");
    check_binding!(DownloadProgress, "DownloadProgress");
    check_binding!(DownloaderToolStatus, "DownloaderToolStatus");
    check_binding!(DownloaderRuntimeStatus, "DownloaderRuntimeStatus");
    check_binding!(DownloaderRuntimeState, "DownloaderRuntimeState");
    check_binding!(DownloaderRuntimeUpdateCheck, "DownloaderRuntimeUpdateCheck");
    check_binding!(
        DownloaderRuntimeUpdateProgress,
        "DownloaderRuntimeUpdateProgress"
    );
    check_binding!(UpdateCheckResult, "UpdateCheckResult");
    check_binding!(UpdateInstallProgress, "UpdateInstallProgress");
    check_binding!(QueueItemState, "QueueItemState");
    check_binding!(OperationKind, "OperationKind");
    check_binding!(OperationState, "OperationState");
    check_binding!(RuntimeReadiness, "RuntimeReadiness");
    check_binding!(QueuePriority, "QueuePriority");
    check_binding!(PublishedOutput, "PublishedOutput");
    check_binding!(IntendedTerminalOutcome, "IntendedTerminalOutcome");
    check_binding!(PersistenceHealth, "PersistenceHealth");
    check_binding!(QueueItemRecord, "QueueItemRecord");
    check_binding!(OperationSnapshot, "OperationSnapshot");
    check_binding!(AppSnapshot, "AppSnapshot");
    check_binding!(AppStateResyncRequired, "AppStateResyncRequired");
    check_binding!(StateDeltaValue, "StateDeltaValue");
    check_binding!(StateDelta, "StateDelta");
    check_binding!(AddQueueItemInput, "AddQueueItemInput");
    check_binding!(UpdateQueueItemInput, "UpdateQueueItemInput");
    check_binding!(BeginInspectionInput, "BeginInspectionInput");
    check_binding!(BeginOperationResult, "BeginOperationResult");
    check_binding!(CancelAllResult, "CancelAllResult");
}
