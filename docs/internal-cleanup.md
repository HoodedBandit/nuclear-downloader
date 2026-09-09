# Internal cleanup execution record

Comparison source: `df582df4ddc566712729b7006dfb080374c5ef45` on `main`.

The accepted scope preserves application output and behavior while separating ownership. Production changes must follow reproducible baseline checks. New branches, pushes, and release publication are outside this implementation run.

## Current status

Stages 1 through 5 passed their source and regression gates. The baseline includes source inventory, 11 independently repeatable renderer workflows, 60 visual scenarios with two stable captures each, and frontend/backend measurements for 1, 100, and 1,000 queue items. Queue metadata lifetime fixes passed their separate regressions. Each of the ten component/style extractions passed its own gate. The integrated candidate passed 304 Rust tests, strict Clippy, 167 frontend tests, all 11 renderer workflows, and exact comparison of all 60 visual scenarios. `frontend-lifecycle-review.md`, `frontend-workflow-review.md`, `frontend-components-review.md`, and `backend-internal-cleanup-review.md` record the changes and evidence.

Frozen candidate `fd58050b054f921d315658880a14793fb93fcae6` then completed all 18 matched frontend performance measurements within the existing thresholds. All three matched backend hard gates passed. Backend timing and memory review flags were distributed 2, 0, and 22 across the repeats; none recurred across two repeats. The reports retain every flag without claiming a causal improvement or zero performance effect.

Both fresh two-hour soaks passed against that candidate. The backend completed 1,438 cycles and 7,190 operations; all 1,440 samples met its resource limits, and final journal reopen and owned-fixture cleanup passed. The renderer completed 468,194 mounts across four workflow/race cases with teardown assertions after each mount. Its heap measurements remain observational. Backend debug tests and renderer jsdom tests do not qualify the native application. Native qualification remains deferred, and the production npm audit still awaits its separately requested network approval.

An installed Chrome update invalidated one playlist visual run. The original browser executable was recovered from Google's signed static package and matched the original baseline SHA-256 exactly. A fresh full playlist gate and the final update-dialog gate passed against the unchanged baseline. `internal-cleanup-browser-recovery.md` records the recovery and retained invalid receipt; no replacement baseline was approved.

The user deferred the disposable Windows 11 environment on September 8, 2026 because licensed installation media or a clean VM image is unavailable. Hyper-V enumeration also requires an administrator token unavailable in this session. No VM, account, desktop permission, or host display setting has been changed. Native installer/portable qualification and real 100%/150% Windows scaling remain incomplete.

Headless Chrome captures use a fresh application-owned profile for each run. Browser scale emulation is useful for renderer regression checks; it is not native Windows scaling qualification. Historical backend qualification receipts do not qualify this future candidate.

## Executed baseline checks

Evidence below was collected before production changes. Each row records the result at that time; later paragraphs record corrected runs and accepted baselines. UTC run identifiers can fall on September 9 while the user's local date is September 8.

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

The strengthened runner now archives exact production and harness inputs, including untracked source files, generated bindings, static assets, and build configuration. A fixture using its real input enumerator passed: source edits and newly added source files invalidate the after-run check, while archived bytes remain unchanged. Its receipts also bind the Node, Chrome, and driver hashes and record both browser emulation and the actual host display scale.

Svelte/TypeScript checking, strict ESLint, frontend formatting, a fresh production build, and production-bundle test-hook exclusion checks passed after the Stage 1 harness additions.

The unchanged backend test executable was subsequently exercised: all 270 non-export tests passed and three long-running harness tests remained ignored. The 34 binding-export tests initially failed because the direct executable was launched from the repository root, so their relative output path fell outside the workspace. Running those 34 tests from `nuclear-app/src-tauri` passed, and generated bindings remained byte-identical in Git. Both executions are retained as `backend-tests.log` and `backend-binding-tests.log` in the baseline evidence directory. This accounts for all 304 ordinary tests without concealing the runner working-directory error.

The expanded workflow rerun at `target/renderer-checks/workflows-20260909T053917Z-c5e3a95f30b44b4396aa5a2f1e5a0383/` passed nine cases and failed two new test interactions. Filename editing was affected by WebDriver's clear-value behavior, which triggers the application's existing blur commit. The test now uses actual keyboard selection and typing. The update dialog test now reacquires its element after Escape removes and reopening recreates it. The corrected run at `target/renderer-checks/workflows-20260909T054758Z-8d5d9a9d93ba4bebabd08a49f8b877eb/` passed all 11 cases and verified unchanged inputs. `frontend-behavior-baseline.md` maps these cases to their observable workflows and complementary unit tests.

The timing runs with raw samples and an idle-renderer control retained the existing failure:

| Queue | Idle frame p95 | Workload frame p95 | State-delta dispatch p95 | Input-to-paint | Receipt directory under `target/renderer-checks/`               |
| ----- | -------------: | -----------------: | -----------------------: | -------------: | --------------------------------------------------------------- |
| 1     |        19.4 ms |            19.6 ms |                   0.3 ms |        18.1 ms | `performance-20260909T054141Z-47c6e8cd59944585962b81d42b05724d` |
| 100   |        20.0 ms |            20.6 ms |                   0.3 ms |        18.4 ms | `performance-20260909T054329Z-1b7e01c86ea34a0cbc2092136cb21d0b` |
| 1,000 |        20.6 ms |            20.6 ms |                   0.2 ms |        30.1 ms | `performance-20260909T054505Z-bb51d4b741794565b0d54ff11c9d7880` |

All three runs verified that inputs and executable identities remained unchanged. All failed the existing workload-frame target of p95 <16.7 ms. The idle control also exceeded that target before application startup, which is evidence of an environmental contribution, not an application performance pass. Raw distributions and the limited Chromium-reported JavaScript heap measurements are retained in each `performance.json`. These are single-run baseline observations; matched repeat and candidate comparisons remain pending.

## Validation record and remaining gates

Candidate results are recorded in the [qualification checklist](internal-cleanup-stage5-qualification.md), [frontend performance report](internal-cleanup-stage5-frontend-performance.md), [backend performance report](internal-cleanup-stage5-backend-performance.md), [backend soak report](internal-cleanup-stage5-backend-soak.md), and [renderer soak report](internal-cleanup-stage5-renderer-soak.md). Their JSON companions retain the measurements and evidence identities. The tested source is `fd58050`; subsequent delivery changes only these documentation records.

The accepted visual baseline is recorded in `internal-cleanup-visual-baseline.json`. Its paired captures are `target/renderer-checks/visual-20260909T060041Z-885155a599d74d619e7da52667d0c328/visual-100.json` and `target/renderer-checks/visual-20260909T055802Z-eacf8baed77645b5b8e066b3d261ab8d/visual-150.json`. All 60 scenarios had identical decoded pixels and semantic evidence across two independent repeats. The final comparator reported zero errors and verified archived source, harness, executable, receipt, and screenshot identities. Its 21 synthetic cases passed, including deliberate pixel, text, geometry, enabled-state, focus-order, provenance, and malformed-input failures. This is baseline stability evidence, not a candidate comparison. Browser scale emulation does not qualify native Windows scaling.

1. Completed: frozen matched frontend measurements passed 18/18 runs at queue sizes 1, 100, and 1,000 without weakening thresholds. Three matched backend comparisons passed every hard gate; their isolated review flags remain documented. The earlier idle-frame failure is retained as environment-sensitive evidence without an established root cause.
2. Completed: generation-aware startup, one page resource owner, waiter cleanup, and their focused/integrated regression gates.
3. Completed: queue presentation/workflow extraction and the separately regression-tested metadata/display-timestamp lifetime corrections.
4. Completed: ten component/style extractions with individual output, controls, focus, geometry, and workflow comparisons.
5. Completed: mechanical StateStore, process, and publication extractions, retaining ownership and lock boundaries; all 1,215 backend review units match current sources.
6. Completed: integrated short checks, matched performance, and fresh candidate-bound backend and renderer soaks. Complete native and external release qualification when its prerequisites are available. Production npm audit remains pending approval to transmit dependency metadata.

Structural implementation, source/regression gates, matched performance review, and the candidate-bound soaks are complete. Native identical-UX and release qualification remain incomplete. No push or release publication was performed.
