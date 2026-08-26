# Nuclear Downloader

If you want to support my work, donations are welcome: [ko-fi.com/hoodedbandit](https://ko-fi.com/hoodedbandit)

Nuclear Downloader is an easy-to-use Windows desktop app for downloading videos from YouTube, X (Twitter), and many other sites supported by `yt-dlp`.

It is built for people who want a simple desktop interface instead of memorizing command-line flags. Paste a URL, choose format and quality, optionally provide cookies for login-required downloads, and download to your chosen folder.

Production UI assets are embedded in the Windows executable and run inside its native app window. Nuclear Downloader does not host a website or require a browser; localhost is used only by the development server.

Installed Windows builds can also check the latest stable GitHub Release from inside the app and reinstall automatically through the published NSIS installer.

## Features

- Download single videos and supported playlists
- Download from YouTube, X, and many other `yt-dlp`-supported sites
- Choose video quality and output format per item
- Rename queued files before download with inline title editing
- Extract audio-only downloads in common formats
- Use browser cookies or a `cookies.txt` file when a supported site requires login
- Track progress, speed, ETA, and per-download status in the desktop UI
- Build Windows releases that can package `yt-dlp`, `ffmpeg`, `ffprobe`, and Deno when you provide the sidecar binaries locally

## Windows Support

Nuclear Downloader 0.6.0 supports 64-bit Windows on x64 processors. Windows on
ARM64 is not supported; the app, installer, portable bundle, managed runtime,
and checked sidecars are all built for `windows-x86_64`.

## Dependencies

Required system dependencies on Windows:

- Node.js 22.23.1 and npm 10.9.9
- Rust 1.94.1 via `rustup`
- Microsoft Visual Studio Build Tools with the C++ workload
- Microsoft Edge WebView2 runtime

Required downloader/media tools:

- `yt-dlp`
- `ffmpeg`
- `ffprobe`
- Deno is recommended for modern YouTube extraction and is included in official release bundles

This source repository intentionally does not include third-party binary dependencies.

- For development, the app can use `yt-dlp`, `ffmpeg`, `ffprobe`, and optionally Deno from your system `PATH`
- For Windows release bundling, fetch the exact x64 inputs recorded in
  `nuclear-app/src-tauri/sidecars.lock.json`; the build rejects missing,
  wrong-hash, or wrong-architecture sidecars

## Download and Run

Prebuilt downloads are published on the GitHub Releases page:

- [GitHub Releases](https://github.com/HoodedBandit/nuclear-downloader/releases)

Use the NSIS setup executable for a normal installation. The self-contained Windows portable ZIP includes the app plus all downloader sidecars; the raw `nuclear.exe` expects a managed runtime or adjacent sidecars.

This source repository does not store release `.exe` files. If you want a ready-to-run installer or portable build, download it from Releases.

If you install Nuclear Downloader with the Windows NSIS installer, the app can later check GitHub Releases for updates and hand off to the latest published installer automatically. It does not patch files in place.

If you want to create your own local build instead, use the compile steps below.

## Developer Setup

The JavaScript and Rust package dependencies are declared in the repo. Install the system dependencies above first, then install the app packages from the `nuclear-app` directory.

## Compile From Source

Install the app dependencies exactly as locked:

```powershell
cd nuclear-app
npm ci
```

Run in development:

```powershell
npm run tauri dev
```

Run the local quality gate:

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

Official release candidates are built only by the protected, manually
dispatched workflow described in [docs/release-process.md](docs/release-process.md).
It verifies locked sidecars, builds the x64 NSIS and portable artifacts, signs
the exact app/runtime manifests, and records an immutable candidate inventory.
Publication is a separate maintainer-approved workflow that reuses those exact
bytes without rebuilding them.

For local sidecar preparation:

```powershell
pwsh -NoProfile -File .\scripts\fetch-sidecars.ps1
```

## More Detail

See [docs/quickstart.md](docs/quickstart.md) for the full setup, development, and release build workflow.

## License

This repository is source-available, not open-source.

All rights are reserved by the author. You may not use, copy, modify, or distribute this code without explicit written permission.

See [LICENSE](LICENSE) for the full terms.

## Login-Required Downloads

Some supported sites require login before media can be fetched. Nuclear Downloader supports:

- Browser cookie import for supported browsers
- Manual `cookies.txt` selection

If a site uses login walls, private media, or regional restrictions, you may need to provide valid cookies from an account that is allowed to access that content.

## Supported Sites

Nuclear Downloader supports sites that `yt-dlp` supports. That includes YouTube and X, along with many other platforms. Site support can change over time as upstream extractors change.

If a site is unsupported, broken, or requires credentials that cannot be exported cleanly, the app may not be able to download from it.

## Legal and Responsible Use

Use Nuclear Downloader only where you have the right to access and download the content.

Do not use it to:

- Infringe copyright
- Bypass access controls you are not authorized to bypass
- Violate platform terms of service
- Evade DMCA restrictions or other applicable law

You are responsible for complying with the laws and platform rules that apply to your jurisdiction and the content you download.
