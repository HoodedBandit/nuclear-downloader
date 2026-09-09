# Frontend workflow extraction review

Stage 3 moved the large workflow implementations out of `+page.svelte` while retaining the UI, state bindings, backend contracts, and async ordering. The page now composes state proxies and controllers once, installs lifecycle/event wiring, and forwards UI events to their domain owner.

## Original-to-owner boundaries

| Original `+page.svelte` responsibility | Current owner |
| --- | --- |
| Queue projection, progress throttling, selection, filenames, and row diagnostic visibility | `queue-presentation.ts` |
| Enqueue priority, downloads, cancellation rollback, retry, removal, and settings commands | `queue-actions.ts` |
| URL/playlist inspection, admission, captured settings, paging, and cancellation | `inspection-workflow.ts` |
| Runtime status, update/repair actions, progress, and startup state | `runtime-workflow.ts` |
| App version, update modal/check/install, progress, and blocking-work guard | `app-update-workflow.ts` |
| Output directory, global setting values, cookie/config paths, and diagnostic copy/export/clear | `settings-diagnostics-workflow.ts` |
| Cross-domain composition, mount/unmount, listeners, and markup forwarding | `+page.svelte` |

Shared contracts now live in `frontend-types.ts`, `frontend-errors.ts`, and `frontend-workflow-ports.ts`. Existing reducers, wait registries, backend derivations, startup state, and IPC typing retain their lower-level ownership.

## Mechanical fidelity review

The extraction was compared with the frozen Stage 2 page for command names and payloads, guards, defaults, error strings, dialog options, confirmation text, optimistic rollback, reload timing, `Promise.all` fanout, single-use inspection cleanup, update sequencing, and disposal guards. Independent boundary reviews found no intentional behavior change in the moved workflows.

The workflow/presentation work adds **54 focused tests**. Completed Stage 3 verification ran **154 frontend tests**, Svelte checking, ESLint, formatting, and the production build successfully. The production bundle excludes test hooks. All **11 browser workflows** passed in `target/renderer-checks/workflows-20260909T071111Z-4b05123a1d8142339d7672cb176279ce/`, with source and executable identities verified after execution. All **60 visual scenarios** passed exact decoded-pixel and semantic comparison against the frozen baseline, with two stable captures per scenario. `internal-cleanup-workflows-visual.json` binds the evidence to archived production hash `d14aec8c1d9e7d6b7fa468289d9f8e2c3d45b199f2ebdec925b926ce3d8a6d2c`. This passes the Stage 3 extraction gate; native Windows scaling and performance qualification remain open.

## Retained issue

`queue-presentation.ts` intentionally preserves the prior `metadataByUrl` behavior for mechanical fidelity. Inspection metadata is retained by URL and is not evicted when queue items disappear, so a long-running renderer that inspects many unique URLs can retain metadata for the page lifetime. Display timestamps also outlive rows removed through authoritative snapshots. These are pre-existing ownership defects. Their corrections will follow this mechanical extraction in a separate regression-backed change.
