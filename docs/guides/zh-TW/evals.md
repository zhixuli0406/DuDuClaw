# Agent 行為評測（`duduclaw eval`）

Golden-task 的**行為回歸測試**（behavioral regression）。每個 case 都會透過**與 gateway 相同的 CLI harness 呼叫方式**（stream-json 輸出、`[capabilities]` 工具允許／拒絕清單接線、per-agent 的 `.mcp.json`、`--max-turns` 預算），把一則 prompt 送給一個 agent，解析產生的 transcript，再拿確定性斷言加上可選的 LLM 判官評分規則去檢查它。

這是把 ADK-evalset／Braintrust 的 eval-action 模式搬進 DuDuClaw 的做法：一個 case 對應一份 TOML 檔案，一個 CI 能拿來把關的 exit code，加上離線重放模式，讓回歸問題不必花費 token 就能被抓到。

> **為什麼這件事對一個會自我演化的平台特別重要。** DuDuClaw 的進化引擎（AEE）會學習
> playbook 規則，並用自己的 Gate 與 Measure 驗證這些規則。這道檢查就在迴圈
> *裡面*：它可能跟著自己評分的對象一起漂移。Eval 正是**外部量尺**：一套固定、
> 由人撰寫的預期行為集合，不論是 prompt 改動、runtime／provider 換人、
> `claude` CLI 升級，還是新提交的 playbook 規則，都**不能悄悄讓它退步**。詳見下方
> [外部量尺](#演化整合外部量尺)。

---

## 快速開始

```bash
# 離線模式（不需要 agent、不需要憑證，確定性回歸）：
duduclaw eval evals/examples/greeting-replay.toml --replay
duduclaw eval evals/examples/grounded-replay.toml --replay

# 即時模式（跑一個真實 agent，錄下之後可重放的基準 transcript）：
duduclaw eval evals/examples/refund-flow.toml --record

# 跑整個 suite（遞迴搜尋、依排序），並寫出機器可讀的報告：
duduclaw eval evals/support --report eval-report.json
```

`PATH` 可以是單一 `*.toml` 的 case 檔案，**或**一個 suite 目錄（遞迴搜尋，依排序後的順序執行）。預設值為 `./evals`。

### 旗標

| 旗標 | 說明 |
|------|------|
| `--filter <substr>` | 只跑 `[case] name` 包含 `<substr>` 的 case。這是子字串比對，不保證唯一，唯一比對請見下方的 `--case`。 |
| `--case <id>` | 用穩定 id 精確選取 case（id 就是 case 檔案的**檔名主體**，例如 `p0-ceo-boundary-money-001`）。可重複指定或用逗號分隔。它不會為了判斷要不要跑而先載入 case，也不會像 `--filter` 那樣出現歧義。 |
| `--exclude-dir <name>` | 排除某個目錄名稱下的 case 檔（可重複指定），例如用 `--exclude-dir held-out` 跳過 held-out 輪替。不指定就照舊涵蓋全部（預設行為不變）。 |
| `--replay` | 解析已錄製的 `*.transcript.jsonl` 檔案，不即時跑 agent（離線、零憑證）。與 `--record` 互斥。 |
| `--record` | 即時執行一次，然後在每個 case 旁邊寫出 `*.transcript.jsonl` 基準檔。case 可以釘選 `[case] runtime` 加 `[case] model` 來錄製非 Claude 的基準，transcript 由該 runtime 可觀察的事件合成。使用 `--record` 時仍禁止 CLI 的 `--model` 覆寫與指定非 Claude runtime 的 CLI `--runtime` 覆寫，免得一次執行悄悄蓋掉另一個宣告過的基準。 |
| `--no-judge` | 即使 case 開啟了 `[judge]` 評分規則也跳過它（完全確定性、零成本）。 |
| `--report <path>` | 寫出 JSON 報告（每個 case 的斷言結果、判官分數／理由、transcript 診斷、耗時，以及一個 `stats` 區塊，見[誠實統計](#誠實統計)）。 |
| `--repeats <N>` | 每個 case 跑 `N` 次，彙總它的通過**率**，取代單次帶雜訊的 0/1（預設 `1`，行為不變）。見[誠實統計](#誠實統計)。 |
| `--baseline <report.json>` | 與先前寫出的 `--report` 檔案做成對統計比較。見[誠實統計](#誠實統計)。 |
| `--mde <fraction>` | 解析度檢查所宣告的最小可偵測效應，以通過率的小數表示（預設 `0.10` = 10 個百分點）。見[誠實統計](#誠實統計)。 |
| `--cluster-by <key>` | cluster-robust 標準誤的分群鍵。目前只實作 `dir`（預設，以每個 case 所在目錄分群），其他值一律拒絕。見[誠實統計](#誠實統計)。 |
| `--runtime <id>` | 由哪個後端執行每個 case（`claude` 為預設，也是 P2 之前的路徑；另有 `codex`、`gemini`（v1.67.0 棄用，v1.71.0 移除，見 [deprecations](deprecations.md#gemini-cli-runtime)）、`antigravity`、`grok`、`openai_compat`，或任何其他 catalog 內的 runtime id）。未知的 id 會被拒絕，不會當成 `claude`。**省略時採用各 case 自己的 `[case] runtime`，沒有則用 `claude`。** 見[能力矩陣](#能力矩陣--matrix)。 |
| `--model <id>` | 在 `--runtime` 範圍內，對每個 case 覆寫 model id。省略時採用各 case 自己的 `[case] model`。報告標頭的 `model` 一律寫明實際跑的是哪個。 |
| `--paired-seeds` | 為每組 `(case id, repeat)` 推導出確定性的 seed，讓同一批抽樣能跨 model 對齊（Miller 的成對設計）。**只記錄，不套用**：本版本沒有任何 runtime 能吃 seed，每次執行都會以 `seed_applied: false` 註明這點。 |
| `--agent <id>` | 改用**這個**已佈署的 agent 執行每個 case，而不是各 case 自己的 `[case] agent`。報告標頭會記為 `agent_override`，每次執行也會記錄 `agent`。見[借用單一 agent](#借用單一-agent--agent)。 |
| `--matrix` | 量測 role→model 能力矩陣，而不是把 suite 跑一次。見[能力矩陣](#能力矩陣--matrix)。它帶出 `--roles`、`--models`、`--weak`、`--strong`、`--domain`、`--budget-usd`、`--max-cases`、`--temperature`，這些旗標**沒有 `--matrix` 就會被拒絕**。 |

`--record` 與指定非 Claude runtime 的 CLI `--runtime` 覆寫（`--runtime claude` 可以），或任何 CLI `--model` 覆寫同時使用時會被**拒絕**：這樣錄製會把每個 case 已 commit 的基準 transcript，換成該 case 並未宣告的 model 所跑出的結果（若是非 Claude runtime，還會換成保真度不同的合成 transcript）。請改在 case 檔案裡釘選，也就是 `[case] runtime` 加 `[case] model`，然後不帶任何覆寫去錄製。沒有 CLI 覆寫時，實際執行用的就是這兩個欄位；CLI 覆寫仍然優先於它們。

**Case id 與 suite 唯一性。** 每個 case 的穩定 id 就是它的檔名主體（`[case] name` 仍是給人看的標題，不是身分識別；`--filter` 比對的是 `name`，`--case` 比對的是 id）。同一次執行中，若有兩個 case 檔案共用同一個檔名主體，suite 會在載入階段就直接失敗，因為悄悄撞名的 id 會讓 `--case` 產生歧義。

**Exit code：** 只要有任何一個 case 失敗，整個程序就會回傳**非零** exit code，直接可以接進 CI 閘。主控台會印出人類可讀的表格；`--report` 檔案是機器可讀的對應版本，現在還多帶了一份精簡的 `{suite, total, passed, per_case: [{id, name, passed, failed_assertions, judge_score, mast_class}]}` 結構（除了原本就有的詳細 `cases` 陣列之外）、與 `mode` 並列的 `model` 標頭，以及 `stats` 區塊和頂層的 `verdict`／`label`／`resolution_ratio_q`（見[誠實統計](#誠實統計)），給 gateway 的 `eval_runner` 這類程式化使用者讀取。

---

## Case 格式

一個 case 對應一份 TOML 檔：

```toml
[case]
name   = "refund-flow"          # [a-zA-Z0-9_-]，≤64 字元；顯示在報告中
agent  = "support-bot"          # ~/.duduclaw/agents/<agent> 底下的 agent id
prompt = "A customer asks for a refund on order #1234. Handle it."
# system_prompt = "..."         # 選填：透過 --system-prompt-file 傳入
# model         = "claude-haiku-4-5"   # 預設值：claude-sonnet-4-6
# runtime       = "codex"       # 為這個 case 釘選後端；未設定 = claude。
                                #   需要 [case] model，且 model 的家族
                                #   必須屬於該 runtime。CLI 的 --runtime
                                #   仍然優先；使用 --record 時（CLI 覆寫
                                #   會被拒絕），這是錄製非 Claude 基準的
                                #   唯一方式。
# team_acceptance = "..."       # 這個 case 透過完整 team 回合驅動時
                                #   （duduclaw eval --team-2x2）使用的
                                #   驗收標準；一般執行會忽略它。
# timeout_secs  = 180           # 即時執行的 wall clock 上限（1..=3600）
# max_turns     = 25            # CLI 的 --max-turns（1..=100）
# transcript    = "custom.jsonl" # 重放檔案，相對於這個 case 檔案；
                                #   預設值：<case 檔名主體>.transcript.jsonl

[expect]                        # 所有欄位皆為選填；每個「有設定」的欄位
                                # 都會在報告中對應到剛好一條斷言
must_use_tools     = ["tasks_create"]  # 必須至少被呼叫一次
must_not_use_tools = ["Bash"]          # 絕不能被呼叫
output_contains     = ["1234"]         # 最終答案中的子字串，區分大小寫
output_not_contains = ["sk-ant-"]      # 最終答案中不能出現
output_regex        = "(?i)refund"     # 最終答案必須符合的 Rust regex
min_text_blocks     = 1                # 至少 N 個 assistant 文字區塊
max_tool_calls      = 10               # 最多 N 個 tool_use 區塊（budget 護欄）

# 零個或多個 trace-grounding 斷言，詳見下方「Trace grounding」一節
[[expect.grounded]]
tool               = "memory_search"   # 必須被呼叫至少一次且不能出錯
min_overlap_chars  = 12                # 預設 12；CJK-safe 字元數
# output_regex     = "30 days"         # 選填，見下方說明

[judge]                         # 選填的 LLM 評分規則（Braintrust scorer 風格）
enabled   = true                # [judge] 區段存在時預設為 true
rubric    = "Politely acknowledges the refund and cites the order number."
min_score = 0.7                 # score >= min_score 時通過（0.0..=1.0）
```

載入時會強制檢查以下規則（fail-fast，錯字絕不會讓 suite 只跑一半）：

- case **必須**至少定義一條 `[expect]` 斷言，**或**啟用 `[judge]`。沒有任何檢查項目的 case 會被拒絕。
- **未知欄位一律拒絕**，例如打錯字的 `tool_calls_includ` 會直接載入失敗，絕不靜靜放過。
- `output_regex` 必須能編譯成功；`min_score` 必須落在 `0.0..=1.0`；`timeout_secs` 與 `max_turns` 都有範圍檢查；`transcript` 路徑不能是絕對路徑，也不能包含 `..`（case 檔案不能被用來誘騙讀取任意檔案）。
- 格式錯誤的 case 一律回報成**帶原因的 FAILED case**，絕不會被跳過。壞掉的 suite 沒辦法偷偷混出綠色的 CI 結果。

### 工具名稱比對

`must_use_tools` / `must_not_use_tools` 比對工具名稱時，只認**完全相符**或最後一段以 `__` 分隔的片段，屬於 token 錨定比對，不是原始子字串比對。所以 `tasks_create` 能比對到 `mcp__duduclaw__tasks_create`，但 `create` **不會**比對到 `tasks_create`（這遵循專案「安全／路由判斷不用未錨定的 `contains`」慣例）。

### 「output」代表什麼

斷言檢查的對象，是從 stream-json transcript 解析出來的**最終答案文字**（有非空的 `result` 事件就用它，否則用最後一個 assistant 文字區塊），這與 gateway 自己的 stream parser 採用的優先順序相同。工具相關的斷言，檢查對象是依序排列的 `tool_use` 區塊清單。regex 與子字串檢查都是 UTF-8／CJK-safe 的（用 Rust 的 `regex`，不做位元組切片）。

---

## Trace grounding（`[[expect.grounded]]`，GroundEval）

一個 worker 可能給出流暢、切題的最終答案，內容卻**憑空捏造**：沒呼叫過 `memory_search` 就宣稱「查過退款政策，30 天內可退」，或呼叫了卻引用一個工具根本沒回傳過的數字。`must_use_tools` 只檢查工具*有沒有被呼叫*，不管最終答案是否真的反映工具回傳的內容。`[[expect.grounded]]` 正是為了補上這個缺口而存在（GroundEval，arXiv:2606.22737）：

```toml
[[expect.grounded]]
tool              = "memory_search"  # 比對方式與 must_use_tools 相同（完全相符
                                      # 或最後一段 `__` 分隔片段）
min_overlap_chars = 12               # 預設 12
output_regex      = "30 days"        # 選填
```

一條 grounded 斷言只有在**同時滿足**以下所有條件時才算通過：

1. `tool` 至少被呼叫一次，且該次呼叫的 `tool_result` **沒有** `is_error`。
2. 最終答案與該工具至少一則結果文字，共享一段**連續且長度 ≥ `min_overlap_chars` 個字元**的內容（CJK-safe：以 `char` 計數，不是位元組，一段 12 字的中文是 12，不是 36）。
3. 若有設定 `output_regex`，它在最終答案中比對到的子字串，也必須逐字出現在該工具的某則結果文字中。光靠*答案本身*的 regex 相符還不夠，如果被引用的事實從未出現在證據裡，一樣算失敗。

這項檢查需要 transcript 裡有 `tool_result` 的擷取內容（隨這項功能一併加入）。如果 transcript 是在 `tool_result` 擷取功能出現之前錄的，或是透過一個等同 `tool_calls.jsonl` 的結果串流已經遺失的 case 載入的，這條斷言會**直接判定失敗**，並在細節裡提示你 `--record` 一份新的 transcript；證據缺失時絕不悄悄放行。

### 這份證據還會出現在哪裡：goal-mode 驗收

同一份 tool-call 證據，也餵給了**goal-mode 驗收判官**（`DispatchEngine::review_goal_tasks`，WP4）：在為一個 `review` task 打分之前，判官會讀取該 task 從 claim 到 review 這段期間的 `tool_calls.jsonl`，並附上一段精簡的 `<tool_activity>` 區塊（每個工具 `tool: N ok, M err`，最多 20 行）到驗收 prompt 裡。`correctness` 這個面向被明確要求：worker *聲稱*做過、但 `<tool_activity>` 裡完全看不到的動作，一律視為未經驗證。這是盡力而為（best-effort）的機制：稽核檔案缺失或讀不到時，只會省略這個區塊，驗收不會因為觀測性缺口而被卡住。

---

## 即時（Live）與重放（Replay）

| 模式 | 指令 | 需要 | 用途 |
|------|------|------|------|
| **即時（Live）** | `duduclaw eval evals/support` | 已佈署的 agent ＋環境中現成的 `claude` 憑證 | 撰寫 case、發版前的行為檢查 |
| **即時 + 錄製** | `duduclaw eval evals/support --record` | 同上 | （重新）建立回歸基準（`*.transcript.jsonl`） |
| **重放（Replay）** | `duduclaw eval evals/support --replay` | 不需要任何東西（離線） | 針對確定性斷言的 CI 回歸閘 |

- 即時執行是在**agent 目錄內部**跑的，會套用該 agent 的 `[capabilities]` 允許／拒絕工具清單，若有 per-agent 的 `.mcp.json` 也會套用（`--strict-mcp-config`）。它們使用的是執行這條指令的人已登入的 `claude` 帳號，不會做多帳號輪換；eval 是操作者／CI 工具，不是通道路徑。
- Case 刻意設計成**單輪、無 session**（不用 `--resume`），確保可重現。
- `[judge]` 評分規則在**重放**時也會執行（評的是錄下來的最終答案）。加上 `--no-judge` 可以得到完全確定性、零成本的執行。

典型流程：撰寫一個 case，先用 `--record` 跑一次以捕捉一份已知良好的 transcript，把 `*.transcript.jsonl` commit 進去，之後讓 CI 在每個 PR 上跑 `--replay`。當你*刻意*要讓行為改變時，再用 `--record` 更新基準。

錄製隔離：spawn 時，runner 會把該 agent 的 `.mcp.json` 改寫成一份**臨時副本**，讓它的 `DUDUCLAW_HOME` 指向 eval home（`DUDUCLAW_MCP_API_KEY` 則是佔位值），所以就算在 sandbox home 裡錄製，也不會把工具的副作用寫進正式環境，或從正式環境洩漏憑證。原始檔案永遠不會被修改。

失控的執行只會被判定為失敗，不會拖垮整個流程：如果一次即時執行因為 agent 撞到 `max_turns` 上限而中止（無窮工具迴圈），會被記錄成 `error_max_turns`：transcript 仍然能解析，斷言仍然會拿 agent 實際做出來的東西去檢查，這個 case 就當作一次行為失敗基準線計入結果。只有基礎設施層級的錯誤（spawn 失敗、憑證錯誤、transcript 格式損毀）才算硬錯誤。

---

## 誠實統計

原始的通過率（「10 個 case 過了 7 個」）算不上統計量：沒有誤差線，case 只有寥寥幾個時，很容易把運氣當成真正的訊號。當 eval 報告成為 **role → model 能力矩陣**的資料來源時，這一點最要緊：在檢定力不足的 suite 上比較 model，製造出假贏家的機率不比找到真贏家低。

`duduclaw eval` 計算數字的方式比照 A/B 測試，依據三篇論文：

- **Miller 2024，「Adding Error Bars to Evals」**（arXiv:2411.00640，Anthropic）：成對比較針對*逐題*的差異（而非各自獨立的通過率）、case 共享結構時（這裡指它們所在的目錄）使用 cluster-robust 標準誤，以及用來規劃實際需要多少題目／重複次數的樣本數算式。
- **「Resolution Diagnostics」**（arXiv:2605.30315）：只有當 suite 大到足以解析所*宣告*的最小可偵測效應（MDE）時，比較才有意義。`q = n / n_required < 1` 必須回報為 `unresolved`，不可悄悄進位成一個贏家。
- **The Replay Gap**（arXiv:2608.08239）：`--replay` 解析的是某次過去 model 執行留下的*凍結* transcript。拿它去跟*另一個* model 的即時執行相比，會憑空造出一個從未發生過的能力差距。

### 計算哪些數字

每次執行都會算出一個 `stats` 區塊（內嵌在 `--report` 的 JSON 裡），並印出一行主控台摘要：

```
n=42 clusters=6 pass=83.3% ±7.1pp (clustered) | MDE@n=10.0pp | q=1.84 → pass (vs chance)
```

- **`n` / `clusters`**：不同 case 的數量（`--repeats N` 會先把每個 case 的 `N` 次執行彙總成一個通過*率*），以及不同 cluster（目錄，`--cluster-by dir`）的數量。
- **`pass ±X pp (clustered)`**：suite 的平均通過率，以及用 **cluster-robust** 標準誤算出的 95% 信賴區間半寬（Miller 2024 §2.2／附錄 C）。同一目錄下的 case 允許彼此相關（例如共用 fixture、prompt 範本、不穩定的工具），cluster 標準誤會把這點算進去，而不是假裝每個 case 都是獨立的擲硬幣。
- **`MDE@n`**：在 95%／80% 檢定力下，你目前的 `n` 實際上能解析的最小效應（式 10）。如果它遠大於你真正在意的效應，就需要更多 case 或更多 `--repeats`，才能信任比較結果。
- **`q`**：針對你的 `--mde` 計算的解析度比 `n / n_required`（式 9）。`q < 1` 時，不論點估計看起來多漂亮，suite 一律回報 `unresolved`。
- **`→ pass|fail|unresolved (vs chance|vs baseline)`**：結論字詞，加上它在回答哪個問題。見下方[頂層 verdict 優先順序](#頂層-verdict-優先順序)。這個後綴就是為此而生：沒有它的話，`--baseline` 執行的頂層 verdict 與它自己的 `stats.suite.verdict` 可能印出相反的字，看起來像個 bug。
- 當 `n_clusters < 5` 時會印出一行 `WARNING: only <k> clusters`：低於這個數量，`se_clustered`（Miller 2024 附錄 C）根本無法可靠估計 cluster 之間的變異數成分；`stats.suite.small_cluster_warning` 在 JSON 裡帶有同樣的訊號。當 `se_ratio`（cluster 標準誤 ÷ 未分群的單純標準誤）超過 `2` 時，還會印出第二行 `WARNING`，表示同一目錄內的 case 高度相關，不分群的數字會過度自信。（實測範例：一個只有 2 個 cluster 的 suite 回報 `se_ratio: 0.27`。cluster 這麼少時，這個比值本身就沒有意義，這正是 cluster 數量警告與 `se_ratio` 警告各自獨立、彼此分開的原因。）

### `--repeats N`：K 次重複取樣

```bash
duduclaw eval evals/support --repeats 5 --report report.json
```

每個 case 跑 `N` 次，並彙總它的**通過率**（例如 `3/5`），取代單次帶隨機性的 0/1。重複的 transcript 會把序號寫進檔名（`<case>.transcript.r1.jsonl` … `.r5.jsonl`），所以不會互相覆蓋，也不會蓋掉 `N=1` 時使用的 `<case>.transcript.jsonl` 基準（預設的 `--repeats 1` 與既有行為逐位元相同）。對*同一個* prompt 重複做 LLM 取樣是彼此相關的（共享上下文、共享評分寬嚴），所以變異不會像獨立取樣那樣隨 `N → ∞` 縮到 0，最低只能降到單次抽樣變異的 1/3（`Var(mean|K) = Var(mean|K=1)·(1+2/K)/3`）。多重複還是有幫助，只是沒有天真假設的那麼大。

**`--repeats N > 1` 需要即時執行，與 `--replay` 同用會被拒絕。** 重複次數量測的是同一個 case 在 `N` 次獨立取樣之間的*執行間*變異；凍結的 `--replay` transcript 只是一個固定樣本，沒有變異可量（這就是縮小版的 Replay Gap，arXiv:2608.08239：永遠只有一份東西可以重放）。要真的取得 `N` 個樣本，請用即時模式跑 `--repeats N --record`（寫成上述 `.r1.jsonl` … `.rN.jsonl` 檔案），之後再用 `--repeats 1`（預設）對它們做 `--replay`。

### `--baseline <report.json>`：成對比較

```bash
duduclaw eval evals/support --report candidate.json \
    --baseline previous-report.json --mde 0.05
```

依 id（`EvalCaseRef`，即檔名主體）比對先前寫出的 `--report` 檔案中的 case，計算**成對**的逐 case 差異。這比比較兩個獨立通過率更有統計檢定力，因為它能抵消「這題對每個 model 都很難」這類共同因素。結果（JSON 中的 `stats.baseline_comparison`；被採用時它也會決定頂層的 `verdict`／`label`／`resolution_ratio_q`）包含：

- `paired_delta`：配對上的 case 之 `candidate_i − baseline_i` 平均值。
- `corr_with_baseline`：兩次執行逐 case 數值之間的 Pearson 相關係數。當它為**負值**時，配對反而會*放大*變異而不是抵消，所以比較會自動退回未配對的雙樣本標準誤，並設定 `fallback_to_unpaired: true`。
- `ci95_low` / `ci95_high`、`resolution_ratio_q`、`verdict`、`label`。

**違反 Replay Gap 時是拒絕，不會捏造。** 如果這次執行或 baseline 其中之一是 `--replay` 模式，*而且*兩次執行釘選了不同的 model（報告在既有的 `mode` 旁邊帶有 `model` 標頭），比較就會被拒絕：`baseline_comparison.error` 會說明原因，頂層 verdict 則退回這次執行自己單獨的「通過率對機率線」檢查。同一個 model 的 replay 對 replay 比較（跨程式碼版本的回歸檢查，而非 model 比較）不受影響。兩份報告沒有任何重疊的 case id，同樣會明確回報 `error`，絕不捏造出一個平手。

### 頂層 verdict 優先順序

JSON 根層的 `verdict`／`label`／`resolution_ratio_q`，以及主控台摘要裡的 `→ 字詞`，**並不一定與** `stats.suite.verdict`／`.label` 是同一個計算，兩者合理地可能不一致。實測範例：一次候選執行在頂層印出 `→ pass`，而 `stats.suite.verdict` 卻是 `fail`（平均通過率 `15%`）。兩個數字都正確，只是回答的問題不同：

| | 比較對象 | 通過線 | 回答的問題 |
|---|---|---|---|
| **頂層，`--baseline` 被採用** | candidate 對 baseline，成對 | `0`（無差異） | 「這次執行相對於 baseline 有改變嗎？」 |
| **頂層，沒有 `--baseline`（或被拒絕／沒有重疊）** | 這次執行的通過率 | `0.5`（機率） | 「這次執行的通過率，能和擲硬幣區分開嗎？」 |
| **`stats.suite.verdict` / `.label`** | 這次執行的通過率，**永遠如此** | `0.5`（機率） | 同樣是單獨的那個問題，永遠會計算、永遠會回報，**即使 baseline 已經覆蓋了頂層欄位。** |

所以一個 `15%` 的通過率，只要比一個更差的 baseline 有進步，頂層還是可能印出 `→ pass (vs baseline)`；`stats.suite.verdict: fail` 則同時另外告訴你，`15%` 本身用單獨的機率線檢查，並不能和一次健康的執行區分開。看主控台那行的 `(vs baseline)`／`(vs chance)`，或看 JSON 裡 `stats.baseline_comparison` 是否非 null，就知道你看到的是哪一個。

### `verdict` / `label`

每個已解析的點（整個 suite、每個目錄一列，以及存在時的 baseline 比較）都會歸入下列其中一類：

| `verdict` | `label` | 意義 |
|-----------|---------|------|
| `unresolved` | `Candidate` | `q < 1`：case／重複次數／cluster 不足，根本無法解析所宣告的 `--mde`。這是樣本量不足，不是判斷。 |
| `unresolved` | `IndistinguishableFromLuck` | `q >= 1`，但 95% 信賴區間仍然跨過通過線（單獨檢查時是機率 `0.5`，`--baseline` 時是 `0` 的無差異）：資料量已經夠了，而結果顯示這個結局無法與機率／baseline 區分。 |
| `pass` | `Supported` | 已解析，且信賴區間完全落在通過線之上。 |
| `fail` | `Supported` | 已解析，且信賴區間完全落在通過線之下。這裡的 `Supported` 指*結論*（真的有退步）有證據支持，並不表示這次執行通過。 |

`label` 刻意比照 `duduclaw-gateway::prediction::calibration::HonestLabel` 的三態命名紀律（同一套 `Supported`／`Candidate`／`IndistinguishableFromLuck` 詞彙，也用於 task forward model 的校準檢查）。兩者統計量並不相同（`calibration.rs` 以 Sharpe ratio 的 PSR 檢查為閘，這裡以解析度比加上信賴區間是否跨線的檢驗為閘），但遵守的是整個平台共通的紀律：絕不回報第四種比較軟的狀態，例如「看起來有效」。

---

## 用 SOUL.md 起步搭建 suite（`eval-scaffold`）

從空白頁開始寫第一個 case 是最難的一步，而且 playbook 的 `Add` 流程要求至少連結 1 個 eval case（G6）並附上 E1 斷言，所以一個沒有 suite 的 agent 沒辦法長出新的 playbook 條目。`eval-scaffold` 會直接從你已經寫好的東西衍生出草稿 case，也就是 agent 自己的 SOUL.md 行為規則（身分區段完全不動），全程零 LLM：

```bash
duduclaw eval-scaffold --agent my-bot
# → <home>/evals-drafts/my-bot/draft-*.toml，每條行為規則各一份
```

草稿刻意設計成**不能直接執行**：每個 `prompt` 都是一個待辦事項，需要你自己填寫（工具不會自己捏造使用者訊息），而且它們會放在正式 suite 根目錄**之外**，這樣未經審查的草稿就永遠不可能污染基準線。審查流程：

1. 幫 `prompt` 填上一句真的會觸發這條規則的訊息。
2. 收緊 `[expect]`（至少一條工具或輸出斷言）。
3. 把檔案搬到 `<home>/evals/my-bot/`，然後跑
   `duduclaw eval <該目錄> --record`。

重複執行這個指令永遠不會覆蓋你已經改過的草稿（要重新產生就加 `--force`）。

---

## CI 範例（GitHub Actions）

重放模式不需要憑證，所以很適合當標準的 PR 閘。非零 exit code 會自動讓這個 job 失敗。

```yaml
name: agent-evals
on: [pull_request]

jobs:
  evals:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build duduclaw
        run: cargo build -p duduclaw-cli --release
      - name: Run behavioral evals (offline replay)
        run: |
          ./target/release/duduclaw eval evals \
            --replay --no-judge \
            --report eval-report.json
      - name: Upload eval report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: eval-report.json
```

若也想讓 `[judge]` 評分規則在 CI 裡也跑，拿掉 `--no-judge`（並提供 `CLAUDE_CODE_OAUTH_TOKEN` 或一組 API key）。如果要做夜間的**即時**行為檢查，在一台已佈署 agent 且已登入 `claude` 的自架 runner 上跑同一條指令，只是不加 `--replay`。

---

## 能力矩陣（`--matrix`）

Team-as-Agent P2。一位 AI 員工可以讓它的 規劃／執行／審核 角色跑在不同廠商的 model 上，這就帶出一個把 suite 單純跑一遍答不了的問題：*哪個角色的 model 選擇真正有影響，這個 model 到底能不能擔任那個角色？* `--matrix` 就是用來量測這件事。

### 一個 cell 是什麼

一個 cell 是一組 `(domain, role, runtime, model)`。`domain` 是一個 eval suite 目錄；cell 的分數是在該 suite 的 case 上量測的，每個 case 跑 `K` 次。

| 角色 | cell 的量測方式 | 分數 |
|------|-----------------|------|
| **executor** | 在該 `(runtime, model)` 上即時執行 case 的 prompt，並檢查確定性的 `[expect]` 斷言。沒有 LLM 判官，斷言就是全部的評分。 | 通過率 |
| **verifier** | 把 case 的**已錄製** transcript 和驗收標準給 model 看，請它回答 `PASS`／`FAIL`，再與同一份 transcript 上的斷言結果（gold label）比對。 | 一致率，加上帶 Wilson 區間的 false-accept／false-reject 率 |
| **planner** | `--team-2x2` 對每個 case 的四份隔離副本（planner 弱／強 × executor 弱／強）執行正式的 composer，verifier 固定。只有完整配對的列才會進入 Shapley 估計。一般的 `--roles planner` 單次呼叫路徑仍然被拒絕。 | 搭配強 executor 時，獨立 verifier 的 PASS 率 |

聯合 team 的 2×2 Shapley 計算與它的正式 composer 產生器，可透過 `--team-2x2` 使用。它會拒絕格式有問題的分數，並省略寬度為零的信賴區間。一次修正後的 12 臂即時探測，在不同目錄下的三個合成 case 上完整跑完了 planner→executor→獨立 verifier 的路徑。四個 model cell 都是 `n=3`，且都維持 `unresolved`。在宣稱任何路由結論之前，仍然需要更廣泛、具代表性的多 domain 即時驗收。無法取得的 verifier 判決不計分，不算 PASS。

公開 benchmark 的任務檔案，不能靠把它們的指示複製進 `[case] prompt` 就加入 `--team-2x2`。互動式 benchmark 需要它們自己任務層級的工具、模擬器狀態和獨立的結果判定機制；team harness 必須先保留這些，它們的 case 才能計入這個矩陣。

對 verifier cell 而言，重放 transcript 是正當的，原因在於被測的對象不是 worker：被要求去評判它的那個 model 永遠是即時呼叫的。這也是 verifier cell 不需要 `--replay` 旗標的原因（也是 `--matrix --replay` 被拒絕的原因，見下方硬性規則）。

verifier 的指標有三個，而不是一個：在大多數都會通過的 suite 上，光看一致率會讓一個從不判失敗的 verifier 顯得很好。**False accept**（gold 為 `FAIL`，它卻說 `PASS`）是代價高昂的錯誤，因為它放行了不良成果。**False reject** 只會多花一輪修正。兩種判決都沒給的回覆，會計為 **unparseable**，並排除在這三個比率之外：「產不出可解析的判決」與「判得很差」是兩種不同的發現。

**判決是從 agent 的訊息讀取，絕不從 stream 的某一行讀取。** 若某個 runtime 回傳的是它原始的事件 stream 而不是訊息，會先把訊息還原出來（對 codex 的 `item.completed`／`agent_message`｜`message` 項目，以及 claude 的 `assistant`／`result` 事件，採後者優先）；不是 stream 的內容，或是沒有可還原訊息的 stream，則逐位元原樣通過，所以結構化的判決永遠不會被改寫。有觸發還原的執行，會標記 `message_recovered_from_stream`，讓這個變通做法保持可見。實測動機（smoke 3）：每一則 codex verifier 的回覆都被計為 `unparseable`，`first_line` 是 `{"type":"turn.completed","usage":{…}}`，也就是 stream 的最後一個事件。把 transcript 的某一行當成判決來計分，什麼也量不到。

**接受兩種判決格式。** 每次 verifier 呼叫都會透過與 team 判官相同的 `--output-schema` 管線，要求結構化回覆（`{"verdict": "PASS"|"FAIL", "reasons": [...]}`），但只有 **codex** 會遵守，其餘 runtime 都會記錄後忽略。解析器接受那個 JSON 物件（可選擇包在 fenced code block 裡，任何 runtime 皆可），**或**純文字的首個 token `PASS`／`FAIL` 形式，其餘情況一律 fail-closed 為 `unparseable`。JSON 形式刻意優先嘗試：像 `{"verdict":"PASS","reasons":["... would FAIL if ..."]}` 這樣的回覆，否則會被純文字形式保守的「第一行任何位置出現 FAIL 就以 FAIL 為準」平手規則，因為它自己的理由文字而誤判。實測動機：第一次 smoke 執行中，一個 codex verifier cell 的 4 次回覆全部是 `unparseable`，因為不論 prompt 要求得多明白，codex 都不會穩定地以單獨的判決 token 開頭。

### 瓶頸啟發式，以及它看不到什麼

使用 `--weak`／`--strong` 時，每個角色會得到 Δ = score(strong) − score(weak)，量測在**相同的 case** 上（逐 case 成對差異，配 cluster-robust 標準誤；只有當兩個臂沒有任何共同的 case id 時，才會退回未配對的平均差，而且會明確提示）。Δ 較大的角色，就是 model 花費買到最多效益的地方。

只有當它的信賴區間排除了所有其他角色的區間時，才會被**稱為**瓶頸。否則答案是 `unresolved`，這是個真實的答案，也是只看點估計排名會答錯的情形。當某個角色的 Δ 無法解讀時，它也會被**完全排除在比較之外**：`degenerate_gold`（suite 需要重新錄製）或 `degenerate_interval`（suite 需要更多 case 或更多 cluster）。拒絕理由會指名每個被排除的角色及其原因。

這是 AgentCARD 的 Shapley 探測（arXiv:2606.20629）的**解耦**版本：角色各自獨立、一次量一個，不做聯合 team 執行。這讓它負擔得起（2 個角色 × 2 個 model，而不是 |models|^|roles| 種 team 組態），同時也依其構造而看不到：真正的交互作用（一個只有搭配弱 executor 才有價值的強 verifier）對它是看不見的。請把輸出理解成「先把錢花在哪個角色」，絕不要當成 team 層級的歸因。

### 借用單一 agent（`--agent`）

矩陣量的是 **model**，不是人格設定。所以當一個 suite 是為這個 home 沒有的 agent 撰寫時（針對 premium suite 做探測時很常見），`--agent <id>` 會讓每個 case 都改在一個已佈署的 agent 底下執行：

```bash
duduclaw eval commercial/evals/hr-recruit --matrix --agent agnes ...
```

對探測而言這是可以接受的折衷，而且是**明確宣告，不是推論出來的**：它會改變每個 case 所用的 system prompt，所以報告標頭帶有 `agent_override`，每一列 `runs[]` 也帶有它實際使用的 `agent`。有兩個後果值得說明：跨 suite 的比較，只有在 `agent_override` *相同*的執行之間才成立；而斷言依賴原本那個 agent 的工具或人格設定的 case，可能因為這個原因而失敗，與 model 的好壞無關。

沒有 `--agent` 時，`[case] agent` 未佈署的 case 仍會以與先前相同的「not found」錯誤失敗。如果缺的是覆寫指定的那一個，訊息會同時指出**兩個** id，讓你分辨是哪一個。

### Verifier cell 需要混合的 gold

verifier cell 的分數，是與已錄製 transcript 上確定性 gold 的一致率。如果每個 case 的 gold 都是同一類，一致率就什麼也量不到：一個無條件回答 `FAIL` 的 verifier，在全部為 FAIL 的 gold 上會得 1.00。

所以 cell 會回報 `gold_pass`／`gold_fail` 的計數，當任何一類不存在時，設定 `degenerate_gold: true`，強制 `verdict: "unresolved"` 並帶 `verdict_reason: "degenerate_gold"`，同時在主控台那行印出警告。統計數字本身仍會完整回報（並以 `verdict_statistical`／`label_statistical` 並列保留），沒有任何東西被隱藏，被保留不給的是*結論*。cell 中含有 degenerate gold 的角色，也會被排除在瓶頸比較之外，理由會指名有問題的 model。

> **P2 債務。** 已出貨的 premium suite 所錄製的 transcript，相對於它們目前的斷言已經過時：2026-09-24 的 P0 即時測試量到重放時 360 個通過 98 個，與前一版 binary 的結果相同，所以幾乎每個 case 的 gold 都是 FAIL，每個 verifier cell 都回報 `degenerate_gold`。**在真正有意義的 verifier 矩陣出現之前，必須重新錄製 premium suite（`--record`），或修正它們的斷言。** executor cell 不受影響：它們即時執行，從不讀取已錄製的 transcript。

### 讓 `--budget-usd` 有意義

成本透過 `duduclaw_llm::ModelRegistry` 計價：內建的價格表會在任何即時執行之前，與 `<DUDUCLAW_HOME>/models.toml` 合併。請在那裡加入目前的 model id 與已驗證的價格（相同 schema：`input_mc`／`output_mc`／`cache_read_mc`，單位為每 MTok 的 millicent，`$1/MTok = 100_000` mc）。使用 `--budget-usd` 時，遇到未知的 model 價格，現在會在第一次派發之前就拒絕整次執行；沒有預算時，仍保留標示過的 `$0.05` 替代值，用於診斷性執行。報告的 `cost_estimate.models_not_in_registry` 會列出用了替代值的 id。上限是用執行前的 token 估算來算的：某個 model 回報過用量之後，後續的執行會預留它觀察到最大單次成本的兩倍。這是估算花費的護欄，不是 provider 端強制的計費上限。

### 一個 cluster 不等於零不確定性

使用 `--cluster-by dir` 時，如果一個 suite 的所有 case 都在**同一個**目錄，它就只有一個 cluster，而 Miller 的 cluster-robust 估計量在這裡*恆為零*：只有一個 cluster 時，cluster 之間的殘差總和依構造為零，剛好抵消 CLT 項。這是一個無用估計量的正確值，把它當成信賴區間回報，就會得到一個點：`mean 0.25 ci=[0.25,0.25]`。

所以一個少於兩個 cluster 的 cell（和 Δ）會改為回報**未分群的 CLT 標準誤**，並且註明：在 `se_used` 旁邊標 `se_source: "clt_single_cluster"`，兩個原始估計量（`se_clt`、`se_clustered`）也仍留在報告中以求透明。這是誠實但較弱的估計：它看不到目錄內部的相關性，這正是 `small_cluster_warning` 仍然會觸發的原因。主控台那行也會指名所用的估計量，所以只有一個 cluster 的區間絕不會被標成「clustered」。

**寬度為零**的區間，只有在它真的成立時才會留下：`n == 1`，或所有觀察值都相同。那是未估計的離散程度，不是精確度，所以該 cell 會被強制成 `unresolved` 並帶 `verdict_reason: "degenerate_interval"`，建立在它之上的 Δ 會被標為 `degenerate`，而[瓶頸判斷](#瓶頸啟發式以及它看不到什麼)會拒絕根據它做出解析。

> 實測動機（smoke 3，2026-09-25）：一個單一目錄 suite 的每個 cell 都回報成一個點區間，兩個 Δ 都是零寬度（`Δ executor = +0.250 [+0.250,+0.250]`），瓶頸卻在四個 case 上被宣告為**已解析**，正是這一層要避免的那種錯誤宣稱。同樣的缺陷也在一般單一 suite 路徑的 suite 層級那一列（`stats.suite`）掃出來並修正；它的逐目錄各列本來就是為了這個原因使用 CLT 標準誤。

### 硬性規則

這些規則由程式碼強制執行，文件裡寫的只是說明：

- **`--matrix` 拒絕 `--replay`。** 透過凍結的 transcript 比較 model，就是 Replay Gap（arXiv:2608.08239）：model A 的錄製內容，對 model B 什麼也說明不了。
- **`--matrix` 絕不錄製。** 在這裡錄製，會用別的 model 的執行結果覆蓋掉某個 domain 的基準 transcript。
- **宣告低於正式環境的 `--temperature` 會被拒絕**（Miller 2024 §3.3）：調低 temperature 會壓低執行間的變異，製造出實際部署的系統並不具備的解析度。本版本沒有任何 runtime 對外提供 temperature 旋鈕，所以被接受的值只會記錄在標頭中，不起作用。
- **`q < 1` 的 cell 是 `unresolved`**，不可當成排名來解讀。宣告的 MDE 會印在主控台摘要裡，並寫入報告與矩陣標頭。
- **由*不同* `(runtime, model)` 回答的執行**（gateway 的 failover 換成了別的）會被排除在它的 cell 之外，計為 `substituted`，絕不記到被要求的那個 model 頭上。
- **執行是嚴格串行的**，一次一個 CLI spawn：這些執行爭用的是你自己的帳號額度，平行的矩陣既會讓自己被限流，也會讓自己的樣本彼此相關。

### 非 Claude runtime：transcript 是什麼

`--runtime claude`（預設）會與每一次 P2 之前的執行完全相同地 spawn `claude` CLI。其他每個 runtime 都經過 gateway 的 runtime 抽象層（`runtime_dispatch::run_agent_prompt`），所以各廠商的 argv 規則留在它自己的 runtime 模組中，不會在這裡重新實作一遍。它的 transcript 是由 `(最終文字, 該 runtime 自己的原生工具事件)` 以 CLI 的 stream-json 形狀**合成**出來的，所以 `must_use_tools`／`max_tool_calls`／`[[expect.grounded]]` 都能透過現有的同一個解析器繼續運作。合成的檔案會以一個 `duduclaw_eval_synthetic` 系統事件自我標示。

保真度警告：合成的 transcript 只帶有該 runtime 的事件 stream 實際帶有的東西，也就是一個文字區塊（所以 `min_text_blocks` 永遠只能觀察到 `1`）、沒有 thinking 區塊，而工具輸入是收集器記錄下來經過遮罩並截斷的文字，不是原始的 JSON。這些訊號在兩條路徑之間**不能比較**，所以同一個 cell 絕不混用它們。

Antigravity `agy` 1.2.10 現在會向它的 runtime 提供 `stream-json` 的終端工具事件與實測的 token 用量。已驗證過一次即時的 sandbox 拒絕工具事件，以及一個不使用工具的 PING eval；成功的原生工具執行仍需另外做一次即時檢查。verifier cell 的金額估算仍然粗略，因為那個 utility 呼叫沒有把用量回傳給矩陣報告器。

### 輸出

`--report <path>` 會寫出兩個檔案：

- **`<path>`**：JSON，包含 `header`（宣告的 MDE、α、檢定力、K、cluster 鍵、`planner: "deferred"`、`replay_forbidden`、宣告的 temperature、`agent_override`、要求的 `verifier_output_schema`，以及所要求的 roles／models／domains）、`cells`（每個 cell：`n`、`n_clusters`、`mean`、`se_clt`、`se_clustered`、`se_ratio`、`ci95_low/high`、`n_required_for_mde`、`resolution_ratio_q`、`mde_at_n`、`q_note`、`se_used`、`se_source`、`verdict`、`label`、`verdict_reason`、`verdict_statistical`／`label_statistical`、`degenerate_gold`、`degenerate_interval`、`small_cluster_warning`、`errors`／`skipped`／`substituted`，而 verifier cell 另有 `verifier` 計數，含 `gold_pass`／`gold_fail`／`unparseable`／`degenerate_gold`）、`bottleneck`（每個角色的 Δ 與區間，加上已解析／未解析的結果及其原因）、`cost_estimate`、`budget_stop`，以及每一列 `runs[]`。
- 旁邊的 **`role_model_matrix.toml`**：team composer 讀取的長期先驗。composer 讀的位置是 `<DUDUCLAW_HOME>/role_model_matrix.toml`，要讓它生效請把檔案複製到那裡。它用在兩件事：`[team.roles.*]` 裡沒寫 `model` 的角色，會採用該角色自己 runtime 上最好的 **resolved** cell（跨 domain 以 n 加權；unresolved 的 cell、平手、勝出者屬於別的模型家族、這台主機上沒有 CLI 或憑證的 runtime 都會被忽略，退回員工的 `[model] preferred`）；團隊閘門的能力差距訊號，則拿執行者目前的模型和這個勝出者比較，並以同一份檔案宣告的 MDE 判斷（見 [Goal loop 的閘門](goal-loop.md#閘門)）。驗證不過的檔案會被忽略。報告標頭的 `planner: "deferred"` 只表示 `--matrix` 本身不量測 planner 角色。一個 `[header]` 加上每個量測過的 cell 各一個 `[[cell]]`。算不出來的統計量是**缺少該鍵**，絕不捏造一個數字；一個沒有任何可用觀察值的 cell 會有報告列，但沒有矩陣 cell。這個檔案在寫入與讀取時都會驗證，所以手動編輯造成的重複 cell 或未知的 runtime id 會被拒絕，不會被採信。每個 cell 還帶有 `conditioned_on`，一個簡短的 token，指名量測它的時候，**其他**角色被固定在什麼狀態：`--matrix` 一次只跑一個角色，迴圈裡沒有其他角色的 model，寫成 `solo`；`--team-2x2` 探測在量 planner 臂時用強 executor（`executor=strong`），量 executor 臂時用強 planner（`planner=strong`）。`conditioned_on` 不同的 cell，*不是*在相同條件下量測的，不可直接比較。這個欄位出現之前，檔案裡沒有任何東西說明這一點。缺少此鍵表示產生者沒有記錄條件（較舊的檔案），絕不會寫成空白。

### 成本與預算上限

`--budget-usd <cap>` 會在派發每次執行**之前**檢查，所以上限是花費的天花板，不會等超支之後才報告。成本透過 `duduclaw_llm::ModelRegistry`（內建價格表加上 `<DUDUCLAW_HOME>/models.toml`）計價：runtime 有回傳用量時，用它實際回報的用量；否則用一個粗略的每次執行 25k 輸入／4k 輸出假設（設計本身的成本模型）。registry 完全不認識的 model，用標示過的固定 $0.05 替代值計價，而這只會發生在沒有 `--budget-usd` 的時候（有預算時，這種 model 會讓整次執行被拒絕，見[讓 `--budget-usd` 有意義](#讓---budget-usd-有意義)）。這三種來源都會逐次標示在 `runs[].cost_source`，絕不混成一個看起來很權威的單一數字。注意 Claude CLI 路徑完全不回報用量，所以只有 Claude 的矩陣，整個都是以粗略假設計價。

### Smoke 執行

```bash
duduclaw eval commercial/evals/hr-recruit --matrix \
  --roles executor,verifier \
  --models claude:claude-haiku-4-5,codex:gpt-5.6-sol \
  --weak claude:claude-haiku-4-5 \
  --strong claude:claude-sonnet-4-6 \
  --repeats 1 --max-cases 6 --mde 0.10 \
  --agent agnes \
  --report reports/matrix-smoke.json
```

要做完整 team 的探測，請替每個選中的 case 加上一個凍結的 `[case] team_acceptance = "..."`。在隔離的 eval home 中、對一個已佈署的 agent 執行：

```bash
duduclaw eval commercial/evals/hr-recruit --matrix --team-2x2 \
  --agent agnes --planner-weak codex:gpt-5.6-terra \
  --planner-strong codex:gpt-5.6-sol \
  --executor-weak codex:gpt-5.6-terra \
  --executor-strong codex:gpt-5.6-sol \
  --verifier-model antigravity:gemini-3.7-flash \
  --team-effort low \
  --repeats 1 --max-cases 4 --report reports/team-2x2.json
```

這條指令需要即時呼叫、明確的報告路徑，以及一個與兩個 executor 臂都不同 model 家族的 verifier。它會拒絕 `--paired-seeds`，因為 composer 無法套用 seed。每個臂會複製 eval home，建立一個 task，並跑一輪真實的 composer 回合。業務 task 與工具不會改動來源 home。探測在複製各臂之前，會先在它的 `config.toml` 中佈建一把內部 MCP key，讓它們的 role member 即使沒有啟動任何 gateway 行程，也能對 `team_handoff` sidecar 進行驗證。每個角色的 MCP 子行程都會收到它所屬臂的 `DUDUCLAW_HOME`，即使呼叫者的環境變數指向的是來源 home 也一樣。`--team-effort` 為所有臂固定相同的推理 effort，並記錄在報告中；省略則使用各 model 設定的預設值。`--team-fanout` 設定一個回合中允許進場的 executor 數量（1–3，預設 1），並記錄在報告中；執行前的估算也會預留一次可能的 executor 修復。請讓 case 的驗收標準符合這個容量。一個只回傳文字、沒有 `team_handoff` packet 的 planner，永遠不會進入驗證；該臂不計分，也不會從一個不完整的四臂列產生任何 executor 或 planner cell。若要在一台 Grok sandbox 無法啟動的主機上做隔離的 Grok 即時探測，`--team-grok-sandbox-off` 會明確地只針對該次評測回合，把 `--sandbox off` 傳給 Grok。報告會記錄 `grok_sandbox_off: true`；正常的 gateway 路徑與角色工具限制不變。報告中的成本，有量測用量時用量測值，否則是粗略估算。主控台會說明是哪一種：第一次退回估算的執行會印出 `WARNING`，探測結束時會印出有多少次執行是以粗略估算而非量測用量計價；最常見的原因是成本遙測的 singleton 已經在行程內別處被綁定，這時 eval home 的 `cost_telemetry.db` 完全不會寫入任何東西。（那個檔案是以唯讀方式開啟，所以探測不會只因為看了一眼就建立出一個空檔案。）`--budget-usd` 會預先檢查已知的 model 價格，並在估算預留額會超過上限的那次執行之前停止；它不是 provider 帳單上限。每個臂複製出來的 eval home，如果 home 大於 256 MiB、巢狀深度超過 32 層目錄，或檔案超過 20,000 個，就會被拒絕。每個臂每次重複都會複製一份，所以請使用精簡的隔離 home。輸出的 TOML 只有在至少一個 case 具備全部四個可用結果時，才會帶有 planner cell。小樣本與退化的區間維持 `unresolved`。位於不同目錄的 case 可以共用同一個 TOML 檔名：矩陣報告與成對統計使用相對於 suite 的路徑當作 case ID（例如 `north/checkins`）。`--case` 接受這個完整 ID，也接受較舊的短檔名；後者會選中所有符合的目錄。

如果這個 home 已經佈署了該 suite 自己的 agent（`hr-recruit`），就拿掉 `--agent agnes`；沒有時就保留，見[借用單一 agent](#借用單一-agent--agent)。

`--max-cases 6` 限制從 suite 取用的 case 數量；`--weak`／`--strong` 的臂即使 `claude-sonnet-4-6` 不在 `--models` 之列，也會被量測（否則 Δ 就沒有臂可比）。在 6 個 case 且 `K=1` 時，實際達到的 MDE 遠比宣告的 10pp 粗糙，所以預期會看到 `unresolved` 的 cell 與 `unresolved` 的瓶頸，這是 smoke 執行誠實的結果，不是失敗。verifier cell 需要每個 case 已錄製的 `*.transcript.jsonl`；沒有的 case 會以 `no_recorded_transcript` 跳過，不計入；而相對於今天過時的 premium 基準，它們還會另外回報 `degenerate_gold`（見 [Verifier cell 需要混合的 gold](#verifier-cell-需要混合的-gold)）。

**Exit code。** 失敗或 `unresolved` 的 cell 是一次*量測*，所以 `--matrix` 回傳 0。它只在規格或基礎設施出問題時才回傳非零：被拒絕的旗標組合、無法寫入的報告，或整個矩陣沒有任何可用觀察值（每次執行都出錯、被跳過或被替換），這時回傳綠燈等於說了不實的話。

---

## 演化整合：外部量尺

Eval 是演化引擎內部驗證器的**獨立**對照組：

- 內部驗證器評分時，用的是模型*自己*的判斷去評一個提案，可能跟著它評分的行為一起漂移。
- 一個 eval suite 評分的對象是*正在跑的 agent*，拿去對照的是**人類撰寫、寫死的預期行為**，不會因為 agent 的規則變了就跟著變。如果某條學到的規則悄悄丟掉了「一定要引用退款政策頁面」這個行為，一條 `must_use_tools` / `output_regex` case 就會亮紅燈，即使內部驗證器已經核准了這次改動。

自 v1.53 起這條線已經上線，而且是**條目層級**的（AEE，也就是預設的演化引擎，見
[`docs/architecture/evolution-engine.md`](../../architecture/zh-TW/evolution-engine.md) 第 12 章）：

- 每個 playbook 條目在建立時都必須連結 ≥1 個 eval case（G6），並附上會針對已錄製 transcript 做零 LLM 重放的 E1 斷言（`G-Assertions` 閘；找不到 transcript 時誠實標記*未驗證*，絕不悄悄放行）。
- AEE 的 Measure 步驟會用 subprocess 方式（runtime-agnostic，絕不 in-process）跑 `duduclaw eval … --replay --report` 來為候選打分，再讀取 JSON 報告。
- 一輪改動 commit 之後，每個條目會在 `aee_settle_hours` 之後各自結算（確認／回滾），依據的是**它自己連結的那個 case**：一旦退步，只會回滾造成問題的那一個條目。

舊版 SOUL.md 路徑那套整份檔案的 24 小時觀察期（`ObservationFinalizer` / `duduclaw evolution finalize`）已於 2026-09-29（S11）移除。現在唯一的觀察窗是條目對自己連結的 eval case 各自定案。

---

## 檔案放在哪裡

```
evals/                              # 你的 eval suite（相對於 repo）
├── examples/
│   ├── greeting-replay.toml        #   離線重放範例
│   ├── greeting-replay.transcript.jsonl
│   ├── grounded-replay.toml        #   離線重放範例（[[expect.grounded]]）
│   ├── grounded-replay.transcript.jsonl
│   └── refund-flow.toml            #   即時範例（需要一個 agent）
└── <suite>/
    ├── <case>.toml
    └── <case>.transcript.jsonl     #   已錄製的基準（透過 --record）
```

實作位於 `crates/duduclaw-cli/src/eval/`：
`case.rs`（格式與驗證）、`transcript.rs`（stream-json 解析）、
`assertions.rs`（確定性檢查）、`judge.rs`（LLM 評分規則，重用 RFC-26 fork-judge 的 `LlmCaller` 管線）、`runner.rs`（即時 spawn、重放與跨 runtime 的執行）、`stats.rs`（Miller／CLT 統計）、`matrix.rs` 加 `verifier_cell.rs`（能力矩陣），以及
`mod.rs`（整體協調與報告產出）。持久化的矩陣型別是
`duduclaw_core::role_model_matrix`（`role_model_matrix.toml`）。
