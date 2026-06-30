# App icons

These are generated, not hand-authored. From `src-tauri/`:

```bash
cargo tauri icon ../web/public/paw-1024.png
```

That command writes `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`
(macOS), and `icon.ico` (Windows) into this directory — the exact set referenced
by `tauri.conf.json > bundle.icon`. Provide a square ≥1024×1024 source PNG (the
🐾 paw mark on the brand amber background).

Until generated, `cargo tauri build` will fail at the bundle step with a missing
-icon error — this is expected for a fresh checkout (TODO §D0).
