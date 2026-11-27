# Google Workspace 工具與安全模型

設定步驟見[連接 Google](google-setup.md)。原生 REST 涵蓋九項服務、三十八個工具；Apps Script 涵蓋三項服務，含狀態查詢共八個動作。整合關閉時仍可開啟儀表板，工具要等 `[integrations] google_workspace = true` 才會顯示。DuDuClaw 不提供共用 OAuth 憑證。

## 你會拿到什麼

三十八個面向 agent 的 MCP 工具，由兩個 scope 管控（`google:read` / `google:write`）：

| 工具 | 類別 | 做什麼 |
|------|-------|--------------|
| `google_status` | read | 依帳號路徑回報身分與憑證診斷，不能證明各服務可讀。整合或員工 Google 身分停用時隱藏。 |
| `gmail_search` | read | 用 Gmail 查詢語法搜尋信箱（`from:… is:unread` 等）。回傳寄件者／主旨／日期／摘要。 |
| `gmail_read` | read | 完整讀取一封信：標頭、純文字內文（過長會截斷）、附件清單（只列檔名與大小，絕不下載）。 |
| `google_chat_spaces` | read | 列出已連結帳號的聊天室、群聊與私訊，需先啟用 Chat 讀取。 |
| `google_chat_messages` | read | 讀取近期 Chat 訊息，回傳經掃描的參考資料；附件只列名稱。 |
| `google_chat_send` | send | 以提出要求的已驗證成員本人帳號，傳送文字到既有空間。須先開啟 `[google] chat_send = true`，每次傳送都要本人確認。見[以本人身分傳送 Google Chat](google-personal-connections.md#以本人身分傳送-google-chat)。 |
| `gmail_send_draft` | send | 逐次確認後寄出草稿快照。 |
| `calendar_delete_event` | delete | 逐次確認後刪除指定版本的活動，預設不通知參加者。 |
| `gmail_create_draft` | write | 建立一封 Gmail **草稿**。此工具只存草稿；寄出需另外呼叫 `gmail_send_draft` 並逐次確認。 |
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
| `docs_append` | write | 把文字附加到一份 Doc 的**結尾**。 |
| `docs_create` | write | 在「我的雲端硬碟」根目錄建立文件，可附初始內文。 |
| `docs_replace_text` | write | 取代文件中相符的文字，預設區分大小寫。 |
| `sheets_create` | write | 建立試算表，可指定工作表名稱。 |
| `sheets_update` | write | 更新最多 10,000 格，預設使用 RAW。 |
| `calendar_update_event` | write | 編輯行事曆活動，可能變更與會者的行事曆，一律走 send 核准規則。 |
| `slides_read` | read | 逐頁讀取簡報的文字內容（圖形、群組圖形、表格儲存格）。 |

這五項寫入工具及 Gmail 回覆欄位（`thread_id`、`in_reply_to`、`references`）需要原生 REST。Apps Script Bridge 會隱藏不支援的工具及回覆欄位；切換後端會透過 `notifications/tools/list_changed` 通知客戶端更新清單。Gmail 的 `to` 與 `cc` 可用逗號分隔電子郵件地址，每欄最多 1,000 字元；每個地址都會驗證，並拒絕控制字元。

寫入類工具的結果只寫 Google 實際回報的內容。`calendar_create_event` 只寫加入了幾位參與者（沒有時寫「Attendees: none」），不宣稱已通知任何人；沒有回傳活動代號時回報錯誤。`gmail_create_draft` 沒有回傳草稿代號時回報錯誤。`sheets_append` 只在 Google 回報時才顯示列數與儲存格數。透過 Apps Script 連線時，新增的列一律加在試算表的**第一個工作表**，結果會說明這點以及指定的範圍沒有被使用。Apps Script 的回覆必須符合該動作成功時的格式才算成功；空的回覆、`ok: false` 或任何錯誤欄位都會回報為工具錯誤並附上原因。

> **刻意沒有 Slides 寫入工具**：DuDuClaw 自己的辦公文件套件已經能產出真正的
> `.pptx` 檔案，這比透過 Slides 的 `batchUpdate` 元件 API 去操作要安全、
> 輸出品質也更好。

> **命名慣例**：Google Tasks 的工具全部叫 `gtasks_*`。DuDuClaw 自己的任務板
> 則維持 `tasks_*`（`tasks_list` / `tasks_create` / `tasks_complete` / …），
> 這是兩套完全獨立的系統，刻意用不同的前綴，讓 agent 永遠不會把「我自己的
> 工作佇列」跟「使用者的 Google Tasks」搞混。

**Forms 與 Tasks 目前沒有官方 MCP server**（2026-07-30 查證：`formsmcp` /
`tasksmcp` 兩個端點都回 404，Google 的 MCP 文件也沒有列出這兩項服務），
這正是這裡選擇原生實作的原因。

上表列出既有工具；九個新增雲端硬碟寫入工具的參數、動作開關與效果類別見 [Drive 寫入參考](google-mcp.md#雲端硬碟寫入工具)。新工具需要原生 REST 與 `google:write`；既有六個 Docs／Sheets 寫入工具沿用原本規則。

### 安全設計

- **寄信需要逐次確認。** `gmail_create_draft` 只存草稿；`gmail_send_draft` 每次都要人確認卡片才會寄出。
- **讀取就只是讀取。** 屬於 read 類別的工具無法修改 Gmail 或 Calendar 中的
  任何內容。
- **Forms 與 Slides 維持唯讀。** Drive 寫入由九個工具另外管控，四項動作開關預設全關，詳見[寫入工具與資料夾規則](google-mcp.md#雲端硬碟寫入工具)。
- **寫入權限分開保存。** 一般 Drive 讀取使用 `drive.readonly`；獨立寫入連線要求 `drive`、`documents` 與 `spreadsheets`，憑證留在 gateway。錄影匯入另用 `drive.file`，Slides 使用 `presentations.readonly`。
- **可選的審批閘門。** 想更謹慎的話，可以把寫入類工具列進 agent 的
  `agent.toml [capabilities] approval_required_tools`，讓每一次草稿、
  活動或試算表寫入都要等待 HITL 審批：

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append", "docs_create", "docs_replace_text", "sheets_create", "sheets_update", "calendar_update_event"]
  ```

## 建立與編輯 Google 內容

以下原生工具沿用既有的 `documents`、`spreadsheets`、`calendar.events` OAuth 範圍，需要 `google:write`；只有 `google:read` 的員工看不到，也不能呼叫。個人連線還必須具備該服務的授權範圍。網域委派使用相同工具名稱。新工具與回覆草稿需要原生 REST 連線，Apps Script Bridge 不支援這些操作。

| 原生工具 | 參數與行為 |
|---|---|
| `docs_create` | 必填 `title`，可選 `body`，在「我的雲端硬碟」根目錄建立文件。 |
| `docs_replace_text` | `document_id`、非空白的 `find`、`replace`；`match_case` 預設為 `true`。取代所有相符文字，回傳 `occurrencesChanged`。 |
| `sheets_create` | 必填 `title`，可選 `sheet_names` 陣列（最多 100 個名稱），在「我的雲端硬碟」根目錄建立試算表。 |
| `sheets_update` | `spreadsheet_id`、A1 格式的 `range`、二維 `values` 陣列，每次最多 10,000 格。`value_input_option` 預設 `RAW`；要解析公式、日期或數字時可選 `USER_ENTERED`。 |
| `calendar_update_event` | `event_id`、可選 `calendar_id`（預設 `primary`），以及至少一個更新欄位：`summary`、`description`、`location`、RFC3339 格式的 `start`／`end`、`attendees` 電子郵件陣列（`[]` 清空與會者）。每次呼叫都套用與 `calendar_create_event` 相同的 `send` 動作規則及核准路徑，因為 `sendUpdates=none` 仍可能變更與會者的行事曆。通知參數預設維持 `none`，`notify_attendees=true` 改為 `all`。 |

文字最多 20,000 字元；標題最多 1,000 字元，工作表名稱最多 100 字元，A1 範圍最多 1,000 字元。超限輸入會在動作規則、核准或摘要綁定之前拒絕，不會截斷輸入。結果的 `structuredContent` 會包含可供後續呼叫使用的 `documentId` 或 `spreadsheetId` 與 `url`，行事曆則回傳 `id` 與 `htmlLink`。Google 回傳並顯示的內容會先經過注入檢查。建立含內文的文件需要兩次請求；若插入內文失敗，重試前先到雲端硬碟確認文件是否已建立。

要在既有郵件討論串建立回覆草稿，請在 `gmail_create_draft` 原有參數之外提供 `thread_id` 與 `in_reply_to`（原信的 RFC `Message-ID`，例如 `<original@example.com>`），主旨維持原信主旨，可加回覆前綴。可選的 `references` 是包含原信 Message-ID 的 References 鏈，預設使用 `in_reply_to`。工具會設定 Gmail `threadId`、`In-Reply-To` 與 `References`；由呼叫端提供標頭，只有撰寫權限的連線也不必多申請讀取範圍。這項操作仍只存草稿，永遠不寄出。

### 從客戶自架 Google MCP 遷移

將 hcatwn-google 等自架伺服器的技能改用原生工具時，可先依下表對照。這些名稱沒有註冊成別名，遷移前還要核對各伺服器的參數格式。

| 自架工具名稱 | DuDuClaw 原生名稱 |
|---|---|
| `calendar_list` | `calendar_list_events` |
| `gmail_draft` | `gmail_create_draft` |

新增文件、試算表與行事曆編輯流程請使用上方的原生工具名稱。其他自架伺服器可能採不同命名，請以該伺服器的工具清單為準。Google 官方遠端 MCP 保留 Google 自訂的名稱，屬於另一套整合。

## 讀取個人 Google Chat 對話（選擇啟用）

`google_chat_spaces` 列出已連結帳號加入的聊天室、群聊與私訊，包含資源名稱、顯示名稱、類型及 Google 有提供的最後活動時間。`limit` 預設 20，範圍 1–50；`filter` 僅接受 `SPACE`、`GROUP_CHAT` 或 `DIRECT_MESSAGE`。

`google_chat_messages` 以嚴格驗證的 `spaces/...` 資源名稱讀取近期訊息，從新到舊排列。`limit` 預設 30，範圍 1–100；選填 `since` 必須是 RFC3339 時間，`thread` 必須是該聊天室內的討論串資源名稱。回傳發送者、建立時間、文字（每則最多 2,000 字元）、討論串及附件名稱，不下載附件。發送者名稱採 Google 回傳的顯示名稱，缺少時顯示 `users/...`；不呼叫 People API，也不另索取會員權限。整份回覆最多 30,000 字元。

1. 管理員在儀表板「管理 → 整合 → Google」分頁啟用「Google Chat 讀取」（`[google] chat_read = true`，預設 false）。
2. 使用者到「我的連線」重新連結自己的 Google 帳號，勾選兩項 Chat 讀取權限。額外 scope 為 `https://www.googleapis.com/auth/chat.spaces.readonly` 與 `https://www.googleapis.com/auth/chat.messages.readonly`；開關關閉時不會要求既有使用者追加同意。
3. 在 OAuth 用戶端所屬的 Google Cloud 專案啟用 **Google Chat API**，並完成 **Chat 應用程式設定**，即使使用個人帳號授權也需要。Workspace 管理員可能還需允許這個 OAuth 應用程式。

兩項工具沿用 Gmail 讀取的員工與帳號綁定、scope、核准及私有資料外送閘，使用既有身分政策選定的帳號，不接受任意指定使用者或帳號的參數。Apps Script 不支援這兩項讀取。這與只接收私訊和提及的 Chat 機器人通道不同。關閉開關會阻止後續讀取，即使權杖仍有效；不會撤銷 Google 端已同意的權限。

回傳內容只作為不可信參考資料，使用群組歷史的包覆格式與注入掃描；任何命中都會拒絕整次回覆，顯示 `not available`。稽核僅保留聊天室識別碼與數量，不記錄訊息文字。本機與固定資料測試不代表真實帳號可讀，交付前仍須以客戶帳號驗證同意流程及讀取結果。


## 每位 AI 員工用不同的 Google 帳號（公司授權代表，v1.76.0）

預設所有 AI 員工共用下方設定的那一個 Google 帳號（公司共用帳號）。Google Workspace 客戶可以改成讓每位員工代表一位指定的同事：業務助理在 `alice@customer.com` 的信箱裡讀信、存草稿，排程助理管理 `bob@customer.com` 的日曆。全部共用一個開了網域委派的服務帳號，每位員工各自指定代表的同事。

### 需要什麼

- 一個 Google Workspace 網域，以及一位能授權用戶端 ID 的超級管理員（Google 管理控制台 → 安全性 → 存取權與資料控管 → API 控制項 → 管理全網域委派），授權的權限清單要和儀表板顯示的完全一致。
- 憑證保護已使用支援的金鑰儲存方式（「安全」頁，見[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈v1760)）。服務帳號金鑰用這把保護金鑰封存，AI 員工讀不到它的檔案。macOS、Windows，以及用 person-credentials 疊加設定啟動的 Docker（見 [Docker 指南](docker.md#74-個人帳號person-credentials-疊加設定)）可以用；[原生 Linux 受保護系統服務](linux-protected-service.md)也可使用。一般 Docker 安裝與 Linux 使用者服務仍拒絕代表同事。

### 設定步驟

1. **安全 → 憑證保護 → 改放系統金鑰圈。**
2. **整合 → Google → 公司授權代表 → 匯入金鑰檔。** 選服務帳號的 JSON 金鑰檔。gateway 檢查後封存，存成 `<home>/protected/google-service-account.sealed`（只有擁有者能讀）。匯入後請刪掉原本的檔案。面板會顯示要交給超級管理員的用戶端 ID 和權限清單。
3. **允許代表的網域。** 只有這些網域的同事能被代表（網域完全相同才算，子網域不算）。沒填時只允許和公司共用服務帳號 `subject` 同網域；兩者都沒有就一律不能代表。
4. **員工頁 → 連線 → 變更 → 代表某位同事（網域委派）**，填同事的公司信箱後儲存。
5. **測試連線**：先逐項確認權限；Gmail 可用時確認回報的是同一個信箱，再逐一服務各取一次權杖。Gmail、日曆、雲端硬碟與工作清單會讀取唯讀端點；試算表、文件、簡報與表單先找得到可讀的範例，再實際 GET 那個服務的資源。找不到範例就標示尚未驗證，不把成功取權杖算成資料可讀。不寄任何東西，也不使用或寫入權杖快取。

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

沒寫 `[google]` 的員工用 `default_identity`，所以既有安裝的行為完全不變。`none` 會讓這位員工看不到 29 個 Google 工具，呼叫也會被拒。讀不懂的值（打錯字、不認得的欄位）會讓這位員工的 Google 工具全部拒絕，絕不會變成公司共用帳號。企業版可用 `owner` 與 `user:<id>`，但指定人必須是有效的直接 Owner，且本人明確同意；見[連結自己的 Google（v1.76.0）](google-personal-connections.md)。

### 誰能透過這位員工使用這個帳號

同事的信箱是個人的資料，所以員工只替部分發問者使用它（`requesters`）：

| 設定 | 允許 |
|---|---|
| `bound_people`（預設） | 綁定這位員工的人（任何權限）與管理員，不論是在儀表板、WebChat 或已驗證的通道綁定；排程與系統工作（cron、heartbeat、Agent Mail）；由上述的人委派出來的工作 |
| `owner_only` | 那位同事自己的 DuDuClaw 帳號，以及排程工作 |
| `anyone` | 能和這位員工對話的人（用 RPC 設定時要加 `confirm_anyone: true`） |

網站的公開訪客（WebChat 小工具、Odoo 線上客服）在任何設定下都會被拒。沒有綁定帳號的通道發話者，除非設成 `anyone`，否則也會被拒。

**Google Chat 的發話者（v1.82）**：管理員在 **使用者 → 驗證身分** 綁定了他的 Chat 使用者代號，或——僅在管理員開啟預設關閉的選用設定 `[google] chat_identity_from_account`（管理 → 整合 → Google）時——同仁在 **我的連線** 連結了自己的 Google 帳號（以 Google 帳號代號比對；這項自動認出**尚未以真實 Google Workspace 驗證**，除非您已在自己的 Workspace 確認兩種代號一致，否則請保持關閉）時，這裡就把他當成該同仁本人。不做 Email 比對。v1.82 以前所有 Chat 發話者都算沒有綁定，只有 `anyone` 會放行。詳見 [儀表板設定 → 使用者 → 驗證身分](dashboard-settings.md#使用者--驗證身分通訊軟體身分v182)。

### 用不了的時候

工具呼叫會失敗，並說明原因和誰能處理，不會悄悄改用公司共用帳號。可能的原因：呼叫者不是通過驗證的員工；發問者沒有權限；信箱不在允許的網域；那位同事的 DuDuClaw 帳號已停權或離職；憑證保護還沒就緒（或保護金鑰被換掉，封存的金鑰解不開，要重設憑證保護後重新匯入金鑰）；還沒匯入金鑰；Google 拒絕委派（`unauthorized_client` 通常是管理控制台的權限清單少了幾項，`invalid_grant` 是帳號不存在或已停用）；DuDuClaw 主程式沒有在執行。

勾選「萬一這位同事的帳號用不了，改用公司共用帳號」（`fallback = "home"`）後，最後那一類（還沒匯入金鑰、同事停權、Google 拒絕或連不上）會改用公司共用帳號，並在結果開頭說明原因。發問者沒有權限、設定錯誤、憑證保護的問題，一律不會改用公司帳號。

### 其他跟著改變的地方

- **`google_status`** 先講這位員工目前代表誰。代表同事時，回報現在能不能取得權杖，不再列公司共用帳號的細節。
- **Agent Mail** 用「收信所屬員工」的身分讀信。如果那位員工代表某位同事，信箱頁只有管理員和那位同事看得到（行為變更；用公司共用帳號的信箱仍是主管以上可看）。一個安裝仍然只有一個信箱。
- **外掛的 Google MCP 伺服器**（`bearer_token = "oauth://google"`）在 HTTP 掛載使用這位員工選定的帳號。權限不通過時只略過那個 Google 掛載，其他已設定的伺服器仍保留，詳見下方。
- **稽核**：gateway 每次發出或拒絕權杖，都在 `tool_calls.jsonl` 寫一列 `google_token`（員工、身分、帳號、發問者類型、工具、拒絕代碼）；用同事身分的每次 Google 工具呼叫寫一列 `google_call`。兩者都不含權杖。變更員工身分會在帳號稽核記錄寫 `agent.google_identity_set`，並在 `security_audit.jsonl` 寫 `agent_google_identity_set`。

### 員工專用 API key 與 Google MCP 掛載（v1.76.0）

通過驗證、綁定真實員工的專用 API key，明確帶有 `google:read` 或 `google:write` 時，可以使用 29 個原生 Google 工具中相應的工具。只有 `admin` 不算；員工設定也必須可讀。一般外部金鑰可申請的權限與其他連接器不變，OAuth 驗證不會產生這條執行路徑需要的憑證。沒有綁定員工的合法內部整合金鑰，仍保留原本的公司共用帳號路徑。

這些呼叫仍受員工的工具允許／拒絕清單、動作規則、PolicyKernel、階段性授權與人工審批限制。等審批時，已接受的員工、工具與最終參數固定不變。金鑰失效、員工被移除或授權設定改變，只會拒絕，不會改用行程的其他身分或別的帳號。Google 操作由 gateway 執行，外部 MCP 用戶端拿到有大小限制的工具結果，不會拿到 Google bearer。

API key 證明的是員工，不是真人發問者。因此使用指定同事的帳號時，它算是 **Unknown**：`bound_people`、`owner_only` 都會拒絕，即使開了公司帳號 fallback 也一樣。只有操作員明確設定 `anyone` 才允許這種用法；網站公開訪客仍不被允許。

員工的 HTTP MCP 掛載可以用 `bearer_token = "oauth://google"`，由 gateway 解析這位員工選定的帳號。每次實際呼叫都會重查發問者、帳號設定、網域、同事狀態與憑證保護，重用連線也不例外。代表同事的 stdio 掛載會略過，因為 stdio 不會使用 HTTP bearer。真的因設定的 fallback 改用公司帳號時，該次結果會另外附上一段說明；成功使用同事帳號時不會誤報成公司帳號。伺服器原本的 JSON，以及遮蔽與 CCR 處理，保留原來的結構。

Agent Mail 另記錄最近完成收信檢查的時間、該次檢查的帳號不可用與連線錯誤次數。帳號不可用或檢查紀錄讀不到，會和空信箱分開顯示；尚未完成檢查也不會顯示成成功讀到零封信。每個安裝一個信箱及原本的讀者限制不變。

專用 gateway 執行路徑對非 GET／HEAD 的 REST 請求只送一次；連線錯誤、429 或 5xx 都不會自動重送寫入。GET／HEAD 保留一次重試。這項限制適用於專用執行路徑，既有 Home 呼叫者仍維持原本共用函式的行為。

### 限制

- `gmail.compose` 讓拿到存取權杖的人可以寄信。建立草稿工具只存草稿，寄出工具另需逐次確認；有不受限 shell 的員工可以拿它替自己那位同事取得的權杖直接呼叫 Google。
- 有 shell 的員工和 gateway 是同一個作業系統帳號，見[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈v1760)中金鑰圈的限制。
- 還沒在真的 Workspace 上驗證：管理控制台的實際流程、Google 真正的錯誤字串、關閉 Gmail 的組織單位，以及 Google 自家遠端 MCP 端點收不收委派權杖。


## What was ruled out, and why

**在本機用 AppleScript 讀 Mail.app／Calendar.app。** 行事曆這條路可行，也已經出貨（`os_calendar_today`），因為查一天的行程是個很小的查詢。信件不行：拿 macOS 15 上一個真實的 54,000 封信件的信箱來測，
`Mail.inbox.messages.dateReceived()` 花了 17 秒，而
`whose({dateReceived: …})` 和 `whose({readStatus: false})` 都沒能在 60～90 秒內回應。`inbox.messages` 回傳的順序也不是照時間排的，所以用便宜的索引存取拿到的可能是任意一封舊信。Spotlight 不會替信件建索引（`kMDItemKind == 'Mail Message'` 查不到任何東西），直接讀 `~/Library/Mail` 又需要完整磁碟取用權限（Full Disk Access）加上一套沒有文件、隨版本變動的 SQLite schema。Gmail 在這條路上就改走 Apps Script。


## 公司授權與身分選項（v1.85）

網域委派每次只要求工具所屬服務的權限：Gmail、日曆、文件、試算表、雲端硬碟、簡報、表單或工作清單。權杖依服務帳號、代表信箱及排序去重後的權限集合分開快取；其他服務缺少授權，不會阻止原本已授權的服務。測試連線會逐項列出 scope 已授權或尚未確認，再另外檢查各服務。取得權杖不代表資料 API 或範例檔案一定可讀。

開啟 `[google] chat_read = true` 後，公司授權也支援 `chat.spaces.readonly` 與 `chat.messages.readonly`（完整前綴都是 `https://www.googleapis.com/auth/`）。請在 Google 管理控制台加入這兩項範圍，並啟用 Google Chat API。設定預設仍為關閉；整合精靈預先勾選 Chat 讀取，管理員按儲存後才寫入。已連結但缺少任一 Chat 權限或本地用途的成員，會在「我的連線」看到「重新連結以加入 Google Chat」。以本人名義傳送 Chat 另須開放 `chat_send`，並由本人逐次確認，見[以本人身分傳送 Google Chat](google-personal-connections.md#以本人身分傳送-google-chat)。

讓公司授權跟隨 AI 員工的負責人：

1. 將該成員綁定為 Owner；若有多位 Owner，先在「員工 → 連線」指定主要負責人。
2. 選擇「公司授權：跟隨負責人」（`identity = "delegate:owner"`），並依前述步驟設定允許網域、匯入封存的服務帳號金鑰。

   權限變更後，正在進行中的那一次呼叫仍可能以原負責人身分完成；之後的呼叫都會重新檢查。
3. 到「整合 → Google → 公司授權代表」，逐列核對 AI 員工、目前負責人、登入信箱與認證狀態，再按確認信箱。一般模式也可操作，但必須是有效管理員，並完成既有的密碼、驗證器二次驗證與工作階段 CSRF 檢查。若信箱是成員自行修改，管理員還須勾選已另外查證的確認項目。
4. 測試連線。每次呼叫都由 gateway 從目前主要負責人與登入帳號算出代表信箱，不採用工具參數。沒有認證、更換負責人（含換回原人）、變更信箱、負責人停權或資料讀取失敗，都會拒絕使用並要求管理員處理。`requesters` 預設 `owner_only`，排程沿用既有發問者規則；此身分不允許 `fallback = "home"`。受保護模式仍保留更嚴格的租戶路由與發問者限制。

認證使用 `google.delegation.attest_subject {agent_id, subject, user_id, acknowledge_self_set?, csrf_token}`。伺服器會核對畫面上的成員 ID、信箱及目前負責人，稽核記錄操作者、受認證成員與信箱。公司授權不會替本人寫入個人 OAuth 同意。

`owner` 與 `user:<id>` 現在也有管理員「測試連線」按鈕（`google.identity.test {agent_id}`）：只檢查連線紀錄、本人同意、權限與用途涵蓋的工具、OAuth refresh，以及 tokeninfo 的權限比對，不呼叫 Gmail、Drive、Calendar 或其他資料端點。測試會留下操作者與受測成員的稽核紀錄，並透過該成員已驗證的通知目的地通知本人；測試不代表本人同意，也不會自行授權 AI 使用。

Google 由另外管理的外部工具提供時，可設定：

```toml
[google]
identity = "external"
external_server = "合作夥伴 Google MCP" # 選填，最多 120 字
```

連線分頁會顯示「由外部工具提供」。原生 Google 工具和 `none` 一樣不列出、不能呼叫；這個選項不會自動設定或授權外部伺服器。名稱只作文字顯示，不接受控制字元。舊版解析器遇到未知身分或欄位會拒絕原生 Google 存取，不會退回公司共用帳號。

### 逐次確認寄信與刪除活動

`gmail_send_draft {"draft_id":"r…"}` 與 `calendar_delete_event {"event_id":"…","calendar_id":"primary","notify_attendees":false}` 需要 `google:write` 及 gateway 原生 REST 執行。Apps Script 不顯示這兩個工具，也拒絕直接呼叫。只傳遞 bearer 的舊版 Home 執行路徑同樣隱藏並拒絕；請使用員工專用 Google 連線或 gateway 中綁定帳號的連接器。

每次呼叫都要確認新卡片。`allow`、Admin 權限與持久工作流程授權無法免除；`block` 仍會拒絕。寄信卡片直接取自 Google，顯示寄件帳號、To／Cc／Bcc、主旨、內文預覽及附件檔名與大小。核准綁定帳號、員工生命週期、草稿 ID、操作與 MIME 指紋。gateway 重新檢查草稿後，寄出核准快照中的 MIME；內容或帳號改變就要重新確認。收件人政策會檢查全部 To／Cc／Bcc，支援顯示名稱加信箱，無法明確解析的格式會拒絕。

刪除活動卡片顯示標題、含時區的起訖時間、參加者、日曆、是否刪除整個重複系列及通知選項。核准綁定活動版本，刪除時以 `If-Match` 檢查；活動變更就要重新確認。`calendar_id` 預設為 `primary`，明確使用 `sendUpdates=none`；只有 `notify_attendees=true` 才用 `all`。HTTP 204 空白回應代表成功。刪除活動也會使用既有的 `calendar.calendarlist.readonly`（或更完整的日曆範圍），讀取主要日曆帳號與所選日曆的名稱、時區。

完整 MIME 只留在 gateway 記憶體，核准資料保存必要預覽與指紋。拒絕、逾期、已使用或內容不符的核准都不能變更 Google。寫入逾時或結果不明會回報 `result_unconfirmed`，請先到 Google 檢查，工具不會自動重送。確認期限為 90 秒。

傳輸行為依循 [Gmail 草稿契約](https://developers.google.com/workspace/gmail/api/guides/drafts)與 [Calendar 條件式修改](https://developers.google.com/workspace/calendar/api/guides/version-resources)。
