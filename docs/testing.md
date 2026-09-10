# Testing Nuclear Downloader 0.6.0

The test strategy deliberately separates renderer simulation from real Windows desktop acceptance.

## Renderer WebDriver suite

`npm run test:e2e:renderer` runs the real Svelte renderer headlessly in Chrome through WebdriverIO browser mode. It provides deterministic Tauri IPC responses for queueing, cancellation, event-gap snapshot reconciliation, playlist selection, diagnostics export/clear, and app/runtime update controls. The command requires `NUCLEAR_E2E_BROWSER_PROFILE_ROOT` and `NUCLEAR_E2E_BROWSER_PROFILE`; the profile must be a regular, immediate child of the root so the suite never uses a personal Chrome profile.

The renderer waits for mock registration only when Vite compiles with mode `webdriver`. `npm run build` is always a normal production build, and `npm run test:e2e:production-bundle` fails if the resulting files contain the WebDriver startup gate or mock-registry tokens.

This fast suite is not a substitute for desktop testing: there is no Rust process in browser mode.

## Native Windows x64 suite

`npm run test:e2e:native` drives the executable named by `NUCLEAR_E2E_APP_BINARY` through the official external `tauri-driver`. No WebDriver or evaluation plugin is compiled into Nuclear Downloader. The protected candidate workflow installs the exactly pinned `tauri-driver` 2.0.6 and verifies a Microsoft-signed EdgeDriver compatible with the runner's WebView2 build. Native acceptance runs under a verified Medium-integrity token, not the elevated GitHub runner token; see the [release process](release-process.md). Run `pwsh -NoProfile -File scripts/test-windows-user-process.ps1` from the repository root to exercise that launcher independently, without touching application data or installing the candidate.

Native workflows wait for Add to become usable before selecting formats or submitting links; seeing the heading alone does not prove hydration, runtime probes, folder validation, or state reconciliation have finished. They require a newly added row and a nonempty, newly published MP3 after conversion. They also cancel a live item and use the visible **Retry** action, requiring a new operation and exactly one published MP4. Failed CI tests retain bounded startup, runtime, and alert text without recording the link field or output path.

The full native suite requires:

- `NUCLEAR_E2E_FIXTURE_URL`, `NUCLEAR_E2E_SLOW_FIXTURE_URL`, and `NUCLEAR_E2E_PLAYLIST_FIXTURE_URL`, served only from the acceptance runner;
- `NUCLEAR_E2E_FIXTURE_FILE`, the runner-owned media used by the loopback playlist fixture;
- `NUCLEAR_E2E_FIXTURE_TITLE` and `NUCLEAR_E2E_RESTART_TITLE`, used to prove reload and interrupted-process journal recovery.

The exact-candidate runner generates bounded media with the candidate's own FFmpeg, hosts it on loopback, installs the exact NSIS bytes into an isolated directory, exercises real download/conversion/cancellation/reload/diagnostics paths, starts a second process to verify journal recovery, starts the exact portable bytes, silently uninstalls, verifies user data retention, and re-verifies every candidate hash.

## Windows 11 manual evidence

The protected candidate runner is automated Windows Server evidence. When paired protected URL/ID variables are configured, it may exercise maintainer-controlled YouTube and X fixtures, recording only case IDs and opaque fixture IDs. The pairs are `NUCLEAR_E2E_YOUTUBE_FIXTURE_URL`/`NUCLEAR_E2E_YOUTUBE_FIXTURE_ID` and `NUCLEAR_E2E_X_FIXTURE_URL`/`NUCLEAR_E2E_X_FIXTURE_ID`; a half-configured pair fails. IDs match `^[a-z0-9][a-z0-9._-]{0,127}$`. URLs are HTTPS-only and limited to YouTube (`youtube.com`, `www.youtube.com`, `youtu.be`) or X (`x.com`, `www.x.com`, `twitter.com`, `www.twitter.com`) hosts. URLs reach only the child process environment. Process output stays in an owned temporary workspace until every retained log has been redacted and the full evidence set has been scanned for the configured URL bytes. Only that sanitized copy enters the candidate artifact directory; redaction failure leaves no uploadable evidence directory. Missing pairs remain explicit, and `extractorQualificationStatus` becomes `complete` only after both extractors pass. Overall automated `qualificationStatus` remains `incomplete` because the seven manual records are still required. Publication requires a separate Windows 11 client record produced by `scripts/write-windows-manual-acceptance.ps1` and checked by `scripts/verify-manual-acceptance-evidence.ps1`. The record covers the clean installer and portable build, maintainer-controlled YouTube and X fixtures, dedicated-account cookie import, signed app update, and signed managed-runtime update/rollback.

Every case is `passed` and carries its fixed case ID, operator, canonical UTC completion time, and the SHA-256 of the exact candidate inventory. Case details bind the installer, portable archive, signed app manifest, signed runtime descriptor, and runtime archive as appropriate. The environment records a Windows `Client` installation with build 22000 or newer, x64 architecture, WebView2, app, managed-runtime, and packaged tool versions. Fixture IDs are opaque. Evidence must not contain URLs, cookies, tokens, account identifiers, or free-form notes.

`scripts/test-e2e-contracts.ps1` exercises the production writer and verifier with closed local fixtures. It rejects candidate/hash drift, missing or failed cases, invalid operators or times, URLs in fixture IDs, Windows Server even at a Windows 11-range build, old client builds, and mismatched WebView2/runtime versions.

## Structural-refactor evidence

The completed Stage 5 work has source-bound reports under `docs/internal-cleanup*`.
Its integrated candidate recorded 304 Rust tests, 167 frontend tests, 11
renderer workflows, 60 browser-emulated visual comparisons, matched
frontend/backend performance, and two fresh two-hour soaks. Later lifecycle and
inspection/playlist corrections have their own current-source short evidence:
332 Rust library tests with three opt-in harnesses ignored, 188 frontend tests
with one opt-in soak skipped, strict static/build checks, all 11 workflows, and
all 60 browser-emulated visual comparisons. See
[internal-cleanup.md](internal-cleanup.md) for the evidence boundaries and links.

The supporting runners are `scripts/run-renderer-check.ps1`,
`scripts/run-backend-performance.ps1`, `scripts/compare-backend-performance.ps1`,
`scripts/compare-frontend-performance.mjs`, `scripts/run-backend-soak.ps1`, and
`scripts/run-renderer-soak.ps1`. Performance comparisons require independently
frozen baseline and candidate inputs; a single run is not a comparison. The
normal soak duration is two hours. These scripts retain hashes, manifests, raw
samples, and cleanup results, but their receipts apply only to the exact source
and executable recorded in them.

This evidence does not qualify current `main` after later source changes. It
also does not qualify the native application, Windows 11 client behavior, real
100%/150% display scaling, live extractors, authenticated cookies, or signed
app/runtime updates. Those remain candidate-specific gates. The published
v0.6.0 assets contain yt-dlp 2026.07.04; current `main` pins 2026.08.19.

## Local commands

From `nuclear-app` on Windows x64:

```powershell
npm ci
npm test
$targetDirectory = Join-Path (Get-Location).Path 'target'
New-Item -ItemType Directory -Path $targetDirectory -Force | Out-Null
$targetItem = Get-Item -LiteralPath $targetDirectory -Force
if (-not $targetItem.PSIsContainer -or
    ($targetItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
  throw 'Renderer test target directory is ambiguous.'
}
$targetRoot = $targetItem.FullName
$profileRoot = Join-Path $targetRoot 'renderer-profiles'
New-Item -ItemType Directory -Path $profileRoot -Force | Out-Null
$profileRootItem = Get-Item -LiteralPath $profileRoot -Force
if ([System.IO.Path]::GetDirectoryName($profileRootItem.FullName) -cne $targetRoot -or
    $profileRootItem.Name -cne 'renderer-profiles' -or
    -not $profileRootItem.PSIsContainer -or
    ($profileRootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
  throw 'Renderer profile root is ambiguous.'
}
$profileName = "nuclear-renderer-$([Guid]::NewGuid().ToString('N'))"
$profile = Join-Path $profileRootItem.FullName $profileName
New-Item -ItemType Directory -Path $profile | Out-Null
$env:NUCLEAR_E2E_BROWSER_PROFILE_ROOT = $profileRootItem.FullName
$env:NUCLEAR_E2E_BROWSER_PROFILE = $profile
try {
  npm run test:e2e:renderer
} finally {
  $profileItem = Get-Item -LiteralPath $profile -Force
  if ([System.IO.Path]::GetDirectoryName($profileItem.FullName) -cne $profileRootItem.FullName -or
      $profileItem.Name -cne $profileName -or
      -not $profileItem.PSIsContainer -or
      ($profileItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'Refusing to remove an ambiguous renderer profile.'
  }
  Remove-Item -LiteralPath $profileItem.FullName -Recurse -Force
}
npm run build
npm run test:e2e:production-bundle
pwsh -NoProfile -File ..\scripts\test-e2e-contracts.ps1
```

Native and exact-candidate tests are intentionally not runnable without a real executable or verified candidate directory. The acceptance runner never signs, tags, uploads, or publishes anything.
