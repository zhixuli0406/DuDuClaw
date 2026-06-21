# Evolution Events 系統

> Agent 的飛航資料記錄器——每一次有意義的演化、治理與耐久性事件，都以保證送達的批次機制完整記錄。

---

## 比喻：飛航資料記錄器

飛機上的黑盒子並不負責駕駛飛機。它安靜地待在機尾，把每一個有意義的事件——高度變化、引擎狀態、操縱輸入、警報——記錄到即使墜機也能倖存的一次性寫入媒體上。飛行員飛行途中從不去讀它。但一旦出事，這個記錄器是唯一能說清楚究竟發生了什麼、以什麼順序、為什麼發生的真相來源。

Evolution Events 系統就是 DuDuClaw 為 Agent 打造的飛航資料記錄器。它本身不*驅動*演化——驅動演化的是 GVU 迴圈、預測引擎、治理層與耐久性框架。它只是安靜地記錄這些子系統發出的每一個有意義事件：一個技能被啟用、一次安全掃描執行、一條政策被違反、一個斷路器跳閘、一次重試耗盡。每一筆記錄都落入一個只能追加（append-only）的 JSONL 紀錄檔，採用固定的 8 欄位 schema，而且永遠不會阻塞發出事件的那個 Agent。

當營運者數週後打開儀表板的 **Reliability**（可靠性）頁面，想問「為什麼這個 Agent 的成功率下降了？」，答案就來自這個記錄器——不靠記憶，也不靠猜測。

---

## 單一筆記錄的形狀

每一個事件——無論由哪個子系統發出——都序列化成一行 JSONL，欄位完全相同。固定寬度的 schema（沒有缺漏的鍵；缺值序列化為 `null`）讓下游解析器保持簡單。

```
AuditEvent {
  timestamp:      "2026-06-21T07:14:02Z"     ← RFC3339，UTC
  event_type:     "gvu_generation"           ← 哪個子系統 / 動作
  agent_id:       "duduclaw-pm"              ← 發生在誰身上
  skill_id:       "python-patterns" | null   ← 涉及的技能（若有）
  generation:     3 | null                    ← GVU 世代計數
  outcome:        "success"                   ← success / failure / suppressed / ...
  trigger_signal: "prediction_error" | null   ← 上游觸發原因
  metadata:       { ... }                     ← 小型（<1 KB）結構化診斷資料
}
```

這個 schema 是**只追加且向後相容**的：P0 變體永遠不會被改名或移除，而之後的每一次擴充都是純粹增量。為第一版寫的解析器，至今仍能讀取最新的紀錄檔。

---

## 事件分類

`AuditEventType` 列舉（位於 `schema.rs`）橫跨三個領域。最初的 P0 集合涵蓋演化；W19-P1 擴充加入了治理（Governance）與耐久性（Durability）。

| 領域 | 事件型別 | 記錄什麼 |
|------|----------|----------|
| **Evolution（P0）** | `skill_activate`、`skill_deactivate`、`security_scan`、`gvu_generation`、`signal_suppressed`、`skill_graduate` | 技能開啟/關閉、安全審查、GVU 自我對弈循環、停滯抑制、跨 Agent 晉升 |
| **Governance（W19-P1）** | `governance_violation`、`governance_approval_requested`、`governance_approval_decided`、`governance_policy_changed`、`governance_quota_reset` | 政策違規、核准工作流程、政策 CRUD、每日配額重置 |
| **Durability（W19-P1）** | `durability_retry_attempt`、`durability_retry_exhausted`、`durability_circuit_opened`、`durability_circuit_recovered`、`durability_checkpoint_saved`、`durability_dlq_replayed` | 重試嘗試與耗盡、斷路器狀態轉換、檢查點儲存、DLQ 重播 |

`Outcome` 列舉同樣分層：P0 有 `success` / `failure` / `suppressed`；W19-P1 加入 `blocked`、`warned`、`throttled`、`pending`、`approved`、`rejected`、`triggered`、`recovered`——因此一個 `governance_violation` 可以是 `blocked`、一個核准可以是 `pending`、一個 `durability_circuit_opened` 可以是 `triggered`。

---

## 四個模組

整個系統由 `gateway/evolution_events/` 底下四個職責明確的模組構成。

| 模組 | 職責 |
|------|------|
| `schema.rs` | 定義 `AuditEvent`（8 欄位記錄）、`AuditEventType`（橫跨 3 領域的 17 個變體）與 `Outcome`。是傳輸格式的唯一真相來源。 |
| `emitter.rs` | 非阻塞的入口。`EvolutionEventEmitter` 提供型別化的輔助方法（`emit_skill_activate`、`emit_gvu_generation`……）；每次 emit 都會 spawn 一個分離的 Tokio 任務，因此呼叫端永遠不會被 I/O 阻塞。一個行程級的全域單例供無法把 emitter 一路傳遞下去的呼叫點使用。 |
| `query.rs` | 讀取路徑。`AuditEventIndex` 是建立在 JSONL 檔案之上、以 SQLite 為後端的索引快取；`AuditQueryFilter` / `AuditQueryResult` 支援分頁、過濾的讀取。 |
| `reliability.rs` | 分析層。純函式把原始事件依時間窗口彙整成每個 Agent 的 `ReliabilitySummary`。 |

（第五個檔案 `logger.rs` 是 emitter 寫入時所經過的 JSONL 追加器——以日期 + 10 MB 大小做輪替、寫入錯誤時重試一次、並在持久化前做 metadata 遮蔽。）

---

## 寫入路徑：Emit → 批次 → 儲存

記錄器的首要任務是絕不拖慢 Agent。寫入路徑從頭到尾都是非同步的。

```
GVU 迴圈 / 治理 / 耐久性子系統
         |
         | emitter.emit_gvu_generation(agent, gen, outcome, ...)
         v
EvolutionEventEmitter        ← 立即返回
         |
         | tokio::spawn（分離）——呼叫端永遠不會阻塞在 I/O 上
         v
EvolutionEventLogger.log(event)
         |
         | 遮蔽敏感 metadata（例如 last_error → [REDACTED]）
         v
追加一行 JSON 到 events/YYYY-MM-DD.jsonl
         |
         +── 日期變了嗎？        → 輪替到新的日期檔
         +── 檔案 ≥ 10 MB？      → 輪替到 YYYY-MM-DD-{seq}.jsonl
         |
         v
flush() 時 fsync 以確保持久性
```

若寫入失敗（輪替過程出問題、暫時性 FS 錯誤），logger 會作廢過期的檔案 handle、重新開啟，並在丟棄記錄前**重試一次**——稽核事件應該能撐過短暫的小故障，而不是憑空消失。

---

## 讀取路徑：儲存 → 查詢 → Reliability 頁面

JSONL 非常適合追加，但做過濾查詢很慢。因此讀取路徑會把紀錄檔索引進 SQLite，再做彙整。

```
events/*.jsonl  ──（背景同步）──>  AuditEventIndex (SQLite)
                                              |
                  ┌───────────────────────────┼───────────────────────────┐
                  v                            v                           v
        audit.evolution_query        audit.reliability_summary    /api/reliability/summary
        （過濾、分頁）                 （彙整後的指標）              （HTTP 端點）
                  |                            |                           |
                  └────────────────┬──────────┴───────────────────────────┘
                                   v
                          Web ReliabilityPage.tsx
                   consistency · task success · skill adoption · fallback rate
```

`AuditEventIndex` 只開啟一次，並以 `Arc` 共享、由背景任務保持同步，因此 RPC handler 與 `/api/reliability/summary` HTTP 端點共用同一條連線，不必各自重新掃描 JSONL。

### 查詢安全

`query.rs` 對濫用做了強化：

- **欄位白名單**——每一個可過濾的欄位都必須出現在 `ALLOWED_FILTER_COLS` 中；不在清單內的欄位名稱會被拒絕，封堵任何未來的 SQL 注入向量。
- **夾限的分頁**——`limit` 被夾限在 `[1, MAX_LIMIT]`，`offset` 也有上界，因此巨大的 OFFSET 無法讓 SQLite 掃描無上限的列數（一道 DoS 防護）。

---

## Reliability Summary（可靠性摘要）

`reliability.rs` 把原始事件流轉化成營運者可讀的健康指標。所有比率欄位都落在 `[0.0, 1.0]`；當窗口內沒有任何事件時，偏向成功的指標回傳保守中性的 `1.0`，而採用率/降級率則回傳 `0.0`。

```
ReliabilitySummary（每個 Agent，預設 7 天窗口）
├─ consistency_score      = 各 event_type 之 (success / total) 的平均
├─ task_success_rate      = success 事件 / 全部事件
├─ skill_adoption_rate    = skill_activate 事件 / 全部事件
├─ fallback_trigger_rate  = llm_fallback 事件 / 全部事件
├─ total_events           = 窗口內計入的稽核列數
└─ generated_at           = 計算當下的 RFC3339 時間戳
```

每個指標都由一個小型**純函式**（`avg_success_rate`、`task_success_rate`、`skill_adoption_rate`、`fallback_trigger_rate`）以彙總計數算出——這讓它們可以用 fixture 輕鬆做單元測試，且完全不涉及任何 I/O。

---

## 可靠性保證

記錄器的設計確保「記錄」永遠不會傷害「被記錄者」，且記錄能撐過一般故障。

```
保證                            機制
─────────────────────────────  ────────────────────────────────────────
永不阻塞呼叫端                  每次 emit 一個分離的 tokio::spawn
撐過暫時性寫入失敗              作廢 handle → 重新開啟 → 重試一次
有界的檔案成長                  日期輪替 + 10 MB 大小輪替
不洩漏機密                      寫入 JSONL 前先遮蔽 metadata
flush 時持久化                  透過 flush() 做 fsync
向後相容的 schema              P0 變體永不改名；只做增量
查詢無法被武器化               欄位白名單 + 夾限的 limit/offset
固定寬度 JSON                  缺漏欄位序列化為 null
```

---

## 為什麼這很重要

### 觀測性而不耦合

每個子系統只發出一行就繼續做自己的事。GVU 迴圈不需要知道儀表板的存在；治理層不需要知道 SQLite 的存在。記錄器把*產生*事件與*消費*事件解耦，因此任何新子系統只要呼叫一個 emit 輔助方法就能開始記錄。

### 一份誠實的稽核軌跡

因為 schema 向後相容、紀錄檔只能追加，歷史無法被悄悄改寫。舊記錄的意義永遠等同於它被寫下時的意義——這對治理與安全審查至關重要。

### 給營運者答案，而非猜測

Reliability 頁面把成千上萬筆原始事件，化為營運者真正在意的四個數字：這個 Agent 是否一致、是否成功、是否採用技能、是否避免雲端降級？當某個數字變動時，底層事件就在那裡，可以鑽進去看。

### 在高負載與惡意攻擊下依然安全

非阻塞的 emit 讓熱路徑保持快速。輪替讓磁碟用量有界。查詢白名單與 offset 夾限，讓讀取路徑無法被轉化成 DoS 或注入的攻擊面。

---

## 總結

黑盒子並不負責駕駛飛機——但少了它，每一次事故都是一團謎。Evolution Events 系統對 DuDuClaw 的 Agent 扮演的正是這個角色：一個非阻塞的 emitter、一個帶有重試一次耐久性的輪替 JSONL 紀錄檔、一個以 SQLite 建索引的查詢層，以及一個呈現在儀表板上的可靠性彙整。演化、治理與耐久性全都流經同一個固定的 8 欄位 schema，因此一個 Agent 如何隨時間改變的故事，永遠被記錄、永遠可查詢、永遠能安全地讀取。
