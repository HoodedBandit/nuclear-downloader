# Matched frontend performance results

Attempt 3 completed all 18 measurements and the frozen comparator returned `qualified: true` and `thresholdsPassed: true`. Each cell below is the three-repeat median with `[minimum, maximum]` in milliseconds.

| Side      | Queue | Metric             |            p50 |            p95 |             p99 |
| --------- | ----: | ------------------ | -------------: | -------------: | --------------: |
| baseline  |     1 | idleFrame          |       8 [8, 8] | 8.1 [8.1, 8.1] |  8.2 [8.1, 8.3] |
| baseline  |     1 | workloadFrame      | 7.3 [7.3, 7.4] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| baseline  |     1 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| baseline  |     1 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.1] |
| baseline  |     1 | stateDeltaDispatch | 0.1 [0.1, 0.1] | 0.2 [0.2, 0.2] |  0.3 [0.2, 0.3] |
| baseline  |   100 | idleFrame          |       8 [8, 8] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.5] |
| baseline  |   100 | workloadFrame      | 7.4 [7.3, 7.6] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| baseline  |   100 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| baseline  |   100 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.2] |
| baseline  |   100 | stateDeltaDispatch |       0 [0, 0] | 0.2 [0.2, 0.2] |  0.3 [0.3, 0.3] |
| baseline  |  1000 | idleFrame          |       8 [8, 8] | 8.1 [8.1, 8.1] |  8.3 [8.2, 8.3] |
| baseline  |  1000 | workloadFrame      | 7.4 [7.3, 7.4] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| baseline  |  1000 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| baseline  |  1000 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.1] |
| baseline  |  1000 | stateDeltaDispatch |       0 [0, 0] | 0.2 [0.2, 0.2] |  0.3 [0.2, 0.3] |
| candidate |     1 | idleFrame          |   7.9 [7.9, 8] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| candidate |     1 | workloadFrame      | 7.3 [7.3, 7.3] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| candidate |     1 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| candidate |     1 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.1] |
| candidate |     1 | stateDeltaDispatch | 0.1 [0.1, 0.1] | 0.2 [0.2, 0.2] |  0.3 [0.2, 0.3] |
| candidate |   100 | idleFrame          |       8 [8, 8] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.4] |
| candidate |   100 | workloadFrame      | 7.3 [7.3, 7.4] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| candidate |   100 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| candidate |   100 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.1] |
| candidate |   100 | stateDeltaDispatch |   0.1 [0, 0.1] | 0.2 [0.2, 0.2] |  0.3 [0.3, 0.3] |
| candidate |  1000 | idleFrame          |       8 [8, 8] | 8.1 [8.1, 8.1] | 8.2 [8.2, 17.7] |
| candidate |  1000 | workloadFrame      | 7.3 [7.3, 7.4] | 8.1 [8.1, 8.1] |  8.2 [8.2, 8.2] |
| candidate |  1000 | reducer            |       0 [0, 0] |       0 [0, 0] |        0 [0, 0] |
| candidate |  1000 | progressDispatch   |       0 [0, 0] | 0.1 [0.1, 0.1] |  0.1 [0.1, 0.1] |
| candidate |  1000 | stateDeltaDispatch |   0.1 [0, 0.1] | 0.2 [0.2, 0.2] |  0.3 [0.3, 0.3] |

All runs recorded 1,500 reducer samples, 1,500 progress dispatch samples, 3,000 state-delta dispatch samples, and 300 idle-frame samples. Workload-frame sample counts and heap ranges are retained in the JSON report. Long-task counts were zero in every run.

Provenance is bound to baseline commit `5504f2284c96838f2895e9fbff0e2fd7eef60561` / production `2f7edd48e66dfba3addbe32070e24062a23c2413afc8dadd973382be2fc7f154` and candidate commit `fd58050b054f921d315658880a14793fb93fcae6` / production `9104595dbaa5b69f027367b4776d902a46f620da9acf319c01513dc79813660b`. The harness hash is `7dff2b7474c644449198227cc915555715404b9d26be63de852980dae1e6cb67`. Executable hashes are Node `f8d162c0641dcee512132f3bcf8a68169c7ecb852efd8e1a46c9fec5a0f469ed`, Chrome `17b09f4c2e7806a05b0b648e7d459c3e3868f215adc93fa887adc3892bc704c0`, and ChromeDriver `4a35f44f324cfe2bb37194447a419bd72a382a5f1a4e87d4305b9b674d1cb212`. All 18 run IDs and receipt hashes were unique; inputs were verified after execution. Both independent dependency copies matched aggregate `454a5359fa77d2a72531eae071b17c11a92c815ba9c1c7c58d0d7ef207b27938` across 22,792 files and 252,885,691 bytes, excluding only `node_modules/.vite`.

Chromium JavaScript heap figures are observational and exclude native and GPU memory. Zero-millisecond duration samples reflect browser timer resolution and quantization, not zero execution cost. The earlier archived run failed its frame threshold with idle-frame p95 near 20 ms; the matched runs measured idle-frame p95 medians near 8.1 ms. This difference does not establish a root cause.

Attempts 1 and 2 remain preserved as setup/failure evidence and receive no qualification credit. Attempt 2 produced one passing baseline queue-1 measurement, but its receipt identified the root checkout commit because the harness ran from the wrong working directory.

The [companion JSON](./internal-cleanup-stage5-frontend-performance.json) retains per-run measurements, counts, and evidence identities.
