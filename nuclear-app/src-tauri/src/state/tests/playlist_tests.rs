use super::*;
use crate::models::{OperationKind, PlaylistAdmissionInput, QueuePreparation, APP_SCHEMA_VERSION};

fn playlist_input(parent: &str, request_id: &str, indices: Vec<usize>) -> AddQueueItemInput {
    AddQueueItemInput {
        inspection_operation_id: parent.to_string(),
        format: "mp4".into(),
        quality: "720p".into(),
        output_dir: "C:\\Downloads".into(),
        cookie_config: None,
        filename_override: None,
        compat_config_path: None,
        playlist: Some(PlaylistAdmissionInput {
            request_id: request_id.to_string(),
            entry_indices: indices,
        }),
    }
}

fn entry(id: &str, playlist_index: Option<u32>, full: bool) -> PlaylistEntry {
    let url = format!("https://example.com/{id}");
    let selection = playlist_index.map(|playlist_index| MediaSelection {
        entry_id: id.to_string(),
        extractor_key: "Youtube".into(),
        playlist_index,
    });
    let video = full.then(|| {
        Box::new(VideoInfo {
            id: id.to_string(),
            title: format!("Full {id}"),
            duration: Some(12.0),
            channel: Some("channel".into()),
            thumbnail: None,
            url: url.clone(),
            available_qualities: vec!["720p".into()],
            has_audio: true,
            selection: selection.clone(),
        })
    });
    PlaylistEntry {
        id: id.to_string(),
        title: Some(format!("Entry {id}")),
        duration: None,
        url,
        thumbnail: None,
        selection,
        video,
    }
}

async fn completed_playlist(store: &StateStore, entries: Vec<PlaylistEntry>) -> String {
    completed_playlist_with_fingerprint(
        store,
        entries,
        Some(crate::models::inspection_settings_fingerprint(None, None)),
    )
    .await
}

async fn completed_playlist_with_fingerprint(
    store: &StateStore,
    entries: Vec<PlaylistEntry>,
    inspection_settings_fingerprint: Option<String>,
) -> String {
    let (parent, _) = store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(
            &parent.id,
            UrlInspection::Playlist {
                playlist: PlaylistInfo {
                    title: "playlist".into(),
                    channel: None,
                    entry_count: entries.len(),
                    truncated: false,
                    entries,
                    inspection_settings_fingerprint,
                },
            },
        )
        .await
        .unwrap();
    parent.id
}

async fn admitted_pending_batch(store: &StateStore, count: usize, request: &str) -> Vec<String> {
    let entries = (0..count)
        .map(|index| {
            entry(
                &format!("pending-{request}-{index}"),
                Some(index as u32 + 1),
                false,
            )
        })
        .collect();
    let parent = completed_playlist(store, entries).await;
    store
        .add_playlist_items(playlist_input(&parent, request, (0..count).collect()))
        .await
        .unwrap()
        .1
}

#[tokio::test]
async fn playlist_indices_address_retained_entries_and_allow_plain_child_urls() {
    let store = test_store();
    let parent = completed_playlist(
        &store,
        vec![
            entry("first", Some(1), false),
            entry("after-gap", Some(9), false),
            entry("plain", None, false),
        ],
    )
    .await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "request-a", vec![1, 2]))
        .await
        .unwrap();
    assert_eq!(result.item_ids.len(), 2);
    assert_eq!(preparations.len(), 2);
    assert_eq!(
        store
            .queue_item(&result.item_ids[0])
            .unwrap()
            .selection
            .as_ref()
            .unwrap()
            .playlist_index,
        9
    );
    assert!(store
        .queue_item(&result.item_ids[1])
        .unwrap()
        .selection
        .is_none());
}

#[tokio::test]
async fn full_metadata_is_ready_without_creating_preparation_operations() {
    let store = test_store();
    let parent = completed_playlist(&store, vec![entry("ready", Some(1), true)]).await;
    let before = store.snapshot().unwrap().operations.len();
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "request-ready", vec![0]))
        .await
        .unwrap();
    let item = store.queue_item(&result.item_ids[0]).unwrap();
    assert!(preparations.is_empty());
    assert_eq!(item.preparation, None);
    assert_eq!(item.title, "Full ready");
    assert_eq!(store.snapshot().unwrap().operations.len(), before);
}

#[tokio::test]
async fn absent_or_changed_inspection_settings_disable_full_metadata_reuse() {
    let store = test_store();
    let absent = completed_playlist_with_fingerprint(
        &store,
        vec![entry("absent-context", Some(1), true)],
        None,
    )
    .await;
    let (_, absent_preparations) = store
        .add_playlist_items(playlist_input(&absent, "absent-context", vec![0]))
        .await
        .unwrap();
    assert_eq!(absent_preparations.len(), 1);

    let changed = completed_playlist_with_fingerprint(
        &store,
        vec![entry("changed-context", Some(1), true)],
        Some(crate::models::inspection_settings_fingerprint(
            Some(&crate::models::CookieConfig {
                enabled: true,
                mode: "browser".into(),
                browser: "firefox".into(),
                cookie_file: None,
            }),
            None,
        )),
    )
    .await;
    let (_, changed_preparations) = store
        .add_playlist_items(playlist_input(&changed, "changed-context", vec![0]))
        .await
        .unwrap();
    assert_eq!(changed_preparations.len(), 1);
}

#[tokio::test]
async fn full_metadata_with_changed_media_id_is_rejected_before_retention() {
    let store = test_store();
    let (parent, _) = store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    let mut mismatched = entry("retained-id", Some(1), true);
    mismatched.video.as_mut().unwrap().id = "different-id".into();
    let result = store
        .complete_inspection(
            &parent.id,
            UrlInspection::Playlist {
                playlist: PlaylistInfo {
                    title: "playlist".into(),
                    channel: None,
                    entry_count: 1,
                    truncated: false,
                    entries: vec![mismatched],
                    inspection_settings_fingerprint: Some(
                        crate::models::inspection_settings_fingerprint(None, None),
                    ),
                },
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        store.operation_state(&parent.id),
        Some(OperationState::Queued)
    );
}

#[tokio::test]
async fn malformed_duplicate_and_out_of_range_indices_are_atomic() {
    let store = test_store();
    let parent = completed_playlist(&store, vec![entry("one", Some(1), false)]).await;
    let baseline = store.snapshot().unwrap().queue.len();
    for (request, indices) in [
        ("duplicate", vec![0, 0]),
        ("range", vec![1]),
        ("empty", vec![]),
    ] {
        assert!(store
            .add_playlist_items(playlist_input(&parent, request, indices))
            .await
            .is_err());
        assert_eq!(store.snapshot().unwrap().queue.len(), baseline);
    }
}

#[tokio::test]
async fn playlist_commit_failure_publishes_no_rows_or_preparation_work() {
    let store = test_store();
    let parent = completed_playlist(&store, vec![entry("one", Some(1), false)]).await;
    let baseline = store.snapshot().unwrap();
    store.fail_persistence_for_test(3);
    assert!(store
        .add_playlist_items(playlist_input(&parent, "save-failure", vec![0]))
        .await
        .is_err());
    assert_eq!(store.snapshot().unwrap().queue.len(), baseline.queue.len());
    assert!(store.pending_operation_ids().is_empty());
}

#[tokio::test]
async fn hundred_row_playlist_admission_uses_exactly_one_journal_save_attempt() {
    let store = test_store();
    let entries = (0..100)
        .map(|index| entry(&format!("batch-{index}"), Some(index as u32 + 1), false))
        .collect();
    let parent = completed_playlist(&store, entries).await;
    store.reset_save_attempts_for_test();
    let (result, preparations) = store
        .add_playlist_items(playlist_input(
            &parent,
            "hundred-row-save",
            (0..100).collect(),
        ))
        .await
        .unwrap();
    assert_eq!(result.item_ids.len(), 100);
    assert_eq!(preparations.len(), 100);
    assert_eq!(store.save_attempts_for_test(), 1);
}

#[tokio::test]
async fn thousand_pending_finalizations_use_one_durable_save() {
    let store = test_store();
    let ids = admitted_pending_batch(&store, 1_000, "terminal-batch").await;
    store.reset_save_attempts_for_test();
    store
        .finalize_pending_batch(&ids, OperationState::Cancelled, None)
        .await
        .unwrap();
    assert_eq!(store.save_attempts_for_test(), 1);
    assert!(ids
        .iter()
        .all(|id| store.operation_state(id) == Some(OperationState::Cancelled)));
    assert_eq!(store.pending_operation_ids().len(), ids.len());
    assert!(store
        .finalize_pending_batch(&ids, OperationState::Cancelled, None)
        .await
        .unwrap()
        .is_empty());
    assert!(store.take_next_preparation().await.is_none());
}

#[tokio::test]
async fn pending_batch_persistence_failure_leaves_no_jobless_active_operations() {
    let store = test_store();
    let ids = admitted_pending_batch(&store, 40, "terminal-failure").await;
    store.fail_persistence_for_test(3);
    store
        .finalize_pending_batch(
            &ids,
            OperationState::Failed,
            Some(AppError::internal("admission publication failed")),
        )
        .await
        .unwrap();
    assert!(ids
        .iter()
        .all(|id| store.operation_state(id) == Some(OperationState::Failed)));
    assert_eq!(store.pending_operation_ids().len(), ids.len());
    assert!(store.take_next_preparation().await.is_none());
    assert!(store.snapshot().unwrap().persistence_health.degraded);
}

#[tokio::test]
async fn pending_batch_snapshot_remains_preterminal_until_blocked_save_publishes() {
    let store = test_store();
    let ids = admitted_pending_batch(&store, 3, "terminal-blocked").await;
    let pause = store.pause_next_journal_save_for_test();
    let task_store = store.clone();
    let task_ids = ids.clone();
    let task = tokio::spawn(async move {
        task_store
            .finalize_pending_batch(&task_ids, OperationState::Cancelled, None)
            .await
    });
    pause.wait_entered().await;
    assert!(ids
        .iter()
        .all(|id| store.operation_state(id) == Some(OperationState::Queued)));
    pause.release();
    task.await.unwrap().unwrap();
    assert!(ids
        .iter()
        .all(|id| store.operation_state(id) == Some(OperationState::Cancelled)));
}

#[tokio::test]
async fn known_no_audio_full_metadata_rejects_audio_only_batch() {
    let store = test_store();
    let mut no_audio = entry("silent-full", Some(1), true);
    no_audio.video.as_mut().unwrap().has_audio = false;
    let parent = completed_playlist(&store, vec![no_audio]).await;
    let mut input = playlist_input(&parent, "silent-full", vec![0]);
    input.format = "mp3".into();
    let error = store.add_playlist_items(input).await.unwrap_err();
    assert_eq!(
        error.summary,
        "Audio-only output is unavailable because this item has no audio stream."
    );
    assert!(store.snapshot().unwrap().queue.is_empty());
}

#[tokio::test]
async fn resolved_no_audio_metadata_stays_pending_for_audio_only_item() {
    let store = test_store();
    let selected = entry("silent-pending", None, false);
    let parent = completed_playlist(&store, vec![selected.clone()]).await;
    let mut input = playlist_input(&parent, "silent-pending", vec![0]);
    input.format = "flac".into();
    let (result, preparations) = store.add_playlist_items(input).await.unwrap();
    let resolved = VideoInfo {
        id: selected.id.clone(),
        title: "silent metadata".into(),
        duration: None,
        channel: None,
        thumbnail: None,
        url: "https://cdn.example.com/silent".into(),
        available_qualities: vec!["720p".into()],
        has_audio: false,
        selection: None,
    };
    let error = store
        .complete_inspection(&preparations[0], UrlInspection::Video { video: resolved })
        .await
        .unwrap_err();
    assert_eq!(
        error.summary,
        "Audio-only output is unavailable because this item has no audio stream."
    );
    let item = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.preparation, Some(QueuePreparation::Pending));
    assert_ne!(item.title, "silent metadata");
}

#[tokio::test]
async fn receipt_replays_after_restart_and_rejects_changed_payload() {
    let root = std::env::temp_dir().join(format!("playlist-replay-{}", uuid::Uuid::new_v4()));
    let journal = root.join("journal.dpapi");
    let diagnostics = root.join("diagnostics");
    let store = StateStore::open_at(journal.clone(), diagnostics.clone()).unwrap();
    let parent = completed_playlist(&store, vec![entry("one", Some(1), false)]).await;
    let input = playlist_input(&parent, "restart-request", vec![0]);
    let first = store.add_playlist_items(input.clone()).await.unwrap().0;
    drop(store);
    let reopened = StateStore::open_at(journal, diagnostics).unwrap();
    let replay = reopened
        .playlist_admission_receipt(&input)
        .unwrap()
        .unwrap();
    assert_eq!(replay.item_ids, first.item_ids);
    let mut changed = input;
    changed.quality = "1080p".into();
    assert!(reopened.playlist_admission_receipt(&changed).is_err());
}

#[tokio::test]
async fn linked_preparation_completion_installs_metadata_without_download_completion() {
    let store = test_store();
    let selected = entry("pending", Some(3), false);
    let parent = completed_playlist(&store, vec![selected.clone()]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "prepare", vec![0]))
        .await
        .unwrap();
    let operation_id = &preparations[0];
    let prepared = VideoInfo {
        id: "pending".into(),
        title: "Normalized title".into(),
        duration: Some(2.0),
        channel: None,
        thumbnail: None,
        url: "https://cdn.example.com/canonical-pending".into(),
        available_qualities: vec!["720p".into(), "1080p".into()],
        has_audio: true,
        selection: selected.selection,
    };
    store
        .complete_inspection(operation_id, UrlInspection::Video { video: prepared })
        .await
        .unwrap();
    let item = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.state, QueueItemState::Inert);
    assert_eq!(item.preparation, None);
    assert_eq!(item.title, "Normalized title");
    assert_eq!(item.source_url, selected.url);
    assert_eq!(
        store.operation_state(operation_id),
        Some(OperationState::Completed)
    );
}

#[tokio::test]
async fn linked_preparation_rejects_changed_media_id_without_mutating_item() {
    let store = test_store();
    let selected = entry("expected-id", Some(1), false);
    let parent = completed_playlist(&store, vec![selected.clone()]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "changed-id", vec![0]))
        .await
        .unwrap();
    let before = store.queue_item(&result.item_ids[0]).unwrap();
    let wrong = VideoInfo {
        id: "different-id".into(),
        title: "wrong metadata".into(),
        duration: None,
        channel: None,
        thumbnail: None,
        url: "https://cdn.example.com/redirect".into(),
        available_qualities: vec!["720p".into()],
        has_audio: true,
        selection: selected.selection,
    };
    assert!(store
        .complete_inspection(&preparations[0], UrlInspection::Video { video: wrong })
        .await
        .is_err());
    let after = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(after.title, before.title);
    assert_eq!(after.preparation, Some(QueuePreparation::Pending));
}

#[tokio::test]
async fn synthetic_url_id_binds_real_media_id_on_first_preparation() {
    let store = test_store();
    let mut synthetic = entry("synthetic", Some(1), false);
    synthetic.id = synthetic.url.clone();
    let parent = completed_playlist(&store, vec![synthetic.clone()]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "synthetic-id", vec![0]))
        .await
        .unwrap();
    assert!(store
        .queue_item(&result.item_ids[0])
        .unwrap()
        .source_media_id
        .is_none());
    let resolved = VideoInfo {
        id: "real-media-id".into(),
        title: "resolved".into(),
        duration: None,
        channel: None,
        thumbnail: None,
        url: "https://cdn.example.com/resolved".into(),
        available_qualities: vec!["720p".into()],
        has_audio: true,
        selection: synthetic.selection,
    };
    store
        .complete_inspection(&preparations[0], UrlInspection::Video { video: resolved })
        .await
        .unwrap();
    let item = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.source_media_id.as_deref(), Some("real-media-id"));
    assert_eq!(item.source_url, synthetic.url);
}

#[tokio::test]
async fn pending_preparation_blocks_edit_and_retries_inspection_after_failure() {
    let store = test_store();
    let parent = completed_playlist(&store, vec![entry("pending", None, false)]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "retry", vec![0]))
        .await
        .unwrap();
    let item_id = &result.item_ids[0];
    assert!(store
        .update_queue_item(
            item_id,
            UpdateQueueItemInput {
                quality: Some("1080p".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    store
        .finalize_operation(
            &preparations[0],
            OperationState::Failed,
            Some(AppError::internal("failed")),
        )
        .await
        .unwrap();
    let (retry, _) = store
        .enqueue(std::slice::from_ref(item_id), QueuePriority::Normal)
        .await
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(
        store.operation_kind(&retry[0].operation_id),
        Some(OperationKind::Inspection)
    );
    assert_eq!(
        store.queue_item(item_id).unwrap().preparation,
        Some(QueuePreparation::Pending)
    );
}

#[tokio::test]
async fn interrupted_preparation_stays_explicit_across_two_restarts() {
    let root = std::env::temp_dir().join(format!("playlist-two-restarts-{}", uuid::Uuid::new_v4()));
    let journal = root.join("journal.dpapi");
    let diagnostics = root.join("diagnostics");
    let store = StateStore::open_at(journal.clone(), diagnostics.clone()).unwrap();
    let parent = completed_playlist(&store, vec![entry("pending", Some(1), false)]).await;
    let result = store
        .add_playlist_items(playlist_input(&parent, "restart-twice", vec![0]))
        .await
        .unwrap()
        .0;
    let live = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(live.source_media_id.as_deref(), Some("pending"));
    assert_eq!(live.source_url, "https://example.com/pending");
    let preparation_id = live.latest_operation_id.clone().unwrap();
    assert!(live.preparation_operation_id.is_none());
    let durable = store.lock().unwrap().persistence_journal();
    let durable_item = durable
        .queue
        .iter()
        .find(|item| item.id == result.item_ids[0])
        .unwrap();
    assert!(durable_item.latest_operation_id.is_none());
    assert_eq!(
        durable_item.preparation_operation_id.as_deref(),
        Some(preparation_id.as_str())
    );
    drop(store);
    let first = StateStore::open_at(journal.clone(), diagnostics.clone()).unwrap();
    let item = first.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.state, QueueItemState::Interrupted);
    assert_eq!(item.preparation, Some(QueuePreparation::Pending));
    assert_eq!(
        item.latest_operation_id.as_deref(),
        Some(preparation_id.as_str())
    );
    assert!(item.preparation_operation_id.is_none());
    assert_eq!(item.source_media_id.as_deref(), Some("pending"));
    assert_eq!(item.source_url, "https://example.com/pending");
    drop(first);
    let second = StateStore::open_at(journal, diagnostics).unwrap();
    let item = second.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.state, QueueItemState::Interrupted);
    assert_eq!(item.preparation, Some(QueuePreparation::Pending));
    assert_eq!(item.source_media_id.as_deref(), Some("pending"));
    assert_eq!(item.source_url, "https://example.com/pending");
}

#[tokio::test]
async fn plain_child_media_identity_survives_restart_and_retry() {
    let root = std::env::temp_dir().join(format!("playlist-plain-retry-{}", uuid::Uuid::new_v4()));
    let journal = root.join("journal.dpapi");
    let diagnostics = root.join("diagnostics");
    let store = StateStore::open_at(journal.clone(), diagnostics.clone()).unwrap();
    let parent = completed_playlist(&store, vec![entry("plain-stable", None, false)]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "plain-stable", vec![0]))
        .await
        .unwrap();
    store
        .finalize_operation(
            &preparations[0],
            OperationState::Failed,
            Some(AppError::internal("retry")),
        )
        .await
        .unwrap();
    drop(store);
    let reopened = StateStore::open_at(journal, diagnostics).unwrap();
    let retry = reopened
        .enqueue(&result.item_ids, QueuePriority::Normal)
        .await
        .unwrap()
        .0;
    assert_eq!(retry.len(), 1);
    assert_eq!(
        retry[0].queue_item.source_media_id.as_deref(),
        Some("plain-stable")
    );
    assert_eq!(
        retry[0].queue_item.source_url,
        "https://example.com/plain-stable"
    );
    assert!(retry[0].queue_item.selection.is_none());
    assert_eq!(
        reopened.operation_kind(&retry[0].operation_id),
        Some(OperationKind::Inspection)
    );
}

#[tokio::test]
async fn playlist_capacity_failure_adds_nothing() {
    let store = test_store();
    let parent = completed_playlist(&store, vec![entry("overflow", Some(1), false)]).await;
    {
        let mut state = store.lock().unwrap();
        for index in 0..super::super::MAX_QUEUE_ITEMS {
            let id = uuid::Uuid::new_v4().to_string();
            state.queue_order.push(id.clone());
            state.queue.insert(
                id.clone(),
                crate::models::QueueItemRecord {
                    schema_version: APP_SCHEMA_VERSION,
                    id,
                    source_url: format!("https://example.com/existing-{index}"),
                    source_media_id: None,
                    title: "existing".into(),
                    available_qualities: vec!["720p".into()],
                    has_audio: true,
                    cookie_config: None,
                    format: "mp4".into(),
                    quality: "720p".into(),
                    output_dir: "C:\\Downloads".into(),
                    filename_override: None,
                    compat_config_path: None,
                    selection: None,
                    preparation: None,
                    preparation_operation_id: None,
                    state: QueueItemState::Inert,
                    latest_operation_id: None,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                }
                .into(),
            );
        }
    }
    assert!(store
        .add_playlist_items(playlist_input(&parent, "capacity", vec![0]))
        .await
        .is_err());
    assert_eq!(
        store.snapshot().unwrap().queue.len(),
        super::super::MAX_QUEUE_ITEMS
    );
}

#[tokio::test]
async fn late_cancelled_attempt_cannot_overwrite_retry_or_resurrect_removed_item() {
    let store = test_store();
    let selected = entry("late", Some(1), false);
    let parent = completed_playlist(&store, vec![selected.clone()]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "late-attempt", vec![0]))
        .await
        .unwrap();
    let item_id = result.item_ids[0].clone();
    let first = preparations[0].clone();
    store.request_cancellation(&first).await.unwrap();
    store
        .finalize_operation(&first, OperationState::Cancelled, None)
        .await
        .unwrap();
    let retry = store
        .enqueue(std::slice::from_ref(&item_id), QueuePriority::Normal)
        .await
        .unwrap()
        .0[0]
        .operation_id
        .clone();
    let late_video = VideoInfo {
        id: "late".into(),
        title: "late result".into(),
        duration: None,
        channel: None,
        thumbnail: None,
        url: selected.url.clone(),
        available_qualities: vec!["720p".into()],
        has_audio: true,
        selection: selected.selection.clone(),
    };
    assert!(store
        .complete_inspection(
            &first,
            UrlInspection::Video {
                video: late_video.clone()
            }
        )
        .await
        .is_err());
    assert_eq!(
        store
            .queue_item(&item_id)
            .unwrap()
            .latest_operation_id
            .as_deref(),
        Some(retry.as_str())
    );
    store.request_cancellation(&retry).await.unwrap();
    store
        .finalize_operation(&retry, OperationState::Cancelled, None)
        .await
        .unwrap();
    store
        .remove_queue_items(std::slice::from_ref(&item_id))
        .await
        .unwrap();
    assert!(store
        .complete_inspection(&retry, UrlInspection::Video { video: late_video })
        .await
        .is_err());
    assert!(store.queue_item(&item_id).is_err());
}

#[tokio::test]
async fn preparation_completion_save_failure_keeps_metadata_unready() {
    let store = test_store();
    let selected = entry("save-finalize", Some(1), false);
    let parent = completed_playlist(&store, vec![selected.clone()]).await;
    let (result, preparations) = store
        .add_playlist_items(playlist_input(&parent, "save-finalize", vec![0]))
        .await
        .unwrap();
    store.fail_persistence_for_test(3);
    let prepared = VideoInfo {
        id: "save-finalize".into(),
        title: "must not publish".into(),
        duration: None,
        channel: None,
        thumbnail: None,
        url: selected.url,
        available_qualities: vec!["720p".into()],
        has_audio: true,
        selection: selected.selection,
    };
    store
        .complete_inspection(&preparations[0], UrlInspection::Video { video: prepared })
        .await
        .unwrap();
    let item = store.queue_item(&result.item_ids[0]).unwrap();
    assert_eq!(item.preparation, Some(QueuePreparation::Pending));
    assert_ne!(item.title, "must not publish");
    assert_eq!(item.state, QueueItemState::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_identical_playlist_requests_create_one_durable_batch() {
    let store = test_store();
    let parent = completed_playlist(
        &store,
        vec![
            entry("concurrent-first", Some(1), false),
            entry("concurrent-second", Some(2), false),
        ],
    )
    .await;
    let input = playlist_input(&parent, "concurrent-request", vec![0, 1]);
    store.reset_save_attempts_for_test();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let first_store = store.clone();
    let first_input = input.clone();
    let first_barrier = barrier.clone();
    let first = tokio::spawn(async move {
        first_barrier.wait().await;
        first_store.add_playlist_items(first_input).await
    });
    let second_store = store.clone();
    let second_barrier = barrier.clone();
    let second = tokio::spawn(async move {
        second_barrier.wait().await;
        second_store.add_playlist_items(input).await
    });
    barrier.wait().await;

    let (first_receipt, first_preparations) = first.await.unwrap().unwrap();
    let (second_receipt, second_preparations) = second.await.unwrap().unwrap();
    assert_eq!(first_receipt.item_ids, second_receipt.item_ids);
    assert!(first_preparations.is_empty() || second_preparations.is_empty());
    assert_eq!(first_preparations.len() + second_preparations.len(), 2);
    assert_eq!(store.snapshot().unwrap().queue.len(), 2);
    assert_eq!(store.save_attempts_for_test(), 1);
}
