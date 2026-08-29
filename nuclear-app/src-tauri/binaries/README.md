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

Development can also use `yt-dlp`, `ffmpeg`, `ffprobe`, and Deno from your system `PATH`.
