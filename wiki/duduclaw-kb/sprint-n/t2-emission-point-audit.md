---
title: "T2 — Agent 事件發射點盤點報告（Part 1）"
created: 2026-04-25
updated: 2026-04-25
tags: [sprint-n, p0, agent, event-emitter, skill-system, evolution-events, audit]
layer: engineering
trust: 0.9
---

# T2 — Agent 事件發射點盤點報告

> **撰寫者**：duduclaw-eng-agent  
> **日期**：2026-04-25  
> **Task**：[Sprint-N-P0-T2] Agent 事件發射層實作（5 種 event_type 埋點）

---

## 一、T1 基礎設施現況確認

T1（duduclaw-eng-infra 負責）已完整交付，以下模組全部存在並有測試：

| 檔案 | 用途 | 狀態 |
|------|------|------|
| `crates/duduclaw-gateway/src/evolution_events/schema.rs` | 8 欄位 AuditEvent schema，5 種 event_type，3 種 Outcome，驗證邏輯 | ✅ 完成 |
| `crates/duduclaw-gateway/src/evolution_events/logger.rs` | JSONL append logger，日期 + 10MB rotation，concurrent-safe | ✅ 完成 |
| `crates/duduclaw-gateway/src/evolution_events/emitter.rs` | Non-blocking wrapper（tokio::spawn fire-and-forget），5 種 typed methods，global singleton | ✅ 完成 |
| `crates/duduclaw-gateway/src/evolution_events/mod.rs` | 模組 re-export | ✅ 完成 |

---

## 二、5 種 event_type 發射點完整盤點

### 2.1 `skill_activate` — ✅ 已實作

| 屬性 | 值 |
|------|----|
| **檔案** | `crates/duduclaw-gateway/src/channel_reply.rs` |
| **行數** | 1419–1423 |
| **觸發情境** | Prediction error diagnosis 建議 skill，`ctrl.activate()` 後立即發射 |
| **trigger_signal** | `"prediction_error_diagnosis"` |
| **outcome** | `Success` |
| **non-blocking** | ✅ `emit_skill_activate()` 內部 `tokio::spawn` |

---

### 2.2 `skill_deactivate` — ✅ 部分實作 / ⚠️ capacity_eviction gap

**埋點 A — effectiveness_evaluation**（channel_reply.rs:1521）
- 每 20 次預測週期評估，效益不足的 skill 停用
- `trigger_signal`: `"effectiveness_evaluation"`

**埋點 B — sandbox_trial_discard**（channel_reply.rs:1604）
- Sandbox trial 決策為 DISCARD
- `trigger_signal`: `"sandbox_trial_discard"`

**❌ 缺失 — capacity_eviction**（activation.rs:52–56）
- `activate()` 內部呼叫 `self.deactivate(agent_id, &worst)` 後**無事件發射**
- 修復：`activate()` 改為返回 `Option<String>`，由呼叫端發射事件

---

### 2.3 `security_scan` — ✅ 已實作

| 屬性 | 值 |
|------|----|
| **檔案** | `crates/duduclaw-cli/src/mcp.rs` |
| **行數** | 4986–4998 |
| **觸發情境** | MCP tool `skill_security_scan` handler，`scan_skill()` 返回後 |
| **trigger_signal** | `"skill_security_scan"` |
| **singleton** | 使用 `EvolutionEventEmitter::global()` |

---

### 2.4 `gvu_generation` — ✅ 已實作（5 種 outcome 全覆蓋）

| GvuOutcome 變體 | 行數 | Outcome |
|----------------|------|---------|
| `Applied(version)` | 1772 | `Success` |
| `Abandoned { last_gradient }` | 1788 | `Failure` |
| `Skipped { reason }` | 1800 | `Failure` |
| `Deferred { ... }` | 1819 | `Failure` |
| `TimedOut { ... }` | 1835 | `Failure` |

P0 約束：`generation` 欄位統一為 `null`。

---

### 2.5 `signal_suppressed` — 🔶 Stub 到位（P0 合規）

| 屬性 | 值 |
|------|----|
| **檔案** | `crates/duduclaw-gateway/src/channel_reply.rs` |
| **行數** | 1715–1724 |
| **現況** | `_signal_should_suppress = false` placeholder；發射呼叫在 comment 中標示位置 |
| **emitter method** | `emit_signal_suppressed_stub()` 已完整實作，P1 只需加入條件判斷 |

---

## 三、盤點總表

| event_type | 埋點位置 | trigger_signal | 狀態 |
|---|---|---|---|
| `skill_activate` | channel_reply.rs:1419 | `prediction_error_diagnosis` | ✅ 已實作 |
| `skill_deactivate` | channel_reply.rs:1521 | `effectiveness_evaluation` | ✅ 已實作 |
| `skill_deactivate` | channel_reply.rs:1604 | `sandbox_trial_discard` | ✅ 已實作 |
| `skill_deactivate` | activation.rs:52–56 | `capacity_eviction` | ❌ Part 2 修復 |
| `security_scan` | mcp.rs:4989 | `skill_security_scan` | ✅ 已實作 |
| `gvu_generation` | channel_reply.rs:1772 | dynamic | ✅ Applied→Success |
| `gvu_generation` | channel_reply.rs:1788 | dynamic | ✅ Abandoned→Failure |
| `gvu_generation` | channel_reply.rs:1800 | dynamic | ✅ Skipped→Failure |
| `gvu_generation` | channel_reply.rs:1819 | dynamic | ✅ Deferred→Failure |
| `gvu_generation` | channel_reply.rs:1835 | dynamic | ✅ TimedOut→Failure |
| `signal_suppressed` | channel_reply.rs:1715 | `stagnation_detection` | 🔶 Stub（P1 預留）|

---

## 四、Part 2 修復記錄（capacity_eviction）

### 4.1 activation.rs 修改

`activate()` 返回值 `()` → `Option<String>`，回傳被驅逐的 skill 名稱（若有）。

### 4.2 channel_reply.rs 修改

呼叫端接收返回值，在 `skill_activate` 事件之前先發射 `skill_deactivate(capacity_eviction)`。

---

## 五、驗收狀態

- [x] 5 種 event_type 均有對應埋點（含 capacity_eviction）
- [x] `signal_suppressed` stub 已就位（TODO P1 標記）
- [x] 所有發射呼叫為 non-blocking
- [x] `generation` 欄位 P0 統一為 `null`
- [ ] 現有 Skill 系統行為不受影響（CI 驗證中）
- [ ] 測試覆蓋率 ≥ 80%

---

*本報告由 duduclaw-eng-agent 撰寫，供 TL 審閱*
