# Discovery 探索 — 在預算內比較工作區解法

Discovery 讓 AI 員工嘗試多個工作區解法，由操作者登錄的評分器打分，並保留通過驗證的成果。每次探索都有 agent 呼叫次數、美元、執行時間與輪數限制；Goals 頁面顯示探索樹和實際留下的證據。

**目前仍在開發與整合驗證。** 以下說明目前原始碼的介面契約，沒有宣稱已發布或已完成瀏覽器驗收。

## 使用核准的工作區

1. 開啟 **Goals**、選擇員工，找到 **探索** 區塊。伺服器 catalog 提供核准工作區 ID、評分器名稱及符合能力要求的 runtime。沒有 catalog 或建立權限時，需要操作者設定或授予存取權。
2. 填寫目標、模型、分支數、每分支改良次數、並行數與預算。每個分支先跑一次，再改良指定的次數，所以每輪節點數 = 分支數 × (改良次數 + 1)；表單會顯示算出的數字，改良次數可以是 0。請求使用 approved root ID，不能帶主機路徑、帳號池、沙箱覆寫或策略原始碼。
3. 建立任務。有權管理該員工的 manager 可以直接排入佇列；其他有權提出請求的身分會得到 `pending_approval`，由有權 manager 在收件匣核准後才執行。請求最多等待 24 小時，逾時自動取消，Goals 卡片會顯示這個期限。申請人在等待期間取消請求時，待核准單會一併撤回，從 manager 的收件匣消失。收件匣把這張核准單標成「啟動一次程式碼探索」，並在原始抽查檢視上方摘要 runtime、模型、評分器、分支數、改良次數、輪數、呼叫上限、美元預算與時間上限。權限由伺服器的已驗證身分決定。
4. 查看每輪的計畫節點、完成節點、父子關係、狀態、分數與實際模型。沒有回報模型時維持未知。`kind="discovery"` 任務由探索專用派工器執行。
5. 需要時取消執行中的探索。出現通過驗證的成品後，使用 **下載已驗證檔案**。伺服器先核對所有權與持久保存的 SHA-256 manifest，再列出檔案或回傳內容；檔案遺失或被改動會拒絕下載。

```mermaid
flowchart LR
  C[核准 catalog] --> T[建立探索任務]
  T --> A[需要時送核准]
  A --> R[有期限的探索與評分]
  R --> G[Goals 樹與成本證據]
  G --> D[下載已驗證成品]
```

探索狀態 `degraded` 表示這次探索提前結束，`stop_code` 說明原因，例如沒有可用的 AI 帳號時是 `no_account`（agent 呼叫 0 次、費用 0）。卡片上的琥珀色隔離提示只在 `isolation_degraded` 為 true 時出現，也就是這次探索真的沒有完整的 OS 隔離界線。比較成果前，請先看探索狀態、停止代碼及費用來源。探索、核准與節點狀態以在地化標籤顯示（英文、繁體中文、日文），經過時間最多顯示一位小數。

累計規劃格線上限為 20,000 格：`branch_count × (refine_count + 1) × max_rounds` 必須在上限內，動態策略規劃的各輪也適用。歷史超大 run 仍出現在清單，並標記 `tree_available=false` 與 `tree_unavailable_reason`；已記錄的完整費用小計會保留，完整樹查詢則明確拒絕。要求的並行數可以超過公開執行共用的四個 worker slot。Attempt 會在原 attempt 與 run 截止時間內等待空位，等待不消耗 agent call，取消會結束等待。

## 公開請求參考

以下 JSON 是協定範例，並非實跑逐字紀錄。ID 與名稱須從自己安裝環境的已驗證 catalog 取得。

```json
{"method":"discovery.catalog","params":{"agent_id":"researcher"}}
```

```json
{
  "method": "tasks.create",
  "params": {
    "assigned_to": "researcher",
    "kind": "discovery",
    "title": "Improve the parser fixture",
    "description": "Improve the parser while preserving the fixture results.",
    "discovery": {
      "approved_root_id": "<id returned by discovery.catalog>",
      "evaluator": "parser_score",
      "runtime": "claude",
      "model": "<model supported by the configured runtime>",
      "branch_count": 2,
      "refine_count": 1,
      "max_parallelism": 1,
      "budget": {"max_agent_calls": 6, "max_usd": 1.0, "max_wall_secs": 120, "max_rounds": 2}
    }
  }
}
```

建立結果包含 `task_id`、`run_id`、`status`（`queued` 或 `pending_approval`），以及可為空的 `approval_id`。可選的 `discovery.direction` 接受 `max`（預設）或 `min`。

| 方法 | 參數 | 用途 |
|---|---|---|
| `discovery.catalog` | `agent_id` | 核准 ID、評分器名稱、runtime、`can_create`、`requires_approval` |
| `discovery.list` | 可選 `agent_id`、`limit`（1–100） | 呼叫者有權查看的探索 |
| `discovery.tree` | `run_id` | 探索、節點及持久輪次紀錄 |
| `discovery.cancel` | `run_id` | 有權呼叫者取消探索 |
| `discovery.artifact` | `run_id` | 驗證後的檔案資訊：opaque `file_id`、名稱及大小 |
| `discovery.artifact` | `run_id`、`file_id` | 有大小限制的 `content_base64` 下載；每檔最多 16 MiB |

`discovery.list`（`runs[]`）與 `discovery.tree`（`run`）回傳的探索摘要帶有以下核准與停止欄位：

| 欄位 | 值 |
|---|---|
| `approval_status` | `pending`、`approved`、`denied`、`expired`、`withdrawn`、`not_required`；`decided` 只出現在沒有決策收據的舊資料 |
| `approval_expires_at` | 等待核准期間為 RFC3339 時間（最多 24 小時），其他時候為 null |
| `isolation_degraded` | 僅當探索真的沒有完整 OS 隔離界線（操作者專用的實驗性 unconfined 模式）時為 true |
| `degraded` | 為相容保留：探索狀態為 `degraded`，或探索以 unconfined 執行；不再決定隔離提示 |
| `stop_code` | null，或 `no_account`、`budget_exhausted`、`rate_limited`、`isolation_unavailable`、`runtime_unsupported`、`cleanup_failed`、`integrity_changed`、`winner_rejected`、`evaluator_unavailable`、`attempt_failed`、`tool_violation`、`other`；不會暴露內部原始原因 |
| `cancel_code` | null，或 `approval_denied`、`approval_expired`、`cancelled_by_user` |

探索任務也會出現在任務看板與任務詳情頁，但在那裡是唯讀的：狀態、標題、指派、釘選、封存與刪除都由探索區管理。伺服器會拒絕一般任務介面對探索任務的修改與刪除，員工交接也不會把探索任務改派給接手者。

公開資訊不包含主機路徑、原始 prompt 或私有策略程式碼。MCP 呼叫者透過對應工具介面及已驗證員工身分提出請求；建立與委派仍須通過伺服器的權限與核准檢查。

## 操作者設定參考

在 `<DUDUCLAW_HOME>/config.toml` 設定 Discovery，準備核准的起始工作區，並把可信評分器放在 `<DUDUCLAW_HOME>/discovery/evaluators/<name>`。探索使用專用帳號池，且須有對應 provider 的可用憑證；公開請求不能自行選擇共用通路帳號。

以下 TOML 使用實際設定欄位。使用前須替換全部範例路徑、帳號池名稱及 image digest；這不是已建好的 image 或已活測的部署步驟。

```toml
[discovery]
approved_workspace_roots = ["/srv/discovery/workspace"]
account_pool = ["discovery-dedicated"]
allow_unconfined = false
max_starting_workspace_bytes = 67108864
max_run_bytes = 536870912
max_total_bytes = 2147483648
retained_hours = 24

[discovery.attempt]
sandbox = "container"
strict_usd = false
allow_shared_account_pool = false
memory_bytes = 4294967296
pids = 128
cpu_millis = 1000
tmp_bytes = 134217728
max_snapshot_bytes = 536870912

[discovery.attempt.runtimes.claude]
image = "registry.example/discovery-claude@sha256:<64-lowercase-hex-digest>"
executable = "/opt/runtime/claude"

[discovery.attempt.runtimes.codex]
image = "registry.example/discovery-codex@sha256:<64-lowercase-hex-digest>"
executable = "/usr/local/bin/codex"

[discovery.attempt.runtimes.antigravity]
image = "registry.example/discovery-agy@sha256:<64-lowercase-hex-digest>"
executable = "/usr/local/bin/agy"

[discovery.attempt.runtimes.grok]
image = "registry.example/discovery-grok@sha256:<64-lowercase-hex-digest>"
executable = "/usr/local/bin/grok"

[discovery.evaluators.parser_score]
command = ["/srv/dudu/discovery/evaluators/parser_score/score.py"]
sha256 = ""
sandbox = "container"
image = "registry.example/discovery-evaluator@sha256:<64-lowercase-hex-digest>"
good_solution = "/srv/dudu/discovery/evaluators/parser_score/good"
cheating_solution = "/srv/dudu/discovery/evaluators/parser_score/cheating"
timeout_secs = 30
memory_bytes = 536870912
pids = 64
scratch_bytes = 67108864
timing_sensitive = false
```

此處 `/srv/dudu` 代表設定的 home。評分命令須是 registry 目錄內由操作者擁有的可執行檔；Python script 要有相容的可執行 shebang。只需為想提供的 runtime 加上對應區塊。容器 image 必須包含該 runtime 的 CLI，以及可信 supervisor 使用的 `python3`；runtime executable 必須是 image 內的 Linux 程式，不能把 macOS 主機 binary 掛進去執行。嘗試、評分與策略容器都以 `--pull never` 建立：本機沒有的 image 不會被下載，而是在建立容器時就失敗。使用前請自行 pull 每一個釘選的 image。

登錄評分器時會測已知正解與已知作弊解，再保存通過驗證的目錄雜湊。操作者 CLI 入口是 `duduclaw discover evaluator register parser_score`；員工 CLI 工作階段不能登錄。評分器從 stdin 接收 JSON，最後一個命令列參數是待評工作區。成功的 envelope 為：

```json
{"pass":true,"valid":true,"score":2.5,"fail_class":"ok","feedback":"verified"}
```

拒絕的解法使用 `valid:false`、`score:null` 及非 `ok` 的 failure class。已知作弊解不能取得任何有效分數。Registry 改動後須重新登錄；`timing_sensitive=true` 讓相同評分器雜湊的跨 run 評分串行，等待也計入期限。

## Runtime 與隔離界線

Discovery 支援六個 runtime 系列，提供相同的保證：attempt 只在自己的節點目錄內工作，只能讀、寫、改、搜尋檔案與執行 shell 指令（沒有 MCP、上網、subagent、瀏覽器或產圖），並受步數上限（`max_turns`）限制。

| Runtime | 設定鍵（`[discovery.attempt.runtimes.<鍵>]`） | 步數上限 | 工具限制 | 接受的憑證 |
|---|---|---|---|---|
| Claude | `claude` | 原生 `--max-turns` | `--tools` 與 `--allowedTools` | `ANTHROPIC_API_KEY` 或 `CLAUDE_CODE_OAUTH_TOKEN` |
| Codex | `codex` | 由 gateway 計數 | 以 `-c` 覆寫關閉 MCP、web、多代理、hook、外掛與記憶 | `OPENAI_API_KEY`（同值也會設為 `CODEX_API_KEY`），或憑證文件 |
| Gemini（v1.67.0 起棄用，v1.71.0 移除，改用 Antigravity，見[棄用說明](../../guides/zh-TW/deprecations.md#gemini-cli-runtime)） | `gemini` | 原生 `maxSessionTurns` | `tools.core` 白名單 | `GEMINI_API_KEY` 或 `GOOGLE_API_KEY` |
| Antigravity（`antigravity`，別名 `agy`） | `antigravity` | 由 gateway 計數 | `PreToolUse` hook 拒絕檔案／shell 以外的所有工具 | 只接受 `GEMINI_API_KEY` 或 `GOOGLE_API_KEY` |
| Grok | `grok` | 原生 `--max-turns` | `--tools`、`--disallowed-tools`、`--disable-web-search`、`--no-subagents`、`--no-plan` | `XAI_API_KEY`，或憑證文件 |
| OpenAI 相容（`openai-compat`，別名 `openai_compat`） | `openai-compat` | adapter 自己的迴圈 | adapter 只提供檔案與 shell 工具 | 設定檔所指 provider 的 key；另須設定 `base_url` |

此表只描述 Discovery，整個平台的一般互動 runtime 支援不變。Catalog、建立及執行共用探索能力閘門；只有 image 設定並不足以通過，`discovery.catalog` 也只列出操作者已設定的 runtime。

### 限制如何執行

gateway 會逐行讀取每個 attempt 的事件串流並自行檢查；表中的 CLI 旗標只是第二道防線。所以沒有原生步數旗標的 runtime，得到的上限與有旗標的相同。

- **步數上限。** attempt 超過 `max_turns` 時，gateway 會停掉它的容器。這不算錯誤：工作區裡有什麼就評什麼，和 Claude 的行為一致。Codex 的一步是一次檔案或 shell 工具呼叫，Antigravity 的一步是一次模型生成。Codex 一次生成可能帶多個平行工具呼叫，所以它的上限不會比 Claude 寬鬆。
- **工具限制。** attempt 只要用了檔案與 shell 以外的任何工具，gateway 就停掉它、捨棄這個 attempt，並以停止代碼 `tool_violation` 結束整個探索，不會重試。稽核紀錄會寫入 `discovery_tool_surface_violation` 事件。

### 憑證

憑證只能透過探索專用帳號池提供。每個 runtime 使用上表列出的環境變數。Codex 與 Grok 另外接受以憑證文件提供的訂閱登入：在專用帳號池加入一個 OAuth 帳號（Codex 用 provider `openai`，Grok 用 `xai`），其儲存的密鑰內容就是該 CLI 的 `auth.json`。gateway 會檢查它是不超過 64 KiB 的 JSON 物件，在 CLI 啟動前寫進容器的私有 home，並從 CLI 的環境中移除傳遞它的變數。

CLI 在容器內更新的 token 不會寫回，因為容器結束時 home 就被刪除。請為 Discovery 準備專用登入（用另一個 `CODEX_HOME` 或 `GROK_HOME` 登入一次，把那份檔案放進帳號池），失效時重新放入。與日常登入共用同一份 `auth.json`，若供應商會輪替 refresh token，可能讓其中一邊被登出；目前沒有第一手證據說明實際情況。API key 沒有這個問題。

Antigravity 只能用 Gemini API key。它的 Google 帳號登入存放在作業系統鑰匙圈，容器內沒有可用的路徑。

帳號池裡對該 runtime 沒有可用憑證的帳號會被跳過。CLI 回報認證失敗時，該帳號在這次 attempt 內不再使用；池裡沒有其他可用帳號就以 `no_account` 結束，不會拿同一份失效憑證反覆重試。密鑰與憑證文件以環境變數名稱傳給容器，值不會出現在主機的行程命令列。

### 已知限制

1. `sandbox = "none"`（操作者專用的 unconfined 實驗模式）仍只支援 Claude，其他 runtime 在此模式會以不支援的能力被拒絕。
2. Antigravity 的訂閱登入無法在容器內使用，只能用 Gemini API key。
3. Codex 與 Grok 的憑證文件在 token 更新後不會寫回（見上）。
4. Codex attempt 在步數上限被停止時費用未知，因為 Codex 只在回合結束時回報 token 用量。
5. Codex 的串流不回報模型，節點的模型欄位維持空白。Antigravity 只在啟動事件帶有模型時才記錄。
6. gateway 的檢查讀的是容器內 CLI 自己輸出的事件串流。它能抓到 AI 以正常方式呼叫了不允許的工具；容器內的行程若刻意偽造或攪亂這條串流，圍住它的是容器隔離與預算上限（呼叫次數、時間）。串流出現超過 3 行無法解析的內容時，attempt 會以 `tool_violation` 停止。
7. Antigravity 的 `PreToolUse` hook 設定檔位於 attempt 可寫的 home 內，只當第二道防線；真正讓 attempt 作廢的是 gateway 的檢查。

正式探索的 attempt 與 evaluator 都要求 Container。每個 attempt 只取得自己的可寫工作區、明確揭露的已完成工作區之不可變快照，以及唯讀可信設定。容器使用非 root 身分、唯讀根檔案系統、記憶體／行程／CPU 限制、有限 tmpfs 及可信截止時間 supervisor。成果要能採用，必須先確認清理完成；清理仍未確認時會阻擋執行。

複製前後會檢查 run／全域配額，包含 retry seed 與私有快照。可寫的主機工作區 bind **沒有 OS 強制的硬磁碟上限**。`none` 僅供操作者明確 opt-in 的實驗，狀態會是 `degraded` 且 `isolation_degraded=true`，沒有對等 OS 界線；它不會成為自動 fallback，也不是公開 task body 可選的參數。正式 Native 會拒絕執行。

## 正確解讀費用

`usd_source` 分成 `reported`、`estimated`、`unknown`、`pending`。Reported 來自 runtime 回報的計費資訊，estimated 來自 token 定價估算。非 Claude runtime 的模型若不在價目表內，會標為費用未知，並以每次呼叫的完整預留額度計費，不再拿 Claude 的價格估算；把該模型加進 `~/.duduclaw/models.toml` 後就會變成估算值。未知或尚未完成的呼叫保留預留責任，不補成零美元帳單。Run 帳目包含基礎設施重試與策略開發；畫面上的已評節點 token 是較窄的範圍，不能當作所有 run 呼叫的用量。

`max_usd` 是派工／預留限制，並非 provider 帳單硬上限；進行中的 generation 仍可能超過估算。不支援嚴格美元執行的 runtime 會拒絕該要求。首次 provider 限流或用量封鎖會取消 run，不換帳重試額度；基礎設施重試會還原不可變 seed，重送完全相同的 prompt。

## 從紀錄 world 做夜間學習

要由排程執行，在員工的 `agent.toml` 設定 `[night_engine] enabled = true`。若其他模型階段要保持關閉，保留全域 `[night] llm_enabled = false`；Discovery 回放本身不呼叫 LLM。

[夜間引擎](58-night-engine.md) 能在沒有 LLM 呼叫的情況下，用記錄的 world 比較凍結策略。取樣保留整個 task，給每個 task 相同權重，區分 training 與新的 held-out task，並在候選接受 held-out 評估前，持久標記該證據已使用。預設策略依員工、runtime、模型、評分器／雜湊及分數方向分開保存；採用時核對版本及證據 receipt。

採用閘門至少要求八個 training task、八個不同的 held-out task、training 嚴格改善、held-out 平均提升至少 0.01，以及單側 Wilson／Bonferroni 檢查；平手不算改善。沒有資料、新 held-out task 不足、world 不完整或不相容、期限／取消及證據無效，都維持原預設，產生 no-data／report-only 結果。

Activity 報告包含雜湊、數量、排除原因與統計證據，不外送私有策略原始碼。這些是記錄 world 上的比較，不能證明因果效益或新任務的最佳策略。Discovery 階段是零 LLM，其他 opt-in 夜間階段另有模型預算。

## 相關文件

- [Goal 與驗收迴圈](34-goal-loop.md)
- [即時執行分支](28-live-forking.md)
- [夜間引擎](58-night-engine.md)
- [多 runtime 執行](13-multi-runtime.md)
