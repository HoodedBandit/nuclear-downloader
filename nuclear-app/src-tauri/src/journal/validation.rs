use super::PersistentJournal;
use crate::app_error::AppError;
use crate::models::{OperationKind, OperationState, QueuePreparation, APP_SCHEMA_VERSION};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalSchemaHeader {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalRecordSchemaHeaders {
    queue: Vec<RecordSchemaHeader>,
    operations: Vec<RecordSchemaHeader>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordSchemaHeader {
    schema_version: u32,
}

pub(super) fn validate_serialized_schema(json: &[u8]) -> Result<(), AppError> {
    let header: JournalSchemaHeader = serde_json::from_slice(json)
        .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
    if header.schema_version != APP_SCHEMA_VERSION {
        return Err(journal_migration_required());
    }
    let records: JournalRecordSchemaHeaders = serde_json::from_slice(json)
        .map_err(|_| AppError::new("journal_corrupt", "The application journal is corrupt."))?;
    if records
        .queue
        .iter()
        .chain(records.operations.iter())
        .any(|record| record.schema_version != APP_SCHEMA_VERSION)
    {
        return Err(journal_migration_required());
    }
    Ok(())
}

fn journal_migration_required() -> AppError {
    AppError::new(
        "journal_migration_required",
        "The application journal was created by an unsupported version.",
    )
    .retryable(true)
}

#[derive(Clone, Copy)]
pub(super) enum LatestReferencePolicy {
    AllowDangling,
    RequirePresent,
}

pub(super) fn validate_journal_structure(
    journal: &PersistentJournal,
    latest_reference_policy: LatestReferencePolicy,
) -> Result<(), AppError> {
    validate_schema_and_limits(journal)?;
    let queue_ids = validate_queue_records(journal)?;
    validate_operation_records(journal, &queue_ids)?;
    validate_pending_update(journal)?;
    validate_queue_operation_references(journal, latest_reference_policy)
}

fn validate_schema_and_limits(journal: &PersistentJournal) -> Result<(), AppError> {
    if journal.schema_version != APP_SCHEMA_VERSION {
        return Err(AppError::new(
            "journal_migration_required",
            "The application journal was created by an unsupported version.",
        )
        .retryable(true));
    }
    if journal
        .queue
        .iter()
        .any(|item| item.schema_version != APP_SCHEMA_VERSION)
        || journal
            .operations
            .iter()
            .any(|operation| operation.schema_version != APP_SCHEMA_VERSION)
    {
        return Err(AppError::new(
            "journal_migration_required",
            "The application journal contains records from an unsupported version.",
        )
        .retryable(true));
    }
    if journal.queue.len() > 1_000 || journal.operations.len() > 1_200 {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal exceeded its record limits.",
        ));
    }
    Ok(())
}

fn validate_queue_records(journal: &PersistentJournal) -> Result<HashSet<&str>, AppError> {
    let mut queue_ids = HashSet::with_capacity(journal.queue.len());
    for item in &journal.queue {
        if uuid::Uuid::parse_str(&item.id).is_err()
            || !queue_ids.insert(item.id.as_str())
            || item.source_media_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 4 * 1024 || id.chars().any(char::is_control)
            })
            || item
                .preparation_operation_id
                .as_ref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
            || item
                .selection
                .as_ref()
                .is_some_and(|selection| selection.validate().is_err())
        {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal contained invalid queue records.",
            ));
        }
    }
    Ok(queue_ids)
}

fn validate_operation_records(
    journal: &PersistentJournal,
    queue_ids: &HashSet<&str>,
) -> Result<(), AppError> {
    let mut operation_ids = HashSet::with_capacity(journal.operations.len());
    for operation in &journal.operations {
        if uuid::Uuid::parse_str(&operation.id).is_err()
            || uuid::Uuid::parse_str(&operation.correlation_id).is_err()
            || !operation_ids.insert(operation.id.as_str())
            || operation
                .queue_item_id
                .as_deref()
                .is_some_and(|id| !queue_ids.contains(id))
        {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal contained invalid operation references.",
            ));
        }
        validate_playlist_receipt(operation)?;
    }
    Ok(())
}

fn validate_playlist_receipt(operation: &crate::models::OperationSnapshot) -> Result<(), AppError> {
    let Some(receipt) = &operation.playlist_admission else {
        return Ok(());
    };
    if operation.kind != OperationKind::Inspection
        || operation.state != OperationState::Completed
        || operation.queue_item_id.is_some()
        || receipt.request_id.is_empty()
        || receipt.request_id.len() > 4 * 1024
        || receipt.request_id.trim() != receipt.request_id
        || receipt.request_id.chars().any(char::is_control)
        || receipt.fingerprint.len() != 64
        || !receipt
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || receipt.item_ids.len() > 1_000
        || receipt.skipped_count > 1_000
        || receipt
            .item_ids
            .iter()
            .any(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal contained an invalid playlist admission receipt.",
        ));
    }
    Ok(())
}

fn validate_pending_update(journal: &PersistentJournal) -> Result<(), AppError> {
    let Some(pending) = &journal.pending_app_update else {
        return Ok(());
    };
    if uuid::Uuid::parse_str(&pending.operation_id).is_err()
        || pending.expected_version.is_empty()
        || pending.expected_version.len() > 128
        || !matches!(
            semver::Version::parse(&pending.expected_version),
            Ok(version)
                if version.pre.is_empty()
                    && version.build.is_empty()
                    && version.to_string() == pending.expected_version
        )
    {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal contained an invalid pending update record.",
        ));
    }
    let operation = journal
        .operations
        .iter()
        .find(|operation| operation.id == pending.operation_id)
        .ok_or_else(|| {
            AppError::new(
                "journal_corrupt",
                "The pending update operation was missing from the application journal.",
            )
        })?;
    if operation.kind != OperationKind::AppUpdate {
        return Err(AppError::new(
            "journal_corrupt",
            "The pending update record referenced the wrong operation kind.",
        ));
    }
    Ok(())
}

fn validate_queue_operation_references(
    journal: &PersistentJournal,
    latest_reference_policy: LatestReferencePolicy,
) -> Result<(), AppError> {
    for item in &journal.queue {
        validate_preparation_reference(journal, item)?;
        validate_latest_reference(journal, item, latest_reference_policy)?;
    }
    Ok(())
}

fn validate_preparation_reference(
    journal: &PersistentJournal,
    item: &crate::models::QueueItemRecord,
) -> Result<(), AppError> {
    let Some(operation_id) = item.preparation_operation_id.as_deref() else {
        return Ok(());
    };
    let operation = journal
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .ok_or_else(|| {
            AppError::new(
                "journal_corrupt",
                "The application journal contained a missing preparation operation reference.",
            )
        })?;
    if item.preparation != Some(QueuePreparation::Pending)
        || item.latest_operation_id.is_some()
        || operation.kind != OperationKind::Inspection
        || operation.queue_item_id.as_deref() != Some(item.id.as_str())
    {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal contained an inconsistent preparation operation reference.",
        ));
    }
    Ok(())
}

fn validate_latest_reference(
    journal: &PersistentJournal,
    item: &crate::models::QueueItemRecord,
    latest_reference_policy: LatestReferencePolicy,
) -> Result<(), AppError> {
    let Some(operation_id) = item.latest_operation_id.as_deref() else {
        return Ok(());
    };
    let Some(operation) = journal
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
    else {
        if matches!(
            latest_reference_policy,
            LatestReferencePolicy::AllowDangling
        ) {
            return Ok(());
        }
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal contained a missing latest operation reference.",
        ));
    };
    if operation.kind != OperationKind::Download
        || operation.queue_item_id.as_deref() != Some(item.id.as_str())
    {
        return Err(AppError::new(
            "journal_corrupt",
            "The application journal contained an inconsistent latest operation reference.",
        ));
    }
    Ok(())
}
