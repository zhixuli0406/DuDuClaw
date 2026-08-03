# Google Workspace 整合設定指南

> **拍板立場（D5，2026-08-04）**：DuDuClaw **不**預埋自家的 Google OAuth
> client id / secret。三條接法都是「你（操作者）或客戶自己申請憑證」，
> DuDuClaw 只負責把申請完成後的憑證加密存放在本機並自動續期。如果日後客戶
> 普遍反映「自己申請太麻煩」，才會考慮改由嘟嘟端提供共用授權——目前尚未
> 到那一步。

這份文件回答一個問題：**要讓 AI 員工用 Gmail／行事曆／Sheets 等 Google
服務，該選哪一條接法、每一步要點什麼？** 完整的工具清單、安全設計、疑難
排解與已知限制，分別在下面三份深入文件裡，這裡只做「選路徑＋照著做」：

- [google-workspace.md](google-workspace.md) — 19 個原生 MCP 工具的完整清單、
  安全設計（草稿不寄送、附加不覆寫）、re-auth 疑難排解。
- [google-no-oauth-client.md](google-no-oauth-client.md) — 服務帳號委派與
  Apps Script 橋接的安全性質、失敗案例、已排除方案的實測數據。
- [google-mcp.md](google-mcp.md) — 官方 remote MCP 掛載（進階選項，僅供
  自用，不可出貨給客戶）。

## 三條路怎麼選

| 路徑 | 適合誰 | 覆蓋工具 | 誰要操作 |
|---|---|---|---|
| ① 自建 OAuth client | 個人 `@gmail.com`，或不想麻煩公司 IT 的 Workspace 使用者 | 19 個工具全通 | 你自己在 Google Cloud Console 申請 |
| ② 服務帳號網域委派（DWD） | 企業 Workspace 客戶，且不想每個帳號都跳一次同意畫面 | 19 個工具全通 | 客戶的網域超級管理員授權一次 |
| ③ Apps Script 橋接 | 個人 `@gmail.com`，完全不想碰 Cloud Console | 僅 Gmail／行事曆／Sheets（3 類，非全部 19 個工具） | 使用者自己部署一份腳本 |

三條路授權的是**同一組工具**，dashboard 上是「三選一」而非可同時疊加：
儲存其中一種時，伺服器會清掉另一種已存的憑證，避免「你以為生效的路徑」跟
「實際生效的路徑」不一致。

## Scopes 一覽（11 個）

> **與會議記錄的落差**：規劃文件寫「19 個 scopes」，但這是把「MCP 工具數」
> 誤植為「OAuth scope 數」。核對程式碼
> （`crates/duduclaw-gateway/src/google_workspace.rs:98-116`
> `REQUIRED_SCOPES`）後，實際請求的 scope **是 11 個，不是 19 個**——19
> 指的是這組憑證解鎖的 MCP 工具數量，這點 UI 文案（`google.cred.intro`
> 等 i18n key）與既有文件用的都是「19 個工具」，說法一致，本文件依程式為準
> 更正為 11 個 scope。

| # | Scope | 用途（一句話） |
|---|---|---|
| 1 | `gmail.readonly` | 讓 AI 能搜尋、讀取 Gmail 郵件內容（唯讀，不能刪除或修改）。 |
| 2 | `gmail.compose` | 讓 AI 能建立 Gmail 草稿（只能存草稿，沒有寄送權限）。 |
| 3 | `calendar.events` | 讓 AI 能讀取、建立你主要行事曆上的事件（含建立 Google Meet 會議連結）。 |
| 4 | `spreadsheets` | 讓 AI 能讀取 Google 試算表的資料，並在表尾新增一列。 |
| 5 | `drive.readonly` | 讓 AI 能搜尋並讀取（匯出）Drive 檔案內容，唯讀，不建立或修改任何檔案。 |
| 6 | `documents` | 讓 AI 能讀取 Google 文件全文，並在文件**結尾**附加文字（無法改寫或刪除既有內容）。 |
| 7 | `presentations.readonly` | 讓 AI 能逐頁讀取 Google 簡報的文字內容，唯讀，沒有對應的寫入工具。 |
| 8 | `forms.body.readonly` | 讓 AI 能讀取 Google 表單的題目結構（標題、題型、選項）。 |
| 9 | `forms.responses.readonly` | 讓 AI 能讀取 Google 表單已收到的填答結果。 |
| 10 | `tasks` | 讓 AI 能讀取、建立 Google Tasks 待辦事項，並標記完成。 |
| 11 | `userinfo.email` | 讓系統知道目前連接的是哪個 Google 帳號，用於連線狀態顯示與診斷，不涉及信件內容。 |

## 設定頁在哪裡、按鈕各做什麼

**位置**：管理選單 → 整合 → Google（網址 `/manage/integrations?tab=google`）。

這個分頁本身在 v1.49.0 起預設可見（先前版本隱藏，等原廠 OAuth App
驗證進度；後來確認驗證只擋「自建 OAuth client」這一條路，服務帳號委派與
Apps Script 橋接都不受影響，分頁就沒有理由繼續隱藏）。但**分頁看得到，
不代表工具會生效**：後端還有一個獨立總開關 `config.toml [integrations]
google_workspace`，預設是 `false`。開關沒開時，憑證可以照樣設定、「測試
連線」也能顯示綠燈，但工具就是不會出現在 AI 員工面前——頁面上會有一則
黃色提示講清楚這件事，不是靜默失敗。要開啟：

```toml
[integrations]
google_workspace = true
```

頁面分兩塊：

1. **上方連線面板**（只用於路徑①自建 OAuth client）：填 Client ID /
   Client secret → 按「**連接 Google**」→ 跳出 Google 同意視窗 → 完成後
   面板轉為「Google 已連接」，列出已授權的存取範圍。已連接時可按「修改
   憑證」換一組用戶端，或「中斷連線」——中斷只撤掉存取權杖，用戶端設定
   保留，方便一鍵重連。
2. **下方「憑證方式」卡片**（三個分頁：OAuth 連線／服務帳號委派／Apps
   Script 橋接）：分頁只是選你要**編輯**哪一種，實際**生效**的路徑以卡片
   上「目前生效」徽章為準。「**儲存**」只寫入設定，不驗證；「**測試連線**」
   會實際打一次 Google API，回傳目前生效帳號或明確的錯誤訊息——所以要先
   按儲存，再按測試連線，兩者不是同一件事。

## 路徑① 自建 OAuth client（個人 / 不想麻煩管理員的 Workspace 使用者）

適合誰：個人 `@gmail.com`，或雖在 Workspace 網域但不想找 IT 走服務帳號流程
的使用者。19 個工具全通。

1. 開啟 [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   （先建立或選一個專案）。
2. 左側「API 和服務 → 程式庫」，啟用這八個 API：Gmail、Calendar、Sheets、
   Drive、Docs、Slides、Forms、Tasks。沒啟用的話對應工具會回 `403`。也可以
   用一行指令：

   ```bash
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com \
     sheets.googleapis.com drive.googleapis.com docs.googleapis.com \
     slides.googleapis.com forms.googleapis.com tasks.googleapis.com \
     --project=PROJECT_ID
   ```
3. 「API 和服務 → OAuth 同意畫面」設定同意畫面（外部或內部皆可）。若應用
   程式停在「測試中」狀態，記得把自己的 Google 帳號加進「測試使用者」
   名單，否則授權會被 Google 直接擋下——而且測試中狀態核發的 refresh
   token 只有 **7 天效期**，過期要重新走一次連線。
4. 「憑證 → 建立憑證 → OAuth 用戶端 ID」，類型選**網頁應用程式**。
5. 「已授權的重新導向 URI」加入：

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

   `18789` 是 gateway 預設埠；若用 `DUDUCLAW_PORT` 改了埠，dashboard 這頁
   會顯示改埠後的正確網址，以畫面顯示為準——這裡埠號不一致是**沉默**失敗：
   Google 會把瀏覽器導到一個沒有東西在聽的埠，畫面停在「尚未連接」，不會
   跳出明顯錯誤訊息。
6. 複製產生的 **Client ID** 與 **Client secret**。
7. 回到 dashboard「整合 → Google」，在上方連線面板貼上 Client ID / Client
   secret，按「連接 Google」。
8. Google 跳出同意視窗，核准後視窗關閉，面板轉為「Google 已連接」，並列出
   已授權的存取範圍。

## 路徑② 服務帳號網域委派（企業 Workspace）

適合誰：Workspace 網域的企業客戶。網域超級管理員授權一次即可，之後每個
使用者都不用再跳一次 Google 同意畫面，也**不需要通過 Google 應用程式
驗證**——這是它相對於路徑①的優勢。19 個工具全通。個人 `@gmail.com`
不屬於任何網域，無法使用這條路。

1. 在你（服務提供者）的 Google Cloud 專案建立一個**服務帳號**，下載其
   JSON 金鑰。記下服務帳號詳細資料頁上顯示的數字**用戶端 ID**（client id，
   跟金鑰檔裡的 `client_email` 不是同一個東西）。
2. 把金鑰檔存到 DuDuClaw 主機並鎖權限：

   ```bash
   mkdir -p ~/.duduclaw/keys && mv ~/Downloads/sa-key.json ~/.duduclaw/keys/google-sa.json
   chmod 600 ~/.duduclaw/keys/google-sa.json
   ```
3. 把上一步的 client id，連同下面這段 scope 清單（dashboard 的憑證卡片上有
   複製按鈕，見下一步），交給客戶的網域**超級管理員**，請對方依序操作：

   **Admin console → 安全性（Security）→ 存取權和資料控管（Access and
   data control）→ API 控管（API controls）→ 管理網域範圍委派（Manage
   Domain Wide Delegation）→ 新增（Add new）** → 貼上 client id → 貼上
   scope 清單（逗號分隔一行）→ 儲存。

   生效通常很快，Google 官方文件說最長可能要 24 小時。網域若開啟「多方
   核准」（2024 年 8 月起可用），需要第二位超級管理員一起簽核，記得提醒
   客戶預留時間。
4. 回到 dashboard「整合 → Google」，下方「憑證方式」卡片切到**服務帳號
   委派**分頁：
   - 填「服務帳號金鑰檔路徑」（相對路徑以 `~/.duduclaw` 為基準，例如
     `keys/google-sa.json`）。
   - 填「要代理的使用者信箱」（要用哪個 Workspace 帳號的身分呼叫 API，
     例如 `boss@customer.com`）。
   - 上方有「交給管理員的 scope 清單」區塊，按「複製」即可直接貼給步驟 3
     的管理員，不用手動謄打。
   - 按「儲存」，再按「測試連線」——這會實際請求一個 token，**綠燈才代表
     管理員的授權真的已經生效**；只設定不代表已授權成功。

## 路徑③ Apps Script 橋接（個人，完全不碰 Cloud Console）

適合誰：個人 `@gmail.com`（Workspace 帳號也能用，除非管理員停用了 Apps
Script），不想申請 OAuth client 也不想麻煩 IT。**涵蓋範圍是子集**：僅
Gmail（搜尋／讀取／建草稿）、行事曆（列出／建立）、Sheets（讀取／新增
一列）——Drive、Docs、Slides、Forms、Tasks 這條路不支援，呼叫會回明確的
「Apps Script 橋接不支援」錯誤，不是靜默回空結果。

1. 開啟 <https://script.google.com>，建立新專案。
2. 用 [`templates/apps-script/duduclaw-bridge.gs`](../../templates/apps-script/duduclaw-bridge.gs)
   的內容整個取代預設檔案內容。
3. 產生一組亂數密鑰，貼到腳本裡取代 `CHANGE_ME_TO_A_LONG_RANDOM_STRING`：

   ```bash
   openssl rand -base64 32
   ```
4. **部署 → 新增部署作業 → 網頁應用程式**：
   - 執行身分：**我**
   - 誰可以存取：**所有人**
5. Google 會顯示「未驗證的應用程式」畫面——這是預期行為（未驗證的應用
   程式就是你自己的腳本）。選「進階 → 前往（專案名稱）」並同意授權。
6. 複製 `/exec` 結尾的網址（不是 `/dev`，那個只授權腳本擁有者自己瀏覽器的
   session，DuDuClaw 呼叫不了）。
7. 回到 dashboard「整合 → Google」，下方「憑證方式」卡片切到 **Apps
   Script 橋接**分頁：
   - 貼上網頁應用程式網址（`/exec` 結尾）。
   - 貼上步驟 3 產生的密鑰（之後編輯只改網址、密鑰欄位留白，會沿用已存
     的密鑰，不會清掉）。
   - 按「儲存」，再按「測試連線」——綠燈會直接顯示腳本實際執行的 Google
     帳號，是抓「部署在錯的登入身分下」這種錯誤最快的方式。

## 疑難排解

三條路各自的錯誤訊息、re-auth 流程、已知限制都在對應的深入文件裡：
- OAuth client 路徑的 `401`／`403`／redirect URI 不符 → [google-workspace.md#troubleshooting](google-workspace.md#troubleshooting)
- 服務帳號 `unauthorized_client`（多半是管理員貼的 scope 清單少一個）、
  Apps Script 網址／密鑰驗證規則 → [google-no-oauth-client.md](google-no-oauth-client.md)

`google_status` MCP 工具隨時可用，會回報目前生效的憑證來源與授權範圍，
是最快的第一步診斷。
