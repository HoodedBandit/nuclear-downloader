# Frozen Chrome 152.0.7977.76 recovery

The committed receipt is `internal-cleanup-browser-recovery.json`. Its paths are
relative to `target/browser-recovery/`, which retains the downloaded package,
full bundle manifest, extracted browser, and original recovery receipt.

The installed Chrome executable auto-updated during visual run
visual-20260909T083201Z-c97822819b2b4c0eb32a0978e04fb483. That receipt records
inputsVerifiedAfter false and the failure “A renderer executable changed during
execution.” The run is invalid and remains preserved as interruption evidence.

Chrome 152.0.7977.76 was recovered from Google's public static release URL:

https://edgedl.me.gvt1.com/dl/release2/chrome/acjhfpyjgspz3vyycvrpc3zsj4da_152.0.7977.76/152.0.7977.76_chrome_installer_uncompressed.exe

The downloaded package is 516,629,928 bytes with SHA-256
23558133eccb12e77b268fe3ff3a46735077b9042b79481ac8ba489729907671 and
valid Google LLC Authenticode. It was extracted without installing into
frozen-152.0.7977.76/Chrome-bin.

The recovered chrome.exe exactly matches the earlier baseline identity:
4,484,760 bytes and SHA-256
17b09f4c2e7806a05b0b648e7d459c3e3868f215adc93fa887adc3892bc704c0.
The complete 267-file bundle is recorded in the retained manifest and recovery
receipt. No bundle entry is a reparse point. The prior successful visual and
performance baselines remain unchanged.
