# 安全防線

> 四道現役守衛、各自跑在哪裡，以及它們都擋不住什麼。

---

## 歷史說明

2026-09 之前，本頁描述的是一套三階段 shell 腳本防禦：確定性黑名單、混淆／外洩掃描器、Haiku AI 判讀，全部放在 `.claude/hooks/`，由 GREEN／YELLOW／RED 威脅等級狀態機協調。

那批腳本已在 commit `ba015a48`（把 `.claude/` 移出公開 repo）時刪除，`.claude/` 現在整個被 gitignore。出貨的 binary 沒有任何一處會讀它們，威脅等級狀態機也不存在。

產品裡真正存在的東西更小、也更好推理：gateway 裝進每個 agent 目錄的**兩個 PreToolUse hook**（兩個都是 Rust 子命令）、訊息路徑上的**一個輸入掃描器**，以及對「誰能命令誰」那組檔案的**一道欄位級凍結**。

---

## 守衛 1 — `agent-file-guard`（PreToolUse，Rust）

`duduclaw hook agent-file-guard` 是真正的子命令而不是 shell 腳本，所以 macOS／Linux／Windows 行為一致。Gateway 以 matcher `Write|Edit|MultiEdit|NotebookEdit|Bash` 把它註冊進 `<agent_dir>/.claude/settings.json`，每次開機重新註冊（`agent_hook_installer`），並且是合併進操作者既有設定，不是整份覆寫。

安裝的指令帶著 agent id（`--agent`），v1.68.1 之後的版本也帶著 DuDuClaw 資料目錄（`--home "<路徑>"`）。Gateway 啟動員工的 CLI 時會清空環境變數，白名單裡沒有 `DUDUCLAW_HOME`，所以先前 hook 一律退回 `$HOME/.duduclaw` 判斷；資料目錄不在預設位置的部署，真正資料目錄裡的每個路徑都被當成「資料目錄之外」。安裝器對每個 `<資料目錄>/agents/<id>`（或 `<資料目錄>/agents/.ephemeral/<id>`）形式的員工目錄都寫入 `--home`，路徑含 shell 特殊字元時改用單引號包起來。既有的安裝會在下一次啟動員工或 gateway 開機時就地改寫。

Hook 依序從這些地方取得資料目錄：`--home`（只認絕對路徑）；沒有員工身分的呼叫者（hook 指令沒帶 `--agent`、環境裡也沒有 `DUDUCLAW_AGENT_ID`）照舊用預設位置；hook 環境裡明確設定的絕對路徑 `DUDUCLAW_HOME`。不會從工作目錄推回資料目錄。全部都推不出來時，該員工的每一次 `Write`／`Edit`／`MultiEdit`／`NotebookEdit`／`Bash` 呼叫都拒絕，不會拿猜測的位置判斷。`NotebookEdit` 也在同一次加入 matcher，它的 `notebook_path` 比照 `Write` 的目標判斷。Hook 子命令只在 stderr 回覆，不再寫日誌檔。

以下情況它會 exit 2（Claude Code 讀成「擋掉這次工具呼叫」）：

- Agent 把 **agent 結構檔**（`agent.toml`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`…）寫到正規的 `<home>/agents/<name>/` 樹之外。開新 agent 只能走 `create_agent` MCP 工具，那條路帶著委派授權閘；
- Agent 寫**自己的 `SOUL.md`**，即使位置正確也擋。人格由操作者管理。這個 hook 沒有開放選項：在 `agent.toml [permissions] can_modify_own_soul = true` 明確開啟的 agent，只能透過 `agent_update_soul` MCP 工具改自己的 `SOUL.md`，不能直接寫檔；
- Agent 寫**自己的 `CONTRACT.toml`**，即使位置正確也擋（判定 `BlockedOwnContractWrite`）。契約是操作者給 agent 的界線，所以完全沒有開放旗標；擋下時的訊息會請 agent 去找操作者，由操作者在儀表板修改（`contract.update`，僅限管理者，不經過這個 hook）；
- Agent 動**別的 agent** 的檔案，一律擋；
- Agent 寫入 **DuDuClaw 資料目錄**裡的其他任何位置。對帶員工身分（或宣稱的身分驗證失敗）的呼叫者，資料目錄底下能寫的只有自己的 agent 目錄與共用的 `attachments/`。這是允許清單，所以涵蓋稽核紀錄（`tool_calls.jsonl`，佐證檢查、判官摘要與近期行動摘要都讀它）、`evals/`（含留出集與其他員工的評測集）、所有 SQLite 資料庫、斷路器狀態、授權與組織檔、全域 `skills/` 與共用 wiki，日後新增的資料也一開始就受保護。這些資料的正當寫入者是 gateway 與受閘控的 MCP 工具，都不經過這個 hook。只有沒有員工身分的呼叫者不受影響，也就是 hook 指令沒帶 `--agent`、環境裡也沒有 `DUDUCLAW_AGENT_ID`。安裝的 hook 指令一定帶 `--agent`，hook 也優先採用它，所以操作者在某位員工的目錄裡手動執行 `claude`，會以該員工的身分被判斷（要求身分權杖的模式下，則是身分驗證失敗的呼叫者）。操作者要改這些檔案，請用儀表板，或直接用編輯器改；
- 寫入目標是指向上述被擋位置的**符號連結**。Write／Edit 的路徑會判斷兩次：照字面一次、解析到實際落點一次（跟隨連結與連結之後的 `..`；懸空連結一律拒絕），任一次擋下就擋。相對路徑以 hook 輸入裡的工作目錄解析，輸入沒有工作目錄時以員工自己的目錄解析。無法確認實際落點的路徑一律拒絕，訊息為「無法確認這次寫入實際會落在哪裡」；
- Agent 寫入、搬移或刪除 **`agents/_trash/`** 底下的任何東西（被移除的員工存放在那裡；Bash 以啟發式判斷）；
- Agent 在 Bash 執行 **`duduclaw agent create <name>`**，而該名稱因為曾有員工被移除而處於保留狀態（見[委派隔離](37-delegation-isolation.md#被移除員工的名稱仍被保留)）；拒絕會以稽核事件 `agent_name_reserved` 記錄，`path_kind` 為 `cli_bash_agent_create`。

在 Bash 上，「自己的 `SOUL.md`」與「自己的 `CONTRACT.toml`」兩條規則是啟發式判斷：寫入形態的指令只要點名這個檔案就擋下，不論寫成 `agents/<自己>/…`，或是 `CONTRACT.toml`、`./CONTRACT.toml` 這類相對寫法。身分驗證失敗的呼叫者，寫入形態的指令只要點名 `agents/` 底下任何位置就擋，與 Write／Edit 一致。

**Bash 與資料目錄允許清單。** Bash 通道用同一份允許清單判斷指令文字。以下說的「受保護的資料目錄目標」，指資料目錄裡自己的員工目錄與 `attachments/` 以外的位置、別的員工的目錄，以及被移除員工的存放區；身分驗證失敗的呼叫者，連自己的目錄也算。

- 判斷前先照 bash 的讀法還原指令：續行、反斜線跳脫與引號都先還原。把輸出或錯誤輸出丟到 `/dev/null`（`2>/dev/null`、`>/dev/null`），以及把一個描述元複製到另一個（`2>&1`），不算寫入；把輸出與錯誤一起重導到檔案則算寫入。
- 列在已知唯讀指令清單上的指令（列出、讀取、搜尋、比對之類）不檢查參數，但它們的輸出重導照樣檢查。有選項能把輸出寫進檔案、或能執行其他指令的指令，不放進清單，或在帶了那些選項時不算唯讀。員工用清單外的指令讀取資料目錄底下的檔案會被拒絕，請改用清單內的指令或對應的 MCP 工具。
- 複製類指令（複製、安裝、下載、解壓縮封存檔）只看目的地，所以把資料目錄裡的檔案複製到自己的目錄放行。
- 會改動參數所指對象的指令（搬移、刪除、建立連結、改權限、改擁有者或時間戳、同步、資料庫命令列工具等），以及直譯器與 shell，只要任一參數（含行內程式碼）是受保護的資料目錄目標就擋。任何指令把輸出重導到受保護的資料目錄目標也擋。
- 其餘指令（含前綴選項無法解析的情況），只要任一參數是受保護的資料目錄目標就擋，不論指令有沒有寫入。清單外的指令要碰受保護的資料目錄路徑，唯一的出口是已知唯讀指令清單。
- 資料目錄裡、**員工目錄與 `attachments/` 以外**的資料庫檔（`*.db` 及其 `-wal`／`-shm`／journal 檔、`*.sqlite*`），不論出現在指令哪裡，即使只是讀取也擋。員工目錄與 `attachments/` 裡的資料庫不在這條規則範圍內；別的員工的目錄仍然禁止寫入。
- 相對路徑以 hook 輸入裡的工作目錄解析（沒有時以員工自己的目錄解析），並跟著指令裡的 `cd`／`pushd` 移動。工作目錄推算不出來、而指令任何地方提到受保護的資料目錄位置時，受檢位置上的相對路徑一律擋下；完全沒提到時則不判斷。
- 受檢路徑上已存在的符號連結會解析，實際落點也一併判斷；無法解析的受檢路徑（包括懸空連結）一律拒絕。

這是減速帶，不是隔離。真正的隔離是不給 agent Bash；限制列在「這些守衛沒有涵蓋什麼」。

Live fork（`fork_run`）從另一側守住同一批檔案：分支可以讀 agent 的結構檔，但把分支採用回 agent 目錄時，絕不會用分支的版本覆蓋上層的 `SOUL.md`、`CONTRACT.toml`、`agent.toml`、`.mcp.json`、`.claude/` 或其他 agent 結構檔。

## 守衛 2 — `data-file-guard`（PreToolUse，Rust，RFC-23 §14.4）

守衛 1 保護 DuDuClaw 自己的結構檔，這一道保護的是客戶的資料。

`Read` 與 `Bash` 是 Claude Code 內建工具，所以 `cat customers.csv` 永遠不會經過 `file_read`／`csv_read`／`xlsx_read` 必經的 MCP 去識別化收斂點。安裝器把 `duduclaw hook data-file-guard` 註冊到 matcher `Read|Bash`；判斷邏輯放在 `duduclaw_core::data_file_guard`，由 CLI 子命令與 gateway 安裝器測試共用。契約與守衛 1 相同：exit 0 放行、exit 2 加 stderr 阻擋，stderr 會顯示給模型看。

除非 gateway 在 spawn 時設 `DUDUCLAW_DATA_FILE_GUARD`，否則它完全不作用；而 gateway 只在該 agent 的去識別化真的生效時才會設。去識別化關閉的部署，行為與這道守衛存在之前逐位相同。

H10（2026-09）之前，這是放在 `<agent_dir>/.claude/hooks/data-file-guard.sh` 的 POSIX shell 腳本，而且在 `PATH` 上沒有 bash 的 Windows 主機上**完全不作用**——hook 指令執行失敗，而 Claude Code 把非 2 的結束碼（包含「command not found」）當成*放行*，守門就在最沒人會發現的地方消失。安裝器現在會在升級時刪掉殘留的舊腳本，避免有人把它誤認成現役守衛。

**明講限制**：`Bash` 那道檢查比對的是檔名。動態組路徑的指令（`python -c "open(chr(99)+…)"`）照樣走得過去。真正的保護是 MCP 工具面，這道守衛只是降低模型走上未設防路徑的機率。它是啟發式，不是沙箱。

## 守衛 3 — `input_guard`（提示注入掃描器，Rust 函式庫）

`duduclaw_security::input_guard::scan_input` 以**十一類規則**對文字評 0–100 分，達到或超過 `DEFAULT_BLOCK_THRESHOLD`（60）就擋：

| 規則 | 權重 | 單條即擋 |
|---|---|---|
| `instruction_override` | 40 | 是 |
| `role_hijack` | 35 | 是 |
| `tool_abuse` | 30 | 是 |
| `data_exfiltration` | 25 | 是 |
| `system_prompt_extraction` | 30 | 否 |
| `encoding_bypass` | 25 | 否 |
| `termination_manipulation` | 30 | 否 |
| `authority_escalation` | 每種訊號 35，兩種不同訊號相加 | 否 |
| `memory_poisoning` | 每種訊號 30，兩種不同訊號相加 | 否 |
| `role_provenance` | 每種框架 35，兩種不同框架相加 | 否 |
| `action_binding` | 30 | 否 |

樣式涵蓋英文與中文（繁體與簡體）。文字先做 NFKC 正規化（`unicode_normalizer`），同形異義字與隱形字元的花招因此躲不過樣式比對。

**中文涵蓋範圍（v1.67.1）。** 已發佈的版本只用四個完全相符的字串比對中文的指令覆寫，句子裡多插一個「所有」「之前」「的」就比對不到；實測時有四句這樣的句子經 `user_profile_record` 存進記憶。v1.67.1 起：

- `instruction_override`：覆寫動詞（忽略／無視／忘記／忘掉／不要理會／不用理會／別管，含簡體）之後，同一個子句內、12 個字以內出現指示類名詞（指示／指令／規則／提示詞／系統提示），而且兩者之間有範圍詞（先前／之前／以上／上面／上述／前面／所有／全部／一切／你的／原本／原來）。空白不計；`。！？；` 與換行會結束子句。權重與立即封鎖都和英文片語相同。
- `system_prompt_extraction`：擷取名詞（系統提示詞／系統提示語／你的系統提示／你的指示／你的設定）與輸出動詞（輸出／顯示／告訴我／給我看／洩漏／列出／重複）彼此相距 12 個字以內，順序不限。計分和英文規則相同：權重 30，單獨出現不會封鎖。單獨的「系統提示」不算擷取名詞，因為它也是「系統通知」的意思。
- `role_hijack`：固定片語，例如「你現在是管理員模式」「開發者模式」「越獄模式」「你現在不受限制」「進入越獄模式」，以及單獨的「越獄模式」。和英文相同。
- 門檻與英文清單不變。

**已知誤判。** 這條規則看句型比對，所以一般句子只要在同一個短子句裡有覆寫動詞、範圍詞和指示類名詞，也會被擋，例如「請忽略之前寄的指示，以新版為準」「請忽略以上規則中的第三條，已經取消」「忘記之前的規則了，可以再說一次嗎」（測試 `known_benign_shapes_that_do_match` 固定了這個行為）。提到「越獄模式」也會被擋。解法是不用覆寫動詞換個說法，例如「之前的指示作廢，以新版為準」；相近的「之前的報價單請作廢，以新的指示為準」在「不可比對」的測試清單裡。這個掃描器是片語規則，不是分類模型，也沒有用真實對話資料量測過。

`termination_manipulation`（LoopTrap，arXiv:2605.05846）刻意不設成單條即擋：權重 30 低於門檻，單次命中只警告並留稽核，不阻斷——這樣一般的「請繼續」不會被誤殺。

**句型家族（v1.68.0）。** 紅隊帳本量出四種針對 AI 員工的技法原本是零分。現在每一種都有一組英文與一組繁體中文的錨定正則：

- `authority_escalation`：要員工拿自己的權限、不用使用者的權限去做事（"use your own service account / admin role"、"not my permissions"、"the system trusts you more than me"；用你自己的服務帳號／管理員身分、不要用我的權限、系統比較信任你）。
- `memory_poisoning`：要員工把一條長期特權存起來（"remember this permanently"、"as a fact about me"、"I am always allowed to"；永久記住、記在你的記憶裡、以後每次對話都照辦、我一律有權限）。一般的「請記住…」、"remember to…"、"remember my name" 不會比對到。
- `role_provenance`：使用者訊息裡出現假的系統、工具或營運方框架：`[SYSTEM]`、`<tool_result>` 標籤、`approval=granted`、"policy update from the platform operator"、「本次對話已解除…限制」、平台營運方政策更新、引文裡寫給 AI 的附註（給 AI 助理的附註）以及要它隱瞞的指示（不要提到這段附註）。框架只比對完整標籤，"our system sends a tool result" 不會中；一組開頭加結尾標籤算一種框架。
- `action_binding`：把一個核准挪去做別的事（"use that same approval to…"、"use the approval from step 1"；用同一個核准、用第一步的核准去…）。「既然已經核准預算，請安排會議」不會比對到。

權重原則：單一訊號只警告並留稽核（30–35，低於 60）。同一則訊息裡出現同一家族的兩種**不同**訊號，或一種訊號再加上既有規則（例如 `instruction_override`、`system_prompt_extraction`），就會擋。可以自身相加的有三個家族：`authority_escalation`（「用你的服務帳號」加「系統比較信任你」＝70）、`memory_poisoning`（「永久記住」加「我一律有權限」＝60）、`role_provenance`（`[SYSTEM]` 加 `approval=granted`＝70）。`action_binding` 不會自己相加，要搭配其他規則才擋。同一種訊號出現兩次只算一次：單獨一個 `[SYSTEM]` 或一組 `<tool_result>…</tool_result>` 維持 35。已知代價：剛好帶兩種訊號的正常句子也會被擋，例如 "please use your admin account, not my permissions, to fix the shared folder"（測試 `known_benign_shapes_blocked_by_stacking` 固定了這個行為）。換成只帶一種訊號的說法，或請管理員自己處理即可。任何命中就丟棄文字的呼叫端（萃取、使用者側寫寫入）現在也會丟棄含這些句型的文字。每個家族的正例與看起來像但不該中的句子，都在 `input_guard.rs` 的測試裡固定。

比對到時會在哪裡出現（已核對的呼叫端）：

- 聊天收到的訊息（`channel_reply`，`scan_input_with_audit`）：被封鎖的訊息會收到警告回覆，不會交給 AI。
- MCP 工具呼叫（`mcp_dispatch`，對序列化後的參數做 `scan_input_with_audit`）：參數引用了會被封鎖的句子時，呼叫被拒絕並記入稽核。
- 對話事實、使用者輪廓與知識分流的萃取（`wiki_ingest`、`profile_distill`、`knowledge_route`）：只要比對到**任何**規則就丟棄，包括不封鎖的擷取規則。
- `user_profile_record`：predicate 與值都會掃描，達到封鎖等級就拒絕。
- `duduclaw migrate from` 匯入會略過被封鎖的項目；expert pack 安裝會拒絕被封鎖的套件。
- Agent Mail：比對到的來信照樣存下但會加上標記，有標記的信永遠不會觸發 AI 員工。
- 提醒：提示內容被封鎖的提醒不會執行。

## 守衛 4 — `org_field_guard`（組織權威凍結）

A2A 委派判定（`delegation_policy::can_delegate`）靠 `agent.toml` 的 `[agent] reports_to`／`department`／`name` 與 `config.toml` 的 `[delegation]`、`[acp]` 決定誰能命令誰。這兩個都是普通檔案：一個手上有 `Edit` 的 agent 可以把自己的 `reports_to` 改指向受害者，再宣稱「下屬 → 上級」那條規則。被審判的一方握有證據。

`org_field_guard` 跑在同一個 `agent-file-guard` hook 裡，把重建出來的**寫入後內容**逐欄位與磁碟上的現況比對，受保護欄位或區段有變動就拒絕。`[capabilities]` 是**整張表**凍結而不是列一份鍵名清單——這樣未來版本新增的 capability 鍵，落地當天就受保護，而不是等誰想起來去補清單。

**員工自己的安全設定。** 對帶員工身分（或身分無法驗證）的呼叫者，它自己 `agent.toml` 的其餘部分以允許清單凍結：只有可編輯的區段能改，其他區段一律受保護，日後版本新增的區段也預設受保護。可編輯的區段是 `[agent]`、`[model]`、`[prompt]`、`[heartbeat]`、`[proactive]`、`[research]`、`[goal_intent]`、`[memory]`、`[skills]`、`[sticker]`、`[cultural_context]`、`[preset]` 與 `[planner]`。這些區段裡，`[agent] role`、`[prompt] cli_bare_mode`（它會讓 Claude CLI 略過 hook）與 `[model] account_pool` 仍凍結，組織欄位 `[agent] reports_to`／`department`／`name` 也一樣；`[capabilities]` 整張表凍結。這份允許清單適用於所有帶員工身分的呼叫者，包括在員工目錄裡手動執行 `claude` 的操作者（見 Guard 1）；較早的組織欄位與 `[capabilities]` 規則照舊對所有呼叫者生效。操作者要改這些區段，請用儀表板，或直接用編輯器改。

依建構方式 fail-closed：新內容無法解析、既有內容無法解析、寫入意圖無法重建，三者全部拒絕。既有的 `agent.toml`、`config.toml` 或 `.mcp.json` 讀不到時也拒絕（以前當成新檔放行）。檔案還不存在則放行，因為建立走的是 `create_agent`，那裡有自己的閘。

合法變更的既有路徑全部保留：MCP `agent_update` 工具與儀表板 `agents.update` RPC，兩者都不經過這個 hook。

---

## 支撐層

**MCP 授權閘** — 每個 MCP 工具都在 scope 表裡逐項列舉；沒被列的工具預設需要 Admin scope。Scope、per-agent capability 授權、`denied_tools` 三者各自在分派總門強制，每次拒絕都帶 `error_class` 落稽核。

**SOUL.md 漂移偵測** — `soul_guard` 在開機與每次 heartbeat tick 以 SHA-256 對每份 `SOUL.md` 取指紋，在 `.soul_history/` 保留最多 10 個版本備份，並連同 Agent Stability Index 一起回報漂移。

**稽核軌跡** — `tool_calls.jsonl` 記錄每次工具呼叫，`result_text`／`input_text` 都經遮罩（三輪秘密遮罩，先遮再截斷），權限 `0600`，行與行之間雜湊串接，16 MB 輪替。`security_audit.jsonl` 另外承載安全事件。這份 log 同時是 grounding precheck 與驗收判官讀的證據來源，弱化它等於弱化驗證。

**每 agent 金鑰隔離** — MCP API key 與連接器憑證都是 per-agent，經 `secret_ref` 解析，所以單一 agent 外洩不等於平台外洩。通道憑證分兩種：LINE、WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘使用 `config.toml [channels]` 裡整個部署共用的憑證；只有 Telegram、Discord、Slack 有員工專屬的 bot token（員工沒有自己的 token 時沿 `reports_to` 往上找，最後用全域 token）。

**通道上的聊天指令（v1.68.0）** — `!STOP`、`!STOP ALL`、`!RESUME`、`/model <名稱>` 需要管理員。WhatsApp、飛書、Teams、企業微信、Google Chat、釘釘以前對每位傳訊者都傳 `is_admin = true`，能傳訊給 bot 的人都能停止或恢復它。現在這些通道用該通道的 `admin_users` 設定（全域範圍；Google Chat 與 Teams 也能設定了）完全比對傳訊者 id 或對話 id，沒有清單就沒有人是管理員。WebChat 只有啟用中、角色為管理員的儀表板帳號算數，網站聊天元件的訪客一律不算。

**緊急停止門檻（v1.68.0）** — `KILLSWITCH.toml [triggers]` 的四個門檻以前沒有讀取端。現在只有寫在檔案裡且數值在範圍內的鍵才生效，安全設定頁每個門檻多了一個勾選框（取消勾選會送 `null`，把它移除）。檔案改動後會重讀。`cost_limit_usd` 比對所有員工 24 小時的花費，達到時把全域 failsafe 等級降為受限，直到 failsafe 自行恢復或有人送 `!RESUME`；`max_replies_per_minute` 依對話計算，超出的訊息靜默丟棄；`max_consecutive_errors` 與 `error_rate_threshold`（最近 20 次、至少 10 次）讓該對話的 failsafe 升一級。每次觸發記稽核 `killswitch_trigger`。`KILLSWITCH.toml` 的 `[audit]` 區段不再讀取。

**去識別化的資料來源保護（v1.68.0）** — 「隱私 / 去識別化」分頁的「資料來源保護」開關開始生效：`user_input` 在通道訊息送進 AI 前遮蔽，`system_prompt` 遮蔽組好的提示（預設只套用標了 `apply_to_system_prompt` 的規則），`cron_context` 遮蔽條件腳本的觸發訊息。出錯時停止這一輪，不送出未遮蔽的內容。`sub_agent`（在同一個分頁或 `config.toml [redaction.sources]` 設定）負責受委派員工的回覆被 gateway 寫進委派方對話紀錄的那條路（`send_to_agent`、`spawn_agent`、`spawn_ephemeral` 的回覆，包含轉寫給發起整條委派鏈那位員工的副本）。設為 `on` 時，回覆先用接收方員工的規則遮蔽再存進紀錄，等那位員工回覆使用者時再還原。預設 `inherit` 照原樣寫入，因為子代理本來就在同一套 `[redaction]` 規則下執行。遮蔽出錯時改存一段固定的提示文字，不寫入原文。不涵蓋：送到使用者通道的那份回覆（那是使用者自己看的）、員工用 `check_responses` 自己去取的回覆（屬於工具結果，依 `tool_results` 處理）、團隊角色之間的交接、Agent Mail。`purge_after_expire_days` 現在決定保管庫清理的天數。

**權限旗標（v1.68.0）** — `agent.toml [permissions]` 的 `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` 寫成 `false` 時，MCP 分派閘會拒絕對應工具（稽核 `permission_denied`）。升級後第一次開機會把舊範本的 `false` 改成 `true`，見[儀表板設定對照](../../guides/zh-TW/dashboard-settings.md#ai-員工編輯頁)。

---

## 這些守衛擋不住什麼

明講這一節本身就是防線的一部分。

- **Hook 看得到的是 Claude Code 自己的工具呼叫，不是 MCP 工具呼叫。** MCP 有自己的閘（scope、授權、`denied_tools`）；hook 是內建 `Write`／`Edit`／`Read`／`Bash` 那面的第二道鎖。
- **`agent-file-guard` 的 Bash 通道是啟發式。** 它讀的是指令文字，下列情況都擋不住：由變數（`$DUDUCLAW_HOME`，以及資料目錄在預設位置時的 `~/.duduclaw`、`$HOME/.duduclaw` 除外）、指令替換或其他計算產生的路徑；編碼後的指令；先寫成腳本再執行；把 here-document 餵給直譯器；別名與函式；透過環境變數讓之後啟動的 shell 載入某個檔案；清單內唯讀指令沒有被考慮到的寫檔選項；沒有寫明目的地的解壓縮或下載指令，它們寫進目前的工作目錄，而這條通道不判斷目前目錄，所以先切換進資料目錄再執行就不會被擋；在同一條指令裡先建立連結再透過它寫入；硬連結；以及檢查與實際執行之間的時間差。
- **Bash 通道也會誤擋一些無害的指令。** 受檢位置上的路徑解析失敗時一律拒絕，即使路徑在資料目錄以外；懸空的符號連結即使指向資料目錄以外也會被拒絕。
- **`agent-file-guard` 不涵蓋 `Read`。** 留出集與稽核紀錄員工仍然讀得到，hook 只擋寫入。
- **只有 Claude runtime 會跑這些 hook。** Codex、Gemini、Antigravity 與其他 runtime 靠各自的沙箱旗標。
- **員工自己目錄裡的狀態檔不在保護範圍**，只有 `SOUL.md`、`CONTRACT.toml`、身分檔（`.mcp.json`、`.claude/settings.json`）與 `agent.toml` 受保護。共用的 `attachments/` 每位員工都能寫。
- **`data-file-guard` 是啟發式。** 它比對 `Bash` 指令列裡的檔名，動態組出來的路徑就繞得過去。（它已不再在 Windows 上失效——H10 已把它改成 Rust 子命令。）
- **沒有威脅等級狀態機。** `~/.duduclaw/threat_level` 還在，作為 computer use 協調器會輪詢的操作者 kill switch（`RED` 中止、`YELLOW` 暫停），但工作區內已沒有任何東西會寫它。檔案不存在才視為 `GREEN`；檔案存在卻讀不到，或內容不是 `GREEN`／`YELLOW`／`RED` 其中之一，間隔 50 毫秒再讀兩次後仍是如此，就視為 `RED`（fail closed）；開頭的 UTF-8 BOM 與前後空白會略過。
- *（2026-09 移除。）* 本節原本註明 PTY session pool 不在去識別化改寫的涵蓋範圍內。該連線池已不存在——每次 Claude spawn 都是單次 spawn，正好就是改寫掛鉤的地方。

---

## 與其他系統的互動

- **CONTRACT.toml** 定義 agent 絕不能做什麼，`duduclaw test` 對它紅隊；守衛負責工具呼叫層級的強制。
- **演化引擎** — 因為 `SOUL.md` 對 agent 唯讀，會演化的產物是 playbook，見 [38-aee-playbook-evolution.md](38-aee-playbook-evolution.md)。
- **去識別化與資料來源** — `data-file-guard` 補的是哪條管線，見 [55-data-sources.md](55-data-sources.md)。
- **委派隔離** — `org_field_guard` 保護的是哪個判定，見 [37-delegation-isolation.md](37-delegation-isolation.md)。

---

## 總結

四道各自明講失效模式的守衛，勝過一個底下已經沒有程式碼的三層故事。防線被移除時，文件必須跟著移除：一頁描述著不存在的 shell 腳本的文件比沒有文件更糟，因為它會讓操作者停止繼續找。

## 頻道核准的保存與核對

高風險 Computer Use 使用原帳號及原對話或討論串送出確認，回覆 `確認 <完整 UUID>` 或 `取消 <完整 UUID>`；問題使用 `回答 <完整 UUID> <答案>`，不授權工具。單獨的是、A/B 不會選取請求。執行前重驗畫面、視窗、政策與取消狀態，重啟會讓舊畫面核准失效。沒有收據的執行成為 `uncertain`，需 Admin 核對。支援入口及限制見[操作指南](../../guides/durable-channel-decisions.md)。
