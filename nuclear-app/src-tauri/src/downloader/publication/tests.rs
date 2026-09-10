use super::super::validation::MAX_ACTIONABLE_FIELD_BYTES;
use super::{
    build_staging_dir, cleanup_abandoned_download_stages, cleanup_staging_dir,
    final_output_record_path, publish_staged_output, reset_staging_dir, resolve_staged_output,
    STAGING_MARKER_NAME, STAGING_ROOT_NAME,
};
use std::path::PathBuf;

fn temp_stage() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nuclear-final-output-record-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn write_record(stage: &std::path::Path, path: &std::path::Path) {
    let record = serde_json::json!({
        "schema_version": 1,
        "filepath": path.to_string_lossy(),
    });
    std::fs::write(final_output_record_path(stage), format!("{record}\n")).unwrap();
}

fn selected_media() -> crate::models::MediaSelection {
    crate::models::MediaSelection {
        entry_id: "selected-id".into(),
        extractor_key: "Youtube".into(),
        playlist_index: 3,
    }
}

fn write_selected_record(
    stage: &std::path::Path,
    path: &std::path::Path,
    id: &str,
    extractor_key: &str,
) {
    let record = serde_json::json!({
        "schema_version": 1,
        "filepath": path.to_string_lossy(),
        "id": id,
        "extractor_key": extractor_key,
    });
    std::fs::write(final_output_record_path(stage), format!("{record}\n")).unwrap();
}

#[test]
fn machine_record_resolves_the_exact_staged_output() {
    let stage = temp_stage();
    let media = stage.join("chosen.mp4");
    std::fs::write(&media, b"media").unwrap();
    std::fs::write(stage.join("other.txt"), b"not media").unwrap();
    write_record(&stage, &media);

    assert_eq!(
        resolve_staged_output(&stage, None).unwrap(),
        media.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn malformed_present_record_never_uses_fallback() {
    let stage = temp_stage();
    std::fs::write(stage.join("only.mp4"), b"media").unwrap();
    std::fs::write(final_output_record_path(&stage), b"not json\n").unwrap();

    let error = resolve_staged_output(&stage, None).unwrap_err();

    assert_eq!(error.code, "staging_output_record_invalid");
    assert!(error.message.contains("malformed"));
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn stale_small_record_metadata_never_allows_an_unbounded_read() {
    let stage = temp_stage();
    let record_path = final_output_record_path(&stage);
    std::fs::write(&record_path, b"{}\n").unwrap();
    let stale_small_metadata = std::fs::symlink_metadata(&record_path).unwrap();
    let oversized = vec![b' '; super::MAX_FINAL_OUTPUT_RECORD_BYTES as usize + 17];
    std::fs::write(&record_path, &oversized).unwrap();
    super::TEST_FINAL_OUTPUT_RECORD_READ_BYTES.with(|bytes| bytes.set(0));

    let error = super::resolve_recorded_output(&record_path, &stale_small_metadata, &stage, None)
        .unwrap_err();
    let observed = super::TEST_FINAL_OUTPUT_RECORD_READ_BYTES.with(std::cell::Cell::get);

    assert_eq!(error.code, "staging_output_record_invalid");
    assert!(
        observed <= super::MAX_FINAL_OUTPUT_RECORD_BYTES as usize + 1,
        "record reader consumed {observed} bytes before rejecting its fixed budget"
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), oversized);
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn multiple_machine_records_are_rejected_as_ambiguous_authority() {
    let stage = temp_stage();
    let media = stage.join("chosen.mp4");
    std::fs::write(&media, b"media").unwrap();
    let line = serde_json::json!({
        "schema_version": 1,
        "filepath": media.to_string_lossy(),
    });
    std::fs::write(
        final_output_record_path(&stage),
        format!("{line}\n{line}\n"),
    )
    .unwrap();

    let error = resolve_staged_output(&stage, None).unwrap_err();

    assert_eq!(error.code, "staging_output_record_invalid");
    assert!(error.message.contains("multiple entries"));
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn absent_record_accepts_exactly_one_media_candidate() {
    let stage = temp_stage();
    let media = stage.join("only.mkv");
    std::fs::write(&media, b"media").unwrap();
    std::fs::write(stage.join("sidecar.info.json"), b"{}").unwrap();

    assert_eq!(
        resolve_staged_output(&stage, None).unwrap(),
        media.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn absent_record_rejects_multiple_media_candidates_without_mtime_selection() {
    let stage = temp_stage();
    std::fs::write(stage.join("older.mp4"), b"older").unwrap();
    std::fs::write(stage.join("newer.mkv"), b"newer").unwrap();

    let error = resolve_staged_output(&stage, None).unwrap_err();

    assert_eq!(error.code, "staging_output_ambiguous");
    assert!(error.message.contains("2 staged media files"));
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn recorded_output_outside_stage_is_rejected() {
    let stage = temp_stage();
    let outside = stage.parent().unwrap().join(format!(
        "nuclear-outside-output-{}.mp4",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&outside, b"outside").unwrap();
    write_record(&stage, &outside);

    let error = resolve_staged_output(&stage, None).unwrap_err();

    assert_eq!(error.code, "path_escape");
    let _ = std::fs::remove_file(outside);
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn selected_media_requires_matching_record_identity() {
    let stage = temp_stage();
    let media = stage.join("selected.mp4");
    std::fs::write(&media, b"media").unwrap();
    write_selected_record(&stage, &media, "selected-id", "Youtube");

    assert_eq!(
        resolve_staged_output(&stage, Some(&selected_media())).unwrap(),
        media.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn selected_media_rejects_missing_or_mismatched_identity() {
    for (id, extractor_key) in [
        (None, None),
        (Some("other-id"), Some("Youtube")),
        (Some("selected-id"), Some("Vimeo")),
    ] {
        let stage = temp_stage();
        let media = stage.join("selected.mp4");
        std::fs::write(&media, b"media").unwrap();
        let mut record = serde_json::json!({
            "schema_version": 1,
            "filepath": media.to_string_lossy(),
        });
        if let Some(id) = id {
            record["id"] = id.into();
        }
        if let Some(extractor_key) = extractor_key {
            record["extractor_key"] = extractor_key.into();
        }
        std::fs::write(final_output_record_path(&stage), format!("{record}\n")).unwrap();

        let error = resolve_staged_output(&stage, Some(&selected_media())).unwrap_err();
        assert_eq!(error.code, "staging_output_identity_mismatch");
        let _ = std::fs::remove_dir_all(stage);
    }
}

#[test]
fn selected_media_rejects_missing_record_without_fallback() {
    let stage = temp_stage();
    std::fs::write(stage.join("only.mp4"), b"media").unwrap();

    let error = resolve_staged_output(&stage, Some(&selected_media())).unwrap_err();

    assert_eq!(error.code, "staging_output_record_invalid");
    assert!(error.message.contains("without an output identity record"));
    let _ = std::fs::remove_dir_all(stage);
}

#[test]
fn selected_media_rejects_multiple_matching_records() {
    let stage = temp_stage();
    let media = stage.join("selected.mp4");
    std::fs::write(&media, b"media").unwrap();
    let line = serde_json::json!({
        "schema_version": 1,
        "filepath": media.to_string_lossy(),
        "id": "selected-id",
        "extractor_key": "Youtube",
    });
    std::fs::write(
        final_output_record_path(&stage),
        format!("{line}\n{line}\n"),
    )
    .unwrap();

    let error = resolve_staged_output(&stage, Some(&selected_media())).unwrap_err();

    assert_eq!(error.code, "staging_output_record_invalid");
    assert!(error.message.contains("multiple entries"));
    let _ = std::fs::remove_dir_all(stage);
}

#[tokio::test]
async fn publishing_never_overwrites_and_adds_a_suffix() {
    let root = std::env::temp_dir().join(format!("nuclear-publish-test-{}", uuid::Uuid::new_v4()));
    let staging_dir = root.join("staging");
    let output_dir = root.join("output");
    std::fs::create_dir_all(&staging_dir).unwrap();
    std::fs::create_dir_all(&output_dir).unwrap();

    let staged = staging_dir.join("Clip.mp4");
    let desired = output_dir.join("Clip.mp4");
    std::fs::write(&staged, b"new bytes").unwrap();
    std::fs::write(&desired, b"old bytes").unwrap();

    let published = publish_staged_output(&staged, &desired, None)
        .await
        .unwrap();
    assert_eq!(published, output_dir.join("Clip (2).mp4"));
    assert_eq!(std::fs::read(&desired).unwrap(), b"old bytes");
    assert_eq!(std::fs::read(&published).unwrap(), b"new bytes");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn concurrent_publication_allocates_distinct_names() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-concurrent-publish-test-{}",
        uuid::Uuid::new_v4()
    ));
    let staging_dir = root.join("staging");
    let output_dir = root.join("output");
    std::fs::create_dir_all(&staging_dir).unwrap();
    std::fs::create_dir_all(&output_dir).unwrap();

    let first_staged = staging_dir.join("first.mp4");
    let second_staged = staging_dir.join("second.mp4");
    let desired = output_dir.join("Clip.mp4");
    std::fs::write(&first_staged, b"first").unwrap();
    std::fs::write(&second_staged, b"second").unwrap();

    let (first, second) = tokio::join!(
        publish_staged_output(&first_staged, &desired, None),
        publish_staged_output(&second_staged, &desired, None),
    );
    let mut published = vec![first.unwrap(), second.unwrap()];
    published.sort();

    assert_eq!(
        published,
        vec![output_dir.join("Clip (2).mp4"), output_dir.join("Clip.mp4")]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn publication_rejects_a_path_that_terminal_metadata_cannot_store() {
    let root = temp_stage();
    let staged = root.join("staged.mp4");
    std::fs::write(&staged, b"staged").unwrap();
    let desired = PathBuf::from("x".repeat(MAX_ACTIONABLE_FIELD_BYTES + 1));

    let error = publish_staged_output(&staged, &desired, None)
        .await
        .unwrap_err();

    assert!(error.contains("4 KiB metadata limit"), "{error}");
    assert!(staged.is_file());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn abandoned_stage_cleanup_deletes_only_marker_owned_uuid_directories() {
    let output =
        std::env::temp_dir().join(format!("nuclear-stage-cleanup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&output).unwrap();
    let valid_id = uuid::Uuid::new_v4().to_string();
    let valid = build_staging_dir(&output, &valid_id);
    reset_staging_dir(&valid, &output, &valid_id).unwrap();
    std::fs::write(valid.join("partial.bin"), b"owned").unwrap();
    let unowned = output
        .join(STAGING_ROOT_NAME)
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&unowned).unwrap();
    std::fs::write(unowned.join("user-file.txt"), b"preserve").unwrap();

    let failures = cleanup_abandoned_download_stages(&[output.to_string_lossy().into_owned()]);

    assert!(failures.is_empty());
    assert!(!valid.exists());
    assert!(unowned.join("user-file.txt").is_file());
    let _ = std::fs::remove_dir_all(output);
}

#[test]
fn active_stage_cleanup_refuses_a_wrong_operation_marker() {
    let output =
        std::env::temp_dir().join(format!("nuclear-stage-marker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&output).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = build_staging_dir(&output, &operation_id);
    reset_staging_dir(&stage, &output, &operation_id).unwrap();
    std::fs::write(stage.join("keep.txt"), b"preserve").unwrap();
    let wrong_marker = serde_json::json!({
        "schemaVersion": 1,
        "owner": "nuclear-downloader",
        "operationId": uuid::Uuid::new_v4().to_string(),
    });
    std::fs::write(
        stage.join(STAGING_MARKER_NAME),
        serde_json::to_vec(&wrong_marker).unwrap(),
    )
    .unwrap();

    assert!(cleanup_staging_dir(&stage, &output, &operation_id).is_err());
    assert!(stage.join("keep.txt").is_file());
    let _ = std::fs::remove_dir_all(output);
}

#[cfg(windows)]
#[test]
fn marker_initialization_failure_removes_the_empty_new_stage() {
    let output = std::env::temp_dir().join(format!(
        "nuclear-stage-marker-init-failure-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = build_staging_dir(&output, &operation_id);
    super::TEST_MARKER_INITIALIZATION_FAILURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|_| Err("forced marker failure".to_string())));
    });

    let error = reset_staging_dir(&stage, &output, &operation_id).unwrap_err();

    assert!(error.contains("forced marker failure"));
    assert!(
        !stage.exists(),
        "failed initialization left a blocking stage"
    );

    reset_staging_dir(&stage, &output, &operation_id).unwrap();
    assert!(stage.join(STAGING_MARKER_NAME).is_file());
    let _ = std::fs::remove_dir_all(output);
}

#[cfg(windows)]
#[test]
fn partial_marker_failure_removes_only_the_handle_owned_stage() {
    let output = std::env::temp_dir().join(format!(
        "nuclear-stage-partial-marker-failure-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = build_staging_dir(&output, &operation_id);
    super::TEST_PARTIAL_MARKER_WRITE_FAILURE.with(|failure| failure.set(true));

    let error = reset_staging_dir(&stage, &output, &operation_id).unwrap_err();

    assert!(error.contains("forced partial marker write failure"));
    assert!(
        !stage.exists(),
        "owned partial marker and stage were not rolled back"
    );
    reset_staging_dir(&stage, &output, &operation_id).unwrap();
    assert!(stage.join(STAGING_MARKER_NAME).is_file());
    let _ = std::fs::remove_dir_all(output);
}

#[test]
fn marker_initialization_rollback_preserves_unexpected_content() {
    let output = std::env::temp_dir().join(format!(
        "nuclear-stage-marker-rollback-preserve-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = build_staging_dir(&output, &operation_id);
    super::TEST_MARKER_INITIALIZATION_FAILURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage| {
            std::fs::write(stage.join("unexpected.txt"), b"preserve me").unwrap();
            Err("forced marker failure with unexpected content".to_string())
        }));
    });

    let error = reset_staging_dir(&stage, &output, &operation_id).unwrap_err();

    assert!(error.contains("forced marker failure with unexpected content"));
    assert!(error.contains("incomplete staging folder was preserved"));
    assert_eq!(
        std::fs::read(stage.join("unexpected.txt")).unwrap(),
        b"preserve me"
    );
    let _ = std::fs::remove_dir_all(output);
}

#[test]
fn oversized_staging_marker_is_rejected_without_removing_input() {
    let output = std::env::temp_dir().join(format!(
        "nuclear-stage-oversized-marker-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = build_staging_dir(&output, &operation_id);
    reset_staging_dir(&stage, &output, &operation_id).unwrap();
    let marker_path = stage.join(STAGING_MARKER_NAME);
    let mut marker = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 1,
        "owner": "nuclear-downloader",
        "operationId": operation_id,
    }))
    .unwrap();
    marker.resize(64 * 1024, b' ');
    std::fs::write(&marker_path, &marker).unwrap();

    assert!(super::verify_staging_marker(&stage, &operation_id).is_err());
    assert_eq!(std::fs::read(&marker_path).unwrap(), marker);

    let _ = std::fs::remove_dir_all(output);
}

#[cfg(windows)]
#[test]
fn active_stage_reset_refuses_a_reparse_staging_root() {
    use std::os::windows::fs::symlink_dir;

    let base = std::env::temp_dir().join(format!("nuclear-stage-reparse-{}", uuid::Uuid::new_v4()));
    let output = base.join("output");
    let target = base.join("outside");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let root = output.join(STAGING_ROOT_NAME);
    if symlink_dir(&target, &root).is_err() {
        let _ = std::fs::remove_dir_all(base);
        return;
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = root.join(&operation_id);

    assert!(reset_staging_dir(&stage, &output, &operation_id).is_err());
    assert!(target.read_dir().unwrap().next().is_none());

    let _ = std::fs::remove_dir(&root);
    let _ = std::fs::remove_dir_all(base);
}
