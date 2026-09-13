use super::*;
use crate::models::{
    CookieConfig, MediaSelection, QueueItemRecord, QueueItemState, RuntimeReadiness,
    StateDeltaValue, APP_SCHEMA_VERSION,
};

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

#[test]
fn max_length_queue_fields_overflow_to_resync_instead_of_remaining_buffered() {
    fn max_string(byte: u8) -> String {
        String::from_utf8(vec![byte; 4 * 1024]).unwrap()
    }

    let outbox = StateOutbox::new(0);
    let reader = outbox.take_reader().unwrap();
    let deltas = (1..=1_000u64)
        .map(|sequence| StateDelta {
            schema_version: APP_SCHEMA_VERSION,
            sequence,
            emitted_at_ms: sequence,
            delta: StateDeltaValue::QueueItemUpserted(QueueItemRecord {
                schema_version: APP_SCHEMA_VERSION,
                id: uuid::Uuid::from_u128(sequence as u128).to_string(),
                source_url: format!("https://example.com/{sequence}"),
                source_media_id: Some(max_string(b'm')),
                title: max_string(b't'),
                available_qualities: vec![max_string(b'q')],
                has_audio: true,
                cookie_config: Some(CookieConfig {
                    enabled: true,
                    mode: max_string(b'o'),
                    browser: max_string(b'b'),
                    cookie_file: Some(max_string(b'c')),
                }),
                format: max_string(b'f'),
                quality: max_string(b'h'),
                output_dir: max_string(b'd'),
                filename_override: Some(max_string(b'n')),
                compat_config_path: Some(max_string(b'p')),
                selection: Some(MediaSelection {
                    entry_id: max_string(b'e'),
                    extractor_key: max_string(b'x'),
                    playlist_index: sequence as u32,
                }),
                preparation: None,
                preparation_operation_id: Some(uuid::Uuid::new_v4().to_string()),
                state: QueueItemState::Inert,
                latest_operation_id: Some(uuid::Uuid::new_v4().to_string()),
                created_at_ms: sequence,
                updated_at_ms: sequence,
            }),
        })
        .collect::<Vec<_>>();

    outbox.enqueue(deltas, 1_000);

    let stats = reader.stats();
    assert_eq!(stats.queued_batches, 1);
    assert_eq!(stats.queued_deltas, 0);
    assert!(stats.coalesced_resyncs >= 1);
    assert!(matches!(
        reader.try_recv(),
        Some(StatePublication::ResyncRequired(AppStateResyncRequired {
            latest_sequence: 1_000
        }))
    ));
}
