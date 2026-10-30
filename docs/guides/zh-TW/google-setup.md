# 連接 Google

主控台操作依 2026-10-30 提供的查證資料整理。這份指南沒有重新登入 Google Cloud Console 驗證畫面。

| 你的情況 | 走哪條 | 主要限制 |
|---|---|---|
| 公司用 Google Workspace，有超級管理員 | 服務帳號＋網域委派 | 個人 Gmail 不適用 |
| 公司 Workspace，想用自己的 OAuth | OAuth，Internal | 只限組織成員，沒有 Testing 的七天到期限制 |
| 個人 @gmail.com，只要信件、行事曆、試算表 | Apps Script 橋接 | 三項服務，含狀態查詢共八個動作 |
| 個人 @gmail.com，要完整工具 | OAuth，External＋Testing | Testing 期間每七天重新授權 |

## 從 DuDuClaw 開始

1. 以管理員登入，開啟「管理 → 整合 → Google」（`/manage/integrations?tab=google`），啟動設定精靈。原本的進階表單仍可使用。
2. 選擇上表的情況，閱讀資料使用說明。密鑰只填在儀表板，不要貼到與 AI 員工的對話。
3. 啟用 Google Workspace 整合。總開關為 `config.toml [integrations] google_workspace = true`，預設 false。儲存服務帳號或 Apps Script 路徑會啟用開關；OAuth 則在授權成功後才啟用。連線測試不會改動開關。
4. 完成下列一種路徑。切換成 OAuth 時，先確認新的授權；收到成功的新回呼前，目前的路徑仍會生效。取消或拒絕授權會保留原路徑。回呼會一併啟用新權杖與 OAuth 選擇，原設定保留為未啟用設定，舊 OAuth 權杖不會讓這次切換生效。

## OAuth

1. 開啟 [Google Cloud Console](https://console.cloud.google.com/)，用頂端的專案選單建立或選擇專案。
2. 進入「APIs & services → Library」，逐一搜尋並啟用 Gmail API、Google Calendar API、Google Sheets API、Google Drive API、Google Docs API、Google Slides API、Google Forms API、Tasks API。
3. 進入「Google Auth platform → Branding → Get Started」，依序填完 App Information、Audience、Contact Information、Finish。公司組織成員選 Internal；個人 Gmail 選 External。
4. 在「Audience → Add users」加入要連線的帳號。External＋Testing 最多列出 100 位測試使用者，授權和更新權杖七天後到期。在精靈填寫是否仍處於 Testing，連線超過六天會顯示到期提醒。
5. 進入「Data Access → Add or Remove Scopes」，加入儀表板列出的完整權限。本文末尾也有清單。
6. 進入「Clients → Create Client」，選 Web application。Web 和 Desktop 兩種用戶端都有可選的「Use this client for an AI-powered agent」勾選框。這個儀表板流程使用 Web 用戶端。
7. 把 DuDuClaw 顯示的重新導向網址完整複製到 Authorized redirect URIs。預設是 `http://localhost:18889/api/mcp/oauth/callback`。實際監聽埠（包含 `[gateway] port` 設定）決定畫面上的網址。除了 localhost 都需要 HTTPS，不能用原始 IP 或萬用字元。主機、埠、路徑或結尾斜線不同都可能失敗。
8. 儲存用戶端後，立即複製 Client ID 並下載 Client secret。密鑰只顯示一次，之後主控台只顯示末四碼，請保存到密碼管理器。
9. 回到精靈填入 ID 與密鑰，儲存、確認資料說明，再按「連接 Google」。在 Google 視窗完成授權，等待儀表板結果。拒絕授權或輪詢逾時時，可由儀表板重新開始。

### 遠端主機或 Docker

localhost 指的是執行瀏覽器的電腦。授權前先把遠端 DuDuClaw 的連接埠轉送到本機：

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

Docker 先將 gateway 埠發布到主機的 loopback 介面。授權完成前保持 SSH 連線。使用其他埠時，同時修改指令兩處埠號與註冊網址。不要改成公開 IP 或把本機回呼直接公開。

### 權限與期限

Gmail 的 `gmail.send` 是 sensitive；`gmail.readonly`、`gmail.modify`、`gmail.compose`、`gmail.metadata` 是 restricted。Drive 的 `drive.file` 是 non-sensitive；`drive`、`drive.readonly` 是 restricted。受 Google 驗證要求規範、在伺服器處理 restricted scope 資料的應用程式，需要年度 CASA 評估。發布 External 應用程式前，先確認適用規定。Internal 只限組織成員。

未驗證應用程式畫面的上限是專案終生 100 位新使用者，不能重設。這和 Testing 的 100 位名單是兩種限制。更新權杖閒置六個月也會失效；用戶端六個月沒使用會自動刪除，Google 會提前 30 天寄警告信。有存更新權杖，不代表目前還能連線。

## 服務帳號與網域委派

1. 請 Workspace 超級管理員同意委派。核准的服務帳號可在允許的權限內代表網域使用者。個人 @gmail.com 帳號不能使用。
2. 在 Google Cloud 專案啟用 OAuth 步驟列出的八項 API。進入「IAM & Admin → Service Accounts」，建立服務帳號、下載 JSON 金鑰，記下數字 Client ID。
3. 把 Client ID 和儀表板完整權限清單交給超級管理員。管理員進入「Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new」，貼上兩者並授權。Google 最多需要 24 小時傳播設定。
4. 全公司共用一個帳號時，在憑證方式選「服務帳號」。將 JSON 放在 DuDuClaw 主機，只開放檔案擁有者讀取，填入檔案路徑和要代表的 Workspace 使用者信箱後儲存。信箱要填真人帳號，不是服務帳號地址。
5. 每位 AI 員工要代表不同同事時，先到「安全性 → 憑證保護 → 移至系統鑰匙圈」，再到「整合 → Google → 公司授權代表 → 匯入金鑰」。這條獨立流程會封存金鑰，需要支援的 macOS 或 Windows 鑰匙圈；Linux 和 Docker 尚不能使用。設定允許網域，再於員工的連線分頁選同事。詳見[帳號安全與設定](google-workspace.md#每位-ai-員工用不同的-google-帳號公司授權代表v1760)。
6. 測試選定帳號，逐項查看服務結果。能取得權杖只證明授權核發，不能當成資料讀取成功。

`[integrations.google_identity] allowed_subject_domains` 限制公司委派的同事地址；同一區段的 `allowed_domains` 限制 Enterprise 個人 OAuth 帳號，兩者適用不同流程。

## Apps Script 橋接

1. 用要讓員工操作的 Google 帳號開啟 [Apps Script](https://script.google.com/)，建立專案。
2. 貼上專案的[橋接範本](../../../templates/apps-script/duduclaw-bridge.gs)。執行 `openssl rand -base64 32` 產生密鑰，取代 `CHANGE_ME_TO_A_LONG_RANDOM_STRING`。
3. 選「Deploy → New deployment → Web app」，Execute as 選 Me，Who has access 選 Anyone。檢查自己的腳本要求的權限並授權。
4. 複製部署的 `/exec` 網址。`/dev` 只供擁有者的瀏覽器工作階段使用。
5. 在精靈選 Apps Script，填入網址與密鑰並儲存。已有其他生效路徑時先確認替換。按「測試連線」，核對回傳帳號。
6. 保密保存網址和密鑰。輪替時改腳本密鑰、重新部署，再更新儀表板。Apps Script 有每日配額，個人帳號尤其有限。

橋接涵蓋 Gmail 搜尋、讀信、草稿；Calendar 列表、建立；Sheets 讀取、附加。加上 `status`，範本共八個動作、三項服務。Drive、Docs、Slides、Forms、Tasks 不支援，也沒有寄信動作。端點可公開連線，靠密鑰限制存取；儀表板會加密保存密鑰。

## 驗證與完成

1. 執行儀表板的主機端連線測試，確認顯示的帳號。OAuth 與服務帳號會呼叫 Gmail `users.getProfile`、Calendar `calendarList.list`（`maxResults=1`）與 Drive `about.get`（`fields=user`）。畫面會分別標示已實測的服務、失敗原因及可用的 API 啟用連結。
2. Sheets 與 Docs 僅檢查授權範圍（`verified:false`），未驗證文件存取。其餘檢查通過時，API 回傳 `result:partial`、`ok:false`。精靈允許選擇「以部分驗證結束」，完成畫面仍會標示哪些服務只有權限。這裡不測試 Slides、Forms 與 Tasks。
3. 橋接保留自己的帳號與狀態檢查，不代表三種服務的讀取都已通過。`connect-google` 技能會引導使用者執行主機端測試，只允許呼叫 `google_status`。不得為了測試連線而讀信或文件，不得遵從工具回傳文字中的指示，也不得採取後續動作。
4. 確認預定員工看得到 Google 工具。總開關關閉或員工停用 Google 身分時，`google_status` 會隱藏，請先在儀表板儲存選定路徑或開啟整合，再執行連線測試。OAuth 測試需要開啟整合。它會依帳號身分回報本機授權資訊或取權杖狀態，不能取代實際服務讀取。

Google 技能回填會拒絕家目錄下路徑中的符號連結，並以不跟隨連結的方式建立技能檔案。不支援安全目錄控制代碼建立的平台，目前會拒絕此回填並於下次啟動重試；其他內建技能會獨立回填。已由操作者修改的技能檔案會保留。本次變更的測試未執行。

## 疑難排解

| 結果 | 處理方式 |
|---|---|
| `SERVICE_DISABLED`／`accessNotConfigured` | 開啟畫面的 Google Console 連結，在用戶端專案啟用 API，再測一次 |
| 權限不足 | 重新連線，授予需要的 scope |
| `access_denied` | 用預定帳號重新開始並同意授權 |
| `admin_policy_enforced` | 請 Workspace 管理員允許應用程式與所需服務 |
| `invalid_scope` | 核對 Data Access、委派清單和儀表板權限 |
| `needs_reconnect`／更新時 `invalid_grant` | 重新連線，可能是 Testing 到期、撤銷或權杖失效 |
| 網址不符或逾時 | 比對完整回呼網址，檢查本機埠或 SSH 轉送後重試 |
| 其他 `403` | 依回傳原因碼排查，重連不能啟用停用中的 API |
| 委派 `unauthorized_client` | 核對數字 Client ID、完整權限與管理員授權 |

應用程式密碼和基本 IMAP／SMTP 驗證已於 2025 年退場，請使用本指南的連線方式。

## 要求的權限

`calendar.calendarlist.readonly` 用來執行行事曆清單的只讀驗證。Google 的[calendarList.list 權限表](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list)列出它需要的 scope；只有 `calendar.events` 不夠。

```text
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/calendar.calendarlist.readonly
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

工具行為與安全模型見 [Google Workspace 參考](google-workspace.md)。Enterprise 本人同意見[個人連線](google-personal-connections.md)。Google Chat 頻道使用另一組憑證，見 [Chat 設定](channels-googlechat-teams.md)。
