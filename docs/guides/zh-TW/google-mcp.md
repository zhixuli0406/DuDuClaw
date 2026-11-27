# Google 官方 remote MCP 掛載（進階選項，非出貨路徑）

> **先看這裡**：DuDuClaw 對 Google Workspace 的**支援路徑是原生工具**：
> Gmail／Calendar／Sheets／Drive／Docs／Slides／Forms／Tasks／Chat 九個服務
> 全部走 GA REST API，38 個原生 MCP 工具，見
> [google-workspace.md](../google-workspace.md)。**一般情況不需要讀本頁。**
>
> 本頁記錄的官方 remote MCP 是**進階選項**，有兩個硬限制：
> ① 仍是 Developer Preview，需申請資格；② Program Terms 規定 GA 之前
> **不得讓自家網域／公司以外的終端使用者**透過你的應用使用 Pre-GA API，
> 也就是**不能出貨給客戶**（除非每個客戶各自申請自己的資格與 GCP 專案）。
> 因此 2026-07-30 拍板：產品面完全走原生工具，本掛載只保留給自用與願意
> 自行申請的進階使用者。

## 2026-11-11 起的用戶端要求

2026-11-11 起，Google Workspace MCP 伺服器會拒絕未標記為 AI agent 的用戶端。在 Google Auth platform → Clients 為這個進階用途另建用戶端，勾選「Use this client for an AI-powered agent」。只用 REST 的應用程式不受影響，DuDuClaw 原生 Google 工具使用 REST。要求與主控台路徑依 2026-10-30 的查證資料整理，這裡沒有重新驗證主控台畫面。

## 八個服務的覆蓋狀況

| 服務 | 原生工具（**出貨路徑**，無資格限制） | 官方 MCP（preview，僅自用） |
|---|---|---|
| Gmail | ✅ 4 | ✅ `preset = "google:gmail"`（13 工具） |
| Calendar | ✅ 4 | ✅ `google:calendar`（9） |
| Sheets | ✅ 6 | ✅ `google:sheets`（7） |
| Drive | ✅ 7 | ✅ `google:drive`（8） |
| Docs | ✅ 6 | ✅ `google:docs`（2） |
| Slides | ✅ 1（唯讀） | ✅ `google:slides`（2） |
| Forms | ✅ 2 | ❌ 官方無 |
| Tasks | ✅ 4 | ❌ 官方無 |

原生工具已支援建立與編輯。遠端工具保留 Google 的參數格式與使用資格，不會套用原生寫入開關或資料夾檢查。需要受控的 Drive 寫入時，請使用下方原生工具。

Forms 與 Tasks 的「官方無」是查證過的結果：`formsmcp.googleapis.com` /
`tasksmcp.googleapis.com` 實測回 404，Google 的 MCP 文件也完全沒有這兩個服務
（連 coming soon 都沒有）。另外官方還有 Chat（`preset = "google:chat"`）
與 People（端點命名不同，未納入 preset）；People 目前沒有原生工具對應，Chat 已有原生讀取與本人確認傳送工具。

### 各服務工具清單（2026-07-30 的 `tools/list` 歷史紀錄，未重新實測）

- **Gmail**：`search_threads` `get_thread` `get_message` `create_draft`
  `list_drafts` `list_labels` `create_label` `label_message` `unlabel_message`
  `label_thread` `unlabel_thread` `apply_sensitive_message_label`
  `apply_sensitive_thread_label`。**沒有寄信工具**，最多到草稿。
- **Calendar**：`list_calendars` `list_events` `get_event` `search_events`
  `suggest_time` `create_event` `update_event` `delete_event`
  `respond_to_event`
- **Drive**：`search_files` `list_recent_files` `get_file_metadata`
  `get_file_permissions` `read_file_content` `download_file_content`
  `create_file` `copy_file`
- **Docs**：`read_doc` `update_doc`
- **Sheets**：`get_spreadsheet` `get_values` `update_values` `update_formulas`
  `update_spreadsheet` `insert_dimension` `copy_sheet_to_another_spreadsheet`
- **Slides**：`read_presentation` `update_presentation`

## 前置設定（一次性）

1. **加入 Developer Preview Program**：<https://developers.google.com/workspace/preview>
   （免費、數日核准；申請需要 Workspace 帳號）。這批 server **仍是 preview
   非 GA**，條款限制 Pre-GA API 僅供自家網域／公司內部使用。
2. **GCP 專案啟用 API**：每個服務要「標準 API + MCP API」兩層：

   ```bash
   gcloud services enable \
     gmail.googleapis.com gmailmcp.googleapis.com \
     calendar-json.googleapis.com calendarmcp.googleapis.com \
     drive.googleapis.com drivemcp.googleapis.com \
     docs.googleapis.com docsmcp.googleapis.com \
     sheets.googleapis.com sheetsmcp.googleapis.com \
     slides.googleapis.com slidesmcp.googleapis.com \
     --project=PROJECT_ID
   ```

3. **連接 Google 帳號**：dashboard「整合 → Google」。scopes 已涵蓋 Drive／
   Docs／Sheets／Slides（見 [google-workspace.md](../google-workspace.md)）；
   v1.47 之前連好的帳號需要重新連接一次才會拿到新 scope。

## 認證怎麼運作

`preset` 會把 bearer 設成 `oauth://google`：掛載時取當前有效 access token，
過期自動用 refresh token 換新。Google 帳號沒連接時**整台 server 被跳過**
（fail-safe：agent 少了這些工具，但回覆不會失敗）。

想繞過 dashboard 整合、自備 token 也可以：

```toml
[[mcp.external]]
preset = "google:sheets"
bearer_token = "env://MY_GOOGLE_TOKEN"   # 覆蓋 preset 的預設 bearer
```

技術細節：這些是 stateless Streamable HTTP server，DuDuClaw 的 MCP client 原生
支援。**不需要** `npx mcp-remote` 之類的 stdio proxy。Google 沒有提供官方
bridge，且其 OAuth 不支援 Dynamic Client Registration，社群 proxy 的預設流程
會失敗。

## 建議收斂工具面

官方 server 一次給的工具不少（Drive 8 個、Gmail 13 個），建議用
`allowed_tools` 只開需要的，並把寫入型工具送進 HITL 審批：

```toml
[[mcp.external]]
preset = "google:calendar"
allowed_tools = ["list_events", "suggest_time", "create_event"]

# agent.toml
[capabilities]
approval_required_tools = ["create_event", "update_doc", "create_file"]
```

## 已知限制

- Preview 階段、rate limit 未公開文件化（GCP Console quota 頁可查）。
- 一般 OAuth 需要瀏覽器同意；遠端或無桌面主機可依[連接 Google](google-setup.md#遠端主機或-docker)建立 SSH 轉送。原生服務帳號委派可用，各官方遠端 MCP 是否接受委派權杖仍須分別驗證。
- Gmail 無寄信工具；官方 reference 頁的工具數與實際端點有出入（reference 列
  10 個、端點實際 13 個），以 `tools/list` 實測為準。
- 端點若在 GA 時改名，只需改 `mcp_external.rs` 的 `GOOGLE_MCP_PRESETS`
  一處（使用者的 `agent.toml` 不必動）。

參考來源：
[configure-mcp-servers](https://developers.google.com/workspace/guides/configure-mcp-servers)、
[Gmail MCP reference](https://developers.google.com/workspace/gmail/api/reference/mcp)、
[Calendar MCP reference](https://developers.google.com/workspace/calendar/api/v3/reference/mcp)、
[Developer Preview Program](https://developers.google.com/workspace/preview)。

`google:chat` 預設集不會增加 Chat 授權。原生 Chat 讀取與傳送範圍須另外開放，掛載仍需要適當授權。Google Chat 頻道使用 `chat.bot`，是另一條連線。

## 聊天授權連結

缺少本人連線或用途時，可由閘道提供確認連結。連結要求同一位成員正常登入 Dashboard，並明確確認資料說明；它不是登入憑證或 SSO。MCP 工具不能接收 ticket，也不能指定成員、帳號、授權範圍或返回網址。Google 授權不代表核准對外傳送。帳號檢查、公開網址與續行規則見[Google 個人連線指南](google-personal-connections.md#從聊天確認-google-授權)。官方 remote MCP 的使用資格與既有能力閘仍須符合。

## 以本人身分傳送 Google Chat

`google_chat_send {space, text, thread?}` 使用提出要求的已驗證成員本人的 Google 帳號，傳送文字到既有空間。需要 `google:write`；管理員須先啟用 `[google] chat_send = true`，預設關閉。不接受指定寄件者，也不會改用公司共用帳號、其他成員、Apps Script 或 Chat app 的 `chat.bot` 權杖。

個人 OAuth 請先連結帳號，再到「我的連線 → Google Chat 傳送」開放獨立的 `chat_send` 用途。新增服務同意會明列已同意的一般服務，再加上 `https://www.googleapis.com/auth/chat.messages.create`，不要求 Google 自動合併先前授權；必須維持相同 Google `sub` 且實際取得權限。公司全網域委派也使用獨立的傳送範圍，仍須 Workspace 管理員授權；不會把傳送範圍加入每項 Google 工具。

每次都由本人確認，五分鐘內有效、只能使用一次。確認卡顯示帳號、空間資源名稱、討論串、完整最終文字及私人資料外傳提醒。短文可在已驗證私訊確認；群組要求與長文改到本人限定的「我的連線」頁面確認。管理員核准、`allow` 規則、Admin MCP scope、收件人白名單都不能代替本人確認；組織要求的外傳核准仍另外執行。文字、目的地、帳號、憑證或政策變更後，須重新確認。

Gateway 保存單次操作的固定 request ID。結果不明會回報 `result_unconfirmed`，不會自動重送；請先檢查 Google Chat，再決定是否提出新的傳送要求。第一版不支援附件或建立空間；指定的討論串必須屬於同一空間。Google 顯示本人為寄件者，同時附上應用程式名稱。[Google Chat API 說明](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create)。

## 雲端硬碟寫入工具

這九個原生工具需要 `google:write`。管理員分別決定是否開放建立、修改、搬移與移到垃圾桶，四項預設全關。開啟動作或放寬規則前，須明確確認了解風險，系統會記錄管理員與時間；這項設定變更不要求額外重新登入。儀表板控制項會隨本版設定頁交付，目前 Google 整合與個人寫入連線區塊尚未提供。

每項動作預設由成員本人確認。管理員可逐項關閉確認，也可要求本人確認後再由管理員核准。本人確認有效五分鐘，只有該成員可以決定；工具允許清單、Admin 權杖與工作流程常駐授權都不能代替。需要確認時，排程與背景工作會被拒絕。使用本人連線或委派給主要擁有者時，請求者必須是帳號持有人；公司共用帳號與固定委派則由已驗證的請求成員確認。

| 工具 | 參數與結果 | 動作開關 | 效果類別 |
|---|---|---|---|
| `drive_create_file` | 必填 `name`，選填 `parent_folder_id`。`text`（最多 100,000 字元）、`attachment`（這次對話的附件檔名）、`folder=true` 三選一。選填 `convert_to`：`document`／`spreadsheet`；`mime_type`：`text/plain`／`text/csv`／`text/markdown`。建立檔案或資料夾。 | 建立 | Publish |
| `drive_copy_file` | 必填 `file_id`、`parent_folder_id`，選填 `name`。將可讀取的來源檔案複製到目的資料夾。 | 建立 | Publish |
| `drive_update_file` | 必填 `file_id`；`name`、`description`、`text` 至少一項。改名、修改說明或取代文字內容（最多 100,000 字元，只限非 Google 格式文字檔）。 | 修改 | Modify |
| `drive_move_file` | 必填 `file_id`、`destination_folder_id`。在同一雲端硬碟內搬移，原有上層位置由 Google 讀取。 | 搬移 | Publish |
| `drive_trash_file` | 必填 `file_id`。只移到垃圾桶。 | 移到垃圾桶 | Delete |
| `docs_insert_text` | 必填 `document_id`、`text`（1～20,000 字元）。位置三選一：`at="start"`、`at="end"`、`after_text`（全文唯一的定位文字，1～1,000 字元）；選填 `tab_id`。 | 修改 | Modify |
| `docs_format` | 必填 `document_id`、`operations`（1～50 項）。支援段落與文字樣式、表格儲存格背景、頁首與新增文件分頁，細節見下方。 | 修改 | Modify |
| `sheets_add_sheet` | 必填 `spreadsheet_id`、`title`，選填 `copy_from_sheet`（既有分頁名稱）。在同一試算表新增空白分頁或複製分頁。 | 修改 | Modify |
| `sheets_add_note` | 必填 `spreadsheet_id`、`range`（A1，最多 1,000 格）、`note`（最多 5,000 字元，空字串會清除）。設定儲存格註解，不建立討論串留言。 | 修改 | Modify |

ID 也可使用支援的 Google 檔案或資料夾網址。建立時省略 `parent_folder_id` 代表「我的雲端硬碟」根目錄，只能在資料夾清單為空時使用。上傳只接受這輪對話附上的檔案，最多 5 MB（5 MiB），不接受任意本機路徑或員工工作資料夾中的檔案。可選擇轉成 Google 文件或試算表。不提供永久刪除、清空垃圾桶、分享或變更權限。

`docs_format` 每項操作使用 `type`，並只接受對應欄位：

- `paragraph_style`：`text` 為唯一定位文字；`named_style` 可用 `NORMAL_TEXT`、`TITLE`、`SUBTITLE`、`HEADING_1`～`HEADING_6`；選填 `alignment`（`START`、`CENTER`、`END`、`JUSTIFIED`）。
- `text_style`：唯一的 `text`，以及 `bold`、`italic`、`underline`、`font_size_pt`（6～96）至少一項。
- `table_cell_style`：從零起算的 `table_index`（0～99）、`row`（0～499）、`column`（0～49），以及 `background_hex`（`#RRGGBB`）。
- `header`：`text`（最多 1,000 字元）。
- `add_tab`：`title`（1～100 字元）。

### 資料夾規則與限制

管理員可列出資料夾或整個共用雲端硬碟。檔案位於清單中的共用雲端硬碟內，即符合範圍；其餘情況，每一條上層路徑都須在十二層內到達清單中的資料夾。上層無法讀取、目標或上層已在垃圾桶、超出檢查上限，或 Google 權限不足，都會拒絕操作。每次呼叫連同重新檢查，最多讀取十六次中繼資料。

建立與複製檢查目的資料夾；複製來源須可讀取且未在垃圾桶。修改、搬移與移到垃圾桶會檢查目標位置。清單上的資料夾本身不能改名、搬移或丟進垃圾桶。搬移的來源與目的都須在範圍內，不能跨「我的雲端硬碟」與共用雲端硬碟，也不能跨不同共用雲端硬碟。捷徑不能列入允許資料夾或當作目的位置；修改會拒絕捷徑，搬移與丟垃圾桶只作用於捷徑本身，依捷徑自己的上層位置檢查，不跟隨原始檔案。

資料夾清單為空時，建立與修改可在帳號能寫入的任何位置執行；搬移與移到垃圾桶不能使用。寫入前後都會檢查資料夾，核准後也會再查一次。這是盡力檢查：Google 本身沒有資料夾允許清單，檢查期間仍可能有其他人變更位置。事後發現超出範圍會通知管理員；無法完成的檢查也會記錄。每次嘗試（含拒絕與結果不明）都留紀錄，不記檔案內容與權杖。結果不明不會自動重試，請先到 Google 確認再要求寫入。

既有 `docs_append`、`docs_create`、`docs_replace_text`、`sheets_append`、`sheets_create`、`sheets_update` 沿用原本的授權規則，不受這四項開關與資料夾規則控制。上表四個新增的 Docs／Sheets 工具歸入「修改」。Google 官方 Drive 遠端 MCP 永遠拿不到完整 Drive 權限的權杖；這九個寫入工具只在主 gateway 行程內執行。

帳號需求見[連線設定](google-setup.md)與[個人寫入同意](google-personal-connections.md)。Google OAuth 隔離與撤銷、委派範圍、文件分頁、格式轉換、共用雲端硬碟角色及私訊確認送達仍待實際帳號驗收；本文不代表這些項目已通過驗證。
