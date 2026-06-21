# 持久性框架

> 持久性的五根支柱——讓抖動的網路、暫停的服務、或執行到一半的崩潰，永遠不會悄悄遺失一筆操作。

---

## 比喻：包裹物流網路

把每一筆關鍵寫入——一次 wiki 編輯、一次記憶儲存、一次 MCP 呼叫、一則對外訊息——都想成交給快遞的一件包裹。

1. **追蹤號碼**避免同一件包裹被寄出兩次。在轉運站掃一下；若系統裡已經有了，就不會再派一台車去送。
2. **重新投遞**處理收件人不在家的情況。快遞等一會兒、回來、再等久一點、再回來——間隔錯開，整支車隊才不會同一分鐘全擠在同一條街。
3. **故障路線的斷路器**在整個區域無法到達時啟動。與其一台接一台把車送進淹水的路，調度直接停掉這條路線，等待，再派*一台*偵察車測試，確認後才重新開放。
4. **退件倉庫**收容那些嘗試過所有投遞後仍真的送不到的包裹——所以它們永遠不會遺失，事後可由人工檢視並重新寄送。
5. **配送清單**記錄一條多站長途路線跑到了哪裡，如此一來若卡車中途拋錨，下一位司機從最後完成的站點接續，而不是從頭來過。

DuDuClaw 的 `duduclaw-durability` crate 正是這張物流網路——五根可組合的支柱，讓操作在網路抖動、服務暫停與程序崩潰之間保持可靠。

---

## 五根支柱

| 支柱 | 模組 | 保證 |
|------|------|------|
| **Idempotency（冪等）** | `idempotency.rs` | 同一操作在 dedup 視窗內最多執行一次——重複者回傳原始結果 |
| **Retry（重試）** | `retry.rs` | 暫時性失敗以指數退避 + jitter 重新嘗試，採 per-operation 策略 |
| **Circuit Breaker（斷路器）** | `circuit_breaker.rs` | 失敗的依賴在拖垮系統前先被隔離（Open），再探測恢復 |
| **Checkpoint（檢查點）** | `checkpoint.rs` | 長任務進度被快照，崩潰後從最後階段接續，而非從零開始 |
| **DLQ（死信佇列）** | `dlq.rs` | 耗盡所有重試的操作被隔離、絕不遺失，且可回放 |

五個介面皆由 `duduclaw_durability::prelude` re-export，組裝成單一的 `DurabilityLayer`。

---

## 支柱 1 — Idempotency：追蹤號碼

每筆操作取得一個確定性的鍵：

```
{agent_id}:{operation_type}:{content_hash}
```

content hash 是 SHA-256 的前 32 個 hex 字元（128-bit 碰撞空間）。`check_and_record` 在單一寫鎖內完成原子的 compare-and-set——封堵兩個並發呼叫都判定為「New」的 TOCTOU 競態：

```
check_and_record(key, placeholder)
     |
     v
取得寫鎖
     |
     +-- key 已在 dedup 視窗內？ ──► CheckResult::Duplicate { original_result, ... }
     |
     +-- key 是新的 ──► 插入 placeholder ──► CheckResult::New
     |                       |
     v                       v
釋放鎖             呼叫者執行操作，再 record(key, real_result)
```

`IdempotencyConfig` 預設值：`dedup_window_seconds = 3600`、`max_key_length = 256`、`cleanup_interval_seconds = 7200`。記憶體內實作可在不更動介面下替換為 Redis / SQLite backend。

---

## 支柱 2 — Retry：重新投遞

`RetryEngine` 為每個 `operation_type` 持有一份 `RetryPolicy`。延遲呈指數成長，再以 jitter 擾動，讓同時發生的失敗不會同步成一場重試風暴：

```
delay = initial_delay_ms * multiplier^attempt   （上限 max_delay_ms）
        + 介於 [0, delay 的 30%) 的隨機 jitter
```

```
attempt 0   ├─ 500ms ─────┤ 重試
attempt 1   ├─ 1000ms ─────────┤ 重試
attempt 2   ├─ 2000ms ──────────────────┤ 重試
            （每一段還會額外加上最多 +30% 的隨機 jitter）
            └─ 耗盡 ──► 送入 DLQ
```

錯誤會被分類：`non_retryable_errors`（例如 `PERMISSION_DENIED`、`INVALID_SCHEMA`）優先且立即停止；`retryable_errors`（例如 `NETWORK_TIMEOUT`、`SERVICE_UNAVAILABLE`、`RATE_LIMITED`）則重新嘗試。預設策略涵蓋 `mcp_call`、`memory_write`、`wiki_write`、`message_send` 與 `external_api`。`RetryOutcome` 回報 `success`、`attempts`、`final_error` 與 `sent_to_dlq`——不可重試的失敗永不進 DLQ；耗盡的重試則會。

```toml
[retry.mcp_call]
max_attempts    = 3
initial_delay_ms = 500
max_delay_ms    = 10000
multiplier      = 2.0
jitter          = true
retryable_errors     = ["NETWORK_TIMEOUT", "SERVICE_UNAVAILABLE", "RATE_LIMITED"]
non_retryable_errors = ["PERMISSION_DENIED", "INVALID_SCHEMA", "NOT_FOUND"]
```

---

## 支柱 3 — Circuit Breaker：關閉故障路線

每個依賴有自己獨立的三狀態斷路器。斷路器在重試把情況弄得更糟之前先隔離失敗的依賴：

```
        failure_rate > threshold
          （達 min_request_count 之上）
 CLOSED ───────────────────────────►  OPEN
   ▲                                    │
   │ probe_success_required 次成功      │ reset_timeout 到期
   │                                    ▼
   └──────────  HALF_OPEN  ◄────────────┘
                   │
                   │ 探測失敗
                   └──────────────────►  OPEN
```

- **Closed** — 所有請求通過；以最近 `window_size` 筆結果的滑動視窗追蹤失敗率。
- **Open** — 在 `reset_timeout_seconds` 到期前，請求一律以 `CircuitOpen` 拒絕。
- **HalfOpen** — 放行有限數量的探測。`probe_inflight` 計數把並發探測上限壓在 `probe_success_required`，如此一來多個探測同時成功也不會過度計數而過早關閉斷路器。

一個細膩的防護：若探測的結果從未被回報（panic / cancel / future 被 drop），`probe_started_at` 加上 `probe_timeout_seconds` 讓斷路器**重新武裝**被放棄的 slot——否則它會永遠卡在 Open。沒有 inflight 探測時的零星 `after_call` 會被完全忽略。

每次轉換都會觸發 `StateTransition` callback 供稽核軌跡使用。預設值涵蓋 `memory_service`（閾值 0.5）、`external_mcp_client`（0.3）與 `wiki_service`（0.4）。

---

## 支柱 4 — Checkpoint：配送清單

長時間執行的任務快照其進度，讓崩潰後從最後階段接續：

```
save("task-123", "agent-1", "phase-2", state_json, ttl=3600)
     |
     v
   [崩潰 / 重啟]
     |
     v
restore("task-123", "agent-1") ──► Some(Checkpoint { phase: "phase-2", state, ... })
     |
     v
   從 phase-2 接續（而非從零）
```

每個 `Checkpoint` 帶有 `checkpoint_id`、`task_id`、`agent_id`、`phase`、任意 JSON `state`、`created_at` / `expires_at`，以及用於血緣關係的 `parent_checkpoint_id`——使「從 checkpoint X 探索另一種做法」的對話狀態分支成為可能（RFC-26 §4.2）。`CheckpointConfig` 預設值：24h TTL、`max_checkpoints = 10_000`、每小時清理。

---

## 支柱 5 — DLQ：退件倉庫

重試耗盡時，操作不會被丟棄——而是隔離在死信佇列中：

```
重試耗盡
     |
     v
DLQ.enqueue(agent_id, operation_type, original_operation, retry_count, last_error, ttl)
     |
     v
DlqRecord { status: Pending }
     |
     +──► 回放 ──► status: Replayed
     |
     +──► 放棄 ──► status: Abandoned
     |
     +──► TTL 到期 ──► 清除
```

每個 `DlqRecord` 保留完整的 `original_operation` JSON、agent、操作類型、重試次數、最後錯誤與時間戳——足夠讓人工（或自動化任務）理解並重新寄送失敗的工作。狀態在 `Pending → Replayed` / `Abandoned` 之間移動。

---

## 支柱如何組合

單一筆持久性寫入會貫穿全部五者：

```
進來的操作
     |
     v
1. Idempotency.check_and_record ── 重複？ ──► 回傳 original_result
     | New
     v
2. CircuitBreaker.before_call ── Open？ ──► 拒絕（快速失敗）
     | 允許
     v
3. RetryEngine.execute(op) ── 暫時性失敗？ ──► 退避 + jitter，重試
     |                                                |
     | 成功                                           | 耗盡
     v                                                v
   CircuitBreaker.after_call(true)            5. DLQ.enqueue
   Idempotency.record(real_result)               CircuitBreaker.after_call(false)
   4. Checkpoint.save(next_phase)
```

這正是 gateway 的 LLM fallback 路徑與 durable cron jobs 所運行的鏈條——同樣的五項保證同時護衛自動化排程與主要推論的後備。

---

## 為什麼這很重要

### 不會悄悄遺失

DLQ 保證一筆耗盡重試的寫入會被捕捉，而非丟棄。搭配檢查點，任務中途崩潰是可恢復的，而非災難性的。

### 不會重複副作用

被重試或重新投遞的訊息，原本可能貼出兩次、儲存兩次記憶，或對外部 API 重複計費。冪等鍵——確定性、以內容定址——讓「視窗內最多一次」成為硬保證，即使在並發呼叫下亦然。

### 不會連鎖失敗

沒有斷路器，一個死掉的 `memory_service` 會吸收每個 agent 的每次重試，直到整個 gateway 停擺。斷路器提早跳脫，以 `CircuitOpen` 快速失敗，並依自己的節奏探測恢復。

### Jitter 防止驚群效應

當許多 agent 在同一瞬間失敗，確定性退避會讓它們全在同一毫秒重試。真正的隨機 jitter（最多 30%）把負載分散開。

### 可配置、無 magic number

每個閾值——重試次數、延遲、失敗率、逾時、TTL——都是帶有已驗證預設值的配置欄位。策略可透過 `upsert_policy` / `upsert_config` 熱重載。

---

## 總結

包裹網路不承諾沒有任何一台車會拋錨——它承諾一台拋錨的車絕不會弄丟你的包裹。DuDuClaw 的持久性框架對每一筆關鍵寫入做出同樣的承諾：被追蹤所以不會重複、被重試所以一次小故障不會殺死它、有斷路器守護所以一個死掉的依賴不會拖垮系統、有檢查點所以崩潰能接續、有 DLQ 撐底所以任何真正失敗的工作永不被悄悄遺失。
