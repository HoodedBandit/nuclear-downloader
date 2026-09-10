use super::data::{SharedRecord, StateData};
use crate::app_error::AppError;
use crate::journal::{now_ms, retained_operation_ids, OperationRetentionMetadata};
use crate::models::{
    CookieConfig, OperationKind, OperationSnapshot, OperationState, PublishedOutput,
    QueueItemState, StateDelta, StateDeltaValue, UrlInspection, VideoInfo, APP_SCHEMA_VERSION,
};
use std::collections::HashSet;
use std::sync::Arc;

pub(super) const MAX_UI_FIELD_BYTES: usize = 4 * 1024;

pub(super) fn apply_operation_transition(
    state: &mut StateData,
    id: &str,
    operation_state: OperationState,
    error: Option<AppError>,
    inspection_result: Option<Arc<UrlInspection>>,
    published_output: Option<PublishedOutput>,
    now: u64,
) -> Result<Vec<StateDelta>, AppError> {
    let queue_item_id = {
        let operation = state
            .operations
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if operation.state.is_terminal() && operation.state != operation_state {
            return Err(AppError::new(
                "invalid_transition",
                "A terminal operation cannot change state.",
            ));
        }
        let published_completion = operation_state == OperationState::Completed
            && published_output.is_some()
            && operation.kind == OperationKind::Download;
        if operation.state == OperationState::Cancelling
            && !matches!(
                operation_state,
                OperationState::Cancelling | OperationState::Cancelled | OperationState::Failed
            )
            && !published_completion
        {
            return Err(AppError::new(
                "invalid_transition",
                "A cancelling operation cannot be started again.",
            ));
        }
        if inspection_result.is_some() && operation.kind != OperationKind::Inspection {
            return Err(AppError::new(
                "invalid_inspection_operation",
                "Only an inspection operation may store an inspection result.",
            ));
        }
        operation.state = operation_state;
        operation.updated_at_ms = now;
        operation.error = error;
        if let Some(inspection_result) = inspection_result {
            operation.inspection_result = Some(inspection_result);
        }
        if let Some(published_output) = published_output {
            operation.published_output = Some(published_output);
        }
        if operation_state.is_terminal() {
            operation.finished_at_ms = Some(now);
            operation.phase = None;
            if operation_state == OperationState::Completed {
                operation.progress = 100.0;
            }
        }
        operation.queue_item_id.clone()
    };

    let operation = state
        .operations
        .get(id)
        .map(SharedRecord::snapshot)
        .unwrap();
    let mut deltas = vec![next_operation_delta(state, operation)];
    if let Some(queue_item_id) = queue_item_id {
        if let Some(item) = state.queue.get_mut(&queue_item_id) {
            item.state = queue_state_for_operation(operation_state);
            item.updated_at_ms = now;
            let item = item.snapshot();
            deltas.push(next_delta(state, StateDeltaValue::QueueItemUpserted(item)));
        }
    }
    Ok(deltas)
}

pub(super) fn queue_state_for_operation(state: OperationState) -> QueueItemState {
    match state {
        OperationState::Queued | OperationState::Starting => QueueItemState::Queued,
        OperationState::Running | OperationState::Cancelling => QueueItemState::Running,
        OperationState::Completed => QueueItemState::Completed,
        OperationState::Failed => QueueItemState::Failed,
        OperationState::Cancelled => QueueItemState::Cancelled,
        OperationState::Interrupted => QueueItemState::Interrupted,
    }
}

pub(super) fn clear_pending_app_update(
    state: &mut StateData,
    operation_id: &str,
    operation_state: OperationState,
) {
    if operation_state.is_terminal()
        && state
            .pending_app_update
            .as_ref()
            .is_some_and(|pending| pending.operation_id == operation_id)
    {
        state.pending_app_update = None;
    }
}

pub(super) fn canonical_app_version(raw: &str) -> Result<String, AppError> {
    if raw.trim() != raw {
        return Err(AppError::invalid("Invalid application version."));
    }
    let normalized = raw.strip_prefix('v').unwrap_or(raw);
    let version = semver::Version::parse(normalized)
        .map_err(|_| AppError::invalid("Invalid application version."))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(AppError::invalid(
            "Application update versions must be stable semantic versions.",
        ));
    }
    Ok(version.to_string())
}

pub(super) fn normalize_inspection(
    mut inspection: UrlInspection,
) -> Result<UrlInspection, AppError> {
    match &mut inspection {
        UrlInspection::Video { video } => {
            validate_actionable_field("video ID", &video.id)?;
            validate_actionable_field("video URL", &video.url)?;
            shrink_string(&mut video.id);
            shrink_string(&mut video.url);
            if let Some(selection) = &video.selection {
                selection.validate().map_err(AppError::invalid)?;
            }
            if let Some(thumbnail) = &mut video.thumbnail {
                validate_actionable_field("thumbnail URL", thumbnail)?;
                shrink_string(thumbnail);
            }
            for quality in &mut video.available_qualities {
                validate_actionable_field("quality label", quality)?;
                shrink_string(quality);
            }
            compact_vec(&mut video.available_qualities);
            truncate_display_field(&mut video.title);
            if let Some(channel) = &mut video.channel {
                truncate_display_field(channel);
            }
        }
        UrlInspection::Playlist { playlist } => {
            truncate_display_field(&mut playlist.title);
            if let Some(channel) = &mut playlist.channel {
                truncate_display_field(channel);
            }
            for entry in &mut playlist.entries {
                validate_actionable_field("playlist entry ID", &entry.id)?;
                validate_actionable_field("playlist entry URL", &entry.url)?;
                shrink_string(&mut entry.id);
                shrink_string(&mut entry.url);
                if let Some(selection) = &entry.selection {
                    selection.validate().map_err(AppError::invalid)?;
                }
                if let Some(thumbnail) = &mut entry.thumbnail {
                    validate_actionable_field("thumbnail URL", thumbnail)?;
                    shrink_string(thumbnail);
                }
                if let Some(title) = &mut entry.title {
                    truncate_display_field(title);
                }
            }
            compact_vec(&mut playlist.entries);
        }
    }
    Ok(inspection)
}

pub(super) fn validate_actionable_field(name: &str, value: &str) -> Result<(), AppError> {
    if value.len() > MAX_UI_FIELD_BYTES {
        Err(AppError::new(
            "field_too_large",
            format!("The {name} exceeds the 4 KiB input limit."),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_optional_actionable_field(
    name: &str,
    value: Option<&str>,
) -> Result<(), AppError> {
    value.map_or(Ok(()), |value| validate_actionable_field(name, value))
}

pub(super) fn validate_cookie_config(config: Option<&CookieConfig>) -> Result<(), AppError> {
    let Some(config) = config else {
        return Ok(());
    };
    validate_actionable_field("cookie mode", &config.mode)?;
    validate_actionable_field("cookie browser", &config.browser)?;
    validate_optional_actionable_field("cookie file path", config.cookie_file.as_deref())
}

pub(super) fn truncate_display_field(value: &mut String) {
    if value.len() > MAX_UI_FIELD_BYTES {
        let mut end = MAX_UI_FIELD_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    shrink_string(value);
}

fn shrink_string(value: &mut String) {
    *value = std::mem::take(value).into_boxed_str().into_string();
}

fn compact_vec<T>(value: &mut Vec<T>) {
    *value = std::mem::take(value).into_boxed_slice().into_vec();
}

pub(super) fn pending_download_available(state: &StateData) -> bool {
    state.pending_downloads.iter().any(|operation_id| {
        state.operations.get(operation_id).is_some_and(|operation| {
            operation.state == OperationState::Queued
                && operation
                    .queue_item_id
                    .as_ref()
                    .is_some_and(|queue_item_id| state.queue.contains_key(queue_item_id))
        })
    })
}

pub(super) fn authoritative_inspection_video(
    state: &StateData,
    operation_id: &str,
) -> Result<VideoInfo, AppError> {
    let operation = state
        .operations
        .get(operation_id)
        .ok_or_else(|| AppError::not_found("inspection operation"))?;
    if operation.kind != OperationKind::Inspection {
        return Err(AppError::new(
            "invalid_inspection_operation",
            "The selected operation is not an inspection.",
        ));
    }
    if operation.state != OperationState::Completed {
        return Err(AppError::new(
            "inspection_not_completed",
            "The inspection must complete before adding its result to the queue.",
        )
        .retryable(true));
    }
    match operation.inspection_result.as_deref() {
        Some(UrlInspection::Video { video }) => Ok(video.clone()),
        Some(UrlInspection::Playlist { .. }) => Err(AppError::new(
            "inspection_result_kind",
            "Playlist entries must be inspected individually before queueing.",
        )),
        None => Err(AppError::new(
            "inspection_result_unavailable",
            "The inspection result is no longer available; inspect the item again.",
        )
        .retryable(true)),
    }
}

pub(super) fn ensure_operation_capacity(
    state: &StateData,
    additional: usize,
    limit: usize,
) -> Result<(), AppError> {
    let active = state
        .operations
        .values()
        .filter(|operation| !operation.state.is_terminal())
        .count();
    if active.saturating_add(additional) > limit {
        Err(AppError::new(
            "operation_limit",
            format!("At most {limit} operations may be queued or active at once."),
        )
        .retryable(true))
    } else {
        Ok(())
    }
}

pub(super) fn prune_live_operations(state: &mut StateData, now: u64) -> Vec<StateDelta> {
    let pending_app_update_id = state
        .pending_app_update
        .as_ref()
        .map(|pending| pending.operation_id.as_str());
    let retained = retained_operation_ids(
        state
            .operations
            .values()
            .map(|operation| OperationRetentionMetadata {
                id: &operation.id,
                state: operation.state,
                finished_at_ms: operation.finished_at_ms,
                updated_at_ms: operation.updated_at_ms,
            }),
        pending_app_update_id,
        now,
    );
    let removed = state
        .operations
        .keys()
        .filter(|id| !retained.contains(*id))
        .cloned()
        .collect::<HashSet<_>>();
    if removed.is_empty() {
        return Vec::new();
    }

    state
        .operation_order
        .retain(|operation_id| !removed.contains(operation_id));
    for operation_id in &removed {
        state.operations.remove(operation_id);
    }

    let mut deltas = removed
        .iter()
        .cloned()
        .map(|operation_id| next_delta(state, StateDeltaValue::OperationRemoved(operation_id)))
        .collect::<Vec<_>>();
    let queue_items_to_clear = state
        .queue
        .iter()
        .filter_map(|(id, item)| {
            item.latest_operation_id
                .as_ref()
                .is_some_and(|operation_id| removed.contains(operation_id))
                .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    for item_id in queue_items_to_clear {
        if let Some(item) = state.queue.get_mut(&item_id) {
            item.latest_operation_id = None;
            let item = item.snapshot();
            deltas.push(next_delta(state, StateDeltaValue::QueueItemUpserted(item)));
        }
    }
    deltas
}

pub(super) fn next_delta(state: &mut StateData, delta: StateDeltaValue) -> StateDelta {
    state.sequence = state.sequence.saturating_add(1);
    StateDelta {
        schema_version: APP_SCHEMA_VERSION,
        sequence: state.sequence,
        emitted_at_ms: now_ms(),
        delta,
    }
}

pub(super) fn next_operation_delta(
    state: &mut StateData,
    mut operation: OperationSnapshot,
) -> StateDelta {
    state.sequence = state.sequence.saturating_add(1);
    operation.sequence = state.sequence;
    state
        .operations
        .insert(operation.id.clone(), operation.clone().into());
    StateDelta {
        schema_version: APP_SCHEMA_VERSION,
        sequence: state.sequence,
        emitted_at_ms: now_ms(),
        delta: StateDeltaValue::OperationUpserted(operation),
    }
}
