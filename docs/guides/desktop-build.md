# Desktop app — local build guide

The desktop shell (`src-tauri/`) wraps the existing `duduclaw` gateway + the
embedded dashboard in a native window (Tauri 2). It runs the gateway as a
**sidecar** child process — the core binary is unchanged (TODO §D).

> The shell is intentionally **excluded** from the Rust workspace (root
> `Cargo.toml`). Build it from `src-tauri/` with the Tauri CLI, not `cargo build`.

## Prerequisites

```bash
# Tauri CLI
cargo install tauri-cli --version "^2"
# Node (for the web build) — already required by the dashboard
# macOS: Xcode CLT;  Linux: libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

Generate the app icons once (needs a ≥1024² source PNG):

```bash
cd src-tauri && cargo tauri icon ../web/public/paw-1024.png
```

## Dev (hot-reload UI)

```bash
cd src-tauri
cargo tauri dev
```

`beforeDevCommand` starts Vite; the window points at `127.0.0.1:5173`. The
gateway sidecar is spawned by the app on launch (see below).

## Production build (unsigned, local)

```bash
# 1. build the release gateway and stage it as the sidecar
cargo build --release -p duduclaw-cli --bin duduclaw
scripts/desktop/stage-sidecar.sh

# 2. build the app bundle
cd src-tauri && cargo tauri build
```

Artifacts land in `src-tauri/target/release/bundle/` (`.app`/`.dmg`,
`.msi`/`.exe`, `.AppImage`/`.deb`).

## Lifecycle behavior (what the shell does)

- **Single instance** — a second launch focuses the existing window (§D2.1).
- **Attach vs spawn** — if a gateway is already serving on `DUDUCLAW_PORT`
  (default **18789**) it attaches and will *not* kill it; otherwise it spawns
  the sidecar on the first free port in `18789..=18797` (§D1 / §D2.2).
- **PATH** — the sidecar is spawned with an augmented PATH (Homebrew, `.local/bin`,
  Bun, Volta, npm-global, asdf, cargo) so Finder/Dock launches still find Claude
  CLI / node / containers (§D2.6).
- **Data dir** — shares `~/.duduclaw` with the CLI; both see the same agents /
  SQLite / wiki (§D2.7).
- **Close to tray** — closing the window hides it; quit via the tray menu (§D2.4).
- **Health + restart** — an unexpected sidecar exit triggers an exponential
  backoff restart (≤5 attempts) before surfacing an error (§D2.5).

## Relationship to launchd

If you already run the gateway via launchd, the desktop app **attaches** to it
(no double-run). To let the app own the gateway instead, stop the launchd job
first. The single-instance lock + pidfile (`~/.duduclaw/desktop-sidecar.pid`)
prevent two app-spawned sidecars.

## Known limitation (this checkout)

`cargo tauri dev/build` requires the Tauri toolchain + a display and was **not**
run in the authoring environment. Signing/notarization requires real
certificates — see [desktop-release.md](./desktop-release.md).
