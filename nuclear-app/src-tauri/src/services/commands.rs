use crate::app_error::AppError;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;

// A disconnected IPC caller must not abandon an admitted durable operation.
pub(crate) async fn run_tracked_command<T, F>(
    coordinator: &DownloadManager,
    kind: TrackedTaskKind,
    future: F,
) -> Result<T, AppError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, AppError>> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task_coordinator = coordinator.clone();
    coordinator.spawn_tracked(kind, async move {
        let result = AssertUnwindSafe(async move {
            task_coordinator.wait_for_startup().await?;
            future.await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(AppError::internal(
                "The backend command stopped unexpectedly.",
            ))
        });
        let _ = sender.send(result);
    })?;
    receiver
        .await
        .map_err(|_| AppError::internal("The backend command could not return its result."))?
}
