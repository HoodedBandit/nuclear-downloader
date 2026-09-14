# Engineering performance evidence — 2026-09-13

## Scope and result

This record covers controlled performance harnesses for the backend, renderer, and playlist admission changes. The Rust benchmarks use the debug profile; browser comparisons use the production frontend with fixture IPC. These are synthetic engineering workloads. They do not represent live network, extractor, full native-app, or end-to-end user measurements unless a section explicitly says otherwise.

The matched frontend comparison qualified all 18 browser runs, and all three final playlist benchmark repetitions passed their size-specific gates. The matched backend comparisons passed every hard gate but remain `review_required` because their timing and memory review thresholds raised flags. The backend result must not be represented as a performance qualification pass.

## Matched backend comparison

The final evidence is in `target/final-matched-backend-performance/20260913T222510Z-8aa031a907af41dd9dfe77e222c871a9`. It contains three counterbalanced baseline/candidate comparisons: baseline/candidate, candidate/baseline, baseline/candidate. Both sides ran the native Rust test executable with label `after` and profile `debug` against deterministic local fixtures.

All three reports passed all 45 hard gates with zero failures, including sample counts, blocked-I/O snapshot isolation, runtime hashing, and event/outbox bounds. All 12 candidate-only evidence checks in each report passed. Nevertheless, every report has status `review_required`; the timing and memory flags remain in force.

A read-only PE inspection found the candidate test image 1.754 MiB larger,
mostly executable and read-only sections; the writable data section is the same
size. This is smaller than the observed working-set difference and does not
establish its cause. The new queue preparation fields are empty in the matched
fixture, so that workload does not allocate their optional ID strings. A later
production-profile memory comparison should include populated playlist rows and
snapshot copies. This observation neither proves a leak nor clears the memory
review (`target/engineering-review/backend-memory-pe-observation.md`).

### Evidence binding

The measurement runner verified receipt and executable hashes before and after the run. Source-manifest hashes below were recorded and checked during compilation. The measurement runner did not rehash either working tree against its manifest, so final working-tree verification is separate evidence.

That final verification completed after both two-hour soaks: all 178 backend
inputs, 110 renderer inputs and six packaged artifacts matched their recorded
sizes and hashes. The exact identity receipt is embedded in
[`engineering-validation-2026-09-13.json`](engineering-validation-2026-09-13.json).
The production source was committed as `dd22401`; subsequent changes are
documentation only. This identity check does not clear the performance review
or qualify native application behavior.

| Role | Source identity | Executable SHA-256 | Receipt SHA-256 |
| --- | --- | --- | --- |
| Baseline | commit `242d3270017dcb2c50631d9327e599b8d1ce5023`; archive SHA-256 `470b6c75bf48cea43e82a13f0bab7fd5db37cf566da46b822ed7861d9191f29e` | `8f3a22a53c47d4c248ca8df2cc84fdc01af81c62a74813f8e68b06508dba1385` | `08ff25e147217b3c27f6b8fb82b217d368258af2649c0597fd0da2bad2ec68ee` |
| Candidate | head `428cac2f0f9ab01703a537e75359979b659ee717`; build metadata recorded ` M nuclear-app/src-tauri/src/runtime/verified.rs`; 178-file build manifest SHA-256 `a43e43699baf1fe6ae415a68af69447fde0f336194c76b3e154b082cea9f504f` | `5236e2a512517f40884e8b7214d6a175495ee23174ed17ff9294d2c843c4092d` | `b299c3a5277858427fd0a8f407ca85913a73b72f7996e49899ce02d7439b8ca0` |

### Exact timings

Values are microseconds and shown as `p50 / p95 / p99`.

| Repeat | Queue | Measurement | Baseline | Candidate |
| ---: | ---: | --- | ---: | ---: |
| 1 | 1 | snapshot | 2 / 3 / 4 | 1 / 2 / 3 |
| 1 | 1 | journal save | 5,607 / 6,344 / 8,259 | 5,424 / 8,369 / 10,975 |
| 1 | 1 | durable command | 6,143 / 8,082 / 8,804 | 5,153 / 6,061 / 10,801 |
| 1 | 100 | snapshot | 141 / 249 / 299 | 113 / 182 / 219 |
| 1 | 100 | journal save | 10,257 / 12,044 / 15,244 | 9,155 / 10,415 / 11,578 |
| 1 | 100 | durable command | 10,016 / 11,329 / 12,873 | 9,595 / 12,288 / 15,146 |
| 1 | 1000 | snapshot | 1,250 / 2,395 / 3,110 | 1,142 / 1,943 / 2,439 |
| 1 | 1000 | journal save | 44,359 / 51,862 / 56,684 | 44,876 / 53,238 / 64,205 |
| 1 | 1000 | durable command | 48,508 / 61,808 / 77,221 | 47,084 / 55,493 / 62,059 |
| 2 | 1 | snapshot | 2 / 3 / 4 | 1 / 3 / 3 |
| 2 | 1 | journal save | 4,797 / 5,702 / 6,413 | 5,053 / 6,018 / 8,885 |
| 2 | 1 | durable command | 5,390 / 6,279 / 6,946 | 5,805 / 6,551 / 7,266 |
| 2 | 100 | snapshot | 117 / 199 / 233 | 120 / 220 / 277 |
| 2 | 100 | journal save | 10,513 / 13,453 / 15,355 | 10,019 / 11,944 / 15,934 |
| 2 | 100 | durable command | 11,911 / 20,021 / 22,078 | 11,361 / 18,191 / 20,948 |
| 2 | 1000 | snapshot | 1,296 / 2,398 / 2,979 | 1,337 / 2,455 / 3,074 |
| 2 | 1000 | journal save | 43,891 / 52,561 / 56,024 | 45,502 / 59,038 / 68,712 |
| 2 | 1000 | durable command | 47,090 / 54,922 / 60,985 | 47,641 / 57,032 / 61,326 |
| 3 | 1 | snapshot | 1 / 2 / 3 | 2 / 3 / 4 |
| 3 | 1 | journal save | 4,691 / 5,524 / 6,605 | 4,979 / 7,906 / 11,015 |
| 3 | 1 | durable command | 5,301 / 7,732 / 14,204 | 4,912 / 5,935 / 6,695 |
| 3 | 100 | snapshot | 111 / 177 / 206 | 122 / 210 / 242 |
| 3 | 100 | journal save | 9,264 / 10,322 / 10,847 | 9,606 / 10,977 / 15,513 |
| 3 | 100 | durable command | 9,762 / 12,394 / 15,116 | 10,042 / 13,667 / 19,523 |
| 3 | 1000 | snapshot | 1,144 / 2,016 / 2,363 | 1,293 / 2,294 / 2,583 |
| 3 | 1000 | journal save | 43,853 / 55,532 / 58,893 | 44,755 / 55,897 / 62,628 |
| 3 | 1000 | durable command | 46,509 / 54,912 / 57,638 | 51,424 / 62,556 / 72,439 |

The earlier uninstrumented comparison showed queue-1000 journal-save p50 candidate increases of 22.022 ms and 29.265 ms in its first two pairs, with near parity in the third. That large median difference did not recur here: the three final candidate-minus-baseline p50 differences are +0.517 ms, +1.611 ms, and +0.902 ms. Diagnostic-only instrumented pairs likewise measured queue-1000 journal-save p50 ratios of 1.03 and 1.02. Those instrumented binaries support diagnosis and are not qualification evidence.

The final queue-1000 journal-save upper-tail differences remain variable: candidate minus baseline is +1.376, +6.477, and +0.365 ms at p95 and +7.521, +12.688, and +3.735 ms at p99. Durable-command differences also vary in direction and magnitude. No measured evidence establishes a cause.

### Memory

Values are MiB; each cell is `baseline / candidate`.

| Repeat | Queue | Working set after load | Working set after samples | Private after load | Private after samples |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 8.15 / 17.08 | 10.38 / 17.89 | 1.34 / 1.79 | 2.07 / 2.55 |
| 1 | 100 | 8.42 / 17.34 | 11.36 / 19.50 | 1.61 / 2.04 | 3.08 / 4.33 |
| 1 | 1000 | 10.04 / 19.50 | 16.96 / 28.04 | 3.21 / 5.00 | 8.82 / 12.97 |
| 2 | 1 | 8.16 / 17.08 | 10.42 / 17.89 | 1.34 / 1.78 | 2.12 / 2.55 |
| 2 | 100 | 8.42 / 17.33 | 11.36 / 18.77 | 1.71 / 2.05 | 3.07 / 3.57 |
| 2 | 1000 | 10.04 / 19.51 | 19.98 / 26.79 | 3.21 / 5.02 | 11.91 / 12.66 |
| 3 | 1 | 8.15 / 17.08 | 10.36 / 17.89 | 1.33 / 1.79 | 2.04 / 2.55 |
| 3 | 100 | 8.42 / 17.33 | 11.98 / 18.99 | 1.70 / 2.10 | 3.87 / 3.81 |
| 3 | 1000 | 10.05 / 19.50 | 17.26 / 27.42 | 3.23 / 5.02 | 9.12 / 12.75 |

The candidate working-set increase appears in all nine observations: about 8.9–9.5 MiB after load and 7.0–11.1 MiB after sampling. Working set includes resident memory beyond private committed memory, so this does not establish heap leakage. At queues 1 and 100, candidate private memory after load is only 0.34–0.46 MiB higher; at queue 1000 it is 1.79–1.81 MiB higher. Private memory after sampling is inconsistent, including queue-1000 differences of 4.15, 0.75, and 3.63 MiB.

### Runtime and event bounds

Runtime-hash results were identical on both sides in every repeat: 100 operations, one health validation, 408 successful resolutions from 408 calls, and tools `yt-dlp`, `ffmpeg`, `ffprobe`, and `deno`. The harness hashed 610 manifest bytes in one invocation and 83 tool bytes in four invocations, for 693 bytes and five invocations total.

| Queue | Event deltas | Event bytes | Contiguous | Outbox batches | Outbox deltas | Estimated bytes | Resyncs |
| ---: | ---: | ---: | :---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 1,088 | yes | 207 | 208 | 91,583 | 0 |
| 100 | 200 | 108,980 | yes | 207 | 406 | 174,923 | 0 |
| 1000 | 2,000 | 1,093,592 | yes | 207 | 2,206 | 934,523 | 0 |

All were below ceilings of 256 batches, 4,096 deltas, and 16,777,216 bytes, with zero resyncs required.

### Review status

| Repeat | Timing flags | Memory flags | Flag families |
| ---: | ---: | ---: | --- |
| 1 | 13 | 8 | timing: durable command, enqueue batch, journal save, snapshot wire; memory: private after samples, private growth, working set after load/samples |
| 2 | 5 | 6 | timing: blocked-I/O snapshot, journal save, snapshot wire; memory: working set after load/samples |
| 3 | 13 | 7 | timing: durable command, journal save, snapshot, snapshot wire; memory: private after samples, working set after load/samples |

The persistent working-set difference and remaining timing-tail flags are unresolved. The passing hard gates do not override `review_required`, and this evidence does not identify a cause.

## Matched frontend comparison

`target/engineering-matched-frontend-performance-comparison.json` records 18 successful browser executions: three repeats for baseline and candidate at queue sizes 1, 100, and 1000. The comparison schema reports `thresholdsPassed: true` and `qualified: true`. Each run used the synthetic renderer performance harness for about 60 seconds; this is browser/JavaScript evidence rather than native-app or live-network evidence.

The comparison binds baseline commit `242d3270017dcb2c50631d9327e599b8d1ce5023` and candidate commit `237c2a24ba56ee0155e9483d659babcf5236c668`, production hashes `5ccde857d8421686128d3f2b0d0f80549ca4d8407217cefabdde33e1f9dad037` and `e14411cf6b43d0880fc776129fd0d59fba747f207cacb03a6237dc60d96ffc2f`, and harness hash `16ddfe3f451443975b43d16661986fb53070a7d3613929d433bd5d242b288b81`. Driver, Node, and browser executable hashes were also pinned. These source hashes are compilation/package evidence and are not a final working-tree rehash.

Candidate median results by queue were:

| Queue | Input-to-paint | Workload frame p95 | Reducer p95 | Progress dispatch p95 | State-delta dispatch p95 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 13.455 ms | 7.005 ms | 0.005 ms | 0.055 ms | 0.280 ms |
| 100 | 12.895 ms | 7.000 ms | 0.005 ms | 0.070 ms | 0.265 ms |
| 1000 | 11.905 ms | 7.005 ms | 0.005 ms | 0.070 ms | 0.285 ms |

Every run passed the configured limits: elapsed at most 61 seconds, reducer and dispatch p95 below 5 ms, workload-frame p95 below 16.7 ms, input-to-paint below 100 ms, and longest task at most 100 ms.

## Playlist admission benchmark

`target/playlist-performance/20260913T223159Z-78e0fe6ac6674e249b70f9eb79f5bff1/summary.json` records three repetitions of `synthetic-durable-playlist-admission`. Every repetition passed at counts 1, 100, and 1000, for nine passing measurements. This debug-profile benchmark uses no network or extractor work and does not measure renderer or end-to-end UI behavior.

| Repeat | 1 item | 100 items | 1000 items |
| ---: | ---: | ---: | ---: |
| 1 | 10 ms | 20 ms | 195 ms |
| 2 | 7 ms | 14 ms | 155 ms |
| 3 | 8 ms | 13 ms | 167 ms |

The gates were at most 1,000 ms for counts 1 and 100 and at most 2,000 ms for count 1000. The summary binds revision `dd224013e8fe613fac98d41a399514ba75caeedd`, compilation-recorded source SHA-256 `26ef5a9c625051c20bcda199f7d536c0a8bdd3d83c08a7f198a288bf6caf1e8f`, and executable SHA-256 `5236e2a512517f40884e8b7214d6a175495ee23174ed17ff9294d2c843c4092d`. The source hash is compilation evidence; independent final source verification remains separate.
