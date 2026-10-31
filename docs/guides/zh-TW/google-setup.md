# 連接 Google

依照這份指南和設定精靈，選擇一種方式連接你的 Google 帳號。

| 你的情況 | 走哪條 | 主要限制 |
|---|---|---|
| 公司 Workspace，有超級管理員 | 服務帳號＋網域委派 | 個人 Gmail 不適用 |
| 公司 Workspace，使用自己的 OAuth | OAuth，Internal | 只限組織成員，沒有 Testing（測試模式）的七天到期限制 |
| 個人 Gmail，只需信件、行事曆、試算表 | Apps Script 橋接 | 三項服務，含狀態查詢共八個動作 |
| 個人 Gmail，需要完整 Google 工具 | OAuth，External＋Testing | Testing 期間每七天重新授權 |

## 從 DuDuClaw 開始

1. 以管理員登入，開啟「管理 → 整合 → Google」（`/manage/integrations?tab=google`），按「設定 Google 連線」。原本的表單可從「進階設定」開啟。
2. 選擇上表的情況。OAuth 路徑會顯示資料使用說明，請閱讀並確認。密鑰只填在儀表板，不要貼到與 AI 員工的對話。
3. 完成下列一種路徑。儲存服務帳號或 Apps Script 憑證，或完成 OAuth 授權後，整合會自動啟用。切換成 OAuth 時，新的授權成功前仍使用目前的連線；取消或拒絕會保留原連線。

## OAuth

1. 開啟 [Google Cloud Console](https://console.cloud.google.com/)，用頂端的專案選單建立或選擇專案。
2. 進入「APIs & services → Library」，逐一搜尋並啟用 Gmail API、Google Calendar API、Google Sheets API、Google Drive API、Google Docs API、Google Slides API、Google Forms API、Tasks API。
3. 進入「Google Auth platform → Branding → Get Started」，依序填完 App Information、Audience、Contact Information、Finish。公司組織成員選 Internal；個人 Gmail 選 External，保持 Testing（測試模式，只讓名單中的帳號使用），不要按 Publish app 發布。
4. External 應用程式處於 Testing 時，在「Audience → Add users」加入每個要連線的帳號。最多可列出 100 位測試使用者，授權和更新權杖七天後到期。在精靈勾選仍處於 Testing，連線超過六天會顯示到期提醒。
5. 進入「Data Access → Add or Remove Scopes」，把精靈提供的完整權限範圍清單貼到「Manually add scopes」，加入並儲存。本文末尾也有同一份清單。
6. 進入「Clients → Create Client」，選 Web application。Web 和 Desktop 兩種用戶端都有可選的「Use this client for an AI-powered agent」勾選框。這個儀表板流程使用 Web 用戶端。
7. 把 DuDuClaw 顯示的重新導向網址完整複製到 Authorized redirect URIs，以畫面上的埠號為準。原生安裝預設 18889，隨附 Docker Compose 預設 18789。localhost 以外需要 HTTPS，不能用原始 IP 或萬用字元。主機、埠、路徑或結尾斜線不同都可能失敗。
8. 儲存用戶端後，立即複製 Client ID 並下載 Client secret。密鑰只顯示一次，之後主控台只顯示末四碼，請保存到密碼管理器。
9. 回到精靈填入 ID 與密鑰，確認資料使用說明，再按「連接 Google」。Google 若顯示「Google hasn't verified this app」，確認是自己建立的專案，選 Advanced，再選 Go to（專案名稱）。在同意畫面勾選每一項權限並繼續，等待儀表板結果。

### 遠端主機或 Docker

localhost 指的是執行瀏覽器的電腦。本機 Docker 安裝不需要 SSH 通道。只有 DuDuClaw 在另一台機器上時，才需要先轉送遠端連接埠。以下以畫面顯示 18889 為例：

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

遠端安裝請保持 SSH 連線直到授權完成。若畫面顯示 18789 或其他埠號，請將指令中的兩個 18889 都改成該埠號，並登記畫面上的完整網址。隨附 Docker Compose 會將 18789 發布到所有網路介面，沒有預先限制為本機連線。

### 權限與期限

DuDuClaw 要求的權限範圍中，`gmail.readonly`、`gmail.compose` 和 `drive.readonly` 屬於 Google 的 restricted（受限）類別，完整清單見本文末尾。[Gmail 分類](https://developers.google.com/workspace/gmail/api/auth/scopes)與 [Drive 分類](https://developers.google.com/workspace/drive/api/guides/api-specific-auth)說明驗證要求。個人使用者請保持 External＋Testing，不要發布應用程式；公開提供這些權限可能需要 Google 驗證，以及伺服器資料處理的安全評估。Internal 只限組織成員。

未驗證應用程式畫面的上限是專案終生 100 位新使用者，不能重設。這和 Testing 的 100 位名單是兩種限制。更新權杖閒置六個月也會失效；用戶端六個月沒使用會自動刪除，Google 會提前 30 天寄警告信。有存更新權杖，不代表目前還能連線。

## 服務帳號與網域委派

1. 請 Workspace 超級管理員同意委派。核准的服務帳號可在允許的權限內代表網域使用者。個人 @gmail.com 帳號不能使用。
2. 請持有 Google Cloud 專案的人在該專案啟用 OAuth 步驟列出的八項 API。進入「IAM & Admin → Service Accounts」建立服務帳號，再開啟該帳號，選「Keys → Add key → Create new key → JSON」並下載。數字 Client ID 就是 JSON 裡的 `client_id` 欄位。
3. 把 Client ID 和儀表板完整權限範圍清單交給超級管理員。管理員進入「Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new」，貼上兩者並授權。Google 最多需要 24 小時傳播設定。
4. 全公司共用一個帳號時，在精靈選「公司 Workspace，有超級管理員」。將 JSON 放在 DuDuClaw 主機，只開放檔案擁有者讀取，填入絕對路徑，例如 DuDuClaw 家目錄下的 `/Users/alex/.duduclaw/keys/google-sa.json`，請換成主機上的實際路徑。不要用 `~`，程式不會展開它。填入要代表的 Workspace 使用者信箱後儲存，請填真人帳號。
5. 每位 AI 員工要代表不同同事時，先到「安全 → 憑證保護 → 改放系統金鑰圈」，再到「整合 → Google → 進階設定 → 公司授權代表 → 匯入金鑰檔」。這條獨立流程會保護金鑰，需要支援的 macOS 或 Windows 鑰匙圈；Linux 和 Docker 尚不能使用。設定允許網域，再於員工的連線分頁選同事。詳見[帳號安全與設定](google-workspace.md#每位-ai-員工用不同的-google-帳號公司授權代表v1760)。
6. 執行服務檢查，確認選定帳號及各服務的結果。取得授權不代表已成功讀取資料。

公司授權代表的設定可限制同事信箱的網域；Enterprise 個人連線則有獨立的帳號網域限制。設定方式見 [Google Workspace 參考](google-workspace.md)。

## Apps Script 橋接

1. 用要讓員工操作的 Google 帳號開啟 [Apps Script](https://script.google.com/)，建立專案。
2. 貼上專案的[橋接範本](../../../templates/apps-script/duduclaw-bridge.gs)。用密碼管理器產生至少 30 個字元的隨機密碼，或執行 `openssl rand -base64 32`，取代 `CHANGE_ME_TO_A_LONG_RANDOM_STRING`。
3. 選「Deploy → New deployment → Web app」，Execute as 選 Me，Who has access 選 Anyone。Google 若顯示「Google hasn't verified this app」，確認是自己的專案，選 Advanced，再選 Go to（專案名稱）。在同意畫面勾選每一項權限並繼續。
4. 複製部署的 `/exec` 網址。`/dev` 只供擁有者的瀏覽器工作階段使用。
5. 在精靈選「個人 Gmail，只需信件、行事曆、試算表」，填入網址與密鑰並儲存。已有其他生效路徑時先確認替換。繼續到服務檢查，按「執行服務檢查」，核對回傳帳號。
6. 保密保存網址和密鑰。輪替時修改腳本密鑰，選「Deploy → Manage deployments」，編輯現有部署，選「New version」並部署，再更新儀表板密鑰。這樣網址會保持不變；選 New deployment 會產生新網址。已部署的腳本也要用此方式重新部署，才會套用新版範本的帳號辨識方式，以執行者 Me 的帳號回報。Apps Script 有每日配額，個人帳號尤其有限。

橋接涵蓋 Gmail 搜尋、讀信、草稿；Calendar 列表、建立；Sheets 讀取、附加。加上 `status`，範本共八個動作、三項服務。Drive、Docs、Slides、Forms、Tasks 不支援，也沒有寄信動作。端點可公開連線，靠密鑰限制存取；儀表板會加密保存密鑰。

## 服務檢查與完成

1. 在精靈按「執行服務檢查」，確認顯示的帳號。OAuth 與服務帳號會檢查 Gmail 帳號、Calendar 行事曆清單與 Drive 帳號資訊。畫面會列出已實測的服務、失敗原因及可用的 API 啟用連結。
2. 上述檢查通過，且 Sheets、Docs 所需權限範圍已授予時，按「完成」即可結束設定。完成畫面會顯示成功，並註明 Sheets 與 Docs 已授予權限，但尚未實際讀取文件。這裡不檢查 Slides、Forms 與 Tasks。
3. Apps Script 的服務檢查會確認執行帳號與腳本狀態，不會實際讀取信件或試算表。通過後按「完成」。
4. 確認預定員工看得到 Google 工具。若看不到，開啟 Google 頁面確認整合已啟用，再檢查員工的 Google 連線設定。

## 疑難排解
Google 顯示自己的錯誤頁時，儀表板只會繼續等待，收不到錯誤原因。請關閉 Google 分頁，依下表修正後回儀表板重新開始。已連線時，先開「進階設定」，按「修改憑證」，再按「連接 Google」。

| 結果 | 處理方式 |
|---|---|
| `SERVICE_DISABLED`／`accessNotConfigured` | 開啟畫面的 Google Console 連結，在用戶端專案啟用 API，再執行服務檢查 |
| 權限不足 | 按「修改憑證」，再按「連接 Google」，勾選每一項權限 |
| `access_denied` | 帳號不在測試使用者名單時會出現；到 Audience → Add users 加入實際使用的帳號，再重新開始 |
| `admin_policy_enforced` | 請 Workspace 管理員允許應用程式與所需服務 |
| `invalid_scope` | 核對 Data Access、委派清單和儀表板權限範圍 |
| `needs_reconnect`／更新時 `invalid_grant` | 按「修改憑證」，再按「連接 Google」；可能是 Testing 到期、撤銷或權杖失效 |
| `redirect_uri_mismatch`（Error 400） | 逐字比對 Google 的 Authorized redirect URIs 與 DuDuClaw 顯示的完整網址，包含埠號、路徑與結尾斜線 |
| `org_internal` | 應用程式設為 Internal，但使用了組織外的帳號；改用同一組織的帳號 |
| 等待逾時 | 檢查畫面上的埠號與遠端 SSH 轉送，再關閉 Google 分頁並重新開始 |
| 其他 `403` | 依顯示的原因碼排查，重新授權無法啟用停用中的 API |
| 委派 `unauthorized_client` | 核對數字 Client ID、完整權限與管理員授權 |

請使用本指南的一種連線方式，不需要設定 IMAP／SMTP。

## 要求的權限

`calendar.calendarlist.readonly` 用來檢查行事曆清單。[calendarList.list 權限表](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list)列出它需要的權限範圍，只有 `calendar.events` 不夠。下列清單以逗號分隔，與精靈的複製按鈕相同。

```text
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/calendar.calendarlist.readonly,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

工具行為與安全模型見 [Google Workspace 參考](google-workspace.md)。Enterprise 本人同意見[個人連線](google-personal-connections.md)。Google Chat 頻道使用另一組憑證，見 [Chat 設定](channels-googlechat-teams.md)。
