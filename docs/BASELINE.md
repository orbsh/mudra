# Baseline: Python/JS line counts (pre-Rust rewrite, 2026-09-26)

Reference for the rewrite comparison (ADR: `ADR-rust-fullstack.md`, PLAN §11).
Measured at commit `59578b1` on `main`, working tree clean.

## Python (mudrad + CLI + lib) — 2,964 lines

| file | lines |
|---|---|
| mudra.py (CLI) | 670 |
| mudrad.py (daemon main) | 491 |
| mudralib/db.py (all SQL) | 402 |
| mudralib/ui.py (panel server + WS) | 499 |
| mudralib/ops.py (shared actions) | 229 |
| mudralib/wm.py (WmExt + NiriExt) | 209 |
| mudralib/cdp.py | 140 |
| mudralib/config.py | 111 |
| mudralib/ctl.py | 114 |
| mudralib/spawn.py | 99 |

## JS (extension + panel) — 1,342 lines (+ 47 html/manifest)

| file | lines |
|---|---|
| frontend/extension/content.js | 555 |
| frontend/ui/src/app.js (panel, solidjs/h) | 373 |
| frontend/shared/lib.js (config/bar/cmd) | 266 |
| frontend/extension/sw.js | 96 |
| frontend/shared/tags.js | 52 |
| frontend/ui/index.html | 15 |
| frontend/manifest.json | 32 |

## Scope note for the comparison

The Rust rewrite replaces: all Python (2,964) + the panel app.js (373) +
shared tags.js (52). It keeps: extension JS files (content.js/sw.js/lib.js,
1,342 - 373 - 52 = ~917 minus shared lib overlap) — lib.js's bar remains JS
render; its capsule markup comes from the tag-forest crate's SSR surface.
Comparison counts lines of Rust (bin+lib, tests excluded and included
separately) against these numbers, same method: `wc -l`.
