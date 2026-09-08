# Backend maintainability reboot checkpoint

Saved 2026-09-08 on `codex/backend-maintainability` at the user's request to
checkpoint and stop before reboot. The checkpoint commit follows `ebc23f1`.
Do not resume implementation or tests until the user asks. No push or publishing
has occurred. All active review agents were interrupted; no Cargo/test/soak
session was running when this checkpoint was made.

## Scope and source state

Implement the approved Backend Maintainability and Complete Method Review plan.
Keep all existing features, UI workflows, 18 command names/arguments, five events,
journal schema 1 and concurrency limits (five downloads, one inspection, one
explicit conversion). Windows 11 x64, Rust/Tauri, no dependency upgrades. Keep
confirmed defect fixes separate from structural changes. Use Sol-only delegation.
Root coordinates all Cargo/test execution; no competing builds or source changes
during a gate or frozen soak. Use owned isolated test roots. Do not launch the app,
installer, or native acceptance against the user's personal desktop/data/profiles.
Stop immediately on executable loader errors; do not repeatedly relaunch them.

Stages 1-5 source implementation is complete. Stage 5 source is included in this
checkpoint, together with explicitly provisional method-review sidecars. Do not
mistake checkpointing drafts for completing their review.

Important earlier commits:

- `34d0769`: prior reliability-overhaul baseline.
- `08a840a`: completed Stage 2 module boundaries.
- `0d7f3ae`, `42d955e`, `e6a9bf3`: state data, reducers, durable commit ownership.
- `a455422`: Stage 3 gates and repeated performance evidence.
- `aa01bfd`: complete services, desktop/bootstrap separation, four regressions.
- `36dcc6b`: regression-backed finalizer panic compensation stays in its owned task.
- `ebc23f1`: sidecars must supply all generated identity fields, with regression.

Stage 5 extracts lifecycle admission/capacity/publication/task tracking; runtime
manifest/release/archive/promotion; transaction journal/OS lock/filesystem; and
updater release/network/installer modules. DOWNLOAD_CAPACITY is authoritative in
the lifecycle module and shared with the service worker loop. Runtime publisher
tests use an explicit isolated cache. Complete publisher guard ordering and
unconditional outer workspace cleanup remain intact.

Independent runtime review against `36dcc6b` found no behavioral blocker: all 15
cancellation checkpoints, transaction store/clear order, four OS-lock scopes,
cache/publication guard lifetimes and derive/serde/cfg attachment are preserved.
Independent updater review matched all 36 function headers/bodies, five structs,
14 constants and 20 tests, allowing required visibility/import changes.

Two nonblocking observations remain UNCHANGED at this checkpoint:

- Runtime release.rs and manifest.rs duplicate equal descriptor/signature limits;
  consider sharing the manifest constants to prevent future drift.
- runtime_transaction/filesystem.rs imports OpenOptions unconditionally although
  only the Windows identity verifier uses it. Windows Clippy passed; a Windows cfg
  on that import would avoid non-Windows unused-import warnings.

Do not rerun scratch extraction generators: some intermediate versions produced
orphan attributes or incomplete imports that were corrected in the final source.

## Executed validation before checkpoint

- Stage 5 Rust: 290 passed, zero failed, three explicitly ignored, 25.91 seconds.
  The initial extraction compile attempt failed on imports only; corrected before
  this successful run. The two new publisher regressions both passed.
- Strict Clippy: passed, 9.24 seconds. Formatting, generated-binding diff, actual
  architecture policy and whitespace checks passed. A final unused import was
  removed before Clippy. No binding/frontend product source changes.
- Frontend: formatting, lint, Svelte/TypeScript check (zero errors/warnings), all
  64 tests in ten files, production build and bundle test-hook exclusion passed.
- Inventory fixtures: 9/9; architecture fixtures: 8/8. The new mandatory source
  identity regression was observed failing before `ebc23f1`, then passing.
- Stage 4 executable acceptance-contract fixtures passed (isolated TEMP, closed
  fake CLI). Rerun after enabling the final ledger gate in CI/release checks.
- npm production audit: zero vulnerabilities, exit 0, using pinned npm 10.9.9.
- cargo-deny 0.20.2: advisories, bans, licenses and sources passed, exit 0 in
  8.85 seconds. Allowed duplicate-crate/license-policy warnings remain. Full log:
  `target/qualification-final/81f2e7b8c666406b83646592d27ddd41/cargo-deny.log`.

Renderer acceptance is BLOCKED, not passed. Matching installed Chrome/driver
152.0.7977.76 used a fresh repository-owned profile. Chrome's GPU subprocesses
exited with access denied (`0xC0000022`), then Chrome reported GPU process unusable.
No WebDriver session or renderer test started. No further browser launch should
be attempted without diagnosing this environment failure. No personal profile or
sandbox bypass. Logs/profile:
`target/renderer-maintainability/6b1029656d484c7a8077077c38bbbb17/`.

The fresh final performance run, packaging fixtures and NEW TWO-HOUR SOAK have
NOT run. Earlier two-hour soak evidence belongs to the old overhaul baseline.

## Resume sequence

1. Read this note, git status/log, and the maintainability/schema documents. Keep
   already committed work. Reestablish owners; review-agent memory may not survive
   reboot. Resolve any confirmed remaining source finding before freezing tests.
2. Generate one final scratch inventory from the frozen source, then give that
   exact inventory to every reviewer. Finish substantive per-method reviews and
   reconcile their identities; do not use discovery or family templates as review.
3. Run remaining packaging fixtures with pinned Node/npm and an isolated TEMP/TMP.
   Only one Cargo consumer at a time. Preserve failed roots/evidence.
4. Run quiet final performance against the current-mode pre-refactor reference.
   Investigate threshold crossings using three matched repeats. A reproducible
   regression blocks completion; do not attribute variation to Defender/noise
   without evidence. No competing compilation during measurement.
5. With Rust/source/build inputs frozen, run `scripts/run-backend-soak.ps1
   -DurationMinutes 120`. This has NOT started. It freezes and hashes the exact
   test executable, preflights discovery, uses owned roots and stops loader errors.
   No source changes/Cargo while running. Docs/ledger/CI work can proceed during
   the actual two-hour wait. This is isolated backend qualification, not native UI.
6. Merge only accepted sidecars, pass strict inventory check, update the canonical
   ledger and enable its check in CI/release-candidate scripts. Update the matching
   acceptance-contract assertion and rerun those executable fixtures.
7. Update current qualification/performance/soak receipts, including source and
   executable hashes. `backend-qualification.md` still contains historical 270-test,
   39-hard-gate and old-soak counts: clearly distinguish baseline from new results.
   Complete final checks and local commits, no push or publication.

## Method ledger: unfinished work

`docs/backend-method-review.json` is still the original 1,112-row inventory.
The final source has more entries; final canonical count is not yet established.
Files under `docs/backend-method-reviews/` are checkpoint drafts, not accepted
final evidence. In particular, a row's `reviewed` label is insufficient without
source-matched identities and accurate function-specific contracts.

All IDENTITY_FIELDS are now mandatory, including ownerCall with explicit null
where absent: kind, classification, file, line, endLine, symbol, qualifiedName,
signature, sourceDigest, cfg, ownerCall. Digests normalize CRLF only. Moves and
context changes require rereading. Every method/Drop/trait/local function/owned
async closure, including test/support code, requires specific review. Tests must
be concrete named coverage or honestly marked static-only; a suite count alone
does not prove a function's behavior.

Prior ownership and review status, to reverify on resume:

- backend_quality_gates_review: tooling, final reconciliation/CI gate, infrastructure
  ledger (91 rows including public_boundary_tests, backend_lifecycle_tests,
  performance and soak). Audits others' sidecars for generic/incorrect fields.
- download_engine_review: downloader (239 rows, substantively accepted pending final
  source reconciliation). Also revising runtime/updater/transaction/build
  nonproduction entries after inaccurate generic contracts were found. Check hash
  counter locks, recovery fixture responsibilities, HTTP parent/closure effects,
  unique_path (generates path only), cache ownership, and dropped waiter semantics.
- phase1_integration_review: lifecycle plus scheduling (120 rows, accepted pending
  final reconciliation). Completed independent Stage 5 runtime extraction review.
- runtime_updater_review: runtime/updater/transaction/build production ledger;
  still needs method-specific contract corrections and final reconciliation.
- state_queue_review: state/journal/models/errors/diagnostics/outbox/state_events.
  Nonproduction rewritten; 13 reducer production entries still needed accurate
  callers/errors/tests in the last cross-review. Include the new finalizer test
  and closure entries, and reconcile all identities.
- independent_backend_review: lib/main/windows_file/bootstrap/desktop/notifications/
  services and their tests/support, excluding public_boundary_tests/scheduling.
  Latest 107-row explicit mapping rewrite awaits substantive second audit. Earlier
  generic claims were withdrawn. Check readiness recording, installer launch helper,
  maintenance release, and closure ownership/cancellation field by field.
- workflow_service_prep: completed clean independent Stage 5 updater review.

Example final ledger sequence (read tool help if necessary):

```powershell
python -B scripts/inventory-backend-methods.py scan --previous docs/backend-method-review.json --ledger target/refactor-ledger/current.json
python -B scripts/inventory-backend-methods.py merge --ledger target/refactor-ledger/current.json --reviews-dir docs/backend-method-reviews
python -B scripts/inventory-backend-methods.py check --ledger target/refactor-ledger/current.json
```

Replace the canonical ledger only after a complete strict passing result and
substantive cross-review. Inventory is a structural lexer, not macro expansion or
a proof of correctness. Scratch ownership-specific scans and draft generators
remain under ignored `target/refactor-scratch/`; these are local evidence only.

## Performance and environment references

Use current/current comparisons: both runs labelled `after`, debug profile. The
script's `baseline` label emulates pre-overhaul behavior and is not suitable here.
Primary pre-refactor reference:
`target/performance/after-20260908T204145Z-1a07b77b1e4b4bedab0cf2e85ffd7973/`.
It has 45 hard gates, five runtime hashes and 408 successful leases. At 1,000 items,
snapshot p50/p95/p99 is 884/1289/1909 microseconds; durable command latency is
41926/46152/51138; journal latency 39700/41972/45006. Blocked-journal snapshot
1758 microseconds observes precommit state. Outbox: 207 batches, 2206 deltas,
876635 bytes, zero resyncs. Snapshot-wire latency: 11935/13724/14651 microseconds.

The four Stage 3 runs and repeat comparison receipts are listed in
`backend-maintainability.md`. All passed 45 hard gates; threshold crossings did not
consistently reproduce on the same metric across the repeats. They are intermediate
evidence, not the final refactor measurement. Review thresholds require both >10%
and >100 microseconds latency, or both >10% and >2 MiB memory. If needed, build
frozen baseline/final executables in separate owned directories and alternate
matched runs; never mutate the current checkout to do that.

Host recorded before reboot: Windows 11 25H2 build 26200.9278, i7-11700K, 16 logical
processors, J: NTFS. Rust 1.94.1. Pinned Node 22.23.1 and npm 10.9.9 are under
`nuclear-app/src-tauri/target/toolchains/`; npm's root contains npm.cmd and its
node_modules/npm/bin/npm-cli.js is the direct CLI. Python uses stdlib with `-B`.

## Qualification boundaries remaining

Clean Windows 11 installer/portable/native process acceptance, maintainer-controlled
extractor fixtures, authentic protected signing/update trust, and exact candidate
hash/operator/timestamp-bound cookie/update/rollback receipts are unavailable.
Report these as incomplete qualification. Do not run test-windows-user-process.ps1
against this personal desktop (it launches real fixtures and can adjust ACLs).
Do not call historical soak, mocked signing, static checks or component tests
clean-desktop release qualification. Publishing is a separate action.
