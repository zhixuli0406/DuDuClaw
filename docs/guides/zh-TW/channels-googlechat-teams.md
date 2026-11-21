# Google Chat 與 Microsoft Teams 通道

Google Chat 頻道憑證授權 Chat 應用程式（`chat.bot`），不會連接 Gmail、Drive 或 Calendar。資料工具請依[連接 Google](google-setup.md)另行設定。儀表板儲存頻道後即時套用；手動修改頻道設定檔後才需重啟。

DuDuClaw 將 Google Chat 與 Microsoft Teams 列為一級通道（與 Telegram／LINE／
Discord／Slack／WhatsApp／Feishu／WebChat 並列）。Teams 與 Google Chat HTTP 模式需要公開 HTTPS 網址；Google Chat Pub/Sub 模式不需要。

兩個通道都具備：

- **Markdown 感知回覆**：LLM 產生的 markdown 會轉換成各平台的原生標記
  （Google Chat 用 `*bold*` ／ `<url|text>` 連結；Teams markdown 中的表格會
  降級為等寬區塊）。
- **輸入提示**：Teams 會顯示真正的打字中指示（每 3 秒刷新一次）；Google
  Chat 沒有打字提示 API，因此 DuDuClaw 會先送出一則佔位訊息（「🤔 思考
  中…」）再原地編輯更新。
- **即時進度**：agent 執行長任務期間，工具活動與 agent 的 TODO 任務看板
  （來自 `TodoWrite`）會透過訊息編輯即時顯示，最終回覆送達後即會移除。

### 回覆顯示與送達

Google Chat 表格改以清單呈現：每列以第一欄作為粗體標題，其餘欄位顯示為「欄名: 值」，保留完整內容。清單符號改用 `•`，標題及跨行粗體會轉成 Chat 標記；粗體旁的中文字不額外加空格。

工具細節與工作清單僅供內部人員查看（管理員，或已驗證且直接綁定該員工的帳號），其他發話者只看到一般處理進度。設定變更通知會保留到下一次內部回覆。存取遭拒、靜音或人工接手時，不會顯示「沒有產生回覆」警告，並會盡可能移除佔位訊息。

最終回覆開始送出後，進度不再修改同一則訊息。若無法更新佔位訊息，會短暫等待後重試一次（遇到流量限制最多等待三秒），再失敗則改發新訊息，並盡可能將舊佔位訊息改成中性提示；其餘回覆段落仍會嘗試送出。若原本的更新已成功、只是回應遺失，第一段可能重複出現。

Google Chat 回覆不附加思考摘要。既有思考設定最多讓內部人員看到一行思考時間，其他人不顯示。回覆語言明確為 `zh-TW` 時，會以有限字表將簡體字轉為繁體字，保留程式碼與網址；自動語言則在每次提問末尾提醒依使用者訊息的語言回覆。

如果閘道在回覆途中重啟，會嘗試將原本的「🤔 思考中…」改成「這次回覆中斷了，請再傳一次。」，直接編輯原訊息，不另發通知。每次啟動最多處理 200 則、耗時最多 30 秒；Google API 暫時無法使用時，會留待下次重啟再試，超過 24 小時便清除復原記錄。Pub/Sub 處理途中中斷的訊息不會自動重跑，請重新傳送訊息。

尚未在真實 Google Chat 用戶端驗證。請在網頁版與手機版確認表格清單、中文粗體邊界、跨行粗體及佔位訊息替換。

## Google Chat

### 開始之前

需要啟用 Chat 的 Google Workspace Business／Enterprise 帳號，且管理員允許使用 Chat 應用程式。您必須能建立或直接擁有 Cloud 專案、啟用 Chat API、建立服務帳戶及金鑰。本機需有 Bash、Python 3、`gcloud`，並以自己的帳號完成 `gcloud auth login`；Cloud Shell 已提供這些工具。HTTP 模式需要公開 HTTPS 網址；Pub/Sub 模式透過向外連線運作，不需公開網址。

### 一步建立與匯入

建議使用新的 Google Cloud 專用專案（預設做法）。在 DuDuClaw 主機執行：

自動匯入需要正在執行的 DuDuClaw gateway 與管理員登入（有啟用 MFA 時也須完成驗證）。

```bash
# HTTP：換成自己的公開 gateway 網址。
duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat
```

指令會建立專案、啟用 Chat API、準備服務帳戶，最後才建立私密 JSON 金鑰，再匯入設定。以 `--project-id your-project-id` 沿用您直接擁有的專案；透過群組或繼承取得的擁有者權限會保守拒絕。另有 `--app-name`、`--agent`、`--out`、`--dry-run`（不變更雲端或本機）。預設專案 ID 為 `duduclaw-chat-<random6>`。金鑰保存在私密輸出目錄的 `service-account.json`（0600）；重跑會沿用符合專案的既有金鑰。匯入後的憑證加密儲存。HTTP 模式下，金鑰建立後若後續腳本步驟失敗，會嘗試撤銷新建的雲端金鑰並刪除本機檔案；撤銷失敗或無法讀取金鑰 ID 時會明確提示人工處理。 若腳本已成功但 CLI 匯入失敗，CLI 會保留金鑰檔並以非零狀態結束。新建的服務帳戶金鑰仍然有效；錯誤提示會列出金鑰檔路徑，以及重試匯入或手動撤銷的完整指令。CLI 不會自動撤銷金鑰或刪除金鑰檔。

本機沒有 gcloud 時，可[在 Cloud Shell 開啟](https://shell.cloud.google.com/cloudshell/editor?cloudshell_git_repo=https://github.com/zhixuli0406/DuDuClaw&cloudshell_tutorial=scripts/googlechat/cloudshell-tutorial.md&show=terminal)，執行 `bash scripts/googlechat/bootstrap.sh --endpoint-url https://chat.example.com/webhook/googlechat`。把金鑰安全下載到 DuDuClaw 主機，執行 `chmod 600 /private/path/service-account.json`，再以下載後路徑執行匯入指令。Windows 請將一般金鑰檔案存放在自己的使用者設定檔目錄內；匯入會拒絕 reparse point，但未驗證擁有者與 ACL 權限。

```bash
duduclaw channels googlechat import --project-number 123456789012 --key-file /private/path/service-account.json
```

### 完成 Google Chat 設定

開啟指令印出的 Chat Configuration 連結。Google 未提供這個設定頁的 API，最後這一步需手動完成：

1. **建立時請取消勾選「以 Google Workspace 外掛程式建構這個 Chat 應用程式」（Build this Chat app as a Google Workspace add-on）。** 填入應用程式名稱（最多 25 個字元）、公開 HTTPS 頭像網址與說明。腳本不假設已有可用的 DuDuClaw 頭像網址。
2. 開啟 **Interactive features**，允許一對一訊息與加入聊天室。
3. 填入 HTTP 端點，**Authentication Audience 選 Project number**。
4. **Visibility** 填自己的 Workspace email，儲存後把應用程式加進聊天室或直接私訊，確認真的收到回覆。

資源建立或匯入成功不代表 Chat 設定與收發已成功。儀表板提供可複製的建立指令、Cloud Shell、設定值；既有欄位保留在「手動設定」。

### 認出傳訊的是誰（v1.82）

Google Chat 的傳訊者預設是不認識的人，所以 `[google] requesters = "bound_people"`／`owner_only` 會拒絕，`@` 也只能叫預設員工與公開員工。符合下列情況時，傳訊者就會被認出是某個 dashboard 帳號：

- 管理員在 **使用者 → 驗證身分 → Google Chat** 綁定了他的 Chat 使用者代號（`users/<數字>`）——預設只有這種方式；或
- 選用、**預設關閉**：管理員在「管理 → 整合 → Google」開啟 **用連結的 Google 帳號認出 Google Chat 傳訊者**（`[google] chat_identity_from_account = true`），且同仁在 **我的連線** 連結了自己的 Google 帳號。系統會把傳訊者的代號與這個 Google 帳號的代號比對，相同就視為本人。**尚未以真實 Google Workspace 驗證**——除非您已在自己的 Workspace 確認兩種代號一致，否則請保持關閉。

不做 Email 比對。其他 Chat 應用程式（不是真人）傳來的訊息一律忽略；Google Chat 身分也不會收到 DuDuClaw 主動發出的通知。Google Chat 不能收登入驗證碼或雙因素驗證碼。詳細規則（含一個代號只能對應一位同仁）：[儀表板設定 → 使用者 → 驗證身分](dashboard-settings.md#使用者--驗證身分通訊軟體身分v182)。

### 金鑰政策

建立金鑰若回報 `FAILED_PRECONDITION`，請組織政策管理員審核專案例外。2024-05-03 起建立的組織通常預設限制金鑰建立。腳本只印出以下指令並停止，不會自動修改政策：

```bash
gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation --project=your-project-id
gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation --project=your-project-id
```

managed constraint 請管理員確認所安裝的 gcloud 是否支援，必要時透過 Organization Policies 設定頁處理。見 [Google 金鑰政策指引](https://cloud.google.com/iam/docs/troubleshoot-org-policies)。

### 附錄：手動設定

自行建立專案、啟用 Chat API、建立服務帳戶與 JSON 金鑰，再照上文完成 Chat 設定頁。到「通道 → 新增 Google Chat → 手動設定」填專案編號與整份 JSON 金鑰。儲存後實測訊息。HTTP 驗證來自 `chat@system.gserviceaccount.com` 且 audience 為專案編號的 JWT。不支援的 Workspace 外掛程式事件會留下診斷，提示取消外掛程式選項。

### Pub/Sub：不需公開 gateway 網址

在 Unix DuDuClaw 主機執行：

```bash
duduclaw channels googlechat bootstrap --connection pubsub
# 或匯入已建立的金鑰與拉取訂閱：
duduclaw channels googlechat import --connection pubsub --project-number 123456789012 --key-file /private/path/service-account.json --subscription projects/your-project-id/subscriptions/duduclaw-chat-events-sub
```

Cloud Shell 執行 `bash scripts/googlechat/bootstrap.sh --connection pubsub`，安全下載金鑰後，在主機執行摘要中的匯入指令。員工專屬應用程式加 `--agent alice`，每個應用程式使用自己的專案。腳本啟用 Chat 與 Pub/Sub API，建立 `projects/<project-id>/topics/duduclaw-chat-events` 與保留一天的拉取訂閱，授予 Google Chat 發布者主題發布權，以及服務帳戶該訂閱的訂閱者與中繼資料讀取權。資源檢查與 IAM 授權完成後才建立金鑰。沿用的訂閱必須指向固定主題，不會自動變更其設定；匯入與 worker 啟動時都會向 Google 讀回訂閱確認。Pub/Sub 設定失敗只列出手動撤銷指令並保留金鑰，不會執行破壞性的 gcloud 指令。

在 Chat Configuration 關閉 **Build this Chat app as a Google Workspace add-on**，連線方式選 **Cloud Pub/Sub**，貼上摘要中的主題。此模式不需公開 HTTP 端點，其餘應用程式與可見性設定照上文完成，再實測訊息。gateway 需能透過 HTTPS 對外連接 Google OAuth、Pub/Sub 與 Chat API。

閘道會向 Google Chat 回讀每則訊息，使用 [`spaces.messages.get`](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/get)、應用程式身分與 `https://www.googleapis.com/auth/chat.bot`，不要求需管理員核准的 Chat scope。拉取訂閱另使用只有 `https://www.googleapis.com/auth/pubsub` 的權杖。Pub/Sub 內容僅是不可信通知，只有嚴格驗證過的 Chat 訊息資源名稱用來查詢；傳訊者、聊天室、本文、附件與討論串全部取自 API 回應。既有傳訊者白名單、配對與核准規則套用在回讀身分。訊息不存在、無權讀取（包括 403／404）、資源格式錯誤或聊天室不符時，會留下診斷並丟棄，不建立回合；看到提示時請檢查應用程式存取權與 `chat.bot`。無法回讀訊息的事件，包括加入聊天室事件，不會啟動員工回合。

持久化收件匣以事件類型與 Chat 訊息資源名稱去重；改用新的 Pub/Sub message ID 重送，也不能重跑仍在保留期的事件。紀錄保留七天，API 建立時間超過七天的訊息在清理後仍會拒絕。接受的事件先保存再確認收件；重啟後可恢復尚未開始的工作，已開始卻中斷的工作標為不確定，不自動重跑。佇列滿時確認收件並丟棄，不啟動回合，並和 HTTP 模式一樣在聊天室提醒對方稍後再傳（同一人每分鐘、同一聊天室每 10 秒最多一次），這不代表端到端恰好一次投遞。僅支援 Unix 的收件匣只存資源參照，不存本文、傳訊者資料或附件；每個 app stream（員工／共用 app 與專案編號）上限為 1,000 筆、10 筆 pending／queued／running，為其他 stream 保留容量；全域硬上限仍為 10,000 筆、100 筆待處理或執行中事件，每筆參照 2 KiB、資料庫 64 MiB，正在排隊或執行的紀錄不淘汰。非 Unix 平台在具備等價的擁有者／ACL 驗證前，拒絕 Pub/Sub 金鑰檔匯入。Google 端回讀與實際收發仍需活體驗收。 匯入與 worker 啟動均拒絕啟用 exactly-once（恰好一次投遞）的訂閱，請在 Google Cloud 關閉該選項。ACK 無效或過期（HTTP 400）時只清除舊投遞批次並重新拉取，持久化去重紀錄仍保留；暫時性 ACK 失敗則保留批次重試。

### 指令、存取與配對

聊天指令（`/new`、`/reset`、`/compact`、`/pair`、`/status` 等）和一般訊息一樣，先在同一個聊天室通過存取檢查：被封鎖的人不會收到任何回應，尚未配對的人會收到配對提示。

一個聊天室只有一份對話，所有成員與討論串共用（AI 員工專屬的 Chat app 在同一聊天室內另有自己的一份）。誰可以清除或壓縮共用的對話（`/new`、`/reset`、`/compact`）：

| 安裝狀態 | 與 app 的一對一私訊 | 聊天室、群組對話（多人私訊）或類型不明 |
|---|---|---|
| 沒有建立使用者帳號，或沒有任何啟用中的**管理員**／**主管**綁定已驗證的通訊帳號（例如個人版） | 通過存取檢查的人都可以 | 通過存取檢查的人都可以 |
| 至少一位啟用中的管理員或主管綁定了已驗證的通訊帳號 | 通過存取檢查的人都可以 | 只有通訊帳號已驗證並綁定到啟用中管理員或主管的人；其他人會收到「只有管理員可以清除這個聊天室的對話。」 |
| 人員資料存在但無法讀取 | 通過存取檢查的人都可以 | 一律拒絕 |

被封鎖或尚未配對的人在任何情況下都不能使用這些指令。第二列中，帳號未驗證或查詢失敗時一律拒絕。聊天室類型取自訊息本身（網址接收的事件，或 Pub/Sub 模式下以 `spaces.messages.get` 重新讀取的訊息）；訊息裡沒有時，閘道會以 app 身分向 Google Chat 查詢（`spaces.get`），結果保留 24 小時。一對一私訊指一個人與 app 之間的私訊；多人的群組對話視為共用對話；仍然無法得知類型時，一律視為多人聊天室。Telegram 群組、Discord 伺服器頻道、Slack 頻道、LINE 群組、Teams 群組與頻道、飛書群組、釘釘群組，以及「開啟新對話」按鈕也套用同一規則。

配對（`require_pairing`）：用 `pairing_manage` 工具（`action: "generate"`）為下列其中一種對象產生配對碼：

- `users/<id>`：某位成員自己的 Chat 帳號。只有這位成員本人能使用，其他人輸入一律失敗。
- `googlechat:spaces/<id>`：整個聊天室。配對後，這個聊天室的所有人都能通過。

`/pair <配對碼>` 先比對傳訊的人，再比對聊天室，和一般訊息相同。AI 員工專屬對話的對象（`googlechat:spaces/<id>#agent:<員工代號>`）會以聊天室本身儲存與檢查，所以在員工專屬 Chat app 中配對也會生效。舊版存下、帶有員工代號的配對紀錄過去不會讓任何人通過，升級後也一樣，不會因此開放整個聊天室；需要時請重新產生配對碼。配對碼 5 分鐘內有效，錯 5 次即鎖定；同一對象累計失敗 15 次後不能再產生新的配對碼。

### 私訊、決定按鈕與儀表板連結（v1.84）

**一對一私訊算私人對話。** 一個人和應用程式之間的私訊，現在和 LINE、Telegram 的私訊一樣算私人對話。Google Chat 身分已驗證並綁定的成員，因此可以在私訊裡取得自己有權讀取的個人或受限知識集合的內容，員工在私人對話才會啟用的功能（人物筆記、記憶）也會套用。成員能讀什麼仍由綁定與授權決定，私訊本身不額外給任何權限；沒有綁定的傳送者在私訊裡讀不到個人或受限的知識集合。聊天室與群組聊天仍是共用對話。判斷依據是 Google 為這則訊息回報的聊天室類型（`spaceType` 為 `DIRECT_MESSAGE` 且 `singleUserBotDm` 為真），不看對話代號，傳送者也必須是真人帳號；類型不明時當成共用。從私訊交辦出去的工作不會被當成那段私訊。

**目標決定卡片的按鈕。** 應用程式以 HTTP 接收事件時，目標「需人工決定」卡片會帶「重試」「標記完成」「放棄」「交給我」四個按鈕（文字為繁體中文）。以 Pub/Sub 接收事件的應用程式收不到可信任的按鈕事件，維持文字卡片，並提示到儀表板的待辦決定頁處理。是 HTTP 還是 Pub/Sub，共用應用程式看 `[channels] googlechat_connection` 與 `googlechat_pubsub_subscription`，員工自己的應用程式看該員工的 `[channels.googlechat] connection` 與 `pubsub_subscription`；只要判斷不明確就不送按鈕。誰可以按、按了之後會怎樣，見[目標迴圈](goal-loop.md#按鈕)。

**聊天訊息裡的儀表板連結。** 通知與決定卡片裡的儀表板連結，依序採用 `[dashboard]`、`[general]`、`[gateway]` 中第一個有效的 `public_url`。有效的值必須是 http 或 https、主機不是 localhost 或本機迴路位址，而且不含帳號、查詢字串或片段；無效的值會被略過，每個區塊記錄一次警告。都無效時，在容器裡執行的閘道（有 `/.dockerenv` 或設定了 `DUDUCLAW_IN_CONTAINER`）不附連結，一般主機上的閘道則保留 `http://localhost:<port>` 連結。邀請連結採用相同順序，但仍須是 https。

### 限制

- Google Chat 目前仍無法直接傳送 AI 產生的檔案或圖片附件。系統會明確說明限制，不再顯示主機路徑，也不會要求可能沒有帳號的成員到 Dashboard 下載。請向管理員索取檔案，或改用可傳檔的通道；排程回覆也會先處理檔案交付標記再送出文字。
- Drive 附件不會自動下載。系統會將驗證過的檔案 ID 交給 AI，讓它嘗試使用 Drive 讀取工具；仍受員工連結的 Google 身分權限與支援檔案類型限制。直接上傳的附件下載失敗時會留下失敗註記，不再把 HTTP 錯誤回應存成附件。貼上的 Drive 連結處理方式不變。
- 主動發送的目標必須是 `spaces/<id>`。收到 403／404 時會說明應用程式可能不在該對話，請對方先私訊應用程式一次，或請管理員安裝；限流與伺服器錯誤則提示稍後重試。本功能不會將人員解析成私訊聊天室，也不會以使用者身分發送。

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
| Google Chat | Chat markup（`*bold*`、`<url\|text>`） | 粗體列標題＋欄名和值清單 | 佔位訊息 + 原地編輯 |
| MS Teams | markdown activity | 等寬 code fence | `typing` activity 每 3 秒一次 |
| WebChat | 原始 markdown（由儀表板渲染） | 原生支援 | `progress` WS 事件 |

## 群組近期對話設定

管理員需在「通道 → 群組設定」逐群開啟保留。Telegram 可透過 BotFather `/setprivacy` 關閉隱私模式，或依 bot 管理員身分取得較廣的收訊；Slack 需訂閱 `message.channels`／`message.groups` 並授予 `channels:history`／`groups:history`，再重新安裝 App、邀請加入頻道。Discord 需開啟 MESSAGE CONTENT INTENT。Google Chat 與 Teams 可能只包含提及 App 的訊息，Teams 完整頻道收訊另需 `ChannelMessage.Read.Group` 等 RSC 權限。保留期限、員工讀取權與平台限制見[群組近期對話](../../features/zh-TW/72-group-context.md)。本次變更尚未驗證實際平台收送。

### Google Chat 回覆與預覽連結

私訊預設回覆在主畫面；使用者在討論串內傳送的訊息（`threadReply = true`）仍回覆在原討論串。聊天室維持既有討論串行為。管理員可到「通道 → Google Chat → 行為 → 私訊回覆位置」選擇「主畫面」（預設）或「一律回覆在討論串」。此設定僅能由儀表板修改，儲存在通道設定資料庫，請勿加入 `config.toml`。對話 session 仍以整個聊天室為單位。

Google 文件／試算表的預覽連結會作為不可信訊息文字交給 AI，移除重複連結後最多保留 10 個 HTTPS 連結，每個上限 2048 位元組，含控制字元的連結會捨棄。閘道不會抓取連結。Chat 讀取工具的 `links` 陣列也納入內容掃描。缺少名稱時顯示可讀的替代文字；資源名稱與附件儲存路徑僅供工具操作，不應轉述給使用者。附件保留原始檔名，不需要新增 OAuth 權限。
