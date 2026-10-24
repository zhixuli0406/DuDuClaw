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
[google-no-oauth-client.md](../google-no-oauth-client.md)。要讓每位 AI 員工各自代表一位同事，見[每位 AI 員工用不同的 Google 帳號](#每位-ai-員工用不同的-google-帳號公司授權代表未發布)。

## 每位 AI 員工用不同的 Google 帳號（公司授權代表，未發布）

預設所有 AI 員工共用下方設定的那一個 Google 帳號（公司共用帳號）。Google Workspace 客戶可以改成讓每位員工代表一位指定的同事：業務助理在 `alice@customer.com` 的信箱裡讀信、存草稿，排程助理管理 `bob@customer.com` 的日曆。全部共用一個開了網域委派的服務帳號，每位員工各自指定代表的同事。

### 需要什麼

- 一個 Google Workspace 網域，以及一位能授權用戶端 ID 的超級管理員（Google 管理控制台 → 安全性 → 存取權與資料控管 → API 控制項 → 管理全網域委派），授權的權限清單要和儀表板顯示的完全一致。
- 憑證保護已改放系統金鑰圈（「安全」頁，見[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈未發布)）。服務帳號金鑰用這把保護金鑰封存，AI 員工讀不到它的檔案。macOS 與 Windows 可以用；Linux 與 Docker 目前沒有支援的金鑰圈，代表同事一律拒絕。

### 設定步驟

1. **安全 → 憑證保護 → 改放系統金鑰圈。**
2. **整合 → Google → 公司授權代表 → 匯入金鑰檔。** 選服務帳號的 JSON 金鑰檔。gateway 檢查後封存，存成 `<home>/protected/google-service-account.sealed`（只有擁有者能讀）。匯入後請刪掉原本的檔案。面板會顯示要交給超級管理員的用戶端 ID 和權限清單。
3. **允許代表的網域。** 只有這些網域的同事能被代表（網域完全相同才算，子網域不算）。沒填時只允許和公司共用服務帳號 `subject` 同網域；兩者都沒有就一律不能代表。
4. **員工頁 → 連線 → 變更 → 代表某位同事（網域委派）**，填同事的公司信箱後儲存。
5. **測試連線**：用完整權限清單替這位同事取一次權杖，確認 Gmail 回報的是同一個信箱，再逐一服務各取一次權杖。Gmail、日曆、雲端硬碟與工作清單會讀取唯讀端點；試算表、文件、簡報與表單先找得到可讀的範例，再實際 GET 那個服務的資源。找不到範例就標示尚未驗證，不把成功取權杖算成資料可讀。不寄任何東西，也不使用或寫入權杖快取。

帳號卡片的「已設定，請測試連線」只表示本地設定已備妥，不能證明封存金鑰可解開或 Google 已接受授權。每次原生委派工具呼叫都會請 gateway 重查當下設定、發問者權限及帳號狀態；只有 gateway 快取簽發結果。

同樣的操作都有 RPC（皆需管理員）：`google.delegation.get`、`google.delegation.import_key`、`google.delegation.remove_key`、`google.delegation.settings_set`、`agents.google_identity_set`、`google.identity.test`。`agents.google_identity_get` 需要對這位員工有檢視權限。

### 設定值

```toml
# agent.toml（管理員設定；員工用 Write/Edit 改不了）
[google]
identity = "delegate:alice@customer.com"  # home | none | delegate:<email>
fallback = "none"           # none（預設）| home
requesters = "bound_people" # bound_people（預設）| owner_only | anyone

# config.toml
[integrations.google_identity]
default_identity = "home"   # 沒寫 [google] 的員工用什麼：home（預設）| none
allowed_subject_domains = ["customer.com"]
```

沒寫 `[google]` 的員工用 `default_identity`，所以既有安裝的行為完全不變。`none` 會讓這位員工看不到 19 個 Google 工具，呼叫也會被拒。讀不懂的值（打錯字、不認得的欄位）會讓這位員工的 Google 工具全部拒絕，絕不會變成公司共用帳號。`owner` 與 `user:<id>`（員工用負責人自己連結的帳號）保留給之後的版本，目前一律拒絕。

### 誰能透過這位員工使用這個帳號

同事的信箱是個人的資料，所以員工只替部分發問者使用它（`requesters`）：

| 設定 | 允許 |
|---|---|
| `bound_people`（預設） | 綁定這位員工的人（任何權限）與管理員，不論是在儀表板、WebChat 或已驗證的通道綁定；排程與系統工作（cron、heartbeat、Agent Mail）；由上述的人委派出來的工作 |
| `owner_only` | 那位同事自己的 DuDuClaw 帳號，以及排程工作 |
| `anyone` | 能和這位員工對話的人（用 RPC 設定時要加 `confirm_anyone: true`） |

網站的公開訪客（WebChat 小工具、Odoo 線上客服）在任何設定下都會被拒。沒有綁定帳號的通道發話者，除非設成 `anyone`，否則也會被拒。

### 用不了的時候

工具呼叫會失敗，並說明原因和誰能處理，不會悄悄改用公司共用帳號。可能的原因：呼叫者不是通過驗證的員工；發問者沒有權限；信箱不在允許的網域；那位同事的 DuDuClaw 帳號已停權或離職；憑證保護還沒就緒（或保護金鑰被換掉，封存的金鑰解不開，要重設憑證保護後重新匯入金鑰）；還沒匯入金鑰；Google 拒絕委派（`unauthorized_client` 通常是管理控制台的權限清單少了幾項，`invalid_grant` 是帳號不存在或已停用）；DuDuClaw 主程式沒有在執行。

勾選「萬一這位同事的帳號用不了，改用公司共用帳號」（`fallback = "home"`）後，最後那一類（還沒匯入金鑰、同事停權、Google 拒絕或連不上）會改用公司共用帳號，並在結果開頭說明原因。發問者沒有權限、設定錯誤、憑證保護的問題，一律不會改用公司帳號。

### 其他跟著改變的地方

- **`google_status`** 先講這位員工目前代表誰。代表同事時，回報現在能不能取得權杖，不再列公司共用帳號的細節。
- **Agent Mail** 用「收信所屬員工」的身分讀信。如果那位員工代表某位同事，信箱頁只有管理員和那位同事看得到（行為變更；用公司共用帳號的信箱仍是主管以上可看）。一個安裝仍然只有一個信箱。
- **外掛的 Google MCP 伺服器**（`bearer_token = "oauth://google"`）在 HTTP 掛載使用這位員工選定的帳號。權限不通過時只略過那個 Google 掛載，其他已設定的伺服器仍保留，詳見下方。
- **稽核**：gateway 每次發出或拒絕權杖，都在 `tool_calls.jsonl` 寫一列 `google_token`（員工、身分、帳號、發問者類型、工具、拒絕代碼）；用同事身分的每次 Google 工具呼叫寫一列 `google_call`。兩者都不含權杖。變更員工身分會在帳號稽核記錄寫 `agent.google_identity_set`，並在 `security_audit.jsonl` 寫 `agent_google_identity_set`。

### 員工專用 API key 與 Google MCP 掛載（未發布）

通過驗證、綁定真實員工的專用 API key，明確帶有 `google:read` 或 `google:write` 時，可以使用 19 個原生 Google 工具中相應的工具。只有 `admin` 不算；員工設定也必須可讀。一般外部金鑰可申請的權限與其他連接器不變，OAuth 驗證不會產生這條執行路徑需要的憑證。沒有綁定員工的合法內部整合金鑰，仍保留原本的公司共用帳號路徑。

這些呼叫仍受員工的工具允許／拒絕清單、動作規則、PolicyKernel、階段性授權與人工審批限制。等審批時，已接受的員工、工具與最終參數固定不變。金鑰失效、員工被移除或授權設定改變，只會拒絕，不會改用行程的其他身分或別的帳號。Google 操作由 gateway 執行，外部 MCP 用戶端拿到有大小限制的工具結果，不會拿到 Google bearer。

API key 證明的是員工，不是真人發問者。因此使用指定同事的帳號時，它算是 **Unknown**：`bound_people`、`owner_only` 都會拒絕，即使開了公司帳號 fallback 也一樣。只有操作員明確設定 `anyone` 才允許這種用法；網站公開訪客仍不被允許。

員工的 HTTP MCP 掛載可以用 `bearer_token = "oauth://google"`，由 gateway 解析這位員工選定的帳號。每次實際呼叫都會重查發問者、帳號設定、網域、同事狀態與憑證保護，重用連線也不例外。代表同事的 stdio 掛載會略過，因為 stdio 不會使用 HTTP bearer。真的因設定的 fallback 改用公司帳號時，該次結果會另外附上一段說明；成功使用同事帳號時不會誤報成公司帳號。伺服器原本的 JSON，以及遮蔽與 CCR 處理，保留原來的結構。

Agent Mail 另記錄最近完成收信檢查的時間、該次檢查的帳號不可用與連線錯誤次數。帳號不可用或檢查紀錄讀不到，會和空信箱分開顯示；尚未完成檢查也不會顯示成成功讀到零封信。每個安裝一個信箱及原本的讀者限制不變。

專用 gateway 執行路徑對非 GET／HEAD 的 REST 請求只送一次；連線錯誤、429 或 5xx 都不會自動重送寫入。GET／HEAD 保留一次重試。這項限制適用於專用執行路徑，既有 Home 呼叫者仍維持原本共用函式的行為。

### 限制

- `gmail.compose` 讓拿到存取權杖的人可以寄信。工具只會存草稿，但有不受限 shell 的員工可以拿它替自己那位同事取得的權杖直接呼叫 Google。
- 有 shell 的員工和 gateway 是同一個作業系統帳號，見[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈未發布)中金鑰圈的限制。
- 還沒在真的 Workspace 上驗證：管理控制台的實際流程、Google 真正的錯誤字串、關閉 Gmail 的組織單位，以及 Google 自家遠端 MCP 端點收不收委派權杖。

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
