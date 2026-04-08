# Quickstart

## What This Covers

This guide shows how to run Nuclear Downloader in development and build a Windows release.

## Prerequisites

Install these first:

- Node.js 20+ and npm
- Rust stable via `rustup`
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
- `nuclear-app/src-tauri/binaries` for bundled `yt-dlp`, `ffmpeg`, and `ffprobe`

## Install Dependencies

From `nuclear-app`:

```powershell
npm install
```

Rust dependencies are resolved automatically by Cargo during checks and builds.

## Required Bundled Tools

This repository does not ship third-party binaries.

For development, the app can use `yt-dlp` and `ffmpeg` from your system `PATH`.

For Windows installer or portable release builds, place these Windows binaries in `nuclear-app/src-tauri/binaries` yourself:

- `yt-dlp-x86_64-pc-windows-msvc.exe`
- `ffmpeg-x86_64-pc-windows-msvc.exe`
- `ffprobe-x86_64-pc-windows-msvc.exe`

At build time, Tauri packages them as sidecars for the application. Keep those files local; they are intentionally excluded from Git history.

## Run in Development

From `nuclear-app`:

```powershell
npm run tauri dev
```

This starts the Svelte frontend and launches the desktop app shell.

## Quality Checks

Run these before publishing changes:

```powershell
npm run check
npm run build
cd src-tauri
cargo check
cargo clippy -- -D warnings
```

## Build a Release

From `nuclear-app`:

```powershell
npm run tauri build -- --no-sign -b nsis
```

Useful outputs:

- Portable app binary in `src-tauri/target/release`
- NSIS installer in `src-tauri/target/release/bundle/nsis`

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
