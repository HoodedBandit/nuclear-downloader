# Frontend performance and lifecycle evidence

These tools retain evidence for the accepted internal cleanup. They do not qualify native Windows rendering, installer behavior, or a release.

## Matched performance comparison

`scripts/compare-frontend-performance.mjs` accepts a `frontend-performance-comparison-input/v1` document naming three independent runs per queue size (1, 100, 1,000) for both baseline and candidate. Expected source commits, production/harness hashes, Node/browser/driver hashes, Windows build, and actual host scale are required. It verifies each archived input and recomputes measured percentiles from retained raw samples before comparing results.

The report includes distributions, three-repeat summaries, relative changes, and every failure against the existing thresholds. Both `thresholdsPassed` and `qualified` must be true for a successful CLI exit. A valid comparison containing a threshold failure is printed in full and exits 1; malformed or inconsistent evidence also fails. Ten focused fixtures passed, including duplicate evidence, tampered archived inputs, raw/summary disagreement, the CLI's retained-failure behavior, and Windows path confinement.

The first GitHub CI run after enabling these fixtures rejected valid archived inputs because it compared a canonical run root with a lexical file path. A local Windows junction regression reproduced that false rejection (nine passed, one failed). The correction checks lexical and canonical confinement separately while retaining every child reparse-link rejection; all ten cases then passed with no skips. The new cases also reject traversal outside the run and a linked child directory. This fixes validation tooling only; it changes no application code, benchmark thresholds, or historical measurements. The initial parse-error attempt is retained separately and receives no regression credit. Red and green logs are under `target/local-build-86f2f96-20260910T024833Z/`.

The original frame-time target remains unchanged. The initial single-run baseline exceeded it even during its idle control. Subsequent [matched measurements](internal-cleanup-stage5-frontend-performance.md) passed all 18 runs for the structural candidate `fd58050`. Later fixes need their own matched evidence; the comparison tool itself is not a performance result.

## Mounted renderer lifecycle soak

`scripts/run-renderer-soak.ps1` runs the opt-in mounted-page test using pinned Node 22.23.1, a single Vitest fork, controlled GC, and mocked Tauri IPC. Its normal duration is 120 minutes. It archives and hashes source/test/configuration inputs, checks them again afterward, verifies the executable identity, owns its process tree, and retains a receipt even when execution fails.

The four repeating cases cover an active operation waiter, a subscription resolving after disposal, a delayed startup snapshot, and successful inspection admission followed by authoritative queue publication. Each cycle checks listener, timer, observer, and post-disposal command behavior. Periodic and final JavaScript heap observations are recorded without inventing a memory pass threshold. Native process, browser, GPU, and backend memory require their separate checks.

The initial runner smoke at `target/renderer-soak/20260909T080452Z-d62731b6db2a42ccab007c5c2c8ed4a2/renderer-soak-receipt.json` passed for 10.014 seconds and 595 mounts. All 89 archived inputs and the Node executable were unchanged; controlled GC was available. This proves the short runner smoke only.

The subsequent [two-hour renderer soak](internal-cleanup-stage5-renderer-soak.md) passed for frozen candidate `fd58050`, with 468,194 mounts and the retained resource observations. The [qualification record](internal-cleanup-stage5-qualification.md) binds its inputs and limits. Later fixes have not received a new two-hour run, and this jsdom workload is not native WebView2 qualification.

Logs have a 10 MiB observed stop threshold sampled every second. This can overshoot during sampling and process termination; it is not a hard byte cap. Earlier source-change failures remain in their original receipt directories.
