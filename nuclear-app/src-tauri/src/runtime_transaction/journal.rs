use super::filesystem::{ensure_regular_path, is_reparse, replace_file_atomically};
use super::RuntimeTransaction;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub(super) const JOURNAL_FILE: &str = ".runtime-transaction-v1.json";
pub(super) const JOURNAL_LIMIT: u64 = 64 * 1024;
pub(super) const QUARANTINE_PREFIX: &str = ".runtime-transaction-v1.quarantine-";

pub(crate) fn load(root: &Path) -> Result<Option<RuntimeTransaction>, String> {
    let path = root.join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(None);
    }
    ensure_regular_path(&path, false, "runtime transaction journal")?;
    let mut file = File::open(&path)
        .map_err(|error| format!("Failed to open runtime transaction journal: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect runtime transaction journal: {error}"))?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err("Runtime transaction journal is not a regular file.".into());
    }
    if metadata.len() > JOURNAL_LIMIT {
        return Err("Runtime transaction journal exceeds the 64 KiB limit.".into());
    }
    read_opened_journal(&mut file).map(Some)
}

pub(super) fn read_opened_journal(file: &mut File) -> Result<RuntimeTransaction, String> {
    let mut bytes = Vec::new();
    Read::take(file, JOURNAL_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Failed to read runtime transaction journal: {error}"))?;
    if bytes.len() as u64 > JOURNAL_LIMIT {
        return Err("Runtime transaction journal exceeds the 64 KiB limit.".into());
    }
    let transaction = serde_json::from_slice::<RuntimeTransaction>(&bytes)
        .map_err(|error| format!("Failed to parse runtime transaction journal: {error}"))?;
    transaction.validate()?;
    Ok(transaction)
}

pub(crate) fn store(root: &Path, transaction: &RuntimeTransaction) -> Result<(), String> {
    transaction.validate()?;
    ensure_regular_path(root, true, "managed runtime root")?;
    let bytes = serde_json::to_vec(transaction)
        .map_err(|error| format!("Failed to serialize runtime transaction journal: {error}"))?;
    let persisted = serde_json::from_slice::<RuntimeTransaction>(&bytes)
        .map_err(|error| format!("Failed to validate runtime transaction journal: {error}"))?;
    if &persisted != transaction {
        return Err("Runtime transaction journal did not round-trip exactly.".into());
    }
    let destination = root.join(JOURNAL_FILE);
    if destination.exists() {
        ensure_regular_path(&destination, false, "runtime transaction journal")?;
    }
    let temporary = root.join(format!(".runtime-transaction-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("Failed to create runtime transaction journal: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "Failed to persist runtime transaction journal: {error}"
        ));
    }
    drop(file);
    replace_file_atomically(&temporary, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Failed to publish runtime transaction journal: {error}")
    })
}

pub(crate) fn clear(root: &Path) -> Result<(), String> {
    let path = root.join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(());
    }
    ensure_regular_path(&path, false, "runtime transaction journal")?;
    std::fs::remove_file(path)
        .map_err(|error| format!("Failed to clear runtime transaction journal: {error}"))
}

pub(crate) fn quarantine(root: &Path, transaction: &RuntimeTransaction) -> Result<(), String> {
    transaction.validate()?;
    let source = root.join(JOURNAL_FILE);
    ensure_regular_path(&source, false, "runtime transaction journal")?;
    let current = load(root)?
        .ok_or_else(|| "Runtime transaction journal disappeared before quarantine.".to_string())?;
    if &current != transaction {
        return Err(
            "Runtime transaction journal changed before quarantine; all artifacts were retained."
                .into(),
        );
    }
    let destination = root.join(format!("{QUARANTINE_PREFIX}{}.json", transaction.update_id));
    if destination.exists() {
        return Err("A runtime transaction quarantine record already exists.".into());
    }
    std::fs::rename(source, destination)
        .map_err(|error| format!("Failed to quarantine runtime transaction journal: {error}"))
}

pub(crate) fn protected_update_ids(root: &Path) -> Result<HashSet<String>, String> {
    let mut protected = HashSet::new();
    if let Some(transaction) = load(root)? {
        protected.insert(transaction.update_id);
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("Failed to inspect runtime transaction records: {error}"))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Failed to enumerate runtime transaction records: {error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(QUARANTINE_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        ensure_regular_path(&path, false, "quarantined runtime transaction")?;
        let mut file = File::open(&path)
            .map_err(|error| format!("Failed to open quarantined runtime transaction: {error}"))?;
        let metadata = file.metadata().map_err(|error| {
            format!("Failed to inspect quarantined runtime transaction: {error}")
        })?;
        if !metadata.is_file() || is_reparse(&metadata) || metadata.len() > JOURNAL_LIMIT {
            return Err(
                "Quarantined runtime transaction exceeds the 64 KiB limit or is not a regular file."
                    .into(),
            );
        }
        let mut bytes = Vec::new();
        Read::take(&mut file, JOURNAL_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Failed to read quarantined runtime transaction: {error}"))?;
        if bytes.len() as u64 > JOURNAL_LIMIT {
            return Err("Quarantined runtime transaction exceeds the 64 KiB limit.".into());
        }
        let transaction = serde_json::from_slice::<RuntimeTransaction>(&bytes)
            .map_err(|error| format!("Failed to parse quarantined runtime transaction: {error}"))?;
        transaction.validate()?;
        let name_update_id = name
            .strip_prefix(QUARANTINE_PREFIX)
            .and_then(|value| value.strip_suffix(".json"))
            .ok_or_else(|| "Quarantined runtime transaction name is invalid.".to_string())?;
        if name_update_id != transaction.update_id {
            return Err(
                "Quarantined runtime transaction name does not match its update ID.".into(),
            );
        }
        protected.insert(transaction.update_id);
    }
    Ok(protected)
}
