# Frontend source ownership

The 0.7.9 Clarity renderer has focused owners for lifecycle, editing, view state,
queue projection, errors, and rendering. The generated
[frontend source inventory](frontend-source-inventory.json) contains **537
callables across 51 production files**. Discovery is not substantive review.
Source/span hashes detect drift; behavior is covered by unit, browser, and
separate native checks in [Clarity QC](clarity-0.7.9.md).

## Responsibilities and invariants

| Concern | Owner | Boundary |
| --- | --- | --- |
| Reactive composition and rendering | `+page.svelte` | Connect typed controllers and components; no duplicated selection, virtual-row, or filename state |
| Startup, listeners, recovery, waiters, disposal | `app-session.ts` | Required subscription readiness stays separate from synchronization health; disposal rejects late callbacks |
| Snapshot/delta ordering | `app-state-controller.ts`, `state-reconciler.ts` | Accept authoritative state, recover gaps, reject stale responses |
| Queue projection and progress | `queue-presentation.ts`, `queue-presentation-helpers.ts` | Project backend facts, prepare metadata, reconcile progress; no direct IPC or filename-controller dependency |
| Filtering, selection, viewport | `queue-view-controller.ts`, `queue-view.ts` | One owner for visible IDs, selection, scroll clamping, and virtual rows |
| Filename drafts and commits | `filename-editor.ts`, `filename-editor-focus.ts` | Row-local focus; deduplicated saves; generation guards; preserve failed drafts; await affected edits before start |
| Row/table commands | `queue-row-actions.ts`, `queue-actions.ts` | Cohesive typed actions and injected command ports; backend owns durable mutations |
| Error reporting and read state | `ui-error-reporter.ts`, `error-inbox.ts`, `interface-errors.ts` | Synchronous attempt identity; bounded 200-entry history; recovery does not erase history; opening Settings marks read |
| Appearance | `appearance-controller.ts`, `theme.ts` | Persist Light/Dark/System preference and release device-theme listeners |
| Inspection/playlist admission | `inspection-workflow.ts`, `playlist-metadata-owner.ts`, `media-identity.ts` | Preserve original entry indices, request identity, cancellation, paging, and bounded retained metadata |
| Download lifecycle | `operation-reducer.ts`, `operation-wait-registry.ts`, `backend-state.ts` | Preserve operation identity, terminal precedence, timeouts, and disposal |
| Runtime and application updates | `runtime-workflow.ts`, `app-update-workflow.ts` | Separate health/update failures, end progress on settlement, preserve blocking guards and installer handoff |
| Settings and diagnostics | `settings-diagnostics-workflow.ts` | Catch rejected dialogs; cancellation is a no-op; preserve redaction and settings on failure |
| Accessible dialogs | `accessible-dialog.ts` | Initial focus, Escape, tab containment, dismissal, and balanced cleanup |

Settings content is split into `SettingsDialog`, `DownloadAccessSettings`,
`RuntimeSettings`, and `DiagnosticsSettings`. `DownloadDefaults` remains in the
main view; `HelpDialog` owns help content. `Sidebar`, `QueueTable`, and `QueueRow`
render the live queue. `HeaderRuntime` is now a Settings child; its filename is
not an instruction to put diagnostics back above the main queue. The retired
`SettingsRow` and `RowDiagnostics` components are removed.

## Complete generated file map

| Source | Callables | Responsibility |
| --- | ---: | --- |
| `src/lib/accessible-dialog.ts` | 6 | Provide keyboard focus, dismissal, and cleanup behavior for accessible dialogs. |
| `src/lib/app-session.ts` | 32 | Coordinate startup, subscriptions, snapshot recovery, waiters and disposal |
| `src/lib/app-state-controller.ts` | 25 | Own renderer snapshot/delta application and resynchronization sequencing. |
| `src/lib/app-update-workflow.ts` | 11 | Own application version, update checks, installation, and update dialog state. |
| `src/lib/appearance-controller.ts` | 9 | Own appearance initialization, system changes, and save rollback |
| `src/lib/backend-state.ts` | 17 | Derive stable operation and published-output facts from backend contracts. |
| `src/lib/components/AppUpdateDialog.svelte` | 0 | Render update details and forward update and dismissal actions. |
| `src/lib/components/DiagnosticsSettings.svelte` | 2 | Render diagnostics export and cleanup controls |
| `src/lib/components/DownloadAccessSettings.svelte` | 1 | Render advanced cookie and configuration access settings |
| `src/lib/components/DownloadDefaults.svelte` | 0 | Render download format, quality and destination defaults |
| `src/lib/components/HeaderRuntime.svelte` | 0 | Render version, readiness, maintenance, and update controls. |
| `src/lib/components/HelpDialog.svelte` | 0 | Render the accessible keyboard and download help dialog |
| `src/lib/components/Icon.svelte` | 0 | Render consistent accessible interface glyphs |
| `src/lib/components/PlaylistDialog.svelte` | 6 | Render the playlist picker and own its local select-all DOM reference. |
| `src/lib/components/QueueRow.svelte` | 14 | Render a queue row and forward selection, filename, and download actions. |
| `src/lib/components/QueueTable.svelte` | 0 | Compose the queue viewport, table controls, and virtual rows. |
| `src/lib/components/QueueToolbar.svelte` | 0 | Render queue and diagnostic actions with their existing admission states. |
| `src/lib/components/RuntimeSettings.svelte` | 5 | Compose download tools and application update controls |
| `src/lib/components/SettingsDialog.svelte` | 1 | Compose appearance, error history, tools, and diagnostics |
| `src/lib/components/Sidebar.svelte` | 2 | Navigate queue filters and expose unread error notifications |
| `src/lib/components/StatusFooter.svelte` | 0 | Render the existing queue counts and status announcement. |
| `src/lib/components/UrlBar.svelte` | 0 | Render URL entry, inspection cancellation, and runtime progress. |
| `src/lib/error-inbox.ts` | 5 | Retain bounded session error history and acknowledge unread errors |
| `src/lib/filename-editor-focus.ts` | 1 | Focus and select the row-local filename input on activation |
| `src/lib/filename-editor.ts` | 12 | Own filename drafts, validation, save ordering and stale completion guards |
| `src/lib/frontend-errors.ts` | 3 | Preserve existing user-facing error normalization and diagnostic detail. |
| `src/lib/frontend-types.ts` | 0 | Define shared renderer presentation types and existing format defaults. |
| `src/lib/frontend-workflow-ports.ts` | 0 | Declare typed command, operation-wait, and lifetime dependencies. |
| `src/lib/inspection-workflow.ts` | 24 | Own URL and playlist inspection, admission, cancellation, and their display state. |
| `src/lib/interface-errors.ts` | 3 | Collect failure state from composed workflows into the error inbox |
| `src/lib/ipc-client.ts` | 8 | Provide the typed command and event boundary used by renderer workflows. |
| `src/lib/media-identity.ts` | 2 | Define stable media identity keys from a URL and optional exact media selection. |
| `src/lib/operation-reducer.ts` | 6 | Order and reduce operation progress without regressing terminal state. |
| `src/lib/operation-wait-registry.ts` | 19 | Own bounded renderer waiters for operation completion and teardown. |
| `src/lib/page-lifetime.ts` | 6 | Own page resources and suppress callbacks after renderer disposal. |
| `src/lib/playlist-metadata-owner.ts` | 12 | Own bounded request-scoped playlist display metadata until confirmed queue rows arrive. |
| `src/lib/queue-actions.ts` | 31 | Own queue command ordering, optimistic cancellation, retries, and settings changes. |
| `src/lib/queue-logic.ts` | 7 | Validate and derive queue, format, selection, and redacted display behavior. |
| `src/lib/queue-presentation-helpers.ts` | 31 | Project authoritative queue records and format queue display state without side effects. |
| `src/lib/queue-presentation.ts` | 40 | Own queue projection, metadata, and progress reconciliation. |
| `src/lib/queue-row-actions.ts` | 0 | Declare cohesive typed queue row and filename action ports |
| `src/lib/queue-view-controller.ts` | 19 | Own queue filtering, selection, viewport and scroll lifecycle |
| `src/lib/queue-view.ts` | 12 | Filter and virtualize queue views and coordinate search visibility |
| `src/lib/runtime-workflow.ts` | 21 | Own runtime checks, repair/update workflows, and runtime presentation state. |
| `src/lib/settings-diagnostics-workflow.ts` | 20 | Own output/cookie settings and diagnostic export, clear, and copy workflows. |
| `src/lib/startup-state.ts` | 6 | Derive startup readiness and subsystem recovery state. |
| `src/lib/state-reconciler.ts` | 9 | Coordinate ordered state-delta delivery, gap recovery, and listener disposal. |
| `src/lib/theme.ts` | 4 | Apply color tokens and invoke durable appearance preferences |
| `src/lib/ui-error-reporter.ts` | 4 | Record synchronous failures with source and attempt identity |
| `src/routes/+layout.js` | 0 | Declare the renderer-only static application layout mode. |
| `src/routes/+page.svelte` | 101 | Compose the main window, user actions, backend workflows, and visible application state. |
## Inventory commands

```powershell
node scripts/frontend-source-inventory.mjs
node scripts/frontend-source-inventory.mjs --check
node --test scripts/frontend-source-inventory.test.mjs
```

The generator resolves the repository-installed TypeScript and Svelte compilers.
It excludes tests and generated bindings, hashes production files even if they
have no callables, and fails on parse errors. It does not establish native
behavior, replace type checking, or grant review status automatically.