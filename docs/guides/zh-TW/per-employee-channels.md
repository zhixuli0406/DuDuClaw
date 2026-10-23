# 讓 AI 員工有自己的 Telegram Bot 與 Google Chat 應用程式

預設所有 AI 員工都透過公司共用的通道對外（一個 Telegram Bot、一個 Google Chat 應用程式）。照這份說明設定之後，大家可以直接找某一位員工專屬的 Bot 或應用程式說話，回答的就是那位員工。

位置：**AI 員工 → 點進員工 → 整合 → 專屬通道**。

| | Telegram Bot | Google Chat 應用程式 |
|---|---|---|
| 誰可以設定 | 只有管理員（員工負責人看得到狀態） | 只有管理員（員工負責人看得到狀態） |
| 要準備什麼 | @BotFather 給的 Token | 一個 Google Cloud 專案、一個 Chat 應用程式、服務帳戶 JSON 金鑰，以及 Google 連得到的 HTTPS 網址 |
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

每位員工要有**自己的 Google Cloud 專案**和自己的 Chat 應用程式（Google 規定一個專案一個 Chat 應用程式）。

1. 替這位員工開一個 Google Cloud 專案，啟用 **Google Chat API**。記下**專案編號**（專案資訊卡上的一串數字，不是英文的專案 ID）。
2. 在同一個專案建立**服務帳戶**，下載 **JSON 金鑰**。
3. 到 Chat API 的**設定（Configuration）**頁建立應用程式：開啟互動功能、允許一對一訊息和聊天室，連線方式選 **HTTP 端點網址**，填員工卡片上顯示的網址：`https://<你的公開網域>/webhook/googlechat/agents/<員工代號>`。**驗證對象（Authentication Audience）**選**專案編號**。
4. 由管理員把專案編號和整份 JSON 金鑰貼到 **專屬通道 → Google Chat**，按 **連線**。系統會用金鑰向 Google 要一次存取權杖來確認，卡片會顯示用的是哪個服務帳戶。
5. 在 Google Chat 把應用程式加進聊天室，或直接私訊它。

實際運作：

- Google 送到這個網址的每個請求，都先用**這位員工**的專案編號驗證，通過才讀內容。替別的應用程式簽的請求、驗證對象不對、沒有或偽造的權杖，一律拒絕（HTTP 401）。沒有設定應用程式的員工網址回 404。
- 回覆（以及這位員工之後轉送到聊天室的訊息）用這個應用程式的身分送出，所以應用程式必須在那個聊天室裡。
- 只改專案編號的話，金鑰欄留空即可，原本的金鑰會保留。
- 移除應用程式後，這個網址立刻關閉。

## 安全說明

- Token 和金鑰加密存在員工的 `agent.toml`（`bot_token_enc`、`service_account_json_enc`）。本機加密金鑰無法使用時，存檔會被拒絕，不會改存明文。
- 狀態只顯示 Bot 帳號和服務帳戶 email，不會顯示 Token 或金鑰。
- 每次設定和移除都寫進安全稽核紀錄（`agent_channel_bound`、`agent_channel_removed`）。

## 還沒涵蓋

- 排程通知和提醒仍然用共用的 Google Chat 應用程式送；回覆和派工結果才用員工自己的應用程式。
- Discord、Slack 的員工專屬 Bot 照舊在同一頁用「新增通道」（管理員）設定。
