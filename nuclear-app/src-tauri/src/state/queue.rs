use super::reducers::{
    authoritative_inspection_video, next_delta, next_operation_delta, validate_actionable_field,
    validate_cookie_config, validate_optional_actionable_field,
};
use super::{StateStore, MAX_QUEUE_ITEMS};
use crate::app_error::AppError;
use crate::journal::now_ms;
use crate::models::{
    AddQueueItemInput, QueueItemRecord, QueueItemState, QueuePreparation, StateDelta,
    StateDeltaValue, UpdateQueueItemInput, VideoInfo, APP_SCHEMA_VERSION,
};
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
        let waiting_filename = permits_waiting_filename(&state, id, &input);
        let item = state
            .queue
            .get_mut(id)
            .ok_or_else(|| AppError::not_found("queue item"))?;
        if !item.state.is_editable() && !waiting_filename {
            return Err(AppError::new(
                "queue_item_active",
                "This download has already been claimed, or the requested settings cannot be changed while queued.",
            ));
        }
        if item.preparation == Some(QueuePreparation::Pending) {
            return Err(AppError::new(
                "queue_item_preparing",
                "A queue item cannot be edited while metadata preparation is pending.",
            ));
        }
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

fn permits_waiting_filename(
    state: &super::data::StateData,
    id: &str,
    input: &UpdateQueueItemInput,
) -> bool {
    if input.filename_override.is_none()
        || input.format.is_some()
        || input.quality.is_some()
        || input.output_dir.is_some()
    {
        return false;
    }
    state.queue.get(id).is_some_and(|item| {
        item.state == QueueItemState::Queued
            && item.preparation.is_none()
            && item
                .latest_operation_id
                .as_ref()
                .is_some_and(|operation_id| {
                    state.pending_downloads.contains(operation_id)
                        && state.operations.get(operation_id).is_some_and(|operation| {
                            operation.kind == crate::models::OperationKind::Download
                                && operation.state == crate::models::OperationState::Queued
                                && operation.queue_item_id.as_deref() == Some(id)
                        })
                })
    })
}
