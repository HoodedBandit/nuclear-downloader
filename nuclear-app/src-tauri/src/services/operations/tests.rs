use super::{cancel_operation, normalize_after_known_operation};
use crate::app_error::AppError;
use crate::lifecycle::{PublicationKind, UpdateCancellation};
use crate::models::{OperationKind, OperationState};
use crate::notifications::DownloadProgressSink;
use crate::services::test_support::started_backend;
use std::sync::Arc;

async fn update_fixture(
    label: &str,
) -> (
    crate::services::Backend,
    std::path::PathBuf,
    String,
    crate::lifecycle::TrackedUpdate,
    crate::lifecycle::UpdateTaskContext,
) {
    let (backend, root) = started_backend(label).await;
    let (operation, _) = backend
        .state_store
        .begin_operation(OperationKind::RuntimeUpdate, None)
        .await
        .unwrap();
    let id = operation.id;
    let (tracked, context) = backend
        .download_manager
        .register_update(id.clone(), OperationKind::RuntimeUpdate)
        .unwrap();
    (backend, root, id, tracked, context)
}

#[tokio::test]
async fn known_terminal_update_treats_missing_registration_as_finished() {
    let (backend, root, id, tracked, context) = update_fixture("cancel-terminal").await;
    backend
        .state_store
        .finalize_operation(&id, OperationState::Cancelled, None)
        .await
        .unwrap();
    drop(tracked);

    assert_eq!(
        normalize_after_known_operation(
            &backend.state_store,
            &id,
            backend.download_manager.cancel_update(&id),
        )
        .unwrap(),
        None
    );
    drop(context);
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn known_dismissed_update_treats_missing_registration_as_finished() {
    let (backend, root, id, tracked, context) = update_fixture("cancel-dismissed").await;
    backend
        .state_store
        .finalize_operation(&id, OperationState::Cancelled, None)
        .await
        .unwrap();
    backend.state_store.dismiss_operation(&id).await.unwrap();
    drop(tracked);

    assert_eq!(
        normalize_after_known_operation(
            &backend.state_store,
            &id,
            backend.download_manager.cancel_update(&id),
        )
        .unwrap(),
        None
    );
    drop(context);
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn live_update_without_registration_preserves_not_found() {
    let (backend, root, id, tracked, context) = update_fixture("cancel-orphan").await;
    drop(tracked);

    let error = normalize_after_known_operation(
        &backend.state_store,
        &id,
        backend.download_manager.cancel_update(&id),
    )
    .unwrap_err();
    assert_eq!(error.code, "not_found");
    let busy = AppError::busy("still busy");
    let preserved =
        normalize_after_known_operation::<()>(&backend.state_store, &id, Err(busy.clone()))
            .unwrap_err();
    assert_eq!(preserved.code, busy.code);
    assert_eq!(preserved.summary, busy.summary);
    drop(context);
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn live_update_reports_requested_and_deferred_publication() {
    let (backend, root, id, tracked, context) = update_fixture("cancel-live").await;
    assert_eq!(
        normalize_after_known_operation(
            &backend.state_store,
            &id,
            backend.download_manager.cancel_update(&id),
        )
        .unwrap(),
        Some(UpdateCancellation::Requested)
    );
    drop(tracked);
    drop(context);

    let (backend2, root2, id2, tracked2, context2) = update_fixture("cancel-deferred").await;
    let publication = context2
        .enter_publication(PublicationKind::RuntimeCommit)
        .unwrap();
    assert_eq!(
        normalize_after_known_operation(
            &backend2.state_store,
            &id2,
            backend2.download_manager.cancel_update(&id2),
        )
        .unwrap(),
        Some(UpdateCancellation::DeferredPublication)
    );
    drop(publication);
    drop(tracked2);
    drop(context2);
    drop(backend);
    drop(backend2);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(root2).unwrap();
}

#[tokio::test]
async fn initially_unknown_operation_remains_not_found() {
    let (backend, root) = started_backend("cancel-unknown").await;
    let progress: DownloadProgressSink = Arc::new(|_| {});
    let error = cancel_operation(progress, backend.clone(), uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found");
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
}
