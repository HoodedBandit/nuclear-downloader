# Stage 5 candidate qualification

Candidate: `fd58050b054f921d315658880a14793fb93fcae6`

Finalized: `2026-09-09T12:35:05Z`

## Completed gates

- Backend library tests: **304 passed, 0 failed, 3 ignored**. The receipt binds a newly compiled executable and unchanged source inputs; no separate doc-test claim is made. Strict Clippy, Cargo formatting, generated bindings, architecture, and all **1,215** source-review units passed.
- Frontend: **167 passed, 1 skipped**; Svelte/type checking, strict ESLint, Prettier, static build, and production mock exclusion passed.
- Renderer workflows and visual parity: **11 workflows** and **60 browser-emulated visual comparisons** passed. This does not qualify native Windows scaling.
- Matched frontend performance: **18/18 measurements** passed frozen thresholds across queue sizes 1, 100, and 1,000. See [Markdown](internal-cleanup-stage5-frontend-performance.md) and [JSON](internal-cleanup-stage5-frontend-performance.json).
- Matched backend performance: all **3 hard gates passed**. Review flags were **2, 0, and 22** across repeats; no threshold signal recurred in two repeats. This does not prove zero performance effect or causality. See [Markdown](internal-cleanup-stage5-backend-performance.md) and [JSON](internal-cleanup-stage5-backend-performance.json).
- `cargo-deny 0.20.2` passed advisories, bans, licenses, and sources after refreshing the public database to `d502590ca247f3e53b56bf6c2ae40b61926800e5` at 2026-09-09T10:26:17+02:00.

## Completed candidate-bound soaks

- Backend runner exited 0 after **7,203,355 ms**, **1,438 cycles**, **7,190 operations**, and **1,440 samples**. Its duration, workload, source/executable stability, resource-bound, final-journal, and cleanup assertions passed. Independent analysis covered all 1,440 samples with zero cap violations; maximum growth from the steady-state baseline was 3,686,400 private bytes, 3,801,088 working-set bytes, and 2 handles, and all 16 final assertions passed. See [Markdown](internal-cleanup-stage5-backend-soak.md) and [JSON](internal-cleanup-stage5-backend-soak.json).
- Renderer runner exited 0 after **7,200,016 ms** and **468,194 mounts**. Mode counts were active operation **117,049**, late listener **117,049**, delayed startup **117,048**, and completed workflow **117,048**. Inputs and pinned Node stayed unchanged. Independent analysis passed: after optional GC, heap use was 5,013,520 bytes above baseline; the periodic sawtooth remains observational with no arbitrary heap threshold. All 102 process samples had stable handles, owned Node cleanup passed, and the final 2026-09-09T12:31:21.5770749Z probe found zero renderer processes. This is Vitest jsdom with mocked Tauri IPC and JavaScript-heap observations; it is not browser, native renderer, backend, or OS-resource qualification. See [Markdown](internal-cleanup-stage5-renderer-soak.md) and [JSON](internal-cleanup-stage5-renderer-soak.json).

The JSON companion records SHA-256 and byte counts for all prior immutable evidence plus the closed raw soak receipts, manifests, logs, caller results, executable, samples, and probe observations. Failed benchmark setup attempts retain zero qualification credit.

## Open and deferred qualification

- Production `npm audit` remains pending the specific approval required to transmit dependency metadata to the registry.
- Clean Windows 11 native application, installer and portable artifact acceptance, and real 100%/150% display scaling remain deferred.
- Release construction and signing, authentic signed update/relaunch/rollback and sidecar download, dedicated-account cookie cases, and maintainer-controlled external extractor cases remain deferred.

Local fixtures, debug benchmarks, browser emulation, and jsdom/backend soaks do not close those external gates.
