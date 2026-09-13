use super::data::SharedRecord;
use super::reducers::{
    authoritative_inspection_video, ensure_operation_capacity, next_delta, next_operation_delta,
    pending_download_available, validate_actionable_field, validate_cookie_config,
    validate_optional_actionable_field,
};
use super::{QueuedDownload, StateStore, MAX_ACTIVE_OPERATIONS, MAX_QUEUE_ITEMS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    AddQueueItemInput, DownloadRequest, OperationKind, OperationSnapshot, OperationState,
    PlaylistAdmissionKind, PlaylistAdmissionReceipt, PlaylistAdmissionResult, QueueItemRecord,
    QueueItemState, QueuePreparation, QueuePriority, StateDelta, StateDeltaValue,
    UpdateQueueItemInput, VideoInfo, APP_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

impl StateStore {
    pub async fn add_queue_item(
        &self,
        input: AddQueueItemInput,
    ) -> Result<(QueueItemRecord, Vec<StateDelta>), AppError> {
        validate_actionable_field("format", &input.format)?;
        validate_actionable_field("quality", &input.quality)?;
        validate_actionable_field("output folder", &input.output_dir)?;
        validate_optional_actionable_field("filename", input.filename_override.as_deref())?;
        validate_optional_actionable_field(
            "compatibility configuration path",
            input.compat_config_path.as_deref(),
        )?;
        validate_cookie_config(input.cookie_config.as_ref())?;
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.queue.len() >= MAX_QUEUE_ITEMS {
            return Err(AppError::new(
                "queue_limit",
                format!("The queue is limited to {MAX_QUEUE_ITEMS} items."),
            ));
        }
        let inspection = authoritative_inspection_video(&state, &input.inspection_operation_id)?;
        if let Some(selection) = &inspection.selection {
            selection.validate().map_err(AppError::invalid)?;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let item = QueueItemRecord {
            schema_version: APP_SCHEMA_VERSION,
            id: id.clone(),
            source_url: inspection.url,
            source_media_id: Some(inspection.id),
            title: inspection.title,
            available_qualities: inspection.available_qualities,
            has_audio: inspection.has_audio,
            cookie_config: input.cookie_config,
            format: input.format,
            quality: input.quality,
            output_dir: input.output_dir,
            filename_override: input.filename_override,
            compat_config_path: input.compat_config_path,
            selection: inspection.selection,
            preparation: None,
            preparation_operation_id: None,
            state: QueueItemState::Inert,
            latest_operation_id: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        state.queue_order.push(id.clone());
        state.queue.insert(id, item.clone().into());
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::QueueItemUpserted(item.clone()),
        ));
        state.operations.remove(&input.inspection_operation_id);
        state
            .operation_order
            .retain(|candidate| candidate != &input.inspection_operation_id);
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::OperationRemoved(input.inspection_operation_id),
        ));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok((item, deltas))
    }

    pub async fn add_playlist_items(
        &self,
        input: AddQueueItemInput,
    ) -> Result<(PlaylistAdmissionResult, Vec<String>), AppError> {
        validate_actionable_field("format", &input.format)?;
        validate_actionable_field("quality", &input.quality)?;
        validate_actionable_field("output folder", &input.output_dir)?;
        validate_optional_actionable_field("filename", input.filename_override.as_deref())?;
        validate_optional_actionable_field(
            "compatibility configuration path",
            input.compat_config_path.as_deref(),
        )?;
        validate_cookie_config(input.cookie_config.as_ref())?;
        let playlist_input = input
            .playlist
            .as_ref()
            .ok_or_else(|| AppError::invalid("Playlist admission requires a playlist request."))?;
        validate_actionable_field("playlist request ID", &playlist_input.request_id)?;
        if playlist_input.request_id.is_empty()
            || playlist_input.request_id.trim() != playlist_input.request_id
            || playlist_input.request_id.chars().any(char::is_control)
            || playlist_input.entry_indices.is_empty()
        {
            return Err(AppError::invalid("The playlist request is malformed."));
        }
        let encoded = serde_json::to_vec(&input)
            .map_err(|_| AppError::internal("Could not fingerprint the playlist request."))?;
        let fingerprint = format!("{:x}", Sha256::digest(encoded));
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
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
            if receipt.request_id != playlist_input.request_id || receipt.fingerprint != fingerprint
            {
                return Err(AppError::new(
                    "playlist_request_conflict",
                    "The playlist request ID was already used with different input.",
                ));
            }
            return Ok((playlist_result(receipt), Vec::new()));
        }
        let playlist = match parent.inspection_result.as_deref() {
            Some(crate::models::UrlInspection::Playlist { playlist }) => playlist.clone(),
            Some(_) => {
                return Err(AppError::new(
                    "inspection_result_kind",
                    "The selected inspection is not a playlist.",
                ))
            }
            None => {
                return Err(AppError::new(
                    "inspection_result_unavailable",
                    "This playlist inspection was already consumed; inspect it again.",
                )
                .retryable(true))
            }
        };
        let reuse_fingerprint = crate::models::inspection_settings_fingerprint(
            input.cookie_config.as_ref(),
            input.compat_config_path.as_deref(),
        );
        let reuse_full_metadata =
            playlist.inspection_settings_fingerprint.as_deref() == Some(reuse_fingerprint.as_str());
        let mut requested = HashSet::with_capacity(playlist_input.entry_indices.len());
        for &index in &playlist_input.entry_indices {
            if index >= playlist.entries.len() || !requested.insert(index) {
                return Err(AppError::invalid(
                    "Playlist entry indices must be unique and reference retained entries.",
                ));
            }
        }
        let existing = state
            .queue
            .values()
            .map(|item| (item.source_url.clone(), item.selection.clone()))
            .collect::<Vec<_>>();
        let mut accepted = Vec::new();
        let mut identities = HashSet::new();
        let mut skipped_count = 0usize;
        for index in &playlist_input.entry_indices {
            let mut entry = playlist.entries[*index].clone();
            if !reuse_full_metadata {
                entry.video = None;
            }
            let selection = entry.selection.clone();
            if let Some(selection) = &selection {
                selection.validate().map_err(AppError::invalid)?;
            }
            if let Some(video) = &entry.video {
                if video.selection != selection {
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
                reject_audio_only_without_stream(&input.format, video.has_audio)?;
            }
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
            let identity = (
                entry.url.clone(),
                selection.as_ref().map(|value| value.entry_id.clone()),
                selection.as_ref().map(|value| value.extractor_key.clone()),
            );
            let duplicate = !identities.insert(identity.clone())
                || existing.iter().any(|(url, candidate)| {
                    url == &identity.0
                        && match (candidate.as_ref(), identity.1.as_ref(), identity.2.as_ref()) {
                            (Some(candidate), Some(entry_id), Some(extractor_key)) => {
                                &candidate.entry_id == entry_id
                                    && &candidate.extractor_key == extractor_key
                            }
                            (None, None, None) => true,
                            _ => false,
                        }
                });
            if duplicate {
                skipped_count = skipped_count.saturating_add(1);
            } else {
                accepted.push((entry, selection));
            }
        }
        if state.queue.len().saturating_add(accepted.len()) > MAX_QUEUE_ITEMS {
            return Err(AppError::new(
                "queue_limit",
                format!("The queue is limited to {MAX_QUEUE_ITEMS} items."),
            ));
        }
        let pending_count = accepted
            .iter()
            .filter(|(entry, _)| entry.video.is_none())
            .count();
        ensure_operation_capacity(&state, pending_count, MAX_ACTIVE_OPERATIONS)?;
        let mut item_ids = Vec::with_capacity(accepted.len());
        let mut preparation_ids = Vec::with_capacity(pending_count);
        for (entry, selection) in accepted {
            let id = uuid::Uuid::new_v4().to_string();
            let ready_video = entry.video.as_deref();
            let preparation = ready_video.is_none().then_some(QueuePreparation::Pending);
            let operation_id = preparation.map(|_| uuid::Uuid::new_v4().to_string());
            let item = QueueItemRecord {
                schema_version: APP_SCHEMA_VERSION,
                id: id.clone(),
                source_url: entry.url.clone(),
                source_media_id: (entry.id != entry.url).then(|| entry.id.clone()),
                title: ready_video.map_or_else(
                    || entry.title.clone().unwrap_or_else(|| entry.id.clone()),
                    |video| video.title.clone(),
                ),
                available_qualities: ready_video
                    .map_or_else(Vec::new, |video| video.available_qualities.clone()),
                has_audio: ready_video.is_some_and(|video| video.has_audio),
                cookie_config: input.cookie_config.clone(),
                format: input.format.clone(),
                quality: input.quality.clone(),
                output_dir: input.output_dir.clone(),
                filename_override: input.filename_override.clone(),
                compat_config_path: input.compat_config_path.clone(),
                selection,
                preparation,
                preparation_operation_id: None,
                state: QueueItemState::Inert,
                latest_operation_id: operation_id.clone(),
                created_at_ms: now,
                updated_at_ms: now,
            };
            let mut item = item;
            if item.preparation == Some(QueuePreparation::Pending) {
                item.state = QueueItemState::Queued;
            }
            state.queue_order.push(id.clone());
            state.queue.insert(id.clone(), item.clone().into());
            deltas.push(next_delta(
                &mut state,
                StateDeltaValue::QueueItemUpserted(item),
            ));
            if let Some(operation_id) = operation_id {
                let operation = OperationSnapshot {
                    schema_version: APP_SCHEMA_VERSION,
                    id: operation_id.clone(),
                    queue_item_id: Some(id.clone()),
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
                };
                state.operation_order.push(operation_id.clone());
                state.pending_preparations.push_back(operation_id.clone());
                deltas.push(next_operation_delta(&mut state, operation));
                preparation_ids.push(operation_id);
            }
            item_ids.push(id);
        }
        let receipt = PlaylistAdmissionReceipt {
            request_id: playlist_input.request_id.clone(),
            fingerprint,
            item_ids,
            skipped_count,
        };
        let parent = state
            .operations
            .get_mut(&input.inspection_operation_id)
            .expect("validated parent");
        parent.inspection_result = None;
        parent.playlist_admission = Some(receipt.clone());
        parent.updated_at_ms = now;
        let parent = parent.snapshot();
        deltas.push(next_operation_delta(&mut state, parent));
        self.commit_candidate(state, mutation, retention_now, deltas)
            .await?;
        self.inner.preparation_notify.notify_waiters();
        Ok((playlist_result(&receipt), preparation_ids))
    }

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

    pub fn completed_inspection_video(&self, operation_id: &str) -> Result<VideoInfo, AppError> {
        let state = self.lock()?;
        authoritative_inspection_video(&state, operation_id)
    }

    pub async fn update_queue_item(
        &self,
        id: &str,
        input: UpdateQueueItemInput,
    ) -> Result<Vec<StateDelta>, AppError> {
        validate_optional_actionable_field("format", input.format.as_deref())?;
        validate_optional_actionable_field("quality", input.quality.as_deref())?;
        validate_optional_actionable_field("output folder", input.output_dir.as_deref())?;
        if let Some(filename) = &input.filename_override {
            validate_optional_actionable_field("filename", filename.as_deref())?;
        }
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut state, mut recovery_deltas, retention_now) = self.durable_candidate()?;
        let item = state
            .queue
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        if !item.state.is_editable() {
            return Err(AppError::new(
                "queue_item_active",
                "A queued or running item cannot be edited.",
            ));
        }
        if item.preparation == Some(QueuePreparation::Pending) {
            return Err(AppError::new(
                "queue_item_preparing",
                "A queue item cannot be edited while metadata preparation is pending.",
            ));
        }
        let item = state
            .queue
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        if let Some(format) = input.format {
            item.format = format;
        }
        if let Some(quality) = input.quality {
            item.quality = quality;
        }
        if let Some(output_dir) = input.output_dir {
            item.output_dir = output_dir;
        }
        if let Some(filename_override) = input.filename_override {
            item.filename_override = filename_override;
        }
        item.updated_at_ms = now_ms();
        let item = item.snapshot();
        let delta = next_delta(&mut state, StateDeltaValue::QueueItemUpserted(item));
        recovery_deltas.push(delta);
        self.commit_candidate(state, mutation, retention_now, recovery_deltas.clone())
            .await?;
        Ok(recovery_deltas)
    }

    pub async fn remove_queue_items(&self, ids: &[String]) -> Result<Vec<StateDelta>, AppError> {
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        let removed_ids = ids.iter().map(String::as_str).collect::<HashSet<_>>();
        for id in ids {
            let item = state
                .queue
                .get(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            if !item.state.is_editable() {
                return Err(AppError::new(
                    "queue_item_active",
                    "A queued or running item cannot be removed.",
                ));
            }
        }
        if state.operations.values().any(|operation| {
            operation
                .queue_item_id
                .as_deref()
                .is_some_and(|id| removed_ids.contains(id))
                && !operation.state.is_terminal()
        }) {
            return Err(AppError::new(
                "queue_item_active",
                "A queue item with an active operation cannot be removed.",
            ));
        }
        for id in ids {
            state.queue.remove(id);
            state.queue_order.retain(|candidate| candidate != id);
        }
        let detached_operation_ids = state
            .operation_order
            .iter()
            .filter(|operation_id| {
                state
                    .operations
                    .get(*operation_id)
                    .and_then(|operation| operation.queue_item_id.as_deref())
                    .is_some_and(|item_id| removed_ids.contains(item_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        deltas.reserve(detached_operation_ids.len() + 1);
        for operation_id in detached_operation_ids {
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.queue_item_id = None;
                operation.updated_at_ms = now_ms();
                let operation = operation.snapshot();
                deltas.push(next_operation_delta(&mut state, operation));
            }
        }
        deltas.push(next_delta(
            &mut state,
            StateDeltaValue::QueueItemsRemoved(ids.to_vec()),
        ));
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        Ok(deltas)
    }

    pub async fn enqueue(
        &self,
        ids: &[String],
        priority: QueuePriority,
    ) -> Result<(Vec<QueuedDownload>, Vec<StateDelta>), AppError> {
        if priority == QueuePriority::Front && ids.len() != 1 {
            return Err(AppError::invalid(
                "Front priority accepts exactly one queue item.",
            ));
        }
        let mutation = self.inner.mutation_gate.clone().lock_owned().await;
        let now = now_ms();
        let (mut state, mut deltas, retention_now) = self.durable_candidate()?;
        if state.maintenance_active || state.draining {
            return Err(AppError::busy(
                "New work is paused while maintenance or cancellation is active.",
            ));
        }
        ensure_operation_capacity(&state, ids.len(), MAX_ACTIVE_OPERATIONS)?;

        let mut unique_ids = HashSet::with_capacity(ids.len());
        for id in ids {
            if !unique_ids.insert(id) {
                return Err(AppError::invalid(
                    "A queue item was requested more than once.",
                ));
            }
            let item = state
                .queue
                .get(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            if matches!(item.state, QueueItemState::Queued | QueueItemState::Running) {
                return Err(AppError::new(
                    "queue_item_active",
                    "The queue item already has an active attempt.",
                ));
            }
            if item
                .latest_operation_id
                .as_ref()
                .is_some_and(|operation_id| {
                    state
                        .operations
                        .get(operation_id)
                        .is_some_and(|operation| !operation.state.is_terminal())
                })
            {
                return Err(AppError::new(
                    "queue_item_active",
                    "The queue item already has an active attempt.",
                ));
            }
        }

        let mut work = Vec::with_capacity(ids.len());
        deltas.reserve(ids.len() * 2);
        for id in ids {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let item = state
                .queue
                .get_mut(id)
                .ok_or_else(|| AppError::not_found("queue item"))?;
            item.state = QueueItemState::Queued;
            item.latest_operation_id = Some(operation_id.clone());
            item.updated_at_ms = now;
            let item = item.snapshot();
            deltas.push(next_delta(
                &mut state,
                StateDeltaValue::QueueItemUpserted(item.clone()),
            ));

            let is_preparation = item.preparation == Some(QueuePreparation::Pending);
            let operation = OperationSnapshot {
                schema_version: APP_SCHEMA_VERSION,
                id: operation_id.clone(),
                queue_item_id: Some(id.clone()),
                kind: if is_preparation {
                    OperationKind::Inspection
                } else {
                    OperationKind::Download
                },
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
            };
            state.operation_order.push(operation_id.clone());
            state
                .operations
                .insert(operation_id.clone(), operation.clone().into());
            if is_preparation {
                if priority == QueuePriority::Front {
                    state.pending_preparations.push_front(operation_id.clone());
                } else {
                    state.pending_preparations.push_back(operation_id.clone());
                }
            } else if priority == QueuePriority::Front {
                state.pending_downloads.push_front(operation_id.clone());
            } else {
                state.pending_downloads.push_back(operation_id.clone());
            }
            deltas.push(next_operation_delta(&mut state, operation));
            work.push(QueuedDownload {
                operation_id,
                queue_item: item,
            });
        }
        self.commit_candidate(state, mutation, retention_now, deltas.clone())
            .await?;
        self.inner.pending_notify.notify_waiters();
        self.inner.preparation_notify.notify_waiters();
        Ok((work, deltas))
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.lock()
            .map(|state| {
                state
                    .pending_downloads
                    .iter()
                    .chain(state.pending_preparations.iter())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn wait_pending_available(&self) {
        loop {
            let notified = self.inner.pending_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .lock()
                .is_ok_and(|state| pending_download_available(&state))
            {
                return;
            }
            notified.await;
        }
    }

    pub async fn take_next_pending(&self) -> Option<QueuedDownload> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock().ok()?;
        while let Some(operation_id) = state.pending_downloads.pop_front() {
            let Some(operation) = state.operations.get(&operation_id) else {
                continue;
            };
            if operation.state != OperationState::Queued {
                continue;
            }
            let Some(queue_item_id) = operation.queue_item_id.as_ref() else {
                continue;
            };
            let Some(queue_item) = state.queue.get(queue_item_id).map(SharedRecord::snapshot)
            else {
                continue;
            };
            return Some(QueuedDownload {
                operation_id,
                queue_item,
            });
        }
        None
    }

    pub async fn wait_preparation_available(&self) {
        loop {
            let notified = self.inner.preparation_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock().is_ok_and(|state| {
                state.pending_preparations.iter().any(|operation_id| {
                    state.operations.get(operation_id).is_some_and(|operation| {
                        operation.state == OperationState::Queued
                            && operation
                                .queue_item_id
                                .as_ref()
                                .is_some_and(|id| state.queue.contains_key(id))
                    })
                })
            }) {
                return;
            }
            notified.await;
        }
    }

    pub async fn take_next_preparation(&self) -> Option<QueuedDownload> {
        let _mutation = self.inner.mutation_gate.lock().await;
        let mut state = self.lock().ok()?;
        while let Some(operation_id) = state.pending_preparations.pop_front() {
            let Some(operation) = state.operations.get(&operation_id) else {
                continue;
            };
            if operation.kind != OperationKind::Inspection
                || operation.state != OperationState::Queued
            {
                continue;
            }
            let Some(queue_item_id) = operation.queue_item_id.as_ref() else {
                continue;
            };
            let Some(queue_item) = state.queue.get(queue_item_id).map(SharedRecord::snapshot)
            else {
                continue;
            };
            return Some(QueuedDownload {
                operation_id,
                queue_item,
            });
        }
        None
    }

    pub async fn cancel_pending(&self, operation_id: &str) -> bool {
        let _mutation = self.inner.mutation_gate.lock().await;
        let Ok(mut state) = self.lock() else {
            return false;
        };
        let before = state.pending_downloads.len();
        state
            .pending_downloads
            .retain(|candidate| candidate != operation_id);
        let before_preparations = state.pending_preparations.len();
        state
            .pending_preparations
            .retain(|candidate| candidate != operation_id);
        before != state.pending_downloads.len()
            || before_preparations != state.pending_preparations.len()
    }

    pub fn journaled_output_roots(&self) -> Vec<String> {
        let Ok(state) = self.lock() else {
            return Vec::new();
        };
        let mut roots = state
            .queue
            .values()
            .map(|item| item.output_dir.clone())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }
}

fn playlist_result(receipt: &PlaylistAdmissionReceipt) -> PlaylistAdmissionResult {
    PlaylistAdmissionResult {
        kind: PlaylistAdmissionKind::Playlist,
        request_id: receipt.request_id.clone(),
        item_ids: receipt.item_ids.clone(),
        skipped_count: receipt.skipped_count,
    }
}

fn reject_audio_only_without_stream(format: &str, has_audio: bool) -> Result<(), AppError> {
    if !has_audio && matches!(format, "mp3" | "flac" | "wav" | "aac" | "opus") {
        Err(AppError::invalid(
            "Audio-only output is unavailable because this item has no audio stream.",
        ))
    } else {
        Ok(())
    }
}
