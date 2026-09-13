# Backend method reviews

The canonical ledger covers 1,382 current backend review units across 110 files:
789 production, 406 tests, and 187 test-support units. The September 13 refresh
is in `engineering-2026-09-13/`; earlier sidecars remain historical records.
The original review covered source commit `d1f6571ed9da69e7aa63b2cdf47818e22f90161b`;
the internal cleanup refreshes moved declarations and their ownership context
against the current source inventory. Every entry was
reconciled to the final inventory and reviewed with explicit callers, effects,
ownership, cancellation, invariants, and concrete or static-only test evidence.
The merged canonical ledger is `../backend-method-review.json`. Merge a refresh
from its dedicated sidecar directory, then run `check`; do not merge historical
and current sidecars indiscriminately.

CI and release-candidate builds run
`python scripts/inventory-backend-methods.py check`, which fails when source
identity changes or a current review unit lacks an accepted review. Current
implementation and execution status is in `../engineering-status.md`. The
previous Stage 6 two-hour isolated soak passed for
its historical candidate; it does not qualify the current internal cleanup.
Historical evidence is in `../backend-maintainability.md` and
`../backend-qualification.md`. Current follow-up checks are recorded in
`../internal-cleanup-pretest-followup.md` and the typed media-selection follow-up
evidence; the previous candidate's completed
performance and soaks remain historical after the follow-up source changes.
Extraction history and the outstanding native and external gates are recorded in
`../backend-internal-cleanup-review.md` and `../internal-cleanup.md`.
