# Frontend performance and lifecycle evidence

These tools retain evidence for the accepted internal cleanup. They do not qualify native Windows rendering, installer behavior, or a release.

## Matched performance comparison

`scripts/compare-frontend-performance.mjs` accepts a `frontend-performance-comparison-input/v1` document naming three independent runs per queue size (1, 100, 1,000) for both baseline and candidate. Expected source commits, production/harness hashes, Node/browser/driver hashes, Windows build, and actual host scale are required. It verifies each archived input and recomputes measured percentiles from retained raw samples before comparing results.

The report includes distributions, three-repeat summaries, relative changes, and every failure against the existing thresholds. Both `thresholdsPassed` and `qualified` must be true for a successful CLI exit. A valid comparison containing a threshold failure is printed in full and exits 1; malformed or inconsistent evidence also fails. Ten focused fixtures passed, including duplicate evidence, tampered archived inputs, raw/summary disagreement, the CLI's retained-failure behavior, and Windows path confinement.

The first GitHub CI run after enabling these fixtures rejected valid archived inputs because it compared a canonical run root with a lexical file path. A local Windows junction regression reproduced that false rejection (nine passed, one failed). The correction checks lexical and canonical confinement separately while retaining every child reparse-link rejection; all ten cases then passed with no skips. The new cases also reject traversal outside the run and a linked child directory. This fixes validation tooling only; it changes no application code, benchmark thresholds, or historical measurements. The initial parse-error attempt is retained separately and receives no regression credit. Red and green logs are under `target/local-build-86f2f96-20260910T024833Z/`.

The original frame-time target remains unchanged. The initial single-run baseline exceeded it even during its idle control. Subsequent [matched measurements](internal-cleanup-stage5-frontend-performance.md) passed all 18 runs for the structural candidate `fd58050`. Later fixes need their own matched evidence; the comparison tool itself is not a performance result.

## Performance clock precision

GitHub run `34431620193` failed the strict `< 16.7 ms` frame-time gate with an idle p95 of `16.700000000000728 ms` and workload p95 of `16.70000000001164 ms`. Chrome's ordinary 100-microsecond timestamp precision cannot reliably distinguish the 60 Hz frame interval (about 16.667 ms) from this boundary. [Chrome documents](https://developer.chrome.com/blog/cross-origin-isolated-hr-timers/) a 5-microsecond clock for cross-origin-isolated documents.

The WebDriver-only Vite server now adds isolation headers to the exact `/?nuclear-performance-clock=isolated` document. The performance spec navigates there before installing fixtures and checks both isolation and observed clock precision. Ordinary workflow and visual documents, application builds, and native WebView2 settings are unchanged. HTTP regression tests verify the header boundary. The performance record includes the observed clock; raw frame durations, percentiles, workload, and all acceptance thresholds remain unchanged. No rounding, smoothing, idle subtraction, or frame-rate override is applied.

This harness change requires new matched baseline/candidate measurements for future comparisons; historical evidence remains bound to its original harness hash. The ordinary GitHub browser check is a CI regression check, not a pinned visual comparison or native Windows release qualification.

Local verification passed four HTTP boundary tests and the existing ten comparison-contract tests. The pinned Chrome `152.0.7977.76` 1,000-item run confirmed isolation, an observed minimum clock step of about 0.005 ms, all 1,500 progress events, and every unchanged workload limit. Its frame p95 was 7.05 ms on this host; this is a single CI-fix regression run, not a new matched benchmark. Raw samples, input hashes, and the successful receipt are retained in `target/renderer-checks/performance-20260910T032003Z-244a3ed308a94b779ff6fe377cac53d7/`.

## Mounted renderer lifecycle soak

`scripts/run-renderer-soak.ps1` runs the opt-in mounted-page test using pinned Node 22.23.1, a single Vitest fork, controlled GC, and mocked Tauri IPC. Its normal duration is 120 minutes. It archives and hashes source/test/configuration inputs, checks them again afterward, verifies the executable identity, owns its process tree, and retains a receipt even when execution fails.

The four repeating cases cover an active operation waiter, a subscription resolving after disposal, a delayed startup snapshot, and successful inspection admission followed by authoritative queue publication. Each cycle checks listener, timer, observer, and post-disposal command behavior. Periodic and final JavaScript heap observations are recorded without inventing a memory pass threshold. Native process, browser, GPU, and backend memory require their separate checks.

The initial runner smoke at `target/renderer-soak/20260909T080452Z-d62731b6db2a42ccab007c5c2c8ed4a2/renderer-soak-receipt.json` passed for 10.014 seconds and 595 mounts. All 89 archived inputs and the Node executable were unchanged; controlled GC was available. This proves the short runner smoke only.

The subsequent [two-hour renderer soak](internal-cleanup-stage5-renderer-soak.md) passed for frozen candidate `fd58050`, with 468,194 mounts and the retained resource observations. The [qualification record](internal-cleanup-stage5-qualification.md) binds its inputs and limits. Later fixes have not received a new two-hour run, and this jsdom workload is not native WebView2 qualification.

Logs have a 10 MiB observed stop threshold sampled every second. This can overshoot during sampling and process termination; it is not a hard byte cap. Earlier source-change failures remain in their original receipt directories.
