# Source health contract

The source-health gate is a review ratchet for production source. It measures nonblank, noncomment lines, callable spans, and control-flow nesting with standard ceilings of 500 lines per file, 100 lines per callable, and five nested flow levels. Svelte instance/module scripts, template, and style are separate regions so a stylesheet cannot hide script or template growth.

Tests, `cfg(test)` bodies, generated bindings, and declared test-support files do not consume production budgets. Compact formatting does not evade nesting checks: braces and flow tokens are measured structurally. The frontend dependency check keeps queue presentation away from IPC, workflow modules away from Svelte components, and the route page limited to composition rather than domain method bodies.

An existing justified overage may be listed only by its exact measurement identity. Every exception records its responsibility, numeric ceiling, reason, considered alternative, and regression tests. The ceiling is the reviewed current bound: any growth fails until a human updates the review. The tools never generate or raise a baseline.

Run the local checks from the repository root:

```text
python -B scripts/source-health.test.py
python -B scripts/source-health.py
node --test scripts/source-health-frontend.test.mjs
node scripts/source-health-frontend.mjs
node scripts/frontend-source-inventory.test.mjs
node scripts/frontend-source-inventory.mjs --check
```

The extraction runner includes both frontend inventory commands and the frontend source-health guard before type checking. CI and release-candidate workflows run the Python fixture/production guard beside the backend architecture inventory, and run the Node fixture/production guard beside the frontend inventory. `scripts/test-e2e-contracts.ps1` requires both workflows to retain those commands.
