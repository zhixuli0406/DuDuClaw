# 讓 AI 員工有自己的 Telegram Bot 與 Google Chat 應用程式

Google Chat 頻道憑證授權 Chat 應用程式（`chat.bot`），不會連接 Gmail、Drive 或 Calendar。資料工具請依[連接 Google](google-setup.md)另行設定。儀表板儲存頻道後即時套用；手動修改頻道設定檔後才需重啟。

預設所有 AI 員工都透過公司共用的通道對外（一個 Telegram Bot、一個 Google Chat 應用程式）。照這份說明設定之後，大家可以直接找某一位員工專屬的 Bot 或應用程式說話，回答的就是那位員工。

位置：**AI 員工 → 點進員工 → 整合 → 專屬通道**。

| | Telegram Bot | Google Chat 應用程式 |
|---|---|---|
| 誰可以設定 | 只有管理員（員工負責人看得到狀態） | 只有管理員（員工負責人看得到狀態） |
| 要準備什麼 | @BotFather 給的 Token | 一個 Google Cloud 專案、一個 Chat 應用程式、服務帳戶 JSON 金鑰，以及公開 HTTPS 網址 |
| 要不要重新啟動 | 不用 | 不用 |
| 沒設定時 | 走共用的 Telegram Bot | 走共用的 Google Chat 應用程式 |

一個 Bot 或一個 Chat 應用程式只能給一位員工。已經被別的員工用掉的，存檔時會被拒絕。

## Telegram

1. 在 Telegram 搜尋 **@BotFather**，傳 `/newbot`，照指示取名字和帳號。
2. BotFather 會回一串 Token，像 `123456789:AAE…`，整串複製。
3. 貼到 **專屬通道 → Telegram**，按 **連線**。

存檔前會先問 Telegram 這個 Bot 是誰。Telegram 不認的 Token 不會存；當下連不到 Telegram 的話會先存起來，Bot 自己會一直重試。存好後卡片會顯示 **已連線，Bot 是 @你的bot**，或是沒連上的原因（例如在 BotFather 撤銷 Token 之後會顯示「Bot Token 無效」，把新的 Token 貼上即可）。

實際運作：

- 私訊和群組都可以。群組裡只有被提及（`@你的bot`）、被回覆或收到指令時才回答；每個群組可以在通道的行為設定裡改。
- 傳給這個 Bot 的訊息都由這位員工回答。用 `@` 叫其他員工只在共用 Bot 有效。
- 同一個人也在跟其他員工的 Bot 聊天時，每位員工的對話紀錄各自分開。
- 誰可以跟這個 Bot 說話，照 Telegram 通道的存取規則（允許／封鎖名單、配對），和共用 Bot 一樣。
- 「思考中…」狀態訊息和共用 Bot 相同。
- 如果共用 Bot 用的是同一個 Token，這位員工的 Bot 會接手；移除員工的 Bot 後還給共用設定。

## Google Chat

每位員工需要**自己的 Google Cloud 專案**與 Chat 應用程式（一個專案只能有一個 Chat app）。先依[共用設定的前提、金鑰政策與 Cloud Shell 步驟](channels-googlechat-teams.md#google-chat)，並加上 `--agent <員工代號>`：

自動匯入需要正在執行的 DuDuClaw gateway 與管理員登入（有啟用 MFA 時也須完成驗證）。

```bash
duduclaw channels googlechat bootstrap --agent alice --endpoint-url https://chat.example.com/webhook/googlechat/agents/alice
```

`--agent` 在建置腳本影響名稱，在 CLI 匯入時指定員工，不會建立員工，請先建立該員工。每位員工仍使用自己的專案。依輸出完成 Google Chat 設定頁的名稱／頭像／說明、互動功能、一對一與聊天室、連線值與自己 email 的可見範圍，再測試私訊與聊天室回覆。Cloud Shell 使用者需安全下載金鑰，在 DuDuClaw 主機執行印出的匯入指令，保留 `--agent alice`。

建議每個應用程式使用新的專用專案。**建立時請取消勾選「以 Google Workspace 外掛程式建構這個 Chat 應用程式」（Build this Chat app as a Google Workspace add-on）。**

### 附錄：手動設定

在「專屬通道 → Google Chat → 手動設定」填專案編號與整份 JSON 金鑰。將 `https://<公開網域>/webhook/googlechat/agents/<員工代號>` 貼到 Google 設定頁，Authentication Audience 選 **Project number**。儲存後實測訊息。只有管理員可以設定或移除憑證，儲存時會加密；金鑰欄留空會沿用既有金鑰。應用程式必須已加入要回覆的聊天室。


## 之後的訊息從哪裡送出

之後回到這段對話的訊息，都從這段對話進來的那個 Bot 或應用程式送出：

- 這位員工交辦給其他 AI 員工後，對方的回覆；
- 員工設在這個聊天室的提醒、員工自己傳回這個聊天室的訊息；
- 這個聊天室的審批請求、`/goal` 進度和其他通知。

Telegram 用的是員工自己的 Bot；如果那個 Bot 在這期間被移除，訊息會送失敗，不會改用別的 Bot。Google Chat 用的是最近從那個聊天室收到訊息的應用程式；如果 Google 因為那個應用程式已經離開聊天室而拒絕，就把它從紀錄移除，下一次改用聊天室裡另一個應用程式。

## 安全說明

- Token 和金鑰加密存在員工的 `agent.toml`（`bot_token_enc`、`service_account_json_enc`）。本機加密金鑰無法使用時，存檔會被拒絕，不會改存明文。
- 狀態只顯示 Bot 帳號和服務帳戶 email，不會顯示 Token 或金鑰。
- 每次設定和移除都寫進安全稽核紀錄（`agent_channel_bound`、`agent_channel_removed`）。

## 還沒涵蓋

- 這一版之前建立的提醒仍然由共用 Bot 送出。
- Google Chat 的 HTTP 連線只支援「專案編號」驗證對象，不支援「HTTP 端點網址」。
- Discord、Slack 的員工專屬 Bot 照舊在同一頁用「新增通道」（管理員）設定。
