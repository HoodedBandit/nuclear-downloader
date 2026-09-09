# Backend method reviews

These sidecars contain the completed source review for all 1,223 backend review units.
The original review covered source commit `d1f6571ed9da69e7aa63b2cdf47818e22f90161b`;
the internal cleanup refreshes moved declarations and their ownership context
against the current source inventory. Every entry was
reconciled to the final inventory and reviewed with explicit callers, effects,
ownership, cancellation, invariants, and concrete or static-only test evidence.
The merged canonical ledger is `../backend-method-review.json`.

CI and release-candidate builds run
`python scripts/inventory-backend-methods.py check`, which fails when source
identity changes or a current review unit lacks an accepted review. This completes the
source method-review gate. The previous Stage 6 two-hour isolated soak passed for
its historical candidate; it does not qualify the current internal cleanup.
Historical evidence is in `../backend-maintainability.md` and
`../backend-qualification.md`. Current follow-up checks are recorded in
`../internal-cleanup-pretest-followup.md`; the previous candidate's completed
performance and soaks remain historical after the follow-up source changes.
Extraction history and the outstanding native and external gates are recorded in
`../backend-internal-cleanup-review.md` and `../internal-cleanup.md`.
