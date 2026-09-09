# Frontend performance and lifecycle evidence

These tools retain evidence for the accepted internal cleanup. They do not qualify native Windows rendering, installer behavior, or a release.

## Matched performance comparison

`scripts/compare-frontend-performance.mjs` accepts a `frontend-performance-comparison-input/v1` document naming three independent runs per queue size (1, 100, 1,000) for both baseline and candidate. Expected source commits, production/harness hashes, Node/browser/driver hashes, Windows build, and actual host scale are required. It verifies each archived input and recomputes measured percentiles from retained raw samples before comparing results.

The report includes distributions, three-repeat summaries, relative changes, and every failure against the existing thresholds. Both `thresholdsPassed` and `qualified` must be true for a successful CLI exit. A valid comparison containing a threshold failure is printed in full and exits 1; malformed or inconsistent evidence also fails. Seven focused fixtures passed, including duplicate evidence, tampered archived inputs, raw/summary disagreement, and the CLI's retained-failure behavior.

The original frame-time target remains unchanged. The single-run baseline exceeded it even during its idle control. Matched candidate measurements are still required; the comparison tool itself is not a performance result.

## Mounted renderer lifecycle soak

`scripts/run-renderer-soak.ps1` runs the opt-in mounted-page test using pinned Node 22.23.1, a single Vitest fork, controlled GC, and mocked Tauri IPC. Its normal duration is 120 minutes. It archives and hashes source/test/configuration inputs, checks them again afterward, verifies the executable identity, owns its process tree, and retains a receipt even when execution fails.

The four repeating cases cover an active operation waiter, a subscription resolving after disposal, a delayed startup snapshot, and successful inspection admission followed by authoritative queue publication. Each cycle checks listener, timer, observer, and post-disposal command behavior. Periodic and final JavaScript heap observations are recorded without inventing a memory pass threshold. Native process, browser, GPU, and backend memory require their separate checks.

The final runner smoke at `target/renderer-soak/20260909T080452Z-d62731b6db2a42ccab007c5c2c8ed4a2/renderer-soak-receipt.json` passed for 10.014 seconds and 595 mounts. All 89 archived inputs and the Node executable were unchanged; controlled GC was available. This proves the short runner smoke only. A fresh two-hour frozen-candidate run remains pending.

Logs have a 10 MiB observed stop threshold sampled every second. This can overshoot during sampling and process termination; it is not a hard byte cap. Earlier source-change failures remain in their original receipt directories.
