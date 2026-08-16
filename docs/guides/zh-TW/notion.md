# Notion 整合（搜尋＋讀取＋新增）

連接一個 Notion workspace，讓你的 AI 員工能搜尋頁面與資料庫、完整讀取頁面
內容，並在頁面上新增筆記。DuDuClaw 原生對接 Notion REST API，不需要安裝任
何第三方 MCP server。存取權杖會存放在 DuDuClaw 加密的 OAuth vault 中。

## 你會拿到什麼

四個面向 agent 的 MCP 工具，由兩個 scope 把關（`notion:read` ／
`notion:write`）：

| 工具 | 類別 | 功能 |
|------|------|------|
| `notion_status` | read | 連線診斷：是否已連接？只讀取本機狀態。 |
| `notion_search` | read | 搜尋已分享給這個 integration 的頁面與資料庫（Notion 搜尋語法比對標題）。回傳 id／title／type／last-edited／url。 |
| `notion_page_read` | read | 完整讀取單一頁面：metadata 加上攤平成純文字的頁面內容（常見 block 類型：paragraph／heading／list／to-do／quote／code／callout／table；最多約 200 個 block）。 |
| `notion_page_append` | write | 把文字以新的 paragraph block 附加到既有頁面（非空白行各自成一個 block）。絕不刪除或覆寫既有內容。 |

### 安全設計

- **只能附加寫入。** `notion_page_append` 只會在頁面底部新增 paragraph
  block，沒有任何刪除或覆寫用的工具。
- **read 就是唯讀。** read 類的工具無法對 Notion 做任何修改。
- **必須明確分享。** 你的 integration 只能看到你明確分享給它的頁面／資料
  庫（在 Notion 中：開啟頁面 → ••• → Connections → 加入你的
  integration）。其他任何內容都碰不到。
- **是外部知識來源，不是共用 wiki。** Notion 的內容只用來查詢與引用，
  **絕不**會自動被複製進 DuDuClaw 的共用 wiki，兩套知識庫彼此分開。
- **可選的審批關卡。** 想更謹慎的話，可以把寫入工具列進該 agent 的
  `agent.toml [capabilities] approval_required_tools`：

  ```toml
  [capabilities]
  approval_required_tools = ["notion_page_append"]
  ```

## 前置需求：建立 Notion OAuth integration

你需要自備一個 Notion integration（DuDuClaw 不會內建共用憑證）。一次性設
定步驟：

1. 開啟 [Notion → My integrations](https://www.notion.so/my-integrations)。
2. 點 **New integration**。把 integration type 設為 **Public**：只有
   public integration 才會提供 OAuth client ID／secret（internal
   integration 用的是固定 token，沒有 OAuth 流程）。
3. 在該 integration 的 **OAuth Domain & URIs** 底下，加入下列這組精確的
   redirect URI：

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. 複製 **OAuth client ID** 與 **OAuth client secret**。
5. 把你希望 AI 能存取的頁面／資料庫分享給這個 integration（開啟每個頁面
   → ••• → Connections → 選擇你的 integration）。

## 從儀表板連線

1. 前往**管理 → 整合／工具連線 → 工具伺服器**（`/manage/integrations`）。
2. 捲到**需要授權的服務**區塊，找到 **Notion** 卡片。
3. 點卡片上的 **Configure**。貼上 OAuth client ID 與 secret，對話框也會
   顯示要登記的精確 callback URL，必須跟你在上面步驟 3 填的完全一致。
4. 會跳出 Notion 的同意授權視窗，選擇要授權的 workspace 與頁面後核准，卡
   片就會變成 **Authenticated**。

Client 憑證會被保存下來（secret 靜態加密儲存），之後要重新授權不需要再
輸入一次 secret。

## 關於 Token

Notion 的 access token **長效且不會過期**，Notion 也不會核發 refresh
token。這代表：

- 連線畫面不會顯示到期日，這是正常現象，不是 bug。
- 沒有什麼需要刷新的。如果 token 在 Notion 端被撤銷，工具會回傳 `401`
  並引導你重新連線。

## Token 交換細節（給好奇的人看）

Notion 的 token endpoint 跟一般 OAuth 慣例不同：它要求 **HTTP Basic
auth**（`client_id:client_secret`）搭配 JSON body（而非 form POST），而
且 authorize URL 需要帶 `owner=user`。這些都由 OAuth 層裡 `notion`
provider 的分支自動處理，你不需要另外設定任何東西。

## 疑難排解

- **「Notion is not connected.」** 代表沒有存到 token，從儀表板連線即可。
- **`401 Unauthorized`** 代表 integration token 已被撤銷，重新連線即可。
- **`403` ／ `404` 「not found」** 代表該頁面／資料庫尚未分享給你的
  integration，分享後（頁面 → ••• → Connections）再試一次。

隨時都可以執行 `notion_status` 取得即時診斷。
