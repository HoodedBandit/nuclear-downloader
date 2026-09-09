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

## Separate ownership correction

The mechanical extraction in `bda6138` preserved the original non-evicting metadata map and display timestamps. A separate correction now releases admission metadata on failure, transfers it into its newly projected row, and clears retained maps on page disposal. Each retained entry records already observed same-URL rows so an old row or a repeated snapshot cannot consume another admission's metadata. Discard closures affect only their own entry. Display cadence records belong to a row/operation pair and are removed on authoritative terminal state, row removal, or operation replacement.

The initial regression run failed three tests for missing admission cleanup, retained claimed metadata, and retained terminal display ownership. Review then demonstrated a fourth case: two same-URL admissions followed by repeated snapshots caused the first row to consume the second row's metadata. The dedicated overlap test failed with null metadata on the second row, then passed after observed IDs were carried into the remaining entries. Original and follow-up RED/GREEN logs are retained under `nuclear-app/target/internal-cleanup-stage3/`. The final focused suites passed **29 tests**; the integrated suite passed **161 tests**, with the opt-in long soak explicitly skipped. Type checking reported zero errors/warnings, strict lint and formatting passed. The next component comparison will also cover this candidate's renderer output.
