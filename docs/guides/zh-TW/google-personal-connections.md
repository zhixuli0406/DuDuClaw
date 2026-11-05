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

Gateway 分別保存基本服務、會議錄影與員工 Drive 工具的本機用途同意，並綁定封存憑證及其授權紀錄。每項能力都必須同時符合 Google 實際範圍與本機用途同意；會議授權額外回傳 `drive.readonly` 或 `drive`，也不會啟用員工 Drive 工具。增量授權保留先前用途，但必須完整保留舊有 Google 範圍，保存時也會比對原憑證版本；缺少範圍或憑證已被其他操作更新，就拒絕取代舊憑證。沒有本機用途同意的舊紀錄須重新連結。重新連結基本服務會將本機用途重設為基本服務，之後須另行開啟會議錄影與員工 Drive 工具。

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
