# Desktop app — local build guide

The desktop shell (`src-tauri/`) wraps the existing `duduclaw` gateway + the
embedded dashboard in a native window (Tauri 2). It runs the gateway as a
**sidecar** child process — the core binary is unchanged (TODO §D).

> The shell is intentionally **excluded** from the Rust workspace (root
> `Cargo.toml`). Build it from `src-tauri/` with the Tauri CLI, not `cargo build`.

## Prerequisites

```bash
# Tauri CLI — needs rustc >= 1.77. If `cargo install` errors with
# "requires rustc 1.77.2 or newer", your rustup default toolchain is too old:
#   rustup default stable && rustup update stable
cargo install tauri-cli --version "^2"
# Node (for the web build) — already required by the dashboard
# macOS: Xcode CLT;  Linux: libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

Generate the app icons once. The brand source `web/public/paw-1024.png` is
committed; the generated icon set under `src-tauri/icons/` is gitignored
(regenerate, don't commit):

```bash
scripts/desktop/gen-icons.sh           # cargo tauri icon, with a macOS sips fallback
# or directly:  cd src-tauri && cargo tauri icon ../web/public/paw-1024.png
```

## Dev (hot-reload UI)

```bash
# Stage the gateway sidecar FIRST — `tauri dev` resolves it next to the dev
# binary (src-tauri/target/debug/), not from binaries/. Without this the app
# can't spawn the gateway and the UI can't reach /api (ECONNREFUSED).
cargo build --release -p duduclaw-cli --bin duduclaw   # from the REPO ROOT
scripts/desktop/stage-sidecar.sh                        # copies into binaries/ + target/{debug,release}/

cd src-tauri && cargo tauri dev
```

In **dev** the window stays on the Vite dev server (`127.0.0.1:5173`) for live
HMR; Vite proxies `/ws` + `/api` to the gateway. The app still spawns the
gateway sidecar on launch and shows the window only once it's ready. In
**release** the window points at the gateway's embedded dashboard instead
(`#[cfg]` split in `main.rs`). So a web edit shows instantly in dev, but to see
it through the *embedded* path you must rebuild the dist + re-embed (the gateway
serves `crates/duduclaw-dashboard/dist` via `rust_embed`, baked at compile time).

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

## First-build gotchas (verified 2026-07 on macOS arm64)

Ordered roughly by when you hit them. All are resolved in the repo; this is the
"why" so a clean machine doesn't re-debug.

1. **`cargo install tauri-cli` fails: "requires rustc 1.77.2 or newer".**
   Your rustup *default* toolchain is stale even if a newer one is installed.
   `rustup default stable && rustup update stable`. (`cargo`/`rustc` on PATH may
   be a separate Homebrew copy — the failing one is the rustup shim.)

2. **Run cargo gateway commands from the REPO ROOT, not `src-tauri/`.**
   `src-tauri` is its own excluded workspace, so `cargo build -p duduclaw-cli`
   there errors with "package ID … did not match any packages". Only
   `cargo tauri dev/build` runs inside `src-tauri/`.

3. **`cargo metadata`/`tauri dev` can't parse the manifest: missing `lib.rs`.**
   The mobile-template `[lib]` was removed — `src-tauri` is a binary crate
   (`src/main.rs`). Don't re-add `[lib]` without a matching `src/lib.rs`.

4. **Both frontend hooks run from the repo root**, not from `src-tauri/`
   (verified: the build hook's `pwd` is the repo root). So both are
   `cd web && npm run …` — NOT `cd ../web`. (`cargo tauri dev/build` is invoked
   from `src-tauri/`, but Tauri runs the hooks from the project root.)

5. **Vite "Waiting for frontend dev server …" forever.** Vite must bind IPv4
   `127.0.0.1` (not the default `localhost`/`::1`) to match the Tauri poller and
   the proxy target — pinned in `web/vite.config.ts` (`host: '127.0.0.1'`,
   `strictPort`, and the gateway proxy default `http://127.0.0.1:18789`).

6. **Compile error in `cookie 0.18.1` (`Parsable::parse` arity).** `time 0.3.52`
   broke the API within 0.3.x; held at `time = "=0.3.51"` in `src-tauri/Cargo.toml`.
   Remove once tauri/wry ship a cookie targeting the new `time`.

7. **Build script: "Permission core:webview:allow-navigate not found".** That
   permission doesn't exist in Tauri 2 (`navigate()` is a Rust API, not gated).
   Keep it out of `capabilities/default.json`.

8. **Login shows ECONNREFUSED / gateway never starts in dev.** The sidecar is
   resolved *next to the running exe*; `tauri dev` runs from
   `src-tauri/target/debug/`, so the binary must be staged there — `stage-sidecar.sh`
   now copies into `target/{debug,release}/` as well as `binaries/`. After a
   `cargo clean`, re-run `stage-sidecar.sh`.

9. **Icon shows a white fringe.** The source must have clean transparent corners
   (don't rasterize an SVG via `qlmanage`, which mattes onto white). Regenerated
   `paw-1024.png` is a full-bleed amber square with a supersampled rounded alpha
   mask. `build.rs` emits `rerun-if-changed=icons`, so a regenerated icon set is
   re-embedded on the next build (otherwise the old icon stays baked in — and
   macOS may still cache it: `sudo rm -rf /Library/Caches/com.apple.iconservices.store && killall Dock`).

10. **`cargo tauri build` ends with "A public key has been found, but no private
    key".** The `.app`/`.dmg` are already built — only the updater-artifact
    signing step fails. Auto-update is **off** until keys exist
    (`plugins.updater.active = false`, `bundle.createUpdaterArtifacts = false`);
    [desktop-unblock.md](./desktop-unblock.md) 關卡 E flips both back on after
    `cargo tauri signer generate`.

11. **The DMG shows a `.VolumeIcon.icns` file.** That's the disk image's volume
    icon (DMG chrome), a dotfile — **normal users with default Finder don't see
    it**; it appears only if you've enabled "show hidden files"
    (`defaults write com.apple.finder AppleShowAllFiles`). It is *not* bundled
    into `DuDuClaw.app`. A polished installer (custom background + window layout)
    would be a separate `bundle.macOS.dmg` enhancement.

## Verified working (2026-07, macOS arm64)

`cargo tauri build` produces a working unsigned `DuDuClaw.app` + `.dmg` locally.
Signing / notarization / auto-update need real Apple + Windows certificates and
updater keys — see [desktop-release.md](./desktop-release.md) and
[desktop-unblock.md](./desktop-unblock.md).
