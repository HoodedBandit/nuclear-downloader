# Preparation before native testing

Tested source: `a3fd3e767a8f555cc797ec8e6d05a4823d806758`, including frontend fix `b2f17f5c2de0a0541d10dbd5676feae7759b9cdd`. The [receipt](internal-cleanup-pretest-followup.json) records changed-source hashes, executed results, and retained evidence identities. Tests ran before committing; changed-source hashes and complete renderer input manifests were verified against the committed working tree afterward. Subsequent delivery changes only documentation.

This follow-up fixes three confirmed ownership races after the structural cleanup. It changes no Svelte markup, styles, command names, event names, generated bindings, journal schema, concurrency limits, or intended workflows. The frontend source inventory is refreshed; generation remains discovery, not substantive review.

## Findings and changes

| Finding | Correction | Regression evidence |
| --- | --- | --- |
| The reconciler rejected an old or duplicate delta, or buffered it during reload, but the controller still forwarded it to queue presentation beside the current snapshot. A row could receive stale metadata or disappear. | The reconciler reports whether a delta was immediately applied. The controller forwards only applied deltas; reload publishes the complete reconciled snapshot. | Four integrated controller/presentation cases cover accepted updates, stale upserts, duplicate removals, and buffering during reload. Three negative cases failed before the fix. |
| A delayed filename-save result could clear a newer edit or attach the old error to it. A stale explicit row callback could also submit another row's draft. | Save completion must still own the editor generation, row, and draft. Submission first checks that the callback's row owns the current editor. | Eight cases cover late success/failure, another row, changed drafts, cancelled/reopened same-row edits, stale callbacks with valid/blank drafts, and cancellation without a replacement edit. The initial completion cases and all three stale-callback cases failed before their corrections. |
| Cancellation could report `not_found` after an operation known to exist completed, deregistered, or was dismissed during cancellation. | One private policy handles `not_found` after the initial existence check. A fallible snapshot read confirms terminal or removed work; live operations with missing registration still fail. Initial unknown IDs and other errors retain their existing behavior. | Five cases cover terminal and dismissed updates, live missing registration, requested/deferred publication, and initially unknown IDs. The terminal/dismissed registration cases failed before the fix. |

The cancellation policy surrounds registration cancellation, the durable cancellation request, and terminal waiting. It adds no production pause hooks. Existing handling of a persistence error when completion has already won remains unchanged.

## Test synchronization correction

The first full Rust run passed 308 tests and failed the existing failed-installer-launch test. That test treated the durable `maintenance_active = false` snapshot as proof that the coordinator had already released admission. Production deliberately clears durable maintenance before awaiting the coordinator lease release.

The test now waits for actual admission, retries only `busy`, fails immediately for any other error, and checks the durable flag afterward. Its two-second deadline and all launch/finalization assertions remain. This is a test-only correction; production maintenance order is unchanged. The earlier failing run is retained.

## Executed checks

- Final frontend suite: 179 passed, one opt-in two-hour soak skipped. Svelte/TypeScript checking, strict ESLint, formatting, production build, and production test-hook exclusion passed.
- Final Rust library suite: 309 passed, zero failed, three long-running harness tests ignored. Cargo formatting and strict Clippy for all targets/features passed. Generated bindings remain unchanged.
- Nineteen backend inventory/architecture fixture tests, the architecture check, all 1,223 source-matched backend review entries, and the packaging/acceptance-evidence contract checks passed. The frontend inventory matches 419 callables across 31 production files.
- The pinned browser workflow rerun passed all 11 cases. All 60 visual scenarios matched the original baseline with zero comparison errors, including decoded pixels and semantic evidence at both browser-emulated scales. See [visual comparison](internal-cleanup-pretest-visual.json). This does not qualify native Windows scaling.

Raw logs are retained under `target/pretest-followup/`. The first backend red run also exposed two fixture cleanup failures from retained owners; the corrected fixtures explicitly release those owners before removal. Those failures are not counted as successful negative regressions.

The first browser attempt ran no tests. Chrome's GPU child repeatedly exited with Windows `STATUS_ACCESS_DENIED`, and Chrome terminated itself. The same pinned browser and harness passed the workflow suite outside the execution sandbox using a fresh application-owned profile. No browser flags, baselines, Windows settings, or personal browser data were changed to obtain that result.

## Qualification boundary

The performance comparisons and two-hour soaks recorded for `fd58050b054f921d315658880a14793fb93fcae6` remain evidence for that earlier candidate. They have not been rerun against these corrections. This follow-up's short regression checks do not transfer that candidate's qualification to new source bytes.

Native Windows 11 installer/portable testing, real display scaling, controlled extractor fixtures, cookie cases, and authentic signed-update acceptance remain deferred. The separately requested production npm audit remains pending approval. No native application or installer was launched, no VM was provisioned, and no push or release publication was performed during this follow-up.
