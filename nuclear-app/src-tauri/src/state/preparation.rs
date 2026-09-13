use super::data::StateData;
use crate::app_error::AppError;
use crate::models::{
    OperationState, QueueItemRecord, QueueItemState, QueuePreparation, UrlInspection, VideoInfo,
};

pub(super) fn validated_preparation_video(
    state: &StateData,
    operation_id: &str,
    inspection: Option<&UrlInspection>,
) -> Result<Option<VideoInfo>, AppError> {
    let Some(inspection) = inspection else {
        return Ok(None);
    };
    let operation = state
        .operations
        .get(operation_id)
        .ok_or_else(|| AppError::not_found("operation"))?;
    let Some(queue_item_id) = operation.queue_item_id.as_ref() else {
        return Ok(None);
    };
    let item = state
        .queue
        .get(queue_item_id)
        .ok_or_else(|| AppError::not_found("queue item"))?;
    if item.preparation != Some(QueuePreparation::Pending)
        || item.latest_operation_id.as_deref() != Some(operation_id)
        || operation.state == OperationState::Cancelling
    {
        return Err(AppError::new(
            "stale_preparation",
            "This metadata preparation attempt is no longer authoritative.",
        ));
    }
    let video = match inspection {
        UrlInspection::Video { video } => video,
        UrlInspection::Playlist { .. } => {
            return Err(AppError::new(
                "inspection_result_kind",
                "Metadata preparation returned a playlist instead of a video.",
            ));
        }
    };
    if item
        .source_media_id
        .as_deref()
        .is_some_and(|expected| expected != video.id)
        || video.selection != item.selection
    {
        return Err(AppError::new(
            "preparation_identity_mismatch",
            "Prepared metadata did not match the queue item identity.",
        ));
    }
    if !video.has_audio
        && matches!(
            item.format.as_str(),
            "mp3" | "flac" | "wav" | "aac" | "opus"
        )
    {
        return Err(AppError::invalid(
            "Audio-only output is unavailable because this item has no audio stream.",
        ));
    }
    Ok(Some(video.clone()))
}

pub(super) fn apply_prepared_video(item: &mut QueueItemRecord, video: VideoInfo) {
    item.title = video.title;
    item.available_qualities = video.available_qualities;
    item.has_audio = video.has_audio;
    item.source_media_id = Some(video.id);
    item.selection = video.selection;
    item.preparation = None;
    item.latest_operation_id = None;
    item.preparation_operation_id = None;
    item.state = QueueItemState::Inert;
}
