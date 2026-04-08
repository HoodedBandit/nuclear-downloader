# Nuclear Downloader

Nuclear Downloader is an easy-to-use Windows desktop app for downloading videos from YouTube, X (Twitter), and many other sites supported by `yt-dlp`.

It is built for people who want a simple desktop interface instead of memorizing command-line flags. Paste a URL, choose format and quality, optionally provide cookies for login-required downloads, and download to your chosen folder.

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

## Download and Run

Use one of the following:

- NSIS installer from `nuclear-app/src-tauri/target/release/bundle/nsis`
- Portable executable from `nuclear-app/src-tauri/target/release`

If you use the portable executable, keep it next to the bundled sidecar tools placed in the same release folder.

## Developer Setup

You need the following on Windows:

- Node.js 20+ and npm
- Rust stable toolchain via `rustup`
- Microsoft Visual Studio C++ build tools for Rust/Tauri native compilation
- Microsoft Edge WebView2 runtime

The project already declares the JavaScript and Rust dependencies in the repo.

This source repository intentionally does not include third-party binary dependencies. Development can use tools from your system `PATH`, and Windows release bundling expects you to provide local copies of these tools in `src-tauri/binaries` before building an installer:

- `yt-dlp`
- `ffmpeg`
- `ffprobe`

## Build from Source

See [docs/quickstart.md](docs/quickstart.md) for the full setup, development, and release build workflow.

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
