# Inspection latency and shared-parent playlist correction

The manual test reported approximately 20 seconds from Add to the playlist picker,
followed by a selected-entry error on an X post. The t.co URL in the error was part
of the title; the inspected source was
`https://x.com/LLMenjoyer/status/2097804593132671192`.

## Findings and changes

The pinned yt-dlp 2026.07.04 extractor returned a playlist containing two fully
resolved videos, both with the same parent `webpage_url`. The old discovery path
ran yt-dlp twice, deduplicated entries by URL, and then inspected the parent URL
again when the user selected a child. This both lost the second entry and returned
another playlist where the frontend required a video.

Discovery now uses one bounded `--flat-playlist --dump-single-json` operation.
Single-video URLs still receive full metadata, while ordinary flat playlist
entries retain their independent child URLs. Embedded videos without an
independent page retain an optional media selection: extractor key, media ID,
and authoritative playlist index. Missing indexes are rejected rather than
inferred from array position.

Selected inspection requests exactly that ordinal and verifies the returned
extractor and media ID. The authoritative inspection result supplies the selector
to the queue; saved schema-1 queues and retry requests retain it. Existing records
without selectors remain valid. The renderer uses URL plus extractor/media ID for
deduplication, retained metadata, and keyed playlist rows, allowing both siblings
to be selected without changing the dialog's markup or controls.

Downloads carry the selector and write the actual media ID and extractor key into
the machine-readable final-output record. Both ordinary and explicit WebM paths
verify that record before publication. A missing, ambiguous, or mismatched selected
output record fails without falling back to a guessed file. A changed source
playlist can therefore require reinspection, but cannot silently publish a
different selected clip.

Captured JSON now uses its cumulative byte budget rather than the 64 KiB progress
line limit. The inspection budget remains 8 MiB, stderr remains bounded, and
streamed progress lines retain their 64 KiB limit. This allows legitimate large
playlist documents without weakening the aggregate output bound.

A further review found that successful child inspection could race playlist
cancellation and still admit a video. The playlist continuation now checks the
cancellation request, dismisses the completed inspection, and stops before queue
admission or inspecting another child. Its new regression failed on the previous
continuation and passed after the guard was added.

## Live metadata evidence

Hidden metadata-only probes used the bundled yt-dlp executable with SHA-256
`52fe3c26dcf71fbdc85b528589020bb0b8e383155cfa81b64dd447bbe35e24b8`.
No cookies or video downloads were used for these probes.

| Probe | Observed elapsed time | Result |
| --- | ---: | --- |
| Old first-item inspection | 3,708 ms | Parent playlist, first child |
| Old additional flat discovery | 3,728 ms | Two children sharing the parent URL |
| New single-pass discovery | 3,716 ms | Both children retained |
| Explicit selection 1 | 3,566 ms | Twitter media `2049184998117588992` |
| Explicit selection 2 | 3,262 ms | Twitter media `2097430424205295616` |

These are individual network observations, not a statistically controlled
benchmark or a new end-to-end Add-button measurement. The old extractor sequence
took 7,436 ms in these probes versus 3,716 ms for one-pass discovery. The user's
reported 20-second UI delay still requires a retest of the rebuilt native app.
Raw probe metadata and timing receipts are retained locally under
`target/inspection-fix/`; expiring media URLs are not committed.

## Regression evidence

- Rust: 332 passed, zero failed, three opt-in long-running harnesses ignored.
- Frontend: 188 passed, zero failed, one opt-in renderer soak skipped.
- The new mounted-page test renders two same-parent rows and verifies both
  selection-bearing IPC requests and queue admissions.
- Rust cases cover selection validation, old-state compatibility, both child
  identities surviving persistence/restart/retry, selected inspection rejection,
  exact final-output identity, metadata bounds, and preserved progress limits.
- Svelte/TypeScript checking, strict ESLint, Prettier, production build and
  test-hook exclusion, strict Clippy, architecture checks, 19 inventory/architecture
  fixtures, packaging contracts, and acceptance-evidence contracts passed.
- All 11 pinned-browser workflow cases passed against the final source; receipt:
  `target/renderer-checks/workflows-20260910T004210Z-c07b6f4d042944e998d7f5986c721ccc/run-receipt.json`.
- All 60 visual scenarios passed exact decoded-pixel and semantic comparison at
  browser-emulated 100% and 150% scale. The final source hash is
  `6291fb519a589fc98921b1efdb042a9373c2e285392a5f6520fc70447a36ce2e`;
  [the comparison receipt](inspection-playlist-visual.json) binds both captures.
- Eighteen frontend inventory, performance-comparison, and style-contract fixtures
  passed.
- The backend source review covers all 1,246 current callable units; the frontend
  source inventory also matches the final production source.

Logs are retained under `target/inspection-fix/`. The first Rust/frontend runs
exposed fixture expectation mismatches; their failed logs remain retained. The
oversized-output fixture was updated to actually exceed its 128 KiB cumulative
budget after removing the inappropriate captured-JSON line limit.
The separate failing cancellation regression is retained as
`playlist-cancel-red.log`; it demonstrated an actual unwanted admission before
the production correction.

An initial visual attempt was invalidated by a concurrent production build
reloading Vite's generated client files; the successful final captures ran
serially after the build. No replacement baselines were approved. These browser
comparisons do not qualify native Windows display scaling.

After the user approved reopening the app, the previous instance was already
closed. The native executable was rebuilt offline from source
`8466d7be60192de1ea0539e19bbad470b658a15e` with the embedded production frontend
and launched once at `2026-09-10T01:03:18Z`; its Nuclear Downloader window was
verified. Executable SHA-256:
`af73bed742af78c9f1d55ff78635907c4b666ee6581c2227c053c25039b4604f`.
The build and launch receipts are retained under
`target/manual-launch/inspection-fix-8466d7b-90423cd133dd452b935e9a9d79993c0d/`.
The user's native workflow and Add-button timing retest remain pending.

The two-hour soaks and native installer/portable, cookie, and signed-update
qualification have not been rerun for these changes. This bug-fix evidence does
not qualify a release. No push or publication is included.
