# Nuclear Downloader 0.6.0 release process

Nuclear Downloader 0.6.0 is a Windows x64-only release. ARM64 builds are not produced or supported. A release candidate is built once, tested as exact bytes, and later published without rebuilding. Do not commit, push, tag, upload a candidate, or publish a release without the maintainer's explicit approval for that step.

## Trust and distribution boundaries

- GitHub Releases is the distribution host.
- The NSIS installer is not Authenticode-signed. Windows SmartScreen reputation is therefore separate from updater authentication.
- App and managed-runtime descriptors are authenticated with the same Tauri signing key.
- The signing private key exists only in the protected `release-candidate` GitHub environment and in an encrypted offline backup. It must never be stored in the repository, an Actions artifact, a build log, or a release asset.
- The current public key and key ID are compiled into release binaries by `build.rs`. An optional second public key/ID pair implements a reviewed rotation window.
- The `production-release` GitHub environment has required maintainers but does not need the signing private key. Publishing consumes already-signed candidate bytes.
- Actions artifacts are private to repository users with Actions access. They are candidates, not public releases.

Configure these non-secret GitHub repository variables. The candidate and production environments must not shadow them with different values:

- `NUCLEAR_UPDATE_KEY_ID`
- `NUCLEAR_UPDATE_PUBLIC_KEY`
- `NUCLEAR_UPDATE_NEXT_KEY_ID` (empty outside a rotation window)
- `NUCLEAR_UPDATE_NEXT_PUBLIC_KEY` (empty outside a rotation window)

Key IDs use 1-64 ASCII letters, digits, `.`, `_`, or `-`. The optional next ID and public key must either both be empty or both be set, and the two IDs must be distinct. Repository variables are appropriate because public keys and IDs are public trust anchors, not credentials. Using one shared repository-level value also prevents the candidate and publish environments from silently verifying different trust sets.

For each public-key variable, copy the exact single-line base64 value written by the pinned Tauri CLI `.pub` file. Do not decode or re-encode it. Tauri's CLI similarly writes detached `.sig` files as a single-line base64 wrapper around Minisign text; the app and release verifier strictly decode that outer wrapper before Minisign verification.

Configure the `release-candidate` environment with required reviewers, deployment-branch restrictions, and only these environment secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

Configure `production-release` with required reviewers and deployment-branch restrictions. Protect both workflow files with normal branch review. See [key-rotation.md](key-rotation.md) for private-key custody, backup, and rotation.

## Exact 0.6.0 public assets

The publish workflow accepts exactly these application files:

- `Nuclear.Downloader_0.6.0_x64-setup.exe`
- `Nuclear.Downloader_0.6.0_x64-portable.zip`
- `nuclear-downloader-v0.6.0-update.json`
- `nuclear-downloader-v0.6.0-update.json.sig`
- `nuclear-downloader-v0.6.0-sha256.txt`
- `SHA256SUMS`

It also accepts exactly these managed-runtime files, where `VERSION` has three numeric components:

- `nuclear-downloader-runtime-windows-x64.json`
- `nuclear-downloader-runtime-windows-x64.json.sig`
- `nuclear-downloader-runtime-VERSION-windows-x64.zip`
- `nuclear-downloader-runtime-VERSION-windows-x64.zip.sha256`

`nuclear-downloader-v0.6.0-sha256.txt` and the runtime `.zip.sha256` are the bounded compatibility bridge for older clients. Version 0.6.0 and later require the signed JSON contracts. The runtime descriptor also binds the exact SHA-256 of the archive's sole root `runtime-manifest.json`; both candidate verification and runtime installation enforce that inner binding. `SHA256SUMS` covers every other public asset. The private `release-candidate-inventory.json` additionally records every public asset's exact size and hash, the source commit, the key ID, and toolchain versions; it is not published.

The app update manifest is UTF-8 without a byte-order mark. Its exact bytes are signed, and its schema is:

```json
{
  "schemaVersion": 1,
  "keyId": "maintainer-assigned-key-id",
  "version": "0.6.0",
  "platform": "windows-x86_64",
  "publishedAt": "2026-08-17T12:00:00Z",
  "installer": {
    "fileName": "Nuclear.Downloader_0.6.0_x64-setup.exe",
    "size": 123,
    "sha256": "64 lowercase hexadecimal digits"
  }
}
```

Do not reformat either signed descriptor after signing it.

## Gate 1: local verification

Use Node.js 22.23.1, npm 10.9.9, and Rust/Cargo 1.94.1 on Windows x64. From a clean worktree, run:

```powershell
cd nuclear-app
npm ci
npm run format:check
npm run lint
npm run check
npm test
npm run test:e2e:renderer
npm run build
npm run test:e2e:production-bundle
npm run audit:production
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --all-features
cargo deny --manifest-path src-tauri/Cargo.toml check
cargo fetch --manifest-path src-tauri/Cargo.toml --locked
cd ..
pwsh -NoProfile -File scripts/test-packaging.ps1
pwsh -NoProfile -File scripts/test-e2e-contracts.ps1
git status --short
```

The last command must show no unexplained changes. This local gate does not sign, upload, tag, or release anything.

## Gate 2: private release candidate

After the maintainer explicitly approves pushing the exact commit:

1. Push that commit normally. Do not create a release tag yet.
2. Manually dispatch **Release Candidate**.
3. Set `release_version` to `0.6.0`.
4. Set `runtime_version` to the approved three-component runtime version.
5. Optionally supply a canonical UTC `published_at` value such as `2026-08-17T12:00:00Z`. If omitted, the builder records its current UTC time.
6. Enter the exact confirmation `BUILD v0.6.0`.
7. Approve the protected `release-candidate` environment after reviewing the commit and inputs.

The workflow reruns the complete gate, fetches only checksum-locked x64 sidecars, builds NSIS and portable outputs with the configured public trust set compiled in, packages the runtime, signs the exact app/runtime manifest bytes using `npm exec tauri signer sign`, creates checksums and the private inventory, then cryptographically verifies both detached signatures with the corresponding configured public key. Its only upload is the private Actions artifact `nuclear-downloader-0.6.0-candidate`. Record the successful Actions run ID.

After the candidate is built, the same protected job installs pinned external `tauri-driver` 2.0.6 and runs `scripts/run-windows-candidate-acceptance.ps1`. No WebDriver plugin is compiled into the app. The runner installs and exercises the exact candidate bytes, then uploads `nuclear-downloader-0.6.0-acceptance` as a separate private evidence artifact. Browser-mode mocked renderer coverage and native desktop results are distinct in the evidence; browser mocks are never described as native acceptance. See [testing.md](testing.md).

The builder refuses a dirty worktree, version drift among npm/Tauri/Cargo metadata, an output path outside Cargo's target directory, existing candidate output, reparse-point traversal, wrong artifact names, oversized files, and manifest/hash mismatches. It never prints the signing key or password.

## Gate 3: exact-byte acceptance

Download the private candidate artifact from the successful run. Preserve the archive and its run ID. Test those exact bytes, not a local rebuild:

1. Clean Windows x64 install and first startup.
2. Fixture download and conversion.
3. Cancellation, process-tree cleanup, and restart.
4. Renderer reload and backend-state reconciliation.
5. Managed-runtime install, update simulation, rollback, and restart.
6. Portable ZIP startup.
7. Diagnostics export and clear.
8. Uninstall and retained-data behavior.
9. Manual authenticated/cookie testing with a dedicated account. Never place cookies in CI secrets or artifacts.

The automated exact-byte runner covers items 1-4, portable startup, diagnostics clear, uninstall, retained data, and post-test hash verification using locally generated deterministic media. Diagnostics export uses the deterministic renderer suite because the native save dialog is outside the WebDriver DOM. Managed-runtime update/rollback using protected signed test assets and the dedicated-account cookie test remain explicit maintainer acceptance items; the generated acceptance JSON records them as required and cannot authorize publication by itself.

Record the candidate run ID, source commit, artifact hashes, toolchain versions, test results, operating-system build, and the maintainer's acceptance decision. Any failed acceptance item returns the release to development; produce a new commit and a new candidate run rather than changing the existing candidate artifact.

## Gate 4: protected publication

Only after exact-byte acceptance and explicit maintainer approval:

1. Manually dispatch **Publish Release**.
2. Supply the successful `candidate_run_id`.
3. Keep `release_version` exactly `0.6.0`.
4. Enter the exact confirmation `PUBLISH v0.6.0`.
5. Approve the protected `production-release` environment.

The publish workflow verifies that the selected run is a successful first-party **Release Candidate** workflow, checks out its recorded commit, downloads both the candidate and acceptance artifacts from that exact run ID, and validates that the evidence binds the source commit, candidate creation time, Windows x64 platform, every asset size/hash, and every automated acceptance result. The maintainer must also enter `COOKIE AND RUNTIME ACCEPTED v0.6.0`, recording that the two deliberately non-CI tests were completed before the protected production approval. It then fetches the exact `minisign-verify` 0.2.5 source pinned in `Cargo.lock` to crates.io checksum `22f9645cb765ea72b8111f36c522475d2daa0d22c957a9826437e97534bc4e9e`. Verification resolves offline with `--locked`, checks the registry source and checksum lock entry, and reruns the complete structural/hash/inventory and detached-signature verification. It compiles only a temporary zero-dependency signature-verification helper; it does not rebuild or sign any application, installer, portable, runtime, manifest, or release asset.

The workflow then creates a draft `v0.6.0` release targeted at the candidate commit, uploads only the ten inventoried public files, and compares every GitHub asset name and size with the private inventory. Where GitHub supplies an asset digest, it must also match. Only after that check does the workflow publish the draft and mark it latest. A failed check leaves a private draft for maintainer inspection; it must never be made public manually without resolving the discrepancy.

## Immutability and recovery

Published assets are immutable. Do not delete, replace, or upload a second file under the `v0.6.0` release. If 0.6.0 is faulty, preserve it and ship a newly signed 0.6.1 through a version-adjusted, reviewed pipeline.

Do not run `gh release create`, `gh release upload`, or `git tag` from a local release workspace for this process. The protected publish workflow is the only publication path.
