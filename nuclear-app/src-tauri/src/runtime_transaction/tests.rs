use super::*;
use std::path::PathBuf;

fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nuclear-{label}-{}", uuid::Uuid::new_v4()))
}

#[test]
fn transaction_round_trips_every_checkpoint() {
    let root = root("runtime-transaction-roundtrip");
    std::fs::create_dir_all(&root).unwrap();
    let mut transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    for checkpoint in [
        RuntimeTransactionCheckpoint::CandidateVerified,
        RuntimeTransactionCheckpoint::OldMoved,
        RuntimeTransactionCheckpoint::NewPublished,
        RuntimeTransactionCheckpoint::CurrentPointerCommitted,
        RuntimeTransactionCheckpoint::BackupCleaned,
    ] {
        transaction.set_checkpoint(checkpoint);
        store(&root, &transaction).unwrap();
        assert_eq!(load(&root).unwrap(), Some(transaction.clone()));
    }
    clear(&root).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn operating_system_lock_rejects_a_second_owner() {
    let root = root("runtime-mutation-lock");
    let first = RuntimeMutationLock::acquire(&root).unwrap();
    assert!(RuntimeMutationLock::acquire(&root).is_err());
    drop(first);
    RuntimeMutationLock::acquire(&root).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn changed_journal_is_preserved_instead_of_quarantining_stale_state() {
    let root = root("runtime-quarantine-identity");
    std::fs::create_dir_all(&root).unwrap();
    let stored = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    let stale = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.10".to_string(),
        false,
    )
    .unwrap();
    store(&root, &stored).unwrap();

    assert!(quarantine(&root, &stale).is_err());
    assert_eq!(load(&root).unwrap(), Some(stored));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn protected_update_ids_rejects_oversized_quarantine_and_preserves_it() {
    let root = root("runtime-quarantine-bound");
    std::fs::create_dir_all(&root).unwrap();
    let transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    let quarantine = root.join(format!("{QUARANTINE_PREFIX}{}.json", transaction.update_id));
    let mut bytes = serde_json::to_vec(&transaction).unwrap();
    bytes.resize(JOURNAL_LIMIT as usize + 1, b' ');
    std::fs::write(&quarantine, &bytes).unwrap();

    let error = protected_update_ids(&root).unwrap_err();
    assert!(error.contains("64 KiB limit"));
    assert_eq!(std::fs::read(&quarantine).unwrap(), bytes);

    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn mutation_lock_rejects_a_reparse_ancestor_without_creating_the_leaf() {
    use std::os::windows::fs::symlink_dir;

    let base = root("runtime-lock-reparse");
    let actual = base.join("actual");
    let linked = base.join("linked");
    let managed = linked.join("managed");
    std::fs::create_dir_all(&actual).unwrap();
    if symlink_dir(&actual, &linked).is_err() {
        let _ = std::fs::remove_dir_all(base);
        return;
    }

    assert!(RuntimeMutationLock::acquire(&managed).is_err());
    assert!(!actual.join("managed").exists());
    std::fs::remove_dir(&linked).unwrap();
    let _ = std::fs::remove_dir_all(base);
}
