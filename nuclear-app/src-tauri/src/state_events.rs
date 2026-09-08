use crate::app_error::AppError;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::outbox::{StateOutboxReader, StatePublication};
use crate::state::StateStore;
use std::time::Duration;
use tauri::Emitter;

const INITIAL_DELIVERY_RETRY_DELAY: Duration = Duration::from_millis(25);
const MAX_DELIVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const SHUTDOWN_DELIVERY_RETRY_BUDGET: Duration = Duration::from_secs(1);

pub(crate) fn spawn_state_events(
    app: &tauri::AppHandle,
    store: &StateStore,
    manager: &DownloadManager,
) -> Result<(), AppError> {
    let reader = store.take_outbox_reader()?;
    let app = app.clone();
    let store = store.clone();
    let coordinator = manager.clone();
    manager.spawn_tracked(TrackedTaskKind::Events, async move {
        publish_state_events(
            &store,
            &coordinator,
            reader,
            |publication| match publication {
                StatePublication::Deltas(deltas) => {
                    for delta in deltas.iter() {
                        app.emit("app-state-changed", delta)
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                }
                StatePublication::ResyncRequired(required) => app
                    .emit("app-state-resync-required", required)
                    .map_err(|error| error.to_string()),
            },
        )
        .await;
    })
}

pub(crate) async fn publish_state_events<S>(
    store: &StateStore,
    manager: &DownloadManager,
    reader: StateOutboxReader,
    mut sink: S,
) where
    S: FnMut(&StatePublication) -> Result<(), String>,
{
    let shutdown = manager.shutdown_token();
    let mut retry_delay = INITIAL_DELIVERY_RETRY_DELAY;
    loop {
        let publication = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            publication = reader.recv() => publication,
        };
        if let Err(error) = sink(&publication) {
            record_delivery_failure(store, &reader, &error);
            // Include newer mutations that raced this failed delivery.
            reader.require_resync(publication_sequence(&publication));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(retry_delay) => {},
            }
            retry_delay = (retry_delay * 2).min(MAX_DELIVERY_RETRY_DELAY);
        } else {
            retry_delay = INITIAL_DELIVERY_RETRY_DELAY;
            tokio::task::yield_now().await;
        }
    }

    // Events is excluded from this producer wait, but remains registered
    // through the final drain for ordinary shutdown accounting.
    manager.wait_for_all_producers_done_on_shutdown().await;
    let retry_deadline = tokio::time::Instant::now() + SHUTDOWN_DELIVERY_RETRY_BUDGET;
    retry_delay = INITIAL_DELIVERY_RETRY_DELAY;
    while let Some(publication) = reader.try_recv() {
        if let Err(error) = sink(&publication) {
            record_delivery_failure(store, &reader, &error);
            reader.require_resync(publication_sequence(&publication));
            let remaining = retry_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(retry_delay.min(remaining)).await;
            retry_delay = (retry_delay * 2).min(MAX_DELIVERY_RETRY_DELAY);
        } else {
            retry_delay = INITIAL_DELIVERY_RETRY_DELAY;
        }
    }
}

fn publication_sequence(publication: &StatePublication) -> u64 {
    match publication {
        StatePublication::Deltas(deltas) => deltas.last().map_or(0, |delta| delta.sequence),
        StatePublication::ResyncRequired(required) => required.latest_sequence,
    }
}

fn record_delivery_failure(store: &StateStore, reader: &StateOutboxReader, detail: &str) {
    let stats = reader.stats();
    store.diagnostics().log(
        "warning",
        "state_event_delivery_failed",
        &uuid::Uuid::new_v4().to_string(),
        &format!(
            "{detail}; pending batches={}, pending deltas={}, estimated bytes={}, resyncs={}",
            stats.queued_batches,
            stats.queued_deltas,
            stats.estimated_bytes,
            stats.coalesced_resyncs,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleCoordinator;
    use crate::models::RuntimeReadiness;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{oneshot, Notify};

    fn test_store(label: &str) -> (StateStore, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nuclear-state-events-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store =
            StateStore::open_at(root.join("journal.dpapi"), root.join("diagnostics")).unwrap();
        (store, root)
    }

    #[tokio::test]
    async fn sink_failure_collapses_pending_deltas_to_latest_resync() {
        let (store, root) = test_store("resync");
        store
            .set_runtime_readiness(RuntimeReadiness::UpdateAvailable)
            .await
            .unwrap();
        store
            .set_runtime_readiness(RuntimeReadiness::Ready)
            .await
            .unwrap();
        let reader = store.take_outbox_reader().unwrap();
        let manager = LifecycleCoordinator::new(1, 1, 1);
        let attempts = Arc::new(AtomicUsize::new(0));
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let resync_delivered = Arc::new(Notify::new());
        let (done_tx, done_rx) = oneshot::channel();
        let sink_attempts = attempts.clone();
        let sink_delivered = delivered.clone();
        let sink_resync = resync_delivered.clone();
        let pump_store = store.clone();
        let pump_manager = manager.clone();
        manager
            .spawn_tracked(TrackedTaskKind::Events, async move {
                publish_state_events(&pump_store, &pump_manager, reader, |publication| {
                    if sink_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err("synthetic sink failure".into());
                    }
                    sink_delivered.lock().unwrap().push(publication.clone());
                    if matches!(publication, StatePublication::ResyncRequired(_)) {
                        sink_resync.notify_one();
                    }
                    Ok(())
                })
                .await;
                let _ = done_tx.send(());
            })
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), resync_delivered.notified())
            .await
            .unwrap();
        manager.begin_shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();

        {
            let delivered = delivered.lock().unwrap();
            assert!(matches!(
                delivered.as_slice(),
                [StatePublication::ResyncRequired(required)] if required.latest_sequence == 2
            ));
        }
        manager
            .wait_for_shutdown_tasks(Duration::from_secs(1))
            .await
            .unwrap();
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn shutdown_waits_for_late_producer_before_final_event_drain() {
        let (store, root) = test_store("late-producer");
        let reader = store.take_outbox_reader().unwrap();
        let manager = LifecycleCoordinator::new(1, 1, 1);
        let release_producer = Arc::new(Notify::new());
        let producer_release = release_producer.clone();
        let producer_store = store.clone();
        manager
            .spawn_tracked(TrackedTaskKind::Admission, async move {
                producer_release.notified().await;
                producer_store
                    .set_runtime_readiness(RuntimeReadiness::Ready)
                    .await
                    .unwrap();
            })
            .unwrap();

        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink_delivered = delivered.clone();
        let (done_tx, mut done_rx) = oneshot::channel();
        let pump_store = store.clone();
        let pump_manager = manager.clone();
        manager
            .spawn_tracked(TrackedTaskKind::Events, async move {
                publish_state_events(&pump_store, &pump_manager, reader, |publication| {
                    sink_delivered.lock().unwrap().push(publication.clone());
                    Ok(())
                })
                .await;
                let _ = done_tx.send(());
            })
            .unwrap();

        manager.begin_shutdown().await;
        tokio::task::yield_now().await;
        assert!(done_rx.try_recv().is_err());
        release_producer.notify_one();
        tokio::time::timeout(Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();

        {
            let delivered = delivered.lock().unwrap();
            assert!(matches!(
                delivered.as_slice(),
                [StatePublication::Deltas(deltas)] if deltas.last().is_some_and(|delta| delta.sequence == 1)
            ));
        }
        manager
            .wait_for_shutdown_tasks(Duration::from_secs(1))
            .await
            .unwrap();
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn shutdown_retries_late_producer_failure_as_high_water_resync() {
        let (store, root) = test_store("late-producer-retry");
        let reader = store.take_outbox_reader().unwrap();
        let manager = LifecycleCoordinator::new(1, 1, 1);
        let release_producer = Arc::new(Notify::new());
        let producer_release = release_producer.clone();
        let producer_store = store.clone();
        manager
            .spawn_tracked(TrackedTaskKind::Admission, async move {
                producer_release.notified().await;
                producer_store
                    .set_runtime_readiness(RuntimeReadiness::Ready)
                    .await
                    .unwrap();
            })
            .unwrap();

        let attempts = Arc::new(AtomicUsize::new(0));
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink_attempts = attempts.clone();
        let sink_delivered = delivered.clone();
        let (done_tx, mut done_rx) = oneshot::channel();
        let pump_store = store.clone();
        let pump_manager = manager.clone();
        manager
            .spawn_tracked(TrackedTaskKind::Events, async move {
                publish_state_events(&pump_store, &pump_manager, reader, |publication| {
                    if sink_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err("one-shot final drain failure".into());
                    }
                    sink_delivered.lock().unwrap().push(publication.clone());
                    Ok(())
                })
                .await;
                let _ = done_tx.send(());
            })
            .unwrap();

        manager.begin_shutdown().await;
        tokio::task::yield_now().await;
        assert!(done_rx.try_recv().is_err());
        release_producer.notify_one();
        tokio::time::timeout(Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        {
            let delivered = delivered.lock().unwrap();
            assert!(matches!(
                delivered.as_slice(),
                [StatePublication::ResyncRequired(required)] if required.latest_sequence == 1
            ));
        }
        manager
            .wait_for_shutdown_tasks(Duration::from_secs(1))
            .await
            .unwrap();
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }
}
