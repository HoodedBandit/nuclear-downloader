# Frontend lifecycle regression record

This record separates demonstrated defects from structural extraction. The comparison production source is `df582df4ddc566712729b7006dfb080374c5ef45`. Renderer disposal owns local resources; durable operations remain owned by Rust.

## Operation waiters

The original registry failed four newly executed deterministic tests: rejected waits retained a refresh timer, already terminal waits scheduled unnecessary refresh timers, an obsolete refresh failure rejected a replacement wait for the same operation, and an obsolete refresh read and settled replacement state. A fifth regression demonstrated that a terminal fast path left a previously registered waiter alive.

The fix binds deadlines, refresh scheduling, completion, and rejection to the exact waiter object. A terminal observation settles an existing waiter before returning. Disposal closes the registry, rejects local waits, clears scheduled timers immediately, and suppresses results from refreshes already in flight. It does not issue cancellation commands. Event-loss reconciliation retains its one-second default interval.

The focused suite passed 15 tests after the fix, including deadline replacement, missed-event recovery, refresh rejection, disposal before and during refresh, terminal fast paths, and timer counts. Strict ESLint passed for the implementation and tests. This change is recorded in local commit `ee14e90`.

## Controller and mounted page

Each controller start owns a fresh reconciler, sequence target, reload slot, and listener set. Late listeners are disposed immediately; stale handlers, results, failures, and finalizers cannot alter a newer session. Startup completes both registrations before requesting its snapshot. Coalesced reloads retain already buffered deltas. The original controller failed three new tests for late subscription cleanup, early resync loading, and overlapping startup; the final focused suite passed 20 tests. It also covers each registration boundary, partial startup failure, old completion while a newer load remains pending, same-turn stop, throwing cleanup, and resync/reload ordering. The fix is recorded in local commit `dd1eabd`.

The mounted page has one resource owner for controller shutdown, listeners, its resize observer, and operation waiters. Callback guards and post-await checks prevent disposed pages from publishing state or continuing workflow admission. Cleanup attempts every owned disposer even if an earlier one throws. Page markup, styles, labels, refresh intervals, and command payloads remain unchanged.

The new mounted-page test was executed against a temporary copy of the original `df582df` page. Holding `download-progress` registration, unmounting, then resolving the listener failed because its unlistener was never called. That failure is retained in `target/internal-cleanup-baseline/20260909T050054Z/page-lifecycle-red.log`; the temporary test sources were removed. The current page passes all 11 mounted-page cases, covering five subscription boundaries, repeated mounts, delayed snapshots and initializers, and late inspection completion. Four resource-owner tests also passed.

The integrated frontend suite passed 100 tests across 12 files. The standalone visual-fixture Node suite passed two tests. Vitest retains its existing native helper tests and excludes only the standalone `node:test` fixture; the latter runs explicitly with Node. Svelte/TypeScript checking reported zero errors and warnings, strict ESLint and formatting passed, and the fresh production build passed the test-hook exclusion check.

All 11 renderer workflows passed from `target/renderer-checks/workflows-20260909T063522Z-ab165cc43b444ad99f36487afc951500/`, with unchanged input hashes verified after execution. The two-scale candidate comparison passed all 60 scenarios with identical decoded pixels and semantic evidence, including stable repeated captures. `internal-cleanup-lifecycle-visual.json` binds both candidate artifacts, their archived production inputs, and the fixed baseline. Stage 2's regression gate passed. Native Windows qualification and the baseline performance failure remain open.
