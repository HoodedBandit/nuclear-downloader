# Internal cleanup execution record

Comparison source: `df582df4ddc566712729b7006dfb080374c5ef45` on `main`.

The accepted scope preserves application output and behavior while separating ownership. Production changes must follow reproducible baseline checks. New branches, pushes, and release publication are outside this implementation run.

## Current status

Stage 1 is in progress. Production frontend and backend sources remain unchanged. The current changes add source inventory, independent renderer workflows, repeatable visual captures, and comparison/evidence tooling.

The user deferred the disposable Windows 11 environment on September 8, 2026 because licensed installation media or a clean VM image is unavailable. Hyper-V enumeration also requires an administrator token unavailable in this session. No VM, account, desktop permission, or host display setting has been changed. Native installer/portable qualification and real 100%/150% Windows scaling remain incomplete.

Headless Chrome captures use a fresh application-owned profile for each run. Browser scale emulation is useful for renderer regression checks; it is not native Windows scaling qualification. Historical backend qualification receipts do not qualify this future candidate.

## Executed baseline checks

Evidence below was collected before production changes. UTC run identifiers can fall on September 9 while the user's local date is September 8.

| Check                                                              | Result                                                                                                                                                                                              | Evidence                                                                                            |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| Fresh Rust test executable build, locked/offline/all features      | Passed compilation; discovered 307 tests, including three ignored tests. This is not a full test-suite execution.                                                                                   | `target/internal-cleanup-baseline/20260909T050054Z/`                                                |
| Backend performance, 1/100/1,000 queue items and runtime hash test | Four harness executions passed. The runner label is `after`, but the sources are the unchanged comparison baseline.                                                                                 | `target/performance/after-20260909T050208Z-8f35836dbc39493b8a886942805e8b59/summary.json`           |
| Frontend unit tests                                                | 10 files, 64 tests passed.                                                                                                                                                                          | Console execution; no candidate qualification receipt.                                              |
| Compiler-backed frontend inventory fixtures and source match       | Eight fixtures passed; 280 callable entries across 11 source files matched current sources. Discovery is not a substantive method review.                                                           | `scripts/frontend-source-inventory.test.mjs`, `docs/frontend-source-inventory.json`                 |
| Initial expanded renderer workflows                                | Four passed, three failed due to incorrect new test expectations. Tests were corrected against the unchanged source; corrected browser rerun is pending.                                            | `target/renderer-checks/workflows-20260909T051053Z-40d22bb7217848af88408c7611946e23/renderer.log`   |
| Initial 1,000-item frontend performance                            | Failed the existing frame-time target: p95 17.6 ms against <16.7 ms. State delta p95 was 0.3 ms; input-to-paint was 12 ms; no long tasks were reported. This failure is retained for investigation. | `target/renderer-checks/performance-20260909T051402Z-724510df037c47489bfb2430c773b60e/renderer.log` |
| Initial visual capture at browser scale 1                          | Capture completed: 30 scenarios, two captures each. Repeat stability and the complete two-scale contract are not yet accepted.                                                                      | `target/renderer-checks/visual-20260909T052122Z-8c504144f6234445b150651e1fe2ce12/visual-100.json`   |

Initial renderer receipts predate the strengthened source/harness archive contract. They remain diagnostic evidence and must not be presented as accepted baseline receipts. The canonical baseline will use fresh, matching runner receipts with archived inputs and validated repeat stability.

## Remaining gates

1. Finish Stage 1: corrected and expanded workflow runs, stable visual captures at both browser scales, deliberate comparator mutation tests, and 1/100/1,000-item frontend measurements. Investigate the existing frame-time failure without weakening its threshold.
2. Implement and test generation-aware startup and one page resource owner.
3. Extract queue presentation and workflow ownership, preserving IPC order, payloads, progress precedence, and existing errors.
4. Extract Svelte components individually and compare output, controls, focus, geometry, and workflows after each extraction.
5. Mechanically separate the specified StateStore, process, and publication modules, retaining ownership and lock boundaries; update source-matched reviews.
6. Run integrated checks and fresh candidate-bound performance/soak validation. Complete native qualification when its prerequisites are available.

No structural implementation, identical-UX qualification, or release qualification is claimed complete by this record.
