# Frontend source ownership

This document assigns current production TypeScript and Svelte callables to responsibilities and workflows. The generated companion is [`frontend-source-inventory.json`](frontend-source-inventory.json).

The compiler-backed inventory contains **411 callables across 21 production files**. Inclusion is discovery, not substantive review. Review requires inspecting the current span, callers, effects, ordering, cleanup, and workflow obligations. A source or callable-span change alters its SHA-256 identity and requires renewed review.

## Scope and method

`scripts/frontend-source-inventory.mjs` uses the repository-installed TypeScript and Svelte compilers. It scans `.js`, `.ts`, and `.svelte` under `nuclear-app/src`, excluding tests, generated bindings, and the declared accessible-dialog harness. Files without callables remain hashed so drift is detected. Entries record path, lexical owner, symbol, source location, classification, async flag, file and span hashes, responsibility, and workflows. Parse diagnostics fail generation. This is not a type check, reachability proof, behavioral review, or workflow result.

## Ownership map

| Source | Entries | Responsibility | Workflows |
| --- | ---: | --- | --- |
| `src/routes/+page.svelte` | 124 | Compose controller state, lifecycle/event wiring, and visible UI. | startup, inspection, queue, download, cancellation, runtime-update, app-update, diagnostics |
| `src/routes/+layout.js` | 0 | Declare renderer-only static layout mode. | startup |
| `src/lib/accessible-dialog.ts` | 6 | Own dialog focus, keyboard dismissal, and cleanup. | dialogs, accessibility |
| `src/lib/app-state-controller.ts` | 24 | Apply snapshots and deltas and sequence resynchronization. | startup, state-sync |
| `src/lib/app-update-workflow.ts` | 11 | Own app version, update checks, installation, and modal state. | startup, app-update |
| `src/lib/backend-state.ts` | 17 | Derive operation and published-output facts. | state-sync, queue, download |
| `src/lib/frontend-errors.ts` | 3 | Normalize visible errors and retain diagnostic detail. | inspection, download, diagnostics |
| `src/lib/frontend-types.ts` | 0 | Define shared renderer types and format defaults. | queue, inspection, settings |
| `src/lib/frontend-workflow-ports.ts` | 0 | Declare command, wait, and lifetime ports. | ipc, startup, cancellation |
| `src/lib/inspection-workflow.ts` | 21 | Own URL/playlist inspection, admission, cancellation, and modal state. | inspection, queue, cancellation |
| `src/lib/ipc-client.ts` | 5 | Provide the typed command and event boundary. | ipc, state-sync |
| `src/lib/operation-reducer.ts` | 6 | Reduce progress without regressing terminal state. | download, cancellation, state-sync |
| `src/lib/operation-wait-registry.ts` | 19 | Own bounded operation waiters and teardown. | download, cancellation, runtime-update, app-update |
| `src/lib/page-lifetime.ts` | 6 | Own page resources and suppress callbacks after disposal. | startup, state-sync, cancellation |
| `src/lib/queue-actions.ts` | 30 | Own queue commands, cancellation rollback, retries, removal, and item settings. | queue, download, cancellation |
| `src/lib/queue-logic.ts` | 7 | Derive formats, qualities, selection, and redacted text. | queue, download, diagnostics |
| `src/lib/queue-presentation.ts` | 77 | Own projection, progress display, selection, filenames, and retained inspection metadata. | queue, download, state-sync |
| `src/lib/runtime-workflow.ts` | 20 | Own runtime checks, repair/update actions, and runtime state. | startup, runtime-update |
| `src/lib/settings-diagnostics-workflow.ts` | 20 | Own output/cookie settings and diagnostic copy/export/clear. | startup, settings, diagnostics |
| `src/lib/startup-state.ts` | 6 | Derive startup readiness and subsystem recovery. | startup, runtime-update |
| `src/lib/state-reconciler.ts` | 9 | Sequence deltas, recover gaps, and dispose listeners. | startup, state-sync |

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
| Markup callbacks | 16 |
| **Total** | **411** |

Regenerate from a frozen source tree before assigning reviewers. Evidence should identify the exact inventory `id`, `sourceHash`, and `spanHash`.

```powershell
node scripts/frontend-source-inventory.mjs
node scripts/frontend-source-inventory.mjs --check
node --test scripts/frontend-source-inventory.test.mjs
```

The generator resolves compilers through `nuclear-app/package.json`; it does not require a global Node installation or add dependencies.
