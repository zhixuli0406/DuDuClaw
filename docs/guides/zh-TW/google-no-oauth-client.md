# Google Workspace without creating an OAuth client

預設路徑（[google-workspace.md](../google-workspace.md)）需要每個客戶自己建立一個 Google Cloud OAuth client。這裡有兩個替代方案可以省掉這一步，依帳號類型挑選：Workspace 網域、有 IT 管理員的用 委派（delegation）；個人 `@gmail.com` 帳號用 Apps Script 橋接。

還有一條路已經走不通了：**透過 IMAP/SMTP 使用應用程式專用密碼（app password）**。Google 在 2025 年 3 月停用了所有 Google 帳號的基本驗證（Workspace 則在 2025 年 5 月完成），所以 IMAP、POP、SMTP、CalDAV、CardDAV 現在全都需要 OAuth。任何還在教你產生 app password 的教學都已經過時了。

---

## Option A — service account with domain-wide delegation

**需要一個 Google Workspace 網域。** 個人 `@gmail.com` 帳號不屬於任何網域，無法被冒充使用，這類帳號請改用 Option B。

服務帳號是由執行 DuDuClaw 的那一方所擁有。客戶的超級管理員只要授權一次它的 client id，之後 DuDuClaw 就能為該網域內的使用者核發權杖，完全不需要跳出同意畫面，更重要的是**完全不需要 Google 的應用程式驗證或 CASA 審查**，而這正是 OAuth 路徑無法服務開發者自身網域以外客戶的卡點。

### 該怎麼跟客戶說明

要跟客戶說清楚：委派（delegation）代表被授權的 client 可以在核准的 scope 範圍內冒充該網域裡的任何使用者。Google 自己的[最佳實務指引](https://support.google.com/a/answer/14437356)也提醒管理員要謹慎授予第三方委派權限，而啟用了多方核准（自 2024 年 8 月起可用）的組織還需要第二位超級管理員一起簽署同意。要有心理準備會被問很多問題，也會有管理員直接拒絕。

### Setup

1. 在你的 Google Cloud 專案裡建立一個服務帳號並下載其 JSON 金鑰，記下服務帳號詳細資訊頁面上顯示的數字**client id**。
2. 把金鑰檔存放在 DuDuClaw 主機上並鎖好權限：

   ```bash
   mkdir -p ~/.duduclaw/keys && mv ~/Downloads/sa-key.json ~/.duduclaw/keys/google-sa.json
   chmod 600 ~/.duduclaw/keys/google-sa.json
   ```

3. 把 client id 與下方的 scope 清單送給客戶的超級管理員。他們要到 **Admin console → Security → Access and data control → API
   controls → Manage Domain Wide Delegation → Add new**，貼上 client id 與 scope 清單並儲存。生效通常很快，但 Google 保留最多 24 小時的緩衝。
4. 設定 DuDuClaw，可以從 dashboard 操作，也可以手動編輯設定檔。

   **Dashboard**（不用重啟）：管理 → 整合／工具連線 → Google → 憑證方式 →
   **服務帳號委派**。填入金鑰檔路徑與要冒充的使用者，按下儲存，再按測試連線；它會實際核發一個權杖，所以看到綠色結果就代表管理員的授權真的已經生效了。scope 清單旁邊附有一鍵複製按鈕。

   **手動設定：**

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_service_account]
   key_file = "keys/google-sa.json"   # relative paths resolve against ~/.duduclaw
   subject  = "boss@customer.com"      # the Workspace user to act as
   ```

   手動改設定檔需要重啟 gateway；dashboard 寫入的是同一個區塊，下一次工具呼叫就會生效。

5. 執行 `google_status` 工具，它會回報 `Credential source: direct API
   token`。

要交給管理員的 scope 清單，貼成一行、以逗號分隔：

```
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

### 出錯的時候

`unauthorized_client` 幾乎都是因為 Admin console 裡的 scope 清單跟 DuDuClaw 實際要求的不完全一致：Google 是比對整組清單，只要少一項就會核發失敗。錯誤訊息裡會附上 client id，可以拿去跟管理員填的那組核對。

一個設定過但設定錯誤的服務帳號會直接回報成錯誤，而不會悄悄退回去用 OAuth vault 裡現成的權杖：既然你指定要用這組憑證，設定錯誤就必須明顯可見，不能被任何剛好存在的其他權杖蓋過去。

---

## Option B — Apps Script bridge

**個人 `@gmail.com` 與 Workspace 帳號都適用。** 使用者在自己的帳號裡部署一個腳本，DuDuClaw 呼叫它的網址。Google 從頭到尾只看到使用者在跑自己的腳本，所以不存在需要驗證的第三方應用程式。

涵蓋範圍是個子集：**Gmail（搜尋／讀取／草稿）、行事曆（列表／建立）、Sheets（讀取／附加）**。Drive、Docs、Slides、Forms 與 Tasks 在這條路徑上不可用，呼叫時會明確回報「not available through the Apps Script bridge」的錯誤，不會回傳空結果。

### Setup

1. 打開 <https://script.google.com> 並建立一個新專案。
2. 把檔案內容換成
   [`templates/apps-script/duduclaw-bridge.gs`](../../../templates/apps-script/duduclaw-bridge.gs)。
3. 產生一組密鑰，貼上去覆蓋掉 `CHANGE_ME_TO_A_LONG_RANDOM_STRING`：

   ```bash
   openssl rand -base64 32
   ```

4. **Deploy → New deployment → Web app**，設定為：
   - Execute as：**Me**
   - Who has access：**Anyone**
5. Google 會跳出「unverified app」的同意畫面，這是預期中的行為，這個未驗證的應用程式就是使用者自己的腳本。選擇 **Advanced → Go to (project
   name)** 並核准。
6. 複製 `/exec` 網址（不是 `/dev`，那個網址只授權給腳本擁有者自己的瀏覽器 session）。
7. 設定 DuDuClaw，可以從 dashboard 操作，也可以手動編輯設定檔。

   **Dashboard**（建議走這條，它會幫你把密鑰加密）：管理 →
   整合／工具連線 → Google → 憑證方式 → **Apps Script 橋接**。貼上 `/exec`
   網址與密鑰，按下儲存，再按測試連線，看到綠色結果會直接顯示這個腳本是用哪個 Google
   帳號執行的，這是抓出「部署在錯的帳號底下」最快的方法。之後只改網址而不重新輸入密鑰，原本存的密鑰會保留不變。

   **手動設定**（密鑰會以明文存在設定檔裡，建議優先用 dashboard）：

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_apps_script]
   url    = "https://script.google.com/macros/s/AKfyc.../exec"
   secret = "the string you generated in step 3"
   ```

8. 執行 `google_status`，它會回報
   `Credential source: apps-script bridge at script.google.com`。

### Security properties

- **網址加密鑰兩者合起來才是一組憑證。**「Who has access: Anyone」代表這個端點不需要 Google 登入就能存取，密鑰是唯一擋住陌生人的東西，要把這一組當密碼對待，絕對不要貼到聊天視窗、issue 或截圖裡。
- 密鑰會以加密形式靜態儲存，跟 channel bot token 的做法一樣。
- DuDuClaw 只會 POST 到 `script.google.com`（並跟隨轉址到
  `script.googleusercontent.com`），走 https，且路徑必須以 `/exec` 結尾。輸錯或被竄改的 `url` 會在密鑰送出之前就被拒絕，包括
  `script.google.com.evil.test` 這類長得很像的網域。
- 要輪替就在腳本裡換掉 `SECRET`、重新部署，並更新
  `config.toml`。舊密鑰會立刻失效。
- 這個橋接沒有寄信動作，跟原生工具的行為一致：agent 只能準備草稿，要真的送出必須由人按下傳送。

### Quotas

Apps Script 有每日每帳號的用量上限，消費者帳號限制最緊。這條路適合互動式助理使用情境，不適合大量同步。

---

## What was ruled out, and why

**在本機用 AppleScript 讀 Mail.app／Calendar.app。** 行事曆這條路可行，也已經出貨（`os_calendar_today`），因為查一天的行程是個很小的查詢。信件不行：拿 macOS 15 上一個真實的 54,000 封信件的信箱來測，
`Mail.inbox.messages.dateReceived()` 花了 17 秒，而
`whose({dateReceived: …})` 和 `whose({readStatus: false})` 都沒能在 60～90 秒內回應。`inbox.messages` 回傳的順序也不是照時間排的，所以用便宜的索引存取拿到的可能是任意一封舊信。Spotlight 不會替信件建索引（`kMDItemKind == 'Mail Message'` 查不到任何東西），直接讀 `~/Library/Mail` 又需要完整磁碟取用權限（Full Disk Access）加上一套沒有文件、隨版本變動的 SQLite schema。Gmail 在這條路上就改走 Option B。
