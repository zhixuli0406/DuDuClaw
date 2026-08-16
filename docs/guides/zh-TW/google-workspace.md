# Google Workspace 整合（八項服務全原生支援）

> **開放狀態（v1.49.0 更新）**：dashboard 的「整合 → Google」分頁預設**已開放**
> 可見。先前版本為了等原廠 OAuth App 驗證而隱藏，後來確認驗證只擋「自建
> OAuth client」這一條路，服務帳號網域委派與 Apps Script 橋接都不受影響，
> 分頁就沒有理由繼續隱藏。但分頁看得到不代表工具會生效：後端仍有獨立總
> 開關 `config.toml [integrations] google_workspace`，**預設 `false`**，
> 沒開的話憑證能設定、測試連線也能通過，但工具不會出現在 AI 員工面前，
> dashboard 上會有明顯的黃色提示。三條接法的選路徑導覽，見
> [google-workspace-integration.md](./google-workspace-integration.md)。

> **設計決策（D5，2026-08-04）**：DuDuClaw 不出貨共用的 Google OAuth 憑證。
> 使用者自備自己的 OAuth client（或改走 DWD／Apps Script 這兩條路），
> DuDuClaw 只負責儲存與刷新拿到的 token，從不內建自己的 client id／secret。
> 決策脈絡見 [google-workspace-integration.md](./google-workspace-integration.md)（D5）。

連上一個 Google 帳號，讓你的 AI 員工可以搜尋並閱讀郵件、準備草稿回覆、列出行事曆、建立活動（附 Google Meet 連結），並讀取／附加 Google 試算表的列資料、讀取 Google 表單回覆、管理 Google Tasks。DuDuClaw 直接原生對接 Google REST API，不需要安裝第三方 MCP server。存取權杖存放在 DuDuClaw 加密的 OAuth vault 中，並自動刷新。

**八項 Workspace 服務全部原生涵蓋**：Gmail、Calendar、Sheets、Drive、Docs、Slides、Forms、Tasks，全部走 GA REST API，因此本頁的內容完全不依賴 Google 的 Developer Preview 計畫，任何客戶都能使用。（Google 自己的官方 remote MCP server 涵蓋了八項中的六項，但僅限 Preview 資格，且條款禁止把 Pre-GA API 開放給自家網域以外的使用者。這條路仍保留作為進階選項：[google-mcp.md](./google-mcp.md)。）

## 你會拿到什麼

十九個面向 agent 的 MCP 工具，由兩個 scope 管控（`google:read` / `google:write`）：

| 工具 | 類別 | 做什麼 |
|------|-------|--------------|
| `google_status` | read | 連線診斷：是否已連接？授予了哪些 scope？token 是否有效？只讀取本機狀態。 |
| `gmail_search` | read | 用 Gmail 查詢語法搜尋信箱（`from:… is:unread` 等）。回傳寄件者／主旨／日期／摘要。 |
| `gmail_read` | read | 完整讀取一封信：標頭、純文字內文（過長會截斷）、附件清單（只列檔名與大小，絕不下載）。 |
| `gmail_create_draft` | write | 建立一封 Gmail **草稿**。永不寄出，寄信永遠是人工動作。 |
| `calendar_list_events` | read | 列出主行事曆的活動（預設列出未來 7 天）。 |
| `calendar_create_event` | write | 建立一個真實、對外可見的活動；可選擇附上 Google Meet 連結。 |
| `sheets_read` | read | 讀取試算表中某個儲存格範圍（接受試算表 ID 或完整的表單網址）。最多回傳 200 列已格式化的值。 |
| `sheets_append` | write | 用 `USER_ENTERED` 輸入模式對試算表附加一列（數字／日期／公式會依照手動輸入的方式解析）。 |
| `forms_get` | read | 讀取一份表單的結構：標題、說明，以及每一題的 `question_id`、題型與選項。 |
| `forms_list_responses` | read | 列出一份表單已送出的回覆（最多 50 筆）。答案以 `question_id` 為索引鍵，需搭配 `forms_get` 才能對應到題目標題。 |
| `gtasks_lists` | read | 列出該帳號的 Google Tasks 清單（id + 標題）。用 `@default` 可直接指向預設清單，不需要另外查詢。 |
| `gtasks_list` | read | 列出某清單中的任務（預設只列未完成；`show_completed=true` 會包含已完成與隱藏的項目）。 |
| `gtasks_create` | write | 在使用者的 Google Tasks 中建立一個真實任務。 |
| `gtasks_complete` | write | 把一個任務標記為完成。 |
| `drive_search` | read | 依**檔名與全文**搜尋 Drive（排除已丟到垃圾桶的檔案，最新的排在前面）。可選擇性用精確 MIME 篩選。 |
| `drive_read` | read | 以文字形式讀取一份 Drive 檔案：Docs／Slides 匯出成純文字，Sheets 匯出成 CSV（**僅第一個工作表**），純文字類檔案照原樣讀取。二進位類型只回傳中繼資料與提示，絕不回傳二進位內容。 |
| `docs_read` | read | 依文件內的順序讀取 Google Doc 的文字內容，包含表格儲存格文字。 |
| `docs_append` | write | 把文字附加到一份 Doc 的**結尾**。僅能附加，沒有任何工具會改寫或刪除既有內容。 |
| `slides_read` | read | 逐頁讀取簡報的文字內容（圖形、群組圖形、表格儲存格）。 |

> **刻意沒有 Slides 寫入工具**：DuDuClaw 自己的辦公文件套件已經能產出真正的
> `.pptx` 檔案，這比透過 Slides 的 `batchUpdate` 元件 API 去操作要安全、
> 輸出品質也更好。

> **命名慣例**：Google Tasks 的工具全部叫 `gtasks_*`。DuDuClaw 自己的任務板
> 則維持 `tasks_*`（`tasks_list` / `tasks_create` / `tasks_complete` / …）——
> 這是兩套完全獨立的系統，刻意用不同的前綴，讓 agent 永遠不會把「我自己的
> 工作佇列」跟「使用者的 Google Tasks」搞混。

**Forms 與 Tasks 目前沒有官方 MCP server**（2026-07-30 查證：`formsmcp` /
`tasksmcp` 兩個端點都回 404，Google 的 MCP 文件也沒有列出這兩項服務），
這正是這裡選擇原生實作的原因。

### 安全設計

- **草稿永不寄出。** `gmail_create_draft` 只會存草稿，沒有任何「寄出」工具，
  寄信永遠是人的決定。
- **讀取就只是讀取。** 屬於 read 類別的工具無法修改 Gmail 或 Calendar 中的
  任何內容。
- **Forms、Drive、Slides 全部唯讀。** 沒有任何工具會建立或編輯表單、寫入
  Drive、或修改簡報。只有 Gmail（草稿）、Calendar、Sheets、Docs（附加）
  和 Tasks 有寫入工具。
- **最小權限原則。** Drive 申請的是 `drive.readonly`（絕不是 `drive` 或
  `drive.file`，因為沒有任何工具會建立 Drive 檔案），Slides 申請
  `presentations.readonly`。Docs 需要完整的 `documents` scope，純粹是因為
  `docs_append` 需要寫入權限。
- **可選的審批閘門。** 想更謹慎的話，可以把寫入類工具列進 agent 的
  `agent.toml [capabilities] approval_required_tools`，讓每一次草稿、
  活動或試算表寫入都要等待 HITL 審批：

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append"]
  ```

## 選擇一條憑證路徑

同樣十九個工具，有三種授權方式可選。差別在於誰要負責設定，以及是否需要
Google 事先驗證過應用程式。

| | 個人 @gmail.com | Workspace 網域 | 誰來設定 | 工具涵蓋範圍 |
|---|---|---|---|---|
| **OAuth client**（下方） | ✅ | ✅ | 每個客戶自己建立 Google Cloud OAuth client | 全部 19 個 |
| **服務帳號 + 網域委派** | ❌ | ✅ | 網域的超級管理員授權一個 client id | 全部 19 個 |
| **Apps Script 橋接** | ✅ | ✅（除非管理員停用 Apps Script） | 終端使用者在自己帳號中部署一個 script | 僅 Gmail / Calendar / Sheets |

若同時設定超過一種，優先順序是服務帳號 → OAuth vault → Apps Script 橋接；
橋接排在最後，因為它涵蓋的工具最少。`google_status` 會回報目前實際生效的
來源。

兩條免憑證的路徑記錄在
[google-no-oauth-client.md](../google-no-oauth-client.md)。

## 前置準備：建立一個 Google OAuth client

你需要自備 Google OAuth client（DuDuClaw 從不出貨共用憑證）。一次性設定：

1. 打開 [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   頁面（先建立／選取一個專案）。
2. 為專案啟用以下八個 API（APIs & Services → Library），或用一行指令：

   ```bash
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com \
     sheets.googleapis.com drive.googleapis.com docs.googleapis.com \
     slides.googleapis.com forms.googleapis.com tasks.googleapis.com \
     --project=PROJECT_ID
   ```
3. 設定 OAuth 同意畫面（External 或 Internal）。若應用程式仍處於「Testing」
   狀態，記得把自己的 Google 帳號加入測試使用者名單。
4. 建立一個類型為 **Web application** 的 **OAuth client ID**。
5. 在 **Authorized redirect URIs** 下，準確加入：

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

   18789 是 gateway 的預設連接埠。如果你在別的連接埠上執行（`DUDUCLAW_PORT`），
   請改註冊那個連接埠，dashboard 的設定步驟會顯示精確的 URI，並依 gateway
   實際監聽的連接埠推導出來。這裡的不匹配是無聲的：Google 會把瀏覽器導向一個
   什麼都沒有的連接埠，token 永遠不會抵達，頁面會一直停在「尚未連接」。

6. 複製產生出來的 **Client ID** 和 **Client secret**。

申請的 scope 是：

```
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

> **Scope 變更（v1.45）**：為了 Sheets 工具，新增了 `spreadsheets` 這個
> scope。v1.45 之前連接的 Google 帳號，因為 token 是這個 scope 存在之前
> 核發的，呼叫 Sheets API 會拿到 `403`，`google_status` 會標出缺少的
> scope，從 dashboard 重新連接會用完整的 scope 集合重新走一次同意畫面。
>
> **Scope 變更（v1.47）**：為了新增的原生工具，加入了 Drive
> （`drive.readonly`）、Docs（`documents`）、Slides
> （`presentations.readonly`）、Forms（`forms.body.readonly`、
> `forms.responses.readonly`）與 Tasks（`tasks`）。規則跟上面一樣：
> 舊的 token 會收到 `403` 並附上重新授權的指引；到「整合 → Google」
> 重新連接即可重新同意。

## 從 dashboard 連接

1. 前往 **整合 → Google**（`/manage/integrations?tab=google`）。
2. 貼上 Client ID 和 Client secret，然後點擊 **連接 Google**。
3. 會跳出一個 Google 同意視窗。核准存取權限後，視窗會顯示成功，dashboard
   會切換成 **Google 已連接**。

client 憑證會被持久化保存（secret 加密儲存），這樣才能自動刷新存取權杖，
之後要重新授權時也不需要再輸入一次 secret。

要斷開連接，在已連接的畫面點擊 **斷開連接**。你儲存的 client 憑證會保留，
方便一鍵重新連接，只有存取權杖會被移除。

## 刷新機制怎麼運作

Google 只有在授權請求要求 offline access 時才會核發 refresh token，所以連接
流程會自動幫 Google 附上 `access_type=offline&prompt=consent`。當存取權杖過期
時，`get_valid_google_token` 會用你儲存的 client 憑證跑一次 refresh grant，
存下新的 token 後繼續。如果無法刷新（沒有 refresh token，或找不到儲存的
憑證），這些工具會回傳明確的訊息，引導你回到「整合 → Google」頁面重新連接。

## Scope 變更後重新授權

在這項整合上線之前（舊 scope 集合）授權過的 token，呼叫新的寫入 API 會拿到
`403`。工具會偵測到這個狀況，並回傳需要授予哪些 scope 的指引。到「整合 →
Google」重新連接，即可用目前的 scope 集合重新同意。

## 疑難排解

- **「Google 尚未連接。」**：沒有儲存任何 token，請從 dashboard 連接。
- **`401 Unauthorized`**：授權已被撤銷或已失效，請重新連接。
- **`403` 並附上 scope 清單**：token 缺少必要的 scope，請重新連接以重新同意。
- **同意過程中出現 Redirect URI 不符**：你 Google OAuth client 中的 redirect
  URI 必須精確等於 `http://localhost:18789/api/mcp/oauth/callback`。

隨時執行 `google_status` 即可取得即時診斷結果。
