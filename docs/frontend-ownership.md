# Frontend source ownership

This document assigns current production TypeScript and Svelte callables to responsibilities and workflows. The generated companion is [`frontend-source-inventory.json`](frontend-source-inventory.json).

The compiler-backed inventory contains **416 callables across 27 production files**. Inclusion is discovery, not substantive review. Review requires inspecting the current span, callers, effects, ordering, cleanup, and workflow obligations. A source or callable-span change alters its SHA-256 identity and requires renewed review.

## Scope and method

`scripts/frontend-source-inventory.mjs` uses the repository-installed TypeScript and Svelte compilers. It scans `.js`, `.ts`, and `.svelte` under `nuclear-app/src`, excluding tests, generated bindings, and the declared accessible-dialog harness. Files without callables remain hashed so drift is detected. Entries record path, lexical owner, symbol, source location, classification, async flag, file and span hashes, responsibility, and workflows. Parse diagnostics fail generation. This is not a type check, reachability proof, behavioral review, or workflow result.

## Ownership map

| Source | Entries | Responsibility | Workflows |
| --- | ---: | --- | --- |
| `src/lib/accessible-dialog.ts` | 6 | Provide keyboard focus, dismissal, and cleanup behavior for accessible dialogs. | dialogs, accessibility |
| `src/lib/app-state-controller.ts` | 24 | Own renderer snapshot/delta application and resynchronization sequencing. | startup, state-sync |
| `src/lib/app-update-workflow.ts` | 11 | Own application version, update checks, installation, and update dialog state. | startup, app-update |
| `src/lib/backend-state.ts` | 17 | Derive stable operation and published-output facts from backend contracts. | state-sync, queue, download |
| `src/lib/components/QueueRow.svelte` | 11 | Render a queue row and forward selection, filename, and download actions. | queue, download, cancellation, diagnostics |
| `src/lib/components/QueueTable.svelte` | 0 | Compose the queue viewport, table controls, and virtual rows. | queue, download, accessibility |
| `src/lib/components/QueueToolbar.svelte` | 0 | Render queue and diagnostic actions with their existing admission states. | queue, cancellation, diagnostics |
| `src/lib/components/RowDiagnostics.svelte` | 1 | Render one row's redacted error details and copy action. | diagnostics, queue |
| `src/lib/components/StatusFooter.svelte` | 0 | Render the existing queue counts and status announcement. | queue, download, accessibility |
| `src/lib/components/UrlBar.svelte` | 0 | Render URL entry, inspection cancellation, and runtime progress. | inspection, runtime-update |
| `src/lib/frontend-errors.ts` | 3 | Preserve existing user-facing error normalization and diagnostic detail. | inspection, download, diagnostics |
| `src/lib/frontend-types.ts` | 0 | Define shared renderer presentation types and existing format defaults. | queue, inspection, settings |
| `src/lib/frontend-workflow-ports.ts` | 0 | Declare typed command, operation-wait, and lifetime dependencies. | ipc, startup, cancellation |
| `src/lib/inspection-workflow.ts` | 21 | Own URL and playlist inspection, admission, cancellation, and their display state. | inspection, queue, cancellation |
| `src/lib/ipc-client.ts` | 5 | Provide the typed command and event boundary used by renderer workflows. | ipc, state-sync |
| `src/lib/operation-reducer.ts` | 6 | Order and reduce operation progress without regressing terminal state. | download, cancellation, state-sync |
| `src/lib/operation-wait-registry.ts` | 19 | Own bounded renderer waiters for operation completion and teardown. | download, cancellation, runtime-update, app-update |
| `src/lib/page-lifetime.ts` | 6 | Own page resources and suppress callbacks after renderer disposal. | startup, state-sync, cancellation |
| `src/lib/queue-actions.ts` | 30 | Own queue command ordering, optimistic cancellation, retries, and settings changes. | queue, download, cancellation |
| `src/lib/queue-logic.ts` | 7 | Validate and derive queue, format, selection, and redacted display behavior. | queue, download, diagnostics |
| `src/lib/queue-presentation.ts` | 77 | Own queue projection, progress presentation, selection, and filename drafts. | queue, download, state-sync |
| `src/lib/runtime-workflow.ts` | 20 | Own runtime checks, repair/update workflows, and runtime presentation state. | startup, runtime-update |
| `src/lib/settings-diagnostics-workflow.ts` | 20 | Own output/cookie settings and diagnostic export, clear, and copy workflows. | startup, settings, diagnostics |
| `src/lib/startup-state.ts` | 6 | Derive startup readiness and subsystem recovery state. | startup, runtime-update |
| `src/lib/state-reconciler.ts` | 9 | Coordinate ordered state-delta delivery, gap recovery, and listener disposal. | startup, state-sync |
| `src/routes/+layout.js` | 0 | Declare the renderer-only static application layout mode. | startup |
| `src/routes/+page.svelte` | 117 | Compose the main window, user actions, backend workflows, and visible application state. | startup, inspection, queue, download, cancellation, runtime-update, app-update, diagnostics |

## Workflow owners

| Workflow | Primary owners | Review obligations |
| --- | --- | --- |
| Startup/state sync | `+page.svelte`, `app-state-controller.ts`, `state-reconciler.ts`, `startup-state.ts`, `ipc-client.ts` | Install listeners before reconciliation, recover gaps, reject stale deltas, and release resources. |
| Inspection/admission | `inspection-workflow.ts`, `queue-presentation.ts`, `queue-logic.ts` | Preserve validation, captured settings, single-use cleanup, deduplication, paging, cancellation, and errors. |
| Queue display/editing | `queue-presentation.ts`, `queue-actions.ts`, `backend-state.ts` | Preserve projection, throttling, selection, filenames, payloads, filters, and rollback. |
| Download/cancellation | `queue-actions.ts`, `operation-reducer.ts`, `operation-wait-registry.ts`, `backend-state.ts` | Preserve priority, terminal precedence, published paths, timeout/disposal, and cancellation diagnostics. |
| Runtime updates | `runtime-workflow.ts`, `startup-state.ts`, `operation-wait-registry.ts` | Preserve readiness, progress, retries, prompts, waiting, and startup callbacks. |
| App updates | `app-update-workflow.ts`, `operation-wait-registry.ts` | Preserve blocking guards, modal/progress state, installer handoff, messages, and startup degradation. |
| Settings/diagnostics | `settings-diagnostics-workflow.ts`, `queue-actions.ts`, `queue-logic.ts` | Preserve defaults, dialogs, directory fanout, snapshots, redaction, clipboard, confirmation, export, and clear. |
| Accessibility | `accessible-dialog.ts`, `+page.svelte` | Preserve focus, Escape, tab containment, dismissal, and balanced cleanup. |

## Classification totals

| Classification | Count |
| --- | ---: |
| Function declarations | 82 |
| Methods | 118 |
| Constructors | 10 |
| Getters | 2 |
| Function-valued declarations/properties | 76 |
| Synchronous callbacks | 106 |
| Async callbacks | 1 |
| Markup callbacks | 21 |
| **Total** | **416** |

Regenerate from a frozen source tree before assigning reviewers. Evidence should identify the exact inventory `id`, `sourceHash`, and `spanHash`.

```powershell
node scripts/frontend-source-inventory.mjs
node scripts/frontend-source-inventory.mjs --check
node --test scripts/frontend-source-inventory.test.mjs
```

The generator resolves compilers through `nuclear-app/package.json`; it does not require a global Node installation or add dependencies.
