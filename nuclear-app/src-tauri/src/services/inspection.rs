use super::commands::run_tracked_command;
use super::Backend;
use crate::app_error::AppError;
use crate::downloader;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::lifecycle_cleanup::InspectionAdmissionGuard;
use crate::models::{
    self, BeginInspectionInput, BeginOperationResult, OperationKind, OperationState,
};
use crate::state::StateStore;
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

pub(crate) async fn begin_inspection(
    backend: Backend,
    input: BeginInspectionInput,
) -> Result<BeginOperationResult, AppError> {
    downloader::validate_fetch_request(
        &input.url,
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
    )
    .map_err(AppError::invalid)?;
    if let Some(selection) = &input.selection {
        selection.validate().map_err(AppError::invalid)?;
    }
    let coordinator = backend.download_manager.clone();
    run_tracked_command(&coordinator, TrackedTaskKind::Admission, async move {
        let admission = backend.download_manager.begin_job_admission(1).await?;
        let (operation, _deltas) = backend
            .state_store
            .begin_operation(OperationKind::Inspection, None)
            .await?;
        let operation_id = operation.id;
        let admission_cleanup = InspectionAdmissionGuard::new(
            backend.state_store.clone(),
            backend.download_manager.clone(),
            operation_id.clone(),
        );
        let published = match admission.publish(std::slice::from_ref(&operation_id)).await {
            Ok(jobs) => jobs,
            Err(error) => return Err(admission_cleanup.finalize(error).await),
        };
        let job = match published.into_iter().next() {
            Some(job) => job,
            None => {
                let error = AppError::internal("The inspection was not registered.");
                return Err(admission_cleanup.finalize(error).await);
            }
        };
        let result_id = operation_id.clone();
        let task_store = backend.state_store.clone();
        let manager = backend.download_manager.clone();
        let task = async move {
            let result = AssertUnwindSafe(bounded_inspection(
                &job,
                execute_inspection(&task_store, &manager, &result_id, input, &job),
                downloader::inspection::INSPECTION_TIMEOUT,
            ))
            .catch_unwind()
            .await;
            let result = match result {
                Ok(result) => result,
                Err(_) => {
                    job.terminate_processes();
                    Err(AppError::internal(
                        "The inspection worker stopped unexpectedly.",
                    ))
                }
            };
            let finalized = finalize_inspection_result(&task_store, &result_id, result).await;
            match finalized {
                Ok(_deltas) => {}
                Err(error) => task_store.diagnostics().log(
                    "error",
                    "inspection_finalization_failed",
                    &error.correlation_id,
                    &error.summary,
                ),
            }
            manager.finish(&result_id).await;
        };
        if let Err(error) = backend
            .download_manager
            .spawn_tracked(TrackedTaskKind::Inspection, task)
        {
            return Err(admission_cleanup.finalize(error).await);
        }
        admission_cleanup.disarm();
        Ok(BeginOperationResult { operation_id })
    })
    .await
}

pub(crate) async fn finalize_inspection_result(
    store: &StateStore,
    operation_id: &str,
    result: Result<Option<models::UrlInspection>, AppError>,
) -> Result<Vec<models::StateDelta>, AppError> {
    let error = match result {
        Ok(Some(inspection)) => match store.complete_inspection(operation_id, inspection).await {
            Ok(deltas) => return Ok(deltas),
            Err(_) if store.operation_state(operation_id) == Some(OperationState::Cancelling) => {
                return store
                    .finalize_operation(operation_id, OperationState::Cancelled, None)
                    .await;
            }
            Err(error) => error,
        },
        Ok(None) => {
            return store
                .finalize_operation(operation_id, OperationState::Cancelled, None)
                .await
        }
        Err(error) => error,
    };
    // Rejected inspection metadata or resource limits still require a
    // terminal state before the registered worker can be released.
    store
        .finalize_operation(operation_id, OperationState::Failed, Some(error))
        .await
}

async fn bounded_inspection<F, T>(
    job: &downloader::process::DownloadJob,
    future: F,
    timeout: Duration,
) -> Result<T, AppError>
where
    F: Future<Output = Result<T, AppError>>,
{
    tokio::pin!(future);
    match tokio::time::timeout(timeout, &mut future).await {
        Ok(result) => result,
        Err(_) => {
            job.terminate_processes();
            // Retain the child owner long enough to observe exit and release
            // its pipes/permit, even if the outer inspection deadline expired.
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut future).await;
            Err(AppError::new(
                "inspection_timeout",
                "The complete inspection exceeded its 120-second deadline.",
            )
            .retryable(true))
        }
    }
}

async fn execute_inspection(
    store: &StateStore,
    manager: &DownloadManager,
    operation_id: &str,
    input: BeginInspectionInput,
    job: &downloader::process::DownloadJob,
) -> Result<Option<models::UrlInspection>, AppError> {
    if job.is_cancelled() {
        return Ok(None);
    }
    let Some(_permit) = manager
        .acquire_inspection(job)
        .await
        .map_err(AppError::internal)?
    else {
        return Ok(None);
    };
    if job.is_cancelled() {
        return Ok(None);
    }
    match store
        .set_operation_state(operation_id, OperationState::Running, None)
        .await
    {
        Ok(_deltas) => {}
        Err(_)
            if job.is_cancelled()
                || store.operation_state(operation_id) == Some(OperationState::Cancelling) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    }
    match downloader::inspection::inspect_url(
        &input.url,
        input.cookie_config.as_ref(),
        input.compat_config_path.as_deref(),
        input.selection.as_ref(),
        job,
    )
    .await
    {
        Ok(inspection) => Ok(Some(inspection)),
        Err(_) if job.is_cancelled() => Ok(None),
        Err(summary) => Err(AppError::new("inspection_failed", summary).retryable(true)),
    }
}
