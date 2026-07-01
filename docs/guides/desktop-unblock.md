# 桌面 App 阻塞項目解除指南(Phase D + Phase 6 手測)

> 對應 [TODO-genspark-workspace-shell.md](../todo/TODO-genspark-workspace-shell.md) 中標為 `[ ]`(阻塞)
> 與 `[~]`(已寫未 build 驗證)的項目。這些項目**不是缺程式碼**,而是缺**工具鏈 / 憑證 / 圖形環境 / 第二台機器**。
> 本文把每個阻塞點拆成「為什麼擋住 → 前置 → 步驟 → 對應 TODO 項驗收」。
>
> **建議順序**:關卡 A(本機,免費,半天)→ 關卡 E(更新金鑰,免費)→ 關卡 B(macOS 簽章,需付費帳號)
> → 關卡 C(Windows 簽章)→ 關卡 D(Linux)。A 做完就能自用 + 跑完所有生命週期驗收。

---

## 關卡 A — 安裝 Tauri 工具鏈,本機跑起來(免簽章)

**擋住的項目**:D0 🧪、D1 🧪、D2.1/D2.3/D2.4/D2.5/D2.6 🧪、D5 第 1 項、P6.3 手測 + 截圖。
**為什麼擋住**:此撰寫環境沒有 Tauri CLI、沒有系統 WebView 開發依賴、沒有顯示器。你的 Mac 三者皆有。
**成本**:免費。**時間**:約 0.5–1 小時(含首次編譯)。

### A.1 前置安裝(macOS)
```bash
# Xcode Command Line Tools(若未裝)
xcode-select --install

# Rust(若未裝)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Tauri CLI v2
cargo install tauri-cli --version "^2" --locked
cargo tauri --version   # 應印出 tauri-cli 2.x
```
> Windows 另需 WebView2 Runtime(Win11 內建)+ MSVC Build Tools;
> Linux 需 `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`。

### A.2 產生 App 圖示(一次性)
```bash
cd src-tauri
# 準備一張 ≥1024×1024 的方形 PNG(🐾 + amber 底),例如放到 web/public/paw-1024.png
cargo tauri icon ../web/public/paw-1024.png
# 產出 icons/32x32.png、128x128.png、128x128@2x.png、icon.icns、icon.ico
```

### A.3 staged sidecar + 開發模式
```bash
# 從 repo 根目錄
cargo build --release -p duduclaw-cli --bin duduclaw   # 產出 target/release/duduclaw
scripts/desktop/stage-sidecar.sh                        # 複製成 src-tauri/binaries/duduclaw-<triple>

cd src-tauri
cargo tauri dev      # 開發視窗;beforeDevCommand 會自動起 Vite
```

### A.4 不簽章正式打包(本機驗證體驗)
```bash
cd src-tauri
cargo tauri build
# 產物:src-tauri/target/release/bundle/{macos,dmg}/...
# 首次直接開 .app 會被 Gatekeeper 擋(正常,因為還沒簽章)——用下列方式本機放行:
xattr -dr com.apple.quarantine "target/release/bundle/macos/DuDuClaw.app"
open "target/release/bundle/macos/DuDuClaw.app"
```

### A.5 逐項驗收(對照 TODO)
| TODO 項 | 怎麼驗 |
| --- | --- |
| **D0 🧪** | `cargo tauri dev` 起得來、視窗顯示登入頁、送一句 chat 有回應 |
| **D1 🧪** | (a) 先確保沒有 launchd/CLI gateway → 開 App,應**自啟 sidecar**;(b) 先 `duduclaw run` 佔住 18789 → 開 App,應**附掛不重啟**(Activity Monitor 只看到一個 `duduclaw`) |
| **D2.1 🧪** | 連續開兩次 App → 只聚焦同一視窗、只有一個 `duduclaw` 進程 |
| **D2.3 🧪** | 正常退出後 `ps aux | grep duduclaw` 無殘留;`kill -9` App 後重開,應回收舊 pidfile(`~/.duduclaw/desktop-sidecar.pid`)指向的孤兒 |
| **D2.4 🧪** | 託盤圖示顯示狀態;選單 Start/Stop 能驅動 sidecar;關窗縮回託盤不退出 |
| **D2.5 🧪** | 手動 `kill <sidecar pid>` → App 應指數退避自動重啟;連殺 5 次以上 → 進 error 態 + 通知,不無限重試 |
| **D2.6 🧪** | 從 **Finder/Dock**(非終端機)啟動 App → 確認子進程(Claude CLI 等)仍找得到;可在 chat 觸發一個需要 CLI 的動作驗證 |
| **D5 第 1 項** | `cargo tauri build` 出可執行 App、自啟 sidecar、開工作空間、可送 chat |
| **P6.3 手測** | 個人版首次落 workspace → 送一句進對話 → 切「進階」見完整儀表板 → 重整後模式記憶留存 |
| **P6.3 截圖** | light/dark 各截一張,和 Genspark 4.0 並排做 critique |

---

## 關卡 E — 產生 Tauri 自動更新簽章金鑰(免費,先做)

**擋住的項目**:D4.4(更新 pubkey 佔位字串)。
**為什麼擋住**:`tauri.conf.json > plugins.updater.pubkey` 目前是 `REPLACE_WITH_...` 佔位;updater 必須有金鑰對才會驗章。在金鑰備妥前,updater 已**整個關閉**(`plugins.updater.active = false` 且 `bundle.createUpdaterArtifacts = false`),否則本機 `cargo tauri build` 會在最後簽 updater artifact 時報 `A public key has been found, but no private key`。

### 步驟
```bash
cargo tauri signer generate -w ~/.tauri/duduclaw-updater.key
# 終端會印出 public key,並把私鑰寫到 ~/.tauri/duduclaw-updater.key
```
1. 把 **public key** 貼進 [src-tauri/tauri.conf.json](../../src-tauri/tauri.conf.json) 的 `plugins.updater.pubkey`。
2. **同檔把 updater 開回來**:`plugins.updater.active = true`、`bundle.createUpdaterArtifacts = true`。
3. 把**私鑰內容**與密碼設成 GitHub repo secrets:
   - `TAURI_SIGNING_PRIVATE_KEY`(私鑰檔內容)
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
4. **私鑰永不入庫**;遺失會導致已發佈的客戶端無法再收到更新,務必備份到密碼管理器。

**驗收(D4.4 一半)**:CI release 後產物含 `latest.json` 且帶簽章欄位。端到端「舊版→更新」需先有兩個已簽章 release(見關卡 B 後再做)。

---

## 關卡 B — Apple Developer ID 簽章 + 公證(macOS 發佈)

**擋住的項目**:D3.1 🧪、D3.2 🧪、D4.1、D4.4(mac 端到端)、D5 簽章/乾淨機。

> **現況(2026-07 實測 Keychain)**:**簽章已解鎖** —— 本機有一張**有效**的
> `Developer ID Application: Dudu Technology Ltd. (7469HYQ6HH)`(到 2031-03,私鑰在
> Keychain,`codesign` 實測通過),且已寫進
> [tauri.conf.json](../../src-tauri/tauri.conf.json) `bundle.macOS.signingIdentity`,
> 所以 `cargo tauri build` 會自動簽章(免帶 env)。**剩下只差公證**:建立
> app-specific password(B.1 第 4 步)並帶 `APPLE_ID` / `APPLE_PASSWORD` /
> `APPLE_TEAM_ID=7469HYQ6HH` 即可,再到第二台乾淨機驗 D4.1 / D5。

### B.1 取得憑證與認證資訊
1. ✅ 已有 Apple Developer Program 帳號 + Developer ID 憑證(Team ID `7469HYQ6HH`)。
2. ✅ **Developer ID Application** 憑證已在 Keychain 且有效(見上「現況」)。
3. (CI 用)匯出成 `.p12`(含私鑰),記下密碼。
4. ⬜ 建立 **app-specific password**:appleid.apple.com → 登入與安全性 → App 專用密碼。（公證唯一還缺的一步）
5. ✅ **Team ID** = `7469HYQ6HH`。

### B.2 本機簽章 + 公證(手動驗一次)
```bash
# signingIdentity 已在 tauri.conf.json,build 會自動簽章。公證再帶下面三個 env:
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="<app-specific-password>"   # B.1 第 4 步
export APPLE_TEAM_ID="7469HYQ6HH"
cd src-tauri && cargo tauri build          # 簽章 + 公證 + staple(env 齊全時)
# 或先 build 再用內附腳本單獨簽章 + 公證 + 釘選:
../scripts/desktop/sign-notarize-macos.sh "target/release/bundle/dmg/DuDuClaw_1.31.0_aarch64.dmg"
```
> 腳本已用 [src-tauri/entitlements.plist](../../src-tauri/entitlements.plist) 的 hardened runtime entitlements。

### B.3 設成 CI secrets(自動化發佈)
在 GitHub repo → Settings → Secrets and variables → Actions 新增:
| Secret | 值 |
| --- | --- |
| `APPLE_CERTIFICATE` | `base64 -i DeveloperID.p12`(整段) |
| `APPLE_CERTIFICATE_PASSWORD` | .p12 密碼 |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <名字> (<TEAMID>)` |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | 同上 |

### B.4 逐項驗收
| TODO 項 | 怎麼驗 | 狀態 |
| --- | --- | --- |
| **D4.1 🧪** | 把簽章+公證後的 `.dmg` 傳到**另一台從未裝過你憑證的 Mac**,雙擊 → **不**跳「來自未識別開發者」 | ✅ **已驗**(2026-07-01,`desktop-v1.31.0`):`stapler validate` = *worked*、`spctl -a` = accepted / Notarized Developer ID |
| **D3.1 🧪** | 簽章+hardened 後開 App,sidecar 仍能 spawn CLI / 連網(在 chat 觸發需網路的動作) | ⬜ 待在簽章版 App 內實跑 |
| **D3.2 🧪** | 首次用 Computer Use → 系統跳 Accessibility / Screen Recording 授權框,授權後截圖/模擬輸入可動 | ⬜ 待驗 |
| **D5 簽章/乾淨機** | 同 D4.1,且 `spctl -a -vvv DuDuClaw.app` 回 `accepted` | ✅ **已驗**:`spctl -a -vvv` = `accepted, source=Notarized Developer ID` |

---

## 關卡 C — Windows Authenticode 簽章

**擋住的項目**:D4.2。
**為什麼擋住**:需要 **Authenticode 程式碼簽章憑證**。

> ⚠️ **2023/6 起的重大變更**:CA/B Forum 規定**連 OV(標準)憑證也必須存放於 FIPS 硬體**
> (USB token 或雲端 HSM),**不能再下載純 `.pfx` 丟進 CI**。因此自動化簽章要走**雲端簽章方案**;
> 純 `.pfx` 路徑僅適用於舊庫存憑證或雲端 HSM 匯出的暫時憑證。

### 去哪裡買(由便宜到貴)
| 方案 | 類型 | 價格(約) | CI 自動簽 | 適合 |
| --- | --- | --- | --- | --- |
| **Azure Trusted Signing** | OV(微軟自家) | **~US$9.99/月** | ✅ 原生 `signtool` dlib | **首選**:最便宜、SmartScreen 信譽最好;需通過身分驗證 |
| **Certum 開源程式碼簽章** | OV(開源專用) | **~US$30–70/年** | ✅ SimplySign 雲端 | DuDuClaw 是 Apache-2.0 → **符合資格**,預算首選 |
| **SSL.com eSigner** | OV / EV | OV ~US$249/年起 | ✅ eSigner 雲 API | 老牌、文件完整 |
| **DigiCert KeyLocker** | OV / EV | 偏高 | ✅ KeyLocker | 企業級 |
| **Sectigo/Comodo**(經銷商:The SSL Store、SignMyCode、Codegic) | OV / EV | OV ~US$200–400/年 | 視方案 | 經銷商常有折扣 |

**OV vs EV**:OV 便宜但 SmartScreen 信譽需**累積下載量**才漸無警告;EV 貴但**即時通過** SmartScreen。
台灣可線上刷卡購買,過程會做身分 / 組織驗證。

### 路徑 1(推薦)— Azure Trusted Signing(雲端,~US$10/月)
1. Azure 入口建立 **Trusted Signing account** + **Certificate Profile**,完成身分驗證。
2. 建立 service principal,設 CI secrets:`AZURE_TENANT_ID`、`AZURE_CLIENT_ID`、`AZURE_CLIENT_SECRET`、
   `AZURE_TS_ENDPOINT`、`AZURE_TS_ACCOUNT`、`AZURE_TS_PROFILE`。
3. CI 用官方 action 簽章(取代純 `.pfx` 步驟):
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

> ⚠️ **地區限制**:Azure Trusted Signing 目前只開放給**美 / 加 / 歐盟 / 英國的組織**,以及
> **美 / 加的個人開發者**。**台灣 / 澳門等地不符資格** —— 表單能填、資源能建,但 **Identity
> Validation 會卡住**,等於白做。非上述地區請改走**路徑 2(Certum)**。

### 路徑 2 — Certum 開源憑證(雲端 SimplySign,**無地區限制,台/澳適用**)
1. 到 [shop.certum.eu](https://shop.certum.eu/) 搜「Open Source Code Signing」,**買「Open Source Code
   Signing in the Cloud」(雲端版,約 €49)** —— 三個版本差別:
   - *code*(€25):只給憑證,**要自備** Certum 加密卡 + 讀卡機 → 不適合。
   - *set*(€69):含實體卡 + 讀卡機 → 要國際寄送、CI 難自動化 → 不適合。
   - **in the Cloud(€49):憑證放雲端,免硬體 → 選這個。**
2. 完成個人身分驗證(接受國際申請,上傳證件),附 DuDuClaw GitHub 連結證明開源。
3. 安裝 **SimplySign**(把雲端憑證映射成本機可用的簽章裝置),或用其 CLI。
4. 簽章工具:
   - Windows:`signtool` 接 SimplySign(PKCS#11 / CSP)。
   - **Mac / Linux / CI**:用 **`osslsigncode`** 搭 SimplySign 雲端金鑰,**不必開 Windows** 也能簽 `.msi`。

> 💳 **付款注意(2026-06 實測)**:Certum 金流(Autopay,EU)**只收 Visa / Mastercard**,
> **不收 JCB**;Apple Pay 跨境常出現「無法取得服務」。若手上只有 JCB:
> ① 試 PayPal(JCB 多半可綁);② 辦一張 **Wise / Revolut 虛擬 Visa**(台/澳可申請,日後 Apple
> Developer $99、各種 SaaS 都用得到,強烈建議);③ 請有 Visa/MC 的人代刷。

### 路徑 3(後備)— 純 .pfx(僅舊庫存 / HSM 匯出的暫時憑證)
維持原腳本 [sign-windows.ps1](../../scripts/desktop/sign-windows.ps1):設 secrets
`WINDOWS_CERT_PFX_BASE64`、`WINDOWS_CERT_PASSWORD`,本機可手動:
```powershell
pwsh scripts/desktop/sign-windows.ps1 -Artifact path\to\DuDuClaw_1.30.1_x64.msi
```

> **建議**:**美/加/歐/英**走路徑 1(Azure,~$10/月、SmartScreen 最友善);
> **台灣 / 澳門等其他地區**走**路徑 2(Certum Cloud €49)**——唯一不受地區限制又能接 CI 的選項。

### ⏭️ 這關可以延後(優先順序提醒)
**Windows 簽章是整個 Phase D 裡最低優先、可延後的一項**,別讓它擋住專案:
- 沒簽章的 Windows 安裝檔**仍可安裝**,只是 SmartScreen 會跳一次「未知發行者」,使用者按
  「仍要執行」即可。
- 開發者在 macOS、受眾偏 Mac / 台灣時,**先做關卡 A(本機跑起來)+ 關卡 B(macOS 簽章)**;
  Windows 可**先出不簽章版**,等辦好 Wise/Revolut 卡或確有 Windows 使用者需求再補簽。
- 對應 CI:repo 變數 `WINDOWS_SIGN_METHOD` 不設時,簽章步驟會**自動 skip**(見
  [desktop-release.yml](../../.github/workflows/desktop-release.yml)),不影響其他平台發佈。

**驗收(D4.2 🧪)**:在乾淨 Windows 下載已簽 `.msi`,SmartScreen **不**攔(OV 需累積信譽,EV/Azure 較快)。

---

## 關卡 D — Linux 打包驗證

**擋住的項目**:D4.3 🧪。
**為什麼擋住**:需要 Linux 環境 / VM 測試 `.AppImage` / `.deb`。免簽章。

### 步驟
```bash
# 在 Ubuntu 22.04(或 CI 已配好)
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
cd src-tauri && cargo tauri build
# 產物:target/release/bundle/{appimage,deb}/...
```
**驗收**:在 Ubuntu / Fedora 各跑一次 `.AppImage`,App 起得來、能連 gateway。

---

## 關卡 F — 端到端自動更新(需 B + E 完成)

**擋住的項目**:D4.4 🧪、D5 自動更新。

### 步驟
1. 確認關卡 E 的 pubkey 已填、私鑰在 secrets。
2. 發第一個版本:`git tag desktop-v1.30.1 && git push origin desktop-v1.30.1`(CI 產 release + `latest.json`)。
3. 安裝該版到測試機。
4. bump `src-tauri/tauri.conf.json` version → `1.30.2`,發第二個 tag。
5. 開舊版 App → 應偵測新版 → 驗章 → 下載 → 提示重啟 → 生效。
6. **負向測試**:用錯誤金鑰簽一個假更新 → 客戶端應**拒絕安裝**(驗章失敗)。

---

## 一次性檢查清單(全部解除)
- [ ] 關卡 A:`cargo tauri build` 本機出 App,生命週期 7 項驗收綠(D0/D1/D2.*/D5-1/P6.3)
- [ ] 關卡 E:updater 金鑰生成、pubkey 填入、私鑰進 secrets(D4.4 一半)
- [ ] 關卡 B:Apple 憑證 → 簽章+公證,乾淨 Mac 不被擋(D3.1/D3.2/D4.1/D5)
- [ ] 關卡 C:Windows 憑證 → 簽章,SmartScreen 不擋(D4.2)
- [ ] 關卡 D:Linux `.AppImage`/`.deb` 可跑(D4.3)
- [ ] 關卡 F:兩版之間自動更新成功 + 驗章拒絕不符(D4.4/D5)

> 全部完成後,把 TODO 對應項從 `[ ]`/`[~]` 改成 `[x]`。
