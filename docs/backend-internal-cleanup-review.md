# Bounded backend extraction review

This stage follows the completed frontend component gates at `9221250`. The
comparison implementation remains `df582df`. These are module extractions;
method behavior, command/event contracts, resource limits, and journal schema
remain unchanged.

## StateStore

The parent owns construction, snapshots, locking, shared state, and the existing
`commit` module. Three private children group queue commands (10 methods),
operation commands (10), and maintenance commands (5). Each child has one
`impl StateStore` and explicit imports. No forwarding facade or new state
container was introduced.

All 25 moved method bodies and seven retained parent methods match the accepted
source review. `begin_operation_with_limit` changes from parent-private to
`pub(super)` in its new child so the existing sibling test module retains the
same access. This is the only signature adjustment. The mutation guard still
spans candidate preparation, journal saving, state installation, and ordered
event publication. Notifications, lock drops, awaits, and cancellation checks
remain in their original method bodies.

The focused `state::tests::` run passed all 35 tests after application. It covers
rollback on failed durable mutations, terminal save retries and degraded-state
recovery, queue retention/restart, admission limits, maintenance handoff,
cancellation, and detached commit ownership. Rust formatting, architecture
checks, and diff whitespace checks also passed. Evidence is retained in
`target/internal-cleanup-stage5/state-tests.log`; source/equivalence records are
under `target/internal-cleanup-stage5/`.

The compiler identified two unused maintenance imports after extraction; they
were removed without changing any method body.

## Process supervision and output

`downloader/process.rs` retains the supervisor, cancellation token, executable
leases, Windows Job handle, suspended-child registration/resume, and child
termination/reaping. Its private `output` child owns stdout/stderr readers,
bounded tails, reader-task lifetimes, concurrent output monitoring, and the
post-exit drain deadline. Narrow re-exports preserve the existing downloader
call sites and test access.

All 39 function/method bodies are byte-equivalent to the pre-extraction source.
The separate declaration review checked constants, types, fields, derives,
platform/test guards, and the Windows handle's unsafe Send/Sync declarations.
Existing test hooks retain their original scope; no process framework or new
public interface was added.

All eight `downloader::process::tests::` cases passed in the focused run. They
cover cancellation/reaping, oversized output, inherited stderr/stdout pipes,
split-line completion, blocked progress callbacks, and cancellation before a
suspended child's first instruction. The receipt is
`target/internal-cleanup-stage5/process-tests.log`. Copied-file timestamps left
uncertainty about that run's compiler freshness. The later, explicitly fresh
integrated suite includes all eight cases and is the authoritative acceptance
evidence for the extracted code.

## Staging, output resolution, and publication

The publication parent is a facade with existing test adapters. Three private
children own stage creation/authentication/cleanup, machine-record and fallback
resolution, and collision-safe destination publication. Open-handle identity
checks, bounded reads, quarantine decisions, flush ordering, and atomic moves
remain in their original method bodies. Downloader-facing items retain narrow
visibility; unused type re-exports were removed after compiler feedback.

The extraction verifier covers all 40 canonical callables, including lease
methods, nested helpers, platform variants, FFI declarations, and test adapters.
Their bodies match the previous source. The declaration review also preserves
fields, derives, serde rules, constants, and platform/test guards.

All 17 `downloader::publication::tests::` cases passed. They exercise machine
records, ambiguous fallbacks, metadata/read bounds, path confinement, staging
ownership, partial marker rollback, and concurrent collision-safe publication.
Evidence is `target/internal-cleanup-stage5/publication-tests.log`.

## Integrated Rust gate

Strict Clippy passed for all targets and features with `-D warnings`. A fresh
Cargo test build then passed all 304 ordinary tests; three performance/soak
harness tests remained opt-in. The Cargo artifact record explicitly reports
`fresh: false`. Source manifests before and after the suite match, and generated
bindings are unchanged. The test executable is bound by SHA-256 in
`target/internal-cleanup-stage5/rust-test-receipt.json`; the full log is
`rust-tests.log` beside it. This fresh suite covers all three extractions.

The final source review, architecture rules, and formatting passed with all
1,215 review units current. The integrated frontend gate passed 167 tests, all
11 renderer workflows, and exact pixels, geometry, text, controls, and focus
comparison for all 60 scenarios against the original baseline. Evidence is
`target/frontend-extractions/stage5-integrated-20260909T092230Z-9499add6a12f412792632fc5ed26bdbc/`
and `internal-cleanup-stage5-integrated-visual.json`.

Packaging, evidence-contract, process-helper, inventory, architecture, renderer
provenance, visual comparator, and performance-comparator fixtures also passed.
These synthetic checks do not qualify an installer or clean Windows desktop.
Pinned cargo-deny passed its cached advisory, license, source, and ban policies;
the networked npm audit awaits specific authorization after automatic approval
review rejected transmission of dependency metadata.

Structural implementation and its source/regression gates are complete. Matched
performance and fresh soaks remain pending. Native qualification remains deferred.
