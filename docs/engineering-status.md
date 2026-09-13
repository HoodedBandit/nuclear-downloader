# Engineering quality and playlist performance

Execution was authorized on 2026-09-13 from main at
`242d3270017dcb2c50631d9327e599b8d1ce5023`. Automated qualification is separate
from native release acceptance.

## Implemented

- One authoritative, durable, idempotent playlist batch replaces per-entry
  inspection/admission round trips. Rows appear in the existing fetching state;
  one lifecycle-owned worker prepares missing metadata through the existing
  inspection permit. No automatic download or resume was added.
- Full metadata reuse requires matching media identity and captured settings.
  Readiness guards, cancellation, explicit retry, restart, lost replies and
  request-scoped display metadata retain backend authority.
- State/preparation commands, journal validation/platform I/O, updater ownership
  and path locks, publication resolution, and pure queue presentation now have
  focused private modules. Filename persistence uses a workflow port.
- Source gates enforce 500 effective file lines, 100 callable lines and five
  nesting levels, with exact reviewed legacy ceilings. Dependency, explicit-any
  and source-matched inventory rules run locally and in CI.
- Pinned local packaging uses unique owned outputs, exact Git-root/version/source
  checks, tool hashes, reparse and protected-installation checks. Its receipt
  distinguishes a standalone executable from an uninspected installer payload.

Local commits: `17f252d` (playlist admission), `74e15ca` (ownership extraction),
`237c2a2` (regression-backed replay/identity fixes), and `664193a` (quality and
validation tooling). No push or release was performed.

## Executed gates

| Gate | Actual result |
| --- | --- |
| Rust | 384 passed, zero failed; four opt-in harnesses excluded from the ordinary run. Strict Clippy and formatting passed. |
| Frontend | 214 passed, one opt-in soak excluded. Svelte zero errors/warnings; lint, formatting, production build and test-hook exclusion passed. |
| Browser workflows | All 15 cases passed, including 100- and 1,000-entry batch admission. |
| Visual comparison | All 60 scenarios match decoded pixels, geometry, text, controls and focus order. Repeated captures are stable. Browser scale emulation does not qualify native DPI. |
| Reviews | 1,382 backend units across 110 files: 789 production, 406 tests, 187 test support. Frontend: 451 callables across 34 production files. |
| Short soaks | Two-minute backend and 60-second five-workflow renderer runs passed. |
| Playlist admission | Three frozen debug runs: 100 rows 11/12/11 ms; 1,000 rows 154/145/162 ms. One durable commit and bounded registration gates passed. Synthetic backend measurements, not network or UI latency. |
| Matched backend | Three paired runs passed hard limits. Timing and memory review flags are retained; intermittent journal delays are under phase-level investigation. |
| Matched renderer | First 1/100/1,000 comparison is within existing thresholds. Full three-repeat comparison is running. |
| Long soaks/build | Fresh two-hour backend and renderer runs are active. Actual packaged build has not yet run. |

Logs use the `target/engineering-*` prefix. See the
[finding-to-test checklist](engineering-findings.md),
[frontend ownership map](frontend-ownership.md), and
[canonical backend review ledger](backend-method-review.json). Supplemental
reviews are in `backend-method-reviews/engineering-2026-09-13/`.

Accepted visual report: `target/engineering-matched-visual-comparison.json`.
Production hashes are `5ccde857d8421686128d3f2b0d0f80549ca4d8407217cefabdde33e1f9dad037`
(original) and `e14411cf6b43d0880fc776129fd0d59fba747f207cacb03a6237dc60d96ffc2f`
(candidate). Renderer production files remain unchanged through later backend
fixes and tooling commits.

## Evidence corrections and open measurements

Archived baseline captures initially inherited the parent Git revision. Their
production hashes remained verifiable; raw receipts were preserved and
supplemental corrections bind them to a fresh archive. Acceptance instead uses
recaptures from an exact detached Git worktree and the same candidate harness.
The comparator's harness-mismatch rejection was not bypassed.

The legacy backend harness uses `baseline` to deliberately rehash and `after`
to exercise runtime snapshots. That mismatched-mode comparison is invalid for
this refactor. Replacement runs use the verified original and candidate
executables in `after` mode, alternating order across three pairs. Provenance:
`target/matched-backend-performance/20260913T210305Z-280c566467894d74b0cc17004747abaf/`.
Both preserve five initial hashes across 408 successful executable resolutions.

At 1,000 rows, journal-save medians in pairs one/two were 64.806/72.406 ms for
the candidate versus 42.784/43.141 ms for the original. Pair three returned to
42.157 versus 41.698 ms. Serialized sizes are identical. Source review finds no
added quadratic path in this direct save loop but cannot establish the cause
of the variance. Diagnostic-only copies measure preparation, JSON, DPAPI,
creation, write, flush and replacement separately; these are not replacement
qualification artifacts.

## Preserved boundaries and incomplete native acceptance

- Five downloads, one inspection, one explicit WebM conversion. The twenty-way
  inspection idea was withdrawn.
- Eighteen commands, five events, generated contracts, DPAPI schema 1, saved
  queues, collision-safe publication and explicit retry after interruption.
- Existing appearance and controls, with approved prompt playlist rows using
  the existing fetching state.
- No installed app, accounts, VMs, taskbar, display or personal data changes.
  `src-tauri/target/install-audit-v0.5.4` remains a protected active installation.
- Native installer/uninstaller, installed-app interruption, cookie-account and
  native runtime rollback acceptance remain paused. Windows 11/WebView2,
  authentic signed-update and controlled extractor-fixture receipts are still
  required for release qualification. Historical receipts do not qualify this
  candidate.

Pushing and publication remain separate actions. Active or blocked checks are
not counted as passes.
