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

All eight `downloader::process::tests::` cases passed after application. They
cover cancellation/reaping, oversized output, inherited stderr/stdout pipes,
split-line completion, blocked progress callbacks, and cancellation before a
suspended child's first instruction. The receipt is
`target/internal-cleanup-stage5/process-tests.log`.

The full Rust, source-review, performance, and soak gates remain pending until
the complete stage is integrated. Native qualification remains deferred.
