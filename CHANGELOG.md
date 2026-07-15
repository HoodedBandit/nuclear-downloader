# Changelog

## v0.5.4 - 2026-07-15

This release focuses on predictable behavior under concurrent use, safer Windows file handling, and a more reproducible release pipeline.

- Prevented stale inspection results when URLs are replaced or cancelled quickly by making inspection single-flight and cancellation-aware.
- Made large playlists safer to import by deduplicating entries by source URL, enforcing a 1,000-entry inspection limit, and paging the selector in groups of 100 without unstable row identities.
- Fixed queued downloads so filename, format, directory, cookie, and other edits made before launch are honored; queued jobs now resolve the current global quality setting when they actually start.
- Protected completed downloads from accidental replacement across every output format with isolated staging, cancellation cleanup, atomic no-overwrite publishing, and automatic `(2)`, `(3)`, and later suffixes when names collide—even across concurrent jobs.
- Hardened filename generation for Windows reserved device names, reserved extensions, invalid characters, trailing dots or spaces, and UTF-16 path-length constraints.
- Stopped the development server from terminating unrelated processes that already own its port, made Tauri build-configuration overlays merge safely, and added CI gates for frontend checks, regression tests, formatting, Rust tests, and Clippy.

## v0.5.3 - 2026-07-13

- Rebuilt download lifecycle management around supervised jobs, idempotent cancellation, Windows Job Objects, coordinated shutdown, and queue-wide cancellation that waits for child processes to exit.
- Serialized WebM conversions to prevent concurrent FFmpeg transcodes from exhausting memory or CPU, with a visible waiting-for-conversion phase in the queue.
- Made URL inspection extractor-driven instead of guessing playlists from URL patterns, and fixed non-YouTube playlist entries so valid source URLs are preserved without fabricated YouTube links.
- Fixed quality caps for MKV and WebM so fallback selectors can no longer silently exceed the requested resolution.
- Split fast local runtime health checks from GitHub update checks, bounded network and tool probes, and made runtime installs checksum-verified, same-volume, atomic, and rollback-safe.
- Hardened app updates by requiring the exact release checksum manifest and verifying the NSIS installer SHA-256 before launch; stale partial installers are cleaned automatically.
- Blocked runtime and app updates while inspections, queued starts, downloads, or conversions are active, and registered progress listeners before startup checks so early events are not lost.
- Updated bundled extraction tools to `yt-dlp 2026.07.04` and Deno `2.9.2`, added a self-contained portable ZIP and reproducible runtime packager, upgraded supported frontend dependencies, and added Rust and Vitest regression coverage for the new reliability paths.

## v0.5.2 - 2026-06-23

- Added a managed downloader runtime layer so release builds resolve `yt-dlp`, FFmpeg, FFprobe, and Deno deterministically from app-controlled locations instead of silently depending on user machine state.
- Updated the bundled YouTube runtime path to `yt-dlp 2026.06.09` with Deno-backed JavaScript extraction support, deterministic `yt-dlp` config flags, and clearer YouTube failure diagnostics.
- Fixed the runtime health check so FFmpeg and FFprobe are probed with their supported `-version` flag, preventing false `Runtime missing` states after install.
- Tightened MP4 format selection to prefer MP4-compatible streams and added copyable diagnostics for downloader/runtime failures.

## v0.5.0 - 2026-04-18

- Added a Windows auto-update flow that checks the latest stable GitHub Release from inside the app, downloads the published NSIS installer, and relaunches automatically after install.
- Hardened the updater path with SemVer tag normalization, strict NSIS asset matching, HTTPS installer URL validation, temp `.part` downloads, duplicate-install guards, and truncated-download detection before installer handoff.
- Improved misleading failure handling by stripping non-actionable DRM warning lines out of the surfaced download error when a real extractor or ffmpeg failure is also present.

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
