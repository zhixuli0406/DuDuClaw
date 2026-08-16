# Agent 行為評測（`duduclaw eval`）

Golden-task 的**行為回歸測試**（behavioral regression）。每個 case 都會透過**與 gateway 相同的 CLI harness 呼叫方式**（stream-json 輸出、`[capabilities]` 工具允許／拒絕清單接線、per-agent 的 `.mcp.json`、`--max-turns` 預算），把一則 prompt 送給一個 agent，解析產生的 transcript，再拿確定性斷言加上可選的 LLM 判官評分規則去檢查它。

這是把 ADK-evalset／Braintrust 的 eval-action 模式搬進 DuDuClaw 的做法：一個 case 對應一份 TOML 檔案，一個 CI 能拿來把關的 exit code，加上離線重放模式，讓回歸問題不必花費 token 就能被抓到。

> **為什麼這件事對一個會自我演化的平台特別重要。** DuDuClaw 的 GVU 迴圈會改寫
> `SOUL.md`，並用自己的 Verifier 驗證自己做的改動。這個 Verifier 就在迴圈
> *裡面*：它可能跟著自己評分的對象一起漂移。Eval 正是**外部量尺**：一套固定、
> 由人撰寫的預期行為集合，不論是 prompt 改動、runtime／provider 換人、
> `claude` CLI 升級，還是 GVU 改寫 `SOUL.md`，都**不能悄悄讓它退步**。詳見下方
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
| `--record` | 即時執行一次，然後把原始 stream-json 寫到每個 case 旁邊，存成 `*.transcript.jsonl` 基準檔，供之後 `--replay` 使用。 |
| `--no-judge` | 即使 case 開啟了 `[judge]` 評分規則也跳過它（完全確定性、零成本）。 |
| `--report <path>` | 寫出 JSON 報告（每個 case 的斷言結果、判官分數／理由、transcript 診斷、耗時）。 |

**Case id 與 suite 唯一性。** 每個 case 的穩定 id 就是它的檔名主體（`[case] name` 仍是給人看的標題，不是身分識別；`--filter` 比對的是 `name`，`--case` 比對的是 id）。同一次執行中，若有兩個 case 檔案共用同一個檔名主體，suite 會在載入階段就直接失敗，因為悄悄撞名的 id 會讓 `--case` 產生歧義。

**Exit code：** 只要有任何一個 case 失敗，整個程序就會回傳**非零** exit code，直接可以接進 CI 閘。主控台會印出人類可讀的表格；`--report` 檔案是機器可讀的對應版本，現在還多帶了一份精簡的 `{suite, total, passed, per_case: [{id, name, passed, failed_assertions, judge_score, mast_class}]}` 結構（除了原本就有的詳細 `cases` 陣列之外），給 gateway 的 `eval_runner` 這類程式化使用者讀取。

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

## 演化整合：外部量尺

Eval 是演化引擎內部驗證器的**獨立**對照組：

- 內部驗證器評分時，用的是模型*自己*的判斷去評一個提案，可能跟著它評分的行為一起漂移。
- 一個 eval suite 評分的對象是*正在跑的 agent*，拿去對照的是**人類撰寫、寫死的預期行為**，不會因為 agent 的規則變了就跟著變。如果某條學到的規則悄悄丟掉了「一定要引用退款政策頁面」這個行為，一條 `must_use_tools` / `output_regex` case 就會亮紅燈，即使內部驗證器已經核准了這次改動。

自 v1.53 起這條線已經上線，而且是**條目層級**的（AEE，也就是預設的演化引擎，見
[`docs/architecture/evolution-engine.md`](../../architecture/zh-TW/evolution-engine.md) 第 12 章）：

- 每個 playbook 條目在建立時都必須連結 ≥1 個 eval case（G6），並附上會針對已錄製 transcript 做零 LLM 重放的 E1 斷言（`G-Assertions` 閘；找不到 transcript 時誠實標記*未驗證*，絕不悄悄放行）。
- AEE 的 Measure 步驟會用 subprocess 方式（runtime-agnostic，絕不 in-process）跑 `duduclaw eval … --replay --report` 來為候選打分，再讀取 JSON 報告。
- 一輪改動 commit 之後，每個條目會在 `aee_settle_hours` 之後各自結算（確認／回滾），依據的是**它自己連結的那個 case**：一旦退步，只會回滾造成問題的那一個條目。

舊版 SOUL.md 路徑（透過 `[evolution] legacy_soul_evolution = true` 選擇加入）仍然使用整份檔案的 24 小時觀察期（`ObservationFinalizer` / `duduclaw evolution finalize`），它的後續指標來自 `prediction.db` + `feedback.jsonl`，不接這條 eval 線。

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
`assertions.rs`（確定性檢查）、`judge.rs`（LLM 評分規則，重用 RFC-26 fork-judge 的 `LlmCaller` 管線）、`runner.rs`（即時 spawn 與重放），以及
`mod.rs`（整體協調與報告產出）。
