# Braille X atlas source

The two HTML files are unchanged copies of the approved 2026-10-04 17:56 prototype. The checked-in binary atlas is the runtime source of truth; xmux does not launch a browser.

To regenerate it, install the pinned Playwright dependency in this directory, install its Chromium browser, then run `node scripts/generate_braille_x.cjs` from the repository root. The browser's fonts affect rasterization. The approved atlas was generated on Windows with Arial, Georgia, and Segoe UI Symbol; a different font environment can produce different bytes. The Rust tests verify every approved front-frame fingerprint and reject isolated unlit dots.
