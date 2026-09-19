use super::*;

fn rename(name: &str) -> UpdateQueueItemInput {
    UpdateQueueItemInput {
        filename_override: Some(Some(name.to_owned())),
        ..Default::default()
    }
}

#[tokio::test]
async fn worker_waits_for_a_durable_rename_before_taking_the_request() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let pause = store.pause_next_commit_for_test();
    let rename_store = store.clone();
    let renaming = tokio::spawn(async move {
        rename_store
            .update_queue_item(&item.id, rename("Committed first"))
            .await
    });
    pause.wait_entered().await;
    let claim_store = store.clone();
    let claiming = tokio::spawn(async move { claim_store.take_next_pending().await });
    tokio::task::yield_now().await;
    assert!(!claiming.is_finished());
    pause.release();
    renaming.await.unwrap().unwrap();
    assert_eq!(
        claiming
            .await
            .unwrap()
            .unwrap()
            .queue_item
            .filename_override
            .as_deref(),
        Some("Committed first")
    );
}

#[tokio::test]
async fn renamed_waiting_filename_survives_a_journal_restart() {
    let root =
        std::env::temp_dir().join(format!("nuclear-rename-restart-{}", uuid::Uuid::new_v4()));
    let journal = root.join("journal.dpapi");
    let diagnostics = root.join("diagnostics");
    let store = StateStore::open_at(journal.clone(), diagnostics.clone()).unwrap();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    store
        .update_queue_item(&item.id, rename("Remember this name"))
        .await
        .unwrap();
    drop(store);
    let reopened = StateStore::open_at(journal, diagnostics).unwrap();
    assert_eq!(
        reopened
            .queue_item(&item.id)
            .unwrap()
            .filename_override
            .as_deref(),
        Some("Remember this name")
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn waiting_rename_preserves_operation_and_queue_order() {
    let store = test_store();
    let (first, _) = add_item(&store, 1).await;
    let (second, _) = add_item(&store, 2).await;
    let ids = [first.id.clone(), second.id.clone()];
    let (work, _) = store.enqueue(&ids, QueuePriority::Normal).await.unwrap();
    store
        .update_queue_item(&second.id, rename("Renamed waiting file"))
        .await
        .unwrap();
    assert_eq!(
        store.take_next_pending().await.unwrap().operation_id,
        work[0].operation_id
    );
    let next = store.take_next_pending().await.unwrap();
    assert_eq!(next.operation_id, work[1].operation_id);
    assert_eq!(
        next.queue_item.filename_override.as_deref(),
        Some("Renamed waiting file")
    );
}

#[tokio::test]
async fn worker_claim_wins_over_a_late_rename() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let claimed = store.take_next_pending().await.unwrap();
    assert!(store
        .update_queue_item(&item.id, rename("Too late"))
        .await
        .is_err());
    assert_eq!(claimed.queue_item.filename_override, None);
    assert_eq!(store.queue_item(&item.id).unwrap().filename_override, None);
}

#[tokio::test]
async fn waiting_rename_failure_rolls_back_without_reordering_pending_work() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    let before = store.snapshot().unwrap();
    store.fail_next_persistence_for_test();
    let error = store
        .update_queue_item(&item.id, rename("Uncommitted"))
        .await
        .unwrap_err();
    assert!(error.summary.contains("Injected"));
    assert_eq!(
        store.snapshot().unwrap().latest_sequence,
        before.latest_sequence
    );
    assert_eq!(
        store
            .take_next_pending()
            .await
            .unwrap()
            .queue_item
            .filename_override,
        None
    );
}

#[tokio::test]
async fn waiting_item_rejects_changes_other_than_its_filename() {
    let store = test_store();
    let (item, _) = add_item(&store, 1).await;
    store
        .enqueue(std::slice::from_ref(&item.id), QueuePriority::Normal)
        .await
        .unwrap();
    for input in [
        UpdateQueueItemInput {
            format: Some("webm".into()),
            ..rename("Mixed")
        },
        UpdateQueueItemInput {
            quality: Some("best".into()),
            ..rename("Mixed")
        },
        UpdateQueueItemInput {
            output_dir: Some("D:\\Other".into()),
            ..rename("Mixed")
        },
    ] {
        assert!(store.update_queue_item(&item.id, input).await.is_err());
    }
    assert_eq!(store.queue_item(&item.id).unwrap().filename_override, None);
}
