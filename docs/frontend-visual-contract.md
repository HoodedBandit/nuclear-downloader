# Frontend visual contract

This Stage 1 contract detects any renderer change in pixels, visible text, element geometry, focus, or control state. It does not create or approve baselines. Baselines remain review-controlled evidence, and a deliberate visual change fails until a reviewer replaces the baseline through the repository's normal review process.

The comparison is deterministic and fail-closed. It decodes PNGs through Windows `System.Drawing` and compares every 32-bit ARGB pixel. PNG container bytes may differ when decoded pixels are identical. The comparator also verifies each file's SHA-256 so an artifact cannot refer to a screenshot that changed after capture.

## Required matrix

Each artifact contains exactly 30 scenarios for one scale. A baseline or candidate group always consists of exactly two artifacts, one for 100 percent and one for 150 percent. Their union is the 60-scenario Cartesian product of these values.

- Viewports: `800x500`, `1000x700`, and `1440x1000` CSS pixels.
- Scale targets: `100` and `150` percent.
- States: `empty`, `populated`, `downloading`, `converting`, `cancelled`, `failed`, `interrupted`, `persistence-degraded`, `playlist-modal`, and `update-modal`.

The canonical scenario ID is `<state>@<width>x<height>@<targetPercent>`, for example `failed@1000x700@150`. Every scenario has exactly two independently captured repeats. Both pixels and semantic data must be identical between repeats. This applies to baseline and candidate artifacts; the baseline stability check is never inferred from one capture.

## Artifact schema

Artifacts use this strict shape. Extra and missing properties fail validation.

```json
{
  "schemaVersion": "renderer-visual-contract/v1",
  "source": {
    "commit": "40 lowercase or uppercase hexadecimal characters",
    "productionHash": "64 lowercase hexadecimal characters",
    "description": "human-readable source provenance"
  },
  "fixture": { "id": "frontend-stage1", "version": "1" },
  "environment": {
    "os": { "name": "Windows", "version": "...", "architecture": "x64" },
    "browser": { "name": "Chromium", "version": "...", "engine": "Blink" },
    "renderer": { "name": "WebView2", "version": "..." }
  },
  "scenarios": [
    {
      "id": "empty@800x500@100",
      "state": "empty",
      "viewport": { "width": 800, "height": 500 },
      "scaling": {
        "targetPercent": 100,
        "deviceScaleFactor": 1.0,
        "actualWindowsScalePercent": 100,
        "mode": "native"
      },
      "repeats": [
        {
          "sequence": 1,
          "screenshot": {
            "path": "screenshots/empty@800x500@100-1.png",
            "sha256": "64 lowercase hexadecimal characters",
            "width": 800,
            "height": 500
          },
          "semantic": {
            "activeElementKey": "download-button",
            "tabOrder": ["download-button", "cancel-button"],
            "elements": [
              {
                "key": "download-button",
                "tag": "button",
                "text": "Download",
                "rect": { "x": 20, "y": 70, "width": 120, "height": 36 },
                "visible": true,
                "enabled": true,
                "focused": true,
                "control": {
                  "kind": "button",
                  "value": null,
                  "checked": null,
                  "expanded": null,
                  "pressed": false,
                  "selected": null
                }
              }
            ]
          }
        },
        {
          "sequence": 2,
          "screenshot": {
            "path": "screenshots/empty@800x500@100-2.png",
            "sha256": "...",
            "width": 800,
            "height": 500
          },
          "semantic": {
            "activeElementKey": "download-button",
            "tabOrder": ["download-button", "cancel-button"],
            "elements": []
          }
        }
      ]
    }
  ]
}
```

The abbreviated second semantic object above only illustrates the repeat position; a real repeat must contain the same complete semantic element list as repeat one. `activeElementKey` is an empty string when nothing is focused. Otherwise it must identify the sole element whose `focused` field is true. `tabOrder` records the observed forward-Tab traversal using stable element keys; every key must identify an enabled captured element. A key may have `visible: false`: narrow responsive layout can produce a zero-width element that Chrome still visits, and visibility remains captured comparison evidence rather than a proxy for focusability. Non-modal sequences contain unique keys. Modal sequences repeat the first key once at the end to prove focus-trap wrap, with no earlier duplicate. Capture takes the screenshot before traversal, bounds traversal to the focusable count plus one (at most 2,001 steps), and restores initial focus and scroll positions before recording the final semantic state. Element keys must be stable DOM-derived identifiers rooted in the captured `main`. `kind` and `value` accept strings or null; `checked`, `expanded`, `pressed`, and `selected` accept booleans or null. Generic elements use null control fields.

The fixture ID, fixture version, OS, browser, renderer, viewport, and scaling values must match exactly between baseline and candidate. The harness identity comes from the archived harness manifest bound by the receipt. The two artifacts within either group must also have the same source, fixture, harness, pinned Windows build/scaling, and executable version/hash/size identities. Run-owned profile paths are validated within each evidence root and may differ across runs. `source` may differ between the baseline group and candidate group so a candidate commit and production-source hash can be compared with the reviewed baseline. `productionHash` covers the production frontend inputs executed by the captured run, never generated screenshots or harness files.

## Run provenance

Every scale artifact has three fixed sibling files: `run-receipt.json`, `production-manifest.json`, and `harness-manifest.json`. A manifest uses `renderer-input-manifest/v1`, identifies its `kind`, and contains an ordinal path-sorted `files` array. Each entry has `path`, `archivePath`, `size`, and `sha256`. The archive path is exactly `inputs/<kind>/<path>`, such as `inputs/production/nuclear-app/src/routes/+page.svelte`. The comparator confines these paths to the evidence root, rejects traversal, alternate streams, and reparse points, and verifies the archived byte count and SHA-256. `aggregateHash` is SHA-256 of UTF-8 compact JSON for the complete sorted `files` array.

The `renderer-check-receipt/v1` receipt must describe suite `visual` and selected spec `e2e/browser/visual-baseline.e2e.mjs`. It binds the full source commit, production aggregate, harness aggregate, exact manifest-file byte hashes, successful exit, post-run input verification, scale factor, scaling mode, Windows environment/profile, and Node/browser/driver path, size, hash, and version. The artifact source and scenario scale data must agree with it. This validates the immutable archived inputs from that run and does not compare an old baseline with the current checkout.

Screenshot paths are relative to the artifact JSON and must remain under its `screenshots/` directory. Absolute paths, traversal, alternate data stream syntax, reparse points, other extensions, missing files, stale hashes, duplicate JSON properties, duplicate repeat sequences, and images inconsistent with `viewport × deviceScaleFactor` fail. The comparator checks the PNG signature and IHDR dimensions before invoking the image decoder. Input limits are 16 MiB per JSON artifact, 32 MiB per PNG, 4,096 pixels per dimension, 20 million decoded pixels, 240 path characters, and 2,000 semantic elements per repeat.

## Display scaling evidence

`deviceScaleFactor` records the renderer scale. `actualWindowsScalePercent` records native Windows display scaling. `mode` says whether the target came from the Windows display (`native`) or browser emulation (`emulated`). A 150 percent capture made with a browser force-scale option while Windows remains at 100 percent is represented as:

```json
{
  "targetPercent": 150,
  "deviceScaleFactor": 1.5,
  "actualWindowsScalePercent": 100,
  "mode": "emulated"
}
```

Emulated captures can pass deterministic renderer comparison, but the report sets `nativeScalingQualified` to `false`. They do not qualify native 150 percent Windows behavior. Use `-RequireNativeScaling` only for the separate native-display acceptance gate; it converts that incomplete qualification into a comparison failure.

## Running the comparator

```powershell
pwsh -NoProfile -File scripts/compare-renderer-baseline.ps1 `
  -BaselineArtifact evidence/baseline-100/visual-100.json,evidence/baseline-150/visual-150.json `
  -CandidateArtifact evidence/candidate-100/visual-100.json,evidence/candidate-150/visual-150.json `
  -ReportPath evidence/comparison.json
```

The default comparison rejects a baseline and candidate group that resolve to any of the same artifact paths. To validate reviewed baseline evidence and its two stable repeats without making a candidate comparison claim, use `-ValidateBaselineOnly` with the two baseline artifacts and omit `-CandidateArtifact`.

The command exits `0` only when all contract checks and comparisons pass, `1` for invalid evidence or a difference, and `2` when the decoded-PNG helper cannot load. Its JSON report records `purpose`, `comparedCandidate`, `passed`, `nativeScalingQualified`, each input artifact/receipt hash and source/harness identity, the comparator PowerShell and C# hashes, and an `errors` array. The command never writes or updates a baseline.

Run the isolated synthetic fixtures with:

```powershell
pwsh -NoProfile -File scripts/test-renderer-baseline.ps1
```

The fixtures exercise matching decoded pixels with different PNG bytes, one changed pixel, geometry, enabled control state, visibility, a zero-width enabled Tab stop, copy, keyboard order, modal focus-cycle wrap, a missing scenario, missing properties, environment mismatch, stale hashes, path traversal, dimension mismatch, duplicate JSON properties, oversized JSON, unstable baseline repeats, and the native-scaling switch. They generate solid synthetic images in a temporary directory and do not launch or qualify the application, WebView2, Chrome, or native Windows display scaling.
