use super::StateStore;
use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
use crate::journal::{JournalStore, TestJournalSavePause, MAX_TERMINAL_ATTEMPTS};
use crate::models::{
    AddQueueItemInput, OperationState, PlaylistEntry, PlaylistInfo, QueueItemState, QueuePriority,
    RuntimeReadiness, UpdateQueueItemInput, UrlInspection, VideoInfo,
};
use crate::outbox::StateOutboxStats;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

pub(crate) struct TestCommitPause {
    entered: Notify,
    release: Notify,
}

impl TestCommitPause {
    pub(crate) async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

impl StateStore {
    pub fn open_at(journal_path: PathBuf, diagnostics_path: PathBuf) -> Result<Self, AppError> {
        let diagnostics = Diagnostics::open(diagnostics_path)?;
        let (journal, loaded, quarantine) = JournalStore::open(journal_path)?;
        if quarantine.is_some() {
            diagnostics.log(
                "error",
                "journal_quarantined",
                &uuid::Uuid::new_v4().to_string(),
                "A corrupt journal was quarantined and replaced with an empty journal.",
            );
        }
        Ok(Self::from_parts(journal, loaded, diagnostics))
    }

    pub(crate) fn outbox_stats(&self) -> StateOutboxStats {
        self.inner.outbox.stats()
    }

    pub async fn begin_maintenance_operation(
        &self,
        kind: crate::models::OperationKind,
    ) -> Result<
        (
            crate::models::OperationSnapshot,
            Vec<crate::models::StateDelta>,
        ),
        AppError,
    > {
        self.begin_maintenance_operation_with_id(kind, uuid::Uuid::new_v4().to_string())
            .await
    }

    fn fail_next_persistence_for_test(&self) {
        self.inner.journal.fail_next_save_for_test();
    }

    pub(crate) fn fail_persistence_for_test(&self, count: usize) {
        self.inner.journal.fail_saves_for_test(count);
    }

    fn fail_next_finalizer_task_for_test(&self) {
        self.inner
            .fail_next_finalizer_task
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn fail_next_finalizer_after_save_for_test(&self) {
        self.inner
            .fail_next_finalizer_after_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn pause_next_commit_for_test(&self) -> Arc<TestCommitPause> {
        let pause = Arc::new(TestCommitPause {
            entered: Notify::new(),
            release: Notify::new(),
        });
        *self.inner.commit_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    pub(crate) fn pause_next_journal_save_for_test(&self) -> Arc<TestJournalSavePause> {
        self.inner.journal.pause_next_save_for_test()
    }

    pub(super) async fn pause_commit_for_test(&self) {
        let pause = self.inner.commit_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }
}

fn test_store() -> StateStore {
    let root = std::env::temp_dir().join(format!("nuclear-state-{}", uuid::Uuid::new_v4()));
    StateStore::open_at(root.join("journal.dpapi"), root.join("diagnostics")).unwrap()
}

fn video(index: usize) -> VideoInfo {
    VideoInfo {
        id: format!("video-{index}"),
        title: format!("Video {index}"),
        duration: None,
        channel: None,
        thumbnail: None,
        url: format!("https://example.com/{index}"),
        available_qualities: vec!["720p".to_string()],
        has_audio: true,
    }
}

fn large_inspection() -> UrlInspection {
    let large_id = "i".repeat(4_000);
    let large_url = "u".repeat(4_000);
    let title = "t".repeat(1_000);
    UrlInspection::Playlist {
        playlist: PlaylistInfo {
            title: "Large retained inspection".to_string(),
            channel: None,
            entry_count: 1_000,
            truncated: false,
            entries: (0..1_000)
                .map(|_| PlaylistEntry {
                    id: large_id.clone(),
                    title: Some(title.clone()),
                    duration: None,
                    url: large_url.clone(),
                    thumbnail: None,
                })
                .collect(),
        },
    }
}

fn input(inspection_operation_id: String) -> AddQueueItemInput {
    AddQueueItemInput {
        inspection_operation_id,
        format: "mp4".to_string(),
        quality: "720p".to_string(),
        output_dir: "C:\\Downloads".to_string(),
        cookie_config: None,
        filename_override: None,
        compat_config_path: None,
    }
}

async fn add_item(
    store: &StateStore,
    index: usize,
) -> (
    crate::models::QueueItemRecord,
    Vec<crate::models::StateDelta>,
) {
    let (operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(
            &operation.id,
            crate::models::UrlInspection::Video {
                video: video(index),
            },
        )
        .await
        .unwrap();
    store.add_queue_item(input(operation.id)).await.unwrap()
}

#[tokio::test]
async fn sequence_is_monotonic_and_snapshot_reports_latest() {
    let store = test_store();
    let (_, first) = add_item(&store, 1).await;
    let (_, second) = add_item(&store, 2).await;
    let snapshot = store.snapshot().unwrap();

    assert!(second.last().unwrap().sequence > first.last().unwrap().sequence);
    assert_eq!(snapshot.latest_sequence, second.last().unwrap().sequence);
    assert_eq!(snapshot.queue.len(), 2);
    assert_eq!(snapshot.runtime_readiness, RuntimeReadiness::RepairRequired);
}

#[tokio::test]
async fn queue_add_consumes_only_authoritative_completed_video_inspections() {
    let store = test_store();
    assert_eq!(
        store
            .add_queue_item(input(uuid::Uuid::new_v4().to_string()))
            .await
            .unwrap_err()
            .code,
        "not_found"
    );

    let (pending, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .add_queue_item(input(pending.id.clone()))
            .await
            .unwrap_err()
            .code,
        "inspection_not_completed"
    );

    let (wrong_kind, _) = store
        .begin_operation(crate::models::OperationKind::Download, None)
        .await
        .unwrap();
    store
        .set_operation_state(&wrong_kind.id, OperationState::Completed, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .add_queue_item(input(wrong_kind.id))
            .await
            .unwrap_err()
            .code,
        "invalid_inspection_operation"
    );

    store
        .set_operation_state(&pending.id, OperationState::Cancelled, None)
        .await
        .unwrap();
    let (playlist, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(
            &playlist.id,
            crate::models::UrlInspection::Playlist {
                playlist: crate::models::PlaylistInfo {
                    title: "Playlist".to_string(),
                    channel: None,
                    entry_count: 0,
                    truncated: false,
                    entries: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .add_queue_item(input(playlist.id))
            .await
            .unwrap_err()
            .code,
        "inspection_result_kind"
    );

    let (video_operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(
            &video_operation.id,
            crate::models::UrlInspection::Video { video: video(42) },
        )
        .await
        .unwrap();
    let (item, deltas) = store
        .add_queue_item(input(video_operation.id.clone()))
        .await
        .unwrap();

    assert_eq!(item.source_url, "https://example.com/42");
    assert!(store.operation_state(&video_operation.id).is_none());
    assert!(deltas.iter().any(|delta| {
        matches!(
            &delta.delta,
            crate::models::StateDeltaValue::OperationRemoved(id) if id == &video_operation.id
        )
    }));
}

#[tokio::test]
async fn retry_preserves_item_identity_and_creates_a_new_attempt() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (first, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    store
        .set_operation_state(&first[0].operation_id, OperationState::Failed, None)
        .await
        .unwrap();
    let (second, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();

    assert_eq!(second[0].queue_item.id, item.id);
    assert_ne!(second[0].operation_id, first[0].operation_id);
}

#[tokio::test]
async fn active_items_cannot_be_edited_or_removed() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();

    assert!(store
        .update_queue_item(&item.id, UpdateQueueItemInput::default())
        .await
        .is_err());
    assert!(store
        .remove_queue_items(std::slice::from_ref(&item.id))
        .await
        .is_err());
}

#[tokio::test]
async fn failed_durable_mutations_roll_back_memory_and_pending_work() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let before = store.snapshot().unwrap();
    store.fail_next_persistence_for_test();

    let error = store
        .update_queue_item(
            &item.id,
            UpdateQueueItemInput {
                quality: Some("1080p".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(error.summary.contains("Injected"));
    let after_update = store.snapshot().unwrap();
    assert_eq!(after_update.latest_sequence, before.latest_sequence);
    assert_eq!(after_update.queue[0].quality, before.queue[0].quality);

    store.fail_next_persistence_for_test();
    assert!(store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .is_err());
    let after_enqueue = store.snapshot().unwrap();
    assert_eq!(after_enqueue.latest_sequence, before.latest_sequence);
    assert_eq!(after_enqueue.operations.len(), before.operations.len());
    assert!(store.take_next_pending().await.is_none());
}

#[cfg(windows)]
#[tokio::test]
async fn removal_detaches_terminal_history_and_reopens_without_quarantine() {
    let root = std::env::temp_dir().join(format!("nuclear-remove-reopen-{}", uuid::Uuid::new_v4()));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let (removed_item, _) = add_item(&store, 1).await;
    let (retained_item, _) = add_item(&store, 2).await;
    let (work, _) = store
        .enqueue(
            std::slice::from_ref(&removed_item.id),
            QueuePriority::Normal,
        )
        .await
        .unwrap();
    store
        .set_operation_state(&work[0].operation_id, OperationState::Completed, None)
        .await
        .unwrap();

    let deltas = store
        .remove_queue_items(std::slice::from_ref(&removed_item.id))
        .await
        .unwrap();
    assert!(deltas.iter().any(|delta| matches!(
        &delta.delta,
        crate::models::StateDeltaValue::OperationUpserted(operation)
            if operation.id == work[0].operation_id && operation.queue_item_id.is_none()
    )));
    drop(store);

    let reopened = StateStore::open_at(journal_path, diagnostics_path).unwrap();
    let snapshot = reopened.snapshot().unwrap();
    assert_eq!(snapshot.queue.len(), 1);
    assert_eq!(snapshot.queue[0].id, retained_item.id);
    assert!(snapshot.operations.iter().any(|operation| {
        operation.id == work[0].operation_id && operation.queue_item_id.is_none()
    }));
    drop(reopened);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn first_mutation_after_high_revision_restart_is_persisted() {
    let root =
        std::env::temp_dir().join(format!("nuclear-revision-reopen-{}", uuid::Uuid::new_v4()));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    add_item(&store, 1).await;
    let revision_before_restart = store.snapshot().unwrap().latest_sequence;
    assert!(revision_before_restart > 1);
    drop(store);

    let reopened = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().latest_sequence,
        revision_before_restart
    );
    add_item(&reopened, 2).await;
    let expected_revision = reopened.snapshot().unwrap().latest_sequence;
    drop(reopened);

    let verified = StateStore::open_at(journal_path, diagnostics_path).unwrap();
    let snapshot = verified.snapshot().unwrap();
    assert_eq!(snapshot.latest_sequence, expected_revision);
    assert_eq!(snapshot.queue.len(), 2);
    drop(verified);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn front_priority_is_single_row_only() {
    let store = test_store();
    let (first, _) = add_item(&store, 1).await;
    let (second, _) = add_item(&store, 2).await;
    assert!(store
        .enqueue(&[first.id, second.id], QueuePriority::Front)
        .await
        .is_err());
}

#[tokio::test]
async fn front_priority_is_next_after_the_five_admitted_rows() {
    let store = test_store();
    let mut items = Vec::new();
    for index in 0..7 {
        items.push(add_item(&store, index).await.0);
    }
    let normal_ids = items[..6]
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    store
        .enqueue(&normal_ids, QueuePriority::Normal)
        .await
        .unwrap();
    for _ in 0..5 {
        store.take_next_pending().await.unwrap();
    }
    let (front, _) = store
        .enqueue(std::slice::from_ref(&items[6].id), QueuePriority::Front)
        .await
        .unwrap();

    assert_eq!(
        store.take_next_pending().await.unwrap().operation_id,
        front[0].operation_id
    );
}

#[tokio::test]
async fn pending_wait_does_not_dequeue_and_ignores_cancelled_pending_work() {
    let store = test_store();
    let (first, _) = add_item(&store, 1).await;
    let (second, _) = add_item(&store, 2).await;
    let (first_work, _) = store
        .enqueue(std::slice::from_ref(&first.id), QueuePriority::Normal)
        .await
        .unwrap();

    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        store.wait_pending_available(),
    )
    .await
    .expect("an already-pending job is visible without another notification");
    assert_eq!(
        store.pending_operation_ids(),
        vec![first_work[0].operation_id.clone()]
    );

    store
        .request_cancellation(&first_work[0].operation_id)
        .await
        .unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(25),
        store.wait_pending_available(),
    )
    .await
    .is_err());
    assert!(store.cancel_pending(&first_work[0].operation_id).await);

    let waiter_store = store.clone();
    let waiter = tokio::spawn(async move {
        waiter_store.wait_pending_available().await;
    });
    tokio::task::yield_now().await;
    let (second_work, _) = store
        .enqueue(std::slice::from_ref(&second.id), QueuePriority::Normal)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(100), waiter)
        .await
        .expect("enqueue notification must wake a registered pending waiter")
        .unwrap();
    assert_eq!(
        store.pending_operation_ids(),
        vec![second_work[0].operation_id.clone()]
    );
}

#[tokio::test]
async fn maintenance_operation_is_registered_in_the_same_state_transition() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (operation, deltas) = store
        .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
        .await
        .unwrap();
    let snapshot = store.snapshot().unwrap();

    assert_eq!(deltas.len(), 2);
    assert!(snapshot.maintenance_active);
    assert!(snapshot
        .operations
        .iter()
        .any(|value| value.id == operation.id));
    assert!(store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .is_err());
    assert!(store.set_maintenance(false, false).await.is_err());
    let ended = store
        .end_maintenance_operation(&operation.id)
        .await
        .unwrap()
        .expect("owner releases maintenance");
    assert!(matches!(
        ended.delta,
        crate::models::StateDeltaValue::MaintenanceChanged {
            active: false,
            draining: false
        }
    ));
    assert!(!store.snapshot().unwrap().maintenance_active);
}

#[tokio::test]
async fn maintenance_operation_accepts_a_preregistered_unique_uuid() {
    let store = test_store();
    let invalid = store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::RuntimeUpdate,
            "not-a-uuid".to_string(),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "invalid_request");
    assert!(!store.snapshot().unwrap().maintenance_active);

    let operation_id = uuid::Uuid::new_v4().to_string();
    let (operation, _) = store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::RuntimeUpdate,
            operation_id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(operation.id, operation_id);
    store
        .finalize_operation(&operation_id, OperationState::Completed, None)
        .await
        .unwrap();
    store
        .end_maintenance_operation(&operation_id)
        .await
        .unwrap();

    let duplicate = store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::RuntimeUpdate,
            operation_id,
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, "duplicate_operation");
    assert!(!store.snapshot().unwrap().maintenance_active);
}

#[tokio::test]
async fn app_update_handoff_does_not_launch_from_an_unpersisted_marker() {
    let store = test_store();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::AppUpdate,
            operation_id.clone(),
        )
        .await
        .unwrap();
    store.fail_next_persistence_for_test();

    let error = store
        .prepare_app_update_handoff(&operation_id, "v0.6.0")
        .await
        .unwrap_err();

    assert!(error.summary.contains("Injected"));
    let snapshot = store.snapshot().unwrap();
    let operation = snapshot
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Queued);
    assert!(operation.phase.is_none());
    assert!(store.lock().unwrap().pending_app_update.is_none());
}

#[tokio::test]
async fn degraded_app_update_finalization_clears_and_flushes_the_pending_handoff() {
    let store = test_store();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::AppUpdate,
            operation_id.clone(),
        )
        .await
        .unwrap();
    store
        .prepare_app_update_handoff(&operation_id, "0.6.0")
        .await
        .unwrap();
    let premature_completion = store
        .finalize_operation(&operation_id, OperationState::Completed, None)
        .await
        .unwrap_err();
    assert_eq!(
        premature_completion.code,
        "app_update_reconciliation_required"
    );
    assert!(store.lock().unwrap().pending_app_update.is_some());
    store.fail_persistence_for_test(3);

    store
        .finalize_operation(
            &operation_id,
            OperationState::Failed,
            Some(AppError::new(
                "installer_launch_failed",
                "The installer could not be launched.",
            )),
        )
        .await
        .unwrap();

    assert!(store.lock().unwrap().pending_app_update.is_none());
    assert_eq!(
        store.operation_state(&operation_id),
        Some(OperationState::Failed)
    );
    assert!(store.snapshot().unwrap().persistence_health.degraded);

    store
        .end_maintenance_operation(&operation_id)
        .await
        .unwrap();
    store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    assert!(!store.snapshot().unwrap().persistence_health.degraded);
    assert!(store.lock().unwrap().pending_app_update.is_none());
}

#[cfg(windows)]
#[tokio::test]
async fn app_update_handoff_completes_only_on_target_version_and_is_restart_idempotent() {
    let root = std::env::temp_dir().join(format!("nuclear-app-handoff-{}", uuid::Uuid::new_v4()));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let (queue_item, _) = add_item(&store, 1).await;
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::AppUpdate,
            operation_id.clone(),
        )
        .await
        .unwrap();
    let prepared = store
        .prepare_app_update_handoff(&operation_id, "v0.6.0")
        .await
        .unwrap();
    assert!(prepared.iter().any(|delta| matches!(
        &delta.delta,
        crate::models::StateDeltaValue::OperationUpserted(operation)
            if operation.id == operation_id
                && operation.state == OperationState::Running
                && operation.phase.as_deref() == Some("installing")
    )));
    drop(store);

    let restarted = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let before_reconcile = restarted.snapshot().unwrap();
    assert_eq!(before_reconcile.queue[0].id, queue_item.id);
    assert_eq!(
        before_reconcile
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap()
            .state,
        OperationState::Running
    );
    let deltas = restarted
        .reconcile_pending_app_update("0.6.0")
        .await
        .unwrap();
    assert!(deltas.iter().any(|delta| matches!(
        &delta.delta,
        crate::models::StateDeltaValue::OperationUpserted(operation)
            if operation.id == operation_id && operation.state == OperationState::Completed
    )));
    assert!(restarted.lock().unwrap().pending_app_update.is_none());
    drop(restarted);

    let second_restart =
        StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    assert!(second_restart
        .reconcile_pending_app_update("0.6.0")
        .await
        .unwrap()
        .is_empty());
    let snapshot = second_restart.snapshot().unwrap();
    assert_eq!(snapshot.queue[0].id, queue_item.id);
    assert_eq!(
        snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap()
            .state,
        OperationState::Completed
    );
    drop(second_restart);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn app_update_handoff_mismatch_becomes_retryable_interruption() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-app-handoff-mismatch-{}",
        uuid::Uuid::new_v4()
    ));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let (queue_item, _) = add_item(&store, 1).await;
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_maintenance_operation_with_id(
            crate::models::OperationKind::AppUpdate,
            operation_id.clone(),
        )
        .await
        .unwrap();
    store
        .prepare_app_update_handoff(&operation_id, "0.6.0")
        .await
        .unwrap();
    drop(store);

    let restarted = StateStore::open_at(journal_path, diagnostics_path).unwrap();
    restarted
        .reconcile_pending_app_update("0.5.0")
        .await
        .unwrap();
    let snapshot = restarted.snapshot().unwrap();
    assert_eq!(snapshot.queue[0].id, queue_item.id);
    let operation = snapshot
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Interrupted);
    assert_eq!(
        operation.error.as_ref().map(|error| error.code.as_str()),
        Some("app_update_handoff_interrupted")
    );
    assert!(operation.error.as_ref().unwrap().retryable);
    assert!(restarted.lock().unwrap().pending_app_update.is_none());
    drop(restarted);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn queued_operation_blocks_maintenance_before_worker_registration() {
    let store = test_store();
    store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();

    let error = store
        .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
        .await
        .unwrap_err();

    assert_eq!(error.code, "busy");
    assert!(!store.snapshot().unwrap().maintenance_active);
}

#[tokio::test]
async fn cancelled_maintenance_operation_cannot_start_after_cancellation() {
    let store = test_store();
    let (operation, _) = store
        .begin_maintenance_operation(crate::models::OperationKind::RuntimeUpdate)
        .await
        .unwrap();
    store
        .set_operation_state(&operation.id, OperationState::Cancelled, None)
        .await
        .unwrap();

    let error = store
        .set_operation_state(&operation.id, OperationState::Running, None)
        .await
        .unwrap_err();

    assert_eq!(error.code, "invalid_transition");
    assert_eq!(
        store.operation_state(&operation.id),
        Some(OperationState::Cancelled)
    );
}

#[tokio::test]
async fn cancellation_cannot_regress_or_complete_from_late_progress() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    store.request_cancellation(&operation_id).await.unwrap();

    for status in ["queued", "downloading"] {
        let progress = crate::models::DownloadProgress {
            download_id: operation_id.clone(),
            status: status.to_string(),
            progress: 100.0,
            phase: Some("download".to_string()),
            download_progress: Some(100.0),
            conversion_progress: None,
            speed: None,
            eta: None,
            error: None,
            error_code: None,
            error_detail: None,
            filename: None,
        };
        assert!(store
            .apply_download_progress(&operation_id, &progress)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store.operation_state(&operation_id),
            Some(OperationState::Cancelling)
        );
    }

    let receipt = store
        .finalize_download(&operation_id, super::DownloadTerminalOutcome::Cancelled)
        .await
        .unwrap();
    assert_eq!(receipt.progress.status, "cancelled");
    assert_eq!(
        store.operation_state(&operation_id),
        Some(OperationState::Cancelled)
    );
}

#[tokio::test]
async fn cancellation_waiter_resolves_only_after_terminal_transition() {
    let store = test_store();
    let (operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store.request_cancellation(&operation.id).await.unwrap();
    let transition_store = store.clone();
    let operation_id = operation.id.clone();
    let transition_id = operation_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        transition_store
            .set_operation_state(&transition_id, OperationState::Cancelled, None)
            .await
            .unwrap();
    });

    let terminal = store
        .wait_for_terminal(&operation_id, std::time::Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(terminal.state, OperationState::Cancelled);
}

#[tokio::test]
async fn cancelling_inspection_cannot_be_completed() {
    let store = test_store();
    let (operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store.request_cancellation(&operation.id).await.unwrap();

    let error = store
        .complete_inspection(
            &operation.id,
            crate::models::UrlInspection::Video { video: video(1) },
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "invalid_transition");
    assert_eq!(
        store.operation_state(&operation.id),
        Some(OperationState::Cancelling)
    );
}

#[tokio::test]
async fn terminal_operations_are_pruned_from_live_snapshots_with_removal_deltas() {
    let store = test_store();
    let mut removed = 0usize;
    for _ in 0..(MAX_TERMINAL_ATTEMPTS + 7) {
        let (operation, _) = store
            .begin_operation(crate::models::OperationKind::Inspection, None)
            .await
            .unwrap();
        let deltas = store
            .set_operation_state(&operation.id, OperationState::Completed, None)
            .await
            .unwrap();
        removed += deltas
            .iter()
            .filter(|delta| {
                matches!(
                    delta.delta,
                    crate::models::StateDeltaValue::OperationRemoved(_)
                )
            })
            .count();
    }

    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), MAX_TERMINAL_ATTEMPTS);
    assert_eq!(removed, 7);
    assert!(snapshot
        .operations
        .iter()
        .all(|operation| operation.state.is_terminal()));
}

#[tokio::test]
async fn concurrent_operation_admission_never_exceeds_the_limit() {
    let store = test_store();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(33));
    let handles = (0..32)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .begin_operation_with_limit(crate::models::OperationKind::Inspection, None, 16)
                    .await
            })
        })
        .collect::<Vec<_>>();
    barrier.wait().await;
    let mut admitted = 0usize;
    for handle in handles {
        admitted += usize::from(handle.await.unwrap().is_ok());
    }

    assert_eq!(admitted, 16);
    assert_eq!(store.snapshot().unwrap().operations.len(), 16);
}

#[tokio::test]
async fn published_download_completion_wins_a_late_cancellation() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    store.request_cancellation(&operation_id).await.unwrap();

    let receipt = store
        .finalize_download(
            &operation_id,
            super::DownloadTerminalOutcome::Completed {
                filename: Some("C:\\Downloads\\published.mp4".to_string()),
            },
        )
        .await
        .unwrap();

    assert_eq!(receipt.durability, super::FinalizationDurability::Persisted);
    assert_eq!(receipt.progress.status, "completed");
    assert_eq!(
        receipt.progress.filename.as_deref(),
        Some("C:\\Downloads\\published.mp4")
    );
    let snapshot = store.snapshot().unwrap();
    let operation = snapshot
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Completed);
    assert_eq!(
        operation
            .published_output
            .as_ref()
            .map(|output| output.path.as_str()),
        Some("C:\\Downloads\\published.mp4")
    );
    assert_eq!(
        snapshot.queue[0].state,
        crate::models::QueueItemState::Completed
    );
}

#[tokio::test]
async fn transient_terminal_save_failure_is_retried_before_completion_is_acknowledged() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    store.fail_next_persistence_for_test();

    let receipt = store
        .finalize_download(
            &work[0].operation_id,
            super::DownloadTerminalOutcome::Completed { filename: None },
        )
        .await
        .unwrap();

    assert_eq!(receipt.durability, super::FinalizationDurability::Persisted);
    assert_eq!(receipt.progress.status, "completed");
    assert!(!store.snapshot().unwrap().persistence_health.degraded);
}

#[tokio::test]
async fn exhausted_terminal_save_retries_retain_recovery_intent_until_the_next_commit() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    store.fail_persistence_for_test(3);

    let receipt = store
        .finalize_download(
            &operation_id,
            super::DownloadTerminalOutcome::Completed {
                filename: Some("C:\\Downloads\\published.mp4".to_string()),
            },
        )
        .await
        .unwrap();

    assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
    assert_eq!(receipt.progress.status, "error");
    assert_eq!(
        receipt.progress.error_code.as_deref(),
        Some("state_persistence_failed")
    );
    assert_eq!(
        receipt.progress.filename.as_deref(),
        Some("C:\\Downloads\\published.mp4")
    );
    let degraded = store.snapshot().unwrap();
    assert!(degraded.persistence_health.degraded);
    assert_eq!(
        degraded.queue[0].state,
        crate::models::QueueItemState::Failed
    );
    let operation = degraded
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(
        operation
            .intended_terminal_outcome
            .as_ref()
            .map(|outcome| outcome.state),
        Some(OperationState::Completed)
    );
    assert_eq!(
        operation
            .published_output
            .as_ref()
            .map(|output| output.path.as_str()),
        Some("C:\\Downloads\\published.mp4")
    );

    let deltas = store
        .update_queue_item(
            &item.id,
            UpdateQueueItemInput {
                quality: Some("1080p".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(deltas.iter().any(|delta| matches!(
        &delta.delta,
        crate::models::StateDeltaValue::PersistenceHealthChanged(health)
            if !health.degraded && health.error.is_none()
    )));
    let recovered = store.snapshot().unwrap();
    assert!(!recovered.persistence_health.degraded);
    assert_eq!(recovered.queue[0].quality, "1080p");
    let operation = recovered
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(
        operation
            .intended_terminal_outcome
            .as_ref()
            .map(|outcome| outcome.state),
        Some(OperationState::Completed)
    );
}

#[tokio::test]
async fn failed_finalizer_task_installs_a_degraded_terminal_compensation() {
    let store = test_store();
    let reader = store.take_outbox_reader().unwrap();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    while reader.try_recv().is_some() {}
    store.fail_next_finalizer_task_for_test();

    let receipt = store
        .finalize_download(
            &operation_id,
            super::DownloadTerminalOutcome::Completed {
                filename: Some("C:\\Downloads\\published.mp4".to_string()),
            },
        )
        .await
        .unwrap();

    assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
    assert_eq!(receipt.progress.status, "error");
    assert_eq!(
        receipt.progress.error_code.as_deref(),
        Some("state_finalizer_failed")
    );
    let snapshot = store.snapshot().unwrap();
    assert!(snapshot.persistence_health.degraded);
    let operation = snapshot
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(
        operation
            .intended_terminal_outcome
            .as_ref()
            .map(|outcome| outcome.state),
        Some(OperationState::Completed)
    );
    assert_eq!(
        operation
            .published_output
            .as_ref()
            .map(|output| output.path.as_str()),
        Some("C:\\Downloads\\published.mp4")
    );
    assert_eq!(snapshot.queue[0].state, QueueItemState::Failed);
    let publications = std::iter::from_fn(|| reader.try_recv()).collect::<Vec<_>>();
    assert!(publications.iter().any(|publication| matches!(
        publication,
        crate::outbox::StatePublication::Deltas(deltas)
            if deltas.iter().any(|delta| matches!(
                &delta.delta,
                crate::models::StateDeltaValue::OperationUpserted(operation)
                    if operation.id == operation_id && operation.state == OperationState::Failed
            ))
    )));
}

#[cfg(windows)]
#[tokio::test]
async fn failure_after_save_advances_compensation_past_the_persisted_candidate_revision() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-finalizer-after-save-{}",
        uuid::Uuid::new_v4()
    ));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let (item, _) = add_item(&store, 1).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let operation_id = work[0].operation_id.clone();
    let terminal_count = MAX_TERMINAL_ATTEMPTS + 7;
    let sequence_before = store.snapshot().unwrap().latest_sequence;
    {
        let mut state = store.lock().unwrap();
        for _ in 0..terminal_count {
            let id = uuid::Uuid::new_v4().to_string();
            let operation = crate::models::OperationSnapshot {
                schema_version: crate::models::APP_SCHEMA_VERSION,
                id: id.clone(),
                queue_item_id: None,
                kind: crate::models::OperationKind::Inspection,
                state: OperationState::Completed,
                progress: 100.0,
                phase: None,
                sequence: 0,
                created_at_ms: 1,
                updated_at_ms: 1,
                finished_at_ms: Some(1),
                error: None,
                inspection_result: None,
                published_output: None,
                intended_terminal_outcome: None,
                correlation_id: uuid::Uuid::new_v4().to_string(),
            };
            state.operation_order.push(id.clone());
            state.operations.insert(id, operation.into());
        }
    }
    store.fail_next_finalizer_after_save_for_test();

    let receipt = store
        .finalize_download(
            &operation_id,
            super::DownloadTerminalOutcome::Completed { filename: None },
        )
        .await
        .unwrap();

    assert_eq!(receipt.durability, super::FinalizationDurability::Degraded);
    assert!(
        store.snapshot().unwrap().latest_sequence
            > sequence_before + u64::try_from(terminal_count).unwrap()
    );
    store
        .update_queue_item(
            &item.id,
            UpdateQueueItemInput {
                quality: Some("1080p".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    drop(store);

    for _ in 0..2 {
        let reopened = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.queue[0].quality, "1080p");
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .unwrap();
        assert_eq!(operation.state, OperationState::Failed);
        assert_eq!(
            operation
                .intended_terminal_outcome
                .as_ref()
                .map(|outcome| outcome.state),
            Some(OperationState::Completed)
        );
        drop(reopened);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn detached_commit_keeps_the_gate_until_save_and_swap_finish() {
    let root =
        std::env::temp_dir().join(format!("nuclear-detached-commit-{}", uuid::Uuid::new_v4()));
    let journal_path = root.join("journal.dpapi");
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(journal_path.clone(), diagnostics_path.clone()).unwrap();
    let (item, _) = add_item(&store, 1).await;
    let pause = store.pause_next_commit_for_test();
    let first_store = store.clone();
    let first_id = item.id.clone();
    let first = tokio::spawn(async move {
        first_store
            .update_queue_item(
                &first_id,
                UpdateQueueItemInput {
                    quality: Some("1080p".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    pause.wait_entered().await;
    first.abort();

    let transient_store = store.clone();
    let transient = tokio::spawn(async move {
        transient_store
            .set_runtime_readiness(RuntimeReadiness::Ready)
            .await
    });
    let second_store = store.clone();
    let second_id = item.id.clone();
    let second = tokio::spawn(async move {
        second_store
            .update_queue_item(
                &second_id,
                UpdateQueueItemInput {
                    quality: Some("1440p".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!transient.is_finished());
    assert!(!second.is_finished());

    pause.release();
    transient.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    drop(store);

    let reopened = StateStore::open_at(journal_path, diagnostics_path).unwrap();
    assert_eq!(reopened.snapshot().unwrap().queue[0].quality, "1440p");
    drop(reopened);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn inspection_budget_counts_payloads_retained_only_by_the_outbox() {
    let store = test_store();
    let reader = store.take_outbox_reader().unwrap();
    let (first, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(&first.id, large_inspection())
        .await
        .unwrap();
    store.dismiss_operation(&first.id).await.unwrap();

    let (second, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let error = store
        .complete_inspection(&second.id, large_inspection())
        .await
        .unwrap_err();
    assert_eq!(error.code, "inspection_retention_limit");

    while reader.try_recv().is_some() {}
    store
        .complete_inspection(&second.id, large_inspection())
        .await
        .unwrap();
}

#[tokio::test]
async fn inspection_budget_counts_payloads_retained_by_a_snapshot() {
    let store = test_store();
    let reader = store.take_outbox_reader().unwrap();
    let (first, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    store
        .complete_inspection(&first.id, large_inspection())
        .await
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    store.dismiss_operation(&first.id).await.unwrap();
    while reader.try_recv().is_some() {}

    let (second, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let error = store
        .complete_inspection(&second.id, large_inspection())
        .await
        .unwrap_err();
    assert_eq!(error.code, "inspection_retention_limit");

    drop(snapshot);
    store
        .complete_inspection(&second.id, large_inspection())
        .await
        .unwrap();
}

#[tokio::test]
async fn inspection_display_fields_are_utf8_safely_truncated_and_action_fields_rejected() {
    let store = test_store();
    let (display_operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let mut display_video = video(1);
    let mut oversized_capacity_title = String::with_capacity(10 * 1024 * 1024);
    oversized_capacity_title.push_str(&"💥".repeat(2_000));
    display_video.title = oversized_capacity_title;
    display_video.channel = Some("channel".repeat(1_000));
    store
        .complete_inspection(
            &display_operation.id,
            UrlInspection::Video {
                video: display_video,
            },
        )
        .await
        .unwrap();
    let completed = store
        .snapshot()
        .unwrap()
        .operations
        .into_iter()
        .find(|operation| operation.id == display_operation.id)
        .unwrap();
    let inspection = completed.inspection_result.unwrap();
    let UrlInspection::Video {
        video: inspected_video,
    } = inspection.as_ref()
    else {
        panic!("expected video inspection");
    };
    assert!(inspected_video.title.len() <= super::MAX_UI_FIELD_BYTES);
    assert!(inspected_video.title.capacity() <= super::MAX_UI_FIELD_BYTES);
    assert!(inspected_video.channel.as_ref().unwrap().len() <= super::MAX_UI_FIELD_BYTES);

    let (action_operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let mut action_video = video(2);
    action_video.url = "u".repeat(super::MAX_UI_FIELD_BYTES + 1);
    let error = store
        .complete_inspection(
            &action_operation.id,
            UrlInspection::Video {
                video: action_video,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "field_too_large");
}

#[tokio::test]
async fn many_empty_quality_records_are_counted_by_the_inspection_budget() {
    let store = test_store();
    let (operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let quality_count = super::MAX_RETAINED_INSPECTION_BYTES
        .checked_div(std::mem::size_of::<String>())
        .unwrap()
        + 1;
    let mut oversized = video(1);
    oversized.available_qualities = vec![String::new(); quality_count];

    let error = store
        .complete_inspection(&operation.id, UrlInspection::Video { video: oversized })
        .await
        .unwrap_err();

    assert_eq!(error.code, "inspection_retention_limit");
    assert_eq!(
        store.operation_state(&operation.id),
        Some(OperationState::Queued)
    );
}

#[tokio::test]
async fn operation_failure_log_uses_snapshot_correlation_and_redacts_detail() {
    let root = std::env::temp_dir().join(format!("nuclear-state-log-{}", uuid::Uuid::new_v4()));
    let diagnostics_path = root.join("diagnostics");
    let store = StateStore::open_at(root.join("journal.dpapi"), diagnostics_path.clone()).unwrap();
    let (operation, _) = store
        .begin_operation(crate::models::OperationKind::Inspection, None)
        .await
        .unwrap();
    let error = AppError::new("fixture_failed", "Inspection failed")
        .with_detail("Authorization: Bearer should-not-escape");

    store
        .set_operation_state(&operation.id, OperationState::Failed, Some(error))
        .await
        .unwrap();

    let log = std::fs::read_to_string(diagnostics_path.join("diagnostics.jsonl")).unwrap();
    assert!(log.contains(&operation.correlation_id));
    assert!(log.contains("operation_failed"));
    assert!(log.contains("fixture_failed"));
    assert!(!log.contains("should-not-escape"));
    let _ = std::fs::remove_dir_all(root);
}
