# Documentation

Nuclear Downloader 0.7.9 introduces Clarity: the new interface, Light/Dark/System
appearance, new icon, Settings error history, reliable filename editing, and
focused frontend/backend ownership. Start with the [project README](../README.md)
and [latest official release](https://github.com/HoodedBandit/nuclear-downloader/releases/latest).

## Current guides

| Document | Purpose |
| --- | --- |
| [Clarity changes and QC](clarity-0.7.9.md) | Current design, behavior, ownership, and preview evidence |
| [Frontend ownership](frontend-ownership.md) | Session, queue-view, filename, presentation, workflow, and component boundaries |
| [Backend ownership](backend-maintainability.md) | State, workers, processes, publication, and persistence |
| [Behavior checklist](frontend-behavior-baseline.md) | Current interactions and regression obligations |
| [Quickstart](quickstart.md) | Pinned Windows development tools and setup |
| [Testing](testing.md) | Frontend, Rust, browser, and native acceptance commands |
| [Local packaged build](local-packaged-build.md) | Guarded local executable/installer construction and isolated previews |
| [Release process](release-process.md) | Signed exact-byte candidates and protected publication |
| [Key rotation](key-rotation.md) | Update trust anchors and private-key custody |
| [Contributing](../CONTRIBUTING.md) | Bug reports, license, and contribution scope |
| [Source health](source-health.md) | Architecture budgets and removal of obsolete exceptions |
| [Backend review ledger](backend-method-review-schema.md) | Source-bound review identities and checks |
| [Renderer visual contract](frontend-visual-contract.md) | Deterministic comparison format and provenance |
| [Frontend validation tools](frontend-validation-tools.md) | Performance and soak tools |

## Validation and qualification

The [Clarity QC record](clarity-0.7.9.md#preview-verification-distinct-from-official-release-acceptance)
records the tested preview: 236 frontend tests, 396 Rust tests, 31 browser checks,
and six real local-media downloads with ready/waiting renames and restart
persistence. Existing skips and native 150% scaling gaps are explicit.

Official candidate artifacts are independently built, signed, installed, and
exercised by the protected Release Candidate workflow. Publish Release verifies
those same bytes before publication. Release notes state artifact-specific
acceptance and pending manual cases; old preview or soak results never qualify
a new executable. Browser fixtures use mocked IPC and are not native results.

## Historical records

These records describe the earlier interface and their original source revisions.
They explain decisions and retain evidence; they are not current UI instructions
or qualification for 0.7.9:

- [0.7.1 release and correction](release-process-0.7.1.md).
- [Earlier internal cleanup](internal-cleanup.md), including page, workflow,
  component, and backend extraction records linked there.
- [Engineering status at 2026-09-13](engineering-status.md) and
  [matched performance](engineering-performance-2026-09-13.md).
- [Earlier structural qualification](internal-cleanup-stage5-qualification.md),
  [backend soak](internal-cleanup-stage5-backend-soak.md), and
  [renderer soak](internal-cleanup-stage5-renderer-soak.md).
- [Inspection/playlist correction](inspection-playlist-fix.md) and
  [YouTube runtime correction](youtube-403-fix.md).

Raw target-directory logs, media, and archived test inputs remain local and
ignored by Git. Public screenshots in `docs/screenshots` are real renderer
captures with deterministic fixture data, not concept artwork.