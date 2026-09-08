use crate::app_error::AppError;
use crate::models::{
    AppStateResyncRequired, OperationSnapshot, QueueItemRecord, StateDelta, StateDeltaValue,
};
use crate::state::estimate_inspection_allocation;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub(crate) const MAX_OUTBOX_BATCHES: usize = 256;
pub(crate) const MAX_OUTBOX_DELTAS: usize = 4_096;
pub(crate) const MAX_OUTBOX_ESTIMATED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) enum StatePublication {
    Deltas(Arc<[StateDelta]>),
    ResyncRequired(AppStateResyncRequired),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StateOutboxStats {
    pub(crate) queued_batches: usize,
    pub(crate) queued_deltas: usize,
    pub(crate) estimated_bytes: usize,
    pub(crate) coalesced_resyncs: u64,
}

#[derive(Clone)]
pub(crate) struct StateOutbox {
    inner: Arc<StateOutboxInner>,
}

struct StateOutboxInner {
    queue: Mutex<OutboxQueue>,
    available: Notify,
    reader_claimed: AtomicBool,
}

#[derive(Default)]
struct OutboxQueue {
    items: VecDeque<QueuedPublication>,
    delta_count: usize,
    estimated_bytes: usize,
    coalesced_resyncs: u64,
    high_water_sequence: u64,
}

enum QueuedPublication {
    Deltas {
        deltas: Arc<[StateDelta]>,
        estimated_bytes: usize,
    },
    ResyncRequired(AppStateResyncRequired),
}

pub(crate) struct StateOutboxReader {
    outbox: StateOutbox,
}

impl StateOutbox {
    pub(crate) fn new(initial_sequence: u64) -> Self {
        Self {
            inner: Arc::new(StateOutboxInner {
                queue: Mutex::new(OutboxQueue {
                    high_water_sequence: initial_sequence,
                    ..OutboxQueue::default()
                }),
                available: Notify::new(),
                reader_claimed: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn take_reader(&self) -> Result<StateOutboxReader, AppError> {
        if self
            .inner
            .reader_claimed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AppError::busy(
                "The application state publisher is already running.",
            ));
        }
        Ok(StateOutboxReader {
            outbox: self.clone(),
        })
    }

    pub(crate) fn enqueue(&self, deltas: Vec<StateDelta>, latest_sequence: u64) {
        if deltas.is_empty() {
            return;
        }
        let deltas: Arc<[StateDelta]> = deltas.into();
        let estimated_bytes = deltas.iter().map(estimate_delta_bytes).sum::<usize>();
        let mut queue = self.inner.queue.lock().unwrap();
        queue.high_water_sequence = queue.high_water_sequence.max(latest_sequence);
        let latest_sequence = queue.high_water_sequence;
        if let Some(QueuedPublication::ResyncRequired(required)) = queue.items.back_mut() {
            required.latest_sequence = required.latest_sequence.max(latest_sequence);
            queue.coalesced_resyncs = queue.coalesced_resyncs.saturating_add(1);
        } else if queue.items.len().saturating_add(1) > MAX_OUTBOX_BATCHES
            || queue.delta_count.saturating_add(deltas.len()) > MAX_OUTBOX_DELTAS
            || queue.estimated_bytes.saturating_add(estimated_bytes) > MAX_OUTBOX_ESTIMATED_BYTES
        {
            queue.items.clear();
            queue.delta_count = 0;
            queue.estimated_bytes = 0;
            queue.coalesced_resyncs = queue.coalesced_resyncs.saturating_add(1);
            queue
                .items
                .push_back(QueuedPublication::ResyncRequired(AppStateResyncRequired {
                    latest_sequence,
                }));
        } else {
            queue.delta_count = queue.delta_count.saturating_add(deltas.len());
            queue.estimated_bytes = queue.estimated_bytes.saturating_add(estimated_bytes);
            queue.items.push_back(QueuedPublication::Deltas {
                deltas,
                estimated_bytes,
            });
        }
        drop(queue);
        self.inner.available.notify_one();
    }

    pub(crate) fn require_resync(&self, latest_sequence: u64) {
        let mut queue = self.inner.queue.lock().unwrap();
        queue.high_water_sequence = queue.high_water_sequence.max(latest_sequence);
        let latest_sequence = queue.high_water_sequence;
        if let Some(QueuedPublication::ResyncRequired(required)) = queue.items.back_mut() {
            required.latest_sequence = required.latest_sequence.max(latest_sequence);
        } else {
            queue.items.clear();
            queue.delta_count = 0;
            queue.estimated_bytes = 0;
            queue
                .items
                .push_back(QueuedPublication::ResyncRequired(AppStateResyncRequired {
                    latest_sequence,
                }));
        }
        queue.coalesced_resyncs = queue.coalesced_resyncs.saturating_add(1);
        drop(queue);
        self.inner.available.notify_one();
    }

    pub(crate) fn stats(&self) -> StateOutboxStats {
        let queue = self.inner.queue.lock().unwrap();
        StateOutboxStats {
            queued_batches: queue.items.len(),
            queued_deltas: queue.delta_count,
            estimated_bytes: queue.estimated_bytes,
            coalesced_resyncs: queue.coalesced_resyncs,
        }
    }

    fn pop(&self) -> Option<StatePublication> {
        let mut queue = self.inner.queue.lock().unwrap();
        match queue.items.pop_front()? {
            QueuedPublication::Deltas {
                deltas,
                estimated_bytes,
            } => {
                queue.delta_count = queue.delta_count.saturating_sub(deltas.len());
                queue.estimated_bytes = queue.estimated_bytes.saturating_sub(estimated_bytes);
                Some(StatePublication::Deltas(deltas))
            }
            QueuedPublication::ResyncRequired(required) => {
                Some(StatePublication::ResyncRequired(required))
            }
        }
    }
}

impl StateOutboxReader {
    pub(crate) async fn recv(&self) -> StatePublication {
        loop {
            let available = self.outbox.inner.available.notified();
            tokio::pin!(available);
            available.as_mut().enable();
            if let Some(publication) = self.outbox.pop() {
                return publication;
            }
            available.await;
        }
    }

    pub(crate) fn stats(&self) -> StateOutboxStats {
        self.outbox.stats()
    }

    pub(crate) fn try_recv(&self) -> Option<StatePublication> {
        self.outbox.pop()
    }

    pub(crate) fn require_resync(&self, latest_sequence: u64) {
        self.outbox.require_resync(latest_sequence);
    }
}

impl Drop for StateOutboxReader {
    fn drop(&mut self) {
        self.outbox
            .inner
            .reader_claimed
            .store(false, Ordering::SeqCst);
    }
}

fn estimate_delta_bytes(delta: &StateDelta) -> usize {
    64 + match &delta.delta {
        StateDeltaValue::QueueItemUpserted(item) => estimate_queue_item_bytes(item),
        StateDeltaValue::QueueItemsRemoved(ids) => ids.iter().map(String::len).sum::<usize>(),
        StateDeltaValue::OperationUpserted(operation) => estimate_operation_bytes(operation),
        StateDeltaValue::OperationRemoved(id) => id.len(),
        StateDeltaValue::RuntimeReadinessChanged(_) => 16,
        StateDeltaValue::MaintenanceChanged { .. } => 16,
        StateDeltaValue::PersistenceHealthChanged(health) => health
            .error
            .as_ref()
            .map(|error| {
                error.code.len()
                    + error.summary.len()
                    + error.detail.as_ref().map_or(0, String::len)
                    + error.correlation_id.len()
            })
            .unwrap_or(0),
    }
}

fn estimate_queue_item_bytes(item: &QueueItemRecord) -> usize {
    192 + item.id.len()
        + item.source_url.len()
        + item.title.len()
        + item
            .available_qualities
            .iter()
            .map(String::len)
            .sum::<usize>()
        + item.format.len()
        + item.quality.len()
        + item.output_dir.len()
        + item.filename_override.as_ref().map_or(0, String::len)
        + item.compat_config_path.as_ref().map_or(0, String::len)
        + item.latest_operation_id.as_ref().map_or(0, String::len)
        + item.cookie_config.as_ref().map_or(0, |cookie| {
            cookie.mode.len()
                + cookie.browser.len()
                + cookie.cookie_file.as_ref().map_or(0, String::len)
        })
}

fn estimate_operation_bytes(operation: &OperationSnapshot) -> usize {
    192 + operation.id.len()
        + operation.queue_item_id.as_ref().map_or(0, String::len)
        + operation.phase.as_ref().map_or(0, String::len)
        + operation.correlation_id.len()
        + operation.error.as_ref().map_or(0, |error| {
            error.code.len()
                + error.summary.len()
                + error.detail.as_ref().map_or(0, String::len)
                + error.correlation_id.len()
        })
        + operation
            .inspection_result
            .as_ref()
            .map_or(0, |inspection| estimate_inspection_allocation(inspection))
        + operation
            .published_output
            .as_ref()
            .map_or(0, |output| output.path.len() + 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RuntimeReadiness, StateDeltaValue, APP_SCHEMA_VERSION};

    fn delta(sequence: u64) -> StateDelta {
        StateDelta {
            schema_version: APP_SCHEMA_VERSION,
            sequence,
            emitted_at_ms: sequence,
            delta: StateDeltaValue::RuntimeReadinessChanged(RuntimeReadiness::Ready),
        }
    }

    #[tokio::test]
    async fn overflow_coalesces_to_one_latest_sequence_resync() {
        let outbox = StateOutbox::new(0);
        let reader = outbox.take_reader().unwrap();
        for sequence in 1..=(MAX_OUTBOX_BATCHES as u64 + 20) {
            outbox.enqueue(vec![delta(sequence)], sequence);
        }

        let stats = reader.stats();
        assert_eq!(stats.queued_batches, 1);
        assert_eq!(stats.queued_deltas, 0);
        assert!(stats.coalesced_resyncs >= 1);
        assert!(matches!(
            reader.recv().await,
            StatePublication::ResyncRequired(AppStateResyncRequired { latest_sequence })
                if latest_sequence == MAX_OUTBOX_BATCHES as u64 + 20
        ));
    }

    #[tokio::test]
    async fn reader_preserves_batch_and_delta_order() {
        let outbox = StateOutbox::new(0);
        let reader = outbox.take_reader().unwrap();
        outbox.enqueue(vec![delta(1), delta(2)], 2);
        outbox.enqueue(vec![delta(3)], 3);

        let first = reader.recv().await;
        let second = reader.recv().await;
        assert!(matches!(
            first,
            StatePublication::Deltas(deltas)
                if deltas.iter().map(|delta| delta.sequence).collect::<Vec<_>>() == vec![1, 2]
        ));
        assert!(matches!(
            second,
            StatePublication::Deltas(deltas) if deltas[0].sequence == 3
        ));
    }

    #[test]
    fn resync_uses_the_outbox_high_water_when_the_observed_sequence_is_stale() {
        let outbox = StateOutbox::new(0);
        let reader = outbox.take_reader().unwrap();
        outbox.enqueue(vec![delta(10)], 10);

        reader.require_resync(5);

        assert!(matches!(
            reader.try_recv(),
            Some(StatePublication::ResyncRequired(AppStateResyncRequired {
                latest_sequence: 10
            }))
        ));
        assert!(reader.try_recv().is_none());
    }

    #[test]
    fn only_one_consumer_can_claim_the_outbox_at_a_time() {
        let outbox = StateOutbox::new(0);
        let first = outbox.take_reader().unwrap();
        let error = match outbox.take_reader() {
            Ok(_) => panic!("a second reader claim unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.code, "busy");
        drop(first);
        assert!(outbox.take_reader().is_ok());
    }
}
