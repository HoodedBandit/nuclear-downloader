# YouTube 403 runtime correction

The user's Best-quality MP4 request for
`https://www.youtube.com/watch?v=ahE57sCR5pQ` failed during media transfer with
`HTTP Error 403: Forbidden`. The saved request had no playlist selection,
cookies, or compatibility configuration. Its metadata inspection had succeeded.

The bundled yt-dlp `2026.07.04` reproduced that failure. The official
`2026.08.19` executable downloaded the complete video with the same MP4 selector
and runtime configuration. Upstream documents the matching failure in
[issue 17456](https://github.com/yt-dlp/yt-dlp/issues/17456), resolved by removing
Android VR from the default YouTube clients in
[PR 17461](https://github.com/yt-dlp/yt-dlp/pull/17461).

## Change

- Pin yt-dlp `2026.08.19` in the sidecar manifest and verify the downloaded
  executable against the official release asset's SHA-256:
  `66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a`.
- Align the minimum recommended version and both runtime packaging defaults.
- Bind the stale-version regression to the production minimum, verifying that
  `2026.07.04` is stale and the replacement and later versions are accepted.

The dependency source is the official
[2026.08.19 release](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19).
The Windows executable is 17,840,399 bytes. Deno, FFmpeg, and ffprobe pins are
unchanged. The application keeps its existing selector, interface, queue state,
explicit retry policy, executable verification, and signed managed-runtime path.
Existing authenticated managed runtimes still take precedence over the bundle;
this host had only the runtime lock file, with no managed current runtime.

## Executed evidence

Live probes ran on 2026-09-10 UTC with Deno `2.9.2` and FFmpeg `8.1`, no cookies,
and the existing Best/MP4 selector. Outputs stayed in the workspace test directory.

| Case | Result |
| --- | --- |
| Full transfer with yt-dlp 2026.07.04 | Exit 1; HTTP 403; 3,826 ms |
| Full transfer with yt-dlp 2026.08.19 | Exit 0; same formats 137+140; 5,217 ms |
| Merged MP4 | 7,282,562 bytes; 238.840454 seconds; H.264 1920x1080 plus AAC |
| Complete FFmpeg decode with error-exit enabled | Exit 0; no decode errors |
| Earlier X multi-video inspection | Both expected Twitter media IDs and ordinals preserved |
| Rust library suite, all features, locked and offline | 332 passed; 0 failed; 3 opt-in harnesses ignored |
| Strict Clippy, all targets and features | Passed |
| Packaging and acceptance-evidence contract fixtures | Passed |
| Architecture/inventory fixture suite | 19 passed |
| Generated binding export tests | Passed in the Rust suite; no binding changes |

The X inspection retained media IDs `2049184998117588992` and
`2097430424205295616` at playlist indices 1 and 2. This checks the recently repaired
metadata shape against the updated dependency without adding those items to the
user's queue.

A tiny `--test` fragment download also succeeded with the old version, so it did
**not** detect this bug. The failure/success comparison above uses complete media
transfers; a fragment-only smoke test must not substitute for this regression.
These timings are individual observations, not a controlled performance benchmark.

Raw receipts, bounded probe logs, the official asset metadata, and the decoded
test file are retained locally under `target/youtube-403/`. Expiring media URLs
and raw verbose logs are not committed. The independent source review found no
active-default, hash, or selector inconsistency.

This evidence establishes the exact link's standalone downloader correction.
The rebuilt application's native queue retry remains a separate user test.
Installer/portable acceptance, controlled extractor coverage, cookies,
signed-update handoff, and the two-hour soak have not been repeated for this
candidate. Contract fixture success does not qualify those workflows. No push
or release publication is included.

## Native build and launch

The application was rebuilt offline from commit
`7a983a354b663fcac87122fbecd52974c6eb1fe7` with the embedded production frontend
and launched once at `2026-09-10T01:24:54Z`. Its responding Nuclear Downloader
window was verified. App executable SHA-256:
`c6ed6ba0a3ead33d784749fc0f78ecdc8d8286c5b516843d2f73569fe45ec091`.

All four adjacent runtime executable hashes matched the embedded lock manifest;
the adjacent yt-dlp version probe returned `2026.08.19`. No managed runtime was
present to override the bundle. The existing failed queue item was left for the
user's explicit retry. Build and launch receipts are
`target/youtube-403/native-build-result.json` and
`target/youtube-403/launch-result.json`.
