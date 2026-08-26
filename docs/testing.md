# Testing Nuclear Downloader 0.6.0

The test strategy deliberately separates renderer simulation from real Windows desktop acceptance.

## Renderer WebDriver suite

`npm run test:e2e:renderer` runs the real Svelte renderer in Chrome through WebdriverIO browser mode. It provides deterministic Tauri IPC responses for queueing, cancellation, event-gap snapshot reconciliation, playlist selection, diagnostics export/clear, and app/runtime update controls.

The renderer waits for mock registration only when Vite compiles with mode `webdriver`. `npm run build` is always a normal production build, and `npm run test:e2e:production-bundle` fails if the resulting files contain the WebDriver startup gate or mock-registry tokens.

This fast suite is not a substitute for desktop testing: there is no Rust process in browser mode.

## Native Windows x64 suite

`npm run test:e2e:native` drives the executable named by `NUCLEAR_E2E_APP_BINARY` through the official external `tauri-driver`. No WebDriver or evaluation plugin is compiled into Nuclear Downloader. The protected candidate workflow installs the exactly pinned `tauri-driver` 2.0.6 and lets the pinned WebdriverIO Tauri service obtain the Edge driver matching the runner's WebView2 installation.

The full native suite requires:

- `NUCLEAR_E2E_FIXTURE_URL` and `NUCLEAR_E2E_SLOW_FIXTURE_URL`, served only from the acceptance runner;
- `NUCLEAR_E2E_PUBLIC_SMOKE_URL`, a maintainer-controlled unauthenticated HTTPS media URL shorter than 15 seconds;
- `NUCLEAR_E2E_FIXTURE_TITLE`, used to prove journal recovery after a renderer reload.

The exact-candidate runner generates bounded media with the candidate's own FFmpeg, hosts it on loopback, installs the exact NSIS bytes into an isolated directory, exercises real download/conversion/cancellation/reload/diagnostics paths, starts a second process to verify journal recovery, starts the exact portable bytes, silently uninstalls, verifies user data retention, and re-verifies every candidate hash.

## Local commands

From `nuclear-app` on Windows x64:

```powershell
npm ci
npm test
npm run test:e2e:renderer
npm run build
npm run test:e2e:production-bundle
pwsh -NoProfile -File ..\scripts\test-e2e-contracts.ps1
```

Native and exact-candidate tests are intentionally not runnable without a real executable or verified private candidate directory. The acceptance runner never signs, tags, uploads, or publishes anything.
