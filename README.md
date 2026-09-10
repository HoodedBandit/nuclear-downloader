# Nuclear Downloader

[![CI](https://github.com/HoodedBandit/nuclear-downloader/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/HoodedBandit/nuclear-downloader/actions/workflows/ci.yml)

A Windows desktop app for downloading video and audio from YouTube, X, and other
sites supported by yt-dlp. Paste a link, choose the items, format, and quality,
then download to your chosen folder.

[Download](https://github.com/HoodedBandit/nuclear-downloader/releases) ·
[Changelog](CHANGELOG.md) · [Documentation](docs/README.md) ·
[Report a bug](https://github.com/HoodedBandit/nuclear-downloader/issues/new/choose) ·
[Support the project](https://ko-fi.com/hoodedbandit)

## Download and run

**Target platform: Windows 11 x64.** Windows on ARM64 is not supported.

1. Open [GitHub Releases](https://github.com/HoodedBandit/nuclear-downloader/releases).
2. Choose the **x64 setup executable** for an installation, or the **x64 portable
   ZIP** and extract the entire archive into one folder.
3. Launch Nuclear Downloader. Paste a link, review its media, and add it to the queue.
4. Choose the output folder, format, and quality, then start the download.

Release bundles include the downloader and media tools. Using the app does not
require Node.js, Rust, or a command prompt. Microsoft Edge WebView2 is required
for the native window. The app's interface is embedded in its executable; it
does not run a public website or require a browser tab.

Keep the portable app and its adjacent tools together. A loose `nuclear.exe`
needs those tools or an authenticated managed runtime. The installed app can
check for published updates and hand off to a verified installer.

**Source versus release:** the latest published release is **0.6.0**. The
refactor and fixes described under **Unreleased** in the changelog are on
`main`; they are not included in those existing downloads. This includes the
yt-dlp `2026.08.19` YouTube fix. Publishing new installer and portable artifacts
is a separate, qualified release step.

## Features

- Single videos, supported playlists, and individual media from multi-video posts.
- MP4, MKV, and WebM video; MP3, FLAC, WAV, AAC, and Opus audio.
- Per-item format and quality choices, inline filenames, selection, and queue actions.
- Progress, speed, ETA, conversion status, cancellation, and explicit retry.
- Saved queues and recent operation history across restarts. Interrupted work stays
  paused until you choose to retry.
- Collision-safe output names that preserve files already in the destination.
- Optional browser cookies or a `cookies.txt` file for content your account can access.
- Runtime health, repair/update controls, app updates, and redacted diagnostics.

Five downloads can run concurrently, with one inspection and one explicit WebM
conversion at a time. Large queues use virtual rows and playlists use pages to
keep the interface responsive.

## What changed on main

The refactor preserves the existing interface and workflows while making their
ownership explicit:

- Each mounted page has one lifecycle owner. Late async results cannot update a
  disposed page, and renderer reloads do not cancel durable backend downloads.
- Queue presentation and workflow controllers are separated from the Svelte
  components that render them. Rust remains the authority for saved state.
- State commands, process supervision/output readers, staging, output resolution,
  and file publication have focused modules with source-matched reviews.
- Follow-up fixes address stale state events, filename-edit races, late cancellation,
  duplicate inspection work, X multi-video selection, and the reproduced YouTube 403.

See the [refactor record](docs/internal-cleanup.md) and
[current evidence guide](docs/README.md#validation-and-qualification) for executed
checks and their limits. Browser comparisons and earlier two-hour soaks are
recorded against specific candidates. Native Windows scaling, exact release
artifacts, cookies, and signed-update qualification remain separate requirements.

## Build from source

The source is available under the repository's [license](LICENSE); it is not
open-source. Development requires:

- Windows 11 x64 and PowerShell 7.
- Node.js **22.23.1** and npm **10.9.9**.
- Rust **1.94.1** through rustup, plus Visual Studio Build Tools with the C++ workload.
- Microsoft Edge WebView2; Python 3 for architecture and source-review checks.

From the repository root:

```powershell
pwsh -NoProfile -File .\scripts\fetch-sidecars.ps1
cd nuclear-app
npm ci
npm run tauri dev
```

The fetcher verifies the exact inputs recorded in
[`sidecars.lock.json`](nuclear-app/src-tauri/sidecars.lock.json). Third-party
executables are not stored in Git.

| Tool             | Current source pin |
| ---------------- | ------------------ |
| yt-dlp           | 2026.08.19         |
| FFmpeg / ffprobe | 8.1                |
| Deno             | 2.9.2              |

See [developer setup](docs/quickstart.md) for local builds and
[testing](docs/testing.md) for the complete checks, isolated renderer profiles,
visual comparisons, performance measurements, and native acceptance.

## Code and contribution guide

| Area                                        | Starting point                                                                                                        |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Page composition and lifecycle wiring       | [`+page.svelte`](nuclear-app/src/routes/+page.svelte)                                                                 |
| Frontend state, controllers, and components | [Frontend ownership map](docs/frontend-ownership.md)                                                                  |
| Rust commands and application wiring        | [`lib.rs`](nuclear-app/src-tauri/src/lib.rs)                                                                          |
| State, lifecycle, downloads, and updates    | [Backend ownership map](docs/backend-maintainability.md)                                                              |
| Behavior and regression obligations         | [Feature checklist](docs/backend-feature-preservation.md) and [frontend baseline](docs/frontend-behavior-baseline.md) |
| Method reviews and architecture rules       | [Review ledger workflow](docs/backend-method-review-schema.md)                                                        |
| Candidate construction and publication      | [Release process](docs/release-process.md)                                                                            |

Bug reports should include the app/runtime version, source site, selected
format/quality, reproduction steps, and redacted error details. Never attach
cookies, tokens, account credentials, or private media links. See
[CONTRIBUTING.md](CONTRIBUTING.md) before proposing code changes.

## Site support and responsible use

Site support follows yt-dlp and can change when a site changes. Login-required,
private, or region-restricted media may require valid cookies and may still be
unavailable. Supporting an extractor does not guarantee every link will work.

Use the app only for content you have the right to access and download, in
accordance with applicable laws and platform terms. Do not use it to infringe
copyright or bypass access controls you are not authorized to bypass.

## License

All rights are reserved. You may not use, copy, modify, or distribute the source
without explicit written permission from the author. See [LICENSE](LICENSE).
