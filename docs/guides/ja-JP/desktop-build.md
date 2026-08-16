# Desktop app — local build guide

デスクトップシェル（`src-tauri/`）は、既存の`duduclaw` gatewayと組み込み済みdashboardをネイティブウィンドウ（Tauri 2）にラップします。gatewayは**sidecar**子プロセスとして実行され、コア バイナリ自体は変更されません（TODO §D）。

> このシェルは意図的にRust workspace（root `Cargo.toml`）から**除外**されています。`src-tauri/`からTauri CLIでビルドしてください。`cargo build`は使いません。

## Prerequisites

```bash
# Tauri CLI — needs rustc >= 1.77. If `cargo install` errors with
# "requires rustc 1.77.2 or newer", your rustup default toolchain is too old:
#   rustup default stable && rustup update stable
cargo install tauri-cli --version "^2"
# Node (for the web build) — already required by the dashboard
# macOS: Xcode CLT;  Linux: libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

アプリアイコンを一度生成します。ブランド素材のソース`web/public/paw-1024.png`はコミット済みです。`src-tauri/icons/`配下に生成されるアイコンセットはgitignore対象なので、再生成してください（コミットしないこと）。

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

**dev**モードではウィンドウはVite dev server（`127.0.0.1:5173`）に留まり、リアルタイムHMRを利用できます。Viteが`/ws`と`/api`をgatewayにプロキシします。アプリは起動時に変わらずgateway sidecarをspawnし、準備が整うまでウィンドウを表示しません。**release**モードでは、ウィンドウはgatewayに組み込まれたdashboardを指すようになります（`main.rs`内の`#[cfg]`で分岐）。そのため、web側のコード編集はdevモードでは即座に反映されますが、*組み込み*経路で確認するにはdistを再ビルドして再度組み込む必要があります（gatewayは`rust_embed`経由で`crates/duduclaw-dashboard/dist`を配信しており、これはコンパイル時に焼き込まれます）。

## Production build (unsigned, local)

```bash
# 1. build the release gateway and stage it as the sidecar
cargo build --release -p duduclaw-cli --bin duduclaw
scripts/desktop/stage-sidecar.sh

# 2. build the app bundle
cd src-tauri && cargo tauri build
```

成果物は`src-tauri/target/release/bundle/`に生成されます（`.app`/`.dmg`、`.msi`/`.exe`、`.AppImage`/`.deb`）。

## Lifecycle behavior (what the shell does)

- **単一インスタンス**：2回目の起動では既存のウィンドウがフォーカスされます（§D2.1）。
- **アタッチ or 新規起動**：`DUDUCLAW_PORT`（デフォルト**18789**）で既にgatewayが動いている場合はそこにアタッチし、*終了させません*。動いていなければ`18789..=18797`の中から最初に空いているポートでsidecarをspawnします（§D1 / §D2.2）。
- **PATH**：sidecarは拡張されたPATH（Homebrew、`.local/bin`、Bun、Volta、npm-global、asdf、cargo）で起動されるため、FinderやDockからの起動でもClaude CLI / node / containersを見つけられます（§D2.6）。
- **データディレクトリ**：CLIと`~/.duduclaw`を共有し、agent／SQLite／wikiは両者で同じものを見ています（§D2.7）。
- **閉じるとトレイに常駐**：ウィンドウを閉じても非表示になるだけです。終了するにはトレイメニューから選んでください（§D2.4）。
- **ヘルスチェックと再起動**：sidecarが予期せず終了すると指数バックオフでの再起動（最大5回）が走り、それでも失敗した場合にエラーを表示します（§D2.5）。

## Relationship to launchd

すでにlaunchd経由でgatewayを実行している場合、デスクトップアプリはそこに**アタッチ**します（二重起動にはなりません）。アプリ側にgatewayを持たせたい場合は、先にlaunchd jobを止めてください。単一インスタンスロックとpidfile（`~/.duduclaw/desktop-sidecar.pid`）により、アプリが起動した2つのsidecarが同時に存在することはありません。

## First-build gotchas (verified 2026-07 on macOS arm64)

おおよそ遭遇する順に並べています。いずれもリポジトリ内では解決済みで、ここに書いているのは「なぜそうなるか」です。クリーンな環境で同じ調査を繰り返さなくて済むように残しています。

1. **`cargo install tauri-cli`が「requires rustc 1.77.2 or newer」で失敗する。**
   新しいバージョンをインストール済みでも、rustupの*デフォルト*ツールチェーンが古いままのことがあります。`rustup default stable && rustup update stable`を実行してください。（PATH上の`cargo`/`rustc`は別のHomebrew版であることもあります。実際に失敗しているのはrustup側のshimです。）

2. **gateway関連のcargoコマンドはREPO ROOTから実行してください。`src-tauri/`からではありません。**
   `src-tauri`は除外された独立workspaceなので、そこで`cargo build -p duduclaw-cli`を実行すると「package ID … did not match any packages」エラーになります。`src-tauri/`内で実行するのは`cargo tauri dev/build`だけです。

3. **`cargo metadata`/`tauri dev`がmanifestを解析できない：`lib.rs`が見つからない。**
   モバイルテンプレート由来の`[lib]`は削除済みです。`src-tauri`はバイナリクレート（`src/main.rs`）です。対応する`src/lib.rs`なしに`[lib]`を再追加しないでください。

4. **2つのfrontend hookはどちらもrepo rootから実行されます**。`src-tauri/`からではありません
   （検証済み：build hookの`pwd`はrepo rootです）。そのため両方とも`cd web && npm run …`であり、**`cd ../web`ではありません**。（`cargo tauri dev/build`自体は`src-tauri/`から呼ばれますが、Tauriがhookを実行する際の作業ディレクトリはproject rootです。）

5. **Viteが「Waiting for frontend dev server …」のまま進まない。** ViteはIPv4の
   `127.0.0.1`にバインドする必要があります（デフォルトの`localhost`/`::1`ではありません）。これがTauriのポーリングとproxy targetに一致します。`web/vite.config.ts`で固定済みです（`host: '127.0.0.1'`、`strictPort`、gateway proxyのデフォルト値`http://127.0.0.1:18789`）。

6. **`cookie 0.18.1`でコンパイルエラー（`Parsable::parse`のarity不一致）。** `time 0.3.52`
   が0.3.x系列内でAPIを壊したため、`src-tauri/Cargo.toml`で`time = "=0.3.51"`に固定しています。tauri/wryが新しい`time`に対応したら外してください。

7. **Build scriptで「Permission core:webview:allow-navigate not found」が出る。** Tauri 2
   にはそもそもこの権限は存在しません（`navigate()`はRust APIであり、権限ゲートの対象外です）。`capabilities/default.json`には入れないでください。

8. **ログイン画面でECONNREFUSEDになる、またはdevモードでgatewayがまったく起動しない。** sidecarは*実行中のバイナリの隣*で解決されます。`tauri dev`は
   `src-tauri/target/debug/`から実行されるため、バイナリを先にそこへ配置しておく必要があります。`stage-sidecar.sh`
   は現在`binaries/`だけでなく`target/{debug,release}/`にもコピーします。`cargo clean`後は必ず`stage-sidecar.sh`を再実行してください。

9. **アイコンに白い縁が出る。** ソース画像は透明な角がクリーンである必要があります（`qlmanage`でSVGをラスタライズすると透明部分が白でマット合成されるため使わないこと）。再生成した
   `paw-1024.png`はフルブリードの琥珀色の正方形で、スーパーサンプリング済みの角丸alphaマスクを持ちます。`build.rs`は
   `rerun-if-changed=icons`を発行するため、再生成したアイコンセットは次のビルドで再度組み込まれます（そうしなければ古いアイコンが焼き込まれたままになります。macOS側でも古いものをキャッシュしている場合があります：`sudo rm -rf /Library/Caches/com.apple.iconservices.store && killall Dock`）。

10. **`cargo tauri build`の最後に「A public key has been found, but no private
    key」と出る。** `.app`/`.dmg`自体はすでにビルド済みで、失敗しているのはupdater成果物の署名ステップだけです。鍵が作成されるまで自動更新は**オフ**になっています
    （`plugins.updater.active = false`、`bundle.createUpdaterArtifacts = false`）。
    [desktop-unblock.md](../desktop-unblock.md)の関門Eで、
    `cargo tauri signer generate`実行後にこの2つを有効に戻します。

11. **DMGの中に`.VolumeIcon.icns`ファイルが見える。** これはディスクイメージのボリュームアイコン（DMGの外装）であり、ドットファイルです。**デフォルトのFinder設定を使う通常のユーザーには見えません**。見えるのは「隠しファイルを表示」
    （`defaults write com.apple.finder AppleShowAllFiles`）を有効にした場合だけです。
    `DuDuClaw.app`の中にバンドルされているわけでは*ありません*。

DMGウィンドウ自体の見た目は`bundle.macOS.dmg`で設定します（カスタム背景、ウィンドウサイズ、アイコン位置）。背景画像は`src-tauri/dmg/background.png`で、
`scripts/desktop/gen-dmg-background.py`（Pillow使用）によって生成されます。すべてのmacOSにPingFangが入っているわけではないため、zh-TWのテキストにはHeiti TCを使っています。編集する場合はPNGではなくこのスクリプトを直してください。

## Verified working (2026-07, macOS arm64)

`cargo tauri build`はローカル環境で、動作する未署名の`DuDuClaw.app`と`.dmg`を生成します。
署名／公証／自動更新には実際のAppleおよびWindowsの証明書、そしてupdater鍵が必要です。詳しくは
[desktop-release.md](../desktop-release.md)と
[desktop-unblock.md](../desktop-unblock.md)を参照してください。
