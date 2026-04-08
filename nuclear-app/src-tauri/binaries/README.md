# Local Sidecars

This directory is intentionally kept out of Git for third-party binaries.

If you want to build a Windows release that bundles its downloader sidecars, place these files here locally before running the Tauri release build:

- `yt-dlp-x86_64-pc-windows-msvc.exe`
- `ffmpeg-x86_64-pc-windows-msvc.exe`
- `ffprobe-x86_64-pc-windows-msvc.exe`

Development can also use `yt-dlp` and `ffmpeg` from your system `PATH`.
