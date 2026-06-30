# App icons

These are **generated, not hand-authored**, and are git-ignored (see
`src-tauri/.gitignore`). Regenerate them from the committed source paw mark:

```bash
# From the repo root — uses `cargo tauri icon`, falls back to sips on macOS:
scripts/desktop/gen-icons.sh            # source: web/public/paw-1024.png
scripts/desktop/gen-icons.sh path.png   # or pass your own >=1024x1024 PNG

# Or directly (from src-tauri/):
cargo tauri icon ../web/public/paw-1024.png
```

That writes `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns` (macOS),
and `icon.ico` (Windows) — the exact set referenced by
`tauri.conf.json > bundle.icon` — plus the Windows Store logos.

The source mark (`web/public/paw-1024.png`, brand-amber squircle + 🐾) is
committed and rasterized from `web/public/paw-source.svg`. CI regenerates the
icons in `.github/workflows/desktop-release.yml` before bundling, so the
ignored, generated files never need to be committed. A fresh checkout must run
the command above (or `cargo tauri build` will fail at the bundle step on a
missing-icon error — expected, §D0).
