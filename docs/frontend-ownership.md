# Frontend source ownership

This document assigns the current production TypeScript and Svelte script callables to concrete responsibilities and workflows before internal extraction begins. The generated companion inventory is [`frontend-source-inventory.json`](frontend-source-inventory.json).

The inventory is automated compiler-backed discovery. Its 280 entries are **not source reviews**, and generation does not mark any entry reviewed, accepted, or behaviorally correct. Substantive review must inspect each current source-bound entry, its callers, effects, cleanup, ordering, and workflow obligations. A source or callable-span change alters its SHA-256 identity and requires renewed review.

## Scope and method

`scripts/frontend-source-inventory.mjs` loads the repository-installed TypeScript 5.6.3 and Svelte 5.56.4 compilers from `nuclear-app/node_modules`. TypeScript compiler nodes identify declarations, methods, accessors, constructors, function-valued expressions, and callbacks. The Svelte compiler identifies module and instance script regions before those regions are parsed as TypeScript, and its template AST identifies inline arrow/function callbacks and snippet callable boundaries.

The production scan includes `.js`, `.ts`, and `.svelte` files under `nuclear-app/src`. It excludes `*.test.ts`, `*.test.svelte`, generated `src/lib/bindings/**`, and the explicitly declared `src/lib/AccessibleDialogHarness.svelte` test-support component. Other extensions are outside this frontend callable inventory. Files with no callables, such as `+layout.js`, remain in the file hash set so source drift is still detected.

Each entry records the repository-relative path, lexical owner, symbol, exact start/end offsets, start/end line, classification, async flag, complete-file `sourceHash`, callable `spanHash`, responsibility, and workflow list. Anonymous callbacks use their compiler parent call and argument position as a stable navigation label. TypeScript parse diagnostics fail inventory generation instead of accepting a compiler recovery tree. These identities are structural review aids rather than a TypeScript type-check or reachability proof.

## Ownership map

| Source                               | Entries | Responsibility                                                                           | Workflows                                                                                   |
| ------------------------------------ | ------: | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `src/routes/+page.svelte`            |     194 | Compose the main window, user actions, backend workflows, and visible application state. | startup, inspection, queue, download, cancellation, runtime-update, app-update, diagnostics |
| `src/routes/+layout.js`              |       0 | Declare the renderer-only static application layout mode.                                | startup                                                                                     |
| `src/lib/accessible-dialog.ts`       |       6 | Provide keyboard focus, dismissal, and cleanup behavior for accessible dialogs.          | dialogs, accessibility                                                                      |
| `src/lib/app-state-controller.ts`    |      14 | Own renderer snapshot/delta application and resynchronization sequencing.                | startup, state-sync                                                                         |
| `src/lib/backend-state.ts`           |      17 | Derive stable operation and published-output facts from backend contracts.               | state-sync, queue, download                                                                 |
| `src/lib/ipc-client.ts`              |       5 | Provide the typed command and event boundary used by renderer workflows.                 | ipc, state-sync                                                                             |
| `src/lib/operation-reducer.ts`       |       6 | Order and reduce operation progress without regressing terminal state.                   | download, cancellation, state-sync                                                          |
| `src/lib/operation-wait-registry.ts` |      16 | Own bounded renderer waiters for operation completion and teardown.                      | download, cancellation, runtime-update, app-update                                          |
| `src/lib/queue-logic.ts`             |       7 | Validate and derive queue, format, selection, and redacted display behavior.             | queue, download, diagnostics                                                                |
| `src/lib/startup-state.ts`           |       6 | Derive startup readiness and subsystem recovery state.                                   | startup, runtime-update                                                                     |
| `src/lib/state-reconciler.ts`        |       9 | Coordinate ordered state-delta delivery, gap recovery, and listener disposal.            | startup, state-sync                                                                         |

## Workflow review boundaries

| Workflow                          | Primary owners                                                                                        | Review obligations                                                                                                                                                                      |
| --------------------------------- | ----------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Startup and state synchronization | `+page.svelte`, `app-state-controller.ts`, `state-reconciler.ts`, `startup-state.ts`, `ipc-client.ts` | Listener installation must precede snapshot reconciliation; sequence gaps trigger bounded resync; stale deltas cannot regress installed state; teardown releases listeners and waiters. |
| Inspection and queue editing      | `+page.svelte`, `queue-logic.ts`, `backend-state.ts`                                                  | User selections and inspected metadata remain authoritative; queue mutations preserve current validation, selection, paging, and visible error behavior.                                |
| Download progress and completion  | `+page.svelte`, `operation-reducer.ts`, `operation-wait-registry.ts`, `backend-state.ts`              | Progress ordering, terminal precedence, published output paths, waiter resolution, timeouts, and disposal retain their present contracts.                                               |
| Cancellation                      | `+page.svelte`, `operation-reducer.ts`, `operation-wait-registry.ts`                                  | Cancelling state cannot be overwritten by late progress; terminal notification resolves each owned waiter once; shutdown rejects remaining waiters.                                     |
| Runtime and application updates   | `+page.svelte`, `startup-state.ts`, `operation-wait-registry.ts`                                      | Maintenance/readiness gates, progress, retry paths, installer handoff, and operation waiting retain existing user-visible ordering and messages.                                        |
| Dialogs and accessibility         | `+page.svelte`, `accessible-dialog.ts`                                                                | Focus capture/restoration, Escape handling, tab containment, dismissal, and action cleanup remain balanced across open/close and component teardown.                                    |
| Diagnostics                       | `+page.svelte`, `queue-logic.ts`                                                                      | Export/clear flows and display redaction retain current payload and visible behavior.                                                                                                   |

## Classification totals

| Compiler classification                 |   Count |
| --------------------------------------- | ------: |
| Function declarations                   |     142 |
| Methods                                 |      21 |
| Constructors                            |       3 |
| Getters                                 |       1 |
| Function-valued declarations/properties |       6 |
| Synchronous callbacks                   |      90 |
| Async callbacks                         |       1 |
| Markup callbacks                        |      16 |
| Markup async callbacks                  |       0 |
| Snippet callables                       |       0 |
| **Total**                               | **280** |

The inventory should be regenerated from a frozen source tree before assigning manual reviewers. Review evidence should refer to the exact `id`, `sourceHash`, and `spanHash` from that generation. Discovery counts, compiler parsing, and fixture tests establish coverage mechanics only; they do not establish that the discovered code has been substantively reviewed or that a workflow passed.

## Commands

From the repository root:

```powershell
node scripts/frontend-source-inventory.mjs
node scripts/frontend-source-inventory.mjs --check
node --test scripts/frontend-source-inventory.test.mjs
```

The generator resolves compiler packages through `nuclear-app/package.json`, so it does not depend on a global Node package installation or add dependencies.
