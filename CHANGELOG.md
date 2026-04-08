# Changelog

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
