# Clarity: UI redesign and quality cleanup

Version 0.7.9 replaces the previous interface with the approved Clarity design.
The actual light and dark renderer screenshots are in [the README](../README.md).
The download destination staging-folder policy is unchanged in this release.

## User experience

The sidebar filters All downloads, In progress, Queued, and Completed. Search
and selection operate on the same authoritative queue projection. Format,
quality, and destination defaults stay next to link entry. Advanced access,
appearance, download tools, app updates, diagnostics, and detailed errors live
in Settings. Light, Dark, and System appearance choices persist across restarts.

Errors are recorded synchronously in a bounded 200-entry session history. A
single failed attempt has one entry even if both a progress event and a rejected
command report it. A later failure can notify again. Opening Settings marks all
entries read; new errors while it is open are read immediately. Recovery clears
active warnings while preserving history. There is no notification-clear button.

Click a prepared ready or waiting title to edit its filename. The input receives
focus and selects the current name. Enter or blur saves; Escape cancels. Invalid
names and rejected saves retain the draft and send details to Settings. Running,
converting, cancelling, and completed rows are read-only. Starting affected items
waits for the edit; failed saves prevent an unintended filename from starting.

## Ownership and backend consistency

- `AppSessionController` owns subscription readiness, startup coordination,
  snapshot recovery, operation waiters, and disposal.
- `QueueViewController` owns filtering, selection, and viewport behavior.
- `FilenameEditorController` owns draft generations and deduplicated commits;
  each rendered row owns its input focus reference.
- The presentation controller retains queue projection, metadata preparation,
  and progress reconciliation. Typed action/command ports connect these owners.
- `UiErrorReporter` separates active failures, attempt identities, and history.
  Runtime health failures and runtime update failures have independent state.
- Focused Svelte components render Settings, download defaults/access, runtime,
  diagnostics, Help, and queue rows. Retired SettingsRow and RowDiagnostics
  implementations are removed.

The existing `update_queue_item` command accepts filename-only changes to a
prepared waiting item only while its download operation is queued and present
in the pending list. Eligibility and commit share the worker-claim mutation
gate. A claim that wins first produces a failed rename with the draft retained;
a rename that wins first reaches the worker. Other waiting-item settings remain
read-only. Operation identity, queue order, durable saves, and rollback remain
backend-owned.

The shared staging directory now tolerates simultaneous creation by legitimate
downloads while retaining regular-directory, ownership, and containment checks.

## Preview verification, distinct from official release acceptance

The isolated preview executable had SHA-256
`d0536073d7e187282c1165075a6c318110f211114bf9ff8d71d8e6014af49193`.
It was packaged as a preview of the new code before the 0.7.9 version bump.

| Executed check | Result |
| --- | --- |
| Frontend tests | 236 passed, one existing opt-in test skipped |
| Rust tests | 396 passed, four existing tests ignored |
| Browser interactions | 31 passed, including both themes, normal/minimum sizes, and 100%/150% emulation |
| Type checking | Zero errors and warnings |
| Lint, formatting, strict Clippy | Passed |
| Source health, architecture, inventories | Passed; no increased limits or added exemptions |
| Native local fixture | Six correctly named outputs matched exact fixture bytes |
| Native restart | Renamed files, completed history, and Dark preference persisted |

The native fixture exercised both ready and waiting renames through pointer
clicks in the real preview. Browser fixtures use mocked IPC and are not native
verification. Native Windows display scaling was 100%; native 150% remains
unverified. Installed application/profile/shortcut hashes stayed unchanged.
Raw logs, screenshots, and media are retained in ignored local QC directories.

The official release uses the [protected release process](release-process.md).
Its exact source, candidate run, asset hashes, signatures, automated acceptance,
and remaining manual qualification are bound to that candidate. Earlier preview
tests and two-hour soak records are not evidence for rebuilt release artifacts.
