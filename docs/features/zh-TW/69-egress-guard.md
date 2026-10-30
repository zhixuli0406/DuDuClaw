# 資料外送防護

v1.77.0 起提供。AI 員工在一輪工作中讀取私人資料後，後續的 MCP 外送呼叫會先經過程式檢查，再決定是否執行；判斷不交給模型。

## 選擇處理方式

開啟 **安全 → 資料外送防護**。預設「僅記錄」（`shadow`）仍允許執行，並在工具呼叫紀錄留下 `would_ask` 與來源工具名稱。「送出前詢問」（`ask`）要求人工核准；「阻擋送出」（`block`）直接拒絕；「關閉」（`off`）停用這一層。

```toml
# config.toml
[security]
egress_guard = "ask"

# agent.toml
[capabilities]
egress_guard = "block"
```

全域與員工設定取較嚴格的一個：`off < shadow < ask < block`。員工設定不能放寬全域限制。全域設定檔不存在時預設為 `shadow`；員工設定檔不存在時沿用全域設定。其他讀取錯誤或設定值無效時，該呼叫會被阻擋。原有工具黑名單、權限與動作規則仍然生效。

## 何時觸發

私人讀取執行前會先在 gateway 登記進行中的讀取，其他 MCP 程序也會視為已污染。只要回覆含有模型可見資料，就轉為正式來源，包含帶資料的錯誤回覆；只有確認完全沒有資料的失敗才釋放登記。取消或完成狀態不明時保留標記。範圍包含記憶、Wiki、知識庫、本機檔案、資料庫、郵件、Google 資料、任務、活動與工作狀態等；所有第三方 MCP 工具也算私人讀取。每個平台工具都有精確名稱分類，測試會檢查是否遺漏；未知工具預設算私人讀取。

`wiki_write` 最終參數為 `scope="shared"` 時屬於發布；私人範圍維持修改。分類使用政策改寫後的參數。

讀過私人資料後，防護涵蓋「傳送、發布、購買」類平台工具，以及會把參數送到網址或目的地的工具，包含網頁擷取。即使是讀取網頁，網址本身也可能夾帶私人資訊。精確工具表也涵蓋遠端搜尋與查詢參數（Google、Notion、GitHub、Odoo 與資料庫）、瀏覽器導覽與輸入、遠端語音處理、模型與技能搜尋下載，以及任務或排程更新中的通知目的地。第三方 MCP 代理則檢查非讀取決策。

Gateway 以 `DUDUCLAW_TURN_ID` 保存記憶體紀錄，期限六小時。MCP 程序透過簽章的本機請求查詢，也會保留六小時內只會升高的本機標記。每筆快取最多 512 個來源，超出的來源折疊為 `additional_private_sources`，並清理過期快取。只有 gateway 已登記的有效回合能回報尚未讀取私人資料。回合識別缺少或未知、紀錄過期、gateway 重啟或無法連線、回應無法驗證時，都視為已讀取私人資料。

外部 API key 與 HTTP MCP 呼叫也套用全域設定。這些呼叫沒有 gateway 登記的回合，一律視為已污染，不會借用 MCP 程序所屬員工的乾淨回合。

已驗證員工即使缺少回合 ID，或使用未知、偽造的 ID，私人讀取仍會經過 gateway 檢查。`ask`／`block` 下，只要同一員工有任何外送 lease，就拒絕讀取，包括另一個有效回合的讀取。lease 資料庫可讀且沒有有效 lease 時，未知或無回合的讀取仍可執行、不建立 reservation，並稽核來源。獨立的 `duduclaw mcp init` 與外部 API key 使用獨立的外部身分，仍可讀取；外送一律視為已污染，在 `ask` 要求核准、`block` 拒絕。員工若無法連到 gateway 或驗證回應，即使沒有回合 ID，`ask`／`block` 也拒絕讀取；`shadow` 維持允許並稽核。

外送 lease 會先寫入 gateway 管理的 `<home>/egress_leases.db` 才授予呼叫端（SQLite WAL、synchronous FULL；Unix 上資料庫與 sidecar 權限為 0600）。每列記錄 lease ID、已驗證的員工 ID、回合 ID、階段與 UTC 取得時間。因為呼叫端收到授權後可立即送出，磁碟階段在授權前先標成 `Dispatched`；傳輸端仍保留本機的「尚未送出」狀態，供安全取消使用。釋放時先刪除磁碟列才移除記憶體保護。新增失敗時，`ask`／`block` 拒絕外送，`shadow` 允許並稽核；刪除失敗則保留保護。

Gateway 在開放 loopback 路由前，將所有格式有效的列恢復為「已送出、終止結果未知」。復原的 lease 只能到期清除，即使舊程序還活著並送來 release 也不能提早釋放。每次重啟都為復原列重新起算完整六小時的單調時鐘保護，不依 UTC 時間戳或時鐘跳動縮短。只有存續中的 lease 確認終止，或持續運行期間單調時鐘到期，才會刪列。反覆重啟可能延長保護；每位員工與全域容量上限仍適用。HTTP 准入與定期紀錄維護在 blocking pool 執行，繼續共用同一把帳本鎖，合計最多 64 件排隊或執行中的工作。非 loopback 來源在排入 blocking 工作前就拒絕；容量已滿時，准入立即回傳 HTTP 503，強制模式呼叫端會拒絕執行。資料庫無法開啟、讀取或遷移時，`ask`／`block` 暫停所有員工的私人讀取（包括有效與未知回合）及新外送，`shadow` 允許並稽核。每五秒重試開啟，每分鐘最多記錄一次錯誤。原本固定 300 秒的重啟窗已移除。回合的污點來源仍只保留在記憶體，重啟後未知回合一律視為已污染。

Store 在存續期間持有 `<home>/egress_leases.lock` 獨占檔案鎖（Unix 0600）。使用同一 home 的第二個 gateway 會拒絕讀取與新外送並重試，取得鎖後先恢復所有 lease 才接受讀取；`shadow` 仍允許並稽核。Gateway 執行中不可刪除鎖檔。重複的有效 lease ID 會在寫入前拒絕，不會連帶阻擋其他員工。

每位員工有獨立且容量固定的 Bloom filter 記錄回合登記；同一 gateway 程序存續期間不清除，因此被淘汰或到期的回合重新登記也不能變回乾淨。雜湊碰撞或 filter 飽和時，可能保守地將該員工的新回合視為已污染。最多保留 1,024 位員工的歷史，每位 32 KiB、合計 32 MiB；超過此上限的其他員工，其回合一律視為已污染。這份歷史不跨重啟保存。

## 復原

檢查 gateway 日誌中的 `egress lease store unavailable; private reads fenced` 與 `error` 欄位。超限會顯示 `egress agent lease store over capacity` 或 `egress lease store over capacity`；格式異常列會顯示 `invalid egress lease row`，SQLite 檔案損壞則可能顯示資料庫錯誤。Gateway 每五秒重試，每分鐘最多記錄一次錯誤。`ask`／`block` 下，私人讀取與新外送會被拒絕。Gateway 回覆的拒絕原因為「外送防護紀錄無法讀取，暫停私人讀取及對外動作；請稍後重試。」，呼叫端也可能顯示一般的稍後重試提示。目前儀表板與 `doctor` 沒有專用的 lease store 健康狀態指標。

外送動作（sink）可能仍在執行時，不可直接刪除資料庫：其中的列可能是唯一留下、能阻止平行私人讀取的 fence。若紀錄無法恢復，請依序離線處理：

1. 停止使用這個 home 的所有 gateway，並暫停自動重啟。
2. 將所有現存的 `<home>/egress_leases.db*` 檔案一起備份到私人位置，包含 WAL／SHM sidecar，保留原權限。
3. 確認沒有員工 CLI 程序及其 MCP／工具子程序仍在執行，且它們的外送動作已停止。無法確認時，保持 gateway 停止，不要重設紀錄。
4. 將資料庫與所有對應 sidecar 一起移到別處，保留備份；不要刪除 `egress_leases.lock`。
5. 啟動 gateway，系統會建立空的 lease store。**所有先前的外送 fence 都會丟棄。** 若舊動作仍在執行，就會失去防止平行私人讀取的保護，因此只有完成第 3 步確認後才能重啟。確認日誌不再出現該錯誤，並檢查私人讀取與外送准入恢復正常。

後續規劃提供受支援的離線復原指令；本版沒有復原 CLI。

## 核准前看清楚

外送核准一律需要目前仍具 Admin 權限的帳號決定，即使沒有工作流程綁定也適用，通道按鈕不能核准。僅管理員可查看的儀表板外送核准卡會顯示目的地、預設展開且可捲動的完整參數、來源工具與到期時間。傳送、發布與購買的核准卡也顯示完整參數。每次核准綁定員工、工具與完整參數摘要雜湊，只能用一次。若動作規則原本就要求詢問，外送原因會附在同一張核准卡。

動作規則要求核准時，卡片會列出命中的工具或動作類別規則，讀取與草稿工具也會顯示。購買與刪除不能新增常設能力授權或工作流程授權；舊授權會被忽略，原紀錄保留。其他檢查與逐次核准仍然適用。

## CLI 內建工具（未發布 E1）

Claude 與 Grok 安裝 `PreToolUse` hook，涵蓋 `WebFetch`、`WebSearch` 與 Bash 字面網路指令。Grok 別名包含 `Fetch`、`Shell`、`web_fetch`、`web_search`、`run_terminal_command`；實際 matcher 與環境繼承仍待真 CLI 驗收。一般非網路 Bash 在本機直接放行，不聯絡 gateway。偵測清單為 `curl wget nc ncat netcat socat ssh scp sftp rsync ftp telnet http httpie aria2c lftp`，以及 `git push/fetch/clone/pull/ls-remote`。Shell `-c`、`eval`、echo/printf 參數最多重解析兩層，每段最多 64 KiB；echo/printf 中長得像指令的資料，即使沒有實際執行，也可能被保守列入。

| 模式 | 未污染 | 已讀私人資料 | 帳本無法讀取或未知回合 |
|---|---|---|---|
| `off` | 放行 | 放行 | 放行 |
| `shadow` | 放行 | 放行，記錄 `would_ask` | 放行，記錄 `would_refuse_ledger_unavailable` |
| `ask` | 放行 | 開審批，拒絕並等待重試 | 拒絕 |
| `block` | 放行 | 拒絕 | 拒絕 |

Hook 將原始工具參數送至 `/api/internal/egress-guard/hook-check`，由 gateway 正規化、雜湊並產生卡片；不接受呼叫端提供的 digest 或摘要。請求與回覆都有簽章，埠從 `config.toml` 讀取，不依賴環境中的埠變數。Hook 有獨立的每員工每分鐘 120 次額度。審批的委託者來自 gateway 登錄的執行脈絡；無法驗證時不開單，避免略過禁止自行核准的政策。

`ask` 第一次先開單並拒絕；目前具 Admin 身分的人在儀表板核准後，於同一回合以完全相同參數重試。審批綁定員工、回合、工具、完整參數及私人資料來源；`WebFetch` 包含完整 URL 與查詢字串。相同待決請求重用同一張卡，新增來源使舊卡失效。核准只可消耗一次，再重試需要新核准。Bash 只有一個直接、簡單指令與一個網路動詞能開單，不接受展開、替換、管線、重導向、`@file`、包裝指令或解釋器；需要核准時，不支援的形狀直接拒絕並解釋原因。綁定的是文字，不是 DNS 結果、執行檔內容、設定檔或符號連結指向。

`ask`／`block` 的 hook 錯誤與可捕捉的 panic 一律 exit 2；安裝失敗停止啟動，不輪替帳號或 runtime。`shadow` 記錄警告並繼續。Claude 在強制模式使用 `--setting-sources "" --settings <file>`，只載入身分所屬的 `.claude/settings.json`，不併入工作目錄或使用者的其他設定，即使工作目錄不同也一樣；強制模式拒絕會略過 hook 的 `cli_bare_mode`。Grok 加上 `--disable-web-search`；目前強制模式拒絕借用／共用工作目錄，或 hook 目錄內留有其他員工 guard 的組合，請使用獨立員工目錄或 Claude/Codex。

Codex 在 `ask`／`block` 加上 `-c sandbox_workspace_write.network_access=false -c web_search="disabled"`，FullAccess 降為 WorkspaceWrite，ReadOnly 保持更嚴，保留 MCP 的自動核准器。這是啟動時收緊網路，不是逐指令核准。通用 CLI 仍未涵蓋，強制模式記錄 `would_refuse_unsupported_runtime`。Antigravity 沒有擴充；OpenAI 相容工具迴圈維持 MCP 閘門。

Cron、派工、提醒、信件、heartbeat、ephemeral 執行入口改為獨立於通道 requester 登錄 host 回合並傳遞 `DUDUCLAW_TURN_ID`，也登錄 `default` 實際指向的員工。既有污染不會被清除；未知呼叫端回合仍視為污染。

**Bash hook 是減速帶與遙測，不是隔離邊界。** 內建 `Read` 與 Bash 讀檔不會標記污染。解釋器、腳本、別名、改名執行檔、`/dev/tcp`、動態組裝指令都可能繞過字面偵測。Hook 程式不存在、被殺或逾時，可能被上游 CLI 忽略。同一 OS 使用者可存取的 hook、金鑰與環境變數，不受簽章機制隔離。原生工具的事前檢查也不會持有 MCP sink lease 到執行結束，無法阻止檢查後的並行讀取。需要隔離時不授予 Bash，或使用任務沙箱；任務沙箱只涵蓋派工任務，不涵蓋所有通道／CLI 執行。安全頁會列出強制模式下有 Bash 且未開任務沙箱的員工。

E1 驗證狀態：已撰寫測試但未執行，本工作包未編譯、未做真 CLI 探測。待驗收：Claude/Grok matcher、回合與委託脈絡繼承、開單／核准／重試／單次使用、gateway 失聯的模式行為、Claude 借用工作目錄，以及 Codex 0.164.0 的網路／搜尋限制與 MCP 仍可用。

## 已知限制

直接由 `[[mcp.external]]` 掛入 API 工具迴圈的工具（stdio、HTTP 與 Google 封閉橋接）也會經過檢查。這條路徑的工具登錄不保留 MCP annotations，因此全部保守視為 **Modify**；即使伺服器自述唯讀，讀過私人資料後仍可能要求外送核准，精確工具名稱的動作規則仍適用。MCP 代理與遠端橋接則保留原有 annotations 與 `trusted_read_hint_servers` 判定。

這是每一輪的 MCP 邊界，不是網路沙箱。未發布的 E1 增加上述 CLI 內建工具檢查，並未提供整個 runtime 的網路隔離。提示中已有的私人資料、從範圍外工具取得的資料不會自動分類。新一輪使用新紀錄，不追蹤跨輪複製的資訊；gateway 重啟後不保留回合污點來源，但會恢復已持久化的外送 lease。「僅記錄」仍會允許外送。

另見[安全防護](05-security-defense.md)與[儀表板設定](../../guides/dashboard-settings.md)。

驗證範圍：自動化測試涵蓋分類、閘門與核准行為。尚未使用實際員工與外部服務完成端到端驗證。

直接外部連線的 scoped／closed 操作可能比核准卡更早逾時。逾時後再核准不會重新執行，必須重新提出工具呼叫；舊卡片可能持續顯示待核准，直到卡片上的期限。

第三方代理每個程序最多保留 512 筆待回覆呼叫，拒絕重複的進行中 ID，五分鐘到期。所有結束路徑會清除對應紀錄；取消與逾時仍保留 gateway 標記。程序一旦快取已污染狀態，即使後來確認無資料失敗，也會繼續採保守判斷。

員工的對外呼叫在實際送出前會向 gateway 取得外送 lease，等候核准後也會重新檢查來源。fence 綁定已驗證的員工身分，涵蓋該員工所有回合，不影響其他員工。lease 存續時，`ask`／`block` 立即拒絕新的私人讀取並提示重試；`shadow` 允許並記錄 `would_refuse_read_during_sink`。lease 分成尚未送出與已送出兩階段，在可能開始部分寫入前標記為已送出。送出前取消可釋放；送出後只有對應呼叫的終止回覆或已確認的上游取消能釋放。本機逾時、取消、工作中止、panic、部分寫入失敗或斷線，都保留 fence 到 **lease 取得後六小時**，不受回合期限影響。每位員工最多 512 個 lease，整個 gateway 最多 65,536 個；回合清理與容量壓力不會移除現有 lease。lease 帳本已滿時，強制模式拒絕新增 lease。回合上限為每位員工 4,096 筆、全域 65,536 筆；容量不足時先淘汰沒有 pending read、lease 或來源的乾淨回合。被淘汰或無法登記的回合仍視為已污染；回合容量不再連帶拒絕其他員工的 lease。核准後若新增來源，必須重新提出呼叫與核准。

代理的回覆去識別化紀錄也限制為 512 筆、五分鐘，重複 ID 不會覆寫原紀錄，本機拒絕的呼叫會立即清理。送往上游時使用獨立 ID，避免遲到的回覆誤用新呼叫的規則；到期回覆會被扣住。已完成的呼叫工作持續回收，同時執行最多 512 個。

內建工具依逐一核對的精確名稱清單判定完成。本機信件草稿、任務／員工佇列與提案，以持久寫入完成為準，不代表下游工作結束。錯誤回覆保守保留 fence。`model_search`、`model_download`、`skill_search`、`codrive_run`、`browser_record_start`、`os_open` 即使表面成功，也保留到 lease 期限：彙整報告、替代下載或背景工作無法確認每個對外動作都已結束。

Remote bridge 使用每次呼叫獨有的 generation wire ID，丟棄過期回覆，避免重用原始 ID 時，舊回覆提早結束新的外送 lease。呼叫端若使用保留的 `duduclaw-egress:` ID 命名空間，會收到明確拒絕。

## 由伺服器綁定收件對象（未發行 E2）

收件對象防護不以「讀過私人資料」為前提。`config.toml [security] recipient_guard` 支援 `off`、`shadow`（預設）與 `ask`；未知值會警告並採用 `ask`。員工層 `[capabilities] recipient_guard` 只能收緊全域設定（`off < shadow < ask`）。安全頁「資料外送防護」可調整全域設定；Admin 可在員工編輯頁「工具與權限 → 允許的收件對象」設定額外清單。

```toml
# config.toml
[security]
recipient_guard = "shadow"

# agents/<id>/agent.toml
[capabilities]
recipient_guard = "ask"
recipients = ["line:U123", "telegram:456", "mail:ops@example.com"]
```

允許集合是「本回合來源對話、已驗證的擁有者、員工設定的清單」。來源由 gateway 的回合表解析；MCP 只透過簽章內部路由傳遞不透明的回合 ID。未知回合沒有來源對話，`DUDUCLAW_REPLY_CHANNEL` 不授予收件許可。通道識別採 `parse_origin` 同款正規化，移除員工後綴與討論串後精確比對。

擁有者**只來自 `users.db` 中 `AccessLevel::Owner` 的綁定**，再解析為 gateway 驗證的通道身分。`[proactive]` 通知對象可由員工修改，因此不算擁有者；需要允許時，由 Admin 加進 `[capabilities] recipients`。整個 capabilities 區段原本就禁止員工改寫，兩個新鍵也受到保護。

| 模式 | 對象在允許集合內 | 對象在集合外 |
|---|---|---|
| `off` | 放行 | 放行 |
| `shadow` | 放行 | 放行，在 `tool_calls.jsonl` 記錄 `recipient_guard: "would_ask"` |
| `ask` | 放行 | 需要 Admin 單次核准 |

核准卡會寫明「目標不是來源對話，也不在允許清單中」。與外送防護同時要求核准時，只開一張卡並列出兩個原因。ActionGrant 綁定員工、工具及完整參數；修改對象或內容需要重新核准。shadow 不會因收件對象阻擋呼叫。

涵蓋 `send_message`、`send_photo`、`send_sticker`、`create_reminder`、`mail_send`，以及 `tasks_create`／`update_cron_task` 的 `notify_channel`、`notify_chat_id`、`notify_thread_id`。`calendar_create_event` 的 attendees 以郵件收件人檢查。排程通知在建立時及實際觸發時依當下設定重新檢查；觸發時來源集合為空，只有已驗證擁有者與設定清單能直接放行。

`responsibility_ask` 的核准對象由伺服器路由，不讓模型指定收件人；`os_notify` 只顯示本機通知。`github_issue_comment` 指定的是議題資源，仍受外送與動作政策約束。`send_to_agent`、`spawn_agent`、`team_handoff` 維持委派政策；第三方 MCP 的目標參數語意未知，維持外送防護。

實際傳送地址不能用對話後綴冒充清單中的收件人；Discord 通知 thread 會連同父頻道一起檢查。`update_cron_task` 修改通知目的地時，必須同時提供 `notify_channel` 與 `notify_chat_id`，空值配對可清除通知；任務與路由欄位一併原子寫入。等待其他核准後會再檢查收件權限。臨時助手繼承父員工的收件防護模式與設定清單，已驗證 owner 綁定仍以各員工為準。

E2 驗證狀態：已撰寫測試，但 **not run**。未編譯，獨立安全審查與 Claude／Codex 真實驗收仍待完成。這是產品路徑的防護，不隔離具有同一使用者無限制檔案或網路權限的程序。
