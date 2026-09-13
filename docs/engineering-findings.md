# Engineering findings and regression checklist

This 2026-09-13 follow-up starts from `242d327`. Mocked renderer and isolated
Rust test-executable evidence do not qualify the installed native application.

| Finding or obligation | Owner | Regression evidence |
| --- | --- | --- |
| Serial per-entry playlist delay | Batch service, StateStore, inspection workflow | Atomic 100/1,000-row admission; one-save count; one-command renderer batch and confirmation-to-paint cases. |
| Duplicate/lost admission replies | Parent durable receipt; scoped metadata owner | Replay, raw-payload conflict, concurrent same request, reply/event ordering, retry and metadata lease release. |
| Replay depends on unavailable output directory | Raw fingerprint prelookup; locked recheck | Both service replay/conflict regressions reproduced before the fix and passed afterward. |
| Missing metadata treated as ready | Preparation queue and linked Inspection operations | Ready/edit guards, completion without download completion, audio/identity checks, cancel/remove/stale completion and explicit retry. |
| Missing extractor ID becomes sentinel unknown | Metadata parser | Absent/null/empty/blank rejection; legitimate literal unknown accepted; failing regression captured before the fix. |
| Queue authority across retention/restart | State retention and journal validation | Parent receipt retention, interrupted preparation, all three optional fields omitted across two opens, existing expiry/overflow/dangling-reference cases. |
| Unclaimed preparations escape cancellation | Lifecycle registration; pending batch finalizer | Registration-before-return, cancel-before-claim/process, caller abort, Cancel All/shutdown and persistence-failure regressions. |
| Runtime/media identity lost during extraction | Executable leases; machine output records | Hash/lease tests, wrong/missing final ID, ambiguous fallback, staging containment, collisions and cleanup. |
| Durable state/event ordering changes | Mutation guard; blocking journal commit; bounded outbox | Blocked-I/O snapshot probe, finalization failures, ordered producers and overflow/resync. |
| Presentation invokes backend directly | Pure queue helpers; filename workflow port | AST dependency rule, filename/error/disposal tests, source inventory and 15 browser workflows. |
| Extraction changes visible output | Page composition and focused components | Stable 60-scenario decoded-pixel/geometry/text/control/focus comparison using matching frozen inputs. |
| Complexity silently grows | Source-health contract and gate parity | Python and frontend fixtures, exact legacy ceilings, stale/malformed exception rejection and explicit-any lint. |
| Local builds are development/stale/unowned outputs | Packaged build and contract helpers | Exact Git root/version, reparse/overlap, tool identity and preflight tests; actual artifact receipt recorded separately after construction. |
| Missing public update keys fail late in release compilation | Packaged build preflight | Missing/malformed current key, partial/duplicate rotation pair and valid public-key fixtures; real public configuration accepted; final production package built. |
| Debug-only binding produces a release warning | Verified runtime fallback | Identifier-only correction; full Rust regression run and strict release Clippy with warnings denied. |
| Evidence attributes wrong source or workload | Input manifests; exact-root guards; frozen executable hashes | Parent-Git rejection, preserved invalid receipts, matched recaptures and same-mode counterbalanced backend runs. |
| Debug benchmark memory and timing review flags | Performance evidence | Three final matched repeats, bounded journal-phase diagnostics and PE inspection; hard gates pass, performance review remains open. See the dated performance report. |

Final ordinary checks executed 384 Rust tests and 214 frontend tests, plus
strict Clippy, frontend static/build and tooling contract gates. Opt-in soaks
and performance checks have separate receipts and are excluded from those
counts. See [current status](engineering-status.md) for active versus completed
measurements, performance observations and native acceptance limits.
