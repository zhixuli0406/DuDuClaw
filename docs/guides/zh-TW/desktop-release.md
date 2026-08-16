# 桌面 App：發布、簽章與自動更新

涵蓋開發者憑證相關的工作（TODO §D4）。發布流程定義在
[`.github/workflows/desktop-release.yml`](../../../.github/workflows/desktop-release.yml)，
推送 `desktop-v*` tag 即可觸發。

## 1. Updater 簽章金鑰（一次性設定）

```bash
cargo tauri signer generate -w ~/.tauri/duduclaw.key
```

- 把**公開金鑰**（public key）填入 `src-tauri/tauri.conf.json > plugins.updater.pubkey`。
- 把**私密金鑰**（private key）與密碼存進 repo secrets
  `TAURI_SIGNING_PRIVATE_KEY` ／ `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。

更新端點是 GitHub 上的 `latest.json`（已經設定好）。每次發布時
`tauri-action` 都會產生一份簽過章的 `latest.json`；客戶端安裝前會驗證簽章，
簽章不符就拒絕安裝（§D4.4）。

## 2. macOS：Developer ID 與公證（notarization）

所需 secrets：

| Secret | 內容 |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application 的 `.p12` 憑證，base64 編碼 |
| `APPLE_CERTIFICATE_PASSWORD` | 該憑證的密碼 |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <Name> (<TEAMID>)` |
| `APPLE_ID` / `APPLE_PASSWORD` | Apple 帳號 + app 專用密碼 |
| `APPLE_TEAM_ID` | 10 碼的 team id |

只要這些 secrets 都存在，`tauri-action` 就會簽章（hardened runtime，使用
`src-tauri/entitlements.plist`）並送公證。若要手動處理某個 artifact，用
[`scripts/desktop/sign-notarize-macos.sh`](../../../scripts/desktop/sign-notarize-macos.sh)。

**驗收標準（§D4.1）：**在一台從未接觸過該憑證的機器上下載 `.dmg`，應該可以
直接開啟，不會跳出 Gatekeeper 的「無法識別開發者」警告。

## 3. Windows：Authenticode

所需 secrets 為 `WINDOWS_CERT_PFX_BASE64` ／ `WINDOWS_CERT_PASSWORD`。workflow
會透過
[`scripts/desktop/sign-windows.ps1`](../../../scripts/desktop/sign-windows.ps1)
簽署 `.msi`。**驗收標準（§D4.2）：**SmartScreen 不會擋下（EV 憑證＝立即信
任）。

## 4. Linux

`.AppImage` 與 `.deb` 都是未簽章建置（已宣告 WebKitGTK 執行期相依）。不會
擋住發布（§D4.3）。

## 5. 切一次發布

一般走法就是核心發布儀式：`scripts/release.sh <bump>` 會連同其他所有
manifest 一起把 `src-tauri/tauri.conf.json` 版本號往上調，並在該 bump
commit 上同時打 `v<X.Y.Z>` 與 `desktop-v<X.Y.Z>` 兩個 tag；接著
`git push --tags` 就會連同核心流程一起觸發這條 pipeline。（桌面版曾經卡
在 1.33.0 長達 11 次發布，就是因為這些步驟被拆成手動流程分開做，千萬別
走回頭路手動打 tag。）

若只想針對既有版本重新建置桌面版：

```bash
git tag desktop-v1.44.0 <bump-commit>
git push origin desktop-v1.44.0
```

這個 workflow 會建置 4 個目標平台的矩陣，完成簽章／公證，並**直接發布**
（不經過 draft）附上安裝檔與 `latest.json`。最後一個 job 會把
`latest.json` 複製到固定 tag `desktop-updater` 的 release 上，這個固定
URL 就是更新端點，因為 `releases/latest/...` 會指向更新頻繁得多的核心版
本發布，反而會 404。

## 6. 憑證衛生守則

- 憑證／金鑰絕不進 commit，只能放在 GitHub secrets。
- 人事異動時要輪替 Apple app 專用密碼與 updater 金鑰。
- 讓 `tauri.conf.json` 的版本號與核心 `Cargo.toml` workspace 版本保持一致，
  避免 updater 出貨 shell 與核心版本不對應的組合（§D4.4／§D4.5）。
