use super::*;
use crate::models::{
    MediaSelection, PlaylistAdmissionInput, PlaylistEntry, PlaylistInfo, UrlInspection, VideoInfo,
};
use crate::services::test_support::started_backend;
use std::path::{Path, PathBuf};

async fn completed_playlist_input(
    backend: &Backend,
    output_dir: &Path,
    request_id: &str,
) -> AddQueueItemInput {
    let (parent, _) = backend
        .state_store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    let selection = MediaSelection {
        entry_id: "receipt-video".into(),
        extractor_key: "Youtube".into(),
        playlist_index: 1,
    };
    let url = "https://example.com/watch?v=receipt-video".to_string();
    backend
        .state_store
        .complete_inspection(
            &parent.id,
            UrlInspection::Playlist {
                playlist: PlaylistInfo {
                    title: "Receipt playlist".into(),
                    channel: None,
                    entry_count: 1,
                    truncated: false,
                    entries: vec![PlaylistEntry {
                        id: "receipt-video".into(),
                        title: Some("Receipt video".into()),
                        duration: Some(10.0),
                        url: url.clone(),
                        thumbnail: None,
                        selection: Some(selection.clone()),
                        video: Some(Box::new(VideoInfo {
                            id: "receipt-video".into(),
                            title: "Receipt video".into(),
                            duration: Some(10.0),
                            channel: None,
                            thumbnail: None,
                            url,
                            available_qualities: vec!["720p".into()],
                            has_audio: true,
                            selection: Some(selection),
                        })),
                    }],
                    inspection_settings_fingerprint: Some(
                        crate::models::inspection_settings_fingerprint(None, None),
                    ),
                },
            },
        )
        .await
        .unwrap();
    AddQueueItemInput {
        inspection_operation_id: parent.id,
        format: "mp4".into(),
        quality: "720p".into(),
        output_dir: output_dir.to_string_lossy().into_owned(),
        cookie_config: None,
        filename_override: None,
        compat_config_path: None,
        playlist: Some(PlaylistAdmissionInput {
            request_id: request_id.into(),
            entry_indices: vec![0],
        }),
    }
}

fn playlist_receipt(result: AddQueueItemResult) -> crate::models::PlaylistAdmissionResult {
    match result {
        AddQueueItemResult::Playlist(receipt) => receipt,
        AddQueueItemResult::Single(_) => panic!("expected playlist admission receipt"),
    }
}

fn noncanonical_output(root: &Path) -> (PathBuf, PathBuf) {
    let parent = root.join("output-parent");
    std::fs::create_dir_all(parent.join("alias")).unwrap();
    (
        parent.join("alias").join("..").join("output"),
        parent.join("output"),
    )
}

#[tokio::test]
async fn playlist_replay_uses_raw_fingerprint_before_revalidating_deleted_output() {
    let (backend, root) = started_backend("playlist-raw-replay").await;
    let (raw_output, canonical_output) = noncanonical_output(&root);
    let input = completed_playlist_input(&backend, &raw_output, "raw-replay").await;

    let first = playlist_receipt(
        add_inspection_result_to_queue(backend.clone(), input.clone())
            .await
            .unwrap(),
    );
    let item = backend.state_store.queue_item(&first.item_ids[0]).unwrap();
    assert_eq!(
        display_output_directory(item.output_dir),
        display_output_directory(
            std::fs::canonicalize(&canonical_output)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        )
    );

    std::fs::remove_dir(&canonical_output).unwrap();
    std::fs::write(
        &canonical_output,
        b"the former output directory is unavailable",
    )
    .unwrap();
    backend.state_store.reset_save_attempts_for_test();
    let replay = playlist_receipt(
        add_inspection_result_to_queue(backend.clone(), input)
            .await
            .expect("a durable receipt must replay without touching the output directory"),
    );
    assert_eq!(replay.item_ids, first.item_ids);
    assert_eq!(backend.state_store.save_attempts_for_test(), 0);

    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn playlist_reused_request_id_checks_raw_payload_before_output_filesystem() {
    let (backend, root) = started_backend("playlist-raw-conflict").await;
    let (raw_output, _) = noncanonical_output(&root);
    let input = completed_playlist_input(&backend, &raw_output, "raw-conflict").await;
    add_inspection_result_to_queue(backend.clone(), input.clone())
        .await
        .unwrap();

    let unavailable = root.join("unavailable-output");
    std::fs::write(&unavailable, b"not a directory").unwrap();
    let mut changed = input;
    changed.output_dir = unavailable.to_string_lossy().into_owned();
    backend.state_store.reset_save_attempts_for_test();
    let error = add_inspection_result_to_queue(backend.clone(), changed)
        .await
        .expect_err("the reused request ID has a different raw payload");
    assert_eq!(error.code, "playlist_request_conflict");
    assert_eq!(backend.state_store.save_attempts_for_test(), 0);

    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}
