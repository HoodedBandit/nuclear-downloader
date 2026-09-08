use crate::lifecycle::DownloadManager;
use crate::outbox::{StateOutboxReader, StatePublication};
use crate::state::StateStore;
use std::time::Duration;

const INITIAL_DELIVERY_RETRY_DELAY: Duration = Duration::from_millis(25);
const MAX_DELIVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const SHUTDOWN_DELIVERY_RETRY_BUDGET: Duration = Duration::from_secs(1);

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
mod tests;
