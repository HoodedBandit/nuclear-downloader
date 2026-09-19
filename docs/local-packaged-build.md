# Local packaged Windows build

Use the local packaged-build command when you need reviewable Windows installer
and executable bytes from the current checkout:

```powershell
pwsh -NoProfile -File .\scripts\build-local-app.ps1
```

The command requires the repository-pinned Node 22.23.1, npm 10.9.9, Rust
1.94.1, the exact sidecars in `sidecars.lock.json`, and the public updater trust
anchor variables `NUCLEAR_UPDATE_KEY_ID` and `NUCLEAR_UPDATE_PUBLIC_KEY`.
During a rotation, set `NUCLEAR_UPDATE_NEXT_KEY_ID` and
`NUCLEAR_UPDATE_NEXT_PUBLIC_KEY` together; otherwise leave both empty. Key IDs
must be canonical and distinct, and each public key must be a valid
Tauri-wrapped Minisign public key. These are public verification keys, not
signing secrets. The preflight records key IDs and SHA-256 hashes of the public
key strings without copying raw key text into the receipt. It invokes Tauri's
build mode, which embeds the production frontend and uses the packaged custom protocol.
It does not invoke the development server.

Each invocation creates a new marker-owned directory below
`nuclear-app/src-tauri/target/local-packaged-builds/`. The command prints the
absolute path of that run's `local-packaged-build-receipt.json`; use that exact
path when reviewing a result. Do not select a receipt or executable by basename.
The receipt binds the source commit, dirty-worktree state and digest, command,
tool versions, sidecar-lock hash, app version, and the size and SHA-256 of the
standalone GUI executable, each adjacent sidecar, and the outer NSIS installer
container. It also records that no launch or workflow qualification occurred.

The command fails before issuing a success receipt if versions or sidecars do
not match, a build directory already exists, a same-run receipt exists, the
payload version is wrong, the source changes during the build, or the build is
not the production Tauri configuration. It never deletes target directories.
The historically named
`nuclear-app/src-tauri/target/install-audit-v0.5.4/nuclear.exe` is the registered
active installation and is explicitly excluded from the local build root.
Preserve it, installed application files, personal data, and taskbar shortcuts.

An installer and its extracted inner `nuclear.exe` are different artifacts and
may have different hashes. This local command does not extract the NSIS payload,
so its receipt does not claim an embedded-executable hash or equivalence with the
standalone executable. Verify each artifact only against a receipt entry that
names those exact bytes. A successful `cargo build --release` is only a Rust
compile; it is not evidence of a packaged, ready GUI.

The receipt proves construction and byte identity only. It does not prove the
app launches or that downloads work. A future native smoke must start the exact
receipt-bound executable in an owned test profile, verify its version and hash,
exercise real controls and a real output path, and record the resulting files.
Window appearance alone is not a smoke test.

To check prerequisites and safeguards without building or launching anything:

```powershell
pwsh -NoProfile -File .\scripts\build-local-app.ps1 -PreflightOnly
```

The final local construction completed from clean source commit
`dd224013e8fe613fac98d41a399514ba75caeedd` as run
`local-0.7.1-20260913T223412Z-473c88b5838e4ff9b260b376a7bad6cc`. Its exact
receipt is
`nuclear-app/src-tauri/target/p/local-0.7.1-20260913T223412Z-473c88b5838e4ff9b260b376a7bad6cc/local-packaged-build-receipt.json`.
The receipt records version 0.7.1, target `x86_64-pc-windows-msvc`, an empty Git
status, and construction from 2026-09-13 22:34:13 UTC through 22:46:13 UTC. The
receipt-bound standalone executable is 20,692,992 bytes with SHA-256
`56fdc3c37c8fb6e2540c3c3f8b4fc30b9c5ac90092f8c3fd72be00a5ed3275b6`.
The separate outer NSIS installer container is 106,321,199 bytes with SHA-256
`c834f3289e7eb3b14f7d272a490bce4ad21ac9a35241f268c19ab171a54d2d83`.
Both files and all four receipt-bound sidecars were independently rehashed at
their exact receipt-relative paths and matched their recorded sizes and hashes.

This is construction and byte-identity evidence only. Native smoke remains
paused: neither the standalone executable nor installer was launched, and no
controls or download workflows were exercised. The build used Tauri's
`--no-sign` mode. The installer inner executable was not extracted or verified,
so no relationship between its hash and the standalone executable is claimed.
This local result does not modify the registered active installation and cannot
replace the protected signed release-candidate workflow.

The first attempt compiled dependencies for about nine minutes and then failed
because the required public updater trust-anchor variables were absent; its
preserved log is failure evidence only. A later successful package built before
the final source rename is superseded by the receipt above. Neither historical
attempt is current qualification evidence.

## Isolated UI preview

Use `scripts/build-local-app.ps1 -Preview` for the Clarity UI preview. This
selects the `local-preview` Cargo feature and `tauri.preview.conf.json` together.
The preview uses a separate Windows application identifier, window title,
queue journal, appearance preference, diagnostics directory, and tool cache.
App installer updates are disabled in this preview; download tool updates
remain available in its own cache. The normal build retains its existing paths.

Run the standalone executable with its four adjacent sidecars. No installer
execution or taskbar change is required. Light, Dark, and System appearance are
saved in the preview profile. Errors are collected into a bounded history of
the most recent 200 errors for the current session; reading Settings clears
the notification dot without deleting those entries. Download errors restored
with the queue are collected again when the application starts.
