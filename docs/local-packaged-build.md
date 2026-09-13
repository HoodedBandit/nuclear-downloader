# Local packaged Windows build

Use the local packaged-build command when you need reviewable Windows installer
and executable bytes from the current checkout:

```powershell
pwsh -NoProfile -File .\scripts\build-local-app.ps1
```

The command requires the repository-pinned Node 22.23.1, npm 10.9.9, Rust
1.94.1, and the exact sidecars in `sidecars.lock.json`. It invokes Tauri's build
mode, which embeds the production frontend and uses the packaged custom protocol.
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

The current documented result is preflight and contract validation only. No
local package has been built, signed, launched, or accepted. The local command
uses Tauri's `--no-sign` mode and cannot replace the protected signed release
candidate workflow.
