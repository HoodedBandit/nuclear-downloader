use super::{begin_app_update_with_prepare, InstallerActions};
use crate::app_error::AppError;
use crate::models::{OperationKind, OperationState, UpdateInstallProgress};
use crate::notifications::UpdateProgressSink;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn verified_handoff_is_persisted_and_unregistered_before_exit() {
    let (backend, root) = crate::services::test_support::started_backend("handoff-order").await;
    let installer_path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let progress_events = Arc::clone(&events);
    let progress: UpdateProgressSink = Arc::new(move |payload: UpdateInstallProgress| {
        progress_events
            .lock()
            .unwrap()
            .push(format!("progress:{}", payload.status));
    });
    let (exit_sender, exit_receiver) = tokio::sync::oneshot::channel();
    let exit_sender = Arc::new(Mutex::new(Some(exit_sender)));
    let launch_backend = backend.clone();
    let launch_events = Arc::clone(&events);
    let exit_backend = backend.clone();
    let exit_events = Arc::clone(&events);
    let actions = InstallerActions {
        launch: Arc::new(move |handoff| {
            let snapshot = launch_backend.state_store.snapshot().unwrap();
            assert!(snapshot.maintenance_active);
            assert!(snapshot.operations.iter().any(|operation| {
                operation.kind == OperationKind::AppUpdate
                    && operation.state == OperationState::Running
                    && operation.phase.as_deref() == Some("installing")
            }));
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(handoff.installer_path())
                .is_err());
            launch_events.lock().unwrap().push("launch".into());
            Ok(())
        }),
        exit: Arc::new(move || {
            // Publication has committed and the initiating TrackedUpdate was
            // dropped before this callback, so its registry entry is gone.
            let operation_id = exit_backend
                .state_store
                .snapshot()
                .unwrap()
                .operations
                .into_iter()
                .find(|operation| operation.kind == OperationKind::AppUpdate)
                .unwrap()
                .id;
            assert_eq!(
                exit_backend
                    .download_manager
                    .cancel_update(&operation_id)
                    .unwrap_err()
                    .code,
                "not_found"
            );
            assert!(
                exit_backend
                    .state_store
                    .snapshot()
                    .unwrap()
                    .maintenance_active
            );
            exit_events.lock().unwrap().push("exit".into());
            if let Some(sender) = exit_sender.lock().unwrap().take() {
                let _ = sender.send(operation_id);
            }
        }),
    };
    let fixture_root = root.clone();

    let admission = begin_app_update_with_prepare(
        backend.clone(),
        "0.5.0".into(),
        "0.6.0".into(),
        progress,
        actions,
        move |_current, _progress, expected, _context| async move {
            crate::updater::test_installer_handoff(&fixture_root, &expected).await
        },
    );
    let admitted = tokio::time::timeout(Duration::from_secs(2), admission)
        .await
        .expect("the service must admit the prepared update after startup")
        .unwrap();
    let exited_id = tokio::time::timeout(Duration::from_secs(2), exit_receiver)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exited_id, admitted.operation_id);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["progress:launching", "launch", "exit"]
    );
    assert!(backend
        .download_manager
        .begin_job_admission(1)
        .await
        .is_err());
    let snapshot = backend.state_store.snapshot().unwrap();
    assert!(snapshot.operations.iter().any(|operation| {
        operation.id == exited_id
            && operation.state == OperationState::Running
            && operation.phase.as_deref() == Some("installing")
    }));

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::OpenOptions::new()
                .write(true)
                .open(&installer_path)
                .is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the verified installer lease should be released after the exit callback");

    backend.download_manager.begin_shutdown().await;
    backend
        .download_manager
        .wait_for_shutdown_tasks(Duration::from_secs(2))
        .await
        .unwrap();
    drop(snapshot);
    drop(backend);
    drop(events);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn failed_installer_launch_finalizes_and_reopens_admission_without_exit() {
    let (backend, root) = crate::services::test_support::started_backend("handoff-failure").await;
    let exits = Arc::new(AtomicUsize::new(0));
    let exit_count = Arc::clone(&exits);
    let actions = InstallerActions {
        launch: Arc::new(|_| Err(AppError::internal("forced launch failure"))),
        exit: Arc::new(move || {
            exit_count.fetch_add(1, Ordering::SeqCst);
        }),
    };
    let fixture_root = root.clone();
    let admission = begin_app_update_with_prepare(
        backend.clone(),
        "0.5.0".into(),
        "0.6.0".into(),
        Arc::new(|_| {}),
        actions,
        move |_current, _progress, expected, _context| async move {
            crate::updater::test_installer_handoff(&fixture_root, &expected).await
        },
    );
    let admitted = tokio::time::timeout(Duration::from_secs(2), admission)
        .await
        .expect("the service must admit the prepared update after startup")
        .unwrap();
    let terminal = backend
        .state_store
        .wait_for_terminal(&admitted.operation_id, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(terminal.state, OperationState::Failed);
    assert_eq!(
        terminal.error.as_ref().map(|error| error.summary.as_str()),
        Some("forced launch failure")
    );
    assert_eq!(exits.load(Ordering::SeqCst), 0);

    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.state_store.snapshot().unwrap().maintenance_active {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the failed handoff should release its maintenance lease");
    let admission = backend
        .download_manager
        .begin_job_admission(1)
        .await
        .unwrap();
    drop(admission);

    backend.download_manager.begin_shutdown().await;
    backend
        .download_manager
        .wait_for_shutdown_tasks(Duration::from_secs(2))
        .await
        .unwrap();
    drop(terminal);
    drop(backend);
    drop(exits);
    std::fs::remove_dir_all(&root).unwrap();
}
