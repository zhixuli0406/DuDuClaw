# Sprint N P0 T2 — 事件發射點盤點文件

> **撰寫者**：duduclaw-eng-agent  
> **日期**：2026-04-25  
> **任務**：[Sprint-N-P0-T2] Agent 事件發射層實作（5 種 event_type 埋點）Part 1  
> **狀態**：Part 1 完成 ✅ | Part 2 實作中 🔧

---

## 一、T1 基礎設施確認

> **T1 已完全實作**，可直接進入 Part 2。

| 模組 | 路徑 | 狀態 |
|------|------|------|
| Schema 型別定義 | `crates/duduclaw-gateway/src/evolution_events/schema.rs` | ✅ 完成（含測試） |
| JSONL Logger | `crates/duduclaw-gateway/src/evolution_events/logger.rs` | ✅ 完成（含並發測試） |
| 模組匯出 | `crates/duduclaw-gateway/src/evolution_events/mod.rs` | ✅ 完成 |

T1 已提供：
- `AuditEventType` enum（5 種 event_type 全部定義）
- `Outcome` enum（`success | failure | suppressed`）
- `AuditEvent` builder（fluent API）
- `EvolutionEventLogger::log()` — 非阻塞 JSONL 寫入
- 日輪替 + 10MB size-based 輪替
- 並發安全（Mutex 保護 + O_APPEND）

---

## 二、5 種 event_type 發射點盤點

### 2.1 `skill_activate`

| 屬性 | 值 |
|------|-----|
| **觸發條件** | Prediction error 診斷建議某 Skill 後，自動啟用 |
| **主要觸發函式** | `SkillActivationController::activate()` |
| **觸發檔案** | `crates/duduclaw-gateway/src/skill_lifecycle/activation.rs:47` |
| **呼叫位置** | `crates/duduclaw-gateway/src/channel_reply.rs:1410` |
| **上下文** | Prediction 背景 Task，Skill lifecycle block（Section 5） |
| **trigger_signal** | `"prediction_error_diagnosis"` |
| **skill_id** | 從 `diagnosis.suggested_skills` 取得 |
| **埋點策略** | 在 `ctrl.activate()` 呼叫後立即 emit（非阻塞） |

**發射點程式碼位置（channel_reply.rs）：**
```
L1407-1411:
for skill_name in &diagnosis.suggested_skills {
    ctrl.activate(&agent_id_for_pred, skill_name, error.composite_error);
    // ← 埋點位置（emit_skill_activate 在此之後）
}
```

---

### 2.2 `skill_deactivate`

此 event 有 **3 個觸發路徑**，全部需要埋點：

#### 路徑 A：定期有效性評估（每 20 次對話）
| 屬性 | 值 |
|------|-----|
| **觸發條件** | `pe.metacognition.total_predictions % 20 == 0` 且 Skill 效能不佳（error 無改善） |
| **觸發函式** | `SkillActivationController::evaluate_all()` |
| **觸發位置** | `channel_reply.rs:1501-1507` |
| **trigger_signal** | `"effectiveness_evaluation"` |

**埋點位置：**
```
L1504-1507:
let deactivated = { ctrl.evaluate_all(&agent_id_for_pred) };
for name in &deactivated {
    // ← 埋點位置（emit_skill_deactivate 在此）
    info!(...);
}
```

#### 路徑 B：沙箱試驗 DISCARD 決策
| 屬性 | 值 |
|------|-----|
| **觸發條件** | Sandbox trial 評估 → `TrialDecision::Discard` |
| **觸發位置** | `channel_reply.rs:1580-1582` |
| **trigger_signal** | `"sandbox_trial_discard"` |

**埋點位置：**
```
L1577-1582:
TrialDecision::Discard => {
    ...
    ctrl.deactivate(&agent_id_for_pred, name);
    // ← 埋點位置（emit_skill_deactivate 在此）
}
```

#### 路徑 C：容量驅逐（activate() 內部）
| 屬性 | 值 |
|------|-----|
| **觸發條件** | `current_count >= max_active`，驅逐最差 Skill 騰出空間 |
| **觸發位置** | `activation.rs:53-56`（呼叫 `self.deactivate()`） |
| **trigger_signal** | `"capacity_eviction"` |
| **埋點策略** | 在 `activation.rs` 的 `activate()` 方法中注入 emitter 參數 **或** 在 `deactivate()` 後埋點 |
| **P0 決策** | 路徑 A、B 優先埋點；路徑 C 在 `deactivate()` 內部埋點（需傳入 trigger_signal 參數）|

> **注意**：路徑 C 的 `deactivate()` 也被路徑 A、B 呼叫，若在函式內部埋點須避免重複 emit。P0 策略：在各呼叫點（路徑 A、B）單獨埋點，`deactivate()` 函式本身不埋點。

---

### 2.3 `security_scan`

| 屬性 | 值 |
|------|-----|
| **觸發條件** | MCP 工具 `skill_security_scan` 被呼叫，掃描完成後 |
| **觸發函式** | `security_scanner::scan_skill()` |
| **觸發檔案** | `crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs:72` |
| **MCP Handler** | `crates/duduclaw-cli/src/mcp.rs:4809` → `handle_skill_security_scan()` |
| **呼叫位置** | `mcp.rs:4880` — `security_scanner::scan_skill(&content, must_not.as_deref())` |
| **trigger_signal** | `"skill_security_scan"` |
| **outcome** | `passed` → `Success`；`!passed` → `Failure` |
| **埋點策略** | 在 `mcp.rs:4880` 的 `scan_skill()` 回傳後立即 emit |

**發射點程式碼位置（mcp.rs）：**
```
L4879-4881:
use duduclaw_gateway::skill_lifecycle::security_scanner;
let result = security_scanner::scan_skill(&content, must_not.as_deref());
// ← 埋點位置（emit_security_scan 在此）
```

**注意事項**：MCP handler 無法透過 `ReplyContext` 取得 emitter。採用**進程全域 singleton**（`OnceLock<EvolutionEventEmitter>`）解決。

---

### 2.4 `gvu_generation`

| 屬性 | 值 |
|------|-----|
| **觸發條件** | Prediction error 超過閾值 → GVU 演化迴圈執行 |
| **GVU 入口** | `gvu.run_with_context()` at `channel_reply.rs:1713` |
| **outcome 映射** | `Applied` → `Success`；`Abandoned` → `Failure`；`Skipped/Deferred/TimedOut` → `Failure` |
| **trigger_signal** | `"gvu_trigger"` / `"epistemic_foraging"` / `"sycophancy_alert"`（依 context 決定，同 etype） |
| **generation 欄位** | P0 統一填 `null`（世代追蹤留 P1） |
| **埋點位置** | `channel_reply.rs:1726-1770` — GVU outcome match block |

**發射點程式碼位置（channel_reply.rs）：**
```
L1726-1770:
match outcome {
    GvuOutcome::Applied(ref version) => {
        // ← 埋點：outcome=Success, metadata={version_id}
    }
    GvuOutcome::Abandoned { .. } => {
        // ← 埋點：outcome=Failure, metadata={last_gradient.critique}
    }
    GvuOutcome::Skipped { .. } => {
        // ← 埋點：outcome=Failure, metadata={reason}
    }
    GvuOutcome::Deferred { .. } => {
        // ← 埋點：outcome=Failure, metadata={retry_count, retry_after_hours}
    }
    GvuOutcome::TimedOut { .. } => {
        // ← 埋點：outcome=Failure, metadata={elapsed_secs, generations_completed}
    }
}
```

---

### 2.5 `signal_suppressed` ⚠️ P0 Stub

| 屬性 | 值 |
|------|-----|
| **P0 實作策略** | Stub 實作 — 邏輯為空，但埋點位置確立 |
| **P1 完整邏輯** | 停滯偵測：`6h 內觸發 3 次` 閾值（`evolution_toggle.stagnation_detection`，見 T3） |
| **埋點位置** | `channel_reply.rs` — 在 `EvolutionAction::TriggerReflection/TriggerEmergencyEvolution` 分支，GVU 執行前 |
| **trigger_signal** | `"stagnation_detection"` |
| **outcome** | `Suppressed` |

**P0 Stub 程式碼（channel_reply.rs，GVU 執行前）：**
```rust
// P0 stub — signal suppression point for stagnation detection (P1)
// TODO P1: replace with real stagnation_detection threshold check from evolution_toggle config
let _signal_should_suppress = false; // always false in P0
// When P1 is ready, replace above with:
//   emitter.emit_signal_suppressed_stub(&agent_id, json!({"consecutive": consecutive}));
```

**位置選擇理由**：
- GVU 執行前是「是否發射演化信號」的決策點
- 停滯偵測邏輯（P1）會在此判斷是否 suppress
- Stub 位置正確，P1 只需替換 `false` 條件即可

---

## 三、Schema Type 對應表

| event_type | Rust 型別 | 對應檔案 | 行號 |
|-----------|-----------|---------|------|
| `skill_activate` | `AuditEventType::SkillActivate` | `schema.rs` | 24 |
| `skill_deactivate` | `AuditEventType::SkillDeactivate` | `schema.rs` | 26 |
| `security_scan` | `AuditEventType::SecurityScan` | `schema.rs` | 28 |
| `gvu_generation` | `AuditEventType::GvuGeneration` | `schema.rs` | 30 |
| `signal_suppressed` | `AuditEventType::SignalSuppressed` | `schema.rs` | 35 |

---

## 四、Part 2 實作計劃

### 4.1 新增檔案

- `crates/duduclaw-gateway/src/evolution_events/emitter.rs`  
  → `EvolutionEventEmitter` struct，含 5 個 typed emit 方法 + global singleton

### 4.2 修改檔案

| 檔案 | 修改內容 |
|------|---------|
| `evolution_events/mod.rs` | 新增 `pub mod emitter;` |
| `channel_reply.rs:114` | `ReplyContext` 新增 `pub evolution_emitter: Arc<EvolutionEventEmitter>` 欄位 |
| `channel_reply.rs:1410` | skill_activate 埋點 |
| `channel_reply.rs:1505` | skill_deactivate 埋點（路徑 A） |
| `channel_reply.rs:1581` | skill_deactivate 埋點（路徑 B） |
| `channel_reply.rs:1660–1664` | signal_suppressed P0 stub 位置 |
| `channel_reply.rs:1726–1770` | gvu_generation 5 種 outcome 埋點 |
| `mcp.rs:4880` | security_scan 埋點（使用全域 singleton） |

### 4.3 Non-blocking 保證

所有 emit 呼叫透過 `tokio::spawn` 完成，主 Agent 流程零阻塞。

---

## 五、驗收矩陣

| 項目 | 狀態 |
|------|------|
| `skill_activate` 埋點 | Part 2 實作中 |
| `skill_deactivate` 埋點（3 路徑） | Part 2 實作中 |
| `security_scan` 埋點 | Part 2 實作中 |
| `gvu_generation` 埋點（5 outcome） | Part 2 實作中 |
| `signal_suppressed` stub 就位 | Part 2 實作中 |
| 所有發射呼叫 non-blocking | 設計保證（tokio::spawn） |
| 現有 Skill 系統行為不受影響 | 待回歸測試 |
| 測試覆蓋率 ≥ 80% | 待 emitter.rs 測試撰寫 |

---

*撰寫者：duduclaw-eng-agent | 任務 ID：dd10bff8-3258-4d36-900a-517a82cbfc21*
