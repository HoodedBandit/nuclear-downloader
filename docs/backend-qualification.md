# Backend finding-to-test checklist

This checklist links the overhaul's failure modes to executable evidence. The phase
results and measurements are recorded in `backend-overhaul.md`. The subsequent
maintainability refactor is tracked in `backend-maintainability.md`; its local
automated/component qualification passed, while the external gates below remain
blocked. A passing unit or
fixture test is not a passing packaged-application acceptance case.

## Regression coverage

These tests passed in the original overhaul gate (270 passed, zero failures at
`34d0769`) and remain in the refactor suite (304 passed, zero failures after
`d1f6571`). Added regressions below passed in that refactor suite. Names are unique
filters accepted by `scripts/test-backend.ps1 -Filter <name>`.

| Failure mode or required invariant | Representative executed regression |
| --- | --- |
| Dismissal, seven-day expiry, or count retention leaves invalid queue references | `removal_detaches_terminal_history_and_reopens_without_quarantine`; `age_pruning_clears_only_the_reference_to_the_expired_attempt`; `count_retention_repairs_references_to_evicted_attempts` |
| Existing dangling latest-operation references reset an otherwise valid queue | `dangling_latest_reference_is_repaired_once_and_persisted` |
| Future schema or oversized journal is destroyed during recovery | `future_schema_fails_closed_without_quarantining_the_journal`; `future_record_schema_is_preserved_before_decoding_its_shape`; `oversized_existing_journal_is_preserved_without_quarantine` |
| Invalid or failed saves replace the previous journal | `invalid_prepared_journal_preserves_previous_published_bytes`; `failed_durable_mutations_roll_back_memory_and_pending_work`; `oversized_save_preserves_the_previous_journal_and_removes_temporary_files` |
| Caller cancellation abandons a commit or allows stale state publication | `detached_commit_keeps_the_gate_until_save_and_swap_finish`; `stale_journal_revision_cannot_overwrite_a_newer_snapshot` |
| Terminal success appears before its durable save, or save failure leaves active work without a worker | `transient_terminal_save_failure_is_retried_before_completion_is_acknowledged`; `exhausted_terminal_save_retries_retain_recovery_intent_until_the_next_commit`; `failed_finalizer_task_installs_a_degraded_terminal_compensation` |
| Failure after disk save causes the next recovery flush to be skipped | `failure_after_save_advances_compensation_past_the_persisted_candidate_revision` (includes two subsequent journal opens) |
| Late cancellation misrepresents an already published file | `published_download_completion_wins_a_late_cancellation` |
| Restart automatically resumes interrupted work | `restart_converts_nonterminal_operations_to_interrupted` |
| Startup cleanup races admission | `startup_gate_blocks_admission_and_worker_claim_until_open`; `tracked_command_waits_for_startup_and_preserves_state_on_startup_failure` |
| Cancellation races operation registration or dequeue | `cancellation_waits_for_atomic_job_publication`; `cancellation_after_dequeue_before_lookup_never_calls_the_process_factory`; `cancelled_job_never_runs_suspended_child_first_instruction` |
| Cancel All leaves admission closed after its response deadline | `late_cancel_all_completion_resumes_the_same_generation`; `shutdown_supersedes_a_late_cancel_all_completion` |
| Cancel All unwinds after acquiring a drain generation and strands pending registrations | `abandoned_empty_drain_reopens_admission`; `abandoned_drain_finalizes_pending_work_before_reopening`; `abandoned_drain_releases_already_finalized_pending_registration` |
| Inspection admission unwinds after its durable record but before task publication | `panic_after_durable_inspection_before_publish_is_cleaned_up` |
| Maintenance interleaves with partial queue admission | `batch_admission_and_maintenance_cannot_partially_interleave`; `queued_operation_blocks_maintenance_before_worker_registration` |
| Shutdown ignores cleanup, updates, or protected publication | `shutdown_accounts_for_cleanup_and_rejects_unrelated_new_tasks`; `publication_defers_update_cancellation_and_shutdown_deadline`; `committed_installer_handoff_permanently_closes_admission` |
| Playlist discovery stalls indefinitely or loses its deadline classification | `stalled_inspection_hits_deadline_without_becoming_user_cancelled`; `streamed_inspection_enforces_one_cumulative_output_limit` |
| Pipe inheritance or a blocked progress callback defeats cancellation or child-exit handling | `streamed_process_bounds_inherited_pipes_after_parent_exit`; `cancellation_remains_live_while_progress_callback_is_blocked`; `child_exit_starts_drain_deadline_while_progress_callback_is_blocked`; `split_stdout_line_survives_parent_exit_selection` |
| Runtime promotion interruption cannot recover repeatedly | `recovery_is_idempotent_after_every_durable_checkpoint`; `recovery_resumes_after_rollback_succeeds_but_journal_clear_fails` |
| Corrupt same-version runtime cannot be repaired, or unrelated data is deleted | `corrupted_marker_owned_same_version_is_authorized_for_repair`; `unowned_same_version_directory_is_preserved`; `quarantined_transaction_remains_fail_closed_and_preserves_unowned_directory` |
| Concurrent runtime mutation or ambiguous transaction paths are admitted | `operating_system_lock_rejects_a_second_owner`; `mutation_lock_rejects_a_reparse_ancestor_without_creating_the_leaf`; `changed_journal_is_preserved_instead_of_quarantining_stale_state` |
| A runtime journal grows after metadata inspection and bypasses its read budget | `journal_growth_after_metadata_is_bounded_and_preserved` (observed failing before the bounded read, then passed in the 291-test suite) |
| A whole-file reader consumes beyond its limit after a file grows | `exact_limits_are_accepted_and_unbounded_sources_stop_after_one_probe_byte`; `asynchronous_reader_enforces_the_same_exact_limit_and_probe_bound`; `io_failure_is_distinct_from_overflow_and_invalid_limits_do_not_read` |
| Encrypted journal growth loses the oversize error and preservation contract | `encrypted_journal_growth_after_metadata_is_rejected_with_the_size_contract`; `oversized_existing_journal_is_preserved_without_quarantine` |
| Oversized runtime pointer, manifest, or authentication data is admitted | `managed_runtime_pointer_rejects_content_beyond_its_read_limit`; `runtime_manifest_rejects_content_beyond_its_read_limit`; `regular_runtime_file_reader_rejects_content_beyond_limit` |
| Oversized ownership markers authorize runtime deletion or block cleanup of other owned residue | `installed_runtime_owner_marker_rejects_oversized_content_without_mutation`; `oversized_runtime_update_owner_marker_is_not_owned`; `abandoned_runtime_cleanup_preserves_oversized_marker_and_continues`; `removal_refuses_and_preserves_oversized_runtime_update_marker` |
| Installer ownership data grows after the initial metadata observation | `ownership_record_growth_is_bounded_and_unrelated_files_are_preserved` |
| A failed partial-file deletion erases ownership proof and prevents later cleanup | `failed_partial_removal_retains_ownership_for_startup_retry` (failed before the fix, then passed; forces a Windows sharing failure and verifies normal cleanup after the lease is released) |
| Invalid installer cache permanently blocks a verified replacement | `invalid_cached_installer_is_quarantined_before_fresh_verification`; `unowned_canonical_collision_is_preserved_while_prepared_path_is_verified` |
| Installer starts before its recovery marker is durable or never reconciles | `app_update_handoff_does_not_launch_from_an_unpersisted_marker`; `app_update_handoff_completes_only_on_target_version_and_is_restart_idempotent`; `app_update_handoff_mismatch_becomes_retryable_interruption` |
| Verified executable identity is lost before runtime mutation | `verified_runtime_cache_waits_for_leases_and_blocks_reacquisition`; `managed_runtime_integrity_is_verified_before_trust` |
| Optional Deno absence blocks otherwise usable tools | `verified_snapshot_allows_optional_deno_to_resolve_as_absent` |
| Inspection allocations escape the aggregate budget through retained snapshots or events | `inspection_budget_counts_payloads_retained_by_a_snapshot`; `inspection_budget_counts_payloads_retained_only_by_the_outbox`; `many_empty_quality_records_are_counted_by_the_inspection_budget` |
| Oversized actionable input is silently accepted or display truncation breaks UTF-8 | `inspection_display_fields_are_utf8_safely_truncated_and_action_fields_rejected`; `rejects_actionable_request_fields_over_four_kibibytes` |
| Concurrent event producers reorder state or delivery failure loses its resync high-water mark | `five_progress_producers_publish_one_contiguous_authoritative_stream`; `sink_failure_collapses_pending_deltas_to_latest_resync`; `shutdown_waits_for_late_producer_before_final_event_drain` |
| A late producer's final shutdown delivery fails after the first event drain | `shutdown_retries_late_producer_failure_as_high_water_resync` |
| Output guessing publishes an ambiguous or unrelated file | `machine_record_resolves_the_exact_staged_output`; `malformed_present_record_never_uses_fallback`; `multiple_machine_records_are_rejected_as_ambiguous_authority`; `absent_record_rejects_multiple_media_candidates_without_mtime_selection` |
| Publication overwrites an existing file or cleanup deletes unowned staging | `concurrent_publication_allocates_distinct_names`; `publishing_never_overwrites_and_adds_a_suffix`; `abandoned_stage_cleanup_deletes_only_marker_owned_uuid_directories` |
| Runtime fixture metrics race other tests or replacement outlives executable leases | `explicit_root_runtime_harness_reuses_leases_and_refreshes` (per-root counters; controlled and parallel suites both passed) |

The recovery checkpoint fixtures exercise production state-machine and filesystem
helpers with controlled authentication predicates. Separate tests verify signed
descriptors and real Minisign validation. These are complementary tests; they do
not simulate physical power loss, a real NSIS handoff, or a maintainer-signed update.

## Performance evidence

The original overhaul's recorded baseline and after run use the same host, development profile, queue
sizes, and sample counts. The comparison checks all 39 hard requirements, including
snapshots during blocked real journal I/O, contiguous event sequences, outbox
bounds, and 408 successful runtime lease resolutions without repeated tool hashes.
Timing changes and memory tradeoffs are explicitly reviewed in `backend-overhaul.md`.
The maintainability comparison has 45 hard gates and uses current-mode workloads
on both revisions. Its final matching-host measurements are tracked separately in
`backend-maintainability.md`; historical comparisons do not qualify the refactor.

## Integrated qualification status

| Gate | Required evidence | Current status |
| --- | --- | --- |
| Isolated two-hour mixed backend soak | Exact compiled test executable hash, source hashes, full two-hour duration, workload counters, bounded memory/handles/outbox, no surviving owned descendants or unexplained owned staging | Final `d1f6571` refactor run passed: 7,203.095 seconds, 7,190 operations, 1,440 clear quiescent samples, 156 unchanged source/build inputs, journal reopened, observed process exited and fixtures empty. All 20 post-run checks passed. Tracked receipt: `backend-maintainability-soak.json`; raw evidence: `target/soak/after-20260909T020321Z-e35c8e5cfdcd499daf3901b16481c134/`. The original overhaul's 7,202.276-second run remains historical only. |
| Native workflows | Successful video and audio, playlist discovery, explicit retry, collision-safe publication, cancellation, forced interruption/restart, and update/repair cases | Acceptance source expanded; pinned yt-dlp validated the two-entry loopback playlist; native app execution remains blocked on the disposable environment |
| Exact installer and portable artifacts on a clean Windows 11 x64 desktop | Candidate inventory and asset hashes, client OS build, WebView2 and tool versions, executed cases | Blocked: no disposable clean desktop environment supplied |
| Advertised YouTube and X extractor smoke tests | Maintainer-controlled fixture configuration and candidate-bound results | Blocked: controlled fixture URLs not supplied |
| Cookie and signed-update acceptance | Exact candidate digest, case ID, operator, timestamp, and relevant observed versions | Blocked: protected acceptance environment/results unavailable |
| Authentic release candidate packaging | Maintainer public trust configuration and protected signing credentials/environment | Blocked: required maintainer trust/signing configuration unavailable |

Publishing is a separate action and is not part of this overhaul. Missing fixtures,
signing configuration, or clean-machine evidence cannot be replaced with passing
unit tests, synthetic signing keys, or tests against the developer's user profile.
