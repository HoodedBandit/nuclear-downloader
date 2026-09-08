use super::{DrainCleanup, DrainGuard};
use crate::lifecycle::DownloadManager;
use crate::models::{OperationState, QueuePriority};
use crate::state::{DownloadTerminalOutcome, StateStore};
use std::sync::Arc;
use std::time::Duration;

async fn unwind_drain(with_pending: bool, finalize_before_panic: bool) {
    let root = std::env::temp_dir().join(format!("nuclear-drain-unwind-{}", uuid::Uuid::new_v4()));
    let store = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let manager = DownloadManager::new(5, 1, 1);
    manager.open_for_test().await;
    let operation_id = if with_pending {
        let item = crate::backend_lifecycle_tests::fixture(&store, &root).await;
        let admission = manager.begin_job_admission(1).await.unwrap();
        let (work, _) = store
            .enqueue(&[item.id], QueuePriority::Normal)
            .await
            .unwrap();
        let id = work[0].operation_id.clone();
        admission.publish(std::slice::from_ref(&id)).await.unwrap();
        Some(id)
    } else {
        None
    };
    let task_manager = manager.clone();
    let task_store = store.clone();
    let task_operation_id = operation_id.clone();
    let task = tokio::spawn(async move {
        let ticket = task_manager.begin_cancel_all().await.unwrap();
        let _guard = DrainGuard(Some(DrainCleanup {
            manager: task_manager,
            store: task_store.clone(),
            publish_progress: Arc::new(|_| {}),
            ticket,
        }));
        if finalize_before_panic {
            task_store
                .finalize_download(
                    task_operation_id.as_deref().unwrap(),
                    DownloadTerminalOutcome::Cancelled,
                )
                .await
                .unwrap();
        }
        panic!("injected panic after acquiring the drain ticket");
    });
    assert!(task.await.unwrap_err().is_panic());
    let claim = tokio::time::timeout(Duration::from_secs(5), manager.wait_worker_claim())
        .await
        .unwrap()
        .unwrap()
        .expect("cleanup must reopen admission");
    drop(claim);
    assert!(manager.active_ids().await.is_empty());
    assert!(store.pending_operation_ids().is_empty());
    if let Some(id) = &operation_id {
        assert_eq!(store.operation_state(id), Some(OperationState::Cancelled));
    }
    manager.begin_shutdown().await;
    manager
        .wait_for_shutdown_tasks(Duration::from_secs(2))
        .await
        .unwrap();
    drop(store);
    let reopened = StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    if let Some(id) = &operation_id {
        assert_eq!(
            reopened.operation_state(id),
            Some(OperationState::Cancelled)
        );
    }
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn abandoned_empty_drain_reopens_admission() {
    unwind_drain(false, false).await;
}

#[tokio::test]
async fn abandoned_drain_finalizes_pending_work_before_reopening() {
    unwind_drain(true, false).await;
}

#[tokio::test]
async fn abandoned_drain_releases_already_finalized_pending_registration() {
    unwind_drain(true, true).await;
}
