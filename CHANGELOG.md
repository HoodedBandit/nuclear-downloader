# Changelog

## v0.4.2 - 2026-04-17

- Smoothed the visible download percent and ETA display so both now refresh at a steadier 500 ms cadence instead of snapping multiple times per second.
- Kept the displayed download percent monotonic during active downloads so progress no longer jumps backward mid-transfer when `yt-dlp` emits noisy per-stage updates.
- Preserved the underlying download, scheduler, and cancellation behavior by keeping the fix entirely in the renderer display layer.

## v0.4.1 - 2026-04-11

- Improved large playlist loading by enabling lazy `yt-dlp` playlist enumeration and switching the backend parser to typed streaming deserialization.
- Removed the worst UI freeze when adding large playlist selections by batching queue-row insertion across animation frames instead of appending everything in one blocking update.
- Preserved the existing playlist picker flow, queue behavior, auth handling, and 5-active-download scheduler while making big imports materially faster and more responsive.

## v0.4.0 - 2026-04-11

- Fixed queue-wide cancellation so `Cancel All` now stops active work, clears queued starts, and prevents the scheduler from immediately restarting new downloads mid-cancel.
- Changed pending downloads to use the current cookie settings at the moment they start, which makes auth-required downloads more reliable after switching browsers or updating `cookies.txt`.
- Added in-place retry for failed and cancelled rows, along with clearer auth-specific error messages for X/Twitter guest-token failures and stale or locked cookie sources.
- Reused already-fetched playlist metadata when adding batches to the queue instead of re-querying every selected entry, reducing extractor churn and auth-related failure surface.
- Tightened `cookies.txt` validation so missing files fail early with a clear error before `yt-dlp` is spawned.

## v0.3.5 - 2026-04-08

- Fixed X and Twitter downloads that were failing with `Failed to query API` and `Bad guest token` errors by retrying through a safer `yt-dlp` fallback path.
- Reworked the queue scheduler to cap bulk downloads at 5 active items, keep queued rows editable, and reliably advance the next item after completion, cancellation, or errors.
- Hardened the desktop app with a real Tauri CSP, stricter backend request validation, `https`-only thumbnails, and removal of the unused opener capability.
- Reduced backend memory use by streaming playlist parsing, bounding stored stderr output, and hoisting progress regex compilation.

## v0.3 - 2026-04-08

- Replaced the app, taskbar, window, shortcut, and installer icon set with the new Nuclear artwork and rebuilt the Windows release as `0.3.0`.
- Switched the repository license and package metadata to the custom all-rights-reserved source-available terms.
- Updated the README and release-facing docs to clarify that Windows binaries are distributed through GitHub Releases, not stored in the source repo.
- Added the support link to the README and cleaned up the public release/distribution messaging.
