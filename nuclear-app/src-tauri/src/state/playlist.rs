use super::data::StateData;
use super::reducers::{
    ensure_operation_capacity, next_delta, next_operation_delta, validate_actionable_field,
    validate_cookie_config, validate_optional_actionable_field,
};
use super::{StateStore, MAX_ACTIVE_OPERATIONS, MAX_QUEUE_ITEMS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    AddQueueItemInput, DownloadRequest, MediaSelection, OperationKind, OperationSnapshot,
    OperationState, PlaylistAdmissionKind, PlaylistAdmissionReceipt, PlaylistAdmissionResult,
    PlaylistEntry, PlaylistInfo, QueueItemRecord, QueueItemState, QueuePreparation, StateDelta,
    StateDeltaValue, APP_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

enum ParentPlaylist {
    Replay(PlaylistAdmissionResult),
    Available(PlaylistInfo),
}

struct PlannedRow {
    entry: PlaylistEntry,
    selection: Option<MediaSelection>,
}

struct AdmissionPlan {
    rows: Vec<PlannedRow>,
    skipped_count: usize,
}

struct AppliedPlan {
    receipt: PlaylistAdmissionReceipt,
    preparation_ids: Vec<String>,
}

impl StateStore {
    pub fn playlist_admission_receipt(
        &self,
        input: &AddQueueItemInput,
    ) -> Result<Option<PlaylistAdmissionResult>, AppError> {
        let requested = input
            .playlist
            .as_ref()
            .ok_or_else(|| AppError::invalid("Playlist admission requires a playlist request."))?;
        let encoded = serde_json::to_vec(input)
            .map_err(|_| AppError::internal("Could not fingerprint the playlist request."))?;
        let fingerprint = format!("{:x}", Sha256::digest(encoded));
        let state = self.lock()?;
        let parent = state
            .operations
            .get(&input.inspection_operation_id)
            .ok_or_else(|| AppError::not_found("inspection operation"))?;
        let Some(receipt) = &parent.playlist_admission else {
            return Ok(None);
        };
        if receipt.request_id != requested.request_id || receipt.fingerprint != fingerprint {
            return Err(AppError::new(
                "playlist_request_conflict",
                "The playlist request ID was already used with different input.",
            ));
        }
        Ok(Some(playlist_result(receipt)))
    }
}

fn validate_playlist_input(input: &AddQueueItemInput) -> Result<(), AppError> {
    validate_actionable_field("format", &input.format)?;
    validate_actionable_field("quality", &input.quality)?;
    validate_actionable_field("output folder", &input.output_dir)?;
    validate_optional_actionable_field("filename", input.filename_override.as_deref())?;
    validate_optional_actionable_field(
        "compatibility configuration path",
        input.compat_config_path.as_deref(),
    )?;
    validate_cookie_config(input.cookie_config.as_ref())?;
    let playlist = input
        .playlist
        .as_ref()
        .ok_or_else(|| AppError::invalid("Playlist admission requires a playlist request."))?;
    validate_actionable_field("playlist request ID", &playlist.request_id)?;
    if playlist.request_id.is_empty()
        || playlist.request_id.trim() != playlist.request_id
        || playlist.request_id.chars().any(char::is_control)
        || playlist.entry_indices.is_empty()
    {
        return Err(AppError::invalid("The playlist request is malformed."));
    }
    Ok(())
}

fn request_fingerprint(input: &AddQueueItemInput) -> Result<String, AppError> {
    let encoded = serde_json::to_vec(input)
        .map_err(|_| AppError::internal("Could not fingerprint the playlist request."))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn authoritative_playlist(
    state: &StateData,
    input: &AddQueueItemInput,
    fingerprint: &str,
) -> Result<ParentPlaylist, AppError> {
    let request = input.playlist.as_ref().expect("validated playlist input");
    let parent = state
        .operations
        .get(&input.inspection_operation_id)
        .ok_or_else(|| AppError::not_found("inspection operation"))?;
    if parent.kind != OperationKind::Inspection || parent.state != OperationState::Completed {
        return Err(AppError::new(
            "inspection_not_completed",
            "The playlist inspection must complete before adding entries.",
        )
        .retryable(true));
    }
    if let Some(receipt) = &parent.playlist_admission {
        if receipt.request_id != request.request_id || receipt.fingerprint != fingerprint {
            return Err(AppError::new(
                "playlist_request_conflict",
                "The playlist request ID was already used with different input.",
            ));
        }
        return Ok(ParentPlaylist::Replay(playlist_result(receipt)));
    }
    match parent.inspection_result.as_deref() {
        Some(crate::models::UrlInspection::Playlist { playlist }) => {
            Ok(ParentPlaylist::Available(playlist.clone()))
        }
        Some(_) => Err(AppError::new(
            "inspection_result_kind",
            "The selected inspection is not a playlist.",
        )),
        None => Err(AppError::new(
            "inspection_result_unavailable",
            "This playlist inspection was already consumed; inspect it again.",
        )
        .retryable(true)),
    }
}

fn plan_rows(
    state: &StateData,
    input: &AddQueueItemInput,
    playlist: PlaylistInfo,
) -> Result<AdmissionPlan, AppError> {
    let request = input.playlist.as_ref().expect("validated playlist input");
    validate_indices(request.entry_indices.as_slice(), playlist.entries.len())?;
    let reuse_fingerprint = crate::models::inspection_settings_fingerprint(
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
    );
    let reuse =
        playlist.inspection_settings_fingerprint.as_deref() == Some(reuse_fingerprint.as_str());
    let existing = state
        .queue
        .values()
        .map(|item| (item.source_url.clone(), item.selection.clone()))
        .collect::<Vec<_>>();
    let mut identities = HashSet::new();
    let mut rows = Vec::new();
    let mut skipped_count = 0usize;
    for index in &request.entry_indices {
        let row = prepare_row(&playlist.entries[*index], input, reuse)?;
        let identity = row_identity(&row);
        if !identities.insert(identity.clone()) || existing_identity(&existing, &identity) {
            skipped_count = skipped_count.saturating_add(1);
        } else {
            rows.push(row);
        }
    }
    if state.queue.len().saturating_add(rows.len()) > MAX_QUEUE_ITEMS {
        return Err(AppError::new(
            "queue_limit",
            format!("The queue is limited to {MAX_QUEUE_ITEMS} items."),
        ));
    }
    let pending = rows.iter().filter(|row| row.entry.video.is_none()).count();
    ensure_operation_capacity(state, pending, MAX_ACTIVE_OPERATIONS)?;
    Ok(AdmissionPlan {
        rows,
        skipped_count,
    })
}

fn validate_indices(indices: &[usize], retained: usize) -> Result<(), AppError> {
    let mut requested = HashSet::with_capacity(indices.len());
    for &index in indices {
        if index >= retained || !requested.insert(index) {
            return Err(AppError::invalid(
                "Playlist entry indices must be unique and reference retained entries.",
            ));
        }
    }
    Ok(())
}

fn prepare_row(
    retained: &PlaylistEntry,
    input: &AddQueueItemInput,
    reuse: bool,
) -> Result<PlannedRow, AppError> {
    let mut entry = retained.clone();
    if !reuse {
        entry.video = None;
    }
    let selection = entry.selection.clone();
    if let Some(selection) = &selection {
        selection.validate().map_err(AppError::invalid)?;
    }
    validate_embedded_video(&mut entry, selection.as_ref(), &input.format)?;
    crate::downloader::validate_download_request(&DownloadRequest {
        url: entry.url.clone(),
        quality: input.quality.clone(),
        format: input.format.clone(),
        output_dir: input.output_dir.clone(),
        cookie_config: input.cookie_config.clone(),
        filename_override: input.filename_override.clone(),
        compat_config_path: input.compat_config_path.clone(),
        selection: selection.clone(),
        expected_media_id: (entry.id != entry.url).then(|| entry.id.clone()),
    })
    .map_err(AppError::invalid)?;
    Ok(PlannedRow { entry, selection })
}

fn validate_embedded_video(
    entry: &mut PlaylistEntry,
    selection: Option<&MediaSelection>,
    format: &str,
) -> Result<(), AppError> {
    if let Some(video) = &entry.video {
        if video.selection.as_ref() != selection {
            return Err(AppError::invalid(
                "A playlist entry's normalized metadata has the wrong identity.",
            ));
        }
        if video.id != entry.id {
            if entry.id == entry.url {
                entry.video = None;
            } else {
                return Err(AppError::invalid(
                    "A playlist entry's normalized metadata has the wrong identity.",
                ));
            }
        }
    }
    if let Some(video) = &entry.video {
        reject_audio_only_without_stream(format, video.has_audio)?;
    }
    Ok(())
}

type RowIdentity = (String, Option<String>, Option<String>);

fn row_identity(row: &PlannedRow) -> RowIdentity {
    (
        row.entry.url.clone(),
        row.selection.as_ref().map(|value| value.entry_id.clone()),
        row.selection
            .as_ref()
            .map(|value| value.extractor_key.clone()),
    )
}

fn existing_identity(
    existing: &[(String, Option<MediaSelection>)],
    identity: &RowIdentity,
) -> bool {
    existing.iter().any(|(url, candidate)| {
        url == &identity.0
            && match (candidate.as_ref(), identity.1.as_ref(), identity.2.as_ref()) {
                (Some(candidate), Some(entry_id), Some(extractor_key)) => {
                    &candidate.entry_id == entry_id && &candidate.extractor_key == extractor_key
                }
                (None, None, None) => true,
                _ => false,
            }
    })
}

fn apply_plan(
    state: &mut StateData,
    deltas: &mut Vec<StateDelta>,
    input: &AddQueueItemInput,
    plan: AdmissionPlan,
    request_id: String,
    fingerprint: String,
    now: u64,
) -> Result<AppliedPlan, AppError> {
    let mut item_ids = Vec::with_capacity(plan.rows.len());
    let mut preparation_ids = Vec::new();
    for row in plan.rows {
        let (item_id, preparation_id) = append_row(state, deltas, input, row, now)?;
        item_ids.push(item_id);
        if let Some(id) = preparation_id {
            preparation_ids.push(id);
        }
    }
    let receipt = PlaylistAdmissionReceipt {
        request_id,
        fingerprint,
        item_ids,
        skipped_count: plan.skipped_count,
    };
    let parent = state
        .operations
        .get_mut(&input.inspection_operation_id)
        .expect("validated parent");
    parent.inspection_result = None;
    parent.playlist_admission = Some(receipt.clone());
    parent.updated_at_ms = now;
    let parent = parent.snapshot();
    deltas.push(next_operation_delta(state, parent));
    Ok(AppliedPlan {
        receipt,
        preparation_ids,
    })
}

fn append_row(
    state: &mut StateData,
    deltas: &mut Vec<StateDelta>,
    input: &AddQueueItemInput,
    row: PlannedRow,
    now: u64,
) -> Result<(String, Option<String>), AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let ready = row.entry.video.as_deref();
    let preparation = ready.is_none().then_some(QueuePreparation::Pending);
    let operation_id = preparation.map(|_| uuid::Uuid::new_v4().to_string());
    let item = new_queue_item(id.clone(), input, row, operation_id.clone(), now);
    state.queue_order.push(id.clone());
    state.queue.insert(id.clone(), item.clone().into());
    deltas.push(next_delta(state, StateDeltaValue::QueueItemUpserted(item)));
    if let Some(operation_id) = &operation_id {
        let operation = new_preparation_operation(operation_id.clone(), id.clone(), now);
        state.operation_order.push(operation_id.clone());
        state
            .operations
            .insert(operation_id.clone(), operation.clone().into());
        state.pending_preparations.push_back(operation_id.clone());
        deltas.push(next_operation_delta(state, operation));
    }
    Ok((id, operation_id))
}

fn new_queue_item(
    id: String,
    input: &AddQueueItemInput,
    row: PlannedRow,
    operation_id: Option<String>,
    now: u64,
) -> QueueItemRecord {
    let ready = row.entry.video.as_deref();
    QueueItemRecord {
        schema_version: APP_SCHEMA_VERSION,
        id,
        source_url: row.entry.url.clone(),
        source_media_id: (row.entry.id != row.entry.url).then(|| row.entry.id.clone()),
        title: ready.map_or_else(
            || {
                row.entry
                    .title
                    .clone()
                    .unwrap_or_else(|| row.entry.id.clone())
            },
            |video| video.title.clone(),
        ),
        available_qualities: ready.map_or_else(Vec::new, |video| video.available_qualities.clone()),
        has_audio: ready.is_some_and(|video| video.has_audio),
        cookie_config: input.cookie_config.clone(),
        format: input.format.clone(),
        quality: input.quality.clone(),
        output_dir: input.output_dir.clone(),
        filename_override: input.filename_override.clone(),
        compat_config_path: input.compat_config_path.clone(),
        selection: row.selection,
        preparation: ready.is_none().then_some(QueuePreparation::Pending),
        preparation_operation_id: None,
        state: if ready.is_none() {
            QueueItemState::Queued
        } else {
            QueueItemState::Inert
        },
        latest_operation_id: operation_id,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

fn new_preparation_operation(id: String, queue_item_id: String, now: u64) -> OperationSnapshot {
    OperationSnapshot {
        schema_version: APP_SCHEMA_VERSION,
        id,
        queue_item_id: Some(queue_item_id),
        kind: OperationKind::Inspection,
        state: OperationState::Queued,
        progress: 0.0,
        phase: None,
        sequence: 0,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
        error: None,
        inspection_result: None,
        published_output: None,
        intended_terminal_outcome: None,
        playlist_admission: None,
        correlation_id: uuid::Uuid::new_v4().to_string(),
    }
}

impl StateStore {
    #[cfg(test)]
    pub async fn add_playlist_items(
        &self,
        input: AddQueueItemInput,
    ) -> Result<(PlaylistAdmissionResult, Vec<String>), AppError> {
        let output_dir = input.output_dir.clone();
        self.add_playlist_items_at_output(input, output_dir).await
    }

    pub async fn add_playlist_items_at_output(
        &self,
        raw_input: AddQueueItemInput,
        canonical_output: String,
    ) -> Result<(PlaylistAdmissionResult, Vec<String>), AppError> {
        validate_playlist_input(&raw_input)?;
        validate_actionable_field("output folder", &canonical_output)?;
        let request_id = raw_input
            .playlist
            .as_ref()
            .expect("validated playlist input")
            .request_id
            .clone();
        let fingerprint = request_fingerprint(&raw_input)?;
        let mut queue_input = raw_input.clone();
        queue_input.output_dir = canonical_output;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let playlist = match authoritative_playlist(&state, &raw_input, &fingerprint)? {
            ParentPlaylist::Replay(result) => return Ok((result, Vec::new())),
            ParentPlaylist::Available(playlist) => playlist,
        };
        let plan = plan_rows(&state, &queue_input, playlist)?;
        let receipt = apply_plan(
            &mut state,
            &mut deltas,
            &queue_input,
            plan,
            request_id,
            fingerprint,
            now,
        )?;
        let preparation_ids = receipt.preparation_ids;
        let result = playlist_result(&receipt.receipt);
        self.commit_candidate(state, mutation, retention_now, deltas)
            .await?;
        self.inner.preparation_notify.notify_waiters();
        Ok((result, preparation_ids))
    }
}

pub(super) fn playlist_result(receipt: &PlaylistAdmissionReceipt) -> PlaylistAdmissionResult {
    PlaylistAdmissionResult {
        kind: PlaylistAdmissionKind::Playlist,
        request_id: receipt.request_id.clone(),
        item_ids: receipt.item_ids.clone(),
        skipped_count: receipt.skipped_count,
    }
}

pub(super) fn reject_audio_only_without_stream(
    format: &str,
    has_audio: bool,
) -> Result<(), AppError> {
    if !has_audio && matches!(format, "mp3" | "flac" | "wav" | "aac" | "opus") {
        Err(AppError::invalid(
            "Audio-only output is unavailable because this item has no audio stream.",
        ))
    } else {
        Ok(())
    }
}
