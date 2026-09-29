# 安全防線

> 四道現役守衛、各自跑在哪裡，以及它們都擋不住什麼。

---

## 歷史說明

2026-09 之前，本頁描述的是一套三階段 shell 腳本防禦：確定性黑名單、混淆／外洩掃描器、Haiku AI 判讀，全部放在 `.claude/hooks/`，由 GREEN／YELLOW／RED 威脅等級狀態機協調。

那批腳本已在 commit `ba015a48`（把 `.claude/` 移出公開 repo）時刪除，`.claude/` 現在整個被 gitignore。出貨的 binary 沒有任何一處會讀它們，威脅等級狀態機也不存在。

產品裡真正存在的東西更小、也更好推理：gateway 裝進每個 agent 目錄的**兩個 PreToolUse hook**（兩個都是 Rust 子命令）、訊息路徑上的**一個輸入掃描器**，以及對「誰能命令誰」那組檔案的**一道欄位級凍結**。

---

## 守衛 1 — `agent-file-guard`（PreToolUse，Rust）

`duduclaw hook agent-file-guard` 是真正的子命令而不是 shell 腳本，所以 macOS／Linux／Windows 行為一致。Gateway 以 matcher `Write|Edit|MultiEdit|Bash` 把它註冊進 `<agent_dir>/.claude/settings.json`，每次開機重新註冊（`agent_hook_installer`），並且是合併進操作者既有設定，不是整份覆寫。

以下情況它會 exit 2（Claude Code 讀成「擋掉這次工具呼叫」）：

- Agent 把 **agent 結構檔**（`agent.toml`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`…）寫到正規的 `<home>/agents/<name>/` 樹之外。開新 agent 只能走 `create_agent` MCP 工具，那條路帶著委派授權閘；
- Agent 寫**自己的 `SOUL.md`**，即使位置正確也擋。人格由操作者管理；唯一例外是該 agent 在 `agent.toml [permissions] can_modify_own_soul = true` 明確開啟，而且也只能改自己的；
- Agent 動**別的 agent** 的檔案，一律擋。

## 守衛 2 — `data-file-guard`（PreToolUse，Rust，RFC-23 §14.4）

守衛 1 保護 DuDuClaw 自己的結構檔，這一道保護的是客戶的資料。

`Read` 與 `Bash` 是 Claude Code 內建工具，所以 `cat customers.csv` 永遠不會經過 `file_read`／`csv_read`／`xlsx_read` 必經的 MCP 去識別化收斂點。安裝器把 `duduclaw hook data-file-guard` 註冊到 matcher `Read|Bash`；判斷邏輯放在 `duduclaw_core::data_file_guard`，由 CLI 子命令與 gateway 安裝器測試共用。契約與守衛 1 相同：exit 0 放行、exit 2 加 stderr 阻擋，stderr 會顯示給模型看。

除非 gateway 在 spawn 時設 `DUDUCLAW_DATA_FILE_GUARD`，否則它完全不作用；而 gateway 只在該 agent 的去識別化真的生效時才會設。去識別化關閉的部署，行為與這道守衛存在之前逐位相同。

H10（2026-09）之前，這是放在 `<agent_dir>/.claude/hooks/data-file-guard.sh` 的 POSIX shell 腳本，而且在 `PATH` 上沒有 bash 的 Windows 主機上**完全不作用**——hook 指令執行失敗，而 Claude Code 把非 2 的結束碼（包含「command not found」）當成*放行*，守門就在最沒人會發現的地方消失。安裝器現在會在升級時刪掉殘留的舊腳本，避免有人把它誤認成現役守衛。

**明講限制**：`Bash` 那道檢查比對的是檔名。動態組路徑的指令（`python -c "open(chr(99)+…)"`）照樣走得過去。真正的保護是 MCP 工具面，這道守衛只是降低模型走上未設防路徑的機率。它是啟發式，不是沙箱。

## 守衛 3 — `input_guard`（提示注入掃描器，Rust 函式庫）

`duduclaw_security::input_guard::scan_input` 以**七類規則**對文字評 0–100 分，達到或超過 `DEFAULT_BLOCK_THRESHOLD`（60）就擋：

| 規則 | 權重 | 單條即擋 |
|---|---|---|
| `instruction_override` | 40 | 是 |
| `role_hijack` | 35 | 是 |
| `tool_abuse` | 30 | 是 |
| `data_exfiltration` | 25 | 是 |
| `system_prompt_extraction` | 30 | 否 |
| `encoding_bypass` | 25 | 否 |
| `termination_manipulation` | 30 | 否 |

平台主要語言是繁體中文，所以樣式同時涵蓋英文與 zh-TW。文字先做 NFKC 正規化（`unicode_normalizer`），同形異義字與隱形字元的花招因此躲不過樣式比對。

`termination_manipulation`（LoopTrap，arXiv:2605.05846）刻意不設成單條即擋：權重 30 低於門檻，單次命中只警告並留稽核，不阻斷——這樣一般的「請繼續」不會被誤殺。

呼叫端：MCP 分派總門（`scan_input_with_audit`）、`duduclaw migrate-from` 匯入、expert pack 安裝、skill 審查——任何未信任文字要進入 agent context 的地方。

## 守衛 4 — `org_field_guard`（組織權威凍結）

A2A 委派判定（`delegation_policy::can_delegate`）靠 `agent.toml` 的 `[agent] reports_to`／`department`／`name` 與 `config.toml` 的 `[delegation]`、`[acp]` 決定誰能命令誰。這兩個都是普通檔案：一個手上有 `Edit` 的 agent 可以把自己的 `reports_to` 改指向受害者，再宣稱「下屬 → 上級」那條規則。被審判的一方握有證據。

`org_field_guard` 跑在同一個 `agent-file-guard` hook 裡，把重建出來的**寫入後內容**逐欄位與磁碟上的現況比對，受保護欄位或區段有變動就拒絕。`[capabilities]` 是**整張表**凍結而不是列一份鍵名清單——這樣未來版本新增的 capability 鍵，落地當天就受保護，而不是等誰想起來去補清單。

依建構方式 fail-closed：新內容無法解析、既有內容無法解析、寫入意圖無法重建，三者全部拒絕。檔案還不存在則放行，因為建立走的是 `create_agent`，那裡有自己的閘。

合法變更的既有路徑全部保留：MCP `agent_update` 工具與儀表板 `agents.update` RPC，兩者都不經過這個 hook。

---

## 支撐層

**MCP 授權閘** — 每個 MCP 工具都在 scope 表裡逐項列舉；沒被列的工具預設需要 Admin scope。Scope、per-agent capability 授權、`denied_tools` 三者各自在分派總門強制，每次拒絕都帶 `error_class` 落稽核。

**SOUL.md 漂移偵測** — `soul_guard` 在開機與每次 heartbeat tick 以 SHA-256 對每份 `SOUL.md` 取指紋，在 `.soul_history/` 保留最多 10 個版本備份，並連同 Agent Stability Index 一起回報漂移。

**稽核軌跡** — `tool_calls.jsonl` 記錄每次工具呼叫，`result_text`／`input_text` 都經遮罩（三輪秘密遮罩，先遮再截斷），權限 `0600`，行與行之間雜湊串接，16 MB 輪替。`security_audit.jsonl` 另外承載安全事件。這份 log 同時是 grounding precheck 與驗收判官讀的證據來源，弱化它等於弱化驗證。

**每 agent 金鑰隔離** — MCP API key、通道 token、連接器憑證都是 per-agent，經 `secret_ref` 解析，所以單一 agent 外洩不等於平台外洩。

---

## 這些守衛擋不住什麼

明講這一節本身就是防線的一部分。

- **Hook 看得到的是 Claude Code 自己的工具呼叫，不是 MCP 工具呼叫。** MCP 有自己的閘（scope、授權、`denied_tools`）；hook 是內建 `Write`／`Edit`／`Read`／`Bash` 那面的第二道鎖。
- **`data-file-guard` 是啟發式。** 它比對 `Bash` 指令列裡的檔名，動態組出來的路徑就繞得過去。（它已不再在 Windows 上失效——H10 已把它改成 Rust 子命令。）
- **沒有威脅等級狀態機。** `~/.duduclaw/threat_level` 還在，作為 computer use 協調器會輪詢的操作者 kill switch（`RED` 中止、`YELLOW` 暫停），但工作區內已沒有任何東西會寫它。檔案不存在或讀不到就視為 `GREEN`。
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
