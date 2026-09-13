use crate::models::{
    AddQueueItemInput, MediaSelection, OperationKind, PlaylistAdmissionInput, PlaylistEntry,
    PlaylistInfo, QueuePreparation, UrlInspection,
};
use crate::outbox::{MAX_OUTBOX_BATCHES, MAX_OUTBOX_DELTAS, MAX_OUTBOX_ESTIMATED_BYTES};
use crate::services::test_support::started_backend;
use crate::services::Backend;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

fn synthetic_entry(index: usize) -> PlaylistEntry {
    PlaylistEntry {
        id: format!("synthetic-{index}"),
        title: Some(format!("Synthetic entry {index}")),
        duration: None,
        url: format!("https://example.invalid/playlist/{index}"),
        thumbnail: None,
        selection: Some(MediaSelection {
            entry_id: format!("synthetic-{index}"),
            extractor_key: "Synthetic".into(),
            playlist_index: u32::try_from(index + 1).unwrap(),
        }),
        // Deliberately absent: admission must durably register the row without
        // waiting for per-entry metadata preparation.
        video: None,
    }
}

async fn playlist_input(
    backend: &Backend,
    root: &Path,
    count: usize,
    request_id: &str,
) -> AddQueueItemInput {
    let entries = (0..count).map(synthetic_entry).collect::<Vec<_>>();
    let (parent, _) = backend
        .state_store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    backend
        .state_store
        .complete_inspection(
            &parent.id,
            UrlInspection::Playlist {
                playlist: PlaylistInfo {
                    inspection_settings_fingerprint: None,
                    title: "Synthetic playlist".into(),
                    channel: Some("local fixture".into()),
                    entry_count: count,
                    truncated: false,
                    entries,
                },
            },
        )
        .await
        .unwrap();
    AddQueueItemInput {
        inspection_operation_id: parent.id,
        format: "mp4".into(),
        quality: "720p".into(),
        output_dir: root.to_string_lossy().into_owned(),
        cookie_config: None,
        filename_override: None,
        compat_config_path: None,
        playlist: Some(PlaylistAdmissionInput {
            request_id: request_id.into(),
            entry_indices: (0..count).collect(),
        }),
    }
}

async fn admit_synthetic_playlist(
    backend: &Backend,
    root: &Path,
    count: usize,
    request_id: &str,
) -> (Duration, Vec<String>, Vec<String>) {
    let input = playlist_input(backend, root, count, request_id).await;
    let started = Instant::now();
    let admission = backend
        .download_manager
        .begin_job_admission(count)
        .await
        .unwrap();
    let (receipt, pending_ids) = backend.state_store.add_playlist_items(input).await.unwrap();
    admission.publish_subset(&pending_ids).await.unwrap();
    (started.elapsed(), receipt.item_ids, pending_ids)
}

async fn assert_admitted_snapshot(backend: &Backend, item_ids: &[String], pending_ids: &[String]) {
    let snapshot = backend.state_store.snapshot().unwrap();
    assert_eq!(snapshot.queue.len(), item_ids.len());
    assert!(item_ids
        .iter()
        .all(|id| snapshot.queue.iter().any(|item| {
            item.id == *id && item.preparation == Some(QueuePreparation::Pending)
        })));
    let receipt_pending = item_ids
        .iter()
        .map(|id| {
            snapshot
                .queue
                .iter()
                .find(|item| item.id == *id)
                .and_then(|item| item.latest_operation_id.clone())
                .expect("each admitted receipt row must reference its preparation operation")
        })
        .collect::<HashSet<_>>();
    assert_eq!(receipt_pending, pending_ids.iter().cloned().collect());
    let durable_pending = snapshot
        .operations
        .iter()
        .filter(|operation| pending_ids.contains(&operation.id))
        .map(|operation| operation.id.clone())
        .collect::<HashSet<_>>();
    assert_eq!(durable_pending, pending_ids.iter().cloned().collect());
    assert_eq!(
        backend
            .download_manager
            .active_ids()
            .await
            .into_iter()
            .collect::<HashSet<_>>(),
        pending_ids.iter().cloned().collect()
    );

    let outbox = backend.state_store.outbox_stats();
    assert!(outbox.queued_batches <= MAX_OUTBOX_BATCHES);
    assert!(outbox.queued_deltas <= MAX_OUTBOX_DELTAS);
    assert!(outbox.estimated_bytes <= MAX_OUTBOX_ESTIMATED_BYTES);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_rows_publish_atomically_after_the_durable_save() {
    let (backend, root) = started_backend("playlist-durable-publication").await;
    let input = playlist_input(&backend, &root, 3, "durable-publication").await;
    let before = backend.state_store.snapshot().unwrap();
    let pause = backend.state_store.pause_next_journal_save_for_test();
    let task_store = backend.state_store.clone();
    let admission = backend
        .download_manager
        .begin_job_admission(3)
        .await
        .unwrap();
    let write = tokio::spawn(async move { task_store.add_playlist_items(input).await });

    pause.wait_entered().await;
    let while_save_is_blocked = backend.state_store.snapshot().unwrap();
    assert_eq!(while_save_is_blocked.queue.len(), before.queue.len());
    assert_eq!(
        while_save_is_blocked.latest_sequence,
        before.latest_sequence
    );

    pause.release();
    let (receipt, pending_ids) = write.await.unwrap().unwrap();
    admission.publish_subset(&pending_ids).await.unwrap();
    assert_eq!(pending_ids.len(), 3);
    assert_admitted_snapshot(&backend, &receipt.item_ids, &pending_ids).await;
    for id in pending_ids {
        backend.download_manager.finish(&id).await;
    }
    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn playlist_admission_registers_every_row_before_metadata_preparation() {
    let (backend, root) = started_backend("playlist-register-first").await;
    let (_elapsed, item_ids, pending_ids) =
        admit_synthetic_playlist(&backend, &root, 4, "register-first").await;
    assert_admitted_snapshot(&backend, &item_ids, &pending_ids).await;
    for id in pending_ids {
        backend.download_manager.finish(&id).await;
    }
    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

// Synthetic disk-and-coordinator benchmark. It performs no network requests and
// makes no claim about live extractor, rendering, or end-to-end UI performance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "controlled playlist admission performance harness"]
async fn synthetic_playlist_admission_meets_durable_registration_gates() {
    for (count, gate) in [
        (1usize, Duration::from_secs(1)),
        (100, Duration::from_secs(1)),
        (1_000, Duration::from_secs(2)),
    ] {
        let (backend, root) = started_backend(&format!("playlist-perf-{count}")).await;
        let (elapsed, item_ids, pending_ids) =
            admit_synthetic_playlist(&backend, &root, count, &format!("perf-{count}")).await;
        assert_admitted_snapshot(&backend, &item_ids, &pending_ids).await;
        eprintln!(
            "synthetic_playlist_admission count={count} elapsed_ms={} gate_ms={}",
            elapsed.as_millis(),
            gate.as_millis()
        );
        let met_gate = elapsed <= gate;
        for id in pending_ids {
            backend.download_manager.finish(&id).await;
        }
        backend.download_manager.begin_shutdown().await;
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            met_gate,
            "synthetic durable admission for {count} rows took {elapsed:?}, gate {gate:?}"
        );
    }
}

#[tokio::test]
async fn immediate_removal_cancels_registered_preparation_before_a_worker_claim() {
    let (backend, root) = started_backend("playlist-remove-pending").await;
    let (_, item_ids, pending_ids) = admit_synthetic_playlist(&backend, &root, 3, "remove").await;
    crate::services::queue::remove_queue_items(backend.clone(), item_ids)
        .await
        .unwrap();
    assert!(backend.state_store.snapshot().unwrap().queue.is_empty());
    assert!(backend.download_manager.active_ids().await.is_empty());
    assert!(backend.state_store.pending_operation_ids().is_empty());
    for id in pending_ids {
        assert_eq!(
            backend.state_store.operation_state(&id),
            Some(crate::models::OperationState::Cancelled)
        );
    }
    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_claimed_preparation_waiting_for_the_inspection_permit_releases_it() {
    let (backend, root) = started_backend("playlist-remove-claimed").await;
    let permit_owner = crate::downloader::process::DownloadJob::new().unwrap();
    let permit = backend
        .download_manager
        .acquire_inspection(&permit_owner)
        .await
        .unwrap()
        .unwrap();
    let (_, item_ids, _) = admit_synthetic_playlist(&backend, &root, 1, "remove-claimed").await;
    super::spawn_preparation_worker(&backend.state_store, &backend.download_manager).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !backend.state_store.pending_operation_ids().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        crate::services::queue::remove_queue_items(backend.clone(), item_ids),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(backend.download_manager.active_ids().await.is_empty());
    assert!(backend.state_store.snapshot().unwrap().queue.is_empty());
    drop(permit);
    backend.download_manager.begin_shutdown().await;
    backend
        .download_manager
        .wait_for_shutdown_tasks(Duration::from_secs(2))
        .await
        .unwrap();
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_all_releases_a_thousand_preparations_and_reopens_admission() {
    let (backend, root) = started_backend("playlist-cancel-all").await;
    let (_, _, pending_ids) = admit_synthetic_playlist(&backend, &root, 1_000, "cancel-all").await;
    backend.state_store.reset_save_attempts_for_test();
    let progress: crate::notifications::DownloadProgressSink = std::sync::Arc::new(|_| {
        panic!("metadata cancellation must not emit download progress");
    });
    let result = crate::services::operations::cancel_all_downloads(backend.clone(), progress)
        .await
        .unwrap();
    assert!(result.idle);
    assert!(result.remaining_operation_ids.is_empty());
    assert!(backend.state_store.pending_operation_ids().is_empty());
    assert_eq!(backend.state_store.save_attempts_for_test(), 1);
    let snapshot = backend.state_store.snapshot().unwrap();
    assert!(snapshot
        .queue
        .iter()
        .all(|item| item.state == crate::models::QueueItemState::Cancelled));
    assert!(pending_ids.iter().all(|id| backend
        .state_store
        .operation_state(id)
        .is_none_or(|state| state.is_terminal())));
    drop(
        backend
            .download_manager
            .begin_job_admission(1)
            .await
            .unwrap(),
    );
    backend.download_manager.begin_shutdown().await;
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_between_durable_admission_and_registration_compensates_as_one_batch() {
    let (backend, root) = started_backend("playlist-admission-shutdown").await;
    let input = playlist_input(&backend, &root, 1_000, "shutdown").await;
    let admission = backend
        .download_manager
        .begin_job_admission(1_000)
        .await
        .unwrap();
    let (_, ids) = backend.state_store.add_playlist_items(input).await.unwrap();
    let cleanup = crate::lifecycle_cleanup::QueueAdmissionGuard::new(
        backend.state_store.clone(),
        backend.download_manager.clone(),
        ids.clone(),
    );
    backend.state_store.reset_save_attempts_for_test();
    backend.download_manager.begin_shutdown().await;
    let error = admission
        .publish_subset(&ids)
        .await
        .err()
        .expect("shutdown must reject registration");
    tokio::time::timeout(Duration::from_secs(15), cleanup.finalize(error))
        .await
        .unwrap();
    assert_eq!(backend.state_store.save_attempts_for_test(), 1);
    assert!(backend.download_manager.active_ids().await.is_empty());
    assert!(backend.state_store.pending_operation_ids().is_empty());
    assert!(backend
        .state_store
        .snapshot()
        .unwrap()
        .queue
        .iter()
        .all(|item| item.state == crate::models::QueueItemState::Failed));
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}
