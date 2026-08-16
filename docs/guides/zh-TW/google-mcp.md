# Google 官方 remote MCP 掛載（進階選項，非出貨路徑）

> **先看這裡**：DuDuClaw 對 Google Workspace 的**支援路徑是原生工具**：
> Gmail／Calendar／Sheets／Drive／Docs／Slides／Forms／Tasks 八個服務
> 全部走 GA REST API，19 個 MCP 工具，見
> [google-workspace.md](../google-workspace.md)。**一般情況不需要讀本頁。**
>
> 本頁記錄的官方 remote MCP 是**進階選項**，有兩個硬限制：
> ① 仍是 Developer Preview，需申請資格；② Program Terms 規定 GA 之前
> **不得讓自家網域／公司以外的終端使用者**透過你的應用使用 Pre-GA API，
> 也就是**不能出貨給客戶**（除非每個客戶各自申請自己的資格與 GCP 專案）。
> 因此 2026-07-30 拍板：產品面完全走原生工具，本掛載只保留給自用與願意
> 自行申請的進階使用者。

## 八個服務的覆蓋狀況

| 服務 | 原生工具（**出貨路徑**，無資格限制） | 官方 MCP（preview，僅自用） |
|---|---|---|
| Gmail | ✅ 4 工具 | ✅ `preset = "google:gmail"`（13 工具） |
| Calendar | ✅ 2 | ✅ `google:calendar`（9） |
| Sheets | ✅ 2 | ✅ `google:sheets`（7） |
| Drive | ✅ 2 | ✅ `google:drive`（8） |
| Docs | ✅ 2 | ✅ `google:docs`（2） |
| Slides | ✅ 1（唯讀） | ✅ `google:slides`（2） |
| Forms | ✅ 2 | ❌ 官方無 |
| Tasks | ✅ 4 | ❌ 官方無 |

官方 MCP 的工具面在 Gmail／Drive／Sheets 上比原生豐富（標籤管理、權限查詢、
公式寫入等），這是它唯一的優勢；代價是 preview 資格與不可出貨。原生工具數
共 19（含 `google_status`）。

Forms 與 Tasks 的「官方無」是查證過的結果：`formsmcp.googleapis.com` /
`tasksmcp.googleapis.com` 實測回 404，Google 的 MCP 文件也完全沒有這兩個服務
（連 coming soon 都沒有）。另外官方還有 Chat（`preset = "google:chat"`，可用）
與 People（端點命名不同，未納入 preset）；這兩個目前沒有原生工具對應。

### 各服務工具清單（實測 `tools/list`）

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
- 只有互動式 OAuth，**無 service account／headless 授權路徑**。
- Gmail 無寄信工具；官方 reference 頁的工具數與實際端點有出入（reference 列
  10 個、端點實際 13 個），以 `tools/list` 實測為準。
- 端點若在 GA 時改名，只需改 `mcp_external.rs` 的 `GOOGLE_MCP_PRESETS`
  一處（使用者的 `agent.toml` 不必動）。

參考來源：
[configure-mcp-servers](https://developers.google.com/workspace/guides/configure-mcp-servers)、
[Gmail MCP reference](https://developers.google.com/workspace/gmail/api/reference/mcp)、
[Calendar MCP reference](https://developers.google.com/workspace/calendar/api/v3/reference/mcp)、
[Developer Preview Program](https://developers.google.com/workspace/preview)。
