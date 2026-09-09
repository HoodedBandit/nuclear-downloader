# Backend performance results

- Baseline: `df582df4ddc566712729b7006dfb080374c5ef45`
- Candidate: `fd58050b054f921d315658880a14793fb93fcae6`
- Profile/order: `debug`, AB/BA/AB
- Hard gates: passed in all three comparisons
- Review counts: 2, 0, 22 (timing 0, 0, 22; memory 2, 0, 0)

Across these three existing comparisons, the flagged metric sets did not reproduce: repeat 1 flagged two queue-1 working-set rows, repeat 2 flagged none, and repeat 3 flagged 22 timing rows. This pattern is consistent with substantial run-to-run variation. It does not prove that the candidate has zero performance effect, establish causality, or define a new pass rule.

The repeat-1 flags were `queue-1/workingSetAfterSamples` (+110.0%) and `queue-1/workingSetGrowth` (+580.2%). Repeat 3 flagged the single enqueue observation at queue 1; durable-command, journal-save, and the single enqueue observation at queues 100 and 1000; and queue-1000 blocked snapshot latency. The other matched repeats did not reproduce those threshold crossings.

## Latency across matched repeats

The change cells report the median and range of the three **paired per-repeat relative changes**. They are not computed by dividing the displayed baseline and candidate medians, so rounded raw medians can appear inconsistent with the paired-change summary. The [companion JSON](./internal-cleanup-stage5-backend-performance.json) contains every absolute p50, p95, and p99 value for every repeat.

| Queue | Metric         | p50 median change (range) | p95 median change (range) |  p99 median change (range) | Flagged repeats |
| ----: | -------------- | ------------------------: | ------------------------: | -------------------------: | --------------- |
|     1 | snapshot       |    +0.0% (+0.0% to +0.0%) |  +0.0% (+0.0% to +100.0%) | +100.0% (+0.0% to +600.0%) | none            |
|     1 | snapshotWire   |    +0.0% (+0.0% to +0.0%) |   +5.0% (+0.0% to +85.0%) |  +47.1% (+45.8% to +59.1%) | none            |
|     1 | durableCommand |    -3.9% (-6.4% to -0.7%) |    -1.8% (-2.6% to +0.9%) |     -1.1% (-6.8% to +5.4%) | none            |
|     1 | journalSave    |   -3.1% (-12.7% to +6.7%) |   -1.7% (-15.6% to +3.3%) |    -4.5% (-21.9% to +3.5%) | none            |
|     1 | enqueueBatch   |   +3.0% (+2.8% to +20.0%) |   +3.0% (+2.8% to +20.0%) |    +3.0% (+2.8% to +20.0%) | 3               |
|   100 | snapshot       |    -3.0% (-4.3% to +3.4%) |    +1.4% (-4.6% to +9.0%) |   +13.7% (-1.0% to +22.8%) | none            |
|   100 | snapshotWire   |    -0.7% (-2.9% to +0.3%) |    -2.5% (-4.2% to +2.5%) |    -4.6% (-17.2% to +2.3%) | none            |
|   100 | durableCommand |  +0.6% (+0.3% to +113.5%) |  +0.9% (-1.6% to +145.4%) |  -0.8% (-25.8% to +118.8%) | 3               |
|   100 | journalSave    |  +4.7% (-0.4% to +126.0%) |  +5.5% (-6.6% to +165.2%) |   +0.8% (-8.7% to +151.2%) | 3               |
|   100 | enqueueBatch   |   -0.4% (-1.8% to +73.9%) |   -0.4% (-1.8% to +73.9%) |    -0.4% (-1.8% to +73.9%) | 3               |
|  1000 | snapshot       |    -3.2% (-3.8% to -2.1%) |    -4.7% (-5.9% to -1.9%) |     -4.7% (-6.4% to -1.9%) | none            |
|  1000 | snapshotWire   |    -1.7% (-3.0% to -0.5%) |    -5.6% (-7.3% to +3.0%) |     -1.9% (-2.2% to +2.1%) | none            |
|  1000 | durableCommand |   -0.3% (-1.0% to +17.1%) |   +3.5% (-2.0% to +21.8%) |    +2.6% (+0.4% to +20.9%) | 3               |
|  1000 | journalSave    |   +2.1% (-0.4% to +19.0%) |   +4.1% (-3.0% to +25.6%) |    +2.4% (-2.3% to +25.5%) | 3               |
|  1000 | enqueueBatch   |  -6.4% (-17.8% to +28.5%) |  -6.4% (-17.8% to +28.5%) |   -6.4% (-17.8% to +28.5%) | 3               |

`enqueueBatch` contains one observation per queue size per run. Its p50, p95, and p99 columns repeat that observation.

## Memory and blocked I/O

| Scope/metric                      | Unit         |      Baseline median (range) |     Candidate median (range) | Paired change median (range) | Flagged repeats |
| --------------------------------- | ------------ | ---------------------------: | ---------------------------: | ---------------------------: | --------------- |
| queue-1/workingSetAfterSamples    | bytes        | 11735040 (11726848–11808768) | 11776000 (11755520–24793088) |     +0.4% (+0.2% to +110.0%) | 1               |
| queue-1/workingSetGrowth          | bytes        |    2179072 (2170880–2236416) |   2224128 (2199552–15212544) |     +2.5% (+0.9% to +580.2%) | 1               |
| queue-1/privateAfterSamples       | bytes        |    2043904 (2043904–2138112) |    2109440 (2056192–2142208) |       +3.2% (-3.8% to +4.8%) | none            |
| queue-1/privateGrowth             | bytes        |       561152 (552960–626688) |       630784 (544768–655360) |    +12.4% (-13.1% to +18.5%) | none            |
| queue-100/workingSetAfterSamples  | bytes        | 12947456 (12800000–13078528) | 12865536 (12816384–13012992) |       +0.1% (-1.6% to +0.5%) | none            |
| queue-100/workingSetGrowth        | bytes        |    3141632 (2998272–3272704) |    3051520 (3002368–3194880) |       +0.1% (-6.8% to +1.7%) | none            |
| queue-100/privateAfterSamples     | bytes        |    3358720 (3153920–4075520) |    3268608 (3190784–3915776) |     +1.2% (-19.8% to +16.6%) | none            |
| queue-100/privateGrowth           | bytes        |    1630208 (1425408–2330624) |    1531904 (1454080–2170880) |     +2.0% (-34.3% to +33.2%) | none            |
| queue-1000/workingSetAfterSamples | bytes        | 19529728 (18636800–20090880) | 18698240 (18677760–18857984) |       -4.3% (-7.0% to +1.2%) | none            |
| queue-1000/workingSetGrowth       | bytes        |    7782400 (6864896–8327168) |    6926336 (6881280–7069696) |     -11.0% (-17.4% to +3.0%) | none            |
| queue-1000/privateAfterSamples    | bytes        |  10293248 (9138176–10563584) |    9326592 (9277440–9388032) |      -9.4% (-12.2% to +2.7%) | none            |
| queue-1000/privateGrowth          | bytes        |    5988352 (5087232–6590464) |    5320704 (5210112–5611520) |     -13.0% (-14.9% to +4.6%) | none            |
| queue-1/snapshot                  | microseconds |                   11 (10–18) |                    10 (9–14) |    -10.0% (-44.4% to +27.3%) | none            |
| queue-100/snapshot                | microseconds |                 103 (98–191) |                 128 (84–129) |    -14.3% (-33.0% to +25.2%) | none            |
| queue-1000/snapshot               | microseconds |               945 (899–1259) |               986 (906–1008) |     +4.3% (-28.0% to +12.1%) | 3               |

Memory values are point-in-time Windows process snapshots, not allocation traces or leak evidence.

## Correctness and bounded resources

- Event sequences were contiguous in every candidate state workload.
- Runtime hash invocation/byte counts were preserved in all repeats, and every runtime resolution succeeded.
- All candidate outbox bounds passed; no coalesced resync occurred.
- Host identity fields matched for each pair: Windows display/build, processor, logical-processor count, and repository filesystem format.
- Runtime workload stayed at 5 hashes / 693 bytes (1 manifest / 610 bytes and 4 tools / 83 bytes), with 408 successful resolutions from 408 calls in every run.

| Queue | queued batches | queued deltas | estimated bytes |
| ----: | -------------: | ------------: | --------------: |
|     1 |            207 |           208 |           81647 |
|   100 |            207 |           406 |          160235 |
|  1000 |            207 |          2206 |          876635 |

## Evidence bindings

- `receipt.json`: `1ec7a84cd829fdd0b2954a803cf0f70f0c3d93e36b054da0bc68de91c6ec5394`
- `preflight-receipt.json`: `26e82784454b191a76d0f4d5b439fe94fe3506932c18913d7e032b6d73f06743`
- `after-warm-receipt.json`: `0ba66ebbe98d8c48f6be60b8bbcc7bfef3024f8cdd7b6c1731541e8343ae505d`
- `baseline-source-manifest.json`: `b645afd45e67c68b7f8782bb592ae1284682dab7b84a1f6ce6c6689d70351f7a`
- `candidate-source-manifest.json`: `5b073ea106f085221011c907e61768cbb63f7bf6bb6e1fc255a16591de9cec68`
- `matched-harness-manifest.json`: `bccf08a1fb16bf689782292fb373e5316d3e11ef51d4c6cea0f61b1ce44075b4`

Bound test executables:

- baseline: `576c69b4ed574e1243f03b9b14e998713e48b0a78976f3b705ac68a28672b3ad` (26300928 bytes, second warm fresh `true`)
- candidate: `bb8b15790eadb4073525b28526424e06c29b9d61e8809f7a19f69b771d5baf39` (26301440 bytes, second warm fresh `true`)

## Limits

- Three matched repeats characterize short-run variation but do not establish long-duration behavior.
- enqueueBatch has one sample per workload per run; its reported p50, p95, and p99 are the same single observation, not three independent percentile estimates.
- workingSet and private-memory fields are point-in-time Windows process snapshots affected by allocator state, paging, and host activity; they are not allocation traces or leak evidence.
- Timing flags are threshold review signals. The evidence does not establish that the refactor caused any observed difference.
- Debug-profile synthetic workloads do not qualify release-profile or interactive end-to-end performance.
