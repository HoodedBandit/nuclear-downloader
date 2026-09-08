use crate::backend_lifecycle_tests::fixture;
use crate::downloader;
use crate::models::{DownloadProgress, OperationState, QueuePriority};
use crate::notifications::DownloadProgressSink;
use crate::services::downloads::{download_notifications, finalize_download};
use crate::state::StateStore;
use std::sync::{Arc, Mutex};

async fn queued_download(store: &StateStore, root: &std::path::Path) -> String {
    let item = fixture(store, root).await;
    let (work, _) = store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    work[0].operation_id.clone()
}

#[tokio::test]
async fn progress_state_is_committed_before_legacy_notification() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-service-progress-order-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let operation_id = queued_download(&store, &root).await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let callback_store = store.clone();
    let callback_observed = observed.clone();
    let notify: DownloadProgressSink = Arc::new(move |payload| {
        // The callback is the observation boundary: state must already contain
        // the accepted progress before the legacy event becomes observable.
        let operation = callback_store
            .snapshot()
            .unwrap()
            .operations
            .into_iter()
            .find(|operation| operation.id == payload.download_id)
            .unwrap();
        assert_eq!(operation.progress, payload.progress);
        assert_eq!(operation.state, OperationState::Running);
        callback_observed.lock().unwrap().push(payload.clone());
    });
    let progress = DownloadProgress {
        download_id: operation_id,
        status: "downloading".into(),
        progress: 37.0,
        phase: Some("download".into()),
        download_progress: Some(37.0),
        conversion_progress: None,
        speed: None,
        eta: None,
        error: None,
        error_code: None,
        error_detail: None,
        filename: None,
    };

    let notifications = download_notifications(&store, notify);
    (notifications.progress)(progress.clone()).await;
    let observed_guard = observed.lock().unwrap();
    assert_eq!(observed_guard.len(), 1);
    assert_eq!(observed_guard[0].download_id, progress.download_id);
    assert_eq!(observed_guard[0].status, progress.status);
    assert_eq!(observed_guard[0].progress, progress.progress);
    drop(observed_guard);
    // DownloadNotifications owns a StateStore clone through its progress
    // callback; release it before checking/removing the isolated fixture.
    drop(notifications);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_terminal_save_never_notifies_successful_completion() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-service-terminal-order-{}",
        uuid::Uuid::new_v4()
    ));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let operation_id = queued_download(&store, &root).await;
    // Exercise the actual journal failure path through the service finalizer.
    store.fail_persistence_for_test(3);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let callback_observed = observed.clone();
    let notify: DownloadProgressSink = Arc::new(move |payload| {
        callback_observed.lock().unwrap().push(payload.clone());
    });

    finalize_download(
        &notify,
        &store,
        &operation_id,
        downloader::DownloadOutcome::Completed {
            filename: Some(r"C:\Downloads\published.mp4".into()),
        },
    )
    .await;
    drop(notify);

    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].status, "error");
    assert_eq!(
        observed[0].error_code.as_deref(),
        Some("state_persistence_failed")
    );
    assert!(!observed.iter().any(|payload| payload.status == "completed"));
    assert_eq!(
        store.operation_state(&operation_id),
        Some(OperationState::Failed)
    );
    drop(observed);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
