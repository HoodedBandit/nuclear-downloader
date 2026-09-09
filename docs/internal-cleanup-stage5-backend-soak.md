# Backend soak evidence analysis

**Status: PASSED**

- Samples: 1440
- Last cycle/elapsed: 1438 / 7203 seconds
- Cap violations observed: 0

## Growth from cycle 12

| Metric                  | Baseline | Maximum growth | Final/current growth |     Limit |
| ----------------------- | -------: | -------------: | -------------------: | --------: |
| privateBytes (bytes)    |  4603904 |        3686400 |              1617920 | 134217728 |
| workingSetBytes (bytes) | 18718720 |        3801088 |              1900544 | 201326592 |
| handleCount (count)     |      150 |              2 |                    2 |        32 |

## Assertions

- completeReceiptPresent: `True`
- runnerPassed: `True`
- soakPassed: `True`
- warmupCycle12Present: `True`
- cycleSequencePassed: `True`
- sampleCapsPassed: `True`
- latestSequenceMonotonic: `True`
- runtimePassed: `True`
- finalJournalReopened: `True`
- fixtureCleanupPassed: `True`
- frozenExecutableUnchanged: `True`
- sourceAndExecutableBindingsPassed: `True`
- durationPassed: `True`
- finalSummaryMatchesSamples: `True`
- finalizedSamplesBound: `True`
- outerLaunchBindingPassed: `True`

## Final workload and bindings

- Cycles/operations: 1438 / 7190
- Completed/cancelled/failed: 4314 / 1438 / 1438
- Runtime resolutions: 5871 / 5871; hashes: 595 / 595 (82467 bytes)
- Resyncs/deltas: 4 / 64510
- Inherited-pipe drains, lifecycle drains, abandoned cleanups: 4 / 119 / 71
- Final journal reopened: `True`; owned fixture removed: `True`
- Finalized samples: `8abd5a3f8a7e727928289317f5236dfa23f41e2c44d6e380f04afad57f7239c9` (595885 bytes)

## Limitations

- This is an isolated Windows debug lib-test workload, not a release build or interactive desktop session.
- Private and working-set values are point-in-time Windows counters; they do not identify allocation ownership, establish causality, or prove the absence of a leak.
- The soak stresses queue/download lifecycle, subprocess supervision, publication cleanup, outbox resync, runtime mutation, journal reopen, and diagnostics bounds. It does not retain inspection or playlist payloads, so the 16 MiB inspection-retention budget and inspection-payload behavior were not stressed.
- Quiescent sampling can miss short-lived peaks between five-second cycles.

The [companion JSON](./internal-cleanup-stage5-backend-soak.json) retains exact measurements, assertions, and evidence identities.
