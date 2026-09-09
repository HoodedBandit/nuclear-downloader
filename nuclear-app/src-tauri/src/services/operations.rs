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
        let result = cancel_known_operation(&publish_progress, &backend, &operation_id, kind).await;
        normalize_after_known_operation(&backend.state_store, &operation_id, result).map(|_| ())
    })
    .await
}

async fn cancel_known_operation(
    publish_progress: &DownloadProgressSink,
    backend: &Backend,
    operation_id: &str,
    kind: OperationKind,
) -> Result<(), AppError> {
    let deferred = if matches!(
        kind,
        OperationKind::RuntimeUpdate | OperationKind::AppUpdate
    ) {
        backend.download_manager.cancel_update(operation_id)?
            == lifecycle::UpdateCancellation::DeferredPublication
    } else {
        backend.download_manager.cancel(operation_id).await?;
        false
    };
    if !deferred {
        match backend.state_store.request_cancellation(operation_id).await {
            Ok(_delta) => {}
            Err(_)
                if backend
                    .state_store
                    .operation_state(operation_id)
                    .is_some_and(|state| state.is_terminal()) =>
            {
                return Ok(())
            }
            Err(error) => return Err(error),
        }
    }
    if kind == OperationKind::Download && backend.state_store.cancel_pending(operation_id).await {
        finalize_download(
            publish_progress,
            &backend.state_store,
            operation_id,
            downloader::DownloadOutcome::Cancelled,
        )
        .await;
        backend.download_manager.finish(operation_id).await;
        return Ok(());
    }
    backend
        .state_store
        .wait_for_terminal(operation_id, CANCELLATION_WAIT_TIMEOUT)
        .await?;
    Ok(())
}

// The caller has already proved that `operation_id` existed. `None` is
// reserved for an operation that finished after that proof.
fn normalize_after_known_operation<T>(
    store: &crate::state::StateStore,
    operation_id: &str,
    result: Result<T, AppError>,
) -> Result<Option<T>, AppError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code == "not_found" => {
            let snapshot = store.snapshot()?;
            match snapshot
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)
            {
                Some(operation) if !operation.state.is_terminal() => Err(error),
                Some(_) | None => Ok(None),
            }
        }
        Err(error) => Err(error),
    }
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

#[cfg(test)]
mod tests;
