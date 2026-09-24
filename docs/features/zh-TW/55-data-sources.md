# 資料來源與原生資料庫連接器

> 去識別化規則能遮的欄位，前提是它找得到那個欄位。這篇講的就是「找得到」這件事怎麼做到：不論資料是從你自己的 MCP server 回來、從客戶自己的 MCP server 回來,還是 DuDuClaw 直接接進去的資料庫。

---

## 這個功能補上的缺口

2026-09 之前,`db_field` 規則(`fields = ["res.partner.name", "hr.employee.*"]`)只認得 Odoo。`model.field` 這套寫法是 `json_path` 規則的語法糖,而它展開時要對照的表(哪個工具會回傳哪張表、記錄在回傳 JSON 裡的哪個位置、欄位名經過 mapper 轉換後還在不在)是寫死在 Rust 常數 `ODOO_TOOLS` 裡的一張表。`db_field` 沒有辦法指向 Odoo 以外的任何東西。

底下還有一層更深的缺口。去識別化從頭到尾只攔得到 DuDuClaw 自己的 MCP server,節流點只有 `duduclaw mcp-server` 內的單一一處。客戶**自己**接的 MCP server(Postgres MCP、MySQL MCP、鼎新 ERP bridge,寫在 agent 的 `.mcp.json` 裡)是 Claude CLI 直接啟動的,回傳結果從來沒有經過這條管線。就算把 Odoo 那張表改成可插拔,對這些 server 一樣沒有幫助,因為資料根本沒有機會走到去識別化這一步。

這個功能同時補上兩塊:一份任何 `db_field` 規則都能指向的**登錄表**,以及兩種讓資料來源的記錄真的流經去識別化的路徑:給已經在用的 MCP server 用的**proxy**,以及給沒有 MCP server 的資料庫用的**原生連接器**。

---

## 兩種資料來源

| 類型 | 描述的內容 | 記錄從哪裡來 | 設定位置 |
|---|---|---|---|
| 經工具回傳的登錄項 | 哪些 MCP 工具會回傳哪張表的記錄,以及記錄在回傳 JSON 裡的位置 | 任何 MCP server(DuDuClaw 自己的,或是透過 MCP proxy 接進來的外部 server) | `[redaction.data_sources.<name>]` |
| 原生資料庫連線 | DuDuClaw 自己開的一條 PostgreSQL / MySQL / SQLite 連線 | DuDuClaw 自己的 `db_select` / `db_query` MCP 工具,不經過任何外部 server | `[db_sources.<name>]` |

兩種來源用同一套命名方式引用:`db_field` 規則的 `source` 欄位,以及儀表板上同一張「資料來源」卡片,兩種都會列在裡面。

---

## 第一層:資料來源登錄表

`[redaction.data_sources.<name>]` 描述三件 `db_field` 規則自己帶不了的事實:哪些工具會回傳一張表的記錄、每次呼叫時表名怎麼判斷、記錄放在回傳 JSON 的哪個位置:

```toml
[redaction.data_sources.crm_pg]
label = "客戶 CRM 資料庫"
tools = ["pg_query", "pg_select"]          # 至少一個,精確名稱或尾碼 * glob
table_arg = "table"                        # 跟固定的 `table = "customers"` 二擇一
record_paths = ["$.rows[*]", "$[*]", "$"]  # 這已經是預設值
key_alias = { name = "customer_name" }     # 只有工具會改欄位名時才需要
```

`db_field` 規則接著引用它:

```toml
[redaction.rules.crm_customers]
type = "db_field"
source = "crm_pg"          # `connector` 仍可用,是已棄用的別名
fields = ["customers.name", "customers.address", "orders.*"]
category = "DB_FIELD"
```

有兩個來源是內建的,不能被重新定義:`odoo`(原本的 `ODOO_TOOLS` 表,現在改用登錄項表達,共九個工具,固定表與動態表混合,經過 mapper struct 轉換的工具各自帶欄位別名)與 `duduclaw_db`(綁定 `db_select`,下面會說明)。`source` 與 `connector` 兩個都不填,仍然預設是 `odoo`,所以登錄表出現前的舊設定不會變意思。

表名不再一定要有點號。`db_field` 的驗證規則從 Odoo 的 `model.model` 形狀放寬,也接受一般的 SQL 識別字(`customers`、`public.customers`)。除此之外 `db_field` 的 fail-closed 行為不變:未知的 `source`、空的 `tools` 清單、寫錯的 `record_paths`、不符樣式的 `table.column` 字串,一律載入失敗,不會悄悄跳過。

---

## 第二層 a:MCP proxy,連別人寫的 server 也一起遮

`duduclaw mcp-proxy --server <name> -- <cmd> [args…]` 是一個隱藏的內部子指令:一支坐在 Claude CLI 與某個外部 MCP server 之間的 stdio JSON-RPC 轉發程式。當某個 agent 這一輪的去識別化是生效的,gateway 會改寫那個 agent 的 per-spawn `.mcp.json`,讓每個非 `duduclaw` 的**stdio** server 改成透過這支 proxy 啟動,而不是直接啟動。這次涵蓋通道回覆的 Claude CLI spawn(一般 spawn 與 one-shot PTY 備援,都在 `channel_reply.rs`),也涵蓋派工／cron／心跳／goal-loop 輪次的 spawn(`claude_runner.rs` 的 `prepare_claude_cmd`,經同一個共用的 `mcp_proxy_cli_args`)。proxy 套用的正是內建節流點在做的那兩個操作:

- `tools/call` 請求的 `arguments` 走一樣的 egress 決策(`Deny` 直接在這裡以 JSON-RPC 錯誤回覆,呼叫永遠不會到 upstream;`Allow` 可能會把白名單工具的 `<REDACT:…>` token 還原成真值);
- 對應回應的 `result` 走一樣的 `redact_value` 管線,工具名稱以 `<server>.<tool>` 加上命名空間,讓 `match_tool = "crm_pg.pg_select"` 不會意外命中同名的 DuDuClaw 工具。

其餘一切,包括 `initialize`、`tools/list`、通知、upstream 主動發起的請求,以及任何無法解析成 JSON 的行,一律原樣轉發。被包裝的 server 原本 `env` 區塊裡的憑證透過環境變數 `DUDUCLAW_MCP_PROXY_ENV`(一段 JSON)傳遞,不走 `argv`,因為 Linux 上 `/proc/<pid>/cmdline` 是任何人可讀,`/proc/<pid>/environ` 則不是。這裡一樣是 fail-closed:proxy 跑的是跟 `duduclaw mcp-server` 完全相同的 `McpRedactionLayer::try_init` 三種結果,去識別化未啟用就是純轉發,設定壞掉就是拒絕啟動,絕不轉發任何未遮蔽的內容。

**目前還沒涵蓋的部分:**

- **HTTP/SSE 型 MCP server。**這種 server 沒有子行程可以包,所以改寫邏輯直接放過它們,只留一行警告日誌,它們的工具結果照樣未遮蔽地送進模型。
- **PTY session pool**(`[runtime] pty_pool_enabled`,預設關,文件上標為備援路徑)。一個 pooled 的互動 REPL session 存活時間跨越多次呼叫,不是單次 spawn,這個改寫機制依附的「每次 spawn 一份暫存 `--mcp-config`」沒有地方可以掛;需要改成 session 自己持有的改寫,而不是每次呼叫各自持有,目前還沒做。上面的一般 spawn 與 one-shot PTY 不受影響。
- **codex／gemini／antigravity。**這幾個 runtime 自己的 MCP 註冊完全還沒接上 proxy。

---

## 第二層 b:direct-API 工具迴圈(openai-compat)

CLI 類後端靠 `duduclaw mcp-server` 或 `duduclaw mcp-proxy` 拿到去識別化;透過 `duduclaw-llm` 的 `run_tool_loop` 驅動的模型(openai-compat runtime,例如 API 模式的 Grok/DeepSeek/MiniMax agent)是直接跟行程內的 `ToolRegistry`對話,沒有子行程可以攔。`duduclaw-llm` 為此新增了 `ToolInterceptor` trait:`before_call(server, tool, args)` 可以在派發前拒絕或改寫引數,`after_call(server, tool, args, &mut result)` 可以在事後改寫模型會看到的結果。gateway 的 `RedactionToolInterceptor` 就是這個 trait 的實作,對接的是管線其他地方共用的同一個 `RedactionManager`,所以同一條規則不管模型是透過 `duduclaw mcp-server`、MCP proxy,還是這條行程內路徑接觸到工具,行為都一致。還有一條工具派發路徑兩邊都還接不到:`local_llm.rs`,本機推論模型呼叫工具的迴圈,它的工具結果目前一樣沒有被遮蔽。

---

## 原生連接器(`duduclaw-db`)

對完全沒有 MCP server、只有一顆 PostgreSQL / MySQL / SQLite 資料庫的客戶,`duduclaw-db` crate 是一套第一方、唯讀的 SQL 連接器,自帶四個 MCP 工具。因為它是透過 DuDuClaw 自己的 MCP server 派發,結果直接抵達去識別化節流點,不需要多跳一次 proxy。

### 三道各自獨立的唯讀保證

1. **語句守門**:最便宜、單獨看也最不可信的一層,必須是單一語句,以 `SELECT` 或 `WITH` 開頭,字串字面值以外不得有 `;`。一個會寫資料的 CTE(`WITH x AS (DELETE … RETURNING *) SELECT * FROM x`)故意能通過這一層,因為它就是單一的 `WITH` 語句,真正擋下它的是第二層。
2. **driver 層唯讀**:PostgreSQL 跑 `BEGIN READ ONLY`,MySQL 跑 `START TRANSACTION READ ONLY`,SQLite 以唯讀檔案控制代碼開啟。讓寫入真的不可能發生的是這一層,不是上面那個像解析器的守門。
3. **上限**:`max_rows`(預設 200,硬上限 1000)與 `timeout_ms`(預設 10 秒,下限 100 毫秒,上限 120 秒)限制單次呼叫能花多少成本。

### 設定一個來源

```toml
[db_sources.crm_pg]
label = "客戶 CRM 資料庫"
driver = "postgres"                       # postgres | mysql | sqlite
url = "secret://env/CRM_PG_DSN"           # 或 url_enc = "<密文>"
allowed_tables = ["customers", "orders"]  # 必填、不可空;["*"] 代表全部
max_rows = 200
timeout_ms = 10000
```

`url` 走專案的 `secret://<backend>/<name>` 憑證慣例。除非 driver 是 `sqlite`(此時值是檔案路徑而非憑證),否則明文連線字串會在載入時被拒絕:一條帶密碼的 DSN,本來就不該躺在 `config.toml` 裡。

`allowed_tables = ["*"]` 是允許的,而且是唯一會連帶解鎖 `db_query`(下面會說)的設定:一份真正的資料表清單代表「可以碰,但只能透過 `db_select`,而 `db_select` 的資料表是驗證過、綁定過的識別字」;沒有真正的 SQL 解析器,自由 SQL 沒辦法對照一份資料表白名單來檢查,所以只要有白名單,整個工具就整支拒用,不做半調子過濾。

### 授權某個 agent 存取

跟其他所有 capability 一樣,預設拒絕。現在通常不用再手改 `agent.toml`:可以在儀表板「設定 → 去識別化」的新增精靈第 4 步,或資料來源列表上的授權對話框勾選;也可以到該 agent 設定頁「工具與權限」分頁的「可使用的資料庫來源」清單勾選;或者直接在對話裡跟 agent 說「把這個來源開給你」,MCP 工具 `agent_update` 認得 `db_sources`／`db_sources_add`／`db_sources_remove`。三條路最後都落在同一個地方:

```toml
# agent.toml
[capabilities]
db_sources = ["crm_pg"]
```

MCP 派發節流點每次呼叫都重讀這份設定,所以不管走哪條路,授權變更立即生效,新的一輪對話馬上就看得到工具,不用重啟 gateway。

兩道關卡,兩個都要過:`Scope::DbRead`(`db:read`,第二十三個 MCP scope)加上這份非空授權清單,在 MCP 派發節流點就先檢查一次,任何工具處理函式執行前;接著在處理函式內部再檢查一次指定的 `source` 名稱,所以就算授權清單是 `["crm"]`,也碰不到剛好用同一種 driver 的 `payroll` 來源。

### 四個工具

| 工具 | 引數 | 回傳 |
|---|---|---|
| `db_sources` | 無 | `[{name, label, driver}]`:只列這個 agent 被授權**且**真的載入得起來的來源;已授權但壞掉的來源會附上原因,不會被悄悄漏掉 |
| `db_tables` | `source` | `{tables: [{name, columns: [{name, type}]}]}`,依 `allowed_tables` 過濾 |
| `db_select` | `source`、`table`、`columns?`、`filter?`(`[{column, op, value}]`,`op` 為 `= != < <= > >= like in` 之一)、`order_by?`、`limit?` | `{rows, row_count, truncated}`:識別字經過驗證並引號化,每個值都以參數綁定 |
| `db_query` | `source`、`sql`、`limit?` | 同樣的形狀。**只有來源的 `allowed_tables` 剛好等於 `["*"]` 才能用**,否則連連線都不開就直接拒絕 |

欄位值的轉換是刻意設計過的,不是 driver 解出什麼就照樣送:布林維持布林;整數與浮點數變成 JSON number;`NUMERIC`/`DECIMAL` 變成**字串**(用浮點數會讓金額進位跑掉);時間戳記變成 ISO-8601 字串;`bytea`/`BLOB` 變成 base64;JSON 欄位解析後回傳,不會被二次編碼。遇到任何一種這裡不認得的型別,會回傳誠實的佔位字串 `"<unsupported type: NAME>"`,不會用猜的、也不會悄悄丟掉:丟掉一個欄位鍵,會讓針對那個欄位寫的去識別化規則靜靜地什麼都命不中,沒有人會發現。

連線池是每次呼叫現開,不做快取,所以憑證輪替或 `[db_sources.…]` 設定的修改,下一次呼叫就生效,不用等重啟。

### 內建的 `duduclaw_db` 登錄來源

`duduclaw_db` 只綁一個工具 `db_select`,表名讀呼叫本身的 `table` 引數,記錄位置在 `$.rows[*]`,欄位名原樣通過不改名。這代表對**任何**一個 `[db_sources.*]` 連線寫 `db_field` 規則,都可以重用這一個內建來源,寫成 `source = "duduclaw_db", fields = ["customers.name"]` 即可,不用另外登錄任何東西,因為這個綁定只認 `table` 這個引數本身,不管是哪一個 `db_sources` 連線發出的呼叫。如果剛好有兩個不同連線都有一張叫 `customers` 的表,而你只想遮其中一個,就要改寫成一條明確的 `json_path` 規則,加上 `match_args = { source = "crm_pg", table = "customers" }`(`source` 與 `table` 都是 `db_select` 呼叫本身的頂層引數,可以任選一個或兩個都用來限定命中範圍)。`db_query` 在登錄表裡刻意不綁任何規則:一段自由 SQL 回傳的欄位由語句本身的投影決定,光看呼叫本身沒有真正的解析器,沒辦法知道「這一列屬於哪張表」。

---

## 地端檔案(CSV／Excel／文字)

`db_field` 規則能遮 Postgres 或 Odoo 的欄位,是因為兩條路徑最後都會走到一次 MCP 工具呼叫,那是去識別化唯一看得到 key 名稱的地方。agent 自己磁碟上的檔案,預設沒有這道節流點:Claude CLI 內建的 `Read`(以及透過 `Bash` 跑的 `head`／`cat`)不是 MCP 工具,`customers.csv` 這樣讀進來會整份原樣進模型的上下文,任何規則都看不到。通道附件讓這個缺口更明顯,不是更輕:`format_attachment_ref` 只把存檔路徑附在訊息文字後面,agent 照樣得靠 `Read` 才能打開它。`office_script`／`sheets_read` 雖然已經經過節流點,但只有樣態規則,一樣沒有欄位概念。PostToolUse hook 又改不了工具已經回傳的結果,「事後遮」從來就不是一個選項,唯一的路是把讀檔本身變成 MCP 呼叫,再把內建那條回頭路擋住。

### 三個工具,一道路徑圍欄

| 工具 | 引數 | 回傳 | 上限 |
|---|---|---|---|
| `file_read` | `path`、`max_bytes?` | `{path, table, text, truncated}` | 512 KiB,僅限純文字 |
| `csv_read` | `path`、`delimiter?`、`has_header?`(預設 `true`)、`limit?`(預設 200)、`offset?` | `{path, table, columns, rows, row_count, truncated}` | 檔案 ≤ 64 MiB,`limit` 上限 2000 |
| `xlsx_read` | `path`、`sheet?`、`limit?`、`offset?` | 同上形狀,另加 `sheet` 與 `sheets` | 檔案 ≤ 32 MiB(試算表解析器展開後遠大於磁碟大小,所以上限比 CSV 更緊) |

`table` 一律是檔名本身含副檔名,`customers.csv`,不是 `customers`。每個路徑在讀一個位元組之前都先過同一道圍欄:canonicalize,然後要求落在 agent 自己的目錄、`<agent_dir>/attachments`、`<home>/attachments`,或 operator 在 `config.toml [files] allowed_roots` 額外宣告的目錄之一。`..` 或逃出圍欄的 symlink 在 canonicalize 之前就先被拒絕,錯誤訊息才能點到真正的問題,而不是含糊的「不在允許範圍內」。這道圍欄上面沒有額外的 per-agent capability 關卡(跟 `db_sources` 不同),因為圍欄本身已經回答了「這是誰的資料」,所以三個工具一律列在 `tools/list` 裡,只受 `files:read` scope(`Scope::FilesRead`,第 24 個 MCP scope)把關。

`csv_read` 用 workspace 既有的 `csv` crate 讀取,沒有表頭時退回 `c1..cN` 欄位名。`xlsx_read` 用 `calamine`(新依賴,pure Rust,只開 `dates` feature)讀 xlsx／xlsm／xls／ods,cell 依序映射成 JSON 數字、布林、null(空值)、ISO-8601 字串(日期時間)、字串。兩個工具的稽核紀錄都不帶 cell 內容,只留 `path`／`table`／`row_count`,是「讀了什麼形狀」的紀錄,跟 `mcp_db.rs` 對 filter 值的規矩一樣。

### 內建的 `duduclaw_files` 來源

登錄表內建一個已經綁好兩個結構化讀取工具的來源:`table` 讀自工具的**回傳結果**,不是引數(`table_result = "/table"`),記錄位置在 `$.rows[*]`,`free_form_names = true`,檔案的表名與欄位名不必是識別字。就是這個旗標讓下面這種規則合法:

```toml
[redaction.rules.customer_files]
type = "db_field"
source = "duduclaw_files"
fields = ["customers.csv.name", "客戶清單.xlsx.地址"]
category = "DB_FIELD"
```

每一條都以**最後一個點**切開,`customers.csv.name` 是資料表 `customers.csv` 的 `name` 欄。因為資料表就是檔名本身,裡面沒有工作表資訊,`客戶清單.xlsx.地址` 這條規則不管 `xlsx_read` 當次打開的是哪張工作表都會生效,沒有工作表層級可比對,等於涵蓋整份活頁簿的所有工作表。中文欄位名底層會編譯成引號路徑形式(`$.rows[*]['地址']`),JsonPath 語法新增的 `['key']` 段落接受除了引號與換行以外的任何 Unicode 字元,就是為這個情境加的;純 `.key` 寫法仍然只接受 ASCII。`file_read` 沒有綁定任何規則,它回傳的是未結構化文字,只有樣態比對 pass 對它讀到的內容生效,不會有欄位規則。

### 資料檔守門

`[redaction] data_file_guard = "on" | "read_only" | "off"`,預設 `on`,只在發起呼叫的那個 agent 去識別化真的生效時才作用。它是一個 PreToolUse hook(`data-file-guard.sh`,隨既有安全 hook 一起安裝),讀 `DUDUCLAW_DATA_FILE_GUARD` 這個環境變數,由 gateway 在 spawn 時設定,不是 agent 自己讀得到或改得了的設定值。`on` 擋內建 `Read` 讀 `.csv/.tsv/.xlsx/.xlsm/.xls/.ods` 路徑,也擋指令文字裡含這些副檔名檔名的 `Bash`;`read_only` 只擋 `Read`;`off` 什麼都不擋。擋下時回傳的拒絕訊息是「此檔案受去識別化保護,請改用 csv_read／xlsx_read／file_read」。

兩個限制老實講清楚,不含糊帶過。`Bash` 那道檢查是檔名啟發式判斷,一條動態組出路徑的指令(`python -c "open(chr(99)+...)"`)直接繞過去。這個 hook 又是一支 shell script,跟它的姊妹 `agent-file-guard`(刻意寫成 Rust 子指令好讓它能在 Windows 上跑)不同,在沒有 `bash` 在 `PATH` 上的 Windows 主機,這個 hook 指令本身會執行失敗,Claude Code 把非 2 的結束碼當放行,守門在那裡就等於不存在。這兩個缺口不會列為待修 bug。PreToolUse hook 這種機制本來就只能做到這樣:降低模型不小心走上未遮蔽路徑的機率。真正的保護面是 MCP 工具本身,一條綁定 `duduclaw_files` 的規則只有在值進了 `$.rows[*]` 以後才看得到,守門上游有沒有擋下什麼都不影響這一點。

### 附件提示

`format_attachment_ref`,也就是 gateway 附加在通道附件參照後面那行文字,現在對 csv/tsv/xlsx/xls/ods/txt/md/json 多加了一句提示:「請用 csv_read／xlsx_read／file_read 讀取」。其他所有副檔名(圖片、音訊、影片)維持跟以前一模一樣。這只是個提醒,不是強制層,真正擋下錯誤工具的是上面那道守門。

### 為什麼 `file_read` 拒讀試算表

`file_read` 回傳的是一整塊文字;而綁定 `duduclaw_files` 的 `db_field` 規則,對應的是**結構化**回傳裡的 `$.rows[*].<column>`。要是讓試算表的原始位元組走 `file_read`,`客戶清單.xlsx.地址` 這種欄位規則根本找不到 `.rows[*]` 可以比對,會原樣過關,只剩樣態規則在保護。所以 `file_read` 一開始就先檢查副檔名,csv/tsv/xlsx/xlsm/xls/ods 一律直接拒絕,把呼叫端導去用 `csv_read`／`xlsx_read`,而不是悄悄回傳一份沒有保護的內容。

---

## 自訂規則:你自己的識別碼,不必寫正規表示式

資料來源告訴去識別化一個欄位*在哪裡*,自訂規則告訴它一個值*長什麼樣子*:公司內部才有、任何內建規則集都不可能知道的識別碼,像是員工編號、內部專案代號、客戶代碼、合約字首。

2026-09 之前,五個內建規則集(`general`／`taiwan_strict`／`taiwan_minimal`／`financial`／`developer`)是一份固定清單,只能勾選,不能擴充。自訂規則集檔案技術上在開機時就會被解析,但產品裡沒有任何介面能建立一份——你只能手寫 TOML,不然就是沒有自訂規則。

### 一條規則 = 一個資料類型名稱 + 一種辨識方式

每條規則帶一個你自己輸入的**資料類型名稱**(`員工編號`、`Employee ID`,最長 32 個字元)和恰好一種比對方式:

- **關鍵字清單**:零語法路線。一行貼一個詞,每個至少兩個字元。ASCII 詞邊界做整詞比對,CJK 做子字串比對,一律不分大小寫(跟 `keyword` 規則種類一直以來的語意相同)。
- **樣式**:正規表示式,認樣態而不是認清單。

每條規則還有一個**啟用**開關,規則可以先擱著不刪。`enabled` 是規則規格新增的欄位,對*每一種*規則種類都生效——`regex`、`keyword`、`identity`、`json_path` 或 `db_field` 規則只要 `enabled = false`,就在引擎編譯階段被跳過,不是在比對階段才過濾掉。沒填就是 `true`,所以你現有的規則不會有任何變化。

### 放在哪裡

自訂規則是一份**規則集檔案**,不是內嵌的 `[redaction.rules.*]` 項目——操作者的心智模型是「再多一套規則集」,規則集檔案本來就會出現在你勾選的規則集清單裡:

```toml
# ~/.duduclaw/redaction/profiles/custom.toml
[meta]
name = "我的規則"
description = "在儀表板建立的自訂規則"
version = "1"

[meta.labels]                        # category id → what a human sees
CUSTOM_EMPLOYEE_ID = "員工編號"
CUSTOM_01 = "內部專案代號"

[rules.employee_id]
type = "regex"
pattern = 'EMP-\d{4}-\d{4}'
category = "CUSTOM_EMPLOYEE_ID"
priority = 60
enabled = true
```

有兩件事值得講清楚。

**`[meta.labels]` 是新的。** 一個 token 類別依構造本來就長得像機器碼(`[A-Z0-9_]{1,32}`),所以一份自創類別的規則集需要一個地方寫清楚這些類別代表什麼。全 ASCII 的資料類型名稱會變成 `CUSTOM_<SLUG>`;沒有任何 ASCII 字母的名稱,像「內部專案代號」,會拿下一個沒用過的兩位數編號變成 `CUSTOM_NN`,可讀的名稱就記在這裡。`redaction.get` 會把目前所有已列出規則集的對照表合併成 `category_labels` 回傳;dashboard 解析一個類別時,先查這份對照表,查不到再查自己內建的翻譯,最後才退回原始 id。

**優先順序 60** 把自訂規則排在預設區間(50)之上——你自己的員工編號規則會贏過通用的連續數字規則——也排在精確的內建規則(100)之下,所以身分證字號這類樣態遇到重疊時還是內建規則贏。

寫入會經過 advisory 檔案鎖與「暫存檔 + 原子 rename」,規則集名稱如果還沒列在 `config.toml [redaction] profiles` 裡就會自動補上,而且立即重建現行管線——跟 `redaction.update` 用的是同一套熱重載,不用重啟 gateway。

### 用範例產生樣式

不會寫正規表示式的話,貼 2 到 5 個真實值(`EMP-2024-0133`、`EMP-2025-0007`),外加最多 3 個反例——長得像但*不該*被遮的東西——`redaction.suggest_pattern` 會幫你把樣式寫出來。

系統依序嘗試三種引擎,回應會告訴你是哪一種答的:

1. **本機推論**,如果本機後端真的連得上。
2. **雲端輔助模型**,走帳號輪替器。
3. **啟發式**,完全不用模型:把每個範例拆成數字/大寫字母/小寫字母的連續片段與字面分隔符,要求所有範例共用同一種形狀,產出 `\d{4}`、`[A-Z]{2,4}`、跳脫過的分隔符。嚴格比對對不齊範例的話,會再放寬一輪,把每段英數字元連續片段一律當成 `[A-Za-z0-9]` 重試。

不管是哪個引擎產生的,樣式**在回傳前都必須通過驗證**:每個範例都要能完整命中(錨定比對),沒有一個反例可以在任何位置命中。反例檢查刻意不做錨定——因為實際引擎是用搜尋而不是錨定比對,所以一個只在反例*內部*觸發的樣式一樣會把它遮起來。模型第一次答案沒通過驗證,會拿到剛好一次重試機會,並把沒過的檢查項當回饋帶進去;再失敗就換下一個引擎。要是沒有一個引擎能產出通過驗證的樣式,回應就是 `pattern: null`、`all_ok: false`——誠實回報查無,絕不亂編一個。

你貼進去的範例值只會進到 prompt 裡,不會去別的地方:絕不寫進 log 或稽核紀錄。在 prompt 內部,這些值會用 XML 分隔符包起來,並明確標示成資料而不是指令。這支 RPC 每位操作者每分鐘限 10 次呼叫。

### 匯入規則包

`redaction.profiles.import` 接收一份 TOML 規則包(貼上或上傳),落地成第二份自訂規則集,存在 `~/.duduclaw/redaction/profiles/<slug>.toml`——這是給要把同一套規則佈署給很多個客戶的整合商用的路徑。

磁碟上的 slug 是 `[a-z0-9_-]{1,40}`,依三個步驟決定:明確指定的 `name` 參數;沒有指定就用 `[meta] name` 轉出的 ASCII slug(前提是這個 slug 還沒被用掉);兩者都不行,就用 `pack_<正規化後的 [meta] name 做 SHA-256、取前 8 碼十六進位>`。台灣的規則包通常會走到最後一步——像「製造業客戶包」完全沒有 ASCII 字元可以轉 slug——而且這個結果是決定性的,所以重新匯入同一份規則包會覆蓋上次建立的規則集,而不是疊出第二份。這整套推導完全不影響人類看得懂的 `[meta] name`:它照樣是 dashboard 各處顯示的規則集標籤,只有檔名被改造成機器可讀的形式。**明確指定**的 `name` 如果不合法或跟內建規則集撞名,算錯誤(是你自己打的,你可以改);自動推導出來的 slug 則絕不會卡死,因為匯入對話框根本沒有名稱欄位可以拒絕。

規則逐條驗證,壞掉的規則會**連同原因與它的 `[rules.<id>]` 表頭所在行號一起被跳過**,不會讓整份匯入失敗:樣式編譯不過、關鍵字不到兩個字元、不合法的規則 id、不合法的類別,都算。卡片自己不擁有的規則種類(`json_path`、`db_field`、`identity`)會實際拿現行引擎選項去編譯它們來驗證,所以一條會毒化下一次重載的規則在這裡就會被攔下來,不會先落到磁碟上。一份**零條**可用規則的規則包算錯誤,不是空規則集——在 dashboard 列出一套什麼都不遮的規則集,會比直接拒絕更糟。

帶 `dry_run: true` 可以拿到完整報告——名稱、匯入筆數、逐條規則的跳過原因、涵蓋的類別——什麼都不會寫入。

`redaction.profiles.remove` 會刪除一份自訂規則集檔案,把它從 `[redaction] profiles` 移除並重載。內建規則集一律拒絕:它們是編譯進 binary 裡的,沒有檔案可刪,悄悄把它從清單拿掉,看起來會像刪除成功了,但其實不是。

### Fail-closed 讀取

如果 `custom.toml` 存在但讀不出來或解析不了,`redaction.custom_rules.list` 會回傳**錯誤**,絕不會退化成一份空清單。操作者看到「你沒有任何規則」,會以為自己的規則被刪了,但事實上規則還好好在磁碟上,是整條管線被同一個解析失敗給毒化了。

### 存檔前先試跑規則

精靈的最後一步,會拿你剛建好的規則跑一次樣本——**在**任何東西被寫入之前。`redaction.dry_run` 為此多了兩個選填參數:

- **`sample_text`**:用一段白話文字取代 `sample_json`。它在內部會包成一個 JSON 字串值(就是 `JSON.stringify(text)`),所以樣本走的還是跟真實工具結果一樣的管線路徑。兩者都給的話,`sample_json` 優先。
- **`draft_rules`**:一組 `{ id, label?, category, kind, keywords?, pattern? }` 陣列,驗證邏輯跟儲存(upsert)用的是同一套程式碼(所以預覽絕不可能放行一個存檔會拒絕的東西)。它們會被編譯成一份候選規則集,**只為這一次呼叫**存在,疊在現行設定的內嵌規則之上——草稿如果跟既有規則共用同一個 id,就會蓋過它,這正是「預覽一次編輯」的意思。什麼都不會寫進任何地方:候選管線在這次呼叫回傳後就丟棄,已儲存的規則集不受影響。

草稿產生的命中,`rule_id` 會帶草稿自己的 `id`,方便 UI 統計「這條規則 · N 處」。其他一切——內建規則集、欄位規則——回報方式跟以前完全一樣。一份無效的草稿會讓整次呼叫報錯,絕不會只跑一部分:一次預覽如果悄悄丟掉一條規則,回報的涵蓋範圍就會跟存檔後的規則集實際能做到的不一樣。

### RPC

| 方法 | 參數 | 回傳 |
|---|---|---|
| `redaction.custom_rules.list` | — | `{ rules: [{ id, category, label, kind, keywords, pattern, example, enabled }] }` |
| `redaction.custom_rules.upsert` | `{ id?, label, category?, kind, keywords?, pattern?, enabled? }` | 儲存後的那一列,加上熱重載回傳的 `applied`／`warning` |
| `redaction.custom_rules.remove` | `{ id }` | `{ ok, removed, applied, warning }` |
| `redaction.custom_rules.set_enabled` | `{ id, enabled }` | 更新後的那一列 |
| `redaction.suggest_pattern` | `{ examples: [2..5], counter_examples?: [0..3] }` | `{ pattern, engine, checks: [{ value, kind, matched, ok }], all_ok }` |
| `redaction.profiles.import` | `{ toml, name?, dry_run? }` | `{ name, imported, skipped: [{ rule_id, line?, reason }], categories, dry_run }` |
| `redaction.profiles.remove` | `{ name }` | `{ ok, removed, applied, warning }` |
| `redaction.dry_run`(擴充)| `{ sample_json? \| sample_text?, tool?, args?, draft_rules? }` | `{ hits: [{ pointer, rule_id, category, token }], token_count, restored_ok }` |

這七個 RPC 全都跟 `redaction.*` 其他 RPC 一樣,擋在同一道 admin 權限門後:每一個都在改動決定「什麼東西會離開這套部署」的規則集。

清單裡每一列的 **example** 欄位值得說明一下。關鍵字規則,它就是第一個關鍵字——一個你自己輸入、看了就認得的真實值。樣式規則,它是**從樣式合成出來的**,不是從表單記下來的:`EMP-\d{4}-\d{4}` 會呈現成 `EMP-0000-0000`(字面文字原樣保留,字元類別取一個代表字元,重複次數取最小值、沒有上限就取 4,交替分支取第一個分支,錨點不呈現任何東西)。你貼進精靈裡的值刻意不存在任何地方,所以能顯示的只有重建出來的結果。合成失敗的情況——樣式展開會超過 128 個字元,或者根本解析不了——這一列就直接顯示樣式本身,誠實勝過假裝正確。

---

## AI 智慧偵測(OpenAI Privacy Filter)

上面每一種規則認的都是「樣態」:身分證字號、信用卡卡號、API 金鑰、你指名的欄位。姓名、地址、生日、帳號這些東西沒有樣態可言。在這之前,唯一抓得到它們的方法,就是知道它們躺在哪個資料庫欄位裡——這招在你自己的 ERP 上有效,碰到貼上來的信件串、多出一個意外欄位的試算表、或是一則聊天訊息,就完全沒轍。

`ai_pii` 規則集(「AI 智慧偵測」)補上這一塊。它跑的是 [OpenAI 的 Privacy Filter](https://huggingface.co/openai/privacy-filter)——一個 1.5B 參數(50M 啟用)的雙向 token 分類器,Apache-2.0 授權——**在本機**跑,透過 ONNX Runtime。什麼都不會送出去:沒有 API、沒有遙測、推論當下完全沒有網路連線。

### 偵測什麼

八個標籤,對應到去識別化的類別,所以模型命中跟同種類的正規表示式命中會 token 化成一模一樣的東西(模型找到的 `EMAIL` 跟 `general` 規則集正規表示式找到的 `EMAIL` 是同一個類別,所以一條指名 `EMAIL` 的來源篩選規則,兩邊都涵蓋):

| 模型標籤 | 類別 | |
|---|---|---|
| `private_person` | `PERSON` | 姓名 |
| `private_address` | `ADDRESS` | 街道地址 |
| `private_email` | `EMAIL` | |
| `private_phone` | `PHONE` | |
| `private_url` | `URL` | |
| `private_date` | `DATE` | 生日與其他個人日期 |
| `account_number` | `ACCOUNT_NUMBER` | 銀行/客戶帳號 |
| `secret` | `SECRET` | 密碼、金鑰、token |

### 它不是什麼

**它不是去識別化保證,一定會漏掉東西。** model card 自己都說這是資料最小化的元件,不是合規控管,我們自己實測的結果也一樣。在我們 25 句 zh-TW 句子、119 個 span 的測試裡:

| | 召回率 |
|---|---|
| 整體 | **79.8%**(不要求標籤正確的話是 89.1%) |
| 電話、email | 100% |
| 地址 | 88% |
| URL | 83% |
| **人名** | **72%**(不看標籤是否正確則 76%) |
| 日期 | 58% |
| 帳號 | 60%(不看標籤是否正確則 100%——台灣的銀行帳號常被標成 `private_phone`,但兩種標籤都會被遮,資料還是遮住了) |
| 長篇多語言文件(3K 字) | 88.0% / 95.1%,零誤判 |

誤判率是 5.5%(110 個裡 6 個),全部都是 span 往回多吃了一個中文欄位標籤,像是「出生日期」——是多遮了,不是漏了。另外還有兩個我們測試中觀察到的已知行為:一個人名緊接著一個日期,有時會合併成一個 span(日期還是有被涵蓋,只是標籤變成 `private_person`);email 的 span 有時會把前面那個空白也吃進去。

其他人在更難的語料(網路爬蟲、病歷、法律文件)上做的獨立基準測試,召回率落在 10% 到 38% 之間。**差距幾乎全部出在召回率上**,這正是這個規則集優先順序訂在 30——排在所有樣式規則之下——的原因,也是為什麼你應該讓 `general`／`taiwan_strict` 跟它一起開著。它是第二層防護,不是取代品。

### 安裝模型

release 版的執行檔本身既沒帶模型,也沒帶 ONNX Runtime。第一次勾選「AI 智慧偵測」時,卡片會跳出下載模型的提示:

| 檔案 | 大小 | 來源 |
|---|---|---|
| 模型(6 個檔案) | 約 945 MB,主要是 `onnx/model_q4.onnx_data` | Hugging Face `openai/privacy-filter`,釘死在一個 commit |
| ONNX Runtime | 依平台 7–75 MB | Microsoft 官方 GitHub release,版本對應 `ort` 目標版本(1.24.2) |

每個檔案的 URL、位元組長度、SHA-256 都寫死在執行檔裡。下載會先串流進 `<name>.part`,支援用 HTTP `Range` 標頭續傳,雜湊驗證過了才會 rename 到正式路徑——所以只要檔案出現在最終路徑上,它就一定是驗證過的檔案。雜湊對不上就刪掉下載到一半的檔案,整個安裝算失敗,不存在「裝了大半個模型」這種狀態。`installed.json` 這個標記檔最後才寫,裡面記著版本;所以裝到一半算**未安裝**,不是「壞掉的安裝」。

```
~/.duduclaw/models/privacy-filter/     config.json, tokenizer.json, tokenizer_config.json,
                                       viterbi_calibration.json, onnx/model_q4.onnx(+_data),
                                       installed.json
~/.duduclaw/lib/onnxruntime/1.24.2/    libonnxruntime.dylib | libonnxruntime.so | onnxruntime.dll
```

**平台支援。** macOS(Apple Silicon)、Linux(x86-64 與 arm64)、Windows(x64 與 arm64)。**Intel 版 macOS 不支援**:Microsoft 在 ONNX Runtime 1.24 沒有發布 `osx-x86_64` 版本,沒有東西可以老實裝上去。卡片會如實說明,規則會 fail closed,不會裝樣子。

### 效能與記憶體

在一台 10 核心 Apple Silicon 機器上實測,ONNX Runtime CPU、4 條 intra-op 執行緒、`q4` 圖:

- 一般 zh-TW 句子 **63–161 毫秒**;長文件約每 1,000 字 214 毫秒。
- 載入期間常駐記憶體約 **1.1–1.7 GB**。`idle_unload_minutes`(預設 10 分鐘)沒有推論就會把 session 卸載,下次用到再重新載入,閒置時的部署可以把記憶體要回來。
- 如果機器記憶體不夠,權重被換出到硬碟,單一句子可能要花**幾秒**而不是幾毫秒。要嘛給它足夠的記憶體空間,要嘛就把這個規則集關掉。

結果依文字內容快取(256 筆,LRU),這件事比聽起來重要:一條 `ner` 規則會編譯成**每個標籤各一條**規則,八條共用同一份快取,所以一輪對話只花一次推論,不是八次。

### 設定

在「偵測規則集」裡勾選「AI 智慧偵測」,或者自己手動加規則集:

```toml
[redaction]
profiles = ["taiwan_strict", "ai_pii"]     # keep the pattern rules on

[redaction.ner]                            # every key optional
threads = 4                 # ONNX Runtime intra-op threads
idle_unload_minutes = 10    # 0 disables unloading
min_chars = 24              # shorter text never reaches the model
max_chars = 32000           # longer text is chunked on paragraph boundaries
cache_entries = 256
# model_dir = "/some/other/place"          # default: <home>/models/privacy-filter
```

你也可以自己寫一條規則,把要偵測的標籤縮小範圍:

```toml
[redaction.rules.names_only]
type = "ner"
category = "PII"            # unused: each match is categorised by its label
labels = ["private_person", "private_address"]
priority = 30
```

`min_chars` 和 `max_chars` 都可以逐條規則覆寫。標籤打錯是載入錯誤,不是規則被跳過。

### Fail-closed 行為

跟整條管線的其他部分一致:**跑不動的規則,絕不能看起來像跑得動。**

- 勾了規則集但沒裝模型,`ner` 規則就會編譯失敗,連帶整個 `RedactionManager` 都建不起來——gateway 進入[毒化狀態](#儀表板),`duduclaw mcp-server` 會拒絕啟動,不會在規則集根本沒在跑的情況下,還把工具結果送出去。存檔前卡片會先警告你。
- 在不支援的平台上,或是編譯時沒開 `ner` 這個 feature 的版本上,結果一樣。規則種類本身在任何地方都還是能被*解析*的,所以一份規則集在不同版本之間仍然是可攜的;它只是在跑不動的地方拒絕編譯。
- 移除模型(`redaction.model.remove`)會立刻重建管線,所以狀態是馬上可見的,不會潛伏到下次重啟才發作。

### 稽核模型命中

`AuditEvent::Redact` 現在會記錄是哪個引擎找到每一個 span:

```json
{"ts":"2026-09-24T…","event":"redact","rule_id":"ai_pii","category":"PERSON",
 "token":"<REDACT:PERSON:…>","engine":"ner",
 "model_revision":"7ffa9a043d54d1be65afb281eddf0ffbe629385b"}
```

`engine` 對每一種決定性比對器都是 `"rule"`,模型命中則是 `"ner"`;`model_revision` 只有模型命中才會出現。這次發布之前寫下的稽核紀錄沒有 `engine` 欄位,讀回來一律當作 `"rule"`。

### RPC

| 方法 | 參數 | 回傳 |
|---|---|---|
| `redaction.model.status` | — | `{ installed, model_revision, ort_version, size_bytes, state, progress, error, avg_latency_ms, p50_latency_ms, calls, last_used_at, platform_supported, reason }` |
| `redaction.model.install` | — | `{ started, installed }`——冪等;下載中再呼叫一次會回傳 `started: false` |
| `redaction.model.cancel` | — | `{ cancelled }`——下載到一半的檔案會保留,下次安裝從中斷處續傳 |
| `redaction.model.remove` | — | `{ removed }`——刪掉模型,保留 ONNX Runtime 函式庫 |

`state` 是 `absent`／`downloading`／`ready`／`loaded`／`error` 其中之一。`avg_latency_ms` 和 `p50_latency_ms` 來自最近 100 次真實推論的滾動視窗,第一次推論之前是 `null`——絕不是用估算值充數。`redaction.get` 的 `available_profiles[]` 裡,任何帶 `ner` 規則的規則集都會標 `requires_model: true`,所以匯入規則包裡如果用到 `ner`,一樣會跳出跟 `ai_pii` 一樣的下載提示。

以上四個 RPC 全都跟 `redaction.*` 其他 RPC 一樣,擋在同一道 admin 權限門後。

---

## 儀表板

**設定 → 去識別化**,原本分開的「外部系統」與「資料來源」兩張卡已合併成一張「外部系統與資料來源」:

- **資料表欄位規則卡**:每條規則一張卡(id、類別、kind 標籤、一行涵蓋摘要)。*新增*在兩種模式之間切換:**資料表欄位(簡易)**,一個 `source` 下拉、多值欄位清單(支援 `*`,並提示會保留 `id`)、類別、誰能還原;以及**JSON 路徑(進階)**,`match_tool`、`match_args`、`paths`、`exclude_keys`,涵蓋簡易表單表達不了的情境。下拉一律只顯示顯示名稱(Odoo ERP、地端檔案,以及你自己定義的每個系統名稱),三個內建登錄 id(`odoo`／`duduclaw_db`／`duduclaw_files`)永遠不會以原始值出現;有真實資料庫連線的來源仍會把資料表欄位變成由 `db_sources.tables` 撐起的資料表/欄位選擇器。每次儲存都會先對整份解析後的規則集試編一次,路徑寫錯或來源未知會就地顯示,什麼都不會寫入。**試跑**按鈕貼一段 JSON 樣本(加上模擬的工具名稱與引數)跑過真實管線,顯示一張命中表:位置、規則 id、類別、token,**絕不顯示原值**。
- **外部系統與資料來源卡**:取代原本分開的「外部系統」與「資料來源」兩張卡,鼎新／Salesforce／HubSpot 這幾個固定樣板已移除,改由通用的「自訂 MCP 工具」類型涵蓋同樣情境。卡片依序列出:**Odoo ERP**(依 `odoo.status` 顯示已連線／未連線,未連線時只有一個跳轉到整合頁的連結,連線本身仍在那裡設定)、每個 `db_sources` 連線各一列、每個自訂 `data_sources` 登錄項各一列,最後固定一列「地端檔案(CSV／Excel)」免設定。三個內建登錄名 `odoo`／`duduclaw_db`／`duduclaw_files` 不會各自成一列,它們是上面四類列背後的底層對照,操作者看不到。每一列固定兩欄:**讀進來**(人話摘要,例如 Odoo 的固定資料表清單,或資料庫連線的允許資料表清單)與**寫回去**(還原政策三值下拉加稽核勾選;資料庫連線與地端檔案是唯讀,顯示「不適用」);每列還有一個可點的「N 條欄位規則」徽章,點下去跳到「資料表欄位規則」卡並套用篩選。資料庫連線那一列另外多一個「N 位 AI 員工可用」徽章(零位時顯示提醒色的「尚未授權任何 AI 員工」),點下去彈出一個小對話框,用跟精靈第 4 步一樣的勾選清單決定誰能用。如果有 AI 員工還在引用一個已經被刪除的資料來源,卡片上會列出一段簡短提示,寫明是哪一位、引用了哪些 id,修正要到那位 AI 員工自己的設定頁進行。

  寫回政策存進 `tool_egress` 的 key 由系統自動推導,操作者看不到 key 本身:Odoo 固定用 `odoo_*`;自訂工具來源看它的 `tools` 有沒有共同前綴(經 `duduclaw mcp-proxy` 命名的 `<server>.<tool>` 形狀就有),有就收成一個 `<server>.*`,沒有就每個工具各自用精確 key,但存檔時全部套同一條規則。歸不進上述任一列的舊 `tool_egress` key,收進卡片底部一個可折疊的「其他寫回規則(進階)」。

  **新增資料來源精靈,固定四步**:1. **類型**,資料庫／Odoo／自訂 MCP 工具三選一(Odoo 只顯示說明並連去整合頁;地端檔案只顯示說明,不可選,它是免設定的固定列)。2. **連線／工具**,依類型只問必要欄位(資料庫問顯示名稱、驅動、連線字串、允許的資料表;自訂工具問顯示名稱、工具清單、資料表怎麼認),`record_paths`／`table_result`／欄位別名／逾時收進「進階設定」折疊。3. **測試**,資料庫類型真的連線並列出資料表逐一勾選,自訂工具類型貼樣本 JSON 試跑;**這一步在自訂工具類型可以跳過**(按鈕文字變成「跳過」),因為新來源通常還沒有規則引用它。4. **寫回**,資料庫連線的第 4 步已經不是「不適用」單一畫面:改成問「哪些 AI 員工可以使用這個資料庫」的勾選清單,編輯既有來源時會照目前的授權預先勾好,已封存的 AI 員工排在清單最後並淡化顯示,旁邊註記資料庫連線本身唯讀、AI 無法寫回;自訂工具的第 4 步不變,問的還是還原政策與稽核勾選。同一份勾選清單也出現在 AI 員工設定頁的「工具與權限」分頁,兩邊擇一操作都會即時生效;清單裡如果有已勾選、但設定裡已經不存在的來源 id,會標「已不存在」,提醒操作者自己取消勾選,不會被悄悄拿掉。**編輯既有來源一律從第 2 步開始**,類型只在新增時能選。系統識別碼由顯示名稱自動產生(純中文名稱會得到 `source_xxxxxx`／`db_xxxxxx` 這類代號),只在「進階設定」唯讀顯示,介面上一律用顯示名稱。

  **已知限制**:自訂資料來源沒有持久化的「已驗證」狀態,精靈第 3 步的測試結果只存在對話框開著的當下;自訂工具來源在第 3 步試跑,沒有規則引用時一律顯示 0 命中(規則庫還沒有規則可套用,連線本身正常);所有資料庫連線一律對應同一個內建通用來源 `duduclaw_db`,「資料表欄位規則」表單裡的「選擇連線」子選單只用來決定資料表建議來自哪一條連線,規則本身不記錄對應哪條連線。
- **毒化橫幅**:當 gateway 的去識別化 manager 從一份「解析得出來但壞掉」的 `[redaction]` 設定建不起來(或者整個設定檔根本解析不了),卡片最上方會出現一則危險色調的橫幅:標題「去識別化保護未能啟動」,底下原文顯示錯誤原因(不翻譯,錯誤訊息本來是什麼語言就顯示什麼語言),最後一行「\<時間\> 起‧稽核與活動紀錄已留痕」。gateway 照常服務聊天流量(不經工具、沒有外洩面),但所有 MCP 工具在設定修好並存檔之前都不可用;存檔並熱重載成功會清掉這則橫幅,不需要重啟。

---

## 走一遍:從零開始遮 PostgreSQL 的 `customers.name`

1. **登錄連線。**可以在儀表板「外部系統與資料來源 → 新增資料來源 → 類型:資料庫」填(精靈第 3 步會先測試連線),也可以直接寫進 `config.toml`:

   ```toml
   [db_sources.crm_pg]
   label = "客戶 CRM 資料庫"
   driver = "postgres"
   url = "secret://env/CRM_PG_DSN"
   allowed_tables = ["customers", "orders"]
   max_rows = 200
   timeout_ms = 10000
   ```

   在 gateway 行程自己的環境變數裡設 `CRM_PG_DSN`,值是真正的 `postgres://user:pass@host/db` 連線字串,這樣它就永遠不會以明文形式出現在 `config.toml` 裡。

2. **授權 agent。**可以在「外部系統與資料來源 → 新增資料來源」精靈第 4 步的勾選清單裡完成(編輯既有來源會照目前授權預先勾好),也可以直接跟 agent 說「把這個資料庫開給你」;兩者最後都等同於把 id 寫進該 agent 的 `agent.toml`:

   ```toml
   [capabilities]
   db_sources = ["crm_pg"]
   ```

3. **寫欄位規則**,重用內建的 `duduclaw_db` 登錄來源(一般情況不需要另外登錄):

   ```toml
   [redaction.rules.crm_customer_names]
   type = "db_field"
   source = "duduclaw_db"
   fields = ["customers.name"]
   category = "DB_FIELD"
   restore_scope = { kind = "owner" }
   ```

   這在載入時會展開成一條綁定 `db_select` 的 `json_path` 規則,以 `args.table == "customers"` 為條件,遮蔽 `$.rows[*].name`。

4. **證明它生效。**可以在儀表板的試跑按鈕貼樣本(工具填 `db_select`,引數填 `{"table": "customers"}`),也可以從終端機:

   ```bash
   duduclaw redaction verify sample.json --tool db_select --arg table=customers
   ```

   報告會列出每個命中的 JSON pointer、規則 id、遮罩後仍看得出型態的原值(如「王**」)、token、類別,以及一次來回的還原驗證,跟去識別化管線其他地方用的證據格式一致。

5. **問 agent 一個會碰到 `customers` 的問題。**`name` 這個欄位現在送到模型手上時已經是 token;通道回覆給擁有者時,一樣會在受信的外送邊界把它還原成真值,跟其他被遮蔽的欄位沒有兩樣。

### 附件 CSV 三步驟

通道附件不需要 `db_sources` 授權,也不需要連線字串,檔案本來就已經在磁碟上,一開始就落在圍欄裡。

1. **寫規則**,重用內建的 `duduclaw_files` 來源,不用另外登錄:

   ```toml
   [redaction.rules.attachment_customers]
   type = "db_field"
   source = "duduclaw_files"
   fields = ["customers.csv.name", "customers.csv.email"]
   category = "DB_FIELD"
   ```

2. **送出檔案。**使用者在通道裡附上 `customers.csv`,agent 看到的附件那行文字帶著「請用 csv_read／xlsx_read／file_read 讀取」的提示,守門若是 `on`,對那個路徑的 `Read` 直接被拒絕。
3. **agent 呼叫 `csv_read`。**`name` 與 `email` 兩欄送到模型手上時已經是 token;通道回覆給檔案擁有者時,一樣在同一個受信的外送邊界還原成真值,跟其他被遮蔽的欄位沒有兩樣。

---

## 邊界

- **還有四個缺口,誠實列出。**HTTP/SSE 型 MCP server(沒有子行程可包,留警告日誌)。PTY session pool(`[runtime] pty_pool_enabled`,預設關,文件標為備援路徑),需要 session 自己持有改寫,而不是這個功能現在做的每次呼叫各自持有,目前還沒做。codex／gemini／antigravity 這幾個 runtime,自己的 MCP 註冊完全沒接上 proxy。以及本機推論的工具迴圈(`local_llm.rs`),本機模型的工具呼叫,proxy 與 `ToolInterceptor` 都碰不到。通道回覆(一般 spawn 與 one-shot PTY)與派工／cron／心跳／goal-loop 輪次都已涵蓋;以上四項還沒有。
- **登錄表描述的是形狀,不是語意。**`record_paths` 或 `table_arg` 打錯字不會讓某個欄位悄悄少一層保護,寫錯的項目要嘛直接載入失敗(fail-closed),要嘛什麼都命不中,而後者「試跑」會如實顯示零命中。
- **`db_query` 只在 operator 明確寫下 `allowed_tables = ["*"]` 的來源上才存在。**沒有針對自由 SQL 的半調子或盡力而為的白名單檢查,設計上是整支工具直接拒用,不會假裝有在過濾。
- **連線池是每次呼叫現開,不快取。**對一個 agent 一輪只會呼叫幾次的工具來說,這是對的取捨,而且代表憑證輪替或改設定會立刻生效,不用重啟 gateway。
- **資料檔守門是提醒,不是沙箱。**`Bash` 那道檢查是檔名啟發式判斷,一條動態組出路徑的指令(`python -c "open(chr(99)+...)"`)直接繞過去;在沒有 `bash` 在 `PATH` 上的 Windows 主機,這個 hook 本身就是一支 shell script,不會執行,守門等於不存在。兩個限制都不是後來才發現的,hook 自己的原始碼裡寫得清清楚楚。真正讓地端檔案的欄位值留在去識別化範圍內的,是 MCP 工具本身(`file_read`／`csv_read`／`xlsx_read`),不是前面那道守門。
- **db_sources 授權立即生效,但有一個例外。**MCP 派發節流點每次呼叫都重讀 `agent.toml`,一輪新對話馬上看得到工具,因為每一輪都是全新 spawn 的 CLI 行程;唯一的例外是預設關閉的 `[runtime] pty_pool_enabled` pooled REPL,那個 session 要等被回收重開才會看到新的授權清單。
- **`db_sources_remove` 刻意不對照設定檔驗證。**`db_sources`／`db_sources_add` 都要求 id 在 `config.toml [db_sources.<id>]` 裡存在,`db_sources_remove` 不用,這樣即使來源已經被刪掉,agent 手上那份過時的授權還是撤得掉。

---

## 一句話總結

`db_field` 以前的意思是「只認 Odoo」。現在的意思是「這個 agent 被允許透過任何工具看到的任何一張表」,不論是客戶自己的 MCP server(隔著 proxy 遮),還是 DuDuClaw 自己接進去的資料庫(從源頭就遮)。兩條路徑最後都落在同一份登錄表、同一張儀表板卡片、規則裡同一個 `source =` 欄位上,operator 只要寫一次遮蔽政策,不需要知道也不需要在乎資料實際上是走哪條路過來的。
