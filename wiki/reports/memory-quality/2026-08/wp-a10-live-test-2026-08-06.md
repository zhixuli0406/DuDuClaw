# WP-A10 活體驗證報告 — 任務層 forward model 全迴路

- 日期：2026-08-06
- 對象：`commercial/docs/design-task-forward-model-2026-08-06.md` §4 / §9 WP-A10
- 執行者：活體驗證 agent（Opus）
- 首輪結論：**PARTIAL**（A3/A4/B2 全鏈路在證據可見時逐段驗證通過；但發現一個會讓整條鏈路在真實 goal loop 上永遠沉默的產品級 bug，需修）
- **修後複驗（2026-08-06 晚，見 §6）：BUG-1 已修並複驗通過，A3/A4 在無任何測試繞道下自然跑完整迴路且 fidelity 達 `full`；但同源的證據斷點仍留在判官/B3 端（BUG-2，P1）。**

---

## 0. 隔離設定（安全紅線遵守情形）

| 項目 | 實際做法 |
|---|---|
| gateway home | `DUDUCLAW_HOME=/tmp/duduclaw-live-test-home`（`duduclaw_core::platform::duduclaw_home()` 支援此覆寫；`server.rs` 開機時 `set_var` 讓所有子行程繼承） |
| 埠 | `DUDUCLAW_PORT=18999`（非預設 18789） |
| 頻道 | config.toml 完全無 token，未啟動任何頻道 bot |
| launchd | 未註冊（`duduclaw gateway` 不含 autostart 邏輯） |
| 生產環境 | `/Applications/DuDuClaw.app/.../duduclaw run --yes`（PID 52406）全程在跑，未受影響 |
| 汙染檢查 | `grep -a -c "wp-a10"` 於 `~/.duduclaw/{tasks.db,prediction.db,tool_calls.jsonl,message_queue.db}` → 全部 **0** |
| 收尾 | 測試 gateway 與輔助行程皆已 kill；`/tmp/duduclaw-live-test-home`、`/tmp/duduclaw-live-test-workspace` 保留供覆查 |

config.toml 關鍵段：

```toml
[dispatch]
enabled = true
[goal_loop]
iteration_cap = 5
iteration_cap_simple = 5
max_concurrent = 1
tick_secs = 15
[task_forward_model]
enabled = true
rule_induction = true
```

測試 agent：`/tmp/duduclaw-live-test-home/agents/tester`（`duduclaw agent create tester --runtime claude` 產生，autonomy_level 未設 ⇒ 預設 `approver`，無 kickoff 閘）。
測試任務工作區：`agents/tester/workspace/{alpha.txt,beta.md,gamma.json}`（無害唯讀 + 只寫回同目錄）。

goal task 以 `sqlite3` 直接寫入 `tasks.db`（`goal_mode=1`）——`/goal` 需要頻道會話，`tasks_create` MCP 工具不支援 `goal_mode` 欄位，直接寫 DB 是最短路徑。

---

## 1. 主結論先講：一個會讓 A3 永遠不結算的 bug

### BUG-1（P0，產品級，非測試設定）：goal-loop 派工的工具稽核 agent 歸屬錯誤

**現象**：goal loop 派工出去、由 worker agent（`tester`）自己呼叫的每一筆 MCP 工具，寫進 `tool_calls.jsonl` 的 `agent_id` 都是 **`goal-loop-driver`**，不是 `tester`。

一手證據（round 1 的原始兩行）：

```json
{"agent_id":"goal-loop-driver","tool_name":"tasks_claim","timestamp":"2026-08-06T15:09:43.519598+00:00","success":true,...}
{"agent_id":"goal-loop-driver","tool_name":"tasks_complete","timestamp":"2026-08-06T15:10:14.971058+00:00","success":true,...}
```

**根因**：`crates/duduclaw-cli/src/mcp.rs:10102`

```rust
let actual_agent = std::env::var(duduclaw_core::ENV_DELEGATION_SENDER)
    .ok()
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| default_agent.to_string());
```

`goal_loop.rs:1641/1654-1655` 把 `sender`/`sender_agent`/`origin_agent` 都填成 `"goal-loop-driver"`，dispatcher 再經 `DelegationEnv::to_env_map()` 把 `DUDUCLAW_DELEGATION_SENDER=goal-loop-driver` 注進 Claude CLI 子行程 env，於是 worker 自己的工具呼叫被記在 driver 頭上。
（`tasks_claim` 內部 `atomic_claim(task_id, default_agent, ...)` 用的是 `default_agent`，所以 `tasks.claimed_by` 仍正確是 `tester` —— 只有**稽核歸屬**錯。）

**下游影響鏈**（三處全部讀 `tool_calls.jsonl` 且以 `claimed_by || assigned_to` 嚴格比對 `agent_id`，見 `tool_activity.rs:66`）：

1. **A3 觀測層永遠 fidelity=None**：`task_observe::observe_round` 找不到任何紀錄 ⇒ `DiffOutcome::Unobservable` ⇒ **`task_prediction_log` 永遠不 settle、`task_state_models` 永遠不成長、A4 規則永遠不誘導**。A3/A4/B2 在真實 goal loop 上等同全死。
2. **B3 grounding 前置檢查永遠 `Skip{"no tool_use in claim→review window"}`**。
3. **判官永遠拿不到 `<tool_activity>` 區塊** ⇒ 會把「有做事但沒證據」的誠實回報判成假宣稱。本次實測就是這樣：agent 真的產出了 `report.md` + `summary.json`（檔案存在、內容正確），判官仍以「zero tool call evidence / UNVERIFIED」連退兩輪。

round 1 / round 2 的 log 一手節錄：

```
15:10:28 DEBUG dispatch_engine: B3 grounding 前置檢查略過 task=wp-a10-task-1 reason="no tool_use in claim→review window"
15:10:47 INFO  dispatch_engine: goal-mode 驗收未通過 task=wp-a10-task-1 status=revising
15:10:47 WARN  task_observe: [unobservable: wp-a10-task-1 round=1: no tool_calls.jsonl evidence in window 2026-08-06 15:09:28..2026-08-06T15:10:47.156335+00:00] agent_id="tester"
15:10:47 DEBUG dispatch_engine: A3 settle: unobservable this round round=1 reason="no tool evidence in claim→review window (fidelity=None)"
15:13:29 WARN  task_observe: [unobservable: wp-a10-task-1 round=2: no tool_calls.jsonl evidence in window 2026-08-06T15:11:11.368426+00:00..2026-08-06T15:13:29.349212+00:00] agent_id="tester"
```

round 2 的窗口起點已是合法 RFC3339（`claimed_at`），仍然 unobservable —— 這一輪乾淨地把變因收斂到「agent_id 不匹配」單一原因。

判官給的退回理由（`tasks.judge_feedback`，round 1）原文：

> [correctness] Worker claims to have completed the task but provides zero tool call evidence. … there is no `<tool_activity>` block showing Read or Write tool invocations. … Cannot confirm that workspace/report.md or workspace/summary.json actually exist …

但檔案其實存在：

```
-rw-r--r--  184 Aug  6 23:11 report.md
-rw-r--r--   75 Aug  6 23:10 summary.json
```

**未修（依紅線只記錄不動 code）。** 建議修法（擇一，需拍板）：
- (a) `mcp.rs:10102` 改為「`DUDUCLAW_DELEGATION_SENDER` 只用於 delegation 追蹤，稽核歸屬一律用 `default_agent`（= `DUDUCLAW_AGENT_ID`）」；或
- (b) 在 `SYSTEM_SENDERS`（`goal-loop-driver`/`cron`/`heartbeat`/`autopilot`）情況下不覆寫，退回 `default_agent`。
  (b) 較小、較保守，且正好對應「系統發起者不是真正的工具使用者」這個語意。修完必須同時回歸測 dispatcher 一般 delegation 路徑的歸屬（那條路徑 sender 是真的另一個 agent，行為要維持不變）。

### 觀察（非 bug，但值得記）：`created_at` 非 RFC3339 會靜默清空證據窗口

我第一次用 `datetime('now')` 寫 `created_at`（`2026-08-06 15:09:28`，空白非 `T`），`filter_tool_activity` 的 `parse_from_rfc3339` 直接失敗 ⇒ 回空陣列。這是**我的測試設定問題**（生產路徑 `TaskRow::new` 一律寫 RFC3339），已在 round 2 前改正；但也說明該函式對壞邊界是「靜默回空」而非可觀測的降級。若日後有第三方寫 task row，這會是個難查的沉默點。

---

## 2. 驗證點逐項結果

前提說明：為了在 BUG-1 未修的情況下把**其餘環節**驗完，round 3 起我在隔離 home 內跑了一個純測試工具
（`/tmp/duduclaw-live-test-home/fix_attrib.py`），每 2 秒把 `tool_calls.jsonl` 裡 `agent_id: "goal-loop-driver"` 改寫成 `"tester"`，模擬 BUG-1 修好後的狀態。**repo 程式碼一行未改。**
該工具總共觸發 6 次改寫（每輪 claim/complete 各一筆），反向證明 gateway 持續寫入錯誤歸屬。

### (a) `task_prediction_log` 每輪一筆 + fidelity + JSON 合理性 — ✅ PASS

```
id|round|state_key                        |settled_at                       |composite_error|category  |fidelity
1 |1    |tester|research_or_qa|first|1     |                                 |               |          |
2 |2    |tester|research_or_qa|retry|1     |                                 |               |          |
3 |3    |tester|research_or_qa|retry|1     |2026-08-06T15:16:25.093928+00:00 |0.925          |critical  |mcp_only
4 |4    |tester|research_or_qa|retry|1     |2026-08-06T15:18:25.370327+00:00 |0.175          |negligible|mcp_only
```

- 每輪一筆、`(task_id, round)` 唯一 ✅
- `phase` 正確：round 1 = `first`，rejection 重派 = `retry` ✅
- `has_outcome_spec=true` 有進 state_key ✅
- fidelity：round 1/2 因 BUG-1 是 `None`（未結算、只寫 `[unobservable: …]` 一行，符合設計 §3.2「不計算 composite、不產生 error」）；round 3/4 = `mcp_only`（工具證據只有 MCP 子集）✅
- prediction JSON（round 1，`source=prior`、`confidence=0.0`，全冷 ⇒ 未走 LLM，符合 T3 預設 false）：

```json
{
  "task_id": "wp-a10-task-1", "round": 1,
  "state_key": {"agent_id":"tester","goal_kind":"research_or_qa","phase":"first","has_outcome_spec":true},
  "expected_tool_classes": ["read","search"],
  "expected_call_band": [1,10],
  "expected_outcome": "accept",
  "expected_artifact": "text_only",
  "confidence": 0.0, "source": "prior"
}
```

- 統計桶確實長出來（round 3 結算後，canonical + marginal 各一列）：

```
tester|research_or_qa|retry|1  → {"tool_class_counts":{"task_board":1,...},"call_counts":[2],
                                  "outcome_counts":{"reject":1},"artifact_counts":{"file_write":1},"n_samples":1}
tester|research_or_qa          → 同上（marginal 桶）
```

round 4 的 composite 從 0.925 掉到 0.175，就是這個桶生效的直接證據（統計預測已學會「這個 state 會用 task_board、產出是 file_write」）。

### (b) predict/settle hook 有跑、無 panic、`<state>` 進派工 payload — ✅ PASS

- 開機 log：`INFO server: A3 task-forward-model enabled ([task_forward_model] enabled = true)` ✅
- 全程 `grep -iE "panic|ERROR"` → 5 筆全部是 debug 欄位名 `error=`／`errors=0` 的字面命中，**零 panic、零 ERROR 級事件** ✅
- `<state>` 區塊在 `message_queue.payload` 中（round 1 原文）：

```
<state>
<goal>
WP-A10 活體驗證：整理 workspace 檔案清單
…
</goal>
<confirmed_facts>
（尚無）
</confirmed_facts>
<pending_hypotheses>
（尚無）
</pending_hypotheses>
<excluded_approaches>
（尚無）
</excluded_approaches>
</state>
```

- round 2 的 `<state>` 證明 A1 回填確實運作（不再是永遠空的）：

```
<confirmed_facts>
- 結構化產出驗收（outcome schema）已通過 deterministic 零成本校驗。
</confirmed_facts>
…
<excluded_approaches>
- [correctness] Worker claims to have completed the task but provides zero tool call evidence. The result states 'already
</excluded_approaches>
```

（`excluded_approaches` 是判官回饋，CJK-safe 截到 120 字。）

### (c) Significant 以上誤差 → 誘導 task-rule → 下一輪注入 — ✅ PASS

round 3 `composite_error = 0.925 / category = critical`（`McpOnly` 權重 w_outcome=0.75，被 reject ⇒ outcome_error=1.0，加上 tool/artifact 貢獻）。

`memory.db` 兩筆一手輸出：

```
# WP-B2 transition sample（episodic）
task wp-a10-task-1 round 3 (tester|research_or_qa|retry|1): 2 tool call(s), outcome=rejected, composite_error=0.92
tags=["task_transition","state:59b74c9b"]  source_event=task_transition

# WP-A4 induced rule（semantic）
agent tester 在 goal_kind research_or_qa(phase retry)的任務中:預期使用工具類別「read、search」但本輪未使用;
額外使用了預期外的工具類別「taskboard」;預期結果為「accept」但實際為「rejected」;
預期產出形態為「text_only」但實際為「file_write」。
tags=["task-rule","probation-rule","goal_kind:research_or_qa"]  source_event=task_forward_induction
```

- 零 LLM、模板合成、只講「什麼偏離」不講「為什麼」✅
- Janus probation tag ✅

round 4 的派工 payload 第 28-29 行確實帶上規則：

```
## 任務經驗規則
- agent tester 在 goal_kind research_or_qa(phase retry)的任務中:預期使用工具類別「read、search」但本輪未使用;…
```

**規則生命週期結算也驗到**：round 4 結算為 `negligible` ⇒ 注入的規則被記 helpful+1。metadata 一手輸出：

```json
{"rule_stats":{"harmful":0,"helpful":2},"source_round":3,"source_task_id":"wp-a10-task-1"}
```

（`RuleStats::initial()` 在 Janus probation 下 seed `helpful=1`，settle 後變 2，符合設計。）

round 1/2 沒觸發規則誘導，原因不是「誤差不夠大」而是 **BUG-1 導致根本沒結算**——如實記錄，未硬湊。

### (d) grounding 前置檢查行為 — ✅ PASS（行為與該輪工具使用一致）

| 輪 | 檢查結果 | 與工具使用是否相符 |
|---|---|---|
| 1、2 | `Skip{"no tool_use in claim→review window"}` | 相符於**當時系統看得到的**證據（0 筆）；但那是 BUG-1 造成的假象，實際 agent 有用工具 |
| 3、4、對照組 | `Degraded{"tool evidence lacks captured result_text"}` | 相符。該窗口只有 `tasks_claim`/`tasks_complete`，兩者都在 `SELF_ECHO_TOOL_NAMES` 上，`result_text` 被刻意抑制（Fix-2 C1a），因此 fail-open 交給判官 —— 設計上正確 |

即：目前 goal-loop 路徑上 grounding 前置檢查**從來不會真的 Grounded 或 Reject**，因為唯一看得到的工具就是 self-echo 的兩顆 task_board 工具。這不是 bug，但是 §8.2「runtime × capability 矩陣」的現況值得記一筆：**要讓 B3 真的有牙齒，需要先把原生工具（Read/Write/Bash）的證據收集接上**（設計文件已標為 WP-A4/A5 的 `Full` fidelity 工作）。

### (e) 對照組 `enabled = false` — ✅ PASS

停 gateway → 改 `[task_forward_model] enabled = false` → 重啟 → 建 `wp-a10-control-1` 跑完整一輪（派工 → agent 執行 → 判官退回 revising）。

| 指標 | 主測後 | 對照組跑完一輪後 |
|---|---|---|
| `task_prediction_log` | 4 | **4**（零新增） |
| `task_state_models` | 2 | **2**（零新增） |
| `memories` | 3 | **3**（零新增） |
| 開機 log `A3 task-forward-model enabled` | 1 次 | **0 次** |
| 對照組 log 中 `A3 `/`A4 ` 行 | — | **0 行** |
| 派工 payload 含 `## 任務經驗規則` | 是（round 4） | **0**（規則注入正確關閉） |
| 派工 payload 含 `<state>` | 是 | **是**（A1 不受 A3 開關影響，正確） |
| goal loop 本身 | 正常 | **正常**（照常派工、執行、驗收、退回） |

---

## 3. 端到端時間軸（主測，一手 log 對照）

| 時間 (UTC) | 事件 |
|---|---|
| 15:08:58 | gateway 起，A3 enabled、dispatch engine、goal loop driver 三者就緒 |
| 15:09:28 | round 1 predict（`prior`）+ 派工（payload 含 `<state>`） |
| 15:09:43 / 15:10:14 | agent `tasks_claim` / `tasks_complete`（真的寫出 report.md + summary.json） |
| 15:10:28 | B3 Skip（無證據） |
| 15:10:47 | 判官退回 → revising；A3 settle → unobservable(round 1) |
| 15:11:11 → 15:13:29 | round 2 同上，unobservable(round 2) |
| 15:13:58 起 | 啟用測試用歸屬修正器，證據開始可見 |
| 15:16:25 | **round 3 settle：0.925 / critical / mcp_only** → 寫 transition + 誘導 task-rule |
| 15:16:46 | round 4 派工，payload 帶 `## 任務經驗規則` |
| 15:18:25 | **round 4 settle：0.175 / negligible** → 判官通過 → task done；注入規則 helpful 1→2 |
| 15:20 起 | 對照組（enabled=false）一輪，三張表零新增 |

---

## 4. 狀態總結

| 驗證點 | 狀態 |
|---|---|
| (a) task_prediction_log / fidelity / predicted-observed JSON | ✅ PASS |
| (b) predict+settle hook 有跑、無 panic、`<state>` 進 payload | ✅ PASS |
| (c) Significant+ → task-rule 誘導 → 下一輪注入 → 規則結算 | ✅ PASS（需先繞過 BUG-1） |
| (d) grounding 前置檢查行為與工具使用相符 | ✅ PASS（現況恆為 Skip/Degraded，原因已釐清） |
| (e) enabled=false 對照組零新增、行為正常 | ✅ PASS |
| **BUG-1：goal-loop 工具稽核歸屬錯誤** | ❌ **未修，P0** |

**整體：PARTIAL。** 設計文件 §4 描述的掛鉤點、§3 的 diff 演算法、§2.4 的統計桶成長、§6.5 的 A4 induce/inject/prune、B2 transition 寫入、以及 §7.3 的 default-off 逐位元組不變性，**全部在真實 gateway + 真實 Claude CLI 上驗證通過**。但在 BUG-1 修好之前，A3/A4/B2 在生產 goal loop 上不會產生任何樣本（永遠 fidelity=None），且會連帶讓判官系統性誤判誠實的工作為「無證據」。

## 5. 欠帳 / 建議下一步

1. **修 BUG-1**（P0）。修完至少要重跑本報告的 (a)(c)(d) 三點；建議把「goal-loop 一輪後 `tool_calls.jsonl` 的 `agent_id` 必須等於 worker」做成一條 `duduclaw eval` 或整合測試，避免回歸。
2. **掃同類**：所有以 `SYSTEM_SENDERS` 身分發起的派工路徑（cron / heartbeat / autopilot / proactive gate）都會踩同一條 `mcp.rs:10102`，其工具稽核歸屬同樣會被記在系統發起者頭上。本次只實測了 goal-loop 一條。
3. **R1 樣本飢餓風險已可量化**：本次 4 輪只產出 2 個結算樣本、`n_samples=1`，`MIN_SAMPLES=3` 的統計階仍未達標（全程走 `Prior`）。設計文件 §9 R1 提的緩解（把 dispatcher 子代理路徑也納入觀測）值得在修完 BUG-1、拿到真實產出速率後重新評估。
4. **B3 的牙齒**：goal-loop 路徑上唯一可見的工具是 self-echo 的 task_board 工具，B3 恆為 Degraded。要讓它真的擋住假宣稱，需要 `Full` fidelity（原生工具證據）落地。

---

# 6. 修後複驗（2026-08-06 晚）

修法：`crates/duduclaw-cli/src/mcp.rs` 新增 `resolve_audit_agent()` —— `DUDUCLAW_DELEGATION_SENDER`
若是 `duduclaw_core::is_system_sender()`（goal-loop-driver / cron / heartbeat / autopilot / dashboard /
webhook 六個保留 id）就退回 `default_agent`（= 執行者 `DUDUCLAW_AGENT_ID`）；真人-agent 對 agent 的
delegation 不受影響。同時 WP-A4 落地：`claude_runner.rs` 的 dispatcher-side stream-json 迴圈新增原生工具
收集器，經 `runtime::NATIVE_TOOL_COLLECTOR` task-local → `dispatcher.rs` 橋接 →
`task_observe::record_native_evidence(task_id, round)` → `dispatch_engine.rs:1294`
`take_native_evidence` 餵進 `observe_round`。

## 6.0 複驗設定

- **全新乾淨隔離 home**：`/tmp/duduclaw-live-test-home2`（避免舊 `memory.db` 的規則讓 B1 novelty gate
  誤擋新誘導）；埠 **18998**（再次避開生產 18789 與前次 18999）；零頻道 token；未註冊 launchd。
- **未使用任何測試改寫器**（`pgrep -f fix_attrib` → 無）。上一輪的 `fix_attrib.py` 全程未啟動。
- binary：`cargo build --release -p duduclaw-cli`（7m55s，exit 0）→ `target/release/duduclaw`（Aug 6 23:58）。
- 任務同上一輪（`workspace/` 三檔 → 寫 `report.md`，outcome spec `files:workspace/report.md,workspace/summary.json`）。
- 生產 `DuDuClaw.app`（PID 52406）全程在跑未受影響；收尾後 `grep -a -c wp-a10` 掃
  `~/.duduclaw/{tasks.db,prediction.db,tool_calls.jsonl}` 全部 **0**。測試 gateway 已 kill（SIGTERM）。

## 6.1 (a) `tool_calls.jsonl` 歸屬 — ✅ PASS

無改寫器，一手全文（該次跑共 4 筆，全部落在執行者身上）：

```
2026-08-06T15:59:41.756114+00:00 | tester | tasks_claim
2026-08-06T16:00:27.469504+00:00 | tester | tasks_complete
2026-08-06T16:01:26.640089+00:00 | tester | tasks_claim
2026-08-06T16:02:17.553977+00:00 | tester | tasks_complete
```

零筆 `goal-loop-driver`。BUG-1 **修復確認**。

## 6.2 (b) `task_prediction_log` fidelity — ✅ PASS，且達 `full`

```
round|settled_at                       |composite_error|category   |fidelity
1    |2026-08-06T16:01:02.125449+00:00 |0.775          |significant|full
2    |2026-08-06T16:03:34.995239+00:00 |0.775          |significant|full
3    |(accept 那輪)                     |0.335          |moderate   |full
```

三輪全部 `full`（不再是 `mcp_only`，更不再是上一輪的全 `None`）。round 1 觀測 JSON 原文：

```json
{
  "task_id": "wp-a10-postfix-1", "round": 1, "agent_id": "tester",
  "observed_tool_classes": ["read","write","exec","task_board","other"],
  "observed_calls": 9, "observed_errors": 0,
  "observed_outcome": "rejected", "observed_artifact": "file_write",
  "fidelity": "full", "runtime": "claude"
}
```

對照上一輪（改寫器輔助下）只看得到 `{"task_board":1}` / `observed_calls: 2` —— 原生收集器確實補上了
`read` / `write` / `exec`，呼叫量從 2 升到 9~11。統計桶也因此長出有意義的分佈：

```
tester|research_or_qa|first|1  n=1  {read:1,write:1,exec:1,task_board:1,other:1} calls=[9]
tester|research_or_qa|retry|1  n=2  {read:2,write:2,exec:2,task_board:2,other:2} calls=[10,11]
tester|research_or_qa (marginal) n=3 {read:3,write:3,exec:3,task_board:3,other:3} calls=[9,10,11]
```

**marginal 桶 `n_samples=3` 已達 `MIN_SAMPLES`** —— 下一次同 goal_kind 的預測就會從 `Prior` 升到
`Marginal`（上一輪三輪跑完仍停在 n=1）。§9 R1 的樣本飢餓風險因原生收集器而明顯緩解。

全程 `grep -ci panic` → **0**。

## 6.3 (c) grounding 前置檢查 — ⚠️ PARTIAL（不再永遠 Skip，但改成永遠 Degraded）

三輪一手 log：

```
16:00:40 DEBUG dispatch_engine: B3 grounding 前置檢查 degrade（跳過，交由判官） task=wp-a10-postfix-1 reason="tool evidence lacks captured result_text"
16:02:32 DEBUG dispatch_engine: B3 grounding 前置檢查 degrade（跳過，交由判官） task=wp-a10-postfix-1 reason="tool evidence lacks captured result_text"
16:05:05 DEBUG dispatch_engine: B3 grounding 前置檢查 degrade（跳過，交由判官） task=wp-a10-postfix-1 reason="tool evidence lacks captured result_text"
```

- **改善確認**：不再是修前的 `Skip{"no tool_use in claim→review window"}`（那是「完全看不到證據」）。現在
  B3 看得到紀錄了，走的是 `ResultTextMissing` 分支。
- **與該輪工具使用對照**：相符但受限。B3 讀的仍是 `read_tool_activity_records()` 的 MCP 稽核集合
  （`dispatch_engine.rs:1100` 傳的是 `records`），該窗口內只有 `tasks_claim` / `tasks_complete`，兩者都在
  `SELF_ECHO_TOOL_NAMES` 上、`result_text` 被 Fix-2 C1a 刻意抑制 ⇒ 必然 `Degraded`（fail-open，設計正確）。
- **殘留缺口**：原生工具證據（本輪 9~11 筆，含 Read/Write/Bash）**沒有**餵給 B3。所以 B3 在 goal-loop
  路徑上依然永遠無法達到 `Grounded` 或 `Reject` —— 它仍然沒有牙齒，只是換了個沒牙的姿勢。

## 6.4 (d) 判官拿得到 `<tool_activity>` — ⚠️ PARTIAL

**改善確認**：修前判官的退回理由是「**there is no `<tool_activity>` block**」；修後變成
「**tool_activity contains only task completion status (1 ok)**」。也就是說區塊確實產生了、確實送進判官
prompt 了 —— 「零證據」的框架已消失。

**但問題沒真的解決**。round 1 判官原文（`tasks.judge_feedback`）：

> [correctness] Worker claims files were created (workspace/report.md and workspace/summary.json), but
> tool_activity contains only task completion status (1 ok) with **no evidence of actual file operations
> (Read, Write, Bash)**. Per review protocol: actions claimed but absent from tool_activity are UNVERIFIED.

round 2 判官原文（zh-TW）：

> …但 tool_activity 中僅顯示 tasks_complete: 1 ok，不見 Read 或 Write 工具的具體執行記錄。…
> summary.json 通過 deterministic_check，但 report.md 的具體內容無法確認。

而檔案在 round 1 就已經正確產出：

```
-rw-r--r--  157 Aug  7 00:00 report.md
-rw-r--r--  201 Aug  7 00:00 summary.json
```

結果：**正確的工作被退回兩輪，第 3 輪才通過**（`goal-mode 驗收通過 → done`，`revision_round=2`）。
不是死鎖（迴圈仍會收斂），但每個誠實完成的 goal 任務都要多燒 2 輪 agent + 2 次判官 LLM 呼叫。

### BUG-2（P1，新發現）：原生工具證據沒有餵給判官與 B3，只餵給 A3

`dispatcher.rs` 橋過來的原生證據，唯一消費者是 `dispatch_engine.rs:1294` 的
`take_native_evidence(...)` → `settle_forward_model`（A3 觀測）。同一個 review pass 裡的另外兩個消費者
仍只吃 MCP 稽核：

```rust
// dispatch_engine.rs:1100  ← B3 grounding 前置檢查
match grounding_precheck(&result, &records, grounding_config) {
// dispatch_engine.rs:1140  ← 判官 prompt 的 <tool_activity> 區塊
if let Some(block) = format_tool_activity(&records) {
```

`records` = `read_tool_activity_records(home, agent_id, since, now)`，純 MCP 稽核。這是 BUG-1 的同源
遺留：BUG-1 修好了「誰做的」，WP-A4 補上了「做了什麼」，但「做了什麼」只接到 A3 這一條線。

**未修（依紅線只記錄不回報）。** 建議：把 `take_native_evidence` 的呼叫上移到 review pass 開頭
（B3 與 `format_tool_activity` 之前），三個消費者共用同一份合併證據；注意 `take_*` 是**移除**語意，
上移後 `settle_forward_model` 要改吃傳入的引用而不是再 take 一次，否則 A3 會拿到 `None` 而從 `full`
退回 `mcp_only`（這正是必須一起改、不能只改一半的原因）。另外 `format_tool_activity` 需要能吃
`NativeToolEvent`（目前簽名只吃 `ToolActivityRecord`）。

## 6.5 A4 規則迴路自然發生 — ✅ PASS（無改寫器，一次完整生命週期）

| 階段 | 一手證據 |
|---|---|
| **induce**（round 1，significant 0.775） | `memory.db` semantic：`agent tester 在 goal_kind research_or_qa(phase first)的任務中:預期使用工具類別「search」但本輪未使用;額外使用了預期外的工具類別「write、exec、taskboard、other」;預期結果為「accept」但實際為「rejected」;預期產出形態為「text_only」但實際為「file_write」。` tags=`["task-rule","probation-rule","goal_kind:research_or_qa"]` |
| **B2 transition** | episodic：`task wp-a10-postfix-1 round 1 (tester\|research_or_qa\|first\|1): 9 tool call(s), outcome=rejected, composite_error=0.77` |
| **inject**（round 2 payload 第 24-25 行） | `## 任務經驗規則` + 上述規則全文 |
| **settle**（round 2，significant ⇒ harmful+1） | metadata `{"rule_stats":{"harmful":1,"helpful":1},"source_round":1,"source_task_id":"wp-a10-postfix-1"}` |
| **retire**（Janus probation：首次 harmful 即退休） | tags 追加 `"retired-rule"` |
| **retire 生效** | round 3 payload `grep -c 任務經驗規則` → **0**（已退休規則不再注入） |

比上一輪更完整：上一輪只驗到 induce→inject→helpful+1，這次連 **harmful → retire → 停止注入** 的另一半
生命週期都自然跑完。而且規則內容因為原生收集器而具體得多（能講出 `write、exec` 這些原本看不見的類別）。

## 6.6 修後複驗結論

| 驗證點 | 修前 | 修後 |
|---|---|---|
| (a) `tool_calls.jsonl` agent_id | ❌ 恆為 `goal-loop-driver` | ✅ `tester`（4/4 筆） |
| (b) A3 fidelity | ❌ 恆 `None`（永不結算） | ✅ `full`（3/3 輪），tool classes 含 read/write/exec |
| (c) B3 grounding | ❌ 恆 `Skip`（看不到證據） | ⚠️ 恆 `Degraded`（看得到 MCP 證據，但拿不到原生證據 ⇒ 仍無牙齒） |
| (d) 判官 `<tool_activity>` | ❌ 區塊根本不存在 | ⚠️ 區塊存在但只含 MCP 工具 ⇒ 誠實工作仍被退 2 輪才過 |
| A4 規則迴路 | 需改寫器繞道才觸發 | ✅ 無繞道自然跑完 induce→inject→settle→retire |
| 樣本成長 | n=1，永遠 `Prior` | ✅ marginal 桶 n=3，達 `MIN_SAMPLES` |
| panic / ERROR | 0 | 0 |

**修後整體：PARTIAL。** BUG-1 確認修復、WP-A4 原生收集器確認生效、A3/A4/B2 全迴路在真實環境無繞道
自然完成 —— 主線目標達成。剩下的 BUG-2 不影響 A3/A4 的正確性（A3 已拿到 full 證據），但會讓判官與 B3
繼續用殘缺證據做判斷，代價是每個誠實完成的 goal 任務多燒約 2 輪。建議與 BUG-1 同批修掉。

## 6.7 修後欠帳

1. **修 BUG-2**（P1）：把原生證據一併餵給 `grounding_precheck` 與 `format_tool_activity`；注意
   `take_native_evidence` 的移除語意（見 §6.4 的改法警告）。
2. **BUG-1 的同類掃描已由修法本身覆蓋**：`resolve_audit_agent` 收斂在單一讀取點，六個
   `SYSTEM_SENDERS`（cron / heartbeat / autopilot / dashboard / webhook）自動受惠。但本次只實測了
   goal-loop 一條，cron / autopilot 路徑建議各補一次活測或整合測試。
3. **回歸護欄**：建議把「goal-loop 一輪後 `tool_calls.jsonl` 的 `agent_id` == worker」與
   「`task_prediction_log.fidelity == full`」做成整合測試或 `duduclaw eval` 斷言，兩者都是這次靠人工
   活測才發現的沉默失敗類型。
