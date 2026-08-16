# Desktop app — local build guide

Desktop 殼層（`src-tauri/`）把既有的 `duduclaw` gateway 與內嵌的 dashboard 包進原生視窗（Tauri 2）。它以 **sidecar** 子行程的方式執行 gateway，核心二進位檔本身不受影響（TODO §D）。

> 這個殼層刻意**排除**在 Rust workspace（root `Cargo.toml`）之外。請用 Tauri CLI 在 `src-tauri/` 底下建置，不要用 `cargo build`。

## Prerequisites

```bash
# Tauri CLI — needs rustc >= 1.77. If `cargo install` errors with
# "requires rustc 1.77.2 or newer", your rustup default toolchain is too old:
#   rustup default stable && rustup update stable
cargo install tauri-cli --version "^2"
# Node (for the web build) — already required by the dashboard
# macOS: Xcode CLT;  Linux: libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

先產生一次 app 圖示。品牌來源圖 `web/public/paw-1024.png` 已提交進版控；`src-tauri/icons/` 底下產生的圖示集則被 gitignore（要重新產生，不要提交）：

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

在 **dev** 模式下，視窗會停留在 Vite dev server（`127.0.0.1:5173`）以取得即時 HMR；Vite 會把 `/ws` 與 `/api` 代理到 gateway。App 啟動時仍會照樣 spawn gateway sidecar，並等它就緒才顯示視窗。在 **release** 模式下，視窗改指向 gateway 內嵌的 dashboard（在 `main.rs` 用 `#[cfg]` 分支切換）。所以修改 web 端程式碼在 dev 模式會立刻看得到，但若要在*內嵌*那條路徑上看到效果，就必須重新建置 dist 並重新內嵌（gateway 透過 `rust_embed` 提供 `crates/duduclaw-dashboard/dist`，是在編譯期就烤進去的）。

## Production build (unsigned, local)

```bash
# 1. build the release gateway and stage it as the sidecar
cargo build --release -p duduclaw-cli --bin duduclaw
scripts/desktop/stage-sidecar.sh

# 2. build the app bundle
cd src-tauri && cargo tauri build
```

產物會落在 `src-tauri/target/release/bundle/`（`.app`/`.dmg`、`.msi`/`.exe`、`.AppImage`/`.deb`）。

## Lifecycle behavior (what the shell does)

- **單一實例**：第二次啟動只會把已存在的視窗帶到前景（§D2.1）。
- **附掛或新啟動**：若 `DUDUCLAW_PORT`（預設 **18789**）上已經有 gateway 在跑，就直接附掛上去，*不會*把它殺掉；否則就在 `18789..=18797` 裡挑第一個空的 port 來 spawn sidecar（§D1 / §D2.2）。
- **PATH**：sidecar 是以擴充過的 PATH 啟動（Homebrew、`.local/bin`、Bun、Volta、npm-global、asdf、cargo），確保從 Finder / Dock 啟動時也找得到 Claude CLI / node / containers（§D2.6）。
- **資料目錄**：與 CLI 共用 `~/.duduclaw`，兩邊看到的 agent／SQLite／wiki 都是同一份（§D2.7）。
- **關閉即縮到系統列**：關閉視窗只是隱藏它，要真正結束要從系統列選單選擇離開（§D2.4）。
- **健康檢查與重啟**：sidecar 意外結束時會觸發指數退避重啟（最多 5 次），仍失敗才顯示錯誤（§D2.5）。

## Relationship to launchd

如果你已經用 launchd 執行 gateway，desktop app 會**附掛**上去（不會重複啟動）。若想改讓 app 自己接管 gateway，要先停掉 launchd job。單一實例鎖加上 pidfile（`~/.duduclaw/desktop-sidecar.pid`）可以防止兩個由 app 啟動的 sidecar 同時存在。

## First-build gotchas (verified 2026-07 on macOS arm64)

大致依你遇到的順序排列。這些都已經在 repo 裡解決，這裡記的是「為什麼」，讓乾淨機器不用重新除錯一遍。

1. **`cargo install tauri-cli` 失敗，錯誤是「requires rustc 1.77.2 or newer」。**
   就算已經裝了更新的版本，rustup 的*預設*工具鏈可能還是舊的。跑 `rustup default stable && rustup update stable`。（`cargo`/`rustc` 在 PATH 上可能是另一份 Homebrew 的拷貝，真正出問題的其實是 rustup 的 shim。）

2. **gateway 相關的 cargo 指令要從 REPO ROOT 執行，不要在 `src-tauri/` 底下。**
   `src-tauri` 是被排除在外的獨立 workspace，所以在那裡跑 `cargo build -p duduclaw-cli` 會出現「package ID … did not match any packages」的錯誤。只有 `cargo tauri dev/build` 才是在 `src-tauri/` 底下執行。

3. **`cargo metadata`/`tauri dev` 無法解析 manifest：找不到 `lib.rs`。**
   Mobile template 帶的 `[lib]` 已被移除，`src-tauri` 是純二進位 crate（`src/main.rs`）。不要在沒有對應 `src/lib.rs` 的情況下重新加回 `[lib]`。

4. **兩個 frontend hook 都是從 repo root 執行**，不是從 `src-tauri/`
   （已驗證：build hook 的 `pwd` 就是 repo root）。所以兩者都是 `cd web && npm run …`，**不是** `cd ../web`。（`cargo tauri dev/build` 是在 `src-tauri/` 底下被呼叫，但 Tauri 執行 hook 時的工作目錄是 project root。）

5. **Vite 一直卡在「Waiting for frontend dev server …」。** Vite 必須綁定 IPv4 的
   `127.0.0.1`（不是預設的 `localhost`/`::1`），才會跟 Tauri 的輪詢器與 proxy target 對上，這點已在 `web/vite.config.ts` 釘死（`host: '127.0.0.1'`、`strictPort`，以及 gateway proxy 預設值 `http://127.0.0.1:18789`）。

6. **`cookie 0.18.1` 出現編譯錯誤（`Parsable::parse` arity 不符）。** `time 0.3.52`
   在 0.3.x 系列內破壞了 API，已在 `src-tauri/Cargo.toml` 中鎖在 `time = "=0.3.51"`。等 tauri/wry 支援新版 `time` 之後就可以拿掉這個鎖定。

7. **Build script 出現「Permission core:webview:allow-navigate not found」。** Tauri 2
   裡根本不存在這個權限（`navigate()` 是 Rust API，不需要權限閘）。別把它放進 `capabilities/default.json`。

8. **登入畫面出現 ECONNREFUSED，或 dev 模式下 gateway 完全沒啟動。** sidecar 是*相對於正在執行的可執行檔*來解析路徑的；`tauri dev` 是從
   `src-tauri/target/debug/` 執行，所以二進位檔必須先放到那裡；`stage-sidecar.sh`
   現在也會複製一份進 `target/{debug,release}/`，不只是 `binaries/`。跑過 `cargo clean` 之後要記得重新執行一次 `stage-sidecar.sh`。

9. **圖示出現白邊。** 來源圖必須有乾淨的透明邊角（不要用 `qlmanage` 把 SVG 轉點陣，它會把透明部分貼上白底）。重新產生的
   `paw-1024.png` 是滿版的琥珀色正方形，帶超取樣過的圓角 alpha 遮罩。`build.rs` 會發出
   `rerun-if-changed=icons`，所以重新產生的圖示集會在下一次建置時重新內嵌（否則舊圖示會一直被烤在裡面，且 macOS 也可能還快取著舊的：`sudo rm -rf /Library/Caches/com.apple.iconservices.store && killall Dock`）。

10. **`cargo tauri build` 最後出現「A public key has been found, but no private
    key」。** `.app`/`.dmg` 其實已經建置完成，只是 updater 產物的簽章步驟失敗了。在金鑰產生之前，自動更新是**關閉**的
    （`plugins.updater.active = false`、`bundle.createUpdaterArtifacts = false`）；
    [desktop-unblock.md](../desktop-unblock.md) 關卡 E 會在跑過
    `cargo tauri signer generate` 之後把兩者都打開。

11. **DMG 裡出現一個 `.VolumeIcon.icns` 檔案。** 那是磁碟映像的磁碟區圖示（DMG 的外殼裝飾），是個隱藏檔，**一般使用者用預設的 Finder 設定看不到它**，只有開了「顯示隱藏檔案」
    （`defaults write com.apple.finder AppleShowAllFiles`）才會看到。它*不會*被打包進
    `DuDuClaw.app` 裡。

DMG 視窗本身的樣式是透過 `bundle.macOS.dmg` 設定的（自訂背景圖、視窗大小、圖示位置）。背景圖是 `src-tauri/dmg/background.png`，由
`scripts/desktop/gen-dmg-background.py` 產生（用 Pillow；因為不是每台 macOS 都有 PingFang，所以 zh-TW 文字用的是 Heiti TC）。要改就改這個腳本，不要直接改 PNG。

## Verified working (2026-07, macOS arm64)

`cargo tauri build` 在本機可以產生一個可用、未簽章的 `DuDuClaw.app` 與 `.dmg`。
簽章／公證／自動更新需要真正的 Apple 與 Windows 憑證，以及 updater 金鑰，參見
[desktop-release.md](../desktop-release.md) 與
[desktop-unblock.md](../desktop-unblock.md)。
