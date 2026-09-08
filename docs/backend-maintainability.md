# Backend maintainability refactor

Implementation baseline: `34d0769` (backend reliability overhaul), Windows 11 x64.
The application remains one Rust/Tauri crate. All supported workflows and public
contracts are preserved. Confirmed defects receive separate regression-backed
fixes; mechanical moves do not also change behavior.

## Stage gates

| Stage | Status | Evidence |
| --- | --- | --- |
| 1. Inventory and baseline | Passed | Fresh Rust 270/270, frontend 64/64, strict Clippy, formatting and binding-diff gates; 1,112 inventory entries across 27 files; inventory lexer/reconciliation tests 4/4 |
| 2. Boundaries and supporting code | Passed | 283 Rust tests, strict Clippy, formatting and unchanged bindings after downloader/runtime/installer leaves and private test extraction; source-tool fixtures 8/8 each |
| 3. State and durable commits | Passed | 283 Rust tests, strict Clippy, formatting and unchanged bindings; initial performance run plus three repeats, all 45 hard gates passed per comparison |
| 4. Complete application workflows | Passed | 287 Rust tests, strict Clippy, formatting/bindings, architecture and executable acceptance-contract fixtures |
| 5. Runtime/updater/lifecycle internals | Pending | Verification, ownership, transaction and lifecycle components |
| 6. Integrated qualification | Pending | Independent review, full local gates, performance comparison, new two-hour soak |

Every production method and ownership-bearing asynchronous closure is recorded in
`backend-method-review.json`. Reviewer sidecars record actual inspection outcomes;
discovery alone never marks an entry reviewed. Source digests invalidate stale
reviews. Tests and test support have separate classifications. The feature matrix
in `backend-feature-preservation.md` links observable behavior to retained checks.
The initial inventory contains 671 production entries (including declarations,
trait implementations and async blocks), 300 test entries, and 141 test-support
entries. These are review units, not a claim of 671 independent business methods.
Per-method review continues through each owning subsystem's refactor and final
source reconciliation.
The structural inventory was cross-checked against all 961 masked Rust `fn`
tokens: 960 declarations and one explicitly excluded function-pointer type.
A lexer regression for paired lifetime annotations added one previously missed
test helper. The inventory records reviewed source syntax, not macro expansion or
a proof of semantic correctness; its limitations are documented in the schema.

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

The intended navigation map is: `lib.rs` for the public command registry and
Tauri wiring; `desktop.rs` for native events, dialogs and installer launch;
`bootstrap.rs` for recovery and shutdown; `services/` for complete operations;
`state/` for data, transitions and durable commits; `lifecycle/` for admission,
task tracking and protected publication; `downloader/` for validation, arguments,
processes and file publication; and `runtime/`, `updater/` and
`runtime_transaction/` for authenticated runtime and installer management.
This map is a target until the corresponding stage gate passes.

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
gate will be enabled only after the final source identities are reconciled.
