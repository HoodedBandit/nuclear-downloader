use super::commands::run_tracked_command;
use super::Backend;
use crate::app_error::AppError;
use crate::downloader;
use crate::lifecycle::TrackedTaskKind;
use crate::models::{
    AddQueueItemInput, AddQueueItemResult, BeginOperationResult, DownloadRequest, OperationKind,
    QueuePriority, UpdateQueueItemInput,
};

#[cfg(test)]
mod tests;

pub(crate) async fn add_inspection_result_to_queue(
    backend: Backend,
    mut input: AddQueueItemInput,
) -> Result<AddQueueItemResult, AppError> {
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        uuid::Uuid::parse_str(input.inspection_operation_id.trim())
            .map_err(|_| AppError::invalid("Invalid inspection operation ID."))?;
        if input.playlist.is_some() {
            return admit_playlist(&backend, input).await;
        }
        input.output_dir = downloader::validate_output_directory(&input.output_dir)?;
        let inspection = backend
            .state_store
            .completed_inspection_video(&input.inspection_operation_id)?;
        let request = DownloadRequest {
            expected_media_id: Some(inspection.id.clone()),
            url: inspection.url.clone(),
            quality: input.quality.clone(),
            format: input.format.clone(),
            output_dir: input.output_dir.clone(),
            cookie_config: input.cookie_config.clone(),
            filename_override: input.filename_override.clone(),
            compat_config_path: input.compat_config_path.clone(),
            selection: inspection.selection.clone(),
        };
        downloader::validate_download_request(&request).map_err(AppError::invalid)?;
        if !inspection.has_audio
            && matches!(
                input.format.as_str(),
                "mp3" | "flac" | "wav" | "aac" | "opus"
            )
        {
            return Err(AppError::invalid(
                "Audio-only output is unavailable because this item has no audio stream.",
            ));
        }
        let (item, _deltas) = backend.state_store.add_queue_item(input).await?;
        Ok(AddQueueItemResult::Single(Box::new(item)))
    })
    .await
}

async fn admit_playlist(
    backend: &Backend,
    input: AddQueueItemInput,
) -> Result<AddQueueItemResult, AppError> {
    if let Some(receipt) = backend.state_store.playlist_admission_receipt(&input)? {
        return Ok(AddQueueItemResult::Playlist(receipt));
    }
    let count = input
        .playlist
        .as_ref()
        .ok_or_else(|| AppError::invalid("Missing playlist selection."))?
        .entry_indices
        .len();
    if count == 0 || count > crate::state::MAX_QUEUE_ITEMS {
        return Err(AppError::invalid(
            "Select between 1 and 1,000 playlist entries.",
        ));
    }
    let canonical_output = downloader::validate_output_directory(&input.output_dir)?;
    let admission = backend.download_manager.begin_job_admission(count).await?;
    let (receipt, ids) = backend
        .state_store
        .add_playlist_items_at_output(input, canonical_output)
        .await?;
    let cleanup = crate::lifecycle_cleanup::QueueAdmissionGuard::new(
        backend.state_store.clone(),
        backend.download_manager.clone(),
        ids.clone(),
    );
    if let Err(error) = admission.publish_subset(&ids).await {
        return Err(cleanup.finalize(error).await);
    }
    cleanup.disarm();
    Ok(AddQueueItemResult::Playlist(receipt))
}

pub(crate) async fn update_queue_item(
    backend: Backend,
    item_id: String,
    mut input: UpdateQueueItemInput,
) -> Result<(), AppError> {
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let current = backend.state_store.queue_item(&item_id)?;
        let mut request = current.to_download_request();
        if let Some(format) = input.format.as_ref() {
            request.format.clone_from(format);
        }
        if let Some(quality) = input.quality.as_ref() {
            request.quality.clone_from(quality);
        }
        if let Some(output_dir) = input.output_dir.as_ref() {
            request.output_dir.clone_from(output_dir);
        }
        if let Some(filename_override) = input.filename_override.as_ref() {
            request.filename_override.clone_from(filename_override);
        }
        downloader::validate_download_request(&request).map_err(AppError::invalid)?;
        let canonical_output = downloader::validate_output_directory(&request.output_dir)?;
        request.output_dir.clone_from(&canonical_output);
        input.output_dir = Some(canonical_output);
        if !current.has_audio
            && matches!(
                request.format.as_str(),
                "mp3" | "flac" | "wav" | "aac" | "opus"
            )
        {
            return Err(AppError::invalid(
                "Audio-only output is unavailable because this item has no audio stream.",
            ));
        }
        let _deltas = backend
            .state_store
            .update_queue_item(&item_id, input)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn remove_queue_items(
    backend: Backend,
    item_ids: Vec<String>,
) -> Result<(), AppError> {
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        // Validate the complete removal before cancelling anything. Only
        // preparation attempts may be cancelled by removal; active downloads
        // retain the existing explicit-cancellation requirement.
        let mut preparations = Vec::new();
        for id in &item_ids {
            let item = backend.state_store.queue_item(id)?;
            if item.preparation.is_some() {
                if let Some(operation_id) = item.latest_operation_id.filter(|id| {
                    backend
                        .state_store
                        .operation_state(id)
                        .is_some_and(|state| !state.is_terminal())
                }) {
                    preparations.push(operation_id);
                }
            } else if !item.state.is_editable() {
                return Err(AppError::new(
                    "queue_item_active",
                    "A queued or running item cannot be removed.",
                ));
            }
        }
        let no_download_progress: crate::notifications::DownloadProgressSink =
            std::sync::Arc::new(|_| {});
        for operation_id in preparations {
            super::operations::cancel_known_operation(
                &no_download_progress,
                &backend,
                &operation_id,
                OperationKind::Inspection,
            )
            .await?;
        }
        let _deltas = backend.state_store.remove_queue_items(&item_ids).await?;
        Ok(())
    })
    .await
}

pub(crate) async fn enqueue_queue_items(
    backend: Backend,
    item_ids: Vec<String>,
    priority: QueuePriority,
) -> Result<Vec<BeginOperationResult>, AppError> {
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let admission = backend
            .download_manager
            .begin_job_admission(item_ids.len())
            .await?;
        let (work, _deltas) = backend.state_store.enqueue(&item_ids, priority).await?;
        let ids: Vec<_> = work.iter().map(|item| item.operation_id.clone()).collect();
        let cleanup = crate::lifecycle_cleanup::QueueAdmissionGuard::new(
            backend.state_store.clone(),
            backend.download_manager.clone(),
            ids.clone(),
        );
        if let Err(error) = admission.publish(&ids).await {
            return Err(cleanup.finalize(error).await);
        }
        cleanup.disarm();
        Ok(ids
            .into_iter()
            .map(|operation_id| BeginOperationResult { operation_id })
            .collect())
    })
    .await
}

pub(crate) fn default_download_dir() -> Result<String, AppError> {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|dir| dir.join("Downloads")))
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| AppError::internal("Could not determine a default downloads folder."))
}

pub(crate) fn validate_output_directory(path: String) -> Result<String, AppError> {
    downloader::validate_output_directory(&path).map(display_output_directory)
}

pub(crate) fn display_output_directory(path: String) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return rest.to_string();
        }
    }
    path
}
