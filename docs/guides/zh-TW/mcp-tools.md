# MCP 工具：AI 員工看得到哪些、為什麼

DuDuClaw 的 MCP server 透過標準的 `tools/list` 宣告工具。這頁解釋兩件最容易誤解的事：

1. 為什麼 AI 員工看到的工具，比伺服器實作的少；
2. 工具說明被限長之後，原本那些長篇細節去哪了。

要新增工具請先看 [custom-mcp-tool.md](../custom-mcp-tool.md)；這頁講的是宣告面，不是實作。不裝 gateway、單獨使用 MCP server，見 [mcp-standalone.md](mcp-standalone.md)。


## MCP 用戶端協定版本

API／openai-compat 用戶端與遠端 MCP bridge 優先使用 **2026-07-28**：先以
`server/discover` 探測，每次請求攜帶版本、用戶端能力與身分 metadata。HTTP 請求
包含 `MCP-Protocol-Version`、`Mcp-Method`，以及適用時的 `Mcp-Name`。
`x-mcp-header` 標註的工具參數會同步放入標頭；標註不合法的工具不列出。
中文等非 ASCII 標頭值使用協定規定的 Base64 編碼。

舊伺服器回退至 **2025-11-25** 或 **2025-06-18**，使用 `initialize`、
`notifications/initialized` 與協商後的 session 標頭。初次探測收到 `-32022` 時，
選擇雙方共同支援的最新版本；連線建立後 bridge 收到同一錯誤會拒絕當次操作並
停用該連線，須重新啟動對話，重新協商、列舉工具及檢查權限，不會跨版本重送呼叫。
API 用戶端的 `tools/call` 收到 `-32022` 也會終止當次呼叫，並拒絕該用戶端的後續請求；
host 須重新連線、列舉工具及檢查權限。初次握手及獨立列舉工具時的版本恢復仍保留。
明確的新版能力或標頭錯誤不會觸發降版。每條新的 API HTTP 連線都獨立探測，
不使用以網址為鍵的全域版本快取；bridge 只保留自身連線的協商狀態。
憑證、session 與工具清單不會跨身分共用。API 用戶端在舊版 initialize
仍回覆 400／404／405 時，也會嘗試舊式 HTTP+SSE；其 `endpoint` 必須與原設定同源。

API 用戶端的新版 MRTR 每回合使用新 ID、原始工具參數，並原樣回傳伺服器的 `requestState`，
且有回合上限。API 用戶端提供 host input handler 接口；未註冊時不宣告互動能力，
拒絕非預期的輸入要求。本次沒有新增同意介面，也不會自動接受 elicitation。
Tasks、採樣與根目錄能力預設不啟用。遠端 bridge 不宣告新版互動能力，拒絕所有
`input_required` 回覆，包括只有狀態的回覆；下游請求若含 `requestState` 或
`inputResponses`，也會在送出前拒絕。完整續傳資料的外送、審批與原核准呼叫綁定
保護完成前，bridge 不開放 MRTR。工具回應串流中斷會回報錯誤，不會悄悄重送
結果不明的操作。原有工具過濾與第三方工具分類仍然適用。

遠端 OAuth 優先使用已註冊的憑證。管理員如有現成的公開 HTTPS metadata 文件，
可設定 CIMD：

```toml
[remote_mcp]
client_metadata_url = "https://your-domain.example/mcp-client.json"
```

授權伺服器支援 CIMD 時，文件須以該網址為 client ID、允許既定 callback URI，
並使用公開用戶端驗證（`token_endpoint_auth_method = "none"`）。選中的文件驗證
失敗便拒絕連線；未設定或授權伺服器不支援時，仍回退動態註冊並送出
`application_type`。PKCE 與 resource indicator 保留；交換授權碼前驗證 callback
的 `iss`，已保存憑證維持與 issuer 綁定。探測取得及已儲存的 OAuth resource 都必須
精確符合 MCP 端點的 canonical resource（scheme、host、port 與移除結尾斜線的 path）。
resource 不可帶 query、fragment，也不可指向同源的其他路徑；不符的舊憑證須重新連線。
尚未實作 403 自動 scope step-up 與
CIMD `private_key_jwt`。

## 會議逐字稿

`meeting_transcribe` 接受呼叫員工自己附件中的 `source_path`，以及選用的 `engine`、
`language_hint`（預設 `zh-TW`）、`write_wiki`（預設 `false`），回傳
背景工作編號與狀態；音訊會送至管理員設定的服務商。以 `meeting_transcribe_status`／`meeting_transcribe_cancel` 帶 `job_id` 查詢或取消自己的工作；`meeting_upload_link {}` 提供原對話用的一次性大檔上傳連結。範圍沿用 `transcribe_audio` 的 `messaging:send`，另須已驗證的員工身分，
並通過原有能力、核准及資料外送閘。外部 fallback 命名空間不能使用。
效果分類是寫入保存資料並對外送出內容；選用引擎必須與管理員設定的服務一致。

音訊會送到 Gemini 或 OpenAI，產出的文字轉為臺灣繁體中文。啟用 RFC-23 時會遮罩
儲存的文字，無法遮罩已上傳的音訊。本版的 `drive_file_id`、`write_wiki: true`
會明確回覆不支援。轉錄由閘道在背景執行；ffmpeg 需求、服務限制、設定與隱私說明見
[會議逐字稿](../../features/zh-TW/73-meeting-transcripts.md)。

## MCP API 金鑰（`ddc_<env>_…`，v1.82 起）

在儀表板「整合 → MCP API 金鑰」建立的金鑰以 SHA-256 指紋存在 `<home>/mcp_keys.db`，不再寫進 `config.toml`。stdio 的 `duduclaw mcp-server`（金鑰放在 `DUDUCLAW_MCP_API_KEY`）與 `duduclaw http-server`（`Authorization: Bearer`）每次呼叫都到儲存區查詢（先以指紋做索引查詢，再對查到的那筆以固定時間比對指紋），所以重新命名、變更到期日、停用或刪除都在下一次呼叫生效，不必重啟。它們只以唯讀方式開啟 `mcp_keys.db`，寫入只有閘道。每把金鑰的到期日由管理員決定（30／90／365 天、指定日期或不過期），不再固定 30 天。已到期與已停用的金鑰收到相同的拒絕訊息（`API key is expired or disabled`）；伺服器紀錄會寫明原因與金鑰 ID，絕不寫出金鑰本身。

最後使用時間記在另一個檔案 `<home>/mcp_keys_usage.db`（只有金鑰 ID 與時間，每把金鑰每分鐘最多一次），閘道列出金鑰時讀取。它不參與任何驗證：損壞或被改過的使用紀錄只會影響「最後使用」欄的顯示。

以用戶端 ID 發出的儀表板金鑰一律是**外部**金鑰。可用的權限範圍由伺服器的一份清單決定（`memory:read`、`memory:write`、`wiki:read`、`wiki:write`、`messaging:send`），其他範圍會被拒絕（`mcp_key_scope_not_grantable`，並註明是哪一項）。用戶端 ID 不能是 AI 員工的代號或名稱，也不能是系統保留名稱（`dashboard`、`cron`、`default`、`gateway-internal`……）。AI 員工自己的金鑰（RFC-21 的每位員工專用金鑰、v1.76 的 Google 專用金鑰）要以 `agent_id` 明確建立，而且必須填員工**代號**（`agents/` 底下的資料夾名稱，大小寫須完全相同）；填 `[agent] name`／顯示名稱或大小寫不同的代號會被拒絕（`mcp_key_employee_id_required`）。金鑰在建立時就綁定這位員工（`agent_binding`，之後不會改變）。它是代表該員工的內部金鑰，只能帶 v1.81 金鑰對話框提供過的權限範圍，不能帶 `admin`；同仁的個人專屬員工只有擁有者，或重新驗證身分（step-up）並留下稽核的管理員能建立。

**內部金鑰用在 `duduclaw http-server`。** HTTP 伺服器對所有呼叫者都以同一位固定員工（它的預設員工）的身分執行，而許多工具是以那位員工、而不是金鑰本身的身分讀寫：`working_state_*`、`mail_*`、所有 `wiki_*` 工具、`canvas_*`、`send_message`、`tasks_list`、`plan_*`、`odoo_*`、電腦操作、分支與知識庫工具，以及 `crates/duduclaw-cli/src/mcp/tools_list.rs` 裡 `DEFAULT_AGENT_HANDLER_TOOLS` ∪ `PROCESS_AGENT_TOOLS` 的其餘工具。在 HTTP 上採「預設拒絕」：

- 只有閘道自己的內部金鑰（閘道記錄過指紋的 `gateway-internal` 金鑰，且伺服器本身的員工身分已驗證）可以呼叫這些工具。
- 其他所有非外部金鑰——員工專用金鑰、以顯示名稱建立的金鑰、搬移過來的金鑰、任何未知的內部用戶端 ID——這些工具一律拒絕（`-32003`，稽核類別 `per_agent_key_process_tool`，不論金鑰帶哪些權限範圍），`tools/list` 也不會列出。這個判斷只依金鑰通過驗證的方式，不看磁碟上的檔案。
- 其餘工具（`memory_store`、`memory_search`、`file_read`……）一律以金鑰**所綁定的員工**身分執行，絕不會改用伺服器的預設員工。`tools/list` 只列出這把金鑰的權限範圍實際能呼叫的工具（例如只有 `memory:read` 的金鑰看不到 `memory_store`）；需要 `admin` 的工具（例如 `tasks_create`）員工專用金鑰一律不能呼叫，也不會列出。
- 沒有綁定員工、或綁定的員工已不存在（資料夾被移除或改名，或 `status = "deleted"`）的非外部金鑰，**所有**工具都會被拒絕（`-32003`，稽核類別 `remote_key_unbound` 並記下原因），`tools/list` 為空；金鑰頁會把它標成「員工已不存在」。

**刪除員工會永久撤銷它的金鑰。** 在儀表板刪除員工時，綁定這位員工的每把金鑰會在同一個操作中被刪除（保留指紋作為墓碑，永遠不會再通過驗證），並逐把寫入稽核 `mcp_key_deleted`（原因 `employee_deleted`）。已刪除的員工無法從儀表板還原；即使管理者在磁碟上恢復員工資料，金鑰仍維持已刪除。**之後以相同代號建立的員工也拿不到這些金鑰**：請為新員工發新的金鑰。在閘道以外移除的員工（MCP `agent_remove` 把資料夾移到 `agents/_trash`，或手動刪除資料夾）或 `status = "deleted"` 的員工，會在閘道下一次金鑰檢查（啟動時與每 10 分鐘）以同樣方式撤銷金鑰（整個 `agents/` 資料夾不存在時不做任何判斷）。第二道防線：比所綁定員工資料夾還早建立的金鑰，HTTP 伺服器一律拒絕（原因 `employee_recreated`，金鑰頁標示「員工已不存在」）——那代表員工是以被移除者的代號重新建立的，即使還沒到下一次檢查也一樣。這項檢查需要作業系統提供資料夾建立時間（macOS、Windows、多數 Linux 檔案系統）；無法取得時，以上述撤銷機制為準。把整個 DuDuClaw 資料夾從備份還原或複製到別處，會重設這些建立時間，還原前發出的員工專用金鑰會被拒絕，需要重新發行。

**被拒絕的金鑰會留下紀錄。** 每次 MCP 金鑰驗證失敗都會記下 `reason`——`missing_header`（沒帶金鑰）、`bad_scheme`（不是 `Bearer`）、`malformed_key`（格式錯誤）、`wrong_credential_type`（憑證種類不對）、`unknown_key`（不存在）、`key_disabled`（已停用）、`key_expired`（已到期）、`key_deleted`（已刪除）、`key_unbound`（員工已不存在）或 `store_unavailable`（儲存區無法讀取）之一——以及固定的路徑代號 `route`（`mcp`、`mcp_v1_call`、`mcp_v1_stream`……）。已停用、已到期、已刪除或員工已不存在的金鑰會附上金鑰 ID（`mk_…`）。紀錄絕不包含金鑰本身、金鑰的任何片段或指紋，也不含網址參數或 User-Agent。紀錄分成兩個檔案：

- `<home>/security_audit.jsonl`（具雜湊鏈的安全稽核，伺服器紀錄也有一行警告）：**已存在金鑰**（停用、到期、已刪除、員工已不存在）的驗證失敗會立即寫入一筆附金鑰 ID 的 `mcp_auth_failed`——每把金鑰、每種原因每小時一筆，該小時其餘的次數在整點後再寫一筆並附次數（`count`）。**其他失敗**每小時彙總成一筆 `mcp_auth_failed_summary`：各原因次數、來源數（最多算到 1,024）、沒寫進去的紀錄數，並註明明細檔名稱。所以沒有有效金鑰的人無論怎麼嘗試，每小時最多只會讓安全稽核多一筆；整合服務若一直用已撤銷的金鑰重試，會立即以帶金鑰 ID 的紀錄出現。
- `<home>/mcp_auth_failures.jsonl`（只有擁有者可讀寫）：逐分鐘、含來源位址 `source` 的明細，來源是伺服器連線看到的 IP（不採用 `X-Forwarded-For`，經反向代理時就是代理的位址）。`http-server` 上，同一來源、同一原因、同一把金鑰每分鐘的第一次失敗立即寫入，該分鐘其餘的合併成一筆並附次數；每分鐘最多 60 筆，超出的合併成一筆 `source: "many"`。檔案到 4 MiB 時改名為 `mcp_auth_failures.jsonl.1`（取代前一份），兩份合計約 8 MiB 以內。`duduclaw import` 不會搬移這個檔案。

stdio `mcp-server` 每個行程最多在安全稽核寫一筆 `mcp_auth_failed`，不寫明細。

外部金鑰與每位員工自己的 stdio `mcp-server` 不受影響。v1.83 會改成所有工具都以金鑰所屬員工的身分執行，再取消這項限制。

**升級後的 `config.toml [mcp_keys]`。** 閘道第一次以 v1.82 啟動時，會在同一筆交易裡把當下 `config.toml` 的 `[mcp_keys]` 項目指紋記為「待搬移」，再逐把搬進儲存區、寫入稽核紀錄，之後才從 `config.toml` 移除；全部搬完會寫下「搬移完成」標記。搬過來的金鑰保留用戶端、外部旗標與權限範圍，到期時間就是 v1.81 開始拒絕它的那一刻（`created_at` 加 31 天，以秒為單位）。搬過來的內部金鑰只有在用戶端 ID 剛好等於某位現有員工的代號，**而且**每個權限範圍都是員工專用金鑰可用的範圍（不含 `admin`）時才會綁定該員工，否則不綁定（HTTP 伺服器會拒絕它，見上文）。員工的舊金鑰若帶有 `admin` 或其他員工專用金鑰不能用的範圍，會照原樣搬移、範圍不變，但不綁定員工：金鑰頁標示「需要換發：權限範圍不適用於員工專用金鑰」，閘道紀錄也會以提示（`ddc_prod_…3f9a`）寫明。請以員工代號為這位員工建立新金鑰，再刪除舊的。綁定員工的金鑰只會生效員工專用的範圍，`admin` 不能代替其他範圍。`created_at` 比現在晚超過幾分鐘的項目一律不接受、不選用、不搬移（列為已忽略）。所有讀取端（MCP 伺服器、OAuth 同意頁、Google 金鑰檢查）都依下列規則：

- `mcp_keys.db` 不存在（這個資料夾從未啟動過 v1.82 閘道，例如升級後的閘道還沒啟動前先跑的 `mcp-server`）：`[mcp_keys]` 依 v1.81 規則（30 天）驗證。這是唯一的過渡窗口，閘道一建立儲存區就永久關閉。刪除 `mcp_keys.db` 需要資料夾的寫入權，而儲存區要求資料夾不可讓群組或其他使用者寫入。
- `mcp_keys.db` 存在但無法讀取、缺少資料表、或結構與 `user_version` 不符：所有金鑰一律拒絕（「儲存區無法使用」），包括 gateway-internal 金鑰；絕不當成「還沒有儲存區」。
- 儲存區裡的金鑰由儲存區單獨決定；`config.toml` 裡的舊複本會被忽略，並在下次啟動時移除。
- 「搬移完成」標記寫下之前，`config.toml` 只對記為待搬移的指紋有效；寫下之後，非內部金鑰再也不看 `config.toml`。之後才加入的項目不會被搬移、永遠不會通過驗證，只會在紀錄寫一次「已忽略的舊金鑰項目」，並在金鑰頁以提示（`ddc_prod_…3f9a`）列出，請管理員刪除。
- `gateway-internal`：閘道產生或輪替內部金鑰時，會把它的指紋記在 `mcp_keys.db`。只有這把金鑰會被接受，條件是以 `gateway-internal` 身分、`config.toml` 裡仍有它的項目、且在 30 天內；只是自稱這個用戶端 ID 的項目會被拒絕。**升級後第一次啟動時，閘道會為自己發一把新的內部金鑰**（若啟動時帶了 `DUDUCLAW_MCP_API_KEY` 則沿用該把，並改寫它在 `config.toml` 的日期），只記錄這一把、交給自己的 MCP 設定，同一次啟動稍後也會把它寫進每位員工的 `.mcp.json`。`config.toml` 裡原有的 `gateway-internal` 項目不論日期一律不採用、不記錄、永遠不會通過驗證，只會在紀錄寫一次（只顯示提示），並在金鑰頁列為「已忽略的舊金鑰項目」。

`http-server` 的 OAuth 同意頁接受儲存區裡使用中的內部（非外部）金鑰，或依上述規則可用的 `config.toml` 金鑰作為操作者證明；金鑰搬進儲存區前用它同意過的授權仍然有效。

**金鑰的搬移是單向的。** v1.81 不讀 `mcp_keys.db`；降級後，外部金鑰必須在 v1.81 重新發行。請不要為此還原舊的 `config.toml`：那會讓已在 v1.82 刪除或停用的金鑰重新生效。

## 每位員工的工具清單精簡

管理員可在員工總覽的「工具清單精簡」卡片選擇模式、展開未使用工具與查看預估節省量；啟用時可還原清單。模式選單沿用既有員工更新流程。`agents.tool_pruning_status {agent_id}` 與 `agents.tool_pruning_restore {agent_id}` 都限 Admin。估算方式是各候選工具定義序列化後的長度除以四，不是 tokenizer 的實際結果，也不代表供應商實測省下的用量。

```toml
# agent.toml
[capabilities]
tool_pruning = "shadow" # "off" | "shadow" | "on"
```

預設 `shadow` 僅觀察與回報，`tools/list` 回覆與 `off` 完全相同。未知或格式錯誤的值視為 `off`，並警告一次。只有 `on` 會在所有既有清單篩選之後隱藏符合條件的工具。這不會授予或撤銷權限：隱藏工具仍可通過原有 dispatch 閘呼叫；成功呼叫後，工具會經 `notifications/tools/list_changed` 重新出現在清單。

計算只讀取 `tool_calls.jsonl` 最後 5 MB。紀錄不存在、無法讀取或不足十個已記錄回合時，不精簡任何工具。工具至少連續十個回合未使用且跨兩個 session，或跨五個 session 未使用，才符合條件。記憶、任務、工作狀態、訊息、核准、wiki 讀取、工具探索、Admin／未知效果類別工具，以及有有效任務範圍授權的工具都保留。回合以主機寫入的 `(session_id, turn_id)` 唯一組合計算，不是 log 行數。頻道呼叫使用對話 session 與 turn；派工作業使用自身的 dispatch session 與 run，一次 run 算一回合。舊紀錄缺少這些欄位時，不建立回合樣本，但成功使用仍會重設該工具的未使用期間。門檻從各工具最後一次成功使用之後開始計算。每個行程快取五分鐘。還原會寫入該員工的重設標記，不再採計標記時間之前的使用紀錄，讓符合既有權限的完整清單回來；原有權限篩選仍適用。

Claude CLI 的 `minimal_context` 透過 `--tools` 控制內建 CLI 工具，不影響 MCP 工具定義；此流程維持原樣。

## 規則：清單遵循授權，精簡只影響探索

`tools/list` 只列出呼叫端**當下真的叫得動**的工具。授權過濾對應派工層原有的閘；選用的員工工具精簡則進一步縮小探索清單，已授權工具可能未列出但仍可呼叫。

這既是正確性問題，也是成本問題。工具 schema 是**每次開機都要付的固定 prompt 成本**：CLI 每個 session 讀一次 `tools/list`，整包在第一個使用者 token 之前就進了模型的上下文。一個註定被拒的工具 schema 等於付兩次錢，token 付一次，模型繞著一個用不了的工具規劃再付一次。

隱藏工具**不是**授權決定。呼叫未列出的工具仍然會打到真正的閘，依原有規則接受或拒絕呼叫。

### 被過濾的項目

| 過濾條件 | 來源 | 關閉／空值時的效果 |
|---|---|---|
| 外部用戶端白名單 | `principal.is_external` | 只列 7 個舊白名單工具加上明確授予 scope 的工具，接著再套下一列的 scope 規則 |
| 非員工呼叫者的 scope | `principal.scopes`，適用於不持有 `admin`、也不是 AI 員工的金鑰（不是 gateway 內部金鑰、不是單一員工金鑰、也不是 gateway 為員工啟動的行程） | 只列金鑰持有其最低 scope 的工具；替行程本身的 agent 動作的工具（`working_state_*`、`canvas_*`、`shared_wiki_delete`、`memory_search_by_layer` 與三個整併狀態查詢等）不列，對所有外部金鑰也不列；呼叫時 dispatch 閘依同一條規則拒絕（`process_agent_tool`）。見 [mcp-standalone.md](mcp-standalone.md) |
| Google Workspace | `config.toml [integrations] google_workspace` | 關閉時隱藏全部 21 個 Google 工具，包含 `google_status`；儀表板仍可設定與測試，見[連接 Google](google-setup.md)。 |
| Google 身分（v1.76.0） | `agent.toml [google] identity`，沒寫時看 `config.toml [integrations.google_identity] default_identity` | `none` 隱藏同樣 21 個工具，呼叫也被拒。其他值都照常列出；用不了的呼叫會回原因。`google_status` 改為先講員工目前代表誰 |
| GitHub | `config.toml [integrations] github` | 隱藏 5 個 `github_*` |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | 被拒的隱藏；允許清單非空時其餘全隱藏。清單項目完全相同才算，或用結尾的 `*` 比對：`*`、`mcp__duduclaw__*`（全部 DuDuClaw 工具）、`mcp__duduclaw__odoo_*`／`memory_*`（從開頭比對的前綴）。`mcp__<其他伺服器>__…` 不會對到 DuDuClaw 工具；其他位置的 `*` 只是一般字元。三份審批清單與 `scoped_tools` 用同一套規則 |
| `os_native` | `agent.toml [capabilities]` | 隱藏 6 個 `os_*` 自動化工具 |
| `recording` | `agent.toml [capabilities]` | 隱藏 5 個錄製工具 |
| `system_operator` | `agent.toml [capabilities]` | 隱藏 19 個值班機操作工具 |
| `codrive` | `agent.toml [capabilities]` | 隱藏 `codrive_run` / `codrive_status` |
| `computer_use` | `agent.toml [capabilities]` | 隱藏 9 個 `computer_*`（見下方 [`computer_*`](#computer_由-gateway-執行的-session)） |
| `db_sources` | `agent.toml [capabilities]` | 隱藏 4 個 `db_*` |
| `[fork] enabled` | `agent.toml` | 隱藏 6 個分支工具 |
| `[knowledge] enabled` 加集合授權 | `config.toml`／`knowledge.db`／`agent.toml [knowledge] collections` | 除非知識庫啟用、且至少有一個集合授權給員工或其部門（經 `[knowledge] collections` 縮小後），否則隱藏 4 個 `knowledge_*`；授權變更會送出 `list_changed`（見下方 [`knowledge_*`](#knowledge_文件知識庫)） |
| `[responsibilities] enabled` | `config.toml` | 隱藏 3 個 `responsibility_*`（見下方[持續任務工具](#responsibility_get--responsibility_followup--responsibility_ask持續任務工具)） |
| `scoped_tools` | `agent.toml [capabilities]` ＋ 有效授權 | 在階段性授權生效前一律隱藏 |
| 團隊角色成員（v1.82） | `agents/.ephemeral/eph-*` 角色成員，`[team_member]` 與 `ephemeral.toml` 的上層員工一致，且身分通過權杖驗證（`DUDUCLAW_AGENT_TOKEN`） | 在任何委派模式下，其他呼叫者都看不到 `team_handoff`，所以一般員工的工具清單比以前少一支；處理程式對同樣的呼叫者回 `not_a_team_member` |

機器操作主控台（`/console`）只開放給管理員。`system_operator` 員工執行機器操作工具時，伺服器也會確認交辦人仍是啟用中的管理員；其他 AI 員工的一般對話仍依原有授權使用。

新建的 AI 員工以上全部都沒開，這正是重點：預設部署只為自己用得到的工具付費。

### 中途變更如何傳到 AI 員工

以前隱藏工具等於永久拿不到，因為 MCP 用戶端只在開場讀一次 `tools/list`。現在伺服器在 `initialize` 回應中宣告 `tools.listChanged`，並在呼叫端的可見集合**真的改變**時送出 `notifications/tools/list_changed`，比較的是集合，所以只是碰了設定檔但內容沒變，不會送出任何通知。

所以這串流程不需要重啟：

1. AI 員工撞到 `scoped_tools` 拒絕，或操作者要授權某個能力。
2. `capability_request` 核准（或儀表板存了 `agent_update`，或有人改了 `agent.toml`）。
3. 幾秒內伺服器察覺變更並通知用戶端。
4. 用戶端重讀 `tools/list`，工具就在那了。

階段性授權在任務進入任一終態時撤銷，同一套機制也會把工具收回去。

### 注意：AI 員工無法自行發現 `scoped_tools` 的名稱

列在 `scoped_tools` 裡的工具在授權生效前是隱藏的，所以 AI 員工沒辦法從 `tools/list` 讀到名稱再去申請。請用別的方式告訴它，寫進 `SOUL.md`、寫成 playbook 規則，或在目標開工時用 `grant:<tool>` 標籤直接鑄出授權。這是裁剪唯一有代價的地方，而且是刻意的：宣告一個當下就會被拒的工具，正是上面那條規則要消滅的失敗模式。

### v1.68.0 起的例外：列出但會拒絕

兩道新的閘門會拒絕呼叫，但不會把工具從 `tools/list` 拿掉：

- `agent.toml [permissions]`：寫成 `false` 的旗標會拒絕 `create_agent`（`can_create_agents`）；`send_to_agent`、`spawn_agent`（`can_send_cross_agent`）；`create_reminder` 與帶 `schedule` 的 `tasks_create`（`can_schedule_tasks`）；`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`（`can_modify_own_skills`）。拒絕時回 JSON-RPC 錯誤 -32003 並記稽核 `permission_denied`。`agent.toml` 存在但讀不到或無法解析時，這些工具一律拒絕；檔案不存在則放行。不帶 `schedule` 的 `tasks_create` 仍然允許，所以這些旗標是逐次呼叫檢查。臨時角色成員建立時 `can_create_agents`、`can_modify_own_skills`、`can_schedule_tasks` 為 `false`。
- `config.toml [odoo] features_*`：Odoo 工具照樣列出，呼叫到已關閉模組的模型時逐次拒絕（project 與 hr 預設關閉）。

### 紀錄關係檢查：列出但會拒絕

有些工具會變更或觸發屬於某位 AI 員工的紀錄。它們對所有呼叫者都照常列出，每次呼叫時再檢查關係：

- **任務**：`tasks_update` 與帶 `task_id` 的 `activity_post`，任務的受派者、認領者或建立者可以直接操作；`tasks_complete` 與 `tasks_block` 只認受派者與認領者；`tasks_claim` 可以認領未指派或本來就指派給自己的任務。其他情況都要與受派者有委派關係（同部門、`reports_to` 上下級，或白名單配對，依 `[delegation] policy`）。未指派、未認領的任務要先認領。用 `tasks_update` 把別人的任務改派給自己，一律要有這個關係。
- **任務欄位**：AI 員工不能改 goal 模式任務的 `title` 或 `description`（`acceptance_criteria` 則所有 MCP 呼叫者都不能改），也不能在 `tasks_update` 或 `tasks_create` 的 `tags` 新增、移除或調換以 `outcome:`、`grant:` 開頭的 tag 與 `auto-research` tag。
- **新任務的上層任務**：閘道會告訴 MCP 伺服器這一輪在處理哪個任務（`DUDUCLAW_TASK_ID`，和核准卡片帶的是同一個值）。這個任務是呼叫者自己的、而且屬於[持續任務](continuous-responsibilities.md)的某一次執行（執行本身或它底下的任務，包括任務看板喚醒的子任務）時，沒給 `parent_task_id` 的 `tasks_create`（含 `kind="goal"`）會掛在它底下，員工自己給的 `parent_task_id` 必須是那個任務或它的子任務，否則拒絕。這個值存在但是空白或格式不對時一律拒絕，不會當成「沒有輪次」。其他情況（一般 goal 輪次、執行之外任務的看板喚醒、沒有輪次資訊）不預設上層任務。這一版對所有呼叫者都新增的規則：給了 `parent_task_id` 需要與上層任務有關係（指派、認領、建立或委派規則），v1.69 原本不檢查；`kind="goal"` 也接受 `parent_task_id`，v1.69 原本忽略；AI 員工在一個任務底下最多建 200 個尚未結束的子任務（`schedule` 建的例行工作與提醒不是子任務）。從 Bash 啟動的伺服器，以及 Grok 執行環境，拿不到輪次資訊；在那裡建立的任務除非員工指定這次執行為上層任務，否則不會掛到執行底下，也不計入它的花費。掛對位置還要靠員工改不了自己 `.mcp.json` 裡的 `duduclaw` 項目，這是另一項平台修補，必須先合併。
- **例行工作**：`update_cron_task`、`delete_cron_task`、`pause_cron_task`、`run_cron_task` 要求呼叫者就是這筆例行工作的執行員工，或與它有關係。以 `name` 指定時只作用於一筆；多筆同名會被拒絕，並列出候選 id。
- **提醒**：`create_reminder` 的 `agent_id` 不是呼叫者自己時，要與該員工有關係。
- **對自己用 `agent_update`**：AI 員工不能對自己送出 `reports_to`、`db_sources`、`db_sources_add`、`db_sources_remove`、`budget_cents` 或 `role`（稽核 `agent_authority_refused`）。修改下屬不變。

操作者（不對應任何 AI 員工的 MCP 金鑰，且行程不是為某位員工啟動的）不受限。行程沒有員工身分時，內部共用金鑰不擁有任何紀錄，所以碰任何人的紀錄都會被拒絕。身分是系統 sender 名稱（`dashboard`、`cron` 等）的行程會被當成不受信任。每次拒絕都記在 `tool_calls.jsonl`。詳見[任務看板](../../features/zh-TW/24-task-board.md)、[委派隔離](../../features/zh-TW/37-delegation-isolation.md)。

## 說明字數預算

每個工具的 `description` 上限 **200 bytes**，每個參數說明上限 **200 bytes**（僅一個列明的例外，見下）。這個上限由測試強制，不是靠自律。

新增或修改工具時的兩個結果：

- **寫「做什麼」與「什麼會被拒」。** 安全關鍵句，「這不會寄出」「這是公開可見的」「超限整筆拒絕、絕不截斷」，留在說明裡。設計理由、範例、內部代號不留。
- **長版寫在這一頁。** 從說明連到這頁的段落，或該工具自己的規格頁。

### 唯一的例外

`team_handoff` 的 `packet` 參數帶著完整的 TaskPacket 形狀（上限 1,024 bytes）。`build_tool_schema` 除了 `PARAM_TYPES` 這份短清單列出的整數、數字、布林與字串陣列之外，參數一律宣告成裸 JSON-Schema 字串，所以 TaskPacket 這種物件只能在參數說明裡寫出真實形狀，而 TaskPacket 是整筆拒絕不截斷的，看不到形狀的 AI 員工就產不出合法的封包。完整規格在 [../../spec/task-packet.md](../../spec/task-packet.md)。

所有例外集中在一份清單（`PARAM_CAP_EXEMPTIONS`），並有測試在例外不再必要時失敗。

## 工具 annotations

`tools/list` 的每個工具都帶 MCP `annotations`，由派工閘門與 `action_rules` 共用的同一張效果分類表（`duduclaw_core::effect_of`）推導：

| 效果分類 | `readOnlyHint` | `destructiveHint` |
|---|---|---|
| `read` | `true` | （不帶） |
| `draft`、`send` | `false` | `false` |
| `publish` | `false` | `true`（公開後可能蓋掉別人已在讀的內容） |
| `modify` | `false` | `true`；`memory_store`、`user_profile_record`、`wiki_rebuild_fts` 只新增或重新產生，標 `false`。`wiki_export` 仍標 `true`：它會覆寫上一次的匯出，包括可能已被人編輯過的 Obsidian vault |
| `delete`、`purchase`、`admin` 與表上沒有的名稱 | `false` | `true` |

記憶、wiki、使用者偏好與 `code_map` 工具另外標 `openWorldHint: false`；`memory_alias_add`、`wiki_export`、`wiki_rebuild_fts` 標 `idempotentHint: true`。這幾份清單在 `crates/duduclaw-core/src/mcp_tool_defs/annotations.rs`。

這些提示給 MCP 用戶端參考（例如呼叫前要不要先問人），不代表任何授權，DuDuClaw 的每道閘門照常檢查。它們不計入上面的說明字數預算：Claude Code 與 Codex 交給模型的工具清單只有名稱、說明與輸入 schema。

## 從說明搬出來的長版細節

### `tasks_create`／`tasks_update` — 目標費用上限

`tasks_create` 搭配 `kind="goal"` 時接受 `cost_cap_cents`，單位是美分，須為正整數（最大 `9223372036854775807`）。省略時使用 `[goal_loop] default_cost_cap_cents`（`0` 代表沒有預設上限）。一般任務與排程請求帶這個參數會被拒絕。例如 `{"kind":"goal","title":"檢查報告","assigned_to":"agnes","cost_cap_cents":500}`，其他目標說明與驗收標準仍照常填寫。

`tasks_update` 可為目標修改同一欄位。員工只能新增或收緊上限，不能提高或移除生效中的上限；提高上限須由操作者執行。儲存新上限不會自行恢復暫停目標；儀表板的「提高上限並重試」會先儲存，再走既有重試決策。目標與子任務的已量測費用達上限時以 `budget_exhausted` 暫停，費用讀不到時跳過派工。見[目標迴圈](../../features/zh-TW/34-goal-loop.md)。

### `tasks_create` — 目標的單次執行時限

`tasks_create` 搭配 `kind="goal"`（非排程）時接受 `timeout_secs`，30～3600 的整數秒：這個目標每一輪的每次 CLI 執行／API 請求都用它，取代被指派員工的單次執行時間上限。透過 MCP 只能設成小於或等於員工本身的上限；要給目標更多時間，由管理員在儀表板交辦。Claude 員工也適用；執行時仍受目前全域 `[runtime] max_timeout_secs` 約束。`tasks_update` 會拒絕這個欄位。例如 `{"kind":"goal","title":"核對帳目","assigned_to":"anna","timeout_secs":180}`。

### `office_script` — 內建 Office 腳本

`office_script` 會執行內嵌於程式、經 manifest 驗證的 `docx`、`xlsx`、`pptx` 或 `pdf` 腳本。可選 `create`、`extract` 或 `to_pdf`；`.py` 後綴可省略，腳本名稱不能含路徑。員工技能資料夾只能改變提示／指示，不能替換可執行腳本。此工具支援 Linux 與 macOS。

`args` 必須是 JSON 字串陣列，例如 `["outline.md", "--out", "attachments/deck.pptx"]`。輸入檔可在員工目錄或共用附件；輸出必須是員工目錄內的新檔案，父目錄須已存在，副檔名須符合該腳本。旗標與值要分開傳入；拒絕 `--key=value`、符號連結及路徑穿越。若要交付產生的檔案，回覆最後一行使用 `DELIVER:<absolute path>`。

### `codrive_run` — 三段式執行階梯

每個步驟依序嘗試三段，能用高的就用高的：

- **C-L2： `api_action`。** 在碰 GUI 之前，先呼叫 `target_app` 已登錄的第三方原生 API/CLI/D-Bus 動作。只要你要的 app＋動作有登錄就優先用它。`action` 是該 app 的短登錄識別字（chromium 的 `open_url`、networkmanager 的 `state`…），`params` 是該動作的 payload，派送時對該動作自己的 schema 驗證。登錄查無或執行失敗會落回該步驟的 `action` 欄位，所以即使設了 `api_action`，`action` 仍然必填。
- **C-L3： `locate`。** 用 `(role, name)` 在 `target_app` 的 AT-SPI2 無障礙樹查出 `move`/`click` 的座標，取代手猜像素。對版面、解析度、佈景變動的耐受度高得多。對 `text` / `key_name` / `wait` / `take_over` 無效。查無則落回字面 `x`/`y`。
- **C-L1： 字面 `x`/`y`。** 前兩段缺席或失敗時的最後手段。

其他腳本層規則：

- `target_app` 是**腳本層級單一欄位**，不是逐步驟。一份腳本只驅動一個 app；要驅動第二個 app 就再發一次 `codrive_run`。
- 每個有後果的步驟（`send` / `submit` / `delete` / `purchase` / `other`）在任何一段派送之前都會停下來等人核准。命中拒絕清單（銀行頁面、繞過 CAPTCHA…）則直接拒絕，連連線都不會嘗試。
- 登入／密碼／付款步驟（`take_over`，或 `credential` 類別）會把共用桌面交還給人。你永遠不自己用任何一段送出憑證文字；人把控制權交回來後腳本才續跑。
- 共用桌面上只要有人動手，AI 的座位立刻凍結。被丟掉的那一步會在人交回控制權後自動重試一次。
- `watch_mode: true` 在本次執行剩餘時間內啟用閒置監看。
- 最多 50 步。

### `working_state_handoff` — 結構化模式

兩種模式：

- **純筆記。** 不傳 `status`；沿用舊行為，約 1,200 字處靜默截斷。
- **結構化（Ralph-loop 式）。** 傳 `status`，並與 `next_steps` / `evidence` / `blocker` 一起校驗：
  - `continue` — 必須有 `next_steps`，不得有 `blocker`。
  - `complete` — 必須有 `evidence`，且 `blocker` 與 `next_steps` 都不得存在。沒有證據的自稱完成、或還留著下一步的完成，一律拒絕：「我做完了」不是證據。
  - `blocked` — 必須有具體的 `blocker`。

  合併後的內容由 `config.toml [memory] working_state_handoff_max_bytes`（預設 16384，CJK 安全位元組計算）限制。超過**整筆拒絕**，絕不靜默截斷，因為截掉的可能正是讓這份交接成為權威的證據。

### `skill_search` — 怎麼挑 source

除非你已經知道技能在哪，否則不要動 `source`。

- `all`（預設）— 已設定的 hub 加上這個 AI 員工學到的技能庫，依名稱去重並標註來源。Hub 結果依相關度 × 信任 × 安裝數 × 新鮮度排序，官方第一方技能保底進前段。
- `github` — 預期在公開 GitHub repo 裡的技能。
- `hub` — 策展登錄庫（`anthropic-skills`、`github`、`clawhub`、`lobehub`、`skills-sh`）；用 `hub` 參數收斂到單一個。
- `bank` — 只看這套部署自己學到的技能。此來源不接受 `hub` 參數。

### `evolution_toggle` — 欄位與權限

`field` 接受 `gvu_enabled`（布林值），以及 `max_silence_hours`、`skill_token_budget`、`max_active_skills`（數值）。舊的 `stagnation_*` 欄位已不能透過此工具修改。見 [evolution-switches.md](../evolution-switches.md)。

AI 員工只能調整自己或下屬的設定，採用與 `agent_update` 相同的身分與組織授權檢查；委派政策設為 `open` 時允許跨團隊調整，操作者不受此限制。未通過身分或組織授權檢查時，會在讀取目標設定檔前拒絕。這些欄位不會授予工具、資料或組織權限，也不會繞過演化主開關，因此保留自身調整。

### `execute_program`：腳本在哪裡執行

腳本在腳本沙箱裡執行，也就是用 `config.toml [container.sandbox] image` 指定的 image 起一個容器（與[任務沙箱](task-sandbox.md)同一個 image，不會自動下載）。容器以主機使用者身分執行（主機行程是 root 時改用 `1000:1000`，WSL2 上一律如此），丟棄所有 capability、`no-new-privileges`、唯讀根檔案系統、沒有網路、2 GiB 記憶體且不用 swap、256 個行程、1 顆 CPU，以及一個小的 `/tmp` tmpfs。只掛載一個放腳本的私有目錄，唯讀掛在 `/workspace`。`timeout_seconds`（預設 30，最多 300）之外還有 600 秒的硬上限；stdout 與 stderr 合成一份輸出回來（讀取上限 2 MiB，回覆上限 1 MiB）；呼叫被取消時容器會被強制移除。macOS 與 Linux 用 Docker；Windows 先試 WSL2，再試 Docker。

沙箱不能用時（沒有 Docker、image 不在本機、`[container.sandbox]` 無效等），腳本**不會執行**：工具回傳 `Script sandbox unavailable (<代碼>): …` 並附上 `docker pull <image>` 指令，同時寫入稽核事件 `script_sandbox_unavailable`。舊版遇到這種情況會默默改在主機上執行。要恢復舊行為，設 `[container.sandbox] script_when_unavailable = "run_unsandboxed"`（與任務沙箱的 `when_unavailable` 是不同的鍵），之後每次在主機上執行都會記一筆 `script_sandbox_bypassed`。

腳本無法回頭呼叫平台工具：容器裡沒有 RPC socket。

### `computer_*`：由 gateway 執行的 session

這九個工具為每位 AI 員工驅動一個電腦操作 session：一個帶虛擬顯示器與 kiosk 瀏覽器的隔離容器。MCP server 只負責把每次呼叫透過 loopback 轉給 gateway（`POST /api/internal/computer-use`，每個請求各自簽章）；容器由 gateway 持有，所有檢查也由 gateway 執行，所以 gateway 必須在執行中。除非 `agent.toml [capabilities] computer_use = true`，否則這些工具是隱藏的。

| 工具 | 參數 | 備註 |
|---|---|---|
| `computer_session_start` | `task` 字串，選填；`width` 整數 320–1920；`height` 整數 240–1200 | 每位員工一個 session。結果會列出各項上限、高風險動作能否在聊天中確認，以及 `computer_navigate` 能開哪些網站 |
| `computer_screenshot` | 無 | MCP 圖片區塊（PNG，已遮罩），後面接一個文字區塊，寫已用動作數與剩餘時間。整張圖被遮掉時，文字會如實說明，並寫出原因（多個視窗、焦點視窗敏感或讀不到、偵測失敗）與下一步 |
| `computer_click` | `x`、`y` 整數（必填）；`button` 字串 `left`/`right`；`double` 布林 | `double` 只限左鍵 |
| `computer_type` | `text` 字串（必填），1–2,000 字元 | 稽核只記字元數 |
| `computer_login` | `credential`（必填，憑證標籤字串） | Gateway 代允許的員工填入並送出登入，只回傳狀態與一句說明 |
| `computer_key` | `key` 字串（必填）：字母、數字、`+`、`-`、`_` | 例如 `Return`、`ctrl+s` |
| `computer_scroll` | `x`、`y` 整數（必填）；`direction` 字串 `up`/`down`（預設 `down`）；`amount` 整數 1–20（預設 3） | |
| `computer_navigate` | `url` 字串（必填） | 只接受 `https://`，主機必須剛好在該員工的 `[capabilities.computer_use_config] allowed_domains` 上且在 session 啟動時解析成功，連接埠不寫或為 443，不得帶使用者名稱或密碼，最長 2,000 位元組。沒有白名單時 session 沒有網路，呼叫會被拒絕 |
| `computer_session_stop` | `session_id` 字串，選填 | 移除容器 |

`computer_login` 是第九個 session 工具。管理員在 **系統 → 安全 → 網站登入憑證** 建立帶標籤、HTTPS `login_url` 的登入資料，並指定至少一位允許的員工；密碼只輸入該表單，不貼到聊天。選填的 `allowed_paths` 預設為空，只允許 helper 送出第一步後到達、含可見密碼欄位的相同來源第二步確切路徑。Gateway 先透過既有導覽檢查開啟保存的網址；導覽與登入各消耗一次 session 動作額度。Helper 檢查確切來源、路徑及 POST 提交方式，包含原生 `requestSubmit` 與備援路徑實際選用送出按鈕的 `formmethod`，submit 事件後也會重查；攔截每個請求與轉址以拒絕其他來源，拒絕查詢參數含完整帳號或密碼值的非 POST 請求，並略過 service worker。MCP 請求只帶標籤，session 啟動與狀態回應只顯示呼叫員工可用的標籤，不含使用者名稱。收到 `needs_takeover` 時請人員使用既有接手流程，沒有 OTP 輸入工具。`logged_in` 只表示密碼欄位消失；若清除失敗（`failed_uncleared`），Gateway 會停止並移除 session 與容器，回傳錯誤要求啟動新 session，不能靠導覽恢復。解除攔截前，helper 會追蹤 `Network.requestWillBeSent`、`Network.loadingFinished` 與 `Network.loadingFailed`，確認請求已結束。最多等待 3 秒，且須連續兩次命令往返都沒有新請求活動或待完成請求。若無法確認完成，回傳 `failed_uncleared`，在 Fetch 攔截仍啟用時呼叫 `Browser.close`，再由 Gateway 移除 session 與容器。登入執行期間暫停截圖。

登入僅限保存的來源（協定、主機、連接埠），表單送出也必須同源。所有網址都必須使用 HTTPS，localhost 也不例外。同源網頁 JavaScript 能讀取已填入的欄位；雙因素驗證須由人員處理。使用前須重建電腦操作容器映像，加入新的 helper。

整數與布林參數也接受數字字串與 `"true"`/`"false"` 字串。點擊、輸入、登入、按鍵、捲動與導覽各算一個動作，計入 `max_actions`（預設 50）。各項上限、審批與確認規則、網路白名單及其殘留風險，見[瀏覽器自動化](../../features/zh-TW/08-browser-automation.md)。

### `os_*`：DuDuClaw OS 上的裝置操作

需要 root 的 `os_*` 工具（電源、更新狀態、由 sysd 重新驗證的更新安裝、開機評估、回退、恢復原廠）在 MCP 伺服器裡照舊先過完所有檢查（Admin 權限範圍、`[permissions]`、確認、恢復原廠的核准），再經本機回送位址把請求交給 gateway（`POST /api/internal/sysd`，跟 `computer_*` 一樣每次請求都簽章）。gateway 用自己的連線轉給 `duduclaw-sysd`，所以 sysd 只接受 gateway 行程時這些工具仍能用。這條路由只接受上述指令，SSH、網路、主機名稱、時區與本地模型操作一律拒絕。這需要裝置上的平台是 v1.73.4 以上，所以從內嵌 v1.73.4 的 DuDuClaw OS v0.4.0 起適用（v0.3.0 內嵌 v1.73.1，工具仍直接呼叫 sysd）。gateway 沒在執行，或 MCP 伺服器不是由 gateway 啟動（沒有員工身分）時，工具會失敗並說明原因，不會直接連 sysd。非 DuDuClaw OS 的安裝沒有變化。

### `belief_stats` / `belief_settle`：已驗證與自報的結算

只有經過平台價格交叉驗證的結算才計入校準。目前沒有任何正式路徑提供這個交叉驗證：`belief_settle` 把每一筆結算都記成員工自己的回報（`settle_source = "agent_unverified"`），所以現有部署的校準狀態都是「沒有已驗證的結算」。

`belief_settle` 回傳結算後的資料列，加上 `counts_toward_calibration`（布林值）；值為 `false` 時另附 `note`，說明這筆結算有記錄但不計入校準。

`belief_stats` 回傳的內容（與儀表板 `belief.summary` 的 `stats` 相同，另加一個 `note`）：

| 欄位 | 意義 |
|------|------|
| `n_submitted` | 提交過的所有信念，不論是否已結算 |
| `n_settled_all` | 所有已結算的信念（`verified.n + self_reported.n`） |
| `calibration_status` | `no_verified_settlements`、`insufficient_samples`（已驗證 1–29 筆）或 `calibrated`（30 筆以上） |
| `verified` | `n`、`hits`，以及 `hit_rate`、`hit_rate_wilson_low`、`mean_brier`、`overconfidence`（不是 `calibrated` 時都是 `null`） |
| `self_reported` | `n` 與描述用的 `hit_rate`（`n` 為 0 時是 `null`），不是校準 |
| `per_subject[]` | `subject`、`verified`（`n`、`hits`、`mean_brier`）、`self_reported`（`n`） |

舊版的扁平欄位（`n_total`、`n_settled`、`insufficient_samples`、最上層的 `hit_rate` 等）已移除。見[信念迴圈](../../features/zh-TW/46-belief-loop.md)。

### `create_agent` / `agent_remove`：移除後的名稱會被保留

`agent_remove` 會把該員工移到 `~/.duduclaw/agents/_trash/`，並回覆該員工已被移除、管理員可以還原、名稱已被保留，不會回傳路徑。接著 `create_agent` 會對所有 MCP 呼叫端拒絕這個名稱，條件是 trash 裡還有對應項目、`org.toml` 仍記錄這個 id 但沒有對應目錄，或 trash 無法列出；換一個名稱則可以建立。操作者可以從 Dashboard 或終端機重用這個名稱。透過 HTTP 且使用非內部金鑰時，兩個工具都以該金鑰自己的 client id 作為身分執行。細節見[委派隔離](../../features/zh-TW/37-delegation-isolation.md#被移除員工的名稱仍被保留)。

### `create_agent`：人格設定與名額（v1.82.0）

人格設定放在 `soul_md`（`soul` 是舊名稱；空字串視同沒給，兩個都有內容且不同才會被拒絕）。內容會經過和板模建立相同的 SOUL.md 安全檢查；檢查沒過或 SOUL.md 寫不進去，整個呼叫都會失敗，磁碟上不留任何東西；拒絕訊息會說明原因類別（HTML 註解、不可見字元、隱藏內容、提示注入句型），但不引用內容。工具沒有讀的參數不會被拒絕，回覆會在「Ignored arguments (no effect)」列出（最多 20 個名稱、每個最多 64 字，其餘以「… and N more」表示）。授權名額計算所有狀態不是 `deleted` 的員工（已封存的員工仍佔名額）；額滿拒絕時只回覆上限與目前已使用的名額數，不列出佔用名額的員工（管理員可在儀表板看到清單）。名額檢查與建立是同一個步驟，和儀表板、專家包匯入共用同一把鎖，同時建立也不會超過上限。員工不能用檔案工具修改自己的 `[agent] status`，`agent_update` 也不能變更已封存或已刪除員工的 `status`；封存、恢復與刪除只能在儀表板操作。之後要改人格設定，見[建立之後修改員工的人格設定](dashboard-settings.md#建立之後修改員工的人格設定v1820)。

### `responsibility_get` / `responsibility_followup` / `responsibility_ask`：持續任務工具

[持續任務](continuous-responsibilities.md)的三個工具。只有 `config.toml [responsibilities] enabled` 為真、而且呼叫端是 AI 員工身分時才會列出（外部用戶端看不到）；功能關閉時每次呼叫都會被拒絕。它們和 `tasks_*` 工具一樣需要 Admin scope，閘道自己的金鑰帶有這個 scope。

| 工具 | 誰能呼叫 | 規則 |
|---|---|---|
| `responsibility_get` | 任何 AI 員工；操作者金鑰可以讀任何持續任務 | 不帶 `responsibility_id`：列出呼叫者自己的。帶 id：回傳摘要（排程、訂閱、本期花費與次數、進行中的那一次、`cost_not_counted`）。別的員工的持續任務會回「找不到」，除非委派規則讓呼叫者與擁有者有關係 |
| `responsibility_followup` | 擁有這個持續任務的 AI 員工，而且只能在它自己的那一次執行進行中、持續任務為 active 時 | 在 `due_at`（RFC 3339）安排一次性喚醒，至少距現在 `min_wake_interval_secs`，不晚於 `stop_at`。計入本期次數上限，額度用完就拒絕；不會被悄悄改到別的時間 |
| `responsibility_ask` | 同 `responsibility_followup` | 一個問題（最多 1000 字），最多 5 個選項（每個最多 1000 字）；`ttl_secs` 預設一天，限制在 60 秒到 `stop_at` 之間。問題會做注入掃描，給操作者看時以引用文字呈現並截到 200 字，掃描命中時附警告。推播走持續任務的通知條件與每期上限。答案在下一次執行以資料形式交給員工，不給任何權限。停止那一次執行會一併撤回這個問題 |

每個持續任務同時最多一個由 AI 員工安排、還在等的喚醒，所以等待中的 `responsibility_followup` 與 `responsibility_ask` 互斥。操作者金鑰不能用這兩個喚醒工具，因為那等於冒充員工。封閉的錯誤代碼：`not_found`、`not_active`、`no_open_occurrence`、`invalid_due_at`、`period_occurrence_limit`、`agent_followup_limit`、`epoch_changed`、`invalid_question`。

沒有建立、修改、恢復或重新啟用持續任務的工具，也沒有送指示的工具。

### `knowledge_*`：文件知識庫

`knowledge_search`、`knowledge_get_passage`、`knowledge_list_collections` 讀取[文件知識庫](../../features/zh-TW/63-knowledge-base.md)。scope 是 `knowledge:read`，不能授權給外部 MCP 金鑰。每次呼叫都透過簽章本機路由 `POST /api/internal/knowledge` 交給 gateway；gateway 算出這次呼叫能讀哪些集合（授權給員工本人與其部門的集合，依 `agent.toml [knowledge] collections` 縮小，再與這個 turn 所服務的人、敏感度規則取交集），並寫一筆存取紀錄。gateway 沒在執行時工具會直接說明，不會自己讀資料庫。

結果都標明是文件內容：每一段內容（以及集合名稱與說明）放在有標記的區塊裡，每行以 `│ ` 開頭；標題與位置壓成一行，`[` `]` 會被換掉，文件無法偽造結果標題或提早結束區塊。`knowledge_search` 以 `[n] 標題 · 位置（集合） chunk_id=…` 編號，方便回覆時引用；匯入時被判定像指令的段落會多一行警告。`collections` 只能縮小範圍。`knowledge_get_passage` 每次都重新檢查權限，找不到的段落與沒權限的段落回應相同（`permission denied`）。在通道或 WebChat 回覆中，編號會接著這個 turn 的引用編號（已經放進 prompt 的段落保留原本的號碼），所以回答裡的 `[n]` 和讀者點開的是同一段。

`knowledge_add_source`（scope `knowledge:write`，同樣不能授權給外部金鑰）替員工這一側的某個集合送審一份文件：`collection`，加上 `path`（員工自己 `attachments/` 裡的檔案）、`url`、`text` 三選一，`title` 選填。它本身不會加入任何東西，只開一張 `knowledge_add_source` 審批單，由管理員或該集合的編輯者在儀表板決定，回傳審批單編號。在公開訪客、未綁定發話者或沒有登記的 turn 裡會拒絕，員工待審的審批單太多時也會拒絕。不論誰呼叫都沒有豁免。

`knowledge_search` 與 `knowledge_list_collections` 的回應第一行會說明檢索模式（混合＝關鍵字加語意向量，或只用關鍵字），沒有用到向量時附上原因（尚未安裝模型、雲端供應者未經確認、去識別化未啟用、雲端出錯等）；`knowledge_list_collections` 另外列出每個集合的向量覆蓋率。向量只會在這次呼叫能讀的集合裡排序段落。

### `odoo_schema_overview`：已存的 Odoo 結構

- `odoo_schema_overview` 需要 **`odoo:read`**。不帶參數時列出已掃描模型名稱與短標籤；`model` 可指定模型並取得欄位，`query` 搜尋模型／欄位名稱與標籤。結果依員工的 Odoo 模型／動作設定、功能模組開關及套用該員工實際解封清單後的封鎖清單限縮，不會回傳封鎖模型的名稱、欄位或關聯；能看結構不代表可以讀預設封鎖模型的業務資料。
- 結構掃描僅供管理員在 dashboard 按「重新掃描 Odoo 結構」（`odoo.schema_scan`）；每個已存 profile 每 60 秒最多嘗試一次，失敗也計入且重啟不重設。總覽明示內容來自不可信的 Odoo 中繼資料，篩查並限長標籤，丟棄不合法技術名稱。
- `odoo_schema_fields {model}` 仍查指定且獲准模型的即時中繼資料；封鎖模型會在連線 Odoo 前拒絕，指向員工看不到之模型的關聯會省略。內建讀取與帶明確欄位清單的通用讀取會透過即時 `fields_get` 移除不存在的欄位；中繼資料失敗或沒有欄位留下時拒絕讀取。
- 持有執行個體鎖的 gateway 啟動後，每 30 分鐘檢查所有有效連線；快照缺少、不完整、伺服器版本不同、已安裝模組名稱／版本變更或缺少權限中繼資料時，會在背景掃描。掃描成功或明確失敗後才計入每小時一次的自動掃描額度，中途取消不占額度。MCP 對話程序不啟動掃描；準備期間可先用 `odoo_schema_fields` 查詢單一資料表。版本與模組清單都沒變的純欄位客製，仍需手動重新掃描；已連線員工可用 `odoo_connect` 更新即時欄位快取。

每次最多保留 **400 個模型、每模型 500 個欄位**。中繼資料與欄位探索上限 **5 分鐘**，`fields_get` 同時最多四個；帳號權限探測另外設 30 秒整體上限。優先順序為內建工具模型、已安裝業務模組的模型（CRM、銷售、採購、庫存、會計、專案、人資、製造、客服及網站），其餘按名稱排序。逾時會標記不完整；身分、版本與模組未變時，下輪沿用已取得的完整欄位，接續處理未完成模型。模組／模型目錄各上限 5,000 筆；不完整的差異不代表資料已刪除。

### `odoo_create` / `odoo_write`：通用新增與修改（v1.81.0）

- `odoo_create {model, values}` 新增一筆紀錄並回傳 ID；`odoo_write {model, ids, values}` 修改 1–100 筆紀錄（重複 ID 只算一次）並回傳 `ok`。兩者都需要 **`odoo:write`**，效果類別為 `modify`，`action_rules`、審批清單與探索通道的規則與 `odoo_crm_create_lead` 相同。
- **明確授權之外，還要另外開啟。** 員工必須有自己的 Odoo 設定（`agent.toml [odoo]`），`allowed_models` 要寫出該資料表的完整名稱，`allowed_actions` 要有 `create`／`write`（或 `create:<model>`／`write:<model>`），**而且** `allowed_actions` 還要有該資料表的開啟標記 `generic_create:<model>`／`generic_write:<model>`。這個開關預設關閉，沒有「全部資料表」或 `*` 的寫法；**既有設定不會改變**：原本已給「新增」／「修改」的設定，在開啟之前不會多出這兩個工具。沒有自己的設定、清單留空或寫 `*` 都不算授權。這比其他 Odoo 工具嚴格（其他工具清單留空代表不限制），其他工具的行為不變，也不理會開啟標記。在 dashboard 設定：員工 → 編輯 → 整合 → Odoo，勾選資料表，在該資料表的規則中給它「新增」或「修改」，再開啟旁邊的「允許通用新增／修改」；開啟後 AI 可以設定這張資料表任何可寫入的欄位。只有當某張已授權的資料表也通過「永不開放通用寫入」清單、功能模組開關與封鎖清單時，工具才會出現在清單中。
- **永不開放通用寫入**（不論授權、開啟或解除封鎖設定，一律回覆「這項功能不提供」）：`res.users`、`res.groups`、`res.company`、`res.users.apikeys`（以及其他 `res.users.*`／`res.groups.*` 資料表）、所有 `ir.*` 資料表（含 `ir.actions.*`、`ir.ui.*`）、`mail.mail`、`mail.followers`、`mail.message`、`mail.template`、`mail.compose.message`、`discuss.channel`、`mail.channel`（及其 `.*` 子資料表）。透過 x2many 指令碰到的資料表也套用同一份清單。
- 改動任何紀錄前依序檢查：永不開放清單、授權與開啟、功能模組開關、封鎖清單，接著在本機檢查 `values`（必須是物件，最多 64 KiB；x2many 指令清單只接受 0 新增、1 修改、4 連結，2 刪除、3 解除連結、5 清空、6 設定一律拒絕；最多巢狀 3 層；`active` 只能設為 `true`，封存一律拒絕），連線後再依結構快照檢查帳號權限（`create`／`write` 必須確定允許，未知也拒絕）與 `values` 的每個欄位：必須是有儲存、非唯讀，且不是關聯欄位（related／`_inherits`，會寫到另一筆紀錄）的欄位。One2many／many2many 的值必須是非空的指令清單：`false`、`null`、`[]` 與單純的 ID 清單（會清空或取代整組）一律拒絕。指令碰到的每張資料表都要通過同一份永不開放清單、功能模組開關與封鎖清單；指令 0 與 1（以及 one2many 上的 4，會把紀錄從原本的上層移走）還需要該資料表自己的授權、開啟與帳號權限，巢狀的值也依該資料表的結構快照檢查。單純連結（many2many 上的 4）不需要該資料表自己的授權。這條連線還沒有結構快照時，會請員工等背景結構準備完成；v1.81.0 之前掃描的快照沒有記錄關聯欄位，這類欄位在重新掃描結構前一律拒絕。不提供刪除與封存紀錄。
- 稽核紀錄只寫模型、欄位名稱與筆數，不寫欄位值；呼叫失敗時只記錄錯誤類別與長度，不記錄 Odoo 的錯誤內容。

總覽不帶篩選時只列模型名稱與短標籤，上限 16 KiB；指定 `model` 或查詢只命中最多五個有權限的模型時才回傳欄位，上限 32 KiB。回應保留符合條件的模型總數及截斷標記。共享 Wiki 摘要上限 16 KiB；`odoo/schema-fields` 僅作為搜尋用的 `deep` 參考頁，上限 32 KiB，不會自動放入員工上下文。

重新掃描會涵蓋已儲存的全域連線，以及每位員工合併後的 `[odoo]` 設定與公司範圍。同一連線身分只掃描一次，同時最多掃描兩個連線；畫面逐一顯示各員工「已準備好」或失敗原因。不同 Odoo 使用者的快照各自隔離。兩個儀表板掃描動作共用持久保存的 60 秒手動冷卻時間。成功準備後，只有在所有有效身分都能確認時才清理過期快照資料；清理會持有目錄鎖再刪除閒置鎖檔，執行中的掃描與鎖檔都會保留。

## 已棄用的別名仍會列出

已棄用的工具名稱仍會出現在 `tools/list`，說明前面加 `[deprecated → …]` 前綴，因為隱藏它等於讓它叫不動，那和棄用緩衝期的用意正好相反。完整的舊 → 新對照表在 [deprecations.md](../deprecations.md)。目前沒有任何 MCP 工具處於棄用狀態：v1.66.0 那一批別名已在 v1.69.0 移除。

## 相關文件

- [custom-mcp-tool.md](../custom-mcp-tool.md) — 新增工具
- [mcp-bridge.md](../mcp-bridge.md) — 掛載外部 MCP server
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth 傳輸
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 規格
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` 工具，由直連 API 的 tool loop 注入，不經 MCP server 派送

## 宣告憑證用途

`credential_declare` 接受 `credential_id` 與 `service`，拒絕員工備註；服務必須符合 `^[a-z0-9][a-z0-9._-]{0,63}$` 並通過憑證偵測。只有通過身分驗證且持有該憑證的員工，才能經過簽章的 gateway loopback 路由宣告；員工不能覆蓋 Admin 設定的服務。這個工具只註記既有稽核資料，不取出秘密、不授權新持有者，也不把真值代入工具呼叫。Admin 可到「安全 → 憑證稽核」查閱與更正。詳見[憑證稽核](../../features/zh-TW/70-credential-audit.md)。


## 第三方 MCP 環境變數憑證

第三方 MCP 金鑰會移出員工的設定檔。`.mcp.json` 只保留 `secret://mcp/<server>/<ENV_NAME>` 參照，實際值使用既有 `.keyfile` 加密，存於 `<home>/secrets/mcp/<agent_id>.enc`。第三方伺服器的所有 `env` 項目都存入憑證庫，例外只有 `PATH`、`HOME`、`NODE_ENV`、`LOG_LEVEL` 與既有 `secret://` 參照。DuDuClaw 自身的伺服器項目與其中的 `DUDUCLAW_*` 變數維持不變。

只有精確名稱 `duduclaw`、`duduclaw-pro` 或目前 instance 的 key 才是內建欄位；`duduclaw-evil` 仍視為第三方。產生有效設定時，內建欄位的 command、args、env 整筆換成主程式當次產生的 canonical 設定，因此金鑰輪替、執行檔改名或偽造 command 都不會沿用原始值。任何名稱的 raw `mcp-proxy` 呼叫仍被拒絕，第三方也不能攜帶 proxy 私有環境變數。Claude 在沒有代理用途或只有內建伺服器時維持不加旗標；內建設定仍由原本的啟動準備流程修復。Fork 複本的來源可能是未經員工準備流程的一般專案，因此即使 gate 關閉，複本仍會重建內建欄位。讀取秘密必須有對應憑證庫擁有者的有效 `DUDUCLAW_AGENT_TOKEN` HMAC，即使 delegation 使用寬鬆設定也不能省略，且不會以預設員工身分代替。

Proxy 只解析自己所屬員工的參照，並在啟動上游伺服器前才取得實際值。它用主機放進 proxy 項目的 `DUDUCLAW_AGENT_TOKEN` 證明是哪位員工：Claude 放在每次啟動專用的 MCP 設定（只有擁有者可讀的暫存檔，CLI 結束即刪除），Codex、Grok、openai-compat 放在啟動環境；員工的 `.mcp.json` 不會替 proxy 存放這個權杖，上游伺服器也拿不到它。參照不存在、不合法或無法解密時，該伺服器不會啟動；錯誤只指出伺服器與變數名稱，不含值。需要解析秘密的設定若無法改寫為 proxy 路徑，Claude／Grok 不會退回直接載入原檔。匯出時只留下環境變數名稱與空白佔位值，匯入者必須重新輸入。

Gateway 啟動時會遷移舊有明文 env：先將原檔加密備份至 `<home>/backups/mcp-migrate/<agent>-<timestamp>.enc`，寫入並讀回驗證憑證庫後，才以原子操作替換設定檔。依金鑰、加密備份、憑證庫、設定檔的順序，每一步都須完成父目錄 fsync 才繼續。任一步失敗即停止該員工的遷移；若替換設定後同步失敗，會還原原始明文。還原也失敗時會明確報錯並保留已持久化的加密備份供復原，然後繼續處理其他員工；清理後每位員工最多保留三份遷移備份；清理若因 I/O 錯誤失敗會記錄，並於下次掃描重試。刪除伺服器或員工會移除對應的憑證庫項目，復原已刪除員工後須重新輸入金鑰。

Read 防護會拒絕讀取 `<home>/secrets`。這不是對 Bash 的隔離：員工與 gateway 使用相同 OS 使用者執行時，有 Bash 仍可讀取 gateway 檔案，包括 `.keyfile`；真正隔離需要不授予 Bash，或使用任務沙箱。這項功能只涵蓋 `env`，不涵蓋 `args`／`url` 裡的金鑰，也不涵蓋工具結果回傳的金鑰。既有明文匯出與備份不會被追溯清除。

## Google Chat 讀取工具

`google_chat_spaces` 列出已連結帳號加入的聊天室、群聊與私訊，包含資源名稱、顯示名稱、類型及 Google 有提供的最後活動時間。`limit` 預設 20，範圍 1–50；`filter` 僅接受 `SPACE`、`GROUP_CHAT` 或 `DIRECT_MESSAGE`。

`google_chat_messages` 以嚴格驗證的 `spaces/...` 資源名稱讀取近期訊息，從新到舊排列。`limit` 預設 30，範圍 1–100；選填 `since` 必須是 RFC3339 時間，`thread` 必須是該聊天室內的討論串資源名稱。回傳發送者、建立時間、文字（每則最多 2,000 字元）、討論串及附件名稱，不下載附件。發送者名稱採 Google 回傳的顯示名稱，缺少時顯示 `users/...`；不呼叫 People API，也不另索取會員權限。整份回覆最多 30,000 字元。

1. 管理員在儀表板「管理 → 整合 → Google」分頁啟用「Google Chat 讀取」（`[google] chat_read = true`，預設 false）。
2. 使用者到「我的連線」重新連結自己的 Google 帳號，勾選兩項 Chat 讀取權限。額外 scope 為 `https://www.googleapis.com/auth/chat.spaces.readonly` 與 `https://www.googleapis.com/auth/chat.messages.readonly`；開關關閉時不會要求既有使用者追加同意。
3. 在 OAuth 用戶端所屬的 Google Cloud 專案啟用 **Google Chat API**，並完成 **Chat 應用程式設定**，即使使用個人帳號授權也需要。Workspace 管理員可能還需允許這個 OAuth 應用程式。

兩項工具沿用 Gmail 讀取的員工與帳號綁定、scope、核准及私有資料外送閘，使用既有身分政策選定的帳號，不接受任意指定使用者或帳號的參數。Apps Script 不支援這兩項讀取。這與只接收私訊和提及的 Chat 機器人通道不同。關閉開關會阻止後續讀取，即使權杖仍有效；不會撤銷 Google 端已同意的權限。

回傳內容只作為不可信參考資料，使用群組歷史的包覆格式與注入掃描；任何命中都會拒絕整次回覆，顯示 `not available`。稽核僅保留聊天室識別碼與數量，不記錄訊息文字。本機與固定資料測試不代表真實帳號可讀，交付前仍須以客戶帳號驗證同意流程及讀取結果。

## `mail_send` — 寄信由交辦人在原對話確認

`mail_send` 沿用收件人與資料外送防護。已驗證且目前有該員工 Operator 操作權限的交辦人，僅可在一對一對話或已登入的儀表板 WebChat 確認完整收件人、主旨與內文摘要；不符資格時由儀表板 Admin 決定。核准綁定最終參數，只能用一次，逾期視為拒絕。`[security] mail_requester_approval = false` 恢復僅 Admin 核准；預設 `true`，但設定無法讀取或格式錯誤時採拒絕此例外。詳見[資格與限制](../../features/zh-TW/05-security-defense.md#寄信由交辦人在原對話確認)。群組對話與未知聊天類型仍須儀表板 Admin 核准，公開 WebChat 小工具訪客不適用。不符資格的通道通知不含收件人、主旨或內文，完整內容只留在儀表板。

此確認只放行對外資料閘門。原生郵件工具仍會建立既有的寄件匣草稿確認；通過此閘門不代表郵件已寄出。
