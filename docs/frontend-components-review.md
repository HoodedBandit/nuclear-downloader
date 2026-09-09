# Frontend component extraction record

The comparison remains the fixed `df582df` production baseline. Stage 3's controllers retain workflow and backend-state ownership. Components receive typed values, callbacks, and the DOM references needed by the page lifetime; they add no layout wrappers or application state containers.

## Stylesheet boundary

Svelte's original scoped selectors contribute one class of specificity per selector. Moving markup into child components would stop those parent-scoped selectors from matching. The proposed route-owned `src/lib/styles/app.css` preserves the original 152 rules, 158 selector arms, declarations, comments, and rule order. Ordinary selectors gain one `:root` ancestor; the original root-variable and global-body rules retain their original specificity. This keeps the effective specificity of the compiled source while covering both the main window and its sibling modal layers.

`scripts/page-style-contract.mjs` compares the original source, the installed Svelte compiler's emitted CSS, and the extracted stylesheet. It checks selector order, normalized scope semantics, specificity, and declaration order. Its three portable fixtures include intentional specificity/order failures. The frozen Stage 2 oracle independently passed its 152-rule/158-selector check before production extraction. Browser comparisons remain required; this source check alone does not prove visual parity.

Shared styles remain in one route-owned sheet to preserve their interleaved cascade. Component boundaries preserve existing semantic elements, classes, accessibility attributes, focus behavior, scrolling, and control bindings. Queue virtualization remains in the queue presentation owner: 53-pixel rows, eight-row overscan. Playlist projection remains in its workflow owner with 100 entries per page.

## Gates

`scripts/check-frontend-extraction.ps1` runs type checking, strict lint, formatting, frontend tests, a production build and test-hook exclusion, all 11 browser workflows, and paired 100%/150% emulated-scale visual captures. It compares each candidate with the same fixed baseline and retains separate logs and a comparison receipt. It cannot generate or replace baselines.

| Extraction | Result | Evidence |
| --- | --- | --- |
| Route stylesheet and status footer | Passed: 161 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-footer-visual.json`; `target/frontend-extractions/footer-20260909T073205Z-02c69ace288b4035ba8876b963505e96/` |
| Row diagnostics | Passed the same 161-test, 11-workflow, and 60-scenario gates with zero visual/semantic differences. | `internal-cleanup-row-diagnostics-visual.json`; `target/frontend-extractions/row-diagnostics-20260909T073852Z-dd10d6060e4444d88718a456af7dce8f/` |
| Queue row | Passed: 165 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-queue-row-visual.json`; `target/frontend-extractions/queue-row-20260909T075535Z-1ba6be520db64eba9bbef1f18684d25e/` |
| Queue table | Passed: 165 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-queue-table-visual.json`; `target/frontend-extractions/queue-table-20260909T080123Z-b11a9056fb8947d6b985ca70a090f33f/` |
| Queue toolbar | Passed: 167 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-queue-toolbar-visual.json`; `target/frontend-extractions/queue-toolbar-20260909T080826Z-7eacabaac0984bef8a55c86e8a7888af/` |
| URL input | Passed: 167 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-url-bar-visual.json`; `target/frontend-extractions/url-bar-20260909T081406Z-0f8010ceb86b4118b175f3bf38ccb147/` |
| Settings controls | Passed: 167 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-settings-row-visual.json`; `target/frontend-extractions/settings-row-20260909T081936Z-3b63f5933d80486ba5c50af1667e495c/` |
| Runtime header | Passed: 167 frontend tests, 11 browser workflows, type/lint/format/build/exclusion checks, and all 60 exact visual/semantic comparisons. The opt-in soak was skipped. | `internal-cleanup-header-runtime-visual.json`; `target/frontend-extractions/header-runtime-20260909T082538Z-5a8e0e26ddaa4f2e807d5fa96e7399a5/` |

Native Windows/WebView2 and actual Windows scaling qualification remain deferred; browser emulation is not a substitute.
