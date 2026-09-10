# Local Sidecars

This directory is intentionally kept out of Git for third-party binaries.

Nuclear Downloader supports Windows x64 only. ARM64 sidecars are not accepted.

If you want to prepare a Windows release candidate, use
`scripts/fetch-sidecars.ps1` to download and verify the exact inputs recorded in
`../sidecars.lock.json`. The Rust build also rejects missing, wrong-hash, or
wrong-architecture inputs before bundling:

- `yt-dlp-x86_64-pc-windows-msvc.exe`
- `ffmpeg-x86_64-pc-windows-msvc.exe`
- `ffprobe-x86_64-pc-windows-msvc.exe`
- `deno-x86_64-pc-windows-msvc.exe`

Current `main` pins yt-dlp 2026.08.19, FFmpeg and FFprobe 8.1, and Deno 2.9.2.
The published v0.6.0 runtime contains yt-dlp 2026.07.04. Treat the lock as
authoritative and do not substitute a same-named executable.

The application prefers an authenticated managed runtime, then integrity-checked
adjacent sidecars. Release builds do not fall back to `PATH`. Debug builds retain
a `PATH` fallback only when initialization finds no managed or bundled runtime;
an integrity error is not a fallback condition. Use the locked binaries here for
reproducible development.

Verify the reported runtime source, path, and version when testing a new bundled
sidecar, because an installed managed runtime takes precedence. Close the app
before replacing local sidecars, then restart: verified file leases protect the
executables and the runtime snapshot is cached for the process lifetime.
