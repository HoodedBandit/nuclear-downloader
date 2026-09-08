use super::{
    DrainCompletion, LifecycleCoordinator, PublicationKind, TrackedTaskKind, UpdateCancellation,
};
use crate::app_error::AppError;
use crate::models::OperationKind;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

#[tokio::test]
async fn startup_gate_blocks_admission_and_worker_claim_until_open() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    assert!(lifecycle.begin_job_admission(1).await.is_err());

    let worker_guard = lifecycle
        .register_task(TrackedTaskKind::Worker, None, false)
        .unwrap();
    let claim_lifecycle = lifecycle.clone();
    let claim = tokio::spawn(async move { claim_lifecycle.wait_worker_claim().await.unwrap() });
    tokio::task::yield_now().await;
    assert!(!claim.is_finished());

    lifecycle.open_after_startup().await.unwrap();
    assert!(claim.await.unwrap().is_some());
    drop(worker_guard);
}

#[tokio::test]
async fn tracked_startup_task_opens_admission_before_it_unregisters() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    let shutdown = lifecycle.shutdown_token();
    lifecycle
        .spawn_tracked(TrackedTaskKind::Worker, async move {
            shutdown.cancelled().await;
        })
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    lifecycle
        .spawn_startup(async { Ok(()) }, move |result| {
            let _ = sender.send(result);
        })
        .unwrap();

    receiver.await.unwrap().unwrap();
    let job = lifecycle.register("after-startup").await.unwrap();
    assert!(!job.is_cancelled());
    lifecycle.finish("after-startup").await;
    lifecycle.begin_shutdown().await;
    lifecycle
        .wait_for_shutdown_tasks(Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn startup_waiter_blocks_during_cleanup_and_receives_failure() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (finish_sender, finish_receiver) = tokio::sync::oneshot::channel();
    lifecycle
        .spawn_startup(
            async move {
                let _ = started_sender.send(());
                let _ = finish_receiver.await;
                Err(AppError::internal("Startup recovery failed."))
            },
            |_| {},
        )
        .unwrap();
    started_receiver.await.unwrap();

    let waiting_lifecycle = lifecycle.clone();
    let waiter = tokio::spawn(async move { waiting_lifecycle.wait_for_startup().await });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    finish_sender.send(()).unwrap();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.summary, "Startup recovery failed.");
}

#[tokio::test]
async fn startup_waiter_returns_typed_shutdown_error() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.begin_shutdown().await;

    let error = lifecycle.wait_for_startup().await.unwrap_err();
    assert_eq!(error.code, "shutting_down");
}

#[tokio::test]
async fn shutdown_during_startup_never_reopens_admission() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    let worker_guard = lifecycle
        .register_task(TrackedTaskKind::Worker, None, false)
        .unwrap();

    lifecycle.begin_shutdown().await;
    assert!(lifecycle.open_after_startup().await.is_err());
    assert!(lifecycle.begin_job_admission(1).await.is_err());
    assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());
    drop(worker_guard);
}

#[tokio::test]
async fn cancellation_waits_for_atomic_job_publication() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let admission = lifecycle.begin_job_admission(1).await.unwrap();
    let operation_id = "operation".to_string();
    let cancel_started = Arc::new(Notify::new());
    let cancel_lifecycle = lifecycle.clone();
    let cancel_id = operation_id.clone();
    let cancel_signal = cancel_started.clone();
    let cancellation = tokio::spawn(async move {
        cancel_signal.notify_one();
        cancel_lifecycle.cancel(&cancel_id).await
    });
    cancel_started.notified().await;
    tokio::task::yield_now().await;
    assert!(!cancellation.is_finished());

    let jobs = admission
        .publish(std::slice::from_ref(&operation_id))
        .await
        .unwrap();
    cancellation.await.unwrap().unwrap();
    assert!(jobs[0].is_cancelled());
    lifecycle.finish(&operation_id).await;
    assert_eq!(lifecycle.active_count().await, 0);
}

#[tokio::test]
async fn cancellation_signals_a_registered_job_while_worker_claim_is_held() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let job = lifecycle.register("dequeued").await.unwrap();
    let claim = lifecycle.wait_worker_claim().await.unwrap().unwrap();

    tokio::time::timeout(Duration::from_millis(100), lifecycle.cancel("dequeued"))
        .await
        .expect("registered job cancellation must not wait for the worker claim")
        .unwrap();
    assert!(job.is_cancelled());

    drop(claim);
    lifecycle.finish("dequeued").await;
}

#[tokio::test]
async fn shutdown_closes_immediately_while_admission_owns_transition_gate() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let admission = lifecycle.begin_job_admission(1).await.unwrap();

    tokio::time::timeout(Duration::from_millis(100), lifecycle.begin_shutdown())
        .await
        .expect("shutdown must not wait for a durable admission commit");
    let error = admission
        .publish(&["committed-during-shutdown".to_string()])
        .await
        .err()
        .expect("publication must fail after shutdown starts");
    assert_eq!(error.code, "shutting_down");
    assert_eq!(lifecycle.active_count().await, 0);
}

#[tokio::test]
async fn shutdown_after_worker_claim_returns_the_existing_job_cancelled() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    lifecycle.register("dequeued").await.unwrap();
    let claim = lifecycle.wait_worker_claim().await.unwrap().unwrap();

    lifecycle.begin_shutdown().await;
    let job = claim.registered_job("dequeued").await.unwrap();
    assert!(job.is_cancelled());
    drop(claim);
    lifecycle.finish("dequeued").await;
}

#[tokio::test]
async fn batch_admission_and_maintenance_cannot_partially_interleave() {
    let lifecycle = LifecycleCoordinator::new(3, 1, 1);
    lifecycle.open_for_test().await;
    let admission = lifecycle.begin_job_admission(3).await.unwrap();
    let maintenance_lifecycle = lifecycle.clone();
    let started = Arc::new(Barrier::new(2));
    let maintenance_started = started.clone();
    let maintenance = tokio::spawn(async move {
        maintenance_started.wait().await;
        maintenance_lifecycle.acquire_maintenance().await
    });
    started.wait().await;
    tokio::task::yield_now().await;
    assert!(!maintenance.is_finished());

    let ids = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    admission.publish(&ids).await.unwrap();
    let error = match maintenance.await.unwrap() {
        Ok(_) => panic!("maintenance unexpectedly acquired"),
        Err(error) => error,
    };
    assert_eq!(error.code, "busy");
    let mut expected_ids = ids.clone();
    expected_ids.sort();
    assert_eq!(lifecycle.active_ids().await, expected_ids);
    for id in &ids {
        lifecycle.finish(id).await;
    }
}

#[tokio::test]
async fn cancellation_is_idempotent_until_a_registered_job_finishes() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let job = lifecycle.register("cancel-before-spawn").await.unwrap();

    lifecycle.cancel("cancel-before-spawn").await.unwrap();
    lifecycle.cancel("cancel-before-spawn").await.unwrap();
    assert!(job.is_cancelled());
    lifecycle.finish("cancel-before-spawn").await;
    assert!(lifecycle.cancel("cancel-before-spawn").await.is_err());
    assert_eq!(lifecycle.active_count().await, 0);
}

#[tokio::test]
async fn duplicate_ids_and_active_jobs_exclude_maintenance() {
    let lifecycle = LifecycleCoordinator::new(2, 1, 1);
    lifecycle.open_for_test().await;
    lifecycle.register("same-id").await.unwrap();

    assert!(lifecycle.register("same-id").await.is_err());
    assert!(lifecycle.acquire_maintenance().await.is_err());
    lifecycle.finish("same-id").await;

    let lease = lifecycle.acquire_maintenance().await.unwrap();
    assert!(lifecycle.register("paused").await.is_err());
    lease.release().await;
    lifecycle.register("resumed").await.unwrap();
    lifecycle.finish("resumed").await;
}

#[tokio::test]
async fn download_and_conversion_slots_preserve_backpressure() {
    let lifecycle = LifecycleCoordinator::new(2, 1, 1);
    lifecycle.open_for_test().await;
    let first_job = lifecycle.register("first").await.unwrap();
    let second_job = lifecycle.register("second").await.unwrap();
    let first_download = lifecycle.acquire_download_slot().await.unwrap();
    let second_download = lifecycle.acquire_download_slot().await.unwrap();
    let first_conversion = lifecycle
        .acquire_conversion(&first_job)
        .await
        .unwrap()
        .unwrap();

    let download_waiter = lifecycle.acquire_download_slot();
    tokio::pin!(download_waiter);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut download_waiter)
            .await
            .is_err()
    );
    let conversion_waiter = lifecycle.acquire_conversion(&second_job);
    tokio::pin!(conversion_waiter);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut conversion_waiter)
            .await
            .is_err()
    );

    drop(first_download);
    drop(first_conversion);
    assert!(download_waiter.await.is_ok());
    assert!(conversion_waiter.await.unwrap().is_some());
    drop(second_download);
    lifecycle.finish("first").await;
    lifecycle.finish("second").await;
}

#[tokio::test]
async fn cancel_all_cannot_release_an_update_maintenance_lease() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let lease = lifecycle.acquire_maintenance().await.unwrap();

    assert!(lifecycle.begin_cancel_all().await.is_err());
    assert!(lifecycle.register("must-remain-paused").await.is_err());
    lease.release().await;
    lifecycle.register("after-update").await.unwrap();
    lifecycle.finish("after-update").await;
}

#[tokio::test]
async fn late_cancel_all_completion_resumes_the_same_generation() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let job = lifecycle.register("slow").await.unwrap();

    let ticket = lifecycle.begin_cancel_all().await.unwrap();
    assert!(job.is_cancelled());
    assert!(lifecycle.begin_job_admission(1).await.is_err());
    lifecycle.finish("slow").await;
    assert_eq!(
        lifecycle.wait_for_drain(ticket).await,
        DrainCompletion::Idle
    );
    assert!(lifecycle.resume_after_drain(ticket).await);

    let resumed = lifecycle.register("resumed").await.unwrap();
    assert!(!resumed.is_cancelled());
    lifecycle.finish("resumed").await;
}

#[tokio::test]
async fn a_second_cancel_all_cannot_share_or_resume_an_active_generation() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    lifecycle.register("slow").await.unwrap();
    let ticket = lifecycle.begin_cancel_all().await.unwrap();

    let error = lifecycle.begin_cancel_all().await.unwrap_err();
    assert_eq!(error.code, "busy");
    lifecycle.finish("slow").await;
    assert_eq!(
        lifecycle.wait_for_drain(ticket).await,
        DrainCompletion::Idle
    );
    assert!(lifecycle.resume_after_drain(ticket).await);
}

#[tokio::test]
async fn shutdown_supersedes_a_late_cancel_all_completion() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    lifecycle.register("slow").await.unwrap();
    let ticket = lifecycle.begin_cancel_all().await.unwrap();

    lifecycle.begin_shutdown().await;
    lifecycle.finish("slow").await;
    assert_eq!(
        lifecycle.wait_for_drain(ticket).await,
        DrainCompletion::SupersededByShutdown
    );
    assert!(!lifecycle.resume_after_drain(ticket).await);
    assert!(lifecycle.begin_job_admission(1).await.is_err());
}

#[tokio::test]
async fn versioned_maintenance_release_cannot_reopen_shutdown() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let lease = lifecycle.acquire_maintenance().await.unwrap();

    lifecycle.begin_shutdown().await;
    lease.release().await;
    assert!(lifecycle.begin_job_admission(1).await.is_err());
    assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());
}

#[tokio::test]
async fn publication_defers_update_cancellation_and_shutdown_deadline() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let (tracked, context) = lifecycle
        .register_update("runtime-update".to_string(), OperationKind::RuntimeUpdate)
        .unwrap();
    let publication = context
        .enter_publication(PublicationKind::RuntimeCommit)
        .unwrap();

    assert_eq!(
        lifecycle.cancel_update("runtime-update").unwrap(),
        UpdateCancellation::DeferredPublication
    );
    assert!(!context.is_cancelled());
    lifecycle.begin_shutdown().await;

    let waiting_lifecycle = lifecycle.clone();
    let waiter = tokio::spawn(async move {
        waiting_lifecycle
            .wait_for_shutdown_tasks(Duration::ZERO)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    drop(publication);
    context.cancelled().await;
    drop(tracked);
    assert!(waiter.await.unwrap().is_ok());
}

#[tokio::test]
async fn committed_publication_cannot_be_cancelled_before_terminal_finalization() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let (tracked, context) = lifecycle
        .register_update("published-update".to_string(), OperationKind::RuntimeUpdate)
        .unwrap();
    context
        .enter_publication(PublicationKind::RuntimeCommit)
        .unwrap()
        .commit();

    assert_eq!(
        lifecycle.cancel_update("published-update").unwrap(),
        UpdateCancellation::DeferredPublication
    );
    assert!(!context.is_cancelled());
    drop(tracked);
    let job = lifecycle.register("after-runtime-commit").await.unwrap();
    assert!(!job.is_cancelled());
    lifecycle.finish("after-runtime-commit").await;
}

#[tokio::test]
async fn committed_installer_handoff_permanently_closes_admission() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;

    let cache_lease = lifecycle.acquire_maintenance().await.unwrap();
    let (cache_task, cache_context) = lifecycle
        .register_update("cache-only".to_string(), OperationKind::AppUpdate)
        .unwrap();
    drop(
        cache_context
            .enter_publication(PublicationKind::InstallerCache)
            .unwrap(),
    );
    drop(cache_task);
    cache_lease.release().await;
    let cache_did_not_latch = lifecycle.register("after-cache").await.unwrap();
    assert!(!cache_did_not_latch.is_cancelled());
    lifecycle.finish("after-cache").await;

    let handoff_lease = lifecycle.acquire_maintenance().await.unwrap();
    let (handoff_task, handoff_context) = lifecycle
        .register_update("installer-handoff".to_string(), OperationKind::AppUpdate)
        .unwrap();
    handoff_context
        .enter_publication(PublicationKind::InstallerHandoff)
        .unwrap()
        .commit();
    drop(handoff_task);
    handoff_lease.release().await;

    assert!(lifecycle.begin_job_admission(1).await.is_err());
    assert!(lifecycle.acquire_maintenance().await.is_err());
    assert!(lifecycle
        .register_update("too-late".to_string(), OperationKind::RuntimeUpdate)
        .is_err());
    assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());

    lifecycle.begin_shutdown().await;
    lifecycle
        .wait_for_shutdown_tasks(Duration::from_millis(100))
        .await
        .unwrap();
}

#[tokio::test]
async fn shutdown_accounts_for_cleanup_and_rejects_unrelated_new_tasks() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let cleanup = lifecycle
        .register_task(TrackedTaskKind::Cleanup, None, false)
        .unwrap();
    lifecycle.begin_shutdown().await;
    assert!(lifecycle
        .register_task(TrackedTaskKind::Cleanup, None, false)
        .is_err());
    let continuation = lifecycle
        .register_task(TrackedTaskKind::Cleanup, None, true)
        .unwrap();

    let waiting_lifecycle = lifecycle.clone();
    let waiter = tokio::spawn(async move {
        waiting_lifecycle
            .wait_for_shutdown_tasks(Duration::from_secs(60))
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    drop(cleanup);
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    drop(continuation);
    assert!(waiter.await.unwrap().is_ok());
}

#[tokio::test]
async fn event_pump_waits_for_producers_without_waiting_for_itself() {
    let lifecycle = LifecycleCoordinator::new(1, 1, 1);
    lifecycle.open_for_test().await;
    let event_pump = lifecycle
        .register_task(TrackedTaskKind::Events, None, false)
        .unwrap();
    let producer = lifecycle
        .register_task(TrackedTaskKind::Admission, None, false)
        .unwrap();

    lifecycle.begin_shutdown().await;
    let waiting_lifecycle = lifecycle.clone();
    let waiter = tokio::spawn(async move {
        waiting_lifecycle
            .wait_for_all_producers_done_on_shutdown()
            .await;
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    drop(producer);
    waiter.await.unwrap();
    let diagnostics = lifecycle.task_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, TrackedTaskKind::Events);

    drop(event_pump);
    lifecycle
        .wait_for_shutdown_tasks(Duration::from_millis(100))
        .await
        .unwrap();
}
