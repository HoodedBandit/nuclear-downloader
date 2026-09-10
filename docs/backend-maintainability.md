# Backend maintainability refactor

Implementation baseline: `34d0769` (backend reliability overhaul), Windows 11 x64.
The application remains one Rust/Tauri crate. Existing features and public
contracts are retained in source and covered by the automated checks below;
external native workflow qualification remains pending. Confirmed defects receive separate regression-backed
fixes; mechanical moves do not also change behavior.

## Stage gates

| Stage | Status | Evidence |
| --- | --- | --- |
| 1. Inventory and baseline | Passed | Fresh Rust 270/270, frontend 64/64, strict Clippy, formatting and binding-diff gates; 1,112 inventory entries across 27 files; inventory lexer/reconciliation tests 4/4 |
| 2. Boundaries and supporting code | Passed | 283 Rust tests, strict Clippy, formatting and unchanged bindings after downloader/runtime/installer leaves and private test extraction; source-tool fixtures 8/8 each |
| 3. State and durable commits | Passed | 283 Rust tests, strict Clippy, formatting and unchanged bindings; initial performance run plus three repeats, all 45 hard gates passed per comparison |
| 4. Complete application workflows | Passed | 287 Rust tests, strict Clippy, formatting/bindings, architecture and executable acceptance-contract fixtures |
| 5. Runtime/updater/lifecycle internals | Passed | 290 Rust tests, strict Clippy, formatting, unchanged bindings, architecture checks and independent runtime/updater extraction review |
| 6. Integrated qualification | Current short gates passed; external gates blocked | Current Rust library suite 332 passed with three opt-in harnesses ignored, strict Clippy passed, and 1,246 source reviews reconcile; matched performance and the later two-hour soaks remain evidence for `fd58050`, while native release acceptance remains blocked |

Every production method and ownership-bearing asynchronous closure is recorded in
`backend-method-review.json`. Reviewer sidecars record actual inspection outcomes;
discovery alone never marks an entry reviewed. Source digests invalidate stale
reviews. Tests and test support have separate classifications. The feature matrix
in `backend-feature-preservation.md` links observable behavior to retained checks.
The initial inventory contains 671 production entries (including declarations,
trait implementations and async blocks), 300 test entries, and 141 test-support
entries. These are review units, not a claim of 671 independent business methods.
The current reconciled inventory contains 1,246 reviewed entries across 97 files:
725 production, 354 test and 167 test-support units. CI now requires an accepted
review matching every current source identity.
The structural inventory was cross-checked against all 961 masked Rust `fn`
tokens: 960 declarations and one explicitly excluded function-pointer type.
A lexer regression for paired lifetime annotations added one previously missed
test helper. The inventory records reviewed source syntax, not macro expansion or
a proof of semantic correctness; its limitations are documented in the schema.

The latest follow-up replaces URL-only playlist identity with a validated media
selector containing the entry id, extractor key, and one-based playlist ordinal.
Inspection discovers a bounded playlist in one pass, selected inspection verifies
the returned identity, queue persistence retains it for retry, and publication
requires the matching machine output record. Captured inspection JSON uses its
8 MiB aggregate cap while streamed progress retains its 64 KiB line cap. The
source-matched review and 332-test library run are current for these bytes; the
older matched performance and two-hour soak receipts are not transferred to this
follow-up. See [the inspection and playlist correction](inspection-playlist-fix.md).

The recommended yt-dlp baseline is now `2026.08.19`. The dedicated regression
binds directly to that constant and passed in the same 332-test library run;
strict Clippy also passed. The retained exact-URL comparison showed the older
binary receiving HTTP 403 and the official `2026.08.19` binary succeeding with
the same selector. The user's subsequent successful YouTube download is useful
confirmation of that case, not broad extractor or release qualification.

## Ownership and dependency rules

- Tauri commands adapt arguments and results. The application boundary owns
  AppHandle, event delivery, native dialogs, and exit requests.
- Workflow services own complete admission, execution and finalization sequences.
  Backend engines receive concrete dependencies and narrow callbacks, without
  reaching back into the crate root or looking up application-managed state.
- StateStore owns authoritative state. Transition/validation helpers and journal
  projection do not perform persistence I/O; some helpers read the clock or create
  error IDs. One commit executor retains its mutation guard through preparation,
  durable saving, installation, and ordered outbox publication.
- The lifecycle coordinator owns admission, tracked tasks, cancellation, capacity,
  and protected publication. State and lifecycle remain independent peers; services
  combine them. Extraction must preserve guard lifetimes and lock order.
- Runtime and installer ownership, authentication, open-file identity, quarantine,
  and durable promotion ordering remain explicit safeguards. Shared helpers are
  introduced only where both semantics and trust assumptions match.
- Public command names/payloads, serde and generated TypeScript contracts, journal
  schema 1, error/event contracts, concurrency, timeouts, retention and resource
  budgets remain unchanged. No actor rewrite or generalized framework is added.

The implemented navigation map is: `lib.rs` for the public command registry and
Tauri wiring; `desktop.rs` for native events, dialogs and installer launch;
`bootstrap.rs` for recovery and shutdown; `services/` for complete operations;
`state/` for data, transitions and durable commits; `lifecycle/` for admission,
task tracking and protected publication; `downloader/` for validation, arguments,
processes and file publication; and `runtime/`, `updater/` and
`runtime_transaction/` for authenticated runtime and installer management.
These module boundaries passed their corresponding stage gates.

## Validation policy

Fresh starting evidence on 2026-09-08: backend tests passed 270 cases with zero
failures and three explicit ignores (24.87 seconds); frontend passed 64 cases;
strict all-target/all-feature Clippy, formatting, and committed-binding diff checks
passed. The clean performance reference is
`target/performance/after-20260908T204145Z-1a07b77b1e4b4bedab0cf2e85ffd7973/`.
It records five runtime hashes, 408 successful leases, and bounded outboxes.
The existing harness's `baseline` label deliberately selects pre-overhaul
emulation, so maintainability before/after comparisons both use its `after` mode.
An earlier current-code run is retained at
`target/performance/after-20260908T204002Z-9f044998c0724623bfdb14a7543169bd/`;
the reference above was repeated without another build competing for resources.

Each mechanical extraction receives its focused tests and compilation check. Full
Rust, strict Clippy and formatting gates close each implementation stage. Binding,
frontend, production-bundle, packaging and acceptance contracts close integration.
Source-text guards are moved or replaced together with their actual owning code;
none are deleted solely because a source file moved.

Performance uses the existing 1/100/1,000 queue workloads and debug profile, with
unchanged fixtures, sample counts and resource limits. All existing hard gates and
runtime hash/lease counts must hold. An apparent latency increase exceeding both
10% and 0.1 ms, or memory increase exceeding both 10% and 2 MiB, requires three
matched runs; reproduced regressions block completion.

Final validation requires a new source/executable-hash-bound two-hour isolated
soak. Previous soak results describe the previous implementation. Test executables
must pass discovery before sustained workload; a loader error stops repeated
launches until diagnosed. No personal application data or browser profile is used.

## Qualification boundaries

Clean Windows 11 installer/portable acceptance, maintainer-controlled extractor
fixtures, cookie acceptance, and authentic signed update acceptance still require
the external environment and credentials described in `backend-qualification.md`.
These checks are not replaced by unit tests, fixture signing, or renderer mocks.
No publishing or pushing is included in this refactor.

## Confirmed defects and isolated commits

- `27cff85`: bounded staging ownership marker reads to 4 KiB. The new oversized
  marker regression failed before the fix, then passed; all 15 publication tests
  and formatting passed. Rejected input is preserved. Verification remains tied
  to the opened non-reparse file identity, with Windows sharing excluding writes
  and deletion during verification.
- `0efa621`: incomplete staging initialization now rolls back through retained
  Windows directory/file handles. Tests cover failure before marker creation,
  a partial marker write, retry, and preservation of unexpected content. The
  ordinary create-directory/open-handle gap is documented; no claim is made of
  atomic directory creation against another process replacing that path.
- `10ce3d1`: quarantined runtime transaction records use a 64 KiB bounded read.
  The regression failed against an oversized valid record before the fix and
  verifies that rejected evidence stays intact.
- `7fa1cc1`: failed diagnostics exports remove the exact opened partial file,
  allowing same-path retry. Preexisting destinations remain unchanged.
- `4650519`: the machine-readable final-output record is read through the opened
  file with a hard byte budget. A regression first demonstrated that stale small
  metadata could allow a larger file to be read; malformed evidence is preserved.
- `077d049`: build-time and installed-runtime artifact checks use one canonical
  lowercase SHA-256 policy. The regression first demonstrated acceptance of an
  uppercase digest that runtime validation would reject.
- `f81ab9e`: build-time updater key validation uses the same strict wrapper,
  UTF-8 and Minisign parser as runtime verification. The malformed-key regression
  failed before the correction; valid current keys, malformed optional rotation
  keys, and empty debug configuration are covered. The build uses the already
  locked Minisign dependency version.

After these fixes and the lifecycle/cancellation/public-boundary test moves in
`701d440`, the full backend suite passed 277 tests with three explicit ignores
(24.14 seconds). Strict Clippy, formatting and binding-diff gates passed. These
are intermediate receipts; the final Stage 2 receipt is recorded below.

After those additional fixes and the state/persistence test moves in `eba68a3`,
the backend suite passed 279 tests with three explicit ignores (24.10 seconds).
Strict Clippy, formatting and binding-diff checks passed. The Windows file helper
consolidation in `8804913` then passed 29 focused publication, diagnostics and
runtime-transaction tests. It shares SDK-typed identity/deletion primitives while
each caller retains its ownership and open-handle policy.

The downloader leaf extraction in `638fa9f` and updater-key correction passed the
full backend suite with 283 tests and three explicit ignores (24.09 seconds),
strict Clippy (15.98 seconds), formatting and binding-diff checks. The architecture
and inventory tooling in `c3306f3` each passed eight fixture tests. The architecture
gate intentionally remains incomplete until the Stage 4 boundary is implemented;
its fixtures distinguish production-capable cfg expressions from test-only code.

The final Stage 2 gate after extracting runtime verification/cache, installer
ownership/cache and large downloader/runtime/updater test modules passed 283 Rust
tests with three explicit ignores (24.11 seconds), strict Clippy (9.43 seconds),
formatting, binding-diff and whitespace checks. Compilation caught ordinary
import/visibility corrections during the moves; no runtime regression remained.

Review also rejected two suspected lifecycle defects after checking their actual
contracts: Tokio 1.50 captures broadcast notification generations when a waiter
is created, and protected publication intentionally outlives ordinary shutdown's
15-second grace. Neither requires a behavior change.

Stage 3 moved state data/projection (`0d7f3ae`), transitions/validation (`42d955e`),
and durable commits/finalization (`e6a9bf3`) into focused children of StateStore.
Each slice passed all 283 Rust tests, strict Clippy, formatting and unchanged
bindings. The final slice passed in 24.01 seconds; all 13 moved function bodies
were token-equivalent to their previous implementations. The obsolete sibling
journal-commit module was absorbed and StateData visibility narrowed.

The initial Stage 3 performance comparison exceeded latency review thresholds,
so three further identical workloads were run without competing builds. All four
comparisons passed all 45 hard gates, retained five runtime hashes and 408 leases,
and kept snapshot reads independent of blocked journal I/O. The timing threshold
crossings did not recur on the same metric across the three repeats. For example,
the 1,000-item durable-command p50 was 48.721 ms initially, then 41.991, 41.931 and
46.391 ms, against the 41.926 ms reference. The corresponding direct-journal p50
was 47.636, then 40.096, 39.972 and 40.396 ms, against 39.700 ms. Snapshot p99 was
1.388, 1.271, 1.441 and 1.347 ms, against 1.909 ms. This closes the intermediate
review as a threshold crossing not consistently reproduced, rather than claiming
that disk timing is constant. Final integrated measurements remain required.

All raw runs are retained under `target/performance/`:

- `after-20260908T224734Z-2b581e3b55804bf2a4a65631b78a4e28`
- `after-20260908T224908Z-42fd2b13c18447a9858e2583ee6522ae`
- `after-20260908T225025Z-8f94a4c77f0949289c41d0c5a47180d9`
- `after-20260908T225129Z-bebfea8e275142f5b8b9080b45c6f03f`

The initial comparison is in
`comparison-20260908T224906Z-51026848b4a04ca1a8b8bca0bd5b610e`;
the repeats are in `maintainability-stage3-repeat-1`, `-2` and `-3`.

Stage 4 moved complete operations into `services/`, recovery/shutdown into
`bootstrap.rs`, and event delivery/dialogs/installer launch into `desktop.rs`.
Downloader and updater engines now receive typed callbacks and an explicit app
version; neither looks up a Tauri-managed StateStore. The public registry still
contains exactly 18 commands, with unchanged generated bindings and five event
names. Cancellation retains one shared ten-second deadline and the production
worker pool remains five.

The four new service regressions exercise state-before-progress publication,
exhausted terminal-save retries, successful installer handoff ordering/leases,
and failed-launch rollback/reopened admission. A new fixture initially opened
admission without completing startup, which correctly left the service waiting;
that test process was stopped and its evidence retained. The fixture now drives
the real tracked startup sequence and bounds admission waits. The integrated
suite then passed 287 tests with three explicit ignores in 25.40 seconds, and
strict Clippy passed in 8.34 seconds after naming two complex callback types.
Formatting, binding-diff, architecture and whitespace checks passed. Both source
tool fixture suites passed 8/8. Executable publish/acceptance-contract fixtures
passed using an isolated temporary directory and a closed fake CLI; no release,
installer, native application, or external service was launched by those checks.

The architecture policy is now active in CI and release-candidate checks. The
WebDriver exclusion guard scans every crate-owned Rust source plus build inputs,
so moving code cannot silently remove that coverage. The final per-method review
gate was enabled after the final source identities were reconciled, as recorded below.

A separate finalizer regression then demonstrated that aborting the awaiting
caller could abandon panic compensation in an otherwise detached finalizer. The
new deterministic test failed at the missing degraded-state assertion before the
fix. Panic recovery now belongs to the detached task itself, which retains the
mutation guard through compensation, preserves the intended outcome and output
path, and advances beyond any possibly persisted candidate revision. The full
suite passed 288 tests with three explicit ignores in 25.56 seconds, including
the existing pre-save and post-save panic recovery tests. This is a narrow owned
finalizer contract correction; the application's tracked workflows already retain
their task owners during ordinary IPC cancellation.

The frontend compatibility rerun passed formatting, strict lint, Svelte/TypeScript
checking (zero errors or warnings), all 64 tests across ten files, the production
build, and the production-bundle test-hook exclusion check. No frontend product
source changed.

Renderer acceptance was attempted with matching installed Chrome/ChromeDriver
152.0.7977.76 and an explicit, newly created profile under the repository's ignored
test directory. ChromeDriver's version preflight passed, but Chrome failed before
creating a WebDriver session: its GPU subprocesses exited with `0xC0000022`
(access denied), followed by `GPU process isn't usable`. No renderer test ran.
Further browser launches were stopped; this remains an environment-blocked gate,
not a passing workflow test or evidence of an application failure. Logs and the
isolated profile are preserved under
`target/renderer-maintainability/6b1029656d484c7a8077077c38bbbb17/`.

Stage 5 split lifecycle admission/capacity/publication/task tracking, runtime
manifest/release/archive/promotion, runtime transaction journal/locking/filesystem,
and updater release/network/installer internals into focused modules. Two direct
publisher regressions cover successful transaction/cache publication and
cancellation while a cache lease prevents mutation. The integrated suite passed
290 tests with zero failures and three explicit ignores in 25.91 seconds. Strict
Clippy passed in 9.24 seconds; formatting, binding-diff, architecture and whitespace
checks passed. Independent reviews found no runtime/updater behavioral blocker.
Two nonblocking extraction cleanup observations are retained in the resume note.

The separate review-tool fix in `ebc23f1` requires every generated source identity
field before accepting a method sidecar. Its missing-identity regression failed
before the fix and passed afterward; inventory fixtures passed 9/9 and architecture
fixtures 8/8. Production npm audit reported zero vulnerabilities. Cargo deny
passed advisories, bans, licenses and sources, with policy-allowed warnings.

At the user's pre-reboot checkpoint, source and draft sidecars were saved, but
the sidecars had not yet been reconciled or finally accepted and no new two-hour
soak had started. `backend-resume-checkpoint.md` preserves that historical stop
state; the user subsequently authorized continuation.

Work resumed after the user rebooted and explicitly requested continuation. The
two recorded extraction cleanup items were addressed in `b3d4602`: release intake
and persisted runtime authentication now use the same descriptor/signature limits,
and the transaction filesystem helper imports OpenOptions only on Windows. The
fresh integrated suite passed 290 tests with three explicit ignores in 34.30
seconds. Strict Clippy passed in 9.66 seconds; formatting, unchanged bindings,
architecture and whitespace gates passed. Packaging parser and signed fixture
tests also passed with an isolated temporary directory.

The reboot updated Windows from build 26200.9278 to 26200.9445. The comparator
correctly rejected the first new benchmark against the earlier host. That run is
retained as unpaired evidence at
`target/performance/after-20260909T002958Z-c169a1a785c048f5822175613a2fef67/`.
The first matching-host comparisons used newly compiled, frozen pre-refactor
(`34d0769`) and intermediate (`b3d4602`) test executables on the updated host. Their identical
state benchmark source, locked sidecars, exact executable hashes, compilation
records and alternating run order are retained under
`target/performance-paired/post-reboot-34d0769-b3d4602/`.

All four matching-host pairs passed all 45 hard gates. Both versions retained five
runtime hashes and 408 successful leases. Timing review counts were 6, 3, 3 and 3;
no single row exceeded both its relative and absolute thresholds in all three
repeat pairs (2, 3 and 4). This supports closing the required repeat review without
claiming an overall speedup or constant disk latency. At 1,000 items, the current
snapshot p99 ranged from 1.603 to 1.936 ms, and snapshots during deliberately
blocked journal writes took 0.994 to 1.239 ms while observing precommit state.
Current durable-command p50 ranged from 43.539 to 44.417 ms. Raw records for both
versions, all review-threshold crossings, executable hashes and build provenance
are preserved in the raw intermediate evidence. The current final measurements
are tracked in `backend-maintainability-performance.json` and described below.

The paired wrapper stopped after the first successful comparison because its
PowerShell exit-code variable had not been initialized. That wrapper was corrected
and resumed at pair 2; pair 1 data was retained. No test executable failed or was
retried during these matching-host runs.

The new 120-minute isolated soak started on 2026-09-09 at approximately 00:46 UTC
from source `b3d4602`, with all 154 build/source manifest entries identical before
and after compilation. Exact test discovery passed before workload execution.
Its frozen executable SHA-256 is
`28f35f1f4b02e54424fd51c3e239935a02cae8ba532a6b4bfd223fe4da63f459`.
Evidence is retained under
`target/soak/after-20260909T004626Z-347020e391234944b2a2a12009d93407/`.
This run was intentionally stopped after approximately six minutes when the
method review confirmed another journal bound defect. Its exit code -1 reflects
termination of the exact owned process tree; `intentional-stop.json` records the
reason and process identity. These partial samples are not two-hour qualification.

`807ce00` fixes the active runtime transaction journal's metadata/read race. A
metadata size check followed by an unbounded read allowed another writer to grow
the journal beyond 64 KiB. The separate application mutation lock does not lock
that journal file. `load` now opens once and validates that handle; its reader
consumes at most 65,537 bytes and rejects overflow before parsing. The deterministic
`journal_growth_after_metadata_is_bounded_and_preserved` regression appends valid
JSON whitespace after a small metadata observation while holding the mutation
lock, then checks the read cap, error, unchanged file and regular reload rejection.
It failed before the byte cap (0 passed, 1 failed, 0.04 seconds) because the grown
journal was accepted. The fixed integrated suite passed 291 tests with three
explicit ignores in 40.09 seconds. Clippy passed in 8.70 seconds; a formatting-only
wrap was applied, then formatting, unchanged bindings, architecture and whitespace
checks passed. Independent source review accepted the fix and regression.

The inventory after that correction contained 1,191 entries across 86 files:
717 production, 314 test and 160 test-support review units. All four intermediate
matching-host benchmark pairs against `807ce00` passed their 45 hard gates; timing
review counts were 17, 7, 16 and 4. No exact row exceeded both thresholds in all
three repeat pairs. These intermediate records remain under
`target/performance-paired/post-reboot-34d0769-807ce00/`.

The same metadata/read pattern was then found in encrypted journal intake,
runtime pointers/manifests/authentication data, and installer ownership records.
`cb616a1` adds shared synchronous and asynchronous bounded readers and applies
them to those paths and exact runtime ownership markers. The shared readers
consume at most the limit plus one probe byte, distinguish overflow from I/O
errors, and accept exact-limit content. Callers retain their path, ownership,
authentication, error and preservation rules. Existing bounded publication and
runtime transaction readers remain intact. Independent review found no analogous
unbounded metadata-then-whole-file production read remaining.

The integrated suite passed 303 tests with zero failures and three explicit
ignores in 38.91 seconds. This includes twelve new tests covering exact limits,
endless/oversized readers, async parity, journal error mapping, runtime metadata,
and preservation of ambiguous markers and unrelated installer files. Strict
Clippy passed in 14.92 seconds; formatting, unchanged generated bindings,
architecture and whitespace checks passed. That intermediate source inventory contained
1,214 entries across 88 files: 726 production, 326 test and 162 test-support review
units. Neither interrupted nor earlier-revision runs qualified that correction;
the final measurements and new full-duration soak use the later corrected source.

The installer review raised a possible hard-link deletion conflict with its
verified read lease. Direct Windows testing disproved that premise: an isolated
Rust test using the production share/reparse flags could delete the partial hard
link while the final link remained protected against writes and last-link
deletion. Discovery and the exact test both passed; no publication reorder was
made. The standalone evidence and source are retained under
`target/regression-evidence/installer-hardlink-lease-47d819603c9c467fb83476fb3ce77c23/`.

A separate cleanup failure was reproduced: when removal of a partial's last link
was blocked, `cleanup_current_artifact` deleted its owner record anyway. The new
`failed_partial_removal_retains_ownership_for_startup_retry` test failed at the
missing-owner assertion (0 passed, 1 failed, 0.02 seconds). `d1f6571` retains the
record unless a post-cleanup metadata check confirms the artifact is absent.
Successful installer publication likewise retains the partial record when unlink
fails. That warning does not turn a verified published installer into a failed
download. Publication order, verified leases and cancellation boundaries are
unchanged. The regression releases the blocking lease and then verifies that the
ordinary owned-partial cleanup removes both file and record. RED source and
receipts are under `target/regression-evidence/installer-partial-owner-red/`.

Independent review accepted this narrow correction. The full suite passed 304
tests with zero failures and three explicit ignores in 37.62 seconds; strict
Clippy passed in 6.77 seconds. Formatting, unchanged generated bindings,
architecture and whitespace checks passed. The source inventory is now 1,215
entries across 88 files: 726 production, 327 test and 162 test-support review units.

Final source-review reconciliation accepted all 1,215 unique entries, with no
unreviewed entries or unresolved findings. Reviews include concrete executed test
coverage where available and explicitly identify static-only evidence elsewhere.
The canonical ledger and accepted subsystem sidecars now carry every generated
identity key, including explicit `ownerCall: null` where no owning call applies.
This fixes a format inconsistency between generated canonical rows and the strict
sidecar contract; missing null keys did not previously authorize a different body.
Two new inventory fixtures failed before the format correction, then all eleven
inventory fixtures and eight architecture fixtures passed (19 tests, 1.075 seconds).

The actual canonical check reported `review ledger covers 1215 current entries`.
The actual architecture check also passed. CI and release-candidate workflows now
require that canonical check; changed source identities invalidate old reviews.
The updated PowerShell acceptance/publish-contract fixtures passed with an isolated
TEMP and closed fake CLI. Those fixtures neither published an artifact nor ran
native application acceptance. Receipts are retained under
`target/qualification-final/source-tools-b16c3e19bbca4e31ae7440692c6697cf/` and
`target/qualification-final/contracts-c5a8ee2e9b2241dc8fee5090c7b593cd/`.

Final matching-host performance uses source `d1f6571` against `34d0769` on Windows
11 25H2 build 26200.9445, with the same debug workloads and locked dependencies.
The four pairs alternate before/after execution order. All 34 discovery/workload
executions exited zero, and each comparison passed all 45 hard gates. Both versions
used five runtime hashes for 408 successful executable-lease resolutions. Timing
review counts were 1, 8, 5 and 10. Five latency rows crossed both review thresholds
in repeat pairs 3 and 4, but all five cleared pair 2; no row crossed both thresholds
in all three required repeats. The single enqueue observation per run is not an
independently measured tail percentile. These results close the specified repeat
review without claiming an overall speedup or constant disk latency.

Current-version ranges across the four matching pairs, in milliseconds:

| Queue items | Snapshot p99 | Serialized snapshot p99 | Durable command p50 | Journal save p50 | Snapshot while journal write is deliberately blocked |
| --- | --- | --- | --- | --- | --- |
| 1 | 0.004-0.007 | 0.068-0.099 | 4.873-5.093 | 4.604-4.696 | 0.013-0.024 |
| 100 | 0.300-0.523 | 2.483-3.187 | 9.266-10.376 | 8.755-9.470 | 0.123-0.288 |
| 1,000 | 2.229-2.708 | 19.888-20.531 | 51.623-53.553 | 47.758-49.266 | 1.031-1.395 |

Blocked-write snapshots observed precommit state and completed independently of
the held disk-write barrier. Raw p50/p95/p99 results, memory, event backlogs, review
rows, hashes and host/build provenance are in
[`backend-maintainability-performance.json`](backend-maintainability-performance.json)
and `target/performance-paired/post-reboot-34d0769-d1f6571/`. These are isolated
backend component measurements, not native packaged-application timing.

The final 120-minute soak started at approximately 2026-09-09 02:03 UTC from
`d1f6571`. All 156 source/build/runner manifest entries matched before and after
compilation. Its frozen executable SHA-256 is
`d10bb3be49f5688a3d7b3cc975377d9b1562e9c117cec3acd5fac31a39d8a647`, identical
to the final performance executable. Exact test discovery passed. The run exited
normally at 2026-09-09 04:03:29 UTC after 7,203.095 seconds, with one test passed
and zero failed. No compiled inputs or soak runner changed during the run.
Evidence root:
`target/soak/after-20260909T020321Z-e35c8e5cfdcd499daf3901b16481c134/`.

The completed run exercised 1,438 mixed cycles and 7,190 operations: 4,314 completed,
1,438 intentionally cancelled, and 1,438 intentionally failed. These controlled
operation failures are workload cases, not failed tests. It also exercised 1,438
publications and collision preservations, four inherited-pipe drains, 71 abandoned
stage cleanups with unowned-stage preservation, 119 lifecycle drains and runtime
replacements, four observed resyncs, and 64,510 state deltas. The runtime made 5,871
successful lease resolutions with 595 hashes, exactly five for each of the 119
coordinated replacements.

All 1,440 quiescent samples had zero pending operations, active jobs, queued outbox
batches/deltas/bytes, descendant processes and output residue. The 100-item queue
remained intact and retained operations never exceeded 200. The final journal was
reopened successfully. The recorded workload PID/start identity had exited, no
process remained at the frozen executable path, and the owned fixture directory
was empty. Stderr was empty.

| Resource | Maximum observed at quiescent samples | Maximum growth after warm-up | Allowed growth |
| --- | --- | --- | --- |
| Private memory | 8,036,352 bytes | 3,039,232 bytes | 128 MiB |
| Working set | 21,086,208 bytes | 3,612,672 bytes | 192 MiB |
| Process handles | 114 | 2 | 32 |

Journal size remained at or below 152,694 bytes and diagnostics at or below
349,434 bytes. These are sampled debug-process measurements, not instantaneous
peaks or native packaged-application memory claims.

Post-run verification passed all 20 checks, including the exact 156-entry source
set and hashes, successful compiler artifact/discovery/runner records, full wall
duration, every sample's resource bounds, workload/lease counters, monotonic event
sequences, and empty owned fixtures. Separate process-exit evidence confirms the
observed test process is gone. The tracked receipt
[`backend-maintainability-soak.json`](backend-maintainability-soak.json) binds the
raw records and verifier by SHA-256. Local implementation and automated/component
qualification are complete. Independent review recomputed the sample bounds,
workload equations and receipt hashes and accepted the final record. The external
native, extractor, cookie and authentic
signed-update gates above remain blocked; no push or publishing occurred.
