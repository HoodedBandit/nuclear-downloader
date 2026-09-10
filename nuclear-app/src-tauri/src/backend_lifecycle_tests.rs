use crate::app_error::AppError;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::models::{
    self, AddQueueItemInput, DownloadProgress, OperationKind, OperationState, QueueItemRecord,
    QueuePriority, RuntimeReadiness, UrlInspection, VideoInfo,
};
use crate::scheduling::run_registered_download;
use crate::services::commands::run_tracked_command;
use crate::services::inspection::finalize_inspection_result;
use crate::state::{self, StateStore};
use crate::{downloader, outbox};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 5)]
async fn five_progress_producers_publish_one_contiguous_authoritative_stream() {
    let root =
        std::env::temp_dir().join(format!("nuclear-event-producers-{}", uuid::Uuid::new_v4()));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let mut item_ids = Vec::new();
    for _ in 0..5 {
        item_ids.push(fixture(&store, &root).await.id);
    }
    let (operations, _) = store
        .enqueue(&item_ids, models::QueuePriority::Normal)
        .await
        .unwrap();
    let reader = store.take_outbox_reader().unwrap();
    while reader.try_recv().is_some() {}
    let initial_sequence = store.snapshot().unwrap().latest_sequence;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
    let mut producers = Vec::new();
    for operation in operations {
        let producer_store = store.clone();
        let producer_barrier = barrier.clone();
        producers.push(tokio::spawn(async move {
            for round in 1..=20 {
                producer_barrier.wait().await;
                producer_store
                    .apply_download_progress(
                        &operation.operation_id,
                        &DownloadProgress {
                            download_id: operation.operation_id.clone(),
                            status: "downloading".into(),
                            progress: f64::from(round),
                            phase: Some("download".into()),
                            download_progress: Some(f64::from(round)),
                            conversion_progress: None,
                            speed: None,
                            eta: None,
                            error: None,
                            error_code: None,
                            error_detail: None,
                            filename: None,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                producer_barrier.wait().await;
            }
        }));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        for producer in producers {
            producer.await.unwrap();
        }
    })
    .await
    .unwrap();
    let mut observed_sequence = initial_sequence;
    let mut delta_count = 0;
    while let Some(publication) = reader.try_recv() {
        let outbox::StatePublication::Deltas(deltas) = publication else {
            panic!("The controlled workload fits within the outbox budget.");
        };
        for delta in deltas.iter() {
            observed_sequence += 1;
            assert_eq!(delta.sequence, observed_sequence);
            delta_count += 1;
        }
    }
    assert_eq!(delta_count, 200);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.latest_sequence, observed_sequence);
    assert_eq!(
        snapshot
            .operations
            .iter()
            .filter(
                |operation| operation.kind == OperationKind::Download && operation.progress == 20.0
            )
            .count(),
        5
    );
    assert_eq!(reader.stats().coalesced_resyncs, 0);
    drop(reader);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn rejected_inspection_metadata_finishes_the_registered_operation() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-inspection-finalize-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let manager = coordinator().await;
    let admission = manager.begin_job_admission(1).await.unwrap();
    let (operation, _) = store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    admission
        .publish(std::slice::from_ref(&operation.id))
        .await
        .unwrap();
    store
        .set_operation_state(&operation.id, OperationState::Running, None)
        .await
        .unwrap();

    finalize_inspection_result(
        &store,
        &operation.id,
        Ok(Some(UrlInspection::Video {
            video: VideoInfo {
                id: "fixture".into(),
                title: "Oversized URL fixture".into(),
                duration: None,
                channel: None,
                thumbnail: None,
                url: format!("https://fixture.invalid/{}", "x".repeat(4096)),
                available_qualities: vec!["720p".into()],
                has_audio: true,
                selection: None,
            },
        })),
    )
    .await
    .unwrap();
    manager.finish(&operation.id).await;
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.operations[0].state, OperationState::Failed);
    assert_eq!(
        snapshot.operations[0].error.as_ref().unwrap().code,
        "field_too_large"
    );
    assert!(manager.active_ids().await.is_empty());
    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn panic_after_durable_inspection_before_publish_is_cleaned_up() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-inspection-admission-panic-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let manager = coordinator().await;
    let admission = manager.begin_job_admission(1).await.unwrap();
    let (operation, _) = store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    let operation_id = operation.id;
    let cleanup = crate::lifecycle_cleanup::InspectionAdmissionGuard::new(
        store.clone(),
        manager.clone(),
        operation_id.clone(),
    );

    let panic = AssertUnwindSafe(async move {
        let _admission = admission;
        let _cleanup = cleanup;
        panic!("injected panic after durable inspection before publication");
    })
    .catch_unwind()
    .await;
    assert!(panic.is_err());

    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(2))
        .await
        .unwrap();
    assert!(manager.active_ids().await.is_empty());
    let operation = store
        .snapshot()
        .unwrap()
        .operations
        .into_iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(
        operation.error.as_ref().map(|error| error.summary.as_str()),
        Some("The inspection admission stopped unexpectedly.")
    );

    drop(store);
    let reopened = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    assert_eq!(
        reopened.operation_state(&operation_id),
        Some(OperationState::Failed)
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

pub(crate) async fn coordinator() -> DownloadManager {
    let manager = DownloadManager::new(1, 1, 1);
    let shutdown = manager.shutdown_token();
    manager
        .spawn_tracked(TrackedTaskKind::Worker, async move {
            shutdown.cancelled().await
        })
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    manager
        .spawn_startup(async { Ok(()) }, move |result| {
            let _ = sender.send(result);
        })
        .unwrap();
    receiver.await.unwrap().unwrap();
    manager
}

#[tokio::test]
async fn tracked_command_waits_for_startup_and_preserves_state_on_startup_failure() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-startup-command-gate-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let initial = store.snapshot().unwrap();
    let manager = DownloadManager::new(1, 1, 1);
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (finish_sender, finish_receiver) = tokio::sync::oneshot::channel();
    manager
        .spawn_startup(
            async move {
                let _ = started_sender.send(());
                let _ = finish_receiver.await;
                Err(AppError::internal("Startup recovery failed."))
            },
            |_| {},
        )
        .unwrap();
    started_receiver.await.unwrap();

    let command_manager = manager.clone();
    let command_store = store.clone();
    let command_body_ran = Arc::new(AtomicBool::new(false));
    let command_body_signal = command_body_ran.clone();
    let command = tokio::spawn(async move {
        run_tracked_command(&command_manager, TrackedTaskKind::Admission, async move {
            command_body_signal.store(true, Ordering::SeqCst);
            command_store
                .set_runtime_readiness(RuntimeReadiness::Ready)
                .await?;
            Ok(())
        })
        .await
    });
    tokio::task::yield_now().await;
    assert!(!command.is_finished());
    assert!(!command_body_ran.load(Ordering::SeqCst));
    let blocked = store.snapshot().unwrap();
    assert_eq!(blocked.latest_sequence, initial.latest_sequence);
    assert_eq!(blocked.runtime_readiness, initial.runtime_readiness);
    assert_eq!(blocked.queue.len(), initial.queue.len());
    assert_eq!(blocked.operations.len(), initial.operations.len());

    finish_sender.send(()).unwrap();
    let error = command.await.unwrap().unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.summary, "Startup recovery failed.");
    assert!(!command_body_ran.load(Ordering::SeqCst));
    let after_failure = store.snapshot().unwrap();
    assert_eq!(after_failure.latest_sequence, initial.latest_sequence);
    assert_eq!(after_failure.runtime_readiness, initial.runtime_readiness);
    assert_eq!(after_failure.queue.len(), initial.queue.len());
    assert_eq!(after_failure.operations.len(), initial.operations.len());

    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

pub(crate) async fn fixture(store: &StateStore, root: &std::path::Path) -> QueueItemRecord {
    let (inspection, _) = store
        .begin_operation(OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(
            &inspection.id,
            UrlInspection::Video {
                video: VideoInfo {
                    id: "fixture".into(),
                    title: "Lifecycle fixture".into(),
                    duration: None,
                    channel: None,
                    thumbnail: None,
                    url: "https://example.com/fixture".into(),
                    available_qualities: vec!["720p".into()],
                    has_audio: true,
                    selection: None,
                },
            },
        )
        .await
        .unwrap();
    store
        .add_queue_item(AddQueueItemInput {
            inspection_operation_id: inspection.id,
            quality: "720p".into(),
            format: "mp4".into(),
            output_dir: root.to_string_lossy().into_owned(),
            cookie_config: None,
            filename_override: None,
            compat_config_path: None,
        })
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn cancellation_after_dequeue_before_lookup_never_calls_the_process_factory() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-worker-cancellation-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let item = fixture(&store, &root).await;
    let manager = coordinator().await;
    let admission = manager.begin_job_admission(1).await.unwrap();
    let (work, _) = store
        .enqueue(&[item.id], QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    admission
        .publish(std::slice::from_ref(&operation_id))
        .await
        .unwrap();
    let claim = manager.wait_worker_claim().await.unwrap().unwrap();
    let work = store.take_next_pending().await.unwrap();

    // The claim is still held, at precisely the old dequeue/registration gap.
    tokio::time::timeout(Duration::from_secs(1), manager.cancel(&operation_id))
        .await
        .unwrap()
        .unwrap();
    store.request_cancellation(&operation_id).await.unwrap();
    assert!(!store.cancel_pending(&operation_id).await);
    let job = claim.registered_job(&operation_id).await.unwrap();
    drop(claim);
    let starts = AtomicUsize::new(0);
    let outcome = run_registered_download(&store, &manager, &work.operation_id, job, |_| async {
        starts.fetch_add(1, Ordering::SeqCst);
        downloader::DownloadOutcome::Completed { filename: None }
    })
    .await;
    assert_eq!(outcome, downloader::DownloadOutcome::Cancelled);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    store
        .finalize_download(&operation_id, state::DownloadTerminalOutcome::Cancelled)
        .await
        .unwrap();
    manager.finish(&operation_id).await;
    assert_eq!(
        store.operation_state(&operation_id),
        Some(OperationState::Cancelled)
    );
    assert!(manager.active_ids().await.is_empty());
    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn dropping_an_admission_response_does_not_abandon_committed_work_registration() {
    let root =
        std::env::temp_dir().join(format!("nuclear-admission-caller-{}", uuid::Uuid::new_v4()));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let item = fixture(&store, &root).await;
    let manager = coordinator().await;
    let (entered_sender, entered) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let (completed_sender, completed) = tokio::sync::oneshot::channel();
    let command_manager = manager.clone();
    let command_store = store.clone();
    let caller = tokio::spawn(async move {
        let task_manager = command_manager.clone();
        run_tracked_command(&command_manager, TrackedTaskKind::Admission, async move {
            let admission = task_manager.begin_job_admission(1).await?;
            let (work, _) = command_store
                .enqueue(&[item.id], QueuePriority::Normal)
                .await?;
            let operation_id = work[0].operation_id.clone();
            let _ = entered_sender.send(());
            let _ = released.await;
            admission
                .publish(std::slice::from_ref(&operation_id))
                .await?;
            let _ = completed_sender.send(operation_id);
            Ok(())
        })
        .await
    });
    entered.await.unwrap();
    caller.abort();
    release.send(()).unwrap();
    let operation_id = tokio::time::timeout(Duration::from_secs(2), completed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manager.active_ids().await, vec![operation_id.clone()]);
    manager.cancel(&operation_id).await.unwrap();
    assert!(store.cancel_pending(&operation_id).await);
    store
        .finalize_download(&operation_id, state::DownloadTerminalOutcome::Cancelled)
        .await
        .unwrap();
    manager.finish(&operation_id).await;
    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
