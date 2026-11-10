# 更新後的 macOS 資料夾存取權限

語言：[English](../macos-folder-access.md) · 繁體中文 · [日本語](../ja-JP/macos-folder-access.md)

macOS 會用隱私權的「檔案和資料夾」保護部分資料夾：`~/Desktop`（桌面）、
`~/Documents`（文件）、`~/Downloads`（下載項目）、iCloud 雲碟
（`~/Library/Mobile Documents`）、雲端儲存資料夾（`~/Library/CloudStorage`），
以及外接磁碟與網路磁碟（`/Volumes/…`）。程式必須先被允許一次，才能讀取這些位置。

## 你會看到什麼

DuDuClaw 更新後，macOS 可能會再次詢問。如果當下沒有人在這台 Mac 前按「允許」：

- AI 員工無法讀取這些資料夾裡的檔案。讀取會立刻失敗，或在 macOS 提示視窗顯示期間一直等到逾時。
- 儀表板會對管理員顯示橫幅，列出受影響的資料夾。
- 已綁定通訊頻道的管理員會在狀態改變時（等待中 → 被拒絕 → 已恢復）收到**一則**訊息；同一個狀態不會重複通知，勿擾時段內的訊息會延到時段結束後送出。
- 儀表板的健康檢查會出現「macOS 資料夾存取權限」一列。
- 檔案工具會回覆明確的說明，而不是籠統的失敗；在 macOS 等待授權期間逾時的對話回覆，也會說明原因。

DuDuClaw 只檢查自己實際會用到的資料夾：`DUDUCLAW_HOME`、`[files] allowed_roots`
的每一個項目，以及委派任務的工作資料夾。檢查時機是啟動時（也就是更新後的重新啟動）、
在儀表板儲存 `[files] allowed_roots` 之後，以及尚未授權期間每 10 分鐘一次。
macOS 沒有提供「不觸發詢問就能查詢目前狀態」的方法，所以檢查會實際讀取每個資料夾。

## 為什麼更新後會再問一次

LaunchAgent 直接執行閘道程式，因此 macOS 把閘道視為提出存取要求的程式，
包括它自己，以及它啟動的每一個 AI 工具。

從 v1.81.0 起，macOS 閘道以已簽署並公證的 App `DuDuClaw Gateway.app` 發布
（識別碼 `ai.duduclaw.gateway`，CE 與 Pro 相同）。更新前後的身分保持一致，
macOS 會保留資料夾、自動化與完整磁碟取用權限：允許一次之後，之後的更新就不會再問。

仍以單一命令列執行檔執行的閘道（v1.81.0 之前安裝、尚未移轉）沒有固定的 App 簽章，
每次更新在 macOS 眼中都像是新的程式，所以每次更新都會問一次。可用下列三種方式移到 App：

- 開啟自動更新、由登入服務（launchd）啟動時，v1.81 第一次啟動會自動移轉一次。
- 在儀表板「系統 → 更新」頁按「移到 DuDuClaw Gateway.app」（管理員）。
- 在終端機執行 `duduclaw service migrate-bundle`，再執行 `duduclaw service restart`。

移轉後原本的執行檔會換成指向 App 的連結，原本的指令照常可用。移轉後 macOS 會為新的 App
**再詢問一次**資料夾存取權限。App 更新時的檢查方式見[儀表板設定](dashboard-settings.md)。

## 如何重新授權

1. 到執行 DuDuClaw 的這台 Mac 前，在提示視窗按「允許」。
2. 如果錯過提示或按了「不允許」：打開「系統設定 › 隱私權與安全性 › 檔案和資料夾」，
   找到 **DuDuClaw Gateway**（仍以單一執行檔執行的閘道則是 **duduclaw**），
   開啟需要的項目（文件、桌面、下載項目、網路卷宗、可移除的卷宗）。
3. 不需要重新啟動。在 DuDuClaw 正等待的提示上作答，幾秒內就會偵測到；
   在系統設定中變更，則會在下一次 10 分鐘檢查時偵測到。之後橫幅會消失，管理員也會收到「已恢復」的訊息。

## 從根本避免：檔案放哪裡

最簡單的做法，是不要把 DuDuClaw 用到的檔案放在受保護的資料夾。

- **AI 員工的工作檔案：**建立一個一般資料夾，例如 `~/DuDuClaw-files`，把檔案搬進去，
  再到儀表板設定把它加入 `[files] allowed_roots`。直接位於個人專屬資料夾下的資料夾不受保護。
- **`DUDUCLAW_HOME`：**維持預設的 `~/.duduclaw`。不要放在文件、桌面或 iCloud 雲碟——
  iCloud 還可能把檔案移到雲端只留佔位檔，會損壞資料庫。
- **共用磁碟：**網路磁碟或 USB 磁碟一律受保護。如果非用不可，為 DuDuClaw Gateway
  允許一次「網路卷宗」／「可移除的卷宗」。仍以單一執行檔執行的閘道，在移到 App 之前每次更新後都要再允許一次。

## 完整磁碟取用權限（選用）

「系統設定 › 隱私權與安全性 › 完整磁碟取用權限」也可以，但範圍大得多：
它讓 DuDuClaw——以及它啟動的每一個 AI 工具——能讀取這台 Mac 上的所有資料，
包括郵件、訊息與其他 App 的資料。建議優先使用上面的逐一資料夾開關。

已知問題：在 macOS 26.1 與 26.2 上，授予命令列執行檔（尚未移到 `DuDuClaw Gateway.app` 的閘道）的
完整磁碟取用權限不一定會生效。若授權後仍讀不到，請把閘道移到 App，或改用逐一資料夾開關。

## 企業：用 PPPC 描述檔預先核准

受管理的 Mac 可以透過 MDM 派送「隱私權偏好設定原則控制」（PPPC）描述檔，預先核准資料夾存取。
**這只適用於 `DuDuClaw Gateway.app`**（Team ID `7469HYQ6HH`、識別碼 `ai.duduclaw.gateway`）；
仍以單一命令列執行檔執行的閘道沒有可供描述檔比對的固定簽章，請先移到 App。

描述檔範例請見[英文版](../macos-folder-access.md#enterprise-pre-approve-with-a-pppc-profile)
（XML 內容與語言無關）。核心是在 `com.apple.TCC.configuration-profile-policy` 的
`Services` 中，為需要的項目（`SystemPolicyDocumentsFolder`、`SystemPolicyDesktopFolder`、
`SystemPolicyDownloadsFolder`、`SystemPolicyNetworkVolumes`、`SystemPolicyRemovableVolumes`）各加一筆：

```xml
<dict>
  <key>Identifier</key><string>ai.duduclaw.gateway</string>
  <key>IdentifierType</key><string>bundleID</string>
  <key>CodeRequirement</key>
  <string>identifier "ai.duduclaw.gateway" and anchor apple generic and certificate leaf[subject.OU] = "7469HYQ6HH"</string>
  <key>Authorization</key><string>Allow</string>
</dict>
```

只核准團隊實際會用到的資料夾。

## 在 macOS 上請用 install.sh 安裝，不要用 npm

`install.sh` 會把 `DuDuClaw Gateway.app` 裝到 `~/Applications/`，CLI 透過指向它的連結啟動，
並保留舊版更新所需的獨立執行檔。安裝前會用 minisign 核對發行檔簽章，並核對 App 的開發者簽章、識別碼、版本與版別：

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

`install.sh` 需要 [minisign](https://jedisct1.github.io/minisign/)（`brew install minisign`）；
找不到時會停止並說明安裝方式，不會略過驗證。

npm 套件會把 App 裝在平台套件內、含有 Node.js 版本號的資料夾下
（例如 `…/node/v22.x/lib/node_modules/…`），升級 Node 或使用 Node 版本管理工具都會讓 App 的位置改變。
`install.sh` 則固定在同一個位置。
