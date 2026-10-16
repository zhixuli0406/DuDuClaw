# App icons

These are **generated, not hand-authored**, and are git-ignored (see
`src-tauri/.gitignore`). Regenerate them from the committed source master
`src-tauri/icons/source/duduclaw-1024.png`:

```bash
# From the repo root — uses `cargo tauri icon`, falls back to sips on macOS:
scripts/desktop/gen-icons.sh            # source: src-tauri/icons/source/duduclaw-1024.png
scripts/desktop/gen-icons.sh path.png   # or pass your own >=1024x1024 PNG

# Or directly (from src-tauri/):
cargo tauri icon icons/source/duduclaw-1024.png
```

That writes `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns` (macOS),
and `icon.ico` (Windows) — the exact set referenced by
`tauri.conf.json > bundle.icon` — plus the Windows Store logos. tauri-cli 2.x
also emits `android/` and `ios/` sets; the app is desktop-only, so those are
ignored too.

The master is the C1 brand cat mark on a cream rounded square. It is built
from `docs/assets/brand/duduclaw-avatar-1024.png` (the brand avatar, opaque
background) by `scripts/desktop/make-icon-master.py`, which cuts the square to
a rounded rectangle with a supersampled alpha mask. Re-run that script only
when the brand avatar changes, then regenerate the icons.

CI regenerates the icons in `.github/workflows/desktop-release.yml` before
bundling, so the ignored, generated files never need to be committed. A fresh
checkout must run the command above (or `cargo tauri build` will fail at the
bundle step on a missing-icon error — expected, §D0).
