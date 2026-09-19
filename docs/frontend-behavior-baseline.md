# Frontend behavior baseline

## Current 0.7.9 Clarity obligations

The approved redesign intentionally replaces the pre-Clarity pixel layout.
Current interaction regressions live in `clarity-ui.e2e.mjs`,
`renderer-workflows.e2e.mjs`, page/controller unit tests, and separate native
fixture suites. Historical visual comparisons below belong to older sources.

| Behavior | Regression coverage |
| --- | --- |
| Light/Dark/System, sidebar counts, filtering, Settings, and Help | Clarity browser scenarios and component/controller tests |
| Actual pointer-click title activation, selected focused input, Enter/blur/Escape | Clarity tests at 1340x850 and 800x500, both themes, 100%/150% browser emulation |
| Ready and waiting renames; read-only active/completed rows | Filename controller, queue row, native fixture, Rust queue/service tests |
| Failed and invalid drafts survive save failures, filtering, and virtual remounts | Filename controller and 1,000-row browser interaction regression |
| Enter/blur deduplication and stale callback suppression | Filename controller generation/attempt tests |
| Starts await affected edits and do not use failed drafts | Queue action and filename controller tests |
| Snapshot recovery restores availability without masking failed required listeners | Session/page lifecycle tests |
| Error attempt deduplication, repeated failures, automatic read, disposal | Error reporter/inbox and page lifecycle tests |
| Health refresh preserves runtime-update failures and clears stale progress | Runtime workflow regressions |
| Rejected dialogs reach Settings; cancellation preserves settings | Settings/diagnostics workflow regressions |
| Waiting rename versus worker claim, durability, rollback, restart, order | Rust queued-rename regressions and real local-media fixture |
| Playlist admission, cancellation, retry, settings, updates, and reconciliation | Fifteen existing renderer workflows adapted to the live UI |

Browser fixtures execute the real renderer with mocked IPC. Native fixture runs
exercise actual Rust, subprocesses, output bytes, and restart; the two are
reported separately in [Clarity QC](clarity-0.7.9.md). No claim of native Windows
150% display qualification is made from browser emulation.

## Historical pre-Clarity behavior and comparisons

The historical comparison application is `df582df4ddc566712729b7006dfb080374c5ef45`; the 2026-09-13 follow-up uses `242d3270017dcb2c50631d9327e599b8d1ce5023` as its original application. This checklist describes executed renderer fixtures and the existing unit tests that constrain ownership extraction. Renderer commands are mocked; passing these fixtures does not prove native subprocess, filesystem, installer, or extractor behavior. The matching-harness comparison for that earlier interface passed as described below; it does not constrain the deliberately redesigned Clarity layout.

The independent renderer cases in `nuclear-app/e2e/browser/renderer-workflows.e2e.mjs` passed together on Chrome 152.0.7977.76. Receipt: `target/engineering-structural-workflows-100.log`. All 15 cases passed, including the batch playlist workflow and bounded 100/1,000-row admission presentation checks.

| Observable workflow                                                                                                                    | Renderer case                                                                    | Complementary unit owner                                                              |
| -------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Inspection admission, queue insertion, download priority, progress, Cancel All, item cancellation, retry, removal, and reload recovery | `covers queue actions, cancellation, retry, removal, and reload reconciliation`  | `queue-logic.test.ts`, `operation-reducer.test.ts`, `operation-wait-registry.test.ts` |
| Rejected item cancellation restores the row and displays existing diagnostics                                                          | `restores a running row and exposes diagnostics when item cancellation fails`    | `operation-reducer.test.ts`                                                           |
| Inspection cancellation sends the admitted ID and clears loading without a failure message                                             | `cancels an in-flight inspection without displaying a failure`                   | `operation-wait-registry.test.ts`                                                     |
| Playlist confirmation makes one backend-authoritative batch admission using original selected indices, then projects pending rows with retained display metadata | `covers backend-authoritative playlist batch admission` | `inspection-workflow.test.ts`, `playlist-metadata-owner.test.ts`, `queue-presentation.test.ts` |
| Batch confirmation renders 100 and 1,000 pending rows within the browser-clock limits while keeping the DOM virtualized | `confirms and renders 100 pending playlist rows within 1000ms`; `confirms and renders 1000 pending playlist rows within 2000ms` | `page-queue-components.test.ts`, `queue-presentation.test.ts` |
| Filename validation, extension/reserved-name sanitization, Enter, Escape, and blur commit                                              | `covers filename sanitizing plus Enter, Escape, and blur editing`                | Backend filename/publication regressions remain authoritative for destination safety. |
| Existing quality/format and output/cookie/compatibility path controls retain command payloads                                          | `covers settings and output, cookie, and compatibility paths`                    | `ipc-client.test.ts`, `queue-logic.test.ts`                                           |
| Diagnostic export/clear and persistence-degraded display                                                                               | `covers diagnostics and persistence degradation`                                 | `backend-state.test.ts`, `startup-state.test.ts`                                      |
| Explicit backend resync requests refresh the rendered queue                                                                            | `reloads the authoritative snapshot when the backend requests resynchronization` | `app-state-controller.test.ts`, `state-reconciler.test.ts`                            |
| Queue-setting rejection reloads authority; diagnostic export rejection uses the existing error display                                 | `shows focused command failures and reconciles failed queue settings`            | `ipc-client.test.ts`, `app-state-controller.test.ts`                                  |
| Runtime checks, progress error presentation, operation settlement, and ready display                                                   | `covers runtime refresh and runtime update completion`                           | `startup-state.test.ts`, `operation-wait-registry.test.ts`                            |
| App-update details, version payload, progress error, completion, modal initial focus, Escape, inert background, and focus restoration  | `covers app update details and completion`                                       | `accessible-dialog.test.ts`, `operation-wait-registry.test.ts`                        |

Each case starts with a fresh renderer mount and restores the prior IPC mocks. Tests use real UI interactions and public mocked commands/events; they do not reach into Svelte component internals. Filename text replacement uses actual keyboard selection and typing because WebDriver's clear-value operation can trigger the application's blur commit.

The current source ownership inventory identifies 451 callables across 34 production files. `frontend-source-inventory.json` and `frontend-ownership.md` describe the current source owners. This is compiler-backed discovery, not a completed method review. New owners introduced during extraction must receive explicit responsibility mappings and regression links, and changed source identities require renewed review.

The final frontend receipt at `target/engineering-final-frontend-test.log` is 214 passed with one opt-in soak skipped. Svelte checking, strict lint, format checking, and the production bundle passed. The current browser workflow receipt is 15 passed. The renderer two-hour run passed under `target/renderer-soak/20260913T210906Z-5d8475e67a4e414f851090988aa723fe`: 7,200,050 ms, 318,745 mounts, 63,749 cycles of each of five workflows and playlist resynchronization, and 110 unchanged inputs. Both exact owned test processes exited. Heap readings, including 74,344,744 bytes after final GC versus 66,712,416 initially, are observational; this is jsdom/mock-IPC lifecycle evidence, not a native memory qualification.

## Playlist batch and authority checklist

- `inspection-workflow.test.ts` covers one batch IPC call, original retained-entry indices, immutable settings, stable request IDs across response-loss retries, failed admission retry, cancellation boundaries, duplicate selections, and parent inspection dismissal only after successful admission.
- `playlist-metadata-owner.test.ts` covers request-scoped retention limits, receipt item ownership, duplicate identity handling, authoritative absence, removal, and disposal.
- `queue-presentation.test.ts` covers pending inspection projection, completed inspection readiness, terminal failures, stale/new-attempt rejection, reload/resync projection, late completed metadata, response-loss retry metadata, progress precedence, and filename late replies.
- `queue-actions.test.ts` covers ready-only download filtering, retry of failed preparation, cancellation through the latest inspection operation, remove-selected behavior, and filename command ownership.
- `app-state-controller.test.ts` and `state-reconciler.test.ts` cover listener-before-snapshot ordering, buffered deltas, stale delta rejection, sequence-gap reload, explicit resync, bounded retry, and disposal of late work.

## Mounted component boundaries

`page-queue-components.test.ts` adds four actual-page regressions using 1,000 queue items: selection after scrolling addresses the correct record, filename focus and Enter editing reach the correct offscreen record, removal clamps the viewport, and resize observation updates the rendered window. These passed before the queue-row/table moves and continue in every component gate.

`page-playlist-dialog.test.ts` adds two actual-page regressions using 205 playlist entries. They verify 100/100/5-entry pages, first/last-page controls, global-index selection retained across page changes, select-all indeterminate state, initial dialog focus, Escape dismissal, and focus restoration. The baseline passed before moving the playlist dialog (`target/internal-cleanup-stage4/playlist-baseline.log`). Its first focus query matched both the backdrop and close button; scoping the test query to the dialog corrected that test ambiguity without changing the application.

## Visual and keyboard matrix

The capture spec covers ten states at 800×500, 1000×700, and 1440×1000 CSS pixels, with separate browser-scale 1 and 1.5 runs and two fresh captures per scenario. It records screenshots, displayed text, geometry, enabled/disabled controls, initial focus, and actual forward-Tab order. Modal layers are siblings of the application main element and must be included explicitly in semantic evidence.

Capture execution and comparator acceptance are separate gates. `frontend-visual-contract.md` defines the paired-run contract, input archives, decoded-pixel comparison, stable-repeat requirement, and deliberate mutation fixtures. The accepted result in `target/engineering-matched-visual-comparison.json` compares the matching baseline and candidate harness and passes all 60 scenarios with unchanged decoded pixels, geometry, text, control state, and focus. `nativeScalingQualified` remains false: browser scale emulation does not qualify real Windows display scaling or WebView2 rendering.

The unchanged 800-pixel-wide populated and persistence-degraded layouts expose a baseline quirk: the filename title button has a zero-width rectangle but remains enabled and reachable through real Tab navigation. The capture records both facts. A Tab-order entry must identify an enabled element; its initial screenshot visibility is compared independently and must not be inferred from focusability. This describes the old interface. Clarity replaces that layout and adds actual pointer-click rename coverage at both minimum and normal sizes.

## Timing evidence

`performance-acceptance.e2e.mjs` now supports 1, 100, and 1,000 queue items while preserving the existing 25 progress events per second workload and all existing thresholds. It retains raw samples, p50/p95/p99 distributions, an idle-renderer frame control before startup, and Chromium's limited JavaScript heap observation.

The initial historical baseline failed the frame-p95 threshold in its headless environment, and its idle control failed as well. That result remains evidence for the original environment rather than a statement about the current candidate. The current matched comparison completed all 18 runs: three repeats per side at 1, 100, and 1,000 items. All existing thresholds passed with matched source, harness, tool and environment identities. The accepted report is `target/engineering-matched-frontend-performance-comparison.json`; distributions and scope limits are recorded in [`engineering-performance-2026-09-13.md`](engineering-performance-2026-09-13.md).
