> Historical record for the earlier interface and its named source revisions. For the current 0.7.9 UI and validation boundaries, see [Clarity](clarity-0.7.9.md) and the [documentation index](README.md).

# Engineering quality and playlist performance

Execution was authorized on 2026-09-13 from main at
`242d3270017dcb2c50631d9327e599b8d1ce5023`. Automated qualification is separate
from native release acceptance.

The structural implementation and its functional regression gates are complete.
The matched backend performance review and native release acceptance remain
open. The [consolidated validation receipt](engineering-validation-2026-09-13.json)
records the exact evidence, source identities, artifact hashes and scope limits.

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
`237c2a2` (regression-backed replay/identity fixes), `664193a` (quality and
validation tooling), `be39965` (current review records and baseline inputs),
`428cac2` (public updater configuration preflight), and `dd22401` (release
warning correction and its source review).
No push or release was performed.

## Executed gates

| Gate | Actual result |
| --- | --- |
| Rust | 384 passed, zero failed; four opt-in harnesses excluded from the ordinary run. Strict release Clippy with warnings denied and formatting passed after the final correction. |
| Frontend | 214 passed, one opt-in soak excluded. Svelte zero errors/warnings; lint, formatting, production build and test-hook exclusion passed. |
| Browser workflows | All 15 cases passed, including 100- and 1,000-entry batch admission. |
| Visual comparison | All 60 scenarios match decoded pixels, geometry, text, controls and focus order. Repeated captures are stable. Browser scale emulation does not qualify native DPI. |
| Reviews | 1,382 backend units across 110 files: 789 production, 406 tests, 187 test support. Frontend: 451 callables across 34 production files. |
| Short soaks | Two-minute backend and 60-second five-workflow renderer runs passed. |
| Playlist admission | Three final frozen debug runs: 100 rows 20/14/13 ms; 1,000 rows 195/155/167 ms. One durable commit and bounded registration gates passed. Synthetic backend measurements, not network or UI latency. |
| Matched backend | All 45 hard gates passed in each of three final paired runs. Every report remains `review_required` for timing and memory differences. The earlier large journal median delay did not recur. |
| Matched renderer | All 18 runs passed the existing thresholds: three repeats per side at 1/100/1,000 rows. Input archives, source/tool hashes and environment matched. |
| Renderer long soak | Two hours passed: 318,745 mount/unmount cycles, 63,749 cycles of each of five workflow patterns and playlist resynchronization. All 110 input hashes stayed unchanged; both owned test processes exited. Heap measurements remain observational. |
| Backend long soak | Two hours passed: 1,438 mixed-workload cycles, 7,190 operations and 1,440 samples. Final journal reopened; no pending jobs, child processes, outbox backlog or output residue at quiescent samples. Frozen executable unchanged; owned test process exited. |
| Local package | Production executable and NSIS container built successfully from clean `dd22401`, without compiler warnings. No signing, installation, launch or native workflow acceptance was performed. |

Logs use the `target/engineering-*` prefix. See the
[finding-to-test checklist](engineering-findings.md),
[frontend ownership map](frontend-ownership.md), and
[canonical backend review ledger](backend-method-review.json). Supplemental
reviews are in `backend-method-reviews/engineering-2026-09-13/`.

Final timing distributions, memory observations and unresolved review flags are
in [the performance report](engineering-performance-2026-09-13.md). The final
package receipt is
`nuclear-app/src-tauri/target/p/local-0.7.1-20260913T223412Z-473c88b5838e4ff9b260b376a7bad6cc/local-packaged-build-receipt.json`.

Accepted visual report: `target/engineering-matched-visual-comparison.json`.
Production hashes are `5ccde857d8421686128d3f2b0d0f80549ca4d8407217cefabdde33e1f9dad037`
(original) and `e14411cf6b43d0880fc776129fd0d59fba747f207cacb03a6237dc60d96ffc2f`
(candidate). Renderer production files remain unchanged through later backend
fixes and tooling commits.

Accepted renderer performance report:
`target/engineering-matched-frontend-performance-comparison.json`.
At 1,000 rows the three-repeat median frame p95 was 7.005 ms on both sides;
input-to-paint was 12.805 ms originally and 11.905 ms for the candidate. Repeat
one preceded the long soaks; repeats two and three on both sides ran with those
soaks active. No Cargo build ran during the browser measurements.

The confirmed backend source-health invocation and exit-zero receipt are
`target/engineering-post-release-warning-source-health.json` and
`target/engineering-post-release-warning-source-health-exit.json`. Earlier failed gate and
unsupported-option logs remain preserved as failures.

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
`target/final-matched-backend-performance/20260913T222510Z-8aa031a907af41dd9dfe77e222c871a9/`.
Both preserve five initial hashes across 408 successful executable resolutions.

In the earlier matched run, journal-save medians at 1,000 rows in pairs one/two were 64.806/72.406 ms for
the candidate versus 42.784/43.141 ms for the original. Pair three returned to
42.157 versus 41.698 ms. Serialized sizes are identical. Source review finds no
added quadratic path in this direct save loop but cannot establish the cause
of the variance. Diagnostic-only copies measure preparation, JSON, DPAPI,
creation, write, flush and replacement separately; these are not replacement
qualification artifacts.

The diagnostic pairs measured 44.911/43.627 ms and 44.451/43.677 ms
(candidate/original). Encryption and disk phases were near parity in both
pairs. Preparation differences were small, and serialization changed direction
between pairs. Those differently instrumented runs do not explain the earlier
slow samples. The original comparator reports retain `review_required`; no
Windows, antivirus, allocator or storage cause is established. Candidate working
sets were typically 2.2–2.9 MiB larger, a separate observation from timing.
The bounded phase investigation is recorded in
`target/engineering-review/journal-performance-investigation.md` and
`target/journal-phase-runs/20260913T213508Z-9b88b9e5bf134478b945f3e87df134be/`.

The final three source-bound pairs measured 1,000-row journal-save medians of
44.876/44.359 ms, 45.502/43.891 ms, and 44.755/43.853 ms
(candidate/original). The earlier large median delay did not recur. Upper-tail
timing flags remain. The final debug test executable's working set was
8.9–9.5 MiB larger immediately after load; private memory was 0.34–0.46 MiB
larger at 1/100 rows and 1.79–1.81 MiB larger at 1,000 rows. Working set is not
equivalent to retained heap. These measurements do not establish a cause or
qualify native app memory behavior; the performance review remains open.

The passing final backend soak is
`target/soak/after-20260913T221524Z-5a83597ec4f045f887260a1f81b3a928/`.
Its frozen executable has SHA-256
`5236e2a512517f40884e8b7214d6a175495ee23174ed17ff9294d2c843c4092d`,
also used by the final playlist and matched backend checks. An earlier backend
soak was intentionally stopped after the last source correction and is retained
as superseded evidence, not a two-hour pass. The passing renderer soak is
`target/renderer-soak/20260913T210906Z-5d8475e67a4e414f851090988aa723fe/renderer-soak-receipt.json`.
It observed 7,200,050 ms with zero stderr bytes. The controlled final GC reading
was 74,344,744 bytes versus 66,712,416 bytes initially; that comparison is not
an arbitrary heap pass threshold or native memory qualification. Exact process
exit evidence is `target/engineering-renderer-process-exit.json`.

The backend observed 7,202,810 ms and completed 1,438 playlist batches,
2,876 row admissions, 1,438 preparation retries, 119 lifecycle drains and
119 runtime mutations. All 1,440 quiescent samples stayed within the configured
resource bounds. Sampled maxima were 24,879,104 bytes working set,
9,248,768 bytes private memory, 100 queue rows and 200 retained operations.
The final journal was 152,134 bytes and reopened successfully. These are isolated
debug test-process observations, not native application memory qualification.

After both soaks, independent verification rehashed all 178 backend inputs,
110 renderer inputs and six packaged artifacts. All matched. The packaged app
source is clean commit `dd22401`; later commits change documentation only.
The consolidated receipt embeds that final identity check and hashes the raw
logs and sample files. Passing the soak does not clear the separate matched
backend performance review.

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
