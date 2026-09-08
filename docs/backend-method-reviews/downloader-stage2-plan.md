# Downloader Stage 2 leaf extraction plan

Source identity: the entries in `downloader.json` use the exact IDs, spans, cfg values, signatures, and SHA-256 digests from `docs/backend-method-review.json`. This plan changes module ownership only. It does not include the two confirmed publication fixes or the later notification-injection work.

## Frozen behavior boundaries

- `downloader.rs` remains the facade used by `lib.rs`, `state.rs`, and sibling modules.
- `start_download`, `DownloadOutcome`, `validate_download_request`, `validate_fetch_request`, and `validate_output_directory` keep their current facade paths and signatures through private imports or `pub(crate)` re-exports.
- `inspection`, `process`, and `publication` keep their current externally referenced paths in this stage.
- Command argument order, error codes/messages, filename normalization, URL/cookie/config validation, progress math, runtime lease acquisition, and staging/publication behavior remain byte-for-byte or structurally equivalent.
- No AppHandle or crate-root callback removal occurs in Stage 2. `DownloadNotifications` belongs to Stage 4.
- The oversized marker and partial-stage stage defects receive failing regressions and isolated fixes after Stage 1; they are not mixed with file moves.

## Extraction order

1. Add `downloader/validation.rs` and move pure policy plus output validation:
   - URL, fetch/download request, cookie/config, actionable-length, format, and quality validation.
   - `validate_output_directory`, including Windows free-space FFI, moves last within this leaf so its synchronous I/O remains visibly separated from pure validation.
   - Facade re-exports the three currently consumed validators.
   - Move existing rejection tests with the functions. Add no behavior tests merely to mirror the move.

2. Add `downloader/naming.rs` and move naming/path derivation:
   - Windows reserved stems, UTF-16 truncation, filename sanitization/normalization, template escaping/building.
   - UTF-8 path projection plus final/staged WebM paths, generic final path, and collision suffix naming move from `publication.rs` only after imports point inward to `naming` rather than back through the facade.
   - Move filename/template/WebM-path tests with these functions.

3. Add `downloader/command_args.rs` and move deterministic argv construction:
   - yt-dlp runtime isolation, cookie, Twitter syndication, video selector, post-processing, complete download arguments, and final-output-record arguments.
   - Keep a small test-only empty-runtime wrapper beside the argument tests.
   - `inspection.rs` imports cookie/runtime/Twitter builders directly from this leaf. `publication.rs` no longer owns command construction.

4. Add `downloader/errors.rs` and move process error classification:
   - Twitter stderr classifiers/retry decision, non-actionable line filtering, summary construction, `DownloadErrorInfo`, simple/classified failures, and fetch-summary projection.
   - Keep `DownloadOutcome` at the facade during Stage 2; its conversion may use the leaf type privately.
   - Move classification and DRM-summary tests.

5. Add `downloader/progress.rs` and move pure progress parsing only:
   - yt-dlp regex parsing helpers/constants and FFmpeg time/percentage parsing.
   - The AppHandle-bound `emit_progress` and the two process callbacks remain in the engine until Stage 4 notification injection, avoiding a temporary cyclic notification abstraction.
   - Move FFmpeg parsing tests.

6. Reduce `downloader.rs` to the facade plus orchestration:
   - `ProgressFields`, `DownloadOutcome`, terminal conversion, probes/conversion/download attempts, WebM orchestration, and `start_download` remain together initially.
   - Once leaf imports are stable, orchestration may move as one unit to `downloader/engine.rs`; the facade re-exports `start_download` and `DownloadOutcome` without changing callers.
   - `process.rs`, `inspection.rs`, and `publication.rs` keep resource-owning behavior intact.

## Dependency direction

```text
downloader facade
  -> engine
      -> validation
      -> naming
      -> command_args -> naming
      -> errors
      -> progress
      -> inspection -> validation, command_args, errors, process
      -> publication -> validation, naming, process
      -> process -> runtime
```

Leaf modules must not import the facade to reach sibling helpers. During each move, update sibling imports to their owning leaf first; this prevents hidden cycles and keeps the facade as an API boundary rather than an implementation dependency.

## Verification after each move

1. `cargo fmt --manifest-path nuclear-app/src-tauri/Cargo.toml --all -- --check`
2. The moved module's existing focused tests by exact module filter.
3. `scripts/test-backend.ps1` after each complete leaf extraction batch.
4. Ordinary parallel all-feature lib tests after all leaves move, preserving the counter/interference coverage.
5. Strict all-target/all-feature Clippy and normal offline build at the Stage 2 gate.

The tests named in `downloader.json` are the required behavior anchors. Any unexpected behavior change stops the extraction; it is not repaired inside the move commit.

## Deferred findings and Stage 4 seam

- Confirmed: ownership-marker reads are not bounded before allocation.
- Confirmed: marker creation failure can strand a backend-created partial stage that future cleanup refuses to own.
- Risk: final-output record validation has a metadata/read race and allocates before its post-read cap.
- Risk: staged-file validation is path-based and does not retain an identity lease through publication.
- Risk: synchronous output-directory I/O can block an async worker, especially on a remote or unhealthy path.
- Policy decision: credential-bearing HTTP(S) URLs are currently accepted and passed as child-process arguments.

Stage 4 replaces `AppHandle` in the engine with root-owned `DownloadNotifications`. Its async progress callback must preserve authoritative state application before legacy event delivery; its cleanup callback must preserve bounded diagnostics without changing a published completion outcome.
