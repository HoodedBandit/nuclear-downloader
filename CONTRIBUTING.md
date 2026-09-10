# Contributing

Nuclear Downloader is source-available under the repository license. The license does not grant permission to copy, modify, or distribute the code. Obtain explicit written permission from the copyright holder before submitting code or documentation changes.

Use the GitHub bug-report form for reproducible problems. Include the application version, Windows 11 x64 release and build, source site, requested format, and cookie mode. Share only redacted diagnostics. Never upload cookies, browser cookie databases, credentials, access tokens, private URLs, or logs containing personal file paths or account data.

If you have written permission to contribute, keep changes focused and preserve the repository gates. Before proposing a change, run the checks in the [testing guide](docs/testing.md) and [developer setup](docs/quickstart.md) that apply to the files changed. Do not regenerate release artifacts, publish assets, or change application/runtime versions as part of an unrelated contribution.

Use the [frontend ownership map](docs/frontend-ownership.md) and [backend module map](docs/backend-maintainability.md) to find the responsible owner. Keep saved-state authority in Rust and presentation concerns in the renderer. Backend changes must refresh the [source-matched review ledger](docs/backend-method-review-schema.md); frontend source changes must refresh its [inventory](docs/frontend-ownership.md). Preserve features and existing UI output unless a behavior change is explicitly agreed.

Report executed checks separately from blocked or unrun checks. Visual baselines are reviewed evidence and must not be replaced automatically. Performance and soak results apply to their exact recorded candidates; a new build needs its own release qualification. Maintainer work currently stays on `main`; source pushes and [release publication](docs/release-process.md) are separate actions.
