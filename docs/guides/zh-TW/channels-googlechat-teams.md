# Google Chat 與 Microsoft Teams 通道

DuDuClaw 將 Google Chat 與 Microsoft Teams 列為一級通道（與 Telegram／LINE／
Discord／Slack／WhatsApp／Feishu／WebChat 並列）。兩者都是 webhook-based：
你的 gateway 必須能透過公開 HTTPS 被連到。

兩個通道都具備：

- **Markdown 感知回覆**：LLM 產生的 markdown 會轉換成各平台的原生標記
  （Google Chat 用 `*bold*` ／ `<url|text>` 連結；Teams markdown 中的表格會
  降級為等寬區塊）。
- **輸入提示**：Teams 會顯示真正的打字中指示（每 3 秒刷新一次）；Google
  Chat 沒有打字提示 API，因此 DuDuClaw 會先送出一則佔位訊息（「🤔 思考
  中…」）再原地編輯更新。
- **即時進度**：agent 執行長任務期間，工具活動與 agent 的 TODO 任務看板
  （來自 `TodoWrite`）會透過訊息編輯即時顯示，最終回覆送達後即會移除。

## Google Chat

### 設定

1. 建立（或沿用既有）Google Cloud 專案，並**啟用 Google Chat API**。記下
   **專案編號**（project number，位置在 IAM & Admin → Settings）。
2. 在同一個專案下建立**服務帳戶**（service account）並下載其 JSON 金鑰。
   不需要網域範圍委派（domain-wide delegation），Chat app 本身就是
   principal（scope 為 `chat.bot`）。
3. 開啟 Chat API 的 **Configuration** 設定頁：填入 app 名稱／頭像／說明，
   啟用 *Interactive features*，勾選 *Receive 1:1 messages* 與 *Join spaces
   and group conversations*，並將 **HTTP endpoint URL** 設為
   `https://<your-host>/webhook/googlechat`。
4. 在 *Authentication Audience* 選擇 **Project number**。
5. 設定 DuDuClaw（`config.toml`，或到儀表板 → 通道 → 新增 `googlechat`）：

```toml
[channels]
googlechat_project_number = "123456789012"
# Paste the full service-account JSON (stored encrypted as *_enc)
googlechat_service_account_json = '{ "type": "service_account", ... }'
```

6. 重啟 gateway。看到 log 印出
   `✅ Google Chat webhook ready at /webhook/googlechat` 即代表成功。

### 備註

- 進站請求一律 fail-closed 驗證：`Authorization: Bearer` JWT 必須由
  `chat@system.gserviceaccount.com` 簽發，且 audience 要等於你的專案編號。
- 回覆是透過 `spaces.messages.create` 非同步送出的（同步窗口只有 30 秒，
  對 agent 任務來說太短），並透過 `REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD`
  正確串起討論串。
- 未發布的 Chat app 只有你的 Workspace 組織內部看得到；要更廣的發佈範圍
  需要透過 Google Workspace Marketplace 上架。

## Microsoft Teams

> Office 365 Connectors／incoming webhooks 已於 **2026 年 5 月停用**，
> 目前唯一支援的傳輸方式是真正的 Azure Bot。

### 設定

1. **Entra app 註冊**（Azure portal → App registrations → New registration，
   選 *single tenant*）：記下**應用程式（用戶端）識別碼**（Application
   client ID）與**目錄（租用戶）識別碼**（Directory tenant ID），並建立一組
   **client secret**。
2. **Azure Bot 資源**（Create a resource → Azure Bot，免費的 **F0** 方案即
   可，Teams 訊息一律免費）：沿用既有的 App ID，將 *Configuration →
   Messaging endpoint* 設為 `https://<your-host>/webhook/teams`，再啟用
   *Channels → Microsoft Teams*。
3. 設定 DuDuClaw：

```toml
[channels]
teams_app_id = "00000000-0000-0000-0000-000000000000"
teams_app_password = "<client secret>"   # stored encrypted as *_enc
teams_tenant_id = "<tenant id>"          # empty = legacy multi-tenant bot
```

4. **Teams app 封裝**：製作一個 zip，內含 `manifest.json`（schema ≥1.19，
   `bots[].botId` 為你的 App ID，scope 涵蓋 `personal`／`team`／
   `groupChat`），再加上 `color.png`（192×192）與 `outline.png`
   （32×32）。透過 Teams → Apps → *Manage your apps* →
   *Upload a custom app* 上傳（需要組織開啟自訂 app 政策）或走組織目錄。
5. 重啟 gateway。看到 log 印出
   `✅ Microsoft Teams webhook ready at /webhook/teams` 即代表成功。

### 備註

- 進站活動一律 fail-closed 對 Bot Framework 的 JWKS
  （`login.botframework.com`）驗證，audience 要等於你的 App ID，且 token
  的 `serviceUrl` claim 必須與該活動相符，另外針對 single-tenant 註冊有
  Entra 租用戶範圍的備援驗證。
- 每次進站訊息都會保存一份**對話參照**（conversation reference，存於
  `~/.duduclaw/teams_conversations.json`，上限 500 筆），讓主動發送（如
  delegation callback 轉發、Computer Use 的截圖／確認訊息）之後也能找到
  該對話。對話必須至少對 bot 發過一次訊息，主動發送才能命中。
- 回覆使用 `textFormat: markdown`。Teams 在一般訊息中不渲染表格／標題，
  因此 DuDuClaw 會把表格降級為等寬 code block、標題降級為粗體。
- 在頻道（channel）中，bot 只會收到 @提及它的訊息（Teams 平台本身的行
  為），該提及會在送進 agent 前先被剝除。

## 格式對照表（所有通道）

| 通道 | 原生格式 | 表格 | 輸入提示 |
|------|----------|------|----------|
| Telegram | HTML parse mode（`<b>`、`<pre><code>`、`<blockquote>`） | 等寬 `<pre>` | `sendChatAction` 每 4 秒一次 |
| Discord | markdown + embeds | 等寬 code fence | `POST /typing` 每 8 秒一次 |
| Slack | 原生 `markdown` block（退回時用 mrkdwn） | 原生支援 | `assistant.threads.setStatus` |
| LINE | 純文字 + Flex bubble | key-value 記錄 | loading 動畫（1:1，≤60 秒） |
| WhatsApp | `*bold*` ／ `~strike~` ／``` 區塊 | 等寬 code fence | `typing_indicator`（≤25 秒，收到訊息時觸發） |
| Feishu | 互動式 Card 2.0 markdown | 原生支援 | 無（透過訊息顯示進度） |
| Google Chat | Chat markup（`*bold*`、`<url\|text>`） | 等寬 code fence | 佔位訊息 + 原地編輯 |
| MS Teams | markdown activity | 等寬 code fence | `typing` activity 每 3 秒一次 |
| WebChat | 原始 markdown（由儀表板渲染） | 原生支援 | `progress` WS 事件 |
