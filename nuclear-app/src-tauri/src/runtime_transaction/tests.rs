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

#[test]
fn journal_growth_after_metadata_is_bounded_and_preserved() {
    use std::io::{Seek, Write};

    let root = root("runtime-journal-growth-bound");
    let mutation_lock = RuntimeMutationLock::acquire(&root).unwrap();
    let transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    store(&root, &transaction).unwrap();
    let path = root.join(journal::JOURNAL_FILE);
    let mut opened = std::fs::File::open(&path).unwrap();
    let observed_size = opened.metadata().unwrap().len();
    assert!(observed_size < JOURNAL_LIMIT);

    // The application mutation lock does not prevent another journal writer.
    // Append valid JSON whitespace after the metadata observation, without timing.
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writer
        .write_all(&vec![b' '; JOURNAL_LIMIT as usize])
        .unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    let expected = std::fs::read(&path).unwrap();

    let error = journal::read_opened_journal(&mut opened).unwrap_err();
    assert!(error.contains("64 KiB limit"));
    assert_eq!(opened.stream_position().unwrap(), JOURNAL_LIMIT + 1);
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    drop(opened);
    assert!(load(&root).unwrap_err().contains("64 KiB limit"));
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    drop(mutation_lock);
    std::fs::remove_dir_all(root).unwrap();
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
