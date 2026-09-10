#![cfg(windows)]

use crate::journal::{now_ms, JournalStore, PersistentJournal};
use crate::models::{
    QueueItemRecord, QueueItemState, QueuePriority, UpdateQueueItemInput, APP_SCHEMA_VERSION,
};
use crate::outbox::{MAX_OUTBOX_BATCHES, MAX_OUTBOX_DELTAS, MAX_OUTBOX_ESTIMATED_BYTES};
use crate::state::StateStore;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const SNAPSHOT_SAMPLES: usize = 10_000;
const SNAPSHOT_WIRE_SAMPLES: usize = 1_000;
const DURABLE_COMMAND_SAMPLES: usize = 200;
const JOURNAL_SAMPLES: usize = 200;
const WARMUP_SAMPLES: usize = 5;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    schema_version: u32,
    label: String,
    profile: String,
    benchmark: &'static str,
    recorded_at_unix_ms: u64,
    workload: Workload,
    metrics: Metrics,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Workload {
    queue_size: usize,
    snapshot_samples: usize,
    snapshot_wire_samples: usize,
    durable_command_samples: usize,
    journal_samples: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    latency_micros: Latencies,
    blocked_journal_snapshot: BlockedJournalSnapshot,
    event_backlog: EventBacklog,
    #[serde(skip_serializing_if = "Option::is_none")]
    outbox: Option<OutboxEvidence>,
    storage_bytes: StorageBytes,
    memory_bytes: MemoryBytes,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Latencies {
    snapshot: LatencySummary,
    snapshot_wire: LatencySummary,
    durable_command: LatencySummary,
    journal_save: LatencySummary,
    enqueue_batch: LatencySummary,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LatencySummary {
    samples: usize,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockedJournalSnapshot {
    latency_micros: u64,
    observed_pre_commit_state: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventBacklog {
    delta_count: usize,
    serialized_bytes: u64,
    sequences_contiguous: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboxEvidence {
    queued_batches: usize,
    queued_deltas: usize,
    estimated_bytes: usize,
    coalesced_resyncs: u64,
    limits: OutboxLimits,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboxLimits {
    max_batches: usize,
    max_deltas: usize,
    max_estimated_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageBytes {
    snapshot_json: u64,
    journal_after_durable: u64,
    journal_after_enqueue: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryBytes {
    working_set_after_load: Option<u64>,
    working_set_after_samples: Option<u64>,
    private_after_load: Option<u64>,
    private_after_samples: Option<u64>,
}

#[derive(Clone, Copy)]
struct MemorySample {
    working_set: u64,
    private: u64,
}

struct TempRootGuard(PathBuf);

impl Drop for TempRootGuard {
    fn drop(&mut self) {
        let marker = self.0.join(".nuclear-phase4-performance-fixture");
        let safe_name = self
            .0
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("nuclear-phase4-performance-"));
        if safe_name && marker.is_file() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
#[ignore = "Phase 4 performance evidence; run through scripts/run-backend-performance.ps1"]
async fn phase4_baseline() {
    let queue_size = required_queue_size();
    let label = performance_label();
    let profile = performance_profile();
    let evidence_path = required_evidence_path();

    let fixture_root = std::env::temp_dir().join(format!(
        "nuclear-phase4-performance-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&fixture_root).expect("create isolated performance fixture root");
    fs::write(
        fixture_root.join(".nuclear-phase4-performance-fixture"),
        b"owned test fixture\n",
    )
    .expect("mark isolated performance fixture root");
    let _fixture_guard = TempRootGuard(fixture_root.clone());

    let state_journal_path = fixture_root.join("state.dpapi");
    let diagnostics_path = fixture_root.join("diagnostics.log");
    let journal_only_path = fixture_root.join("journal-only.dpapi");
    let queue = fixture_queue(queue_size);
    seed_journal(&state_journal_path, queue.clone());
    seed_journal(&journal_only_path, queue.clone());

    let store = StateStore::open_at(state_journal_path.clone(), diagnostics_path)
        .expect("open isolated state store");
    let memory_after_load = process_memory();
    let item_ids = queue.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    let initial_snapshot = store.snapshot().expect("read seeded state snapshot");
    assert_eq!(initial_snapshot.queue.len(), queue_size);
    let snapshot_json_bytes = serialized_len(&initial_snapshot);

    let blocked_journal_snapshot = blocked_snapshot_probe(&store, &item_ids[0]).await;

    for _ in 0..20 {
        std::hint::black_box(store.snapshot().expect("warm snapshot"));
    }
    let mut snapshot_latencies = Vec::with_capacity(SNAPSHOT_SAMPLES);
    for _ in 0..SNAPSHOT_SAMPLES {
        let started = Instant::now();
        std::hint::black_box(store.snapshot().expect("snapshot sample"));
        snapshot_latencies.push(started.elapsed());
    }

    for _ in 0..WARMUP_SAMPLES {
        let snapshot = store.snapshot().expect("warm wire snapshot");
        std::hint::black_box(serde_json::to_vec(&snapshot).expect("serialize warm snapshot"));
    }
    let mut snapshot_wire_latencies = Vec::with_capacity(SNAPSHOT_WIRE_SAMPLES);
    for _ in 0..SNAPSHOT_WIRE_SAMPLES {
        let started = Instant::now();
        let snapshot = store.snapshot().expect("wire snapshot sample");
        std::hint::black_box(serde_json::to_vec(&snapshot).expect("serialize snapshot sample"));
        snapshot_wire_latencies.push(started.elapsed());
    }

    let mut durable_latencies = Vec::with_capacity(DURABLE_COMMAND_SAMPLES);
    for sample in 0..(WARMUP_SAMPLES + DURABLE_COMMAND_SAMPLES) {
        let input = quality_update(sample);
        let started = Instant::now();
        std::hint::black_box(
            store
                .update_queue_item(&item_ids[0], input)
                .await
                .expect("durable command sample"),
        );
        if sample >= WARMUP_SAMPLES {
            durable_latencies.push(started.elapsed());
        }
    }

    let journal_after_durable = file_len(&state_journal_path);
    let journal_save_latencies = measure_journal_saves(&journal_only_path, queue);

    let enqueue_started = Instant::now();
    let (_, deltas) = store
        .enqueue(&item_ids, QueuePriority::Normal)
        .await
        .expect("enqueue benchmark fixture");
    let enqueue_latency = enqueue_started.elapsed();
    let serialized_delta_bytes = deltas
        .iter()
        .map(serialized_len)
        .fold(0u64, u64::saturating_add);
    let sequences_contiguous = deltas
        .windows(2)
        .all(|pair| pair[1].sequence == pair[0].sequence.saturating_add(1));
    let journal_after_enqueue = file_len(&state_journal_path);
    let memory_after_samples = process_memory();
    let outbox = (label == "after").then(|| outbox_evidence(&store));

    let evidence = Evidence {
        schema_version: 1,
        label,
        profile,
        benchmark: "state_journal",
        recorded_at_unix_ms: now_ms(),
        workload: Workload {
            queue_size,
            snapshot_samples: SNAPSHOT_SAMPLES,
            snapshot_wire_samples: SNAPSHOT_WIRE_SAMPLES,
            durable_command_samples: DURABLE_COMMAND_SAMPLES,
            journal_samples: JOURNAL_SAMPLES,
        },
        metrics: Metrics {
            latency_micros: Latencies {
                snapshot: summarize(snapshot_latencies),
                snapshot_wire: summarize(snapshot_wire_latencies),
                durable_command: summarize(durable_latencies),
                journal_save: summarize(journal_save_latencies),
                enqueue_batch: summarize(vec![enqueue_latency]),
            },
            blocked_journal_snapshot,
            event_backlog: EventBacklog {
                delta_count: deltas.len(),
                serialized_bytes: serialized_delta_bytes,
                sequences_contiguous,
            },
            outbox,
            storage_bytes: StorageBytes {
                snapshot_json: snapshot_json_bytes,
                journal_after_durable,
                journal_after_enqueue,
            },
            memory_bytes: MemoryBytes {
                working_set_after_load: memory_after_load.map(|sample| sample.working_set),
                working_set_after_samples: memory_after_samples.map(|sample| sample.working_set),
                private_after_load: memory_after_load.map(|sample| sample.private),
                private_after_samples: memory_after_samples.map(|sample| sample.private),
            },
        },
    };

    let json = serde_json::to_string(&evidence).expect("serialize performance evidence");
    write_evidence(&evidence_path, &json);
    println!("PHASE4_PERF_JSON={json}");
}

fn outbox_evidence(store: &StateStore) -> OutboxEvidence {
    let stats = store.outbox_stats();
    assert!(stats.queued_batches <= MAX_OUTBOX_BATCHES);
    assert!(stats.queued_deltas <= MAX_OUTBOX_DELTAS);
    assert!(stats.estimated_bytes <= MAX_OUTBOX_ESTIMATED_BYTES);
    assert_eq!(
        stats.coalesced_resyncs, 0,
        "the fixed baseline workload unexpectedly required an outbox resync"
    );
    OutboxEvidence {
        queued_batches: stats.queued_batches,
        queued_deltas: stats.queued_deltas,
        estimated_bytes: stats.estimated_bytes,
        coalesced_resyncs: stats.coalesced_resyncs,
        limits: OutboxLimits {
            max_batches: MAX_OUTBOX_BATCHES,
            max_deltas: MAX_OUTBOX_DELTAS,
            max_estimated_bytes: MAX_OUTBOX_ESTIMATED_BYTES,
        },
    }
}

fn required_queue_size() -> usize {
    let value = std::env::var("NUCLEAR_PERF_QUEUE_SIZE")
        .expect("NUCLEAR_PERF_QUEUE_SIZE must be set by the performance runner");
    match value.as_str() {
        "1" => 1,
        "100" => 100,
        "1000" => 1_000,
        _ => panic!("NUCLEAR_PERF_QUEUE_SIZE must be 1, 100, or 1000"),
    }
}

fn performance_label() -> String {
    let value = std::env::var("NUCLEAR_PERF_LABEL").unwrap_or_else(|_| "baseline".to_owned());
    assert!(matches!(value.as_str(), "baseline" | "after"));
    value
}

fn performance_profile() -> String {
    let fallback = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let value = std::env::var("NUCLEAR_PERF_PROFILE").unwrap_or_else(|_| fallback.to_owned());
    assert!(matches!(value.as_str(), "debug" | "release"));
    value
}

fn required_evidence_path() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("NUCLEAR_PERF_EVIDENCE_PATH")
            .expect("NUCLEAR_PERF_EVIDENCE_PATH must be set by the performance runner"),
    );
    assert!(
        path.is_absolute(),
        "performance evidence path must be absolute"
    );
    path
}

fn fixture_queue(queue_size: usize) -> Vec<QueueItemRecord> {
    (0..queue_size)
        .map(|index| QueueItemRecord {
            schema_version: APP_SCHEMA_VERSION,
            id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
            source_url: format!("https://fixture.invalid/video/{index}"),
            title: format!("Phase 4 fixture {index}"),
            available_qualities: vec!["720p".to_owned(), "1080p".to_owned()],
            has_audio: true,
            cookie_config: None,
            format: "mp4".to_owned(),
            quality: "720p".to_owned(),
            output_dir: "C:\\phase4-fixture-output".to_owned(),
            filename_override: Some(format!("fixture-{index:04}")),
            compat_config_path: None,
            selection: None,
            state: QueueItemState::Inert,
            latest_operation_id: None,
            created_at_ms: 1_700_000_000_000 + index as u64,
            updated_at_ms: 1_700_000_000_000 + index as u64,
        })
        .collect()
}

fn seed_journal(path: &Path, queue: Vec<QueueItemRecord>) {
    let (store, _, quarantine) =
        JournalStore::open(path.to_path_buf()).expect("open isolated seed journal");
    assert!(quarantine.is_none());
    store
        .save(&PersistentJournal {
            schema_version: APP_SCHEMA_VERSION,
            revision: 1,
            queue,
            operations: Vec::new(),
            pending_app_update: None,
        })
        .expect("seed isolated journal");
}

async fn blocked_snapshot_probe(store: &StateStore, item_id: &str) -> BlockedJournalSnapshot {
    let pause = store.pause_next_journal_save_for_test();
    let writer_store = store.clone();
    let writer_item_id = item_id.to_owned();
    let writer = tokio::spawn(async move {
        writer_store
            .update_queue_item(
                &writer_item_id,
                UpdateQueueItemInput {
                    quality: Some("1080p".to_owned()),
                    ..Default::default()
                },
            )
            .await
    });
    pause.wait_entered().await;
    let started = Instant::now();
    let snapshot = store
        .snapshot()
        .expect("snapshot during blocked journal commit");
    let latency_micros = micros(started.elapsed());
    let observed_pre_commit_state = snapshot
        .queue
        .iter()
        .find(|item| item.id == item_id)
        .is_some_and(|item| item.quality == "720p");
    pause.release();
    writer
        .await
        .expect("blocked journal writer task")
        .expect("complete blocked journal writer");
    assert!(
        observed_pre_commit_state,
        "snapshot exposed an uncommitted candidate"
    );
    BlockedJournalSnapshot {
        latency_micros,
        observed_pre_commit_state,
    }
}

fn quality_update(sample: usize) -> UpdateQueueItemInput {
    UpdateQueueItemInput {
        quality: Some(
            if sample.is_multiple_of(2) {
                "720p"
            } else {
                "1080p"
            }
            .to_owned(),
        ),
        ..Default::default()
    }
}

fn measure_journal_saves(path: &Path, queue: Vec<QueueItemRecord>) -> Vec<std::time::Duration> {
    let (store, mut journal, quarantine) =
        JournalStore::open(path.to_path_buf()).expect("open isolated journal benchmark");
    assert!(quarantine.is_none());
    journal.queue = queue;
    let mut latencies = Vec::with_capacity(JOURNAL_SAMPLES);
    for sample in 0..(WARMUP_SAMPLES + JOURNAL_SAMPLES) {
        journal.revision = journal.revision.saturating_add(1);
        let started = Instant::now();
        store.save(&journal).expect("journal save sample");
        if sample >= WARMUP_SAMPLES {
            latencies.push(started.elapsed());
        }
    }
    latencies
}

fn summarize(mut samples: Vec<std::time::Duration>) -> LatencySummary {
    samples.sort_unstable();
    LatencySummary {
        samples: samples.len(),
        p50: percentile_micros(&samples, 50),
        p95: percentile_micros(&samples, 95),
        p99: percentile_micros(&samples, 99),
        max: samples.last().copied().map(micros).unwrap_or(0),
    }
}

fn percentile_micros(samples: &[std::time::Duration], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let rank = samples
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    micros(samples[rank.min(samples.len() - 1)])
}

fn micros(duration: std::time::Duration) -> u64 {
    duration.as_micros().try_into().unwrap_or(u64::MAX)
}

fn serialized_len<T: Serialize>(value: &T) -> u64 {
    serde_json::to_vec(value)
        .expect("serialize performance metric payload")
        .len()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).expect("read journal metadata").len()
}

fn write_evidence(path: &Path, json: &str) {
    let parent = path.parent().expect("performance evidence parent");
    fs::create_dir_all(parent).expect("create performance evidence directory");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create new performance evidence file");
    file.write_all(json.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .expect("publish performance evidence");
}

fn process_memory() -> Option<MemorySample> {
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
    }
    #[link(name = "Psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    // SAFETY: the pseudo-handle is valid for the current process and the
    // writable structure and size agree with PROCESS_MEMORY_COUNTERS_EX.
    let succeeded = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        )
    };
    (succeeded != 0).then_some(MemorySample {
        working_set: counters.working_set_size as u64,
        private: counters.private_usage as u64,
    })
}
