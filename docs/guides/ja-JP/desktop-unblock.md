# デスクトップアプリのブロック解除ガイド（Phase D + Phase 6手動テスト）

> [TODO-genspark-workspace-shell.md](../../todo/TODO-genspark-workspace-shell.md)内で`[ ]`（ブロック中）
> および`[~]`（実装済みだがbuild未検証）と表示されている項目に対応する。これらの項目が止まっている原因は
> ツールチェーン・認証情報・GUI環境・2台目のマシンといった外部リソースであり、コードが書けていないことではない。
> 本ドキュメントは各ブロック要因を「なぜ止まっているか → 前提条件 → 手順 → 対応するTODO項目の検収基準」の形式で分解する。
>
> **推奨する順序**：ゲートA（ローカル、無料、半日）→ ゲートE（更新用署名鍵、無料）→ ゲートB（macOS署名、有料アカウントが必要）
> → ゲートC（Windows署名）→ ゲートD（Linux）。Aまで終われば自分で使い始められ、ライフサイクル検収も一通り実行できる。

---

## ゲートA — Tauriツールチェーンを導入してローカルで動かす（署名なし）

**ブロックしている項目**：D0🧪、D1🧪、D2.1/D2.3/D2.4/D2.5/D2.6🧪、D5の1項目目、P6.3の手動テスト＋スクリーンショット。
**止まっている理由**：この執筆環境にはTauri CLIも、システムWebViewの開発依存関係も、ディスプレイもない。あなたのMacにはこの3つがすべて揃っている。
**コスト**：無料。**所要時間**：初回ビルドを含めて約0.5〜1時間。

### A.1 事前インストール（macOS）
```bash
# Xcode Command Line Tools(未インストールの場合)
xcode-select --install

# Rust(未インストールの場合)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Tauri CLI v2
cargo install tauri-cli --version "^2" --locked
cargo tauri --version   # tauri-cli 2.xと表示されるはず
```
> WindowsではさらにWebView2 Runtime（Win11には標準搭載）とMSVC Build Toolsが必要。
> Linuxでは`libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`が必要。

### A.2 アプリアイコンの生成（一度だけ）
```bash
cd src-tauri
# 1024×1024以上の正方形PNGを用意する(🐾＋amber背景)。例えばweb/public/paw-1024.pngに置く
cargo tauri icon ../web/public/paw-1024.png
# icons/32x32.png、128x128.png、128x128@2x.png、icon.icns、icon.icoが出力される
```

### A.3 sidecarのステージングと開発モード
```bash
# repoのルートディレクトリから
cargo build --release -p duduclaw-cli --bin duduclaw   # target/release/duduclawが出力される
scripts/desktop/stage-sidecar.sh                        # src-tauri/binaries/duduclaw-<triple>としてコピーされる

cd src-tauri
cargo tauri dev      # 開発用ウィンドウが起動する。beforeDevCommandが自動でViteを起動する
```

### A.4 未署名の本番ビルド（ローカルでの動作確認）
```bash
cd src-tauri
cargo tauri build
# 出力先：src-tauri/target/release/bundle/{macos,dmg}/...
# 初回に.appを直接開くとGatekeeperにブロックされる(署名していないので想定どおり)。次の方法でローカルに限り許可する：
xattr -dr com.apple.quarantine "target/release/bundle/macos/DuDuClaw.app"
open "target/release/bundle/macos/DuDuClaw.app"
```

### A.5 項目ごとの検収（TODOと対応）
| TODO項目 | 検証方法 |
| --- | --- |
| **D0🧪** | `cargo tauri dev`が起動し、ウィンドウにログインページが表示され、チャットを1件送ると応答が返ってくる |
| **D1🧪** | （a）まずlaunchd／CLI gatewayが動いていないことを確認 → アプリを開くと**sidecarが自動起動する**こと。（b）先に`duduclaw run`でポート18789を占有しておく → アプリを開くと**再起動せずに接続する**こと（Activity Monitorに`duduclaw`プロセスが1つしか見えない） |
| **D2.1🧪** | アプリを2回連続で開く → 既存のウィンドウにフォーカスが移るだけで、`duduclaw`プロセスは1つしか存在しない |
| **D2.3🧪** | 正常終了後に`ps aux | grep duduclaw`で残留プロセスがないこと。`kill -9`でアプリを強制終了して再度開くと、古いpidfile（`~/.duduclaw/desktop-sidecar.pid`）が指していた孤児プロセスを回収すること |
| **D2.4🧪** | トレイアイコンに状態が表示される。メニューのStart／Stopでsidecarを操作できる。ウィンドウを閉じてもトレイに常駐し終了しない |
| **D2.5🧪** | 手動で`kill <sidecar pid>`を実行 → アプリが指数バックオフで自動再起動すること。5回以上連続で殺すとエラー状態に入り通知が出て、無限にリトライしないこと |
| **D2.6🧪** | ターミナルではなく**Finder／Dock**からアプリを起動 → 子プロセス（Claude CLIなど）が引き続き見つかること。チャットでCLIを必要とする操作を1つ実行して検証できる |
| **D5の1項目目** | `cargo tauri build`で実行可能なアプリが生成され、sidecarが自動起動し、workspaceが開き、チャットを送信できる |
| **P6.3手動テスト** | 個人版を初めて起動しworkspaceに着地 → チャットを1件送信 → 「Advanced」に切り替えてフルダッシュボードを表示 → リロード後もモードの選択が保持されている |
| **P6.3スクリーンショット** | light／darkそれぞれ1枚ずつ撮影し、Genspark 4.0と並べてcritiqueする |

---

## ゲートE — Tauri自動更新の署名鍵を生成する（無料、最初にやる）

**ブロックしている項目**：D4.4（更新用pubkeyのプレースホルダーを置き換える）。
**止まっている理由**：`tauri.conf.json > plugins.updater.pubkey`は現在プレースホルダーの`REPLACE_WITH_...`のままで、updaterは署名を検証するために実際の鍵ペアを必要とする。鍵が用意できるまでの間、updaterは**完全に無効化**されている（`plugins.updater.active = false`かつ`bundle.createUpdaterArtifacts = false`）。そうしないと、ローカルの`cargo tauri build`が最後にupdater artifactへ署名する段階で`A public key has been found, but no private key`というエラーになる。

### 手順
```bash
cargo tauri signer generate -w ~/.tauri/duduclaw-updater.key
# ターミナルにpublic keyが表示され、private keyは~/.tauri/duduclaw-updater.keyに書き込まれる
```
1. **public key**を[src-tauri/tauri.conf.json](../../../src-tauri/tauri.conf.json)の`plugins.updater.pubkey`に貼り付ける。
2. **同じファイルでupdaterを再度有効化する**：`plugins.updater.active = true`、`bundle.createUpdaterArtifacts = true`。
3. **private keyの内容**とパスワードをGitHub repoのsecretsに設定する。
   - `TAURI_SIGNING_PRIVATE_KEY`（private keyファイルの中身）
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
4. **private keyは絶対にrepoへコミットしない。** 紛失すると公開済みのクライアントが以後の更新を受け取れなくなるため、必ずパスワードマネージャーへバックアップすること。

**検収（D4.4の半分）**：CI releaseの成果物に`latest.json`が含まれ、署名フィールドが付いていること。「旧バージョン→更新」の一連の流れをend-to-endで検証するには、署名済みのreleaseが2つ必要（ゲートBの後に行う）。

---

## ゲートB — Apple Developer ID署名＋公証（macOSリリース）

**ブロックしている項目**：D3.1🧪、D3.2🧪、D4.1、D4.4（mac側のend-to-end）、D5署名／クリーンマシン。

> **現状（2026-07、Keychainで実測）**：**署名はすでにブロック解除済み。** このマシンには有効な
> `Developer ID Application: Dudu Technology Ltd. (7469HYQ6HH)`証明書があり（有効期限2031-03、private keyは
> Keychain内、`codesign`での実測も通過済み）、すでに
> [tauri.conf.json](../../../src-tauri/tauri.conf.json)の`bundle.macOS.signingIdentity`に設定されているため、
> `cargo tauri build`は環境変数なしで自動的に署名する。**残っているのは公証のみ**：app-specific
> passwordを作成し（B.1の手順4）、`APPLE_ID`／`APPLE_PASSWORD`／
> `APPLE_TEAM_ID=7469HYQ6HH`を渡してから、2台目のクリーンなマシンでD4.1／D5を検証すればよい。

### B.1 証明書と認証情報の取得
1. ✅ Apple Developer Programアカウント＋Developer ID証明書（Team ID `7469HYQ6HH`）はすでに取得済み。
2. ✅ **Developer ID Application**証明書はすでにKeychainにあり有効（上の「現状」を参照）。
3. （CI用に）`.p12`としてエクスポートし（private key込み）、パスワードを控えておく。
4. ⬜ **app-specific password**を作成する：appleid.apple.com → Sign-In and Security → App-Specific Passwords。（公証に向けて残っている唯一のステップ）
5. ✅ **Team ID** = `7469HYQ6HH`。

### B.2 ローカルでの署名＋公証（手動で1回検証する）
```bash
# signingIdentityはすでにtauri.conf.jsonに設定済みなので、buildは自動的に署名する。公証には以下の3つの環境変数を渡す：
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="<app-specific-password>"   # B.1の手順4
export APPLE_TEAM_ID="7469HYQ6HH"
cd src-tauri && cargo tauri build          # 環境変数が揃っていれば署名＋公証＋stapleまで行われる
# もしくは先にbuildしてから、同梱スクリプトで署名＋公証＋stapleだけを個別に実行する：
../scripts/desktop/sign-notarize-macos.sh "target/release/bundle/dmg/DuDuClaw_1.31.0_aarch64.dmg"
```
> このスクリプトは[src-tauri/entitlements.plist](../../../src-tauri/entitlements.plist)のhardened runtime entitlementsを使用する。

### B.3 CI secretsとして設定する（リリースの自動化）
GitHub repoの Settings → Secrets and variables → Actions で以下を追加する。
| Secret | 値 |
| --- | --- |
| `APPLE_CERTIFICATE` | `base64 -i DeveloperID.p12`の出力全体 |
| `APPLE_CERTIFICATE_PASSWORD` | .p12のパスワード |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <名前> (<TEAMID>)` |
| `APPLE_ID`／`APPLE_PASSWORD`／`APPLE_TEAM_ID` | 上記と同じ |

### B.4 項目ごとの検収
| TODO項目 | 検証方法 | 状態 |
| --- | --- | --- |
| **D4.1🧪** | 署名＋公証済みの`.dmg`を**あなたの証明書を一度もインストールしたことがない別のMac**にコピーし、ダブルクリックする → 「from an unidentified developer」（未確認の開発者からのものです、という趣旨の警告）が表示されないこと | ✅ **検証済み**（2026-07-01、`desktop-v1.31.0`）：`stapler validate` = *worked*、`spctl -a` = accepted / Notarized Developer ID |
| **D3.1🧪** | hardened runtimeで署名した後にアプリを開き、sidecarが引き続きCLIをspawnできること／ネットワークに到達できることを確認する（チャットでネットワークを必要とする操作をトリガーする） | ⬜ 署名版アプリでの実測がまだ |
| **D3.2🧪** | Computer Useを初めて使う際、システムがAccessibility／Screen Recordingの権限プロンプトを表示し、許可後にスクリーンショット撮影／擬似入力が動作すること | ⬜ 未検証 |
| **D5署名／クリーンマシン** | D4.1と同様。加えて`spctl -a -vvv DuDuClaw.app`が`accepted`を返すこと | ✅ **検証済み**：`spctl -a -vvv` = `accepted, source=Notarized Developer ID` |

---

## ゲートC — Windows Authenticode署名

**ブロックしている項目**：D4.2。
**止まっている理由**：**Authenticodeコード署名証明書**が必要なため。

> ⚠️ **2023年6月からの重大な変更**：CA/B Forumの規定により、OV（標準）証明書であってもFIPS対応ハードウェア
> （USBトークンまたはクラウドHSM）に保管することが必須になった。もはや純粋な`.pfx`をダウンロードしてCIに投入することはできない。
> そのため自動化された署名にはクラウド署名サービスを使う必要がある。純粋な`.pfx`のパスは、
> 古い在庫証明書やクラウドHSMからエクスポートした一時的な証明書にのみ引き続き通用する。

### どこで買うか（安い順）
| 選択肢 | 種別 | 価格（目安） | CIでの自動署名 | 向いているケース |
| --- | --- | --- | --- | --- |
| **Azure Trusted Signing** | OV（Microsoft自社） | **約US$9.99／月** | ✅ ネイティブの`signtool` dlib | **第一候補**：最も安く、SmartScreenの信頼度も最良。本人確認が必要 |
| **Certumオープンソースコード署名** | OV（オープンソース専用） | **約US$30〜70／年** | ✅ SimplySignクラウド | DuDuClawはApache-2.0なので**要件を満たす**。予算重視ならこれ |
| **SSL.com eSigner** | OV／EV | OV約US$249／年〜 | ✅ eSignerクラウドAPI | 老舗で情報が豊富 |
| **DigiCert KeyLocker** | OV／EV | やや高め | ✅ KeyLocker | エンタープライズ向け |
| **Sectigo/Comodo**（販売代理店：The SSL Store、SignMyCode、Codegic） | OV／EV | OV約US$200〜400／年 | プランによる | 代理店経由だと割引があることが多い |

**OVとEVの違い**：OVは安いが、SmartScreenの信頼度は**ダウンロード数が積み上がる**まで警告が徐々にしか消えない。EVは高いが**即座に**SmartScreenを通過する。
台湾からでもオンラインでカード決済して購入可能で、手続きの中で本人確認／組織確認が行われる。

### ルート1（推奨） — Azure Trusted Signing（クラウド、約US$10／月）
1. Azureポータルで**Trusted Signing account**＋**Certificate Profile**を作成し、本人確認を完了する。
2. service principalを作成し、CI secretsとして`AZURE_TENANT_ID`、`AZURE_CLIENT_ID`、`AZURE_CLIENT_SECRET`、
   `AZURE_TS_ENDPOINT`、`AZURE_TS_ACCOUNT`、`AZURE_TS_PROFILE`を設定する。
3. CIでは純粋な`.pfx`のステップの代わりに公式actionで署名する。
   ```yaml
   - name: Azure Trusted Signing
     if: matrix.os == 'windows-latest'
     uses: azure/trusted-signing-action@v0
     with:
       azure-tenant-id: ${{ secrets.AZURE_TENANT_ID }}
       azure-client-id: ${{ secrets.AZURE_CLIENT_ID }}
       azure-client-secret: ${{ secrets.AZURE_CLIENT_SECRET }}
       endpoint: ${{ secrets.AZURE_TS_ENDPOINT }}
       trusted-signing-account-name: ${{ secrets.AZURE_TS_ACCOUNT }}
       certificate-profile-name: ${{ secrets.AZURE_TS_PROFILE }}
       files-folder: src-tauri/target
       files-folder-filter: msi,exe
       file-digest: SHA256
       timestamp-rfc3161: http://timestamp.acs.microsoft.com
       timestamp-digest: SHA256
   ```

> ⚠️ **地域制限**：Azure Trusted Signingは現在、**米国／カナダ／EU／英国**の組織、および
> **米国／カナダの個人開発者**にしか開放されていない。**台湾／マカオなどの地域は対象外**であり、
> フォームの入力やリソースの作成自体はできても、Identity Validationの段階で止まってしまい、結局は無駄になる。
> 対象地域以外の場合は**ルート2（Certum）**を使うこと。

### ルート2 — Certumオープンソース証明書（クラウドSimplySign、**地域制限なし、台湾／マカオでも利用可**）
1. [shop.certum.eu](https://shop.certum.eu/)で「Open Source Code Signing」を検索し、**「Open Source Code
   Signing in the Cloud」（クラウド版、約€49）を購入する**。3つのエディションの違いは次のとおり。
   - *code*（€25）：証明書のみで、**Certum暗号カードとカードリーダーは自分で用意する**必要があるため、今回には向かない。
   - *set*（€69）：実物のカード＋カードリーダーが付属するが、国際発送が必要でCIを自動化しづらいため、これも向かない。
   - **in the Cloud（€49）：証明書がクラウド上にあり、ハードウェア不要。これを選ぶ。**
2. 個人の本人確認を完了する（海外からの申請も可能で、身分証のアップロードが必要）。あわせてDuDuClawのGitHubリンクを添えてオープンソースであることを証明する。
3. **SimplySign**をインストールする（クラウド上の証明書をローカルで使える署名デバイスとしてマッピングする）。CLIを使ってもよい。
4. 署名ツール：
   - Windows：`signtool`をSimplySign（PKCS#11／CSP）に接続して使う。
   - **Mac／Linux／CI**：**`osslsigncode`**をSimplySignのクラウド鍵と組み合わせて使う。Windowsを一切開かなくても`.msi`に署名できる。

> 💳 **支払いに関する注意（2026-06実測）**：Certumの決済（Autopay、EU）は**Visa／Mastercardのみ対応**で、
> **JCBは使えない**。国境をまたぐApple Payでは「service unavailable」（サービスを利用できません、という趣旨のエラー）が頻発する。JCBしか持っていない場合は次のいずれか。
> ①PayPalを試す（JCBに対応していることが多い）。②**Wise／Revolutのバーチャルカード（Visa）**を作る（台湾／マカオからでも申請可能で、後々の
> Apple Developerの$99や各種SaaSの支払いにも使えるため強くおすすめ）。③Visa／Mastercardを持っている人に代わりに決済してもらう。

### ルート3（バックアップ） — 純粋な.pfx（古い在庫証明書／HSMからエクスポートした一時的な証明書のみ）
既存の[sign-windows.ps1](../../../scripts/desktop/sign-windows.ps1)スクリプトをそのまま使う。secretsに
`WINDOWS_CERT_PFX_BASE64`と`WINDOWS_CERT_PASSWORD`を設定し、ローカルでは手動で実行できる。
```powershell
pwsh scripts/desktop/sign-windows.ps1 -Artifact path\to\DuDuClaw_1.30.1_x64.msi
```

> **おすすめ**：**米国／カナダ／EU／英国**であればルート1（Azure、約$10／月、SmartScreenに最も友好的）。
> **台湾／マカオなどそれ以外の地域**であればルート2（Certum Cloud、€49）。地域制限がなくCIにも対応できる唯一の選択肢。

### ⏭️ このゲートは後回しにできる（優先順位について）
**Windows署名はPhase D全体の中で最も優先度が低く、後回しにしてよい項目である。** これでプロジェクトを止めないこと。
- 未署名のWindowsインストーラーでも**インストール自体は問題なくできる**。SmartScreenが一度「Unknown publisher」（発行元不明、という趣旨の表示）を出すだけで、
  ユーザーが「Run anyway」（このまま実行、という趣旨のボタン）をクリックすれば進める。
- macOSで開発していて、対象ユーザーもMac／台湾寄りであれば、**まずゲートA（ローカルで動かす）＋ゲートB（macOS署名）を優先する**。
  Windowsは**未署名版を先にリリース**し、Wise／Revolutのカードが用意できたり、実際にWindowsユーザーからの要望が出てきたりしてから署名を追加すればよい。
- CI側では、repoの変数`WINDOWS_SIGN_METHOD`が未設定の場合、署名ステップは**自動的にskip**される（[desktop-release.yml](../../../.github/workflows/desktop-release.yml)を参照）ため、他プラットフォームのリリースには影響しない。

**検収（D4.2🧪）**：クリーンなWindows環境で署名済みの`.msi`をダウンロードし、SmartScreenに**ブロックされない**こと（OVは信頼度の蓄積が必要、EV／Azureの方が早く通過する）。

---

## ゲートD — Linuxパッケージング検証

**ブロックしている項目**：D4.3🧪。
**止まっている理由**：`.AppImage`／`.deb`をテストするためのLinux環境／VMが必要なため。署名は不要。

### 手順
```bash
# Ubuntu 22.04(またはすでに設定済みのCI環境)で
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
cd src-tauri && cargo tauri build
# 出力先：target/release/bundle/{appimage,deb}/...
```
**検収**：UbuntuとFedoraそれぞれで`.AppImage`を1回ずつ実行し、アプリが起動してgatewayに接続できること。

---

## ゲートF — end-to-endの自動更新（B＋Eの完了が必要）

**ブロックしている項目**：D4.4🧪、D5自動更新。

### 手順
1. ゲートEのpubkeyが入力済みで、private keyがsecretsに登録されていることを確認する。
2. 最初のバージョンを公開する：`git tag desktop-v1.30.1 && git push origin desktop-v1.30.1`（CIがrelease＋`latest.json`を生成する）。
3. そのバージョンをテスト用マシンにインストールする。
4. `src-tauri/tauri.conf.json`のversionを`1.30.2`にbumpし、2つ目のtagを公開する。
5. 旧バージョンのアプリを開く → 新バージョンを検知する → 署名を検証する → ダウンロードする → 再起動を促す → 反映される、という流れになること。
6. **ネガティブテスト**：誤った鍵で偽の更新に署名する → クライアントが**インストールを拒否する**こと（署名検証が失敗する）。

---

## ワンタイムチェックリスト（すべて解除）
- [ ] ゲートA：`cargo tauri build`でローカルにアプリが生成され、ライフサイクル検収7項目がすべて通る（D0/D1/D2.*/D5-1/P6.3）
- [ ] ゲートE：updater鍵を生成し、pubkeyを入力し、private keyをsecretsに登録した（D4.4の半分）
- [ ] ゲートB：Apple証明書 → 署名＋公証済みで、クリーンなMacでもブロックされない（D3.1/D3.2/D4.1/D5）
- [ ] ゲートC：Windows証明書 → 署名済みで、SmartScreenにブロックされない（D4.2）
- [ ] ゲートD：Linuxの`.AppImage`／`.deb`が動く（D4.3）
- [ ] ゲートF：2つのバージョン間で自動更新が成功し、署名不一致は拒否される（D4.4/D5）

> すべて完了したら、対応するTODO項目を`[ ]`/`[~]`から`[x]`に変更する。
