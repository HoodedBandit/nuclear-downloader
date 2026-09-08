# Backend overhaul implementation and validation

Baseline: `c42a86780c0d0f39e2e70329d5e79659faf1516f` (0.6.0).
Target: Windows 11 x64. UI changes are limited to backend-contract integration.
No automatic resume, concurrency increase, publishing, or unrelated dependency upgrades.

## Phase gates

| Phase | Status | Gate |
| --- | --- | --- |
| Baseline | Passed | `cargo test --manifest-path nuclear-app/src-tauri/Cargo.toml --locked --offline --all-features`: 163 passed |
| 1. Persistence and completion | Passed | Rust 178/178; frontend 59/59; bindings, types, Clippy, lint, formatting |
| 2. Lifecycle and processes | Passed | Rust 199/199; Clippy and formatting; deterministic lifecycle/process regressions |
| 3. Runtime and updater recovery | Passed | Rust 219/219; Clippy and formatting; promotion checkpoints, preservation, handoff |
| 4. Performance and boundaries | Passed | Rust 264/264; Clippy, formatting, frontend 64/64; before/after benchmark hard gates 39/39 |
| 5. Integrated qualification | Local gates passed; external qualification blocked | Rust 270/270, frontend 64/64, renderer 2/2, contract/package checks, and two-hour isolated soak passed; authentic candidate/native/manual evidence remains unavailable |

Each phase must pass its regression gate before implementation of the next phase begins.
Tests use isolated temporary journals and fixture output directories, not the user's application data.

## Phase 1 implementation

- Queue-reference repair and terminal retention share one journal helper. Unsupported
  schemas remain preserved; structurally invalid prepared journals cannot replace the file.
- Durable mutations prepare a candidate, serialize on an asynchronous mutation gate,
  save on a blocking worker, then install the candidate. The snapshot mutex is free
  during disk I/O. The commit worker owns the gate even if its caller is cancelled.
- Download workers return a typed terminal outcome. The finalizer retries persistence
  immediately, after 250 ms, and after another second. Exhaustion exposes
  `state_persistence_failed`, retaining the intended outcome and published path.
- Persistence health and published paths flow through generated contracts and existing
  frontend state/error adapters. Stale raw progress cannot regress terminal rows.

Executed Phase 1 gate:

- `scripts/test-backend.ps1`: 178 passed, 0 failed (13.12 s test execution after
  the final internal representation change). Includes generated binding contracts,
  two-restart journal repair, caller cancellation during commit, terminal save retry,
  exhausted retry/degraded flush, and publication racing cancellation.
- `cargo clippy --manifest-path nuclear-app/src-tauri/Cargo.toml --locked --offline
  --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --manifest-path nuclear-app/src-tauri/Cargo.toml --check`: passed.
- Pinned `npm test`: 59 passed across 10 files.
- Pinned `npm run check`: 0 errors, 0 warnings.
- Pinned `npm run lint`: passed. Prettier whole-project check: passed.

Clippy identified the added recovery field's effect on event size. Boxing that optional
field reduced the common state representation without changing the JSON/TypeScript
contract; the full Rust gate passed again afterward.

The pinned Node 22.23.1 and npm 10.9.9 toolchain is isolated under the ignored Rust
`target/toolchains` directory. The system toolchain and application data are unchanged.
`scripts/test-backend.ps1` confines Rust test temporary files to a marker-owned
`target/regression-temp` directory and preserves them on failure.

## Phase 2 implementation and gate

One lifecycle coordinator owns admission, pre-registered jobs, tracked tasks,
cancellation, maintenance generations, and update publication guards. Admission
opens only after tracked startup cleanup and worker publication. IPC cancellation
does not abandon backend admission or cancellation bookkeeping.

Cancel All responds within ten seconds while its tracked drain continues. Shutdown
cancels work and allows fifteen seconds for ordinary cleanup; it waits longer for
an already-started runtime/installer publication to commit or roll back. Publication
success is latched through terminal state finalization so late cancellation cannot
leave a completed update represented as cancelling.

Inspection permits and playlist discovery share a 120-second outer deadline. Child
exit, cancellation, stdout, and stderr are supervised together; inherited pipes have
a five-second drain bound. Cleanup warnings preserve successful download outcomes.

Executed gate: `scripts/test-backend.ps1` passed 199 tests with zero failures
(13.41 s test execution). This includes production-helper tests for cancellation
after dequeue and caller cancellation during admission, and coordinator barriers
for startup, maintenance, late drains, shutdown, and publication. Clippy with
`--all-targets --all-features -- -D warnings` and whole-crate formatting passed.
The Phase 1 frontend contract is unchanged by Phase 2.

## Phase 3 implementation

Runtime publication now records durable promotion checkpoints under an OS-backed
mutation lock. Startup recovers those transactions before abandoned-stage cleanup.
Ownership markers and authenticated executable integrity serve separate purposes,
so an owned corrupted installation can be repaired at the same version while
ambiguous directories and quarantines remain preserved.

Installer preparation returns leased, verified bytes to the lifecycle coordinator.
The coordinator persists the expected version before launching the installer and
latches admission closed during handoff. The next launch reconciles that marker
against the installed version; an interrupted handoff remains explicitly retryable.
Installer cleanup requires matching ownership records. Ambiguous legacy cache
entries are preserved while fresh bytes use a separate owned preparation directory.

The first Phase 3 regression compile exposed unstable Windows metadata identity
methods on the pinned Rust toolchain. Stable Win32 handle identity checks replace
those calls. The complete regression suite passed 219 tests, zero failures
(15.39 s test execution). Clippy with all targets/features and warnings denied,
and the whole-crate formatting check, passed. The one Clippy correction changed
only a boolean test assertion to its idiomatic equivalent.

## Phase 4 baseline

Captured before optimization with `scripts/run-backend-performance.ps1 -Label baseline
-Profile debug`. All three queue workloads and the runtime hashing workload passed.
Raw JSON: `target/performance/baseline-20260908T061619Z-99a1c1d6914d413e9827d7d508461ac3/summary.json`.

Environment: Windows build 26200.9278 (25H2), Intel i7-11700K, 16 logical processors,
NTFS repository volume. These measurements use the unoptimized development test
profile. Release builds require the maintainer updater key ID/public key, absent in
this environment; the after comparison must use the same profile.

| Queue items | Snapshot p50/p95/p99 ms | Durable command p50/p95/p99 ms | Journal save p50/p95/p99 ms | Snapshot during blocked I/O ms |
| ---: | --- | --- | --- | ---: |
| 1 | 0.001 / 0.001 / 0.002 | 4.734 / 5.441 / 5.802 | 4.503 / 5.303 / 5.860 | 0.012 |
| 100 | 0.104 / 0.169 / 0.205 | 9.657 / 10.786 / 11.653 | 9.060 / 9.804 / 10.522 | 0.129 |
| 1,000 | 1.102 / 1.435 / 1.732 | 46.671 / 48.862 / 50.157 | 43.763 / 46.033 / 46.869 | 1.178 |

Each workload used 10,000 snapshot samples, 1,000 wire snapshot samples, 200 durable
command samples, and 200 journal saves after warmup. At 1,000 items, wire snapshot
p95 was 15.007 ms, private memory grew from 4.54 MB after load to 12.76 MB after
sampling and enqueue, and one enqueue produced a contiguous 2,000-delta burst
(1.09 MB serialized). This measures produced event volume; the baseline has no
bounded authoritative outbox.

The authenticated tiny runtime fixture recorded 409 manifest hashes and 412 executable
hashes (821 total) across one health discovery, eight health resolutions, and 100
operations resolving four tools each. This is a count comparison, not a throughput
claim for the full-size shipped binaries. All fixtures use isolated paths.

## Phase 4 implementation

Durable state candidates share immutable records rather than duplicating all record
contents. Journal projection runs on the blocking writer and excludes transient
inspection results. Inspection allocation accounting includes vector and string
storage retained by live operations, snapshots, and pending events; it enforces a
16 MiB aggregate limit. Display text is compacted to 4 KiB, oversized actionable
fields are rejected, and an existing journal over the 32 MiB plaintext limit is
preserved with an explicit error.

One outbox owns authoritative event order. Its limits are 256 batches, 4,096 deltas,
and 16 MiB estimated queued bytes. Overflow or failed delivery requests a snapshot
at the latest known sequence. The existing frontend controller subscribes before
loading its first snapshot and reconciles these requests through its existing state
adapter. The event task drains after other backend producers finish at shutdown.

Managed and bundled tools now share a verified immutable runtime snapshot. Open
executable handles pin the verified file identity, and operation leases keep the
cache reader registered until release. Runtime replacement waits cancellably for
readers before entering its short publication transaction. A failed cache builder
can be retried; corrupt owned runtime state remains an explicit repair condition.
Optional Deno absence continues to return a warning rather than blocking every
download.

The downloader uses a bounded machine-readable `after_move` output record from
yt-dlp. A present malformed or ambiguous record cannot fall back to guessing.
When no record exists, exactly one valid media candidate is required. Process
supervision, inspection, publication, journal commits, scheduling, runtime cache,
runtime transactions, and state event delivery now have focused modules.

Integration review also strengthened the startup barrier for ordinary commands,
future-schema preservation, and terminal finalizer compensation. A finalizer that
fails after disk publication advances its recovery sequence beyond the possibly
persisted candidate, so the next durable command cannot silently skip that recovery
save. The regression flushes this state and checks two successive restarts.

The final Phase 4 regression gate passed 264 tests, zero failures, with the two
explicit benchmark tests ignored by the ordinary suite (24.52 s test execution).
Strict Clippy across all targets/features and whole-crate formatting passed. The
pinned frontend suite passed 64 tests across 10 files; type checking reported zero
errors/warnings, and lint and whole-project Prettier checks passed.

Process regressions use actual hidden Windows child processes and explicitly
inherited handles. They cover cancellation while a progress callback is blocked,
the five-second post-exit drain deadline, and a stdout line split across parent
exit. Reader tasks own partial-line buffers and are joined or aborted and awaited.

## Phase 4 after measurements and review

Executed `scripts/run-backend-performance.ps1 -Label after -Profile debug`, followed
by `scripts/compare-backend-performance.ps1` against the recorded baseline.
All four benchmark workloads passed; the comparator passed all 39 semantic and
resource-bound gates. Raw after evidence is in
`target/performance/after-20260908T075732Z-e3b9c05c6e854b20b1549f639f653fd4/`;
the comparison and raw-file hashes are in
`target/performance/comparison-20260908T075851Z-ac243f46d82b4881925e1543983ec550/`.

| Queue items | Snapshot p50/p95/p99 ms | Durable command p50/p95/p99 ms | Journal save p50/p95/p99 ms | Snapshot during blocked I/O ms |
| ---: | --- | --- | --- | ---: |
| 1 | 0.001 / 0.002 / 0.003 | 4.719 / 5.624 / 6.322 | 4.475 / 5.354 / 5.965 | 0.009 |
| 100 | 0.100 / 0.169 / 0.212 | 9.548 / 10.681 / 11.620 | 9.014 / 10.473 / 11.882 | 0.184 |
| 1,000 | 0.996 / 1.307 / 1.486 | 45.763 / 48.485 / 52.224 | 43.674 / 46.426 / 48.585 | 1.415 |

Runtime hash calls fell from 821 to 5 (one manifest and four tools), while all 408
lease resolutions succeeded. Unchanged operations added zero executable hashes.
At 1,000 queue items, snapshot p95 improved 8.9% and wire snapshot p95 improved
2.3%. Journal p50 changed little. These development-profile measurements establish
lease reuse and responsiveness, not release-build download throughput.

The comparator intentionally flags every timing increase for review: 22 rows rose,
including single enqueue samples repeated across percentile columns. The largest
repeated-sample journal increase was the 100-item p99 (10.522 to 11.882 ms).
The 1,000-item durable-command p99 rose from 50.157 to 52.224 ms. No uniform disk
latency improvement is claimed. The blocked-I/O checks still observed the previous
committed snapshot while the real journal writer was held; their absolute snapshot
latencies remained at or below 1.415 ms.

The new outbox retained at most 207 batches, 2,206 deltas, and 876,635 estimated
bytes in these controlled workloads, with zero overflow resynchronizations and
contiguous produced sequences. At 1,000 items, sampled private memory ended at
13,373,440 bytes versus 12,759,040 before (614,400 bytes higher); working set ended
753,664 bytes lower. The baseline did not retain the new authoritative outbox, so
this is a recorded tradeoff rather than evidence of a memory reduction. Long-run
memory and backlog stability remain part of Phase 5.

## Phase 5 executed checks

The pinned production frontend build passed. The production-bundle check passed:
no WebDriver gate or mock-registry tokens were present. These checks do not launch
the native application or qualify an installer.

`cargo build --manifest-path nuclear-app/src-tauri/Cargo.toml --locked --offline`
also passed for the normal development binary and was rerun successfully after
the final integration changes. The application was not launched against the
developer's user profile.

Integration review identified three additional failure intersections before the
soak executable was frozen: final event delivery failure during shutdown,
inspection admission unwinding after its journal commit, and Cancel All unwinding
after entering its drain generation. Focused recovery changes and regressions are
integrated and passed the final backend gate. The cancellation
core accepts a progress publisher; its IPC adapter owns the Tauri event handle.
This keeps backend tests independent of GUI initialization.

The final integrated Rust gate passed 270 tests, zero failures, with three explicit
benchmark/soak tests ignored by the ordinary suite. Both the controlled
`scripts/test-backend.ps1` run and the parallel all-features library run passed.
Strict Clippy across all targets/features, normal development compilation, and
whole-crate formatting passed. Parallel testing exposed interference in the
test-only runtime hash counters; per-fixture root accounting preserves the exact
hash assertions without sharing resets between tests. A typed options structure
replaced the oversized argument list in the inherited-pipe test helper.

The expanded native acceptance source covers exact MP4 fixture bytes, retrying a
cancelled queue item, collision suffixes, two-entry generic playlists, Cancel All
and admission reopening, renderer reload, and a forced process interruption that
must restore an interrupted row with explicit retry. The loopback HTML playlist
was checked with the pinned yt-dlp 2026.07.04 binary: it returned a playlist with
exactly two HTML5 entries. This was a fixture validation, not a native app run.

Structured manual evidence now binds the clean Windows 11 installer and portable
cases, YouTube/X fixtures, cookie case, and signed app/runtime update cases to the
candidate inventory digest, artifact hashes, operators, and UTC timestamps. The
writer refuses unbound input; the verifier rejects Server environments, invalid
versions, stale/future case times, failed/duplicate cases, and mismatched identities.
The publish workflow verifies this evidence before its draft mutation step. The
workflow was edited but not executed.

Executed `pwsh -NoProfile -File scripts/test-e2e-contracts.ps1`: passed. This includes
19 publication contract cases, automated evidence verification, manual writer and
verifier success, and rejection fixtures. Final pinned frontend checks again passed
64/64 tests, types (zero errors/warnings), lint, and whole-project formatting after
the native acceptance additions.

`scripts/test-packaging.ps1` passed its packaging parser and synthetic signed-fixture
contracts using the isolated pinned Node/npm toolchain. The first attempt stopped
because the local npm launcher resolved incorrectly; an explicit launcher under
the ignored toolchain directory now selects Node 22.23.1 and npm 10.9.9. The passing
run used disposable fixture keys, not maintainer keys or a real release candidate.

The candidate runner now accepts configured YouTube/X HTTPS fixture URLs paired
with opaque fixture IDs through protected workflow configuration. Their installed
UI smoke cases run only when configured. Evidence contains IDs and canonical
missing case names, with a separate extractor status; overall automated
qualification remains incomplete until the seven manual requirements are met.
The final acceptance contract run passed again after this configuration support,
including zero/one/both configured-site cases and rejection of false overall
qualification claims.

Final evidence review added guaranteed redaction of the configured site fixture
URLs before diagnostic log tails and artifact retention. Output remains in owned
staging until all retained files pass a second byte scan. Invalid log encoding or
redaction failure leaves no uploadable evidence directory. The acceptance and
publication contracts passed again, including privacy success, failure-finalizer,
and invalid-UTF8 cases. No protected workflow was executed.

The exact-source two-minute soak passed at
`target/soak/after-20260908T092713Z-8873bcb968a6477084b36436e32cf253/`.
It ran 120.311 seconds and 120 operations, with zero descendants at every sample,
successful journal reopening, ten runtime hashes, and 98 successful lease
resolutions. Handles rose from 81 at cold startup to 113 after the first twelve
cycles and ended at 115. Every cycle is retained; the original growth limits apply
after this explicit warmup. This smoke check does not replace the two-hour run.

The harness setup failures are retained separately in earlier ignored soak run
directories. They identified accidental GUI linkage in a cancellation test,
Windows console-host allocation, and a cold-start handle baseline that preceded
process supervision initialization. The backend test core is now GUI-free; the
ignored soak detaches only its own console, and descendant checks verify live
creation-time ancestry. The strict zero-descendant requirement remains in force.

The pinned `cargo-deny` 0.20.2 policy check passed against a freshly fetched RustSec
database at commit `8a1eb4f933fb5821add5b4e98601ebd90b8b3538`. Advisories, source,
license, and dependency-ban checks reported zero errors. The existing policy
retains its advisory exceptions; 21 duplicate-version warnings and three unused
license-allowance warnings remain. Only the database cache path was relocated in
the effective test configuration. Project dependency versions and policy rules
were unchanged. The pinned audit tool was built offline into the ignored workspace
tool directory; no global tool was upgraded. Evidence, policy hashes, and logs:
`nuclear-app/src-tauri/target/qualification-audit/`.

The final isolated headless renderer suite passed both tests. Its 1,500-event
workload ran for 59.814 seconds, reporting reducer p95 0 ms, progress-handler p95
0.1 ms, state-delta p95 0.2 ms, frame p95 7.1 ms, and input-to-paint 13 ms. These
are renderer simulation measurements with deterministic IPC mocks. The fixtures
now include the persistence-health and terminal-publication contract fields.
Browser tests require one explicitly supplied, caller-owned profile directory;
CI creates and cleans that directory with path and ownership checks. No native
application or personal browser profile was launched. The pinned production npm
audit completed with zero vulnerabilities.
The retained frontend report is
`nuclear-app/target/qualification-reports/frontend-qualification-20260908T0937Z.json`.

The required 120-minute run passed from 2026-09-08 09:35:39 UTC to
11:35:41 UTC, with an actual workload duration of 7,202.276 seconds. Evidence is in
`target/soak/after-20260908T093535Z-afb163eef5e8413c8183957ee05d4693/`.
The frozen executable SHA-256 remained
`6fccd3788041bdc7179be51b7672f7bb417f74f09c184cd1b2e6e32d56638e3a`.
All 95 manifest entries still matched after the run. `final-verification.json`
records that check, raw evidence hashes, and metrics from all 1,440 samples.

Across 1,438 cycles, the workload finalized 7,190 operations: 4,314 completed,
1,438 intentionally cancelled, and 1,438 intentionally failed. It supervised
1,438 real Windows child processes in each of the success, failure, and cancellation
cases; exercised four inherited-pipe drains; and performed 1,438 publications with
collision preservation. It also completed 71 abandoned-stage cleanups while
preserving unowned entries, 119 lifecycle drains, and 119 runtime-cache mutations.
All 5,871 runtime resolutions succeeded with exactly 595 hashes, five per mutation.
Four forced event-overflow rounds delivered resnapshot requests; 64,510 deltas were
observed in total.

Every quiescent sample retained exactly 100 queue items, no more than 200 terminal
operations, and zero pending registrations, active jobs, descendants, output
residue, or undelivered outbox batches. From the explicit twelve-cycle warmup,
handles rose from 112 to 114 and never exceeded 114. Private memory started at
4,530,176 bytes, peaked at 8,024,064 bytes, and ended at 6,422,528 bytes. Working set
peaked at 21,508,096 bytes. Journal and diagnostics sizes peaked at 152,694 and
349,434 bytes respectively. The final encrypted journal reopened successfully and
the owned workload directory was removed, leaving the runner's fixture directory
empty. Standard error was empty and the test exited zero.

This is an isolated Windows debug backend workload using fixture runtimes and media,
real process supervision, DPAPI persistence, and lifecycle/publication helpers.
Quiescent samples establish cleanup between cycles; they are not continuous process
telemetry. The run does not establish native packaged-app behavior, full-size media
throughput, real installer handoff, or leak freedom beyond the observed workload.

## Qualification limits

Unit and contract tests do not establish clean-machine, installer, external-site, cookie,
or signed-release acceptance. Missing credentials, controlled fixtures, or Windows 11
desktop test infrastructure must remain explicit qualification blockers. Exact candidate
hashes and the test environment must be recorded before calling a release qualified.

This file is updated with executed commands, results, measurements, and remaining gaps
as implementation proceeds. Pending entries are not passing results.
