use super::{
    protect_for_current_user, read_encrypted_journal, JournalStore, LatestReferencePolicy,
    PersistentJournal, PreparedJournal, MAX_DECRYPTED_JOURNAL_BYTES, MAX_ENCRYPTED_JOURNAL_BYTES,
    MAX_TERMINAL_ATTEMPTS, TERMINAL_RETENTION_MS,
};
use crate::models::{
    OperationKind, OperationSnapshot, OperationState, PendingAppUpdateRecovery, QueueItemRecord,
    QueueItemState, UrlInspection, VideoInfo, APP_SCHEMA_VERSION,
};
use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::Notify;

#[test]
fn encrypted_journal_growth_after_metadata_is_rejected_with_the_size_contract() {
    let mut grown_reader = std::io::repeat(0).take(MAX_ENCRYPTED_JOURNAL_BYTES + 1);

    let error = read_encrypted_journal(&mut grown_reader).unwrap_err();

    assert_eq!(error.code, "journal_too_large");
    assert_eq!(
        error.summary,
        "The saved application state is too large to load safely. The existing journal was preserved."
    );
}

#[derive(Debug)]
pub(crate) struct TestJournalSavePause {
    entered: Notify,
    released: Mutex<bool>,
    release_changed: Condvar,
}

impl TestJournalSavePause {
    pub(crate) async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        if let Ok(mut released) = self.released.lock() {
            *released = true;
            self.release_changed.notify_all();
        }
    }
}

impl JournalStore {
    pub(crate) fn fail_next_save_for_test(&self) {
        self.fail_saves_for_test(1);
    }

    pub(crate) fn fail_saves_for_test(&self, count: usize) {
        self.fail_saves_remaining.store(count, Ordering::SeqCst);
    }

    pub(crate) fn pause_next_save_for_test(&self) -> Arc<TestJournalSavePause> {
        let pause = Arc::new(TestJournalSavePause {
            entered: Notify::new(),
            released: Mutex::new(false),
            release_changed: Condvar::new(),
        });
        *self.save_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    pub(super) fn pause_save_for_test(&self) {
        let pause = self.save_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            let mut released = pause.released.lock().unwrap();
            while !*released {
                released = pause.release_changed.wait(released).unwrap();
            }
        }
    }
}

fn operation(index: usize, state: OperationState, updated_at_ms: u64) -> OperationSnapshot {
    OperationSnapshot {
        schema_version: APP_SCHEMA_VERSION,
        id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
        queue_item_id: None,
        kind: OperationKind::Download,
        state,
        progress: 0.0,
        phase: None,
        sequence: 0,
        created_at_ms: updated_at_ms,
        updated_at_ms,
        finished_at_ms: state.is_terminal().then_some(updated_at_ms),
        error: None,
        inspection_result: None,
        published_output: None,
        intended_terminal_outcome: None,
        correlation_id: uuid::Uuid::from_u128(index as u128 + 10_000).to_string(),
    }
}

fn queue_item(index: usize, latest_operation_id: Option<String>) -> QueueItemRecord {
    QueueItemRecord {
        schema_version: APP_SCHEMA_VERSION,
        id: uuid::Uuid::from_u128(index as u128 + 100_000).to_string(),
        source_url: format!("https://example.com/{index}"),
        title: format!("item-{index}"),
        available_qualities: vec!["720p".to_string()],
        has_audio: true,
        cookie_config: None,
        format: "mp4".to_string(),
        quality: "720p".to_string(),
        output_dir: "C:\\Downloads".to_string(),
        filename_override: None,
        compat_config_path: None,
        state: QueueItemState::Completed,
        latest_operation_id,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

#[test]
fn schema_one_journal_defaults_a_missing_pending_update_record() {
    let mut value = serde_json::to_value(PersistentJournal::default()).unwrap();
    value.as_object_mut().unwrap().remove("pendingAppUpdate");

    let journal: PersistentJournal = serde_json::from_value(value).unwrap();

    assert!(journal.pending_app_update.is_none());
    assert_eq!(journal.schema_version, APP_SCHEMA_VERSION);
}

#[test]
fn restart_normalization_and_retention_preserve_pending_app_update_proof() {
    let mut app_update = operation(1, OperationState::Running, 1);
    app_update.kind = OperationKind::AppUpdate;
    app_update.phase = Some("installing".to_string());
    let operation_id = app_update.id.clone();
    let mut journal = PersistentJournal {
        operations: vec![app_update],
        pending_app_update: Some(PendingAppUpdateRecovery {
            operation_id: operation_id.clone(),
            expected_version: "0.6.0".to_string(),
            prepared_at_ms: 1,
        }),
        ..PersistentJournal::default()
    };

    assert!(!journal.normalize_after_restart(TERMINAL_RETENTION_MS + 10));
    assert_eq!(journal.operations[0].state, OperationState::Running);

    journal.operations[0].state = OperationState::Interrupted;
    journal.operations[0].finished_at_ms = Some(1);
    for index in 2..=(MAX_TERMINAL_ATTEMPTS + 5) {
        journal.operations.push(operation(
            index,
            OperationState::Completed,
            TERMINAL_RETENTION_MS + 10,
        ));
    }
    journal.prune_and_repair_latest_references(TERMINAL_RETENTION_MS + 10);

    assert!(journal
        .operations
        .iter()
        .any(|operation| operation.id == operation_id));
    assert!(
        super::validate_journal_structure(&journal, LatestReferencePolicy::RequirePresent).is_ok()
    );
}

#[test]
fn pruning_retains_active_and_only_recent_bounded_terminal_attempts() {
    let now = TERMINAL_RETENTION_MS + 10_000;
    let mut journal = PersistentJournal::default();
    journal
        .operations
        .push(operation(0, OperationState::Running, 1));
    for index in 1..=(MAX_TERMINAL_ATTEMPTS + 20) {
        journal
            .operations
            .push(operation(index, OperationState::Completed, now));
    }
    journal
        .operations
        .push(operation(999, OperationState::Completed, 1));

    journal.prune_and_repair_latest_references(now);

    assert_eq!(journal.schema_version, APP_SCHEMA_VERSION);
    assert_eq!(journal.operations.len(), MAX_TERMINAL_ATTEMPTS + 1);
    assert!(journal
        .operations
        .iter()
        .any(|item| item.id == operation(0, OperationState::Running, 1).id));
    assert!(!journal
        .operations
        .iter()
        .any(|item| item.id == operation(999, OperationState::Completed, 1).id));
}

#[test]
fn dismiss_repair_clears_a_dangling_latest_operation_reference() {
    let operation = operation(1, OperationState::Completed, 25);
    let mut journal = PersistentJournal {
        queue: vec![queue_item(1, Some(operation.id.clone()))],
        operations: vec![operation.clone()],
        ..PersistentJournal::default()
    };
    journal
        .operations
        .retain(|candidate| candidate.id != operation.id);

    let changes = journal.prune_and_repair_latest_references(25);

    assert_eq!(
        changes.cleared_latest_operation_item_ids,
        vec![journal.queue[0].id.clone()]
    );
    assert!(journal.queue[0].latest_operation_id.is_none());
}

#[test]
fn age_pruning_clears_only_the_reference_to_the_expired_attempt() {
    let now = TERMINAL_RETENTION_MS + 10_000;
    let mut expired = operation(1, OperationState::Completed, 1);
    let mut recent = operation(2, OperationState::Completed, now);
    let expired_item = queue_item(1, Some(expired.id.clone()));
    let recent_item = queue_item(2, Some(recent.id.clone()));
    expired.queue_item_id = Some(expired_item.id.clone());
    recent.queue_item_id = Some(recent_item.id.clone());
    let recent_id = recent.id.clone();
    let mut journal = PersistentJournal {
        queue: vec![expired_item, recent_item],
        operations: vec![expired, recent],
        ..PersistentJournal::default()
    };

    journal.prune_and_repair_latest_references(now);

    assert!(journal.queue[0].latest_operation_id.is_none());
    assert_eq!(
        journal.queue[1].latest_operation_id.as_deref(),
        Some(recent_id.as_str())
    );
}

#[test]
fn count_retention_repairs_references_to_evicted_attempts() {
    let now = 10_000;
    let mut operations = (0..=MAX_TERMINAL_ATTEMPTS)
        .map(|index| operation(index, OperationState::Completed, index as u64 + 1))
        .collect::<Vec<_>>();
    let oldest_id = operations[0].id.clone();
    let newest_id = operations[MAX_TERMINAL_ATTEMPTS].id.clone();
    let mut oldest_item = queue_item(1, Some(oldest_id));
    let mut newest_item = queue_item(2, Some(newest_id.clone()));
    operations[0].queue_item_id = Some(oldest_item.id.clone());
    operations[MAX_TERMINAL_ATTEMPTS].queue_item_id = Some(newest_item.id.clone());
    oldest_item.updated_at_ms = now;
    newest_item.updated_at_ms = now;
    let mut journal = PersistentJournal {
        queue: vec![oldest_item, newest_item],
        operations,
        ..PersistentJournal::default()
    };

    journal.prune_and_repair_latest_references(now);

    assert_eq!(journal.operations.len(), MAX_TERMINAL_ATTEMPTS);
    assert!(journal.queue[0].latest_operation_id.is_none());
    assert_eq!(
        journal.queue[1].latest_operation_id.as_deref(),
        Some(newest_id.as_str())
    );
}

#[test]
fn restart_converts_nonterminal_operations_to_interrupted() {
    let mut journal = PersistentJournal::default();
    journal
        .operations
        .push(operation(1, OperationState::Running, 1));

    journal.normalize_after_restart(25);

    let operation = &journal.operations[0];
    assert_eq!(operation.state, OperationState::Interrupted);
    assert_eq!(operation.finished_at_ms, Some(25));
    assert_eq!(operation.error.as_ref().unwrap().code, "interrupted");
}

#[test]
fn persistence_strips_large_transient_inspection_results() {
    let mut journal = PersistentJournal::default();
    let mut completed = operation(1, OperationState::Completed, 25);
    completed.kind = OperationKind::Inspection;
    completed.inspection_result = Some(std::sync::Arc::new(UrlInspection::Video {
        video: VideoInfo {
            id: "video".to_string(),
            title: "x".repeat(2 * 1024 * 1024),
            duration: None,
            channel: None,
            thumbnail: None,
            url: "https://example.com/video".to_string(),
            available_qualities: vec!["720p".to_string()],
            has_audio: true,
        },
    }));
    journal.operations.push(completed);

    let persisted = journal.prepare_for_persistence(25).unwrap().journal;
    let serialized = serde_json::to_vec(&persisted).unwrap();

    assert!(persisted.operations[0].inspection_result.is_none());
    assert!(serialized.len() < 16 * 1024);
}

#[cfg(windows)]
#[test]
fn oversized_save_preserves_the_previous_journal_and_removes_temporary_files() {
    let root = std::env::temp_dir().join(format!("nuclear-journal-limit-{}", uuid::Uuid::new_v4()));
    let path = root.join("state.dpapi");
    let (store, _, _) = JournalStore::open(path.clone()).unwrap();
    let previous = PersistentJournal {
        revision: 1,
        ..PersistentJournal::default()
    };
    store.save(&previous).unwrap();

    let mut item = queue_item(1, None);
    item.source_url = "u".repeat(MAX_DECRYPTED_JOURNAL_BYTES + 1);
    let oversized = PersistentJournal {
        revision: 2,
        queue: vec![item],
        ..PersistentJournal::default()
    };
    let error = store.save(&oversized).unwrap_err();

    assert_eq!(error.code, "journal_too_large");
    assert_eq!(store.read().unwrap().revision, 1);
    assert!(std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-")));
    drop(store);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn oversized_existing_journal_is_preserved_without_quarantine() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-journal-load-limit-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.dpapi");
    let oversized_len = MAX_ENCRYPTED_JOURNAL_BYTES + 1;
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(oversized_len).unwrap();

    let error = JournalStore::open(path.clone()).unwrap_err();

    assert_eq!(error.code, "journal_too_large");
    assert!(path.exists());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), oversized_len);
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        2,
        "the oversized journal and its lock should be the only files"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn journal_lock_rejects_a_concurrent_instance_and_releases_on_drop() {
    let root = std::env::temp_dir().join(format!("nuclear-lock-{}", uuid::Uuid::new_v4()));
    let path = root.join("state.dpapi");
    let (first, _, _) = JournalStore::open(path.clone()).unwrap();

    let error = JournalStore::open(path.clone()).unwrap_err();
    assert_eq!(error.code, "already_running");

    drop(first);
    assert!(JournalStore::open(path).is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn damaged_dpapi_ciphertext_is_quarantined_as_corrupt() {
    for damage in ["truncate", "bitflip"] {
        let root = std::env::temp_dir().join(format!(
            "nuclear-journal-damage-{damage}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.dpapi");
        let json = serde_json::to_vec(&PersistentJournal::default()).unwrap();
        let mut protected = protect_for_current_user(&json).unwrap();
        if damage == "truncate" {
            protected.truncate(protected.len() / 2);
        } else {
            let middle = protected.len() / 2;
            protected[middle] ^= 0x5a;
        }
        std::fs::write(&path, protected).unwrap();

        let (_, journal, quarantine) = JournalStore::open(path.clone()).unwrap();

        assert!(journal.queue.is_empty());
        assert!(journal.operations.is_empty());
        assert!(!path.exists());
        assert!(quarantine.unwrap().is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(windows)]
#[test]
fn stale_journal_revision_cannot_overwrite_a_newer_snapshot() {
    let root = std::env::temp_dir().join(format!("nuclear-revision-{}", uuid::Uuid::new_v4()));
    let path = root.join("state.dpapi");
    let (store, _, _) = JournalStore::open(path).unwrap();
    let newer = PersistentJournal {
        revision: 2,
        ..PersistentJournal::default()
    };
    let older = PersistentJournal {
        revision: 1,
        ..PersistentJournal::default()
    };

    store.save(&newer).unwrap();
    store.save(&older).unwrap();

    assert_eq!(store.read().unwrap().revision, 2);
    drop(store);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn dangling_latest_reference_is_repaired_once_and_persisted() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-dangling-reference-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.dpapi");
    let dangling_id = uuid::Uuid::new_v4().to_string();
    let journal = PersistentJournal {
        revision: 7,
        queue: vec![queue_item(1, Some(dangling_id))],
        ..PersistentJournal::default()
    };
    let protected = protect_for_current_user(&serde_json::to_vec(&journal).unwrap()).unwrap();
    std::fs::write(&path, protected).unwrap();

    let (first_store, first, first_quarantine) = JournalStore::open(path.clone()).unwrap();
    assert!(first_quarantine.is_none());
    assert!(first.queue[0].latest_operation_id.is_none());
    assert_eq!(first.revision, 8);
    drop(first_store);
    let repaired_bytes = std::fs::read(&path).unwrap();

    let (second_store, second, second_quarantine) = JournalStore::open(path.clone()).unwrap();
    assert!(second_quarantine.is_none());
    assert!(second.queue[0].latest_operation_id.is_none());
    assert_eq!(second.revision, 8);
    assert_eq!(std::fs::read(&path).unwrap(), repaired_bytes);
    drop(second_store);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn invalid_prepared_journal_preserves_previous_published_bytes() {
    let root =
        std::env::temp_dir().join(format!("nuclear-invalid-prepared-{}", uuid::Uuid::new_v4()));
    let path = root.join("state.dpapi");
    let (store, _, _) = JournalStore::open(path.clone()).unwrap();
    store
        .save_prepared(
            PersistentJournal {
                revision: 1,
                ..PersistentJournal::default()
            }
            .prepare_for_persistence(1)
            .unwrap(),
        )
        .unwrap();
    let previous_bytes = std::fs::read(&path).unwrap();

    let duplicate = operation(1, OperationState::Completed, 2);
    let invalid_candidate = PersistentJournal {
        revision: 2,
        operations: vec![duplicate.clone(), duplicate.clone()],
        ..PersistentJournal::default()
    };
    let prepare_error = invalid_candidate.prepare_for_persistence(2).unwrap_err();
    assert_eq!(prepare_error.code, "journal_corrupt");
    assert_eq!(std::fs::read(&path).unwrap(), previous_bytes);

    let invalid = PreparedJournal {
        journal: PersistentJournal {
            revision: 2,
            operations: vec![duplicate.clone(), duplicate],
            ..PersistentJournal::default()
        },
    };
    let error = store.save_prepared(invalid).unwrap_err();

    assert_eq!(error.code, "journal_corrupt");
    assert_eq!(std::fs::read(&path).unwrap(), previous_bytes);
    drop(store);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn structural_validation_rejects_duplicate_operation_ids() {
    let id = uuid::Uuid::new_v4().to_string();
    let correlation = uuid::Uuid::new_v4().to_string();
    let mut first = operation(1, OperationState::Completed, 1);
    first.id.clone_from(&id);
    first.correlation_id.clone_from(&correlation);
    let mut second = first.clone();
    second.correlation_id = uuid::Uuid::new_v4().to_string();
    let journal = PersistentJournal {
        operations: vec![first, second],
        ..PersistentJournal::default()
    };

    assert_eq!(
        super::validate_journal_structure(&journal, LatestReferencePolicy::RequirePresent,)
            .unwrap_err()
            .code,
        "journal_corrupt"
    );
}

#[cfg(windows)]
#[test]
fn future_schema_fails_closed_without_quarantining_the_journal() {
    let root = std::env::temp_dir().join(format!("nuclear-journal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.dpapi");
    let future = serde_json::json!({
        "schemaVersion": 2,
        "queue": { "futureLayout": [1, 2, 3] },
        "operations": "future-layout",
    });
    let protected = protect_for_current_user(&serde_json::to_vec(&future).unwrap()).unwrap();
    std::fs::write(&path, &protected).unwrap();

    for _ in 0..2 {
        let error = JournalStore::open(path.clone()).unwrap_err();
        assert_eq!(error.code, "journal_migration_required");
        assert_eq!(std::fs::read(&path).unwrap(), protected);
    }
    assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.contains("corrupt-"))
    }));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn future_record_schema_is_preserved_before_decoding_its_shape() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-journal-record-schema-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.dpapi");
    let future = serde_json::json!({
        "schemaVersion": APP_SCHEMA_VERSION,
        "queue": [{
            "schemaVersion": APP_SCHEMA_VERSION + 1,
            "futureRecordLayout": { "nested": true }
        }],
        "operations": [],
    });
    let protected = protect_for_current_user(&serde_json::to_vec(&future).unwrap()).unwrap();
    std::fs::write(&path, &protected).unwrap();

    for _ in 0..2 {
        let error = JournalStore::open(path.clone()).unwrap_err();
        assert_eq!(error.code, "journal_migration_required");
        assert_eq!(std::fs::read(&path).unwrap(), protected);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn malformed_encrypted_json_is_quarantined() {
    let root = std::env::temp_dir().join(format!("nuclear-journal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.dpapi");
    std::fs::write(&path, protect_for_current_user(b"not-json").unwrap()).unwrap();

    let (_, journal, quarantine) = JournalStore::open(path.clone()).unwrap();

    assert!(journal.queue.is_empty());
    assert!(!path.exists());
    assert!(quarantine.unwrap().is_file());
    let _ = std::fs::remove_dir_all(root);
}
