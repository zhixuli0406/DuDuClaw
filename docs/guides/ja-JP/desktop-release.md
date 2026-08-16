# デスクトップアプリ：リリース・署名・自動更新

開発者クレデンシャル関連の作業をカバーします（TODO §D4）。パイプライン
本体は
[`.github/workflows/desktop-release.yml`](../../../.github/workflows/desktop-release.yml)
で、`desktop-v*`タグをpushするとトリガーされます。

## 1. Updater署名鍵（一度だけ）

```bash
cargo tauri signer generate -w ~/.tauri/duduclaw.key
```

- **公開鍵**（public key）は`src-tauri/tauri.conf.json >
  plugins.updater.pubkey`に入れる。
- **秘密鍵**（private key）とパスワードはrepo secrets
  `TAURI_SIGNING_PRIVATE_KEY` ／ `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`に
  入れる。

updaterのエンドポイントはGitHub上の`latest.json`です（設定済み）。リ
リースのたびに`tauri-action`が署名済みの`latest.json`を生成し、クライ
アントはインストール前に署名を検証、一致しなければインストールを拒否
します（§D4.4）。

## 2. macOS：Developer IDと公証（notarization）

必要なsecrets：

| Secret | 内容 |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Applicationの`.p12`証明書、base64エンコード |
| `APPLE_CERTIFICATE_PASSWORD` | その証明書のパスワード |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <Name> (<TEAMID>)` |
| `APPLE_ID` / `APPLE_PASSWORD` | Appleアカウント + app専用パスワード |
| `APPLE_TEAM_ID` | 10桁のteam id |

これらのsecretsが揃っていれば、`tauri-action`が（`src-tauri/entitlements.plist`
を使ったhardened runtimeで）署名し、公証も行います。手動でartifactを処
理する場合は
[`scripts/desktop/sign-notarize-macos.sh`](../../../scripts/desktop/sign-notarize-macos.sh)
を使います。

**受け入れ基準（§D4.1）：**その証明書に一度も触れたことのないマシンで
`.dmg`をダウンロードし、Gatekeeperの「開発元が未確認」という警告なし
に開けること。

## 3. Windows：Authenticode

必要なsecretsは`WINDOWS_CERT_PFX_BASE64` ／ `WINDOWS_CERT_PASSWORD`で
す。workflowは
[`scripts/desktop/sign-windows.ps1`](../../../scripts/desktop/sign-windows.ps1)
経由で`.msi`に署名します。**受け入れ基準（§D4.2）：**SmartScreenにブ
ロックされないこと（EV証明書 = 即時信頼）。

## 4. Linux

`.AppImage`と`.deb`は未署名でビルドされます（WebKitGTKランタイム依存
は宣言済み）。リリースを止める要因にはなりません（§D4.3）。

## 5. リリースを切る

通常の経路はコアのリリース儀式そのものです。`scripts/release.sh
<bump>`が他の全manifestと一緒に`src-tauri/tauri.conf.json`のバージョ
ンを上げ、そのbump commitに`v<X.Y.Z>`と`desktop-v<X.Y.Z>`の両方のtag
を打ちます。続いて`git push --tags`がコアのパイプラインと一緒にこのパ
イプラインもトリガーします。（デスクトップ版はかつてこれらが別々の手
動ステップだった間、11回のリリースにわたって1.33.0のまま凍結していま
した。手動tag付けには絶対に戻らないでください。）

既存バージョンをデスクトップ版だけ再ビルドしたい場合:

```bash
git tag desktop-v1.44.0 <bump-commit>
git push origin desktop-v1.44.0
```

このworkflowは4つのターゲットからなるmatrixをビルドし、署名／公証を
行った上で、インストーラーと`latest.json`を添えて**直接リリースを公
開**します（draftを経由しません）。最後のjobが`latest.json`を固定タ
グ`desktop-updater`のreleaseにコピーします。この固定URLがupdaterのエ
ンドポイントになっているのは、`releases/latest/...`だとずっと頻繁に
出るコア版のリリースを指してしまい、404になるためです。

## 6. 証明書の衛生管理

- 証明書／鍵は絶対にcommitしない。GitHub secretsのみに置く。
- 人員が変わったらApple app専用パスワードとupdater鍵をローテーション
  する。
- `tauri.conf.json`のバージョンをコアの`Cargo.toml`のworkspaceバー
  ジョンと一致させ、updaterがshellとcoreの不一致な組み合わせを出荷し
  ないようにする（§D4.4／§D4.5）。
