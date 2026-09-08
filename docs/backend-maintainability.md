# Backend maintainability refactor

Implementation baseline: `34d0769` (backend reliability overhaul), Windows 11 x64.
The application remains one Rust/Tauri crate. All supported workflows and public
contracts are preserved. Confirmed defects receive separate regression-backed
fixes; mechanical moves do not also change behavior.

## Stage gates

| Stage | Status | Evidence |
| --- | --- | --- |
| 1. Inventory and baseline | Passed | Fresh Rust 270/270, frontend 64/64, strict Clippy, formatting and binding-diff gates; 1,112 inventory entries across 27 files; inventory lexer/reconciliation tests 4/4 |
| 2. Boundaries and supporting code | Pending | Exact commands/bindings, private tests, focused policy components |
| 3. State and durable commits | Pending | Pure transitions, projection and one commit owner behind StateStore |
| 4. Complete application workflows | Pending | Thin commands, explicit dependencies, startup/shutdown composition |
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
- StateStore owns authoritative state. Pure reducers and journal projection do not
  perform I/O. One commit executor retains its mutation guard through preparation,
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
