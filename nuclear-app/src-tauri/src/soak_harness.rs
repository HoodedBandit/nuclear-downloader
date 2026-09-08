#![cfg(windows)]

use crate::app_error::AppError;
use crate::downloader::process::{
    test_run_supervised_absolute_child, test_run_supervised_inherited_pipe_fixture,
    TestInheritedPipeFixtureOptions,
};
use crate::downloader::publication::{
    test_cleanup_abandoned_stages, test_cleanup_owned_stage, test_create_owned_stage,
    test_publish_staged_file,
};
use crate::journal::{JournalStore, PersistentJournal, MAX_TERMINAL_ATTEMPTS};
use crate::lifecycle::{create_download_manager, DrainCompletion};
use crate::models::{
    DownloadProgress, OperationState, QueueItemRecord, QueueItemState, QueuePriority,
    APP_SCHEMA_VERSION,
};
use crate::outbox::{
    StatePublication, MAX_OUTBOX_BATCHES, MAX_OUTBOX_DELTAS, MAX_OUTBOX_ESTIMATED_BYTES,
};
use crate::runtime::test_support::VerifiedRuntimeHarness;
use crate::state::{DownloadTerminalOutcome, StateStore};
use futures_util::FutureExt;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessHandleCount, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

const QUEUE_SIZE: usize = 100;
const BATCH_SIZE: usize = 5;
const CYCLE_PERIOD: Duration = Duration::from_secs(5);
const WARMUP_CYCLES: u64 = 12;
const MAX_PRIVATE_GROWTH_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WORKING_SET_GROWTH_BYTES: u64 = 192 * 1024 * 1024;
const MAX_HANDLE_GROWTH: u32 = 32;
const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DIAGNOSTICS_BYTES: u64 = 1024 * 1024;
const OWNER_MARKER: &str = ".nuclear-phase5-soak-owner";

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn FreeConsole() -> i32;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceSample {
    elapsed_seconds: u64,
    cycle: u64,
    working_set_bytes: u64,
    private_bytes: u64,
    handle_count: u32,
    descendant_count: usize,
    descendants: Vec<DescendantEvidence>,
    queue_items: usize,
    retained_operations: usize,
    pending_operations: usize,
    active_lifecycle_jobs: usize,
    outbox_batches: usize,
    outbox_deltas: usize,
    outbox_estimated_bytes: usize,
    outbox_coalesced_resyncs: u64,
    journal_bytes: u64,
    diagnostics_bytes: u64,
    residual_output_entries: usize,
    latest_sequence: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DescendantEvidence {
    pid: u32,
    parent_pid: u32,
    creation_filetime: u64,
    image_name: String,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkloadCounts {
    cycles: u64,
    operations: u64,
    completed: u64,
    cancelled: u64,
    failed: u64,
    supervised_successes: u64,
    supervised_failures: u64,
    supervised_cancellations: u64,
    inherited_pipe_drains: u64,
    publications: u64,
    collision_preservations: u64,
    abandoned_stage_cleanups: u64,
    unowned_stage_preservations: u64,
    lifecycle_drains: u64,
    runtime_mutations: u64,
    resyncs_observed: u64,
    deltas_observed: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceBounds {
    scope: &'static str,
    max_private_growth_bytes: u64,
    max_working_set_growth_bytes: u64,
    max_handle_growth: u32,
    max_descendants: usize,
    max_journal_bytes: u64,
    max_diagnostics_bytes: u64,
    max_residual_output_entries: usize,
    max_outbox_batches: usize,
    max_outbox_deltas: usize,
    max_outbox_estimated_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SoakEvidence<'a> {
    schema_version: u32,
    status: &'a str,
    error: Option<&'a str>,
    label: &'a str,
    profile: &'a str,
    requested_duration_seconds: u64,
    actual_duration_millis: u64,
    queue_size: usize,
    download_workers: usize,
    workload: &'a WorkloadCounts,
    samples: usize,
    first_sample: Option<&'a ResourceSample>,
    steady_state_baseline: Option<&'a ResourceSample>,
    last_sample: Option<&'a ResourceSample>,
    runtime_hash_invocations: u64,
    runtime_hash_bytes: u64,
    runtime_resolution_calls: u64,
    runtime_successful_resolutions: u64,
    final_journal_reopened: bool,
    resource_bounds: ResourceBounds,
}

#[derive(Clone)]
struct Context {
    run_root: PathBuf,
    fixture_root: PathBuf,
    evidence_path: PathBuf,
    samples_path: PathBuf,
    journal_path: PathBuf,
    diagnostics_path: PathBuf,
    output_root: PathBuf,
    powershell: PathBuf,
    duration: Duration,
    label: String,
    profile: String,
}

struct CycleResult {
    terminal: OperationState,
    supervised_success: bool,
    supervised_failure: bool,
    supervised_cancellation: bool,
    publication: bool,
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "Phase 5 is an explicit, isolated two-hour soak run"]
async fn phase5_soak() {
    // The ignored soak is launched with redirected file handles and does not
    // need a Windows console. Detaching this child-local console releases its
    // conhost before the strict quiescent descendant samples begin.
    let _ = unsafe { FreeConsole() };
    let context = Context::from_environment().expect("validate explicit Phase 5 environment");
    let started = Instant::now();
    let mut counts = WorkloadCounts::default();
    let mut samples = Vec::new();
    let mut runtime_counts = (0, 0, 0, 0);
    let result = std::panic::AssertUnwindSafe(run_soak(
        &context,
        &mut counts,
        &mut samples,
        &mut runtime_counts,
        started,
    ))
    .catch_unwind()
    .await
    .unwrap_or_else(|panic| Err(format!("unexpected soak panic: {}", panic_message(panic))));
    let error = result.as_ref().err().map(String::as_str);
    let evidence = SoakEvidence {
        schema_version: 1,
        status: if result.is_ok() { "passed" } else { "failed" },
        error,
        label: &context.label,
        profile: &context.profile,
        requested_duration_seconds: context.duration.as_secs(),
        actual_duration_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        queue_size: QUEUE_SIZE,
        download_workers: 5,
        workload: &counts,
        samples: samples.len(),
        first_sample: samples.first(),
        steady_state_baseline: samples.get(WARMUP_CYCLES as usize),
        last_sample: samples.last(),
        runtime_hash_invocations: runtime_counts.0,
        runtime_hash_bytes: runtime_counts.1,
        runtime_resolution_calls: runtime_counts.2,
        runtime_successful_resolutions: runtime_counts.3,
        final_journal_reopened: result.is_ok(),
        resource_bounds: ResourceBounds {
            scope: "quiescent samples in an isolated Windows debug lib-test process",
            max_private_growth_bytes: MAX_PRIVATE_GROWTH_BYTES,
            max_working_set_growth_bytes: MAX_WORKING_SET_GROWTH_BYTES,
            max_handle_growth: MAX_HANDLE_GROWTH,
            max_descendants: 0,
            max_journal_bytes: MAX_JOURNAL_BYTES,
            max_diagnostics_bytes: MAX_DIAGNOSTICS_BYTES,
            max_residual_output_entries: 0,
            max_outbox_batches: MAX_OUTBOX_BATCHES,
            max_outbox_deltas: MAX_OUTBOX_DELTAS,
            max_outbox_estimated_bytes: MAX_OUTBOX_ESTIMATED_BYTES,
        },
    };
    publish_json(&context.evidence_path, &evidence, true).expect("publish Phase 5 evidence");
    if let Err(error) = result {
        panic!("Phase 5 soak failed: {error}");
    }
}

async fn run_soak(
    context: &Context,
    counts: &mut WorkloadCounts,
    samples: &mut Vec<ResourceSample>,
    runtime_evidence: &mut (u64, u64, u64, u64),
    started: Instant,
) -> Result<(), String> {
    fs::create_dir(&context.fixture_root).map_err(to_string("create fixture root"))?;
    fs::create_dir(&context.output_root).map_err(to_string("create output root"))?;
    seed_journal(&context.journal_path, fixture_queue(&context.output_root))?;
    let store = StateStore::open_at(
        context.journal_path.clone(),
        context.diagnostics_path.clone(),
    )
    .map_err(display_error)?;
    let outbox = store.take_outbox_reader().map_err(display_error)?;
    let lifecycle = create_download_manager();
    lifecycle.open_for_test().await;
    let runtime = Arc::new(
        VerifiedRuntimeHarness::create_at(context.fixture_root.join("runtime"))
            .map_err(|error| format!("runtime fixture: {error}"))?,
    );
    runtime.initialize().await?;
    runtime.reset_counts();
    capture_runtime_counts(&runtime, runtime_evidence);

    let mut observed_sequence = store.snapshot().map_err(display_error)?.latest_sequence;
    force_outbox_overflow(&store).await?;
    drain_outbox(&store, &outbox, counts, &mut observed_sequence, true)?;
    let first = sample(context, &store, &lifecycle, started, counts.cycles).await?;
    append_sample(&context.samples_path, &first)?;
    assert_quiescent_invariants(&first)?;
    samples.push(first);

    while started.elapsed() < context.duration {
        let cycle_started = Instant::now();
        let cycle_result = run_cycle(context, &store, &lifecycle, &runtime, counts).await;
        capture_runtime_counts(&runtime, runtime_evidence);
        cycle_result?;
        counts.cycles += 1;
        drain_outbox(&store, &outbox, counts, &mut observed_sequence, false)?;

        if counts.cycles.is_multiple_of(12) {
            exercise_lifecycle_drain(&lifecycle, counts).await?;
            let mutation_result = exercise_runtime_mutation(&runtime, counts).await;
            capture_runtime_counts(&runtime, runtime_evidence);
            mutation_result?;
        }
        if counts.cycles == 5 || counts.cycles.is_multiple_of(360) {
            exercise_inherited_pipe(context, &lifecycle, counts).await?;
        }
        if counts.cycles.is_multiple_of(20) {
            exercise_abandoned_cleanup(context, counts)?;
        }
        if counts.cycles.is_multiple_of(360) {
            force_outbox_overflow(&store).await?;
            drain_outbox(&store, &outbox, counts, &mut observed_sequence, true)?;
        }
        let current = sample(context, &store, &lifecycle, started, counts.cycles).await?;
        append_sample(&context.samples_path, &current)?;
        assert_quiescent_invariants(&current)?;
        if counts.cycles > WARMUP_CYCLES {
            assert_sample_growth_bounds(&samples[WARMUP_CYCLES as usize], &current)?;
        }
        samples.push(current);
        if let Some(remaining) = CYCLE_PERIOD.checked_sub(cycle_started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
    }

    let final_sample = sample(context, &store, &lifecycle, started, counts.cycles).await?;
    append_sample(&context.samples_path, &final_sample)?;
    assert_quiescent_invariants(&final_sample)?;
    if counts.cycles >= WARMUP_CYCLES {
        assert_sample_growth_bounds(&samples[WARMUP_CYCLES as usize], &final_sample)?;
    }
    samples.push(final_sample);

    lifecycle.begin_shutdown().await;
    if lifecycle.active_count().await != 0 {
        return Err("lifecycle was not quiescent at shutdown".into());
    }
    drain_outbox(&store, &outbox, counts, &mut observed_sequence, false)?;
    let runtime_counts = runtime.counts();
    *runtime_evidence = (
        runtime_counts.hash_invocations,
        runtime_counts.hash_bytes,
        runtime_counts.resolution_calls,
        runtime_counts.successful_resolutions,
    );
    let expected_resolutions = counts.cycles * 4 + counts.runtime_mutations;
    let expected_hashes = counts.runtime_mutations * 5;
    if runtime_counts.resolution_calls != expected_resolutions
        || runtime_counts.successful_resolutions != expected_resolutions
        || runtime_counts.hash_invocations != expected_hashes
        || (expected_hashes != 0 && runtime_counts.hash_bytes == 0)
    {
        return Err("verified runtime cache hash/resolution invariants regressed".into());
    }
    drop(outbox);
    drop(store);
    let reopened = StateStore::open_at(
        context.journal_path.clone(),
        context.diagnostics_path.clone(),
    )
    .map_err(display_error)?;
    let snapshot = reopened.snapshot().map_err(display_error)?;
    if snapshot.queue.len() != QUEUE_SIZE
        || snapshot.operations.len() > MAX_TERMINAL_ATTEMPTS
        || snapshot
            .operations
            .iter()
            .any(|operation| !operation.state.is_terminal())
        || snapshot.persistence_health.degraded
    {
        return Err("final journal reopen did not preserve a bounded terminal state".into());
    }
    drop(reopened);
    drop(runtime);
    remove_owned_fixture_tree(context)?;
    Ok(())
}

fn capture_runtime_counts(runtime: &VerifiedRuntimeHarness, evidence: &mut (u64, u64, u64, u64)) {
    let counts = runtime.counts();
    *evidence = (
        counts.hash_invocations,
        counts.hash_bytes,
        counts.resolution_calls,
        counts.successful_resolutions,
    );
}

async fn run_cycle(
    context: &Context,
    store: &StateStore,
    lifecycle: &crate::lifecycle::DownloadManager,
    runtime: &VerifiedRuntimeHarness,
    counts: &mut WorkloadCounts,
) -> Result<(), String> {
    let ids = (0..BATCH_SIZE)
        .map(|offset| {
            uuid::Uuid::from_u128(
                ((counts.cycles as usize * BATCH_SIZE + offset) % QUEUE_SIZE + 1) as u128,
            )
            .to_string()
        })
        .collect::<Vec<_>>();
    let admission = lifecycle
        .begin_job_admission(BATCH_SIZE)
        .await
        .map_err(display_error)?;
    let (work, _) = store
        .enqueue(&ids, QueuePriority::Normal)
        .await
        .map_err(display_error)?;
    let operation_ids = work
        .iter()
        .map(|queued| queued.operation_id.clone())
        .collect::<Vec<_>>();
    let _jobs = admission
        .publish(&operation_ids)
        .await
        .map_err(display_error)?;

    let mut tasks = Vec::with_capacity(BATCH_SIZE);
    for (index, expected_id) in operation_ids.iter().enumerate() {
        let queued = store
            .take_next_pending()
            .await
            .ok_or_else(|| "state queue unexpectedly became empty".to_string())?;
        if queued.operation_id != *expected_id {
            return Err("state and lifecycle operation order diverged".into());
        }
        let claim = lifecycle
            .wait_worker_claim()
            .await?
            .ok_or_else(|| "worker claim stopped during an accepting cycle".to_string())?;
        let job = claim
            .registered_job(expected_id)
            .await
            .map_err(display_error)?;
        drop(claim);
        let task_store = store.clone();
        let task_lifecycle = lifecycle.clone();
        let task_context = context.clone();
        let task_id = expected_id.clone();
        tasks.push(tokio::spawn(async move {
            run_cycle_job(
                index,
                task_context,
                task_store,
                task_lifecycle,
                task_id,
                job,
            )
            .await
        }));
    }
    for task in tasks {
        let result = task
            .await
            .map_err(|error| format!("cycle worker task failed: {error}"))??;
        match result.terminal {
            OperationState::Completed => counts.completed += 1,
            OperationState::Cancelled => counts.cancelled += 1,
            OperationState::Failed => counts.failed += 1,
            _ => return Err("cycle worker reported a nonterminal result".into()),
        }
        counts.supervised_successes += u64::from(result.supervised_success);
        counts.supervised_failures += u64::from(result.supervised_failure);
        counts.supervised_cancellations += u64::from(result.supervised_cancellation);
        counts.publications += u64::from(result.publication);
        counts.collision_preservations += u64::from(result.publication);
        counts.operations += 1;
    }
    if lifecycle.active_count().await != 0 || !store.pending_operation_ids().is_empty() {
        return Err("cycle did not return state and lifecycle to quiescence".into());
    }
    for name in ["yt-dlp", "ffmpeg", "ffprobe", "deno"] {
        let lease = runtime
            .resolve(name)?
            .ok_or_else(|| format!("missing runtime fixture {name}"))?;
        drop(lease);
    }
    Ok(())
}

async fn run_cycle_job(
    index: usize,
    context: Context,
    store: StateStore,
    lifecycle: crate::lifecycle::DownloadManager,
    operation_id: String,
    job: crate::downloader::process::DownloadJob,
) -> Result<CycleResult, String> {
    let _slot = lifecycle.acquire_download_slot().await?;
    apply_progress(&store, &operation_id, 25.0).await?;
    let (outcome, result) = match index {
        0 => {
            let output = test_run_supervised_absolute_child(
                &job,
                &context.powershell,
                &[
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "[Console]::Out.WriteLine('phase5-out'); [Console]::Error.WriteLine('phase5-err')"
                        .into(),
                ],
                Duration::from_secs(5),
                64 * 1024,
                64 * 1024,
            )
            .await?;
            if !output.status.success() || output.stdout.is_empty() || output.stderr.is_empty() {
                return Err(format!(
                    "supervised success fixture was incomplete: status={:?}, stdoutBytes={}, stderrBytes={}",
                    output.status.code(),
                    output.stdout.len(),
                    output.stderr.len()
                ));
            }
            exercise_publication(&context, &operation_id, &job).await?;
            (
                DownloadTerminalOutcome::Completed { filename: None },
                CycleResult {
                    terminal: OperationState::Completed,
                    supervised_success: true,
                    supervised_failure: false,
                    supervised_cancellation: false,
                    publication: true,
                },
            )
        }
        1 => {
            let output = test_run_supervised_absolute_child(
                &job,
                &context.powershell,
                &[
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "[Console]::Error.WriteLine('expected-failure'); exit 7".into(),
                ],
                Duration::from_secs(5),
                64 * 1024,
                64 * 1024,
            )
            .await?;
            if output.status.success()
                || output.status.code() != Some(7)
                || output.stderr.is_empty()
            {
                return Err(format!(
                    "supervised failure fixture was incomplete: status={:?}, stdoutBytes={}, stderrBytes={}",
                    output.status.code(),
                    output.stdout.len(),
                    output.stderr.len()
                ));
            }
            (
                DownloadTerminalOutcome::Failed(AppError::new(
                    "phase5_fixture_failure",
                    "The controlled Phase 5 failure fixture exited with status 7.",
                )),
                CycleResult {
                    terminal: OperationState::Failed,
                    supervised_success: false,
                    supervised_failure: true,
                    supervised_cancellation: false,
                    publication: false,
                },
            )
        }
        2 => {
            exercise_process_cancellation(&context, &lifecycle, &operation_id, &job).await?;
            (
                DownloadTerminalOutcome::Cancelled,
                CycleResult {
                    terminal: OperationState::Cancelled,
                    supervised_success: false,
                    supervised_failure: false,
                    supervised_cancellation: true,
                    publication: false,
                },
            )
        }
        _ => (
            DownloadTerminalOutcome::Completed { filename: None },
            CycleResult {
                terminal: OperationState::Completed,
                supervised_success: false,
                supervised_failure: false,
                supervised_cancellation: false,
                publication: false,
            },
        ),
    };
    apply_progress(&store, &operation_id, 75.0).await?;
    store
        .finalize_download(&operation_id, outcome)
        .await
        .map_err(display_error)?;
    lifecycle.finish(&operation_id).await;
    Ok(result)
}

async fn apply_progress(store: &StateStore, id: &str, progress: f64) -> Result<(), String> {
    let applied = store
        .apply_download_progress(
            id,
            &DownloadProgress {
                download_id: id.to_owned(),
                status: "downloading".into(),
                progress,
                phase: Some("download".into()),
                download_progress: Some(progress),
                conversion_progress: None,
                speed: Some("fixture".into()),
                eta: None,
                error: None,
                error_code: None,
                error_detail: None,
                filename: None,
            },
        )
        .await
        .map_err(display_error)?;
    if applied.is_none() {
        return Err("nonterminal progress was unexpectedly rejected".into());
    }
    Ok(())
}

async fn exercise_process_cancellation(
    context: &Context,
    lifecycle: &crate::lifecycle::DownloadManager,
    operation_id: &str,
    job: &crate::downloader::process::DownloadJob,
) -> Result<(), String> {
    let ready = context
        .fixture_root
        .join(format!("cancel-{operation_id}.ready"));
    let escaped = ready.to_string_lossy().replace('\'', "''");
    let args = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        format!("[IO.File]::WriteAllText('{escaped}','ready'); Start-Sleep -Seconds 30"),
    ];
    let wait_job = job.clone();
    let executable = context.powershell.clone();
    let waiter = tokio::spawn(async move {
        test_run_supervised_absolute_child(
            &wait_job,
            &executable,
            &args,
            Duration::from_secs(35),
            64 * 1024,
            64 * 1024,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "supervised cancellation fixture did not become ready".to_string())?;
    lifecycle
        .cancel(operation_id)
        .await
        .map_err(display_error)?;
    let error = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .map_err(|_| "supervised cancellation did not reap promptly".to_string())?
        .map_err(|error| format!("supervised cancellation task failed: {error}"))?
        .expect_err("cancelled supervised fixture must fail");
    let _ = fs::remove_file(ready);
    if !error.starts_with("process_cancelled:") {
        return Err(format!("unexpected cancellation result: {error}"));
    }
    Ok(())
}

async fn exercise_publication(
    context: &Context,
    operation_id: &str,
    job: &crate::downloader::process::DownloadJob,
) -> Result<(), String> {
    let stage = test_create_owned_stage(&context.output_root, operation_id)?;
    let staged = stage.join("media.bin");
    fs::write(&staged, b"phase5-published-bytes").map_err(to_string("write staged output"))?;
    let desired = context.output_root.join("collision.bin");
    fs::write(&desired, b"preserve-existing").map_err(to_string("write collision control"))?;
    let published = test_publish_staged_file(&staged, &desired, Some(job)).await?;
    if fs::read(&desired).map_err(to_string("read collision control"))? != b"preserve-existing"
        || fs::read(&published).map_err(to_string("read published output"))?
            != b"phase5-published-bytes"
        || published == desired
    {
        return Err("collision-safe publication did not preserve both byte sequences".into());
    }
    test_cleanup_owned_stage(&context.output_root, operation_id)?;
    fs::remove_file(&published).map_err(to_string("remove published fixture"))?;
    fs::remove_file(&desired).map_err(to_string("remove collision control"))?;
    remove_empty_staging_root(&context.output_root)?;
    Ok(())
}

async fn exercise_lifecycle_drain(
    lifecycle: &crate::lifecycle::DownloadManager,
    counts: &mut WorkloadCounts,
) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let admission = lifecycle
        .begin_job_admission(1)
        .await
        .map_err(display_error)?;
    let jobs = admission
        .publish(std::slice::from_ref(&id))
        .await
        .map_err(display_error)?;
    let ticket = lifecycle.begin_cancel_all().await.map_err(display_error)?;
    if !jobs[0].is_cancelled() || lifecycle.begin_job_admission(1).await.is_ok() {
        return Err("cancel-all did not cancel active work and close admission".into());
    }
    lifecycle.finish(&id).await;
    if lifecycle.wait_for_drain(ticket).await != DrainCompletion::Idle
        || !lifecycle.resume_after_drain(ticket).await
    {
        return Err("cancel-all drain did not resume after quiescence".into());
    }
    counts.lifecycle_drains += 1;
    Ok(())
}

async fn exercise_runtime_mutation(
    runtime: &Arc<VerifiedRuntimeHarness>,
    counts: &mut WorkloadCounts,
) -> Result<(), String> {
    let lease = runtime
        .resolve("yt-dlp")?
        .ok_or_else(|| "runtime mutation fixture lease was unavailable".to_string())?;
    let runtime_for_mutation = Arc::clone(runtime);
    let mut mutation = tokio::spawn(async move { runtime_for_mutation.begin_mutation().await });
    if tokio::time::timeout(Duration::from_millis(100), &mut mutation)
        .await
        .is_ok()
    {
        return Err("runtime mutation acquired while a verified lease was held".into());
    }
    drop(lease);
    let mutation = tokio::time::timeout(Duration::from_secs(3), mutation)
        .await
        .map_err(|_| "runtime mutation did not acquire after lease release".to_string())?
        .map_err(|error| format!("runtime mutation task failed: {error}"))?;
    mutation.refresh().await?;
    counts.runtime_mutations += 1;
    Ok(())
}

async fn exercise_inherited_pipe(
    context: &Context,
    lifecycle: &crate::lifecycle::DownloadManager,
    counts: &mut WorkloadCounts,
) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let job = lifecycle.register(&id).await?;
    let result = test_run_supervised_inherited_pipe_fixture(
        &job,
        TestInheritedPipeFixtureOptions {
            caller_root: &context.fixture_root,
            parent_script: "[Console]::Out.WriteLine('parent-exit')",
            descendant_script:
                "[Console]::Out.WriteLine('descendant-ready'); Start-Sleep -Seconds 30",
            timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_millis(500),
            stdout_limit: 64 * 1024,
            stderr_limit: 64 * 1024,
        },
    )
    .await;
    lifecycle.finish(&id).await;
    let error = result.expect_err("inherited pipe fixture must reach the drain deadline");
    if !error.starts_with("process_drain_timeout:") {
        return Err(format!("unexpected inherited-pipe result: {error}"));
    }
    counts.inherited_pipe_drains += 1;
    Ok(())
}

fn exercise_abandoned_cleanup(
    context: &Context,
    counts: &mut WorkloadCounts,
) -> Result<(), String> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let stage = test_create_owned_stage(&context.output_root, &operation_id)?;
    fs::write(stage.join("partial.bin"), b"partial").map_err(to_string("write partial stage"))?;
    let staging_root = stage
        .parent()
        .ok_or_else(|| "stage has no parent".to_string())?;
    let unowned = staging_root.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&unowned).map_err(to_string("create unowned stage control"))?;
    fs::write(unowned.join("keep.bin"), b"keep")
        .map_err(to_string("write unowned stage control"))?;
    test_cleanup_abandoned_stages(&context.output_root)?;
    if stage.exists() || fs::read(unowned.join("keep.bin")).ok().as_deref() != Some(b"keep") {
        return Err("abandoned cleanup removed the wrong ownership class".into());
    }
    fs::remove_dir_all(&unowned).map_err(to_string("remove unowned stage control"))?;
    remove_empty_staging_root(&context.output_root)?;
    counts.abandoned_stage_cleanups += 1;
    counts.unowned_stage_preservations += 1;
    Ok(())
}

async fn force_outbox_overflow(store: &StateStore) -> Result<(), String> {
    for index in 0..(MAX_OUTBOX_BATCHES + 32) {
        store
            .set_maintenance(index.is_multiple_of(2), false)
            .await
            .map_err(display_error)?;
    }
    store
        .set_maintenance(false, false)
        .await
        .map_err(display_error)?;
    let stats = store.outbox_stats();
    if stats.queued_batches > MAX_OUTBOX_BATCHES
        || stats.queued_deltas > MAX_OUTBOX_DELTAS
        || stats.estimated_bytes > MAX_OUTBOX_ESTIMATED_BYTES
        || stats.coalesced_resyncs == 0
    {
        return Err("outbox did not coalesce within its configured hard bounds".into());
    }
    Ok(())
}

fn drain_outbox(
    store: &StateStore,
    reader: &crate::outbox::StateOutboxReader,
    counts: &mut WorkloadCounts,
    observed_sequence: &mut u64,
    require_resync: bool,
) -> Result<(), String> {
    let mut saw_resync = false;
    while let Some(publication) = reader.try_recv() {
        match publication {
            StatePublication::Deltas(deltas) => {
                for delta in deltas.iter() {
                    if delta.sequence != observed_sequence.saturating_add(1) {
                        return Err(format!(
                            "outbox sequence was not contiguous: observed {}, received {}",
                            *observed_sequence, delta.sequence
                        ));
                    }
                    *observed_sequence = delta.sequence;
                }
                counts.deltas_observed += deltas.len() as u64;
            }
            StatePublication::ResyncRequired(required) => {
                let snapshot = store.snapshot().map_err(display_error)?;
                if snapshot.latest_sequence < required.latest_sequence {
                    return Err("resync snapshot lagged the outbox high-water mark".into());
                }
                counts.resyncs_observed += 1;
                *observed_sequence = snapshot.latest_sequence;
                saw_resync = true;
            }
        }
    }
    let stats = reader.stats();
    if stats.queued_batches != 0 || stats.queued_deltas != 0 || stats.estimated_bytes != 0 {
        return Err("outbox did not drain to zero".into());
    }
    if require_resync && !saw_resync {
        return Err("overflow workload did not publish a resync marker".into());
    }
    Ok(())
}

async fn sample(
    context: &Context,
    store: &StateStore,
    lifecycle: &crate::lifecycle::DownloadManager,
    started: Instant,
    cycle: u64,
) -> Result<ResourceSample, String> {
    let memory = process_memory()?;
    let snapshot = store.snapshot().map_err(display_error)?;
    let outbox = store.outbox_stats();
    let descendants = descendant_processes()?;
    Ok(ResourceSample {
        elapsed_seconds: started.elapsed().as_secs(),
        cycle,
        working_set_bytes: memory.0,
        private_bytes: memory.1,
        handle_count: process_handle_count()?,
        descendant_count: descendants.len(),
        descendants,
        queue_items: snapshot.queue.len(),
        retained_operations: snapshot.operations.len(),
        pending_operations: store.pending_operation_ids().len(),
        active_lifecycle_jobs: lifecycle.active_count().await,
        outbox_batches: outbox.queued_batches,
        outbox_deltas: outbox.queued_deltas,
        outbox_estimated_bytes: outbox.estimated_bytes,
        outbox_coalesced_resyncs: outbox.coalesced_resyncs,
        journal_bytes: file_len(&context.journal_path),
        diagnostics_bytes: recursive_regular_file_bytes(&context.diagnostics_path)?,
        residual_output_entries: recursive_entry_count(&context.output_root)?,
        latest_sequence: snapshot.latest_sequence,
    })
}

fn assert_quiescent_invariants(sample: &ResourceSample) -> Result<(), String> {
    if sample.queue_items != QUEUE_SIZE
        || sample.retained_operations > MAX_TERMINAL_ATTEMPTS
        || sample.pending_operations != 0
        || sample.active_lifecycle_jobs != 0
        || sample.outbox_batches > MAX_OUTBOX_BATCHES
        || sample.outbox_deltas > MAX_OUTBOX_DELTAS
        || sample.outbox_estimated_bytes > MAX_OUTBOX_ESTIMATED_BYTES
        || sample.descendant_count != 0
        || sample.residual_output_entries != 0
        || sample.journal_bytes > MAX_JOURNAL_BYTES
        || sample.diagnostics_bytes > MAX_DIAGNOSTICS_BYTES
    {
        return Err(format!(
            "a quiescent sample violated a controlled-harness invariant: {}",
            serde_json::to_string(sample)
                .unwrap_or_else(|_| "sample serialization failed".to_owned())
        ));
    }
    Ok(())
}

fn assert_sample_growth_bounds(
    baseline: &ResourceSample,
    sample: &ResourceSample,
) -> Result<(), String> {
    if sample.private_bytes.saturating_sub(baseline.private_bytes) > MAX_PRIVATE_GROWTH_BYTES
        || sample
            .working_set_bytes
            .saturating_sub(baseline.working_set_bytes)
            > MAX_WORKING_SET_GROWTH_BYTES
        || sample.handle_count.saturating_sub(baseline.handle_count) > MAX_HANDLE_GROWTH
    {
        return Err(format!(
            "a post-warmup resource sample exceeded growth bounds: baseline={}, sample={}",
            serde_json::to_string(baseline)
                .unwrap_or_else(|_| "baseline serialization failed".to_owned()),
            serde_json::to_string(sample)
                .unwrap_or_else(|_| "sample serialization failed".to_owned())
        ));
    }
    Ok(())
}

impl Context {
    fn from_environment() -> Result<Self, String> {
        let run_root = required_absolute_path("NUCLEAR_SOAK_RUN_ROOT")?;
        let fixture_root = run_root.join("fixtures").join("workload");
        if !run_root.join(OWNER_MARKER).is_file() || fixture_root.exists() {
            return Err(
                "soak run root lacks its ownership marker or fixture root is not fresh".into(),
            );
        }
        let evidence_path = required_contained_path("NUCLEAR_SOAK_EVIDENCE_PATH", &run_root)?;
        let samples_path = required_contained_path("NUCLEAR_SOAK_SAMPLES_PATH", &run_root)?;
        let duration_seconds = required_text("NUCLEAR_SOAK_DURATION_SECS")?
            .parse::<u64>()
            .map_err(|_| "NUCLEAR_SOAK_DURATION_SECS must be an integer".to_string())?;
        if !(120..=10_800).contains(&duration_seconds) {
            return Err("soak duration must be between 120 and 10800 seconds".into());
        }
        let windows = PathBuf::from(std::env::var_os("WINDIR").ok_or("WINDIR is unavailable")?);
        let powershell = windows.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        if !powershell.is_file() {
            return Err("required absolute Windows fixture executables are unavailable".into());
        }
        Ok(Self {
            journal_path: fixture_root.join("state/state-v1.dpapi"),
            diagnostics_path: fixture_root.join("diagnostics/backend.jsonl"),
            output_root: fixture_root.join("outputs"),
            run_root,
            fixture_root,
            evidence_path,
            samples_path,
            powershell,
            duration: Duration::from_secs(duration_seconds),
            label: required_text("NUCLEAR_SOAK_LABEL")?,
            profile: required_text("NUCLEAR_SOAK_PROFILE")?,
        })
    }
}

fn fixture_queue(output_root: &Path) -> Vec<QueueItemRecord> {
    (0..QUEUE_SIZE)
        .map(|index| QueueItemRecord {
            schema_version: APP_SCHEMA_VERSION,
            id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
            source_url: format!("https://fixture.invalid/phase5/{index}"),
            title: format!("Phase 5 fixture {index}"),
            available_qualities: vec!["1080p".into()],
            has_audio: true,
            cookie_config: None,
            format: "mp4".into(),
            quality: "1080p".into(),
            output_dir: output_root.to_string_lossy().into_owned(),
            filename_override: Some(format!("phase5-{index:03}")),
            compat_config_path: None,
            state: QueueItemState::Inert,
            latest_operation_id: None,
            created_at_ms: 1_700_000_000_000 + index as u64,
            updated_at_ms: 1_700_000_000_000 + index as u64,
        })
        .collect()
}

fn seed_journal(path: &Path, queue: Vec<QueueItemRecord>) -> Result<(), String> {
    let (store, _, quarantine) = JournalStore::open(path.to_path_buf()).map_err(display_error)?;
    if quarantine.is_some() {
        return Err("fresh Phase 5 journal was unexpectedly quarantined".into());
    }
    store
        .save(&PersistentJournal {
            schema_version: APP_SCHEMA_VERSION,
            revision: 1,
            queue,
            operations: Vec::new(),
            pending_app_update: None,
        })
        .map_err(display_error)
}

fn publish_json<T: Serialize>(path: &Path, value: &T, create_new: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "evidence path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(to_string("create evidence directory"))?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .truncate(!create_new)
        .create(!create_new)
        .create_new(create_new);
    let mut file = options
        .open(path)
        .map_err(to_string("open evidence file"))?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|error| error.to_string())?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(to_string("sync evidence file"))
}

fn append_sample(path: &Path, sample: &ResourceSample) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "sample path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(to_string("create sample directory"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(to_string("open sample log"))?;
    serde_json::to_writer(&mut file, sample).map_err(|error| error.to_string())?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(to_string("sync sample log"))
}

fn remove_owned_fixture_tree(context: &Context) -> Result<(), String> {
    let expected_parent = context.run_root.join("fixtures");
    if !context.run_root.join(OWNER_MARKER).is_file()
        || context.fixture_root.parent() != Some(expected_parent.as_path())
    {
        return Err("refusing to remove a fixture tree outside the owned run root".into());
    }
    fs::remove_dir_all(&context.fixture_root).map_err(to_string("remove owned fixture tree"))
}

fn required_text(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} must be explicitly set by the soak runner"))
}

fn required_absolute_path(name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(std::env::var_os(name).ok_or_else(|| format!("{name} must be set"))?);
    if !path.is_absolute() {
        return Err(format!("{name} must be absolute"));
    }
    Ok(path)
}

fn required_contained_path(name: &str, root: &Path) -> Result<PathBuf, String> {
    let path = required_absolute_path(name)?;
    if !path.starts_with(root) {
        return Err(format!("{name} must stay inside the owned run root"));
    }
    Ok(path)
}

fn to_string(context: &'static str) -> impl FnOnce(std::io::Error) -> String {
    move |error| format!("{context}: {error}")
}

fn display_error(error: AppError) -> String {
    format!("{}: {}", error.code, error.summary)
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn recursive_regular_file_bytes(root: &Path) -> Result<u64, String> {
    if !root.exists() {
        return Ok(0);
    }
    let mut bytes = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(to_string("read diagnostics directory"))? {
            let entry = entry.map_err(to_string("read diagnostics entry"))?;
            let metadata = entry
                .metadata()
                .map_err(to_string("inspect diagnostics entry"))?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(bytes)
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn recursive_entry_count(root: &Path) -> Result<usize, String> {
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(to_string("read fixture output directory"))? {
            let entry = entry.map_err(to_string("read fixture output entry"))?;
            count += 1;
            if entry
                .file_type()
                .map_err(to_string("inspect fixture output entry"))?
                .is_dir()
            {
                pending.push(entry.path());
            }
        }
    }
    Ok(count)
}

fn remove_empty_staging_root(output_root: &Path) -> Result<(), String> {
    let root = output_root.join(".nuclear-downloader-staging");
    match fs::remove_dir(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove empty staging root: {error}")),
    }
}

fn process_handle_count() -> Result<u32, String> {
    let mut count = 0u32;
    let succeeded = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    if succeeded == 0 {
        Err(format!(
            "GetProcessHandleCount failed: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(count)
    }
}

fn descendant_processes() -> Result<Vec<DescendantEvidence>, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "CreateToolhelp32Snapshot failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut processes = HashMap::<u32, (u32, String)>::new();
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut success = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while success {
        let name_end = entry
            .szExeFile
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(entry.szExeFile.len());
        processes.insert(
            entry.th32ProcessID,
            (
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..name_end]),
            ),
        );
        success = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    let root = unsafe { GetCurrentProcessId() };
    let root_created = process_creation_time(root)
        .ok_or_else(|| "could not read the soak process creation time".to_string())?;
    let mut descendants = HashMap::<u32, DescendantEvidence>::new();
    let mut descendant_created = HashMap::new();
    loop {
        let before = descendants.len();
        for (&pid, (parent, image_name)) in &processes {
            let parent_created = if *parent == root {
                Some(root_created)
            } else {
                descendant_created.get(parent).copied()
            };
            let Some(parent_created) = parent_created else {
                continue;
            };
            let Some(child_created) = process_creation_time(pid) else {
                continue;
            };
            if child_created >= parent_created {
                descendants.insert(
                    pid,
                    DescendantEvidence {
                        pid,
                        parent_pid: *parent,
                        creation_filetime: child_created,
                        image_name: image_name.clone(),
                    },
                );
                descendant_created.insert(pid, child_created);
            }
        }
        if descendants.len() == before {
            let mut descendants = descendants.into_values().collect::<Vec<_>>();
            descendants.sort_by_key(|descendant| descendant.pid);
            return Ok(descendants);
        }
    }
}

fn process_creation_time(pid: u32) -> Option<u64> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    let succeeded =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    unsafe { CloseHandle(process) };
    (succeeded != 0)
        .then_some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn process_memory() -> Result<(u64, u64), String> {
    #[repr(C)]
    struct Counters {
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
    #[link(name = "Psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(process: isize, counters: *mut Counters, size: u32) -> i32;
    }
    let mut counters: Counters = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<Counters>() as u32;
    let succeeded = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess() as isize,
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        )
    };
    if succeeded == 0 {
        Err(format!(
            "GetProcessMemoryInfo failed: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok((
            counters.working_set_size as u64,
            counters.private_usage as u64,
        ))
    }
}
