# Documentation

Start with the [project README](../README.md) for downloads, supported platforms,
features, and development prerequisites. The latest published release is 0.6.0;
the refactor and recent downloader fixes on `main` are **unreleased**.

## Setup and maintenance

| Document                              | Use it for                                                                               |
| ------------------------------------- | ---------------------------------------------------------------------------------------- |
| [Quickstart](quickstart.md)           | Windows 11 x64 setup, pinned tools, local builds, and runtime resolution                 |
| [Testing](testing.md)                 | Unit, renderer, architecture, packaging, and native acceptance commands                  |
| [Contributing](../CONTRIBUTING.md)    | Bug reports, permission requirements, and change scope                                   |
| [Release process](release-process.md) | Protected candidate construction, exact-byte evidence, signing, and separate publication |
| [Key rotation](key-rotation.md)       | App/runtime verification keys and their rotation rules                                   |

## Code ownership

| Document                                                        | Scope                                                                                                   |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| [Frontend ownership](frontend-ownership.md)                     | Page lifecycle, queue presentation, workflow controllers, typed IPC, and Svelte components              |
| [Backend maintainability](backend-maintainability.md)           | Rust services, state commits, process/file ownership, runtime transactions, and architecture boundaries |
| [Frontend behavior baseline](frontend-behavior-baseline.md)     | Observable workflows and their independently repeatable tests                                           |
| [Backend feature preservation](backend-feature-preservation.md) | Features and invariants mapped to regression obligations                                                |
| [Method review schema](backend-method-review-schema.md)         | Source identities, substantive review requirements, and ledger checks                                   |
| [Reviewer records](backend-method-reviews/README.md)            | Individual backend review scopes and merge/check workflow                                               |
| [Visual contract](frontend-visual-contract.md)                  | Frozen fixture states, geometry, text, focus, pixels, and baseline policy                               |
| [Frontend validation tools](frontend-validation-tools.md)       | Matched performance evidence and renderer lifecycle soak tools                                          |

## Refactor and corrections

The [internal cleanup record](internal-cleanup.md) is the current stage summary.
The following records explain the changes without replacing their original
candidate hashes or test receipts:

- [Page lifecycle and waiter fixes](frontend-lifecycle-review.md).
- [Queue and workflow extraction](frontend-workflow-review.md).
- [Svelte component extraction](frontend-components-review.md).
- [State, process, and publication extraction](backend-internal-cleanup-review.md).
- [Stale events, filename editing, and cancellation follow-up](internal-cleanup-pretest-followup.md).
- [One-pass inspection and exact playlist media identity](inspection-playlist-fix.md).
- [YouTube 403 and verified yt-dlp refresh](youtube-403-fix.md).
- [Earlier backend reliability overhaul](backend-overhaul.md).

## Validation and qualification

Results belong to the source, executable, environment, and artifacts named by
their receipts. A passing result for an earlier candidate does not qualify a
later fix or a rebuilt installer.

| Evidence                                                                                                                                          | Tested scope                                                                                                                                                              |
| ------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [YouTube runtime correction](youtube-403-fix.md)                                                                                                  | `7a983a3`: 332 Rust tests, strict Clippy, packaging contracts, full download/decode of the reported link, verified local app build, and the maintainer's successful retry |
| [Inspection/playlist correction](inspection-playlist-fix.md)                                                                                      | `8466d7b`: 188 frontend tests, 11 renderer workflows, 60 browser-emulated visual comparisons, 332 Rust tests, and the recorded supporting checks                          |
| [Structural candidate qualification](internal-cleanup-stage5-qualification.md)                                                                    | `fd58050`: integrated checks, matched performance comparisons, and fresh two-hour backend/renderer soaks                                                                  |
| [Frontend performance](internal-cleanup-stage5-frontend-performance.md) and [backend performance](internal-cleanup-stage5-backend-performance.md) | Frozen 1/100/1,000-item comparisons for the structural candidate                                                                                                          |
| [Backend soak](internal-cleanup-stage5-backend-soak.md) and [renderer soak](internal-cleanup-stage5-renderer-soak.md)                             | Separate two-hour workloads, with source/executable identities and resource observations                                                                                  |
| [Backend finding-to-test checklist](backend-qualification.md)                                                                                     | Reliability regressions and external acceptance obligations                                                                                                               |

The structural work and its recorded local gates are complete. Exact current
installer/portable artifacts, real Windows 100%/150% display scaling, controlled
extractor fixtures, dedicated-account cookie cases, and authentic signed app/runtime
updates still need qualification. The later fixes have not received new two-hour
soaks. Production npm audit is configured in CI; its separate local run remains
unexecuted in the recorded qualification evidence.

The maintainer confirmed the reported YouTube Best/MP4 retry works in the rebuilt
application. That is one real workflow observation, not broad release acceptance.

Markdown and JSON evidence summaries are committed. Raw logs, screenshots, media,
and archived inputs referenced under `target/` are retained locally and ignored by
Git; those paths are not public downloads. Historical checkpoints such as
[the backend resume record](backend-resume-checkpoint.md) describe their original
stage, not the current completion state.
