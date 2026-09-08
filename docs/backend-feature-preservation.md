# Backend feature preservation

Starting contract: commit `34d0769`. A structural extraction must preserve every
row below. Automated coverage is evidence for the behavior it actually exercises;
the native/signed acceptance requirements remain separately open.

## Public commands

The registered command set and argument/result shapes remain exact:
`begin_inspection`, `cancel_operation`, `dismiss_operation`,
`cancel_all_downloads`, `get_app_snapshot`, `add_inspection_result_to_queue`,
`update_queue_item`, `remove_queue_items`, `enqueue_queue_items`,
`check_downloader_runtime`, `check_runtime_update`, `begin_runtime_update`,
`default_download_dir`, `validate_output_directory`, `check_app_update`,
`begin_app_update`, `export_diagnostics`, and `clear_diagnostics`.

Generated TypeScript declarations and serde names/defaults remain unchanged.
Authoritative state events keep `app-state-changed` and
`app-state-resync-required`; legacy progress retains `download-progress`,
`downloader-runtime-update-progress`, and `update-install-progress`.
Native runtime event spelling is checked against its producer before extraction.

## Behavior and evidence

| Behavior to preserve | Automated evidence retained | External evidence still needed |
| --- | --- | --- |
| Inspect HTTP/HTTPS media, metadata and quality choices | Inspection parsing, URL validation, metadata-budget and deadline tests | Maintainer-controlled YouTube/X fixtures on candidate |
| Playlist discovery, bounded entries, deduplication and selection | Playlist parser tests; loopback native fixture source; renderer workflows | Packaged native playlist workflow |
| Queue add consumes authoritative inspection; edit, remove, priority and explicit retry | State queue/transition tests; frontend reducers/controllers; native retry source | Packaged native queue workflow |
| Preserve queue IDs and valid history across restart, retention and dismissal | Dangling reference, seven-day/count retention, journal restart tests | Forced interruption with installed candidate |
| MP4/MKV/WebM video choices and selected quality | Format selector, remux, capped fallback, explicit WebM conversion tests | Native video output and conversion |
| MP3/FLAC/WAV/AAC/Opus audio choices; reject audio-only output for silent items | Audio argument tests; queue validation; native MP3 source | Native media output validation |
| Cookie browser/file modes and compatibility configuration | Request and command construction tests; error mapping/redaction | Dedicated-account manual cookie acceptance |
| Preserve X syndication retry and extractor-specific error guidance | X URL/auth retry and error classification tests | Controlled X extractor smoke |
| Custom names, Windows reserved names, Unicode limits and literal percent escaping | Filename and output-template tests | Native collision/custom-name workflow |
| Publish only validated final output without overwriting existing files | Machine-record, ambiguous fallback, atomic collision and publication tests | Exact candidate native publication |
| Cancel queued/running work and all work; late drain reopens admission | Registration/dequeue/cancel/panic/drain deterministic regressions | Native child-process cancellation |
| Five download slots, one inspection, one explicit WebM conversion | Lifecycle capacity and worker registration checks | Mixed native workload |
| Terminal state follows durable save; degraded persistence preserves outcome and path | Immediate/250ms/1s retry, compensation and next-command flush tests | Disk failure behavior under native deployment |
| No automatic redownload or resume after interruption | Journal interruption, retry identity and published-outcome tests | Installed-app restart workflow |
| Bounded journal, inspection payloads, display fields and ordered outbox | Oversize preservation, retained-allocation, concurrent producer/resync tests | Sustained native workload |
| Runtime readiness, optional Deno, shared verified executable leases | Managed/bundled integrity, optional-tool and hash/lease tests | Candidate runtime health on clean Windows |
| Same-version repair, authenticated promotion/rollback, repeated recovery | Ownership tests and every durable checkpoint recovery fixture | Signed managed-runtime update/rollback |
| Invalid owned installer cache recovers; unrelated entries remain | Cache quarantine, owner, lock, lease and preservation tests | Signed app update with exact candidate |
| Persist installer handoff before launch and reconcile target on restart | Handoff durability, version match/mismatch and cancellation tests | Real installer handoff/relaunch |
| Startup recovery/cleanup precedes admission; shutdown waits for tracked/protected work | Startup, single-instance, shutdown and publication guard tests | Clean desktop lifecycle acceptance |
| Export/clear bounded, redacted diagnostics | Diagnostic rotation/redaction tests; renderer/native workflow source | Native export/clear workflow |
| Production bundle contains no WebDriver/mock hooks | Production-bundle and packaging/acceptance contracts | Exact installer/portable artifact qualification |

## Completion rules

The method review links methods to these workflows and individual tests. Test moves
retain their test function names and assertions. Every confirmed defect has a
separate failing trigger, passing correction, and commit reference. Discovered
false positives are explicitly closed with evidence, not patched speculatively.

The starting local suite passed 270 Rust and 64 frontend tests. All local gates
must be rerun after integration, with fresh performance and two-hour soak evidence
bound to the refactored source. Existing external qualification gaps remain visible
in `backend-qualification.md`; no passing fixture test closes those gaps.
