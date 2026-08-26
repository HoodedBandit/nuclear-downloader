# Quickstart

## What This Covers

This guide shows how to run Nuclear Downloader in development and build a Windows release.

## Prerequisites

Install these first:

- Node.js 22.23.1 and npm 10.9.9
- Rust 1.94.1 via `rustup`
- Microsoft Visual Studio Build Tools with C++ workload
- Microsoft Edge WebView2 runtime

Confirm the toolchain:

```powershell
node --version
npm --version
rustc --version
cargo --version
```

## Repository Layout

The desktop app lives in `nuclear-app`.

Important directories:

- `nuclear-app/src` for the Svelte UI
- `nuclear-app/src-tauri` for the Rust/Tauri backend
- `nuclear-app/src-tauri/binaries` for local `yt-dlp`, `ffmpeg`, `ffprobe`, and Deno sidecars used in release bundling

## Install Dependencies

From `nuclear-app`:

```powershell
cd nuclear-app
npm ci
```

Rust dependencies are resolved automatically by Cargo during checks and builds.

## Required Bundled Tools

This repository does not ship third-party binaries.

For development, the app can use `yt-dlp`, `ffmpeg`, `ffprobe`, and Deno from your system `PATH`. Deno is optional but recommended for modern YouTube extraction.

Nuclear Downloader 0.6.0 supports Windows x64 only. ARM64 is unsupported. For
installer or portable release builds, use the exact Windows x64 binaries pinned
in `nuclear-app/src-tauri/sidecars.lock.json`:

- `yt-dlp-x86_64-pc-windows-msvc.exe`
- `ffmpeg-x86_64-pc-windows-msvc.exe`
- `ffprobe-x86_64-pc-windows-msvc.exe`
- `deno-x86_64-pc-windows-msvc.exe`

Fetch and verify them with:

```powershell
pwsh -NoProfile -File .\scripts\fetch-sidecars.ps1
```

At build time, the Rust build script verifies each file's source metadata,
version, license, architecture, filename, and SHA-256 before Tauri packages it.
Keep the binaries local; they are intentionally excluded from Git history.

To build the separately updatable, checksum-pinned runtime bundle used by official releases:

```powershell
.\scripts\package-runtime.ps1
```

## Run in Development

From `nuclear-app`:

```powershell
npm run tauri dev
```

This starts the Svelte frontend and launches the desktop app shell.

## Quality Checks

Install `cargo-deny` 0.20.2, then run the same local gates as CI before asking
for a candidate build:

```powershell
npm run format:check
npm run lint
npm run check
npm test
npm run test:e2e:renderer
npm run build
npm run test:e2e:production-bundle
npm run audit:production
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
cd ..\..
pwsh -NoProfile -File .\scripts\test-packaging.ps1
pwsh -NoProfile -File .\scripts\test-e2e-contracts.ps1
```

## Build a Release

Do not publish an ad-hoc local build. The protected release-candidate workflow
requires explicit maintainer approval, locked x64 sidecars, and the configured
manifest-signing key. It builds and tests the NSIS installer, portable ZIP,
runtime bundle, signed manifests, checksums, and candidate inventory. A separate
protected publish workflow verifies and publishes those exact bytes without a
rebuild. See [release-process.md](release-process.md).

Installed Windows builds verify the signed release manifest and exact installer
size/SHA-256 before handing the NSIS installer off. The updater never patches
application files in place.

## Compile Summary

Common commands:

```powershell
cd nuclear-app
npm ci
npm run tauri dev
npm run format:check
npm run lint
npm run check
npm test
npm run test:e2e:renderer
npm run build
npm run test:e2e:production-bundle
npm run audit:production
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
cd ..\..
pwsh -NoProfile -File .\scripts\test-packaging.ps1
pwsh -NoProfile -File .\scripts\test-e2e-contracts.ps1
```

## Using the App

Typical workflow:

1. Paste a supported video or playlist URL
2. Choose output format and quality
3. Pick an output folder
4. Add cookies if the site requires login
5. Start the download and monitor progress in the queue

## Cookies and Authenticated Downloads

For supported sites with login requirements:

- Use browser cookie import when available
- Or export a `cookies.txt` file and select it in the app

If a site blocks cookie extraction from a browser profile, close the browser first or use a cookie export file instead.

## Notes on Supported Sites

The app is built on `yt-dlp`, so support depends on the upstream extractor ecosystem. YouTube and X are primary examples, but many other sites may also work.

Do not assume every site or embedded player will work identically.

## Legal Note

Use this tool only when you have permission to access and download the content. Do not use it to violate copyright, terms of service, or DMCA restrictions.

## Source Code License

The repository source code is distributed under the custom terms in the root [LICENSE](../LICENSE). It is not an open-source license, and reuse or redistribution requires explicit written permission from the author.
