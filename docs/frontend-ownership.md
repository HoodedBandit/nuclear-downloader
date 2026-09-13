# Frontend source ownership

This document assigns current production TypeScript and Svelte callables to responsibilities and workflows. The generated companion is [`frontend-source-inventory.json`](frontend-source-inventory.json).

The compiler-backed inventory contains **451 callables across 34 production files**. Inclusion is discovery, not substantive review. Review requires inspecting the current span, callers, effects, ordering, cleanup, and workflow obligations. A source or callable-span change alters its SHA-256 identity and requires renewed review.

## Scope and method

`scripts/frontend-source-inventory.mjs` uses the repository-installed TypeScript and Svelte compilers. It scans `.js`, `.ts`, and `.svelte` under `nuclear-app/src`, excluding tests, generated bindings, and the declared accessible-dialog harness. Files without callables remain hashed so drift is detected. Entries record path, lexical owner, symbol, source location, classification, async flag, file and span hashes, responsibility, and workflows. Parse diagnostics fail generation. This is not a type check, reachability proof, behavioral review, or workflow result.

The current inventory includes the typed `media-identity.ts` owner, the
request-scoped `playlist-metadata-owner.ts`, and the pure
`queue-presentation-helpers.ts` projection boundary. `inspection-workflow.ts`
owns one backend-authoritative playlist batch request per confirmation: it
captures settings once, preserves the request ID for an identical retry, keeps
the parent inspection open until success, and passes original retained-entry
indices rather than renderer-authored child admission metadata.

The final frontend receipt, `target/engineering-final-frontend-test.log`, reports
214 passing tests with one opt-in soak skipped. Svelte checking, strict lint,
format checking, and the production bundle also passed. The renderer receipt
reports all 15 browser workflows passing, including batch admission and
100/1,000-row timing cases. The matched visual comparison at
`target/engineering-matched-visual-comparison.json` passed all 60 scenarios with
unchanged decoded pixels, geometry, text, controls, and focus. Its browser scale
emulation does not qualify native Windows scaling. The renderer two-hour run
started under `target/renderer-soak/20260913T210906Z-5d8475e67a4e414f851090988aa723fe`
and remains active, so it is not a passed gate. These results do not establish
native Windows behavior.

## Ownership map

| Source | Entries | Responsibility | Workflows |
| --- | ---: | --- | --- |
| `src/lib/accessible-dialog.ts` | 6 | Provide keyboard focus, dismissal, and cleanup behavior for accessible dialogs. | dialogs, accessibility |
| `src/lib/app-state-controller.ts` | 24 | Own renderer snapshot/delta application and resynchronization sequencing. | startup, state-sync |
| `src/lib/app-update-workflow.ts` | 11 | Own application version, update checks, installation, and update dialog state. | startup, app-update |
| `src/lib/backend-state.ts` | 17 | Derive stable operation and published-output facts from backend contracts. | state-sync, queue, download |
| `src/lib/components/AppUpdateDialog.svelte` | 0 | Render update details and forward update and dismissal actions. | app-update, dialogs, accessibility |
| `src/lib/components/HeaderRuntime.svelte` | 0 | Render version, readiness, maintenance, and update controls. | startup, runtime-update, app-update |
| `src/lib/components/PlaylistDialog.svelte` | 6 | Render the playlist picker and own its local select-all DOM reference. | inspection, queue, dialogs, accessibility |
| `src/lib/components/QueueRow.svelte` | 11 | Render a queue row and forward selection, filename, and download actions. | queue, download, cancellation, diagnostics |
| `src/lib/components/QueueTable.svelte` | 0 | Compose the queue viewport, table controls, and virtual rows. | queue, download, accessibility |
| `src/lib/components/QueueToolbar.svelte` | 0 | Render queue and diagnostic actions with their existing admission states. | queue, cancellation, diagnostics |
| `src/lib/components/RowDiagnostics.svelte` | 1 | Render one row's redacted error details and copy action. | diagnostics, queue |
| `src/lib/components/SettingsRow.svelte` | 1 | Render existing format, output, cookie, and compatibility settings. | settings, queue |
| `src/lib/components/StatusFooter.svelte` | 0 | Render the existing queue counts and status announcement. | queue, download, accessibility |
| `src/lib/components/UrlBar.svelte` | 0 | Render URL entry, inspection cancellation, and runtime progress. | inspection, runtime-update |
| `src/lib/frontend-errors.ts` | 3 | Preserve existing user-facing error normalization and diagnostic detail. | inspection, download, diagnostics |
| `src/lib/frontend-types.ts` | 0 | Define shared renderer presentation types and existing format defaults. | queue, inspection, settings |
| `src/lib/frontend-workflow-ports.ts` | 0 | Declare typed command, operation-wait, and lifetime dependencies. | ipc, startup, cancellation |
| `src/lib/inspection-workflow.ts` | 24 | Own URL and playlist inspection, admission, cancellation, and their display state. | inspection, queue, cancellation |
| `src/lib/ipc-client.ts` | 8 | Provide the typed command and event boundary used by renderer workflows. | ipc, state-sync |
| `src/lib/media-identity.ts` | 2 | Define stable queue identity from URL plus optional exact media selection. | inspection, queue, state-sync |
| `src/lib/operation-reducer.ts` | 6 | Order and reduce operation progress without regressing terminal state. | download, cancellation, state-sync |
| `src/lib/operation-wait-registry.ts` | 19 | Own bounded renderer waiters for operation completion and teardown. | download, cancellation, runtime-update, app-update |
| `src/lib/page-lifetime.ts` | 6 | Own page resources and suppress callbacks after renderer disposal. | startup, state-sync, cancellation |
| `src/lib/playlist-metadata-owner.ts` | 12 | Own bounded request-scoped playlist display metadata until confirmed queue rows arrive. | inspection, queue, state-sync |
| `src/lib/queue-actions.ts` | 31 | Own queue command ordering, optimistic cancellation, retries, filename persistence, and settings changes. | queue, download, cancellation |
| `src/lib/queue-logic.ts` | 7 | Validate and derive queue, format, selection, and redacted display behavior. | queue, download, diagnostics |
| `src/lib/queue-presentation-helpers.ts` | 30 | Project authoritative queue records and format queue display state without side effects. | queue, download, state-sync |
| `src/lib/queue-presentation.ts` | 57 | Own queue presentation state, metadata claims, progress throttling, selection, virtualization, and filename drafts. | queue, download, state-sync |
| `src/lib/runtime-workflow.ts` | 20 | Own runtime checks, repair/update workflows, and runtime presentation state. | startup, runtime-update |
| `src/lib/settings-diagnostics-workflow.ts` | 20 | Own output/cookie settings and diagnostic export, clear, and copy workflows. | startup, settings, diagnostics |
| `src/lib/startup-state.ts` | 6 | Derive startup readiness and subsystem recovery state. | startup, runtime-update |
| `src/lib/state-reconciler.ts` | 9 | Coordinate ordered state-delta delivery, gap recovery, and listener disposal. | startup, state-sync |
| `src/routes/+layout.js` | 0 | Declare the renderer-only static application layout mode. | startup |
| `src/routes/+page.svelte` | 114 | Compose the main window, user actions, backend workflows, and visible application state. | startup, inspection, queue, download, cancellation, runtime-update, app-update, diagnostics |

## Workflow owners

| Workflow | Primary owners | Review obligations |
| --- | --- | --- |
| Startup/state sync | `+page.svelte`, `app-state-controller.ts`, `state-reconciler.ts`, `startup-state.ts`, `ipc-client.ts` | Install listeners before reconciliation, recover gaps, reject stale deltas, and release resources. |
| Inspection/admission | `inspection-workflow.ts`, `playlist-metadata-owner.ts`, `media-identity.ts`, `queue-presentation.ts` | Preserve validation, original selected-entry indices, stable request identity, immutable captured settings, authoritative parent lifetime, bounded display metadata, paging, cancellation, retries, and errors. |
| Queue display/editing | `queue-presentation.ts`, `queue-presentation-helpers.ts`, `queue-actions.ts`, `backend-state.ts` | Preserve authoritative projection, metadata ownership, progress precedence and throttling, selection, filenames, payloads, filters, and rollback. Presentation must remain free of direct IPC. |
| Download/cancellation | `queue-actions.ts`, `operation-reducer.ts`, `operation-wait-registry.ts`, `backend-state.ts` | Preserve priority, terminal precedence, published paths, timeout/disposal, and cancellation diagnostics. |
| Runtime updates | `runtime-workflow.ts`, `startup-state.ts`, `operation-wait-registry.ts` | Preserve readiness, progress, retries, prompts, waiting, and startup callbacks. |
| App updates | `app-update-workflow.ts`, `operation-wait-registry.ts` | Preserve blocking guards, modal/progress state, installer handoff, messages, and startup degradation. |
| Settings/diagnostics | `settings-diagnostics-workflow.ts`, `queue-actions.ts`, `queue-logic.ts` | Preserve defaults, dialogs, directory fanout, snapshots, redaction, clipboard, confirmation, export, and clear. |
| Accessibility | `accessible-dialog.ts`, `+page.svelte` | Preserve focus, Escape, tab containment, dismissal, and balanced cleanup. |

## Classification totals

| Classification | Count |
| --- | ---: |
| Function declarations | 90 |
| Methods | 129 |
| Constructors | 11 |
| Getters | 2 |
| Function-valued declarations/properties | 82 |
| Synchronous callbacks | 114 |
| Async callbacks | 1 |
| Markup callbacks | 22 |
| **Total** | **451** |

Regenerate from a frozen source tree before assigning reviewers. Evidence should identify the exact inventory `id`, `sourceHash`, and `spanHash`.

```powershell
node scripts/frontend-source-inventory.mjs
node scripts/frontend-source-inventory.mjs --check
node --test scripts/frontend-source-inventory.test.mjs
```

The generator resolves compilers through `nuclear-app/package.json`; it does not require a global Node installation or add dependencies.
