use super::commands::run_tracked_command;
use super::downloads::finalize_download;
use super::Backend;
use crate::app_error::AppError;
use crate::cancellation::CANCELLATION_WAIT_TIMEOUT;
use crate::lifecycle::TrackedTaskKind;
use crate::models::{CancelAllResult, OperationKind};
use crate::notifications::DownloadProgressSink;
use crate::{cancellation, downloader, lifecycle};

pub(crate) async fn cancel_all_downloads(
    backend: Backend,
    publish_progress: DownloadProgressSink,
) -> Result<CancelAllResult, AppError> {
    cancellation::cancel_all(
        backend.state_store,
        backend.download_manager,
        publish_progress,
    )
    .await
}
pub(crate) async fn cancel_operation(
    publish_progress: DownloadProgressSink,
    backend: Backend,
    operation_id: String,
) -> Result<(), AppError> {
    uuid::Uuid::parse_str(operation_id.trim())
        .map_err(|_| AppError::invalid("Invalid operation ID."))?;
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Drain, async move {
        let kind = backend
            .state_store
            .operation_kind(&operation_id)
            .ok_or_else(|| AppError::not_found("operation"))?;
        if backend
            .state_store
            .operation_state(&operation_id)
            .is_some_and(|state| state.is_terminal())
        {
            return Ok(());
        }
        let deferred = if matches!(
            kind,
            OperationKind::RuntimeUpdate | OperationKind::AppUpdate
        ) {
            backend.download_manager.cancel_update(&operation_id)?
                == lifecycle::UpdateCancellation::DeferredPublication
        } else {
            match backend.download_manager.cancel(&operation_id).await {
                Ok(()) => {}
                Err(error)
                    if error.code == "not_found"
                        && backend
                            .state_store
                            .operation_state(&operation_id)
                            .is_some_and(|state| state.is_terminal()) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
            false
        };
        if !deferred {
            match backend
                .state_store
                .request_cancellation(&operation_id)
                .await
            {
                Ok(_delta) => {}
                Err(_)
                    if backend
                        .state_store
                        .operation_state(&operation_id)
                        .is_some_and(|state| state.is_terminal()) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
        }
        if kind == OperationKind::Download
            && backend.state_store.cancel_pending(&operation_id).await
        {
            finalize_download(
                &publish_progress,
                &backend.state_store,
                &operation_id,
                downloader::DownloadOutcome::Cancelled,
            )
            .await;
            backend.download_manager.finish(&operation_id).await;
            return Ok(());
        }
        backend
            .state_store
            .wait_for_terminal(&operation_id, CANCELLATION_WAIT_TIMEOUT)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn dismiss_operation(
    state: Backend,
    operation_id: String,
) -> Result<(), AppError> {
    let coordinator = state.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let _deltas = state.state_store.dismiss_operation(&operation_id).await?;
        Ok(())
    })
    .await
}
