# Renderer lifecycle soak results

The frozen candidate renderer soak passed for 7200 seconds across 468194 mounts. The four workflow counts were active operation 117049, late listener 117049, delayed startup 117048, and completed workflow 117048.

Source commit: `fd58050b054f921d315658880a14793fb93fcae6`. Runner harness: `91543708afbfc8c7d1d9163d0ebf37da6d8db605677f4792a3de0b4d33e12f50`. Lifecycle soak test: `3357bcbc3e148477d84eac851a5252a24b8845ceb7f0ad1fa084b31a7678fc88`. Node: `v22.23.1` / `f8d162c0641dcee512132f3bcf8a68169c7ecb852efd8e1a46c9fec5a0f469ed`. Inputs and Node remained unchanged.

The passing Vitest test applied 2,340,970 listener-unlisten assertions and 468,194 assertions each for owned-timer cleanup, no post-unmount IPC admission, one ResizeObserver, and exactly one observer disconnect.

JavaScript heap started at 66312136 bytes, reached a recorded maximum of 334632792, ended at 206790232, and measured 71325656 after controlled GC. These values are observational; no memory threshold was added. PID-and-start-time-bound `probeVersion: 2` process memory ranges are recorded in the JSON report.

The periodic heap samples form a collection sawtooth rather than a monotonic trajectory. The final pre-GC heap was above baseline, while controlled post-GC heap was 5,013,520 bytes above baseline. Across 102 PID/start-bound samples, the two observed Node-process private-byte deltas were 3,338,240 and 2,334,720 bytes, and their handle counts were flat. The final observation found no owned Node process. These finite-run observations do not establish a long-term memory bound or rule out a leak.

This test uses Vitest jsdom and mocked Tauri IPC. It does not exercise or qualify the native renderer, native IPC, backend, GPU, or OS renderer resources.
The [companion JSON](./internal-cleanup-stage5-renderer-soak.json) retains exact measurements, assertions, and evidence identities.
