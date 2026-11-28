# Enterprise 管理員的個人 Google 連線指南

需要讓每個人決定哪些 AI 員工可使用自己的 Google 帳號時，使用本指南。帳號本人必須親自連線並同意。公司共用連線、OAuth 用戶端或網域委派，先看[連接 Google](google-setup.md)。

## 準備安裝環境

1. 在「管理 → 整合 → Google」設定 Google 應用程式，開啟 Google Workspace 整合。個人連線不會自行開啟全機總開關。
2. 啟用「安全性 → 憑證保護」。個人連線需要支援的 macOS 或 Windows 鑰匙圈、[Docker person-credentials 疊加設定](docker.md#74-個人帳號person-credentials-疊加設定)，或[原生 Linux 受保護系統服務](linux-protected-service.md)。一般 Docker 與 Linux 使用者服務仍不支援。只有密碼保護、鑰匙圈或獨立金鑰不可用、或憑證讀不到，都會拒絕使用。
3. 把「我的連線」顯示的完整返回網址註冊到 Google 用戶端。個人返回路徑是 `/api/me/google/callback`，共用連線精靈則使用 `/api/mcp/oauth/callback`，請依實際流程註冊。核准的 HTTPS 儀表板使用該站網址，其他來源使用本機 HTTP，並提供返回網址貼上欄位。主機、埠、路徑必須完全相符，不要為了排除錯誤而公開本機回呼。
4. 依[設定指南](google-setup.md)檢查帳號和應用程式限制。External＋Testing 每七天需要重新授權；正式使用受限制的 Gmail 權限前，可能還需要 Google 驗證與安全評估。
5. 閱讀 Google 資料使用說明，核對 AI 供應商條款。Google Workspace 資料不得用來訓練或改善通用模型。無法確認供應商符合規定時，改用適當的本機模型，或維持 Google 未連線。

請使用專用 Google 連線畫面，自訂 OAuth 或遠端伺服器設定不能取代資料使用說明。由於目前無法保證訓練資料排除了 Google Workspace 資料，這個安裝的資料集建立、匯出與新微調工作都會拒絕。既有工作仍可查看或取消，資料集可刪除。舊版連線須重新接受目前的資料說明，才能使用或更新憑證。

## 請帳號本人連線

1. 請本人登入 Enterprise 儀表板，開啟「我的連線」。畫面要求重新登入時，先完成登入。
2. 本人閱讀並同意資料說明，按「連結我的 Google」，在 Google 視窗選擇要授予的服務。沒同意的服務不能使用。
3. 返回「我的連線」。本機返回頁開不了時，把完整網址列內容貼到原儀表板的返回網址欄位，不要傳給別人。完成連線需使用啟動時的同一人、同一次登入，且在十分鐘內；過期或被取代的流程要重新開始。
4. 核對連上的 Google 信箱，可能和 DuDuClaw 登入信箱不同。
5. 本人逐一允許或拒絕員工。管理員選定帳號，不代表本人已經同意。

## 另外開啟會議錄影匯入

帳號本人到「我的連線」開啟會議錄影存取，完成獨立的同意步驟。一般初次連結個人帳號不會要求 Drive 或 Meet 權限；只有啟用這項功能，才在既有 Google 授權上加上 `https://www.googleapis.com/auth/drive.file` 與 `https://www.googleapis.com/auth/meetings.space.readonly`。拒絕其中一項就不能匯入，既有較廣的授權也不會自動撤銷。

需要既有員工 Drive 工具或 Google 遠端 MCP 時，帳號本人可在完成連線後，另外於「我的連線」選擇「啟用員工的 Drive 工具」。這會要求 `https://www.googleapis.com/auth/drive.readonly`，允許唯讀存取該 Google 帳號可存取的所有 Drive 檔案。這是獨立的同意步驟，不是錄影匯入的必要條件；員工仍受本人使用同意與角色權限限制。每次請完成一種額外授權流程。

Gateway 分別保存基本服務、會議錄影與員工 Drive 工具的本機用途同意，並綁定封存憑證及其授權紀錄。每項能力都必須同時符合 Google 實際範圍與本機用途同意；會議授權額外回傳 `drive.readonly` 也不會啟用員工 Drive 工具。一般憑證若含完整 `drive` 範圍，會被拒絕並要求重新連結。新增服務同意使用 `include_granted_scopes=false`，明列已同意的一般服務與新增服務，Google 可能再次顯示這些權限。新增授權保留先前用途，也須完整保留舊有 Google 範圍，保存時也會比對原憑證版本；缺少範圍或憑證已被其他操作更新，就拒絕取代舊憑證。沒有本機用途同意的舊紀錄須重新連結。重新連結基本服務會將本機用途重設為基本服務，之後須另行開啟會議錄影與員工 Drive 工具。

`drive.file` 搭配 Google Picker 可選取不是由應用程式建立的既有錄影，因此匯入不要求 `drive.readonly`。Google 的 `drive.file` 權限也允許建立與修改選取／分享的檔案，但 DuDuClaw 匯入只執行讀取。DuDuClaw 內的搜尋只列出已分享給應用程式的音訊／影片，其他檔案（包括尚未分享的近期 Meet 錄影）可透過 Google Picker 選取。詳見 [Google 權限範圍說明](https://developers.google.com/workspace/drive/api/guides/api-specific-auth)與 [Picker 設定](https://developers.google.com/workspace/drive/picker/guides/web-picker)。

管理員須在 OAuth 應用程式所屬 Google Cloud 專案啟用 **Google Drive API**、**Google Meet API** 與 **Google Picker API**。使用網頁應用程式 OAuth 用戶端，把控制台來源加入允許的 JavaScript 來源，並保留前述個人連線的完整 callback URI。在 `config.toml` 設定瀏覽器金鑰與數字專案編號：

```toml
[integrations.google_meeting_picker]
developer_key = "your-browser-restricted-google-picker-key"
app_id = "123456789012" # Google Cloud project number, not project ID
```

金鑰與專案編號是會送至瀏覽器的公開設定，不是 OAuth 用戶端秘密；必須與 Google OAuth 用戶端使用同一專案。將金鑰限制為 Google Picker API 與必要的網站來源；Google 目前的 Picker 指南也要求允許 `https://docs.google.com/*`，因為 Picker 在該來源的 iframe 中執行。不要填入伺服器或轉錄服務的 API 金鑰。缺少或無效的設定會使 Picker 維持不可用。

Picker 透過 Google Identity Services 要求僅含 `drive.file`、`include_granted_scopes=false` 的短期瀏覽器權杖，可能再次請本人確認。這個權杖只用於選檔流程，不寫入瀏覽器儲存；gateway 保存的存取／更新權杖不會送到瀏覽器。匯入使用目前登入者在 gateway 的個人連線，在 Google 視窗切換帳號不會讓 gateway 改用另一個人的帳號。

本人在「會議逐字稿」選取檔案，再確認傳給指定員工與設定的轉錄供應商；這不會授予員工瀏覽 Drive 的能力。Meet 參照須符合 Google 本身的存取規則，錄影檔可能仍需另外分享。詳見 [Google 會議產物存取說明](https://developers.google.com/workspace/meet/api/guides/artifacts)與[會議逐字稿](../../features/zh-TW/73-meeting-transcripts.md)。

每次匯入都需要伺服器簽發的確認收據。它證明經 JWT 驗證的控制台使用者，在最近五分鐘內明確登記並確認將這個確切的 Drive 檔案 ID 交給這位接收員工；員工／MCP 權杖不能登記收據。收據限用一次，綁定該使用者、員工與檔案 ID；同一使用者對同一員工的新確認會取代上一張收據。控制台只在使用者確認匯入時登記檔案。缺少、過期、已用過或綁定不符的收據會遭拒。進入匯入程序後，即使後續檢查失敗也會用掉收據，重試須重新確認。伺服器另以該使用者的 Google 連線檢查 Drive 中繼資料，要求 `isAppAuthorized=true`。收據與這個 Google 旗標都不能證明曾在 Google Picker 點擊；控制台呼叫者可用已知 ID 明確確認先前已授權給應用程式的檔案。

僅供控制台使用的 `POST /api/meeting-drive/selection` 必須提供 `agent_id`、`file_id` 與 `confirm: true`，回傳 `selection_receipt` 及 `expires_in`。每位使用者每分鐘最多登記請求 20 次，最多保留八張未過期收據；`POST /api/meeting-drive/import` 必須附上該收據。

`google_status` 只讀取帳號、狀態與依本機用途篩選的範圍中繼資料，不解封憑證、不更新權杖，也不回傳 bearer。Token 端點使用明確的工具允許清單，個人憑證須同時符合對應本機用途與 Google 範圍；狀態查詢與未知工具不能取得 bearer。

本機 Drive 來源紀錄含選取帳號、Google 信箱、檔案 ID 與版本。取消、失敗及上傳過期時，來源紀錄會與對應狀態變更在同一資料庫交易內刪除。其餘來源紀錄（含成功匯入）自 `imported_at` 起保留 30 天，期限到後由 gateway 執行期間每 60 秒的既有清理程序刪除；停機後於下一輪清理執行。逐字稿檔案與稽核日誌有各自的保存規則，不會隨這項來源中繼資料清理一併刪除。

`user.google_connect` 稽核記錄授權用途、回傳範圍集合的雜湊摘要及非預期範圍；Drive／Meet 清單讀取與早期拒絕也會記錄。這些事件不保存權杖與檔案內容，匯入識別碼使用雜湊。

中斷個人 Google 連線後，本機會拒絕後續匯入存取。要撤銷新增的 Google 授權，請到 Google 已連結應用程式檢查，再以需要的服務重新連線。匯入時持續檢查本機連線與實際授權範圍；Google 端撤銷會在 API 拒絕存取，或更新後的權限反映變更時生效。斷線不會撤回已匯入副本、逐字稿或已送至轉錄供應商的資料。OAuth、Picker 與供應商流程仍須用真實帳號驗收。

## 選擇員工與分享範圍

1. 開啟員工的「連線」分頁，選「跟隨主要負責人」或「指定同事」。對方必須仍有效，並直接綁定為這位員工的 Owner。
2. 有多位 Owner 且尚未確定主要負責人時，明確選定一位。新增第二位 Owner 會保留原主要負責人；移除或降級主要負責人後會取消指定，只剩一位 Owner 時自動採用該人。
3. 更換主要負責人前先確認影響：員工的瀏覽器工作階段會停止，已存登入會清除。清理失敗就保留原負責人；換回原人也不會還原登入。升級後部分舊登入也需要重新登入。
4. 設定誰能要求員工使用帳號。本人同意前可查看同事、頻道及分享規則。
5. 除非預期在個人帳號不可用時改用公司資料，且已向本人說明，否則保持公司備援關閉。

| 發問者規則 | 誰可以要求使用 |
|---|---|
| `bound_people` | 綁定這位員工的人，仍受原有存取規則限制 |
| `owner_only` | 選定的帳號本人；排程工作仍須通過員工、帳號與同意檢查 |
| `anyone` | 須由管理員明確確認；公開網站訪客仍會拒絕 |

直接綁定的 Owner 可以選「使用我的 Google」或停用 Google，可以縮小發問者範圍、關閉公司備援，不能擴大權限或指定別人的帳號。停用時會關閉備援，保留或縮小原分享規則；重新使用自己的帳號仍需確認分享範圍。請從「連線」操作。

只有個人帳號不可用、且管理員已允許時，才會考慮公司備援。發問者被拒、設定錯誤或憑證保護失敗，不會直接改用公司帳號。個人帳號操作開始後再發生錯誤，也不會換成公司資料。核准前請核對卡片上的帳號；公司備援可能明示尚未驗證 Google 信箱。

## 交付前測試

1. 確認全機整合已開啟、選定的人仍是有效 Owner，且本人已同意這位員工使用。
2. 用已知可讀的信件、行事曆或文件，請員工對每項需要的服務各讀一次。分別測試應該允許與應該拒絕的發問者，不必為了測連線建立資料。
3. 公司共用連線另用儀表板測試。`google_status` 可回報帳號與本機授權資訊，不能證明特定服務可讀；整合或員工 Google 身分停用時，這個工具會隱藏。
4. 授權到期或需要新服務時重新連線。每次使用仍會檢查目前帳號、Owner、本人同意及已授予權限。撤回同意會立即拒絕後續呼叫，已送到 Google 的請求無法收回。

員工專用外部 API 金鑰證明的是員工，沒有證明真人發問者。透過這條路徑使用個人帳號，需要管理員明確允許 `requesters = "anyone"`，並符合原本同意與核准要求。HTTP Google MCP 每次操作會檢查實際發問者；個人帳號的 stdio 掛載會拒絕。其他外部整合須另外驗收。

## 中斷、離職與使用紀錄

1. 本人在「我的連線」按「中斷」，或由管理員在「整合 → Google」中斷該人的連線。系統會先停止本機存取與尚未完成的連線，再嘗試向 Google 撤銷。
2. 離職時停權或移除該人。即使憑證清理沒有完成，失效人員仍不能使用。
3. 檢查 Google 是否確認撤銷。撤銷同一份 Google 授權可能影響其他個人或公司連線，因此有些情況只做本機中斷。帳號不明、紀錄讀不到或公司帳號檢查失敗時，也不會自動向 Google 撤銷。要撤銷整份授權時，請到 Google 的已連結應用程式設定確認。
4. 公司網域委派是獨立設定，中斷個人連線不會刪除它。Windows 的個人中斷目前只在本機生效；動態遠端 OAuth 也只做本機中斷，請自行到 Google 撤銷。
5. 可查看最近三十天最多 500 筆保留紀錄，只列時間、員工、工具與成功狀態，不含參數、結果或信件內容。輪替可能移除較舊紀錄。能看個人來信不代表可以寄信或核准外寄。

## Google 撤銷結果不明時

撤銷送出後逾時、程序中斷或網路失敗，都可能無法確認結果。只有 Google 當次立即回覆 HTTP 200 才算確認。系統會維持本機存取停止，並在不確定紀錄存在時阻擋新的 Google 或遠端連線。這個阻擋不會自動到期，也沒有能安全清除它的線上按鈕。重啟、換 Client ID 或看到 Google 已連結清單空白，都不能證明原請求已結束。

1. 維持新連線停用，保存事件紀錄、操作代號、稽核資料與 `<home>/locks/google-grant-revoke-uncertain.json`。
2. 在 Google 撤銷舊應用程式授權，並從外部取得原請求已結束的確認。無法確認就保留阻擋、聯絡 Google，不能自行猜等待時間。
3. 停止使用這個安裝的所有 gateway 與 CLI 程序。只有取得外部確認後，離線操作人員才能把不確定紀錄封存到作用中檔名之外；保留操作代號與理由，不要刪其他憑證或鎖定檔。
4. 重啟後請本人重新連線。註記事件是由操作人員根據外部證據解除阻擋，產品本身無法證明不會再有延遲的供應商請求。

不同 Client ID 仍可屬於同一 Google Cloud 專案，所以換 ID 不能當復原證據。升級後先重啟所有 gateway 與 CLI，再使用這些控制。系統鑰匙圈操作可能在畫面逾時後才結束；取消也不能復原已送出的 Google 變更。寫入結果不明（含 `result_unconfirmed`）時，先到 Google 檢查再決定是否重試。

## 管理員設定與限制

```toml
# config.toml
[integrations.google_identity]
allowed_domains = ["example.com"]
allowed_subject_domains = ["example.com"]

# agent.toml
[google]
identity = "owner"          # or "user:<direct-owner-id>"
fallback = "none"           # explicit company fallback: "home"
requesters = "bound_people" # or "owner_only"; "anyone" needs confirmation
```

`allowed_domains` 限制個人 OAuth 帳號網域，空清單允許任何已驗證的 Google 帳號，包含 Gmail。優先採用 Google 回傳的組織網域，否則看信箱網域；這項檢查不能單獨證明 Workspace 成員身分。`allowed_subject_domains` 限制委派同事地址，空清單規則不同，見 [Workspace 安全模型](google-workspace.md)。設定無效時拒絕存取。

備份或搬遷時，請保留支援的系統鑰匙圈；使用個人連線期間不要降版。憑證雖有磁碟保護，與 gateway 使用同一作業系統帳號、又有不受限 Bash 的員工仍可繞過產品控制或修改本機資料，請只給必要能力。Google 的 Gmail 草稿權限也能透過 Google API 寄信，DuDuClaw 原生草稿工具則不會寄出。

明確選定的公司憑證路徑優先，失敗會回報；舊安裝在管理員儲存路徑前保留舊來源判定。Apps Script 涵蓋 Gmail、Calendar、Sheets，七個資料動作加狀態共八個動作，不支援 CC、活動參與者、Meet 或遠端 MCP bearer。選定帳號不代表已通過健康檢查；可能已送到 Google 的操作不能自動換路徑再送一次。

從產品外更換憑證不一定能立即偵測。逾時和取消也不能強制中斷已開始的鑰匙圈或本機磁碟操作；檢查設定到送出請求之間的變更仍有限制。


## 公司授權與身分選項（v1.85）

網域委派每次只要求工具所屬服務的權限：Gmail、日曆、文件、試算表、雲端硬碟、簡報、表單或工作清單。權杖依服務帳號、代表信箱及排序去重後的權限集合分開快取；其他服務缺少授權，不會阻止原本已授權的服務。測試連線會逐項列出 scope 已授權或尚未確認，再另外檢查各服務。取得權杖不代表資料 API 或範例檔案一定可讀。

開啟 `[google] chat_read = true` 後，公司授權也支援 `chat.spaces.readonly` 與 `chat.messages.readonly`（完整前綴都是 `https://www.googleapis.com/auth/`）。請在 Google 管理控制台加入這兩項範圍，並啟用 Google Chat API。設定預設仍為關閉；整合精靈預先勾選 Chat 讀取，管理員按儲存後才寫入。已連結但缺少任一 Chat 權限或本地用途的成員，會在「我的連線」看到「重新連結以加入 Google Chat」。傳送另須開放 `chat_send`，並由本人逐次確認，詳見下方說明。

讓公司授權跟隨 AI 員工的負責人：

1. 將該成員綁定為 Owner；若有多位 Owner，先在「員工 → 連線」指定主要負責人。
2. 選擇「公司授權：跟隨負責人」（`identity = "delegate:owner"`），並依前述步驟設定允許網域、匯入封存的服務帳號金鑰。

   權限變更後，正在進行中的那一次呼叫仍可能以原負責人身分完成；之後的呼叫都會重新檢查。
3. 到「整合 → Google → 公司授權代表」，逐列核對 AI 員工、目前負責人、登入信箱與認證狀態，再按確認信箱。一般模式也可操作，但必須是有效管理員，並完成既有的密碼、驗證器二次驗證與工作階段 CSRF 檢查。若信箱是成員自行修改，管理員還須勾選已另外查證的確認項目。
4. 測試連線。每次呼叫都由 gateway 從目前主要負責人與登入帳號算出代表信箱，不採用工具參數。沒有認證、更換負責人（含換回原人）、變更信箱、負責人停權或資料讀取失敗，都會拒絕使用並要求管理員處理。`requesters` 預設 `owner_only`，排程沿用既有發問者規則；此身分不允許 `fallback = "home"`。受保護模式仍保留更嚴格的租戶路由與發問者限制。

認證使用 `google.delegation.attest_subject {agent_id, subject, user_id, acknowledge_self_set?, csrf_token}`。伺服器會核對畫面上的成員 ID、信箱及目前負責人，稽核記錄操作者、受認證成員與信箱。公司授權不會替本人寫入個人 OAuth 同意。

`owner` 與 `user:<id>` 現在也有管理員「測試連線」按鈕（`google.identity.test {agent_id}`）：只檢查連線紀錄、本人同意、權限與用途涵蓋的工具、OAuth refresh，以及 tokeninfo 的權限比對，不呼叫 Gmail、Drive、Calendar 或其他資料端點。測試會留下操作者與受測成員的稽核紀錄，並透過該成員已驗證的通知目的地通知本人；測試不代表本人同意，也不會自行授權 AI 使用。

Google 由另外管理的外部工具提供時，可設定：

```toml
[google]
identity = "external"
external_server = "合作夥伴 Google MCP" # 選填，最多 120 字
```

連線分頁會顯示「由外部工具提供」。原生 Google 工具和 `none` 一樣不列出、不能呼叫；這個選項不會自動設定或授權外部伺服器。名稱只作文字顯示，不接受控制字元。舊版解析器遇到未知身分或欄位會拒絕原生 Google 存取，不會退回公司共用帳號。

## 從聊天確認 Google 授權

已驗證的成員請 AI 員工使用 Google 服務，而本人連線或用途尚未開放時，閘道可以提供十分鐘有效的授權連結。未綁定、身分衝突或只有顯示名稱的通道帳號不會取得連結。**這不是登入憑證，也不是 SSO 連結**：請用同一個成員帳號登入 Dashboard，閱讀資料說明與員工共用提醒，再按「確認並繼續」。單純開啟頁面或聊天預覽不會消耗請求，也不會開始 OAuth。

個人 OAuth 必須使用頁面指定的 Google 帳號。系統需有既有 Google 身分或管理員信箱認證，不能只相信成員自行填寫的登入信箱；缺少依據時，請先到「我的連線」完成設定。即使 Google 允許登入，選錯 Google 帳號也不會保存連線。公司授權代表不另做 OAuth，頁面會列出代表帳號、員工與服務權限；管理員的全網域委派與信箱認證仍須有效，本人按確認不能取代這兩項設定。

外部通道的連結只使用管理員設定的 `[dashboard] public_url`，必須是 HTTPS、非本機回送位址，並保留子路徑。沒有設定時只提示開啟 **Dashboard → 我的連線**，不提供 localhost 連結，也不要求貼回授權碼。WebChat 可使用同源頁面。Telegram、Slack、Discord、LINE 可顯示開啟網址按鈕；Google Chat 使用網址，卡片點擊不算本人確認。

確認完成後，系統重新檢查成員、通道綁定、員工權限、原對話、Google 帳號、用途及設定，再續行原本工作。這不代表核准寄信、傳訊息或其他對外操作。可到 **我的連線 → 聊天授權請求** 查看進度。等待續行的工作可在閘道重啟後繼續；已開始的工作若中斷，結果視為未確認，不會自動重做，請回原對話重新提出要求。尚未完成的 OAuth 流程若遇到重啟，需重新開始。

## 以本人身分傳送 Google Chat

`google_chat_send {space, text, thread?}` 使用提出要求的已驗證成員本人的 Google 帳號，傳送文字到既有空間。需要 `google:write`；管理員須先啟用 `[google] chat_send = true`，預設關閉。不接受指定寄件者，也不會改用公司共用帳號、其他成員、Apps Script 或 Chat app 的 `chat.bot` 權杖。

個人 OAuth 請先連結帳號，再到「我的連線 → Google Chat 傳送」開放獨立的 `chat_send` 用途。新增服務同意會明列已同意的一般服務，再加上 `https://www.googleapis.com/auth/chat.messages.create`，不要求 Google 自動合併先前授權；必須維持相同 Google `sub` 且實際取得權限。公司全網域委派也使用獨立的傳送範圍，仍須 Workspace 管理員授權；不會把傳送範圍加入每項 Google 工具。

每次都由本人確認，五分鐘內有效、只能使用一次。確認卡顯示帳號、空間資源名稱、討論串、完整最終文字及私人資料外傳提醒。短文可在已驗證私訊確認；群組要求與長文改到本人限定的「我的連線」頁面確認。管理員核准、`allow` 規則、Admin MCP scope、收件人白名單都不能代替本人確認；組織要求的外傳核准仍另外執行。文字、目的地、帳號、憑證或政策變更後，須重新確認。

Gateway 保存單次操作的固定 request ID。結果不明會回報 `result_unconfirmed`，不會自動重送；請先檢查 Google Chat，再決定是否提出新的傳送要求。第一版不支援附件或建立空間；指定的討論串必須屬於同一空間。Google 顯示本人為寄件者，同時附上應用程式名稱。[Google Chat API 說明](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create)。

## 另外連結雲端硬碟寫入

AI 員工使用你的 Google 連線時，寫入需要第二次 Google 同意，授予 Drive、Docs 與 Sheets 寫入權限，憑證會與一般連線分開保存。請使用同一個 Google 帳號。管理員須先開放所需動作，個人同意不會自行開啟功能。在「整合 → Google → 雲端硬碟寫入」設定四種動作、各動作的核准模式與資料夾清單。就緒檢查列出委派權限範圍、個別員工測試與公司帳號同意。成員在「我的連線 → 雲端硬碟寫入」連結並處理寫入確認卡；紀錄在「記錄 → 歷史」用「雲端硬碟寫入」快速篩選。

建立、修改、搬移與移到垃圾桶預設全關。管理員開啟或放寬存取前須確認了解風險，系統會記錄身分與時間，不要求額外重新驗證。每項動作預設由成員本人確認；管理員可逐項關閉確認，或加上管理員核准。資料夾清單為空時，建立與修改可在帳號能寫入的任何位置執行，搬移與移到垃圾桶不能使用。詳見[寫入工具與資料夾規則](google-mcp.md#雲端硬碟寫入工具)。

需要確認時，只有該成員能在五分鐘內核准或拒絕。使用你的個人連線時，請求者也須是你本人。決定前請檢查 Google 帳號、動作、目的位置與預計寫入的內容；Admin 權杖與工具允許清單不能代你確認。內容、憑證、帳號或規則改變，就需要重新確認。無人值守與背景工作無法等待確認，因此不會寫入；要讓排程寫入，管理員須先關閉該動作的確認要求，帳號與存取檢查仍會照常執行。

移除寫入連線只會刪除 DuDuClaw 本機的寫入憑證，一般連線仍保留在本機，也不會撤銷 Google 授權。若要完整撤銷，請到 [Google 帳戶的第三方存取頁面](https://myaccount.google.com/connections)移除這個應用程式的存取權。這也會影響同一應用程式的一般 Google 連線，之後請只勾選需要的服務重新連結。Google 端撤銷與權杖隔離仍待實際帳號驗收。移除連線不會復原已完成的寫入。

一般同意流程不再要求 Google 自動合併先前授予的權限。DuDuClaw 會明列已同意的一般服務，加上這次新增的服務，因此 Google 可能再次列出所有這些權限。一般連線若含完整 Drive 權限，系統會拒絕使用並要求重新連結；獨立寫入憑證不會交給員工 CLI 或官方遠端 MCP。
