# Nuclear Downloader

If you want to support my work, donations are welcome: [ko-fi.com/hoodedbandit](https://ko-fi.com/hoodedbandit)

Nuclear Downloader is an easy-to-use Windows desktop app for downloading videos from YouTube, X (Twitter), and many other sites supported by `yt-dlp`.

It is built for people who want a simple desktop interface instead of memorizing command-line flags. Paste a URL, choose format and quality, optionally provide cookies for login-required downloads, and download to your chosen folder.

Installed Windows builds can also check the latest stable GitHub Release from inside the app and reinstall automatically through the published NSIS installer.

## Features

- Download single videos and supported playlists
- Download from YouTube, X, and many other `yt-dlp`-supported sites
- Choose video quality and output format per item
- Rename queued files before download with inline title editing
- Extract audio-only downloads in common formats
- Use browser cookies or a `cookies.txt` file when a supported site requires login
- Track progress, speed, ETA, and per-download status in the desktop UI
- Build Windows releases that can package `yt-dlp`, `ffmpeg`, and `ffprobe` when you provide the sidecar binaries locally

## Windows Support

This repository currently targets Windows desktop builds.

## Dependencies

Required system dependencies on Windows:

- Node.js 20+ and npm
- Rust stable via `rustup`
- Microsoft Visual Studio Build Tools with the C++ workload
- Microsoft Edge WebView2 runtime

Required downloader/media tools:

- `yt-dlp`
- `ffmpeg`
- `ffprobe`

This source repository intentionally does not include third-party binary dependencies.

- For development, the app can use `yt-dlp`, `ffmpeg`, and `ffprobe` from your system `PATH`
- For Windows release bundling, place local copies in `nuclear-app/src-tauri/binaries`

## Download and Run

Prebuilt downloads are published on the GitHub Releases page:

- [GitHub Releases](https://github.com/HoodedBandit/nuclear-downloader/releases)

This source repository does not store release `.exe` files. If you want a ready-to-run installer or portable build, download it from Releases.

If you install Nuclear Downloader with the Windows NSIS installer, the app can later check GitHub Releases for updates and hand off to the latest published installer automatically. It does not patch files in place.

If you want to create your own local build instead, use the compile steps below.

## Developer Setup

The JavaScript and Rust package dependencies are declared in the repo. Install the system dependencies above first, then install the app packages from the `nuclear-app` directory.

## Compile From Source

Install the app dependencies:

```powershell
cd nuclear-app
npm install
```

Run in development:

```powershell
npm run tauri dev
```

Run checks:

```powershell
npm run check
npm run build
cd src-tauri
cargo check
cargo clippy -- -D warnings
```

Build a Windows release:

```powershell
cd ..
npm run tauri build -- --no-sign -b nsis
```

Release outputs:

- Portable app in `nuclear-app/src-tauri/target/release`
- NSIS installer in `nuclear-app/src-tauri/target/release/bundle/nsis`

For installer or portable bundling with sidecars, provide local copies of `yt-dlp`, `ffmpeg`, and `ffprobe` in `nuclear-app/src-tauri/binaries` first.

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
