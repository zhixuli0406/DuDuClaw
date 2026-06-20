---
title: "EvolutionEvents JSONL Schema — Sprint N P0 技術說明（ADR 用）"
created: 2026-04-25T00:00:00Z
updated: 2026-04-25T00:00:00Z
tags: [sprint-n, p0, evolution, jsonl, schema, adr, evolution-toggle]
layer: reference
trust: 0.95
---

# EvolutionEvents JSONL Schema 技術說明

> **Sprint N P0 T1 + T3 交付物** — 供 PM 整合至 ADR（Architecture Decision Record）。

---

## Part 1：EvolutionEvents JSONL Schema（T1）

### Agnes 最終確認 Schema（8 欄位）

每筆記錄為單行 JSON，文件為 JSONL 格式（每行一筆，`\n` 結尾）。

| 欄位 | 型別 | 說明 |
|------|------|------|
| `timestamp` | ISO8601 string | 事件記錄時的 UTC 時間（RFC3339） |
| `event_type` | enum | 見下方 EventType 說明 |
| `agent_id` | string | 觸發或受影響的 agent 識別碼（必填，不可空） |
| `skill_id` | string \| null | 涉及的技能 ID；不適用時為 `null` |
| `generation` | int \| null | GVU 世代編號（1-based）；P0 固定為 `null` |
| `outcome` | enum | `success` \| `failure` \| `suppressed` |
| `trigger_signal` | string \| null | 觸發事件的上游信號（如 `"prediction_error"`） |
| `metadata` | JSON object | 任意結構化診斷資料（建議 < 1 KB） |

### EventType 枚舉（5 種）

| 值 | 說明 |
|----|------|
| `skill_activate` | 技能被啟用 |
| `skill_deactivate` | 技能被停用 |
| `security_scan` | 安全掃描執行 |
| `gvu_generation` | GVU 世代循環（Generator-Verifier-Updater） |
| `signal_suppressed` | 信號被抑制（P1 Anti-Repair-Loop 機制審計基礎） |

> ⚠️ `signal_suppressed` 在 P0 已定義於 Schema（型別穩定），但觸發條件（停滯閾值）在 P1 才實作。

### 基礎設施設計

**並發安全**：Tokio `Mutex` 保護單一 file handle；跨 process 依賴 `O_APPEND` 原子性。

**非阻塞**：寫入失敗不影響主流程，錯誤降級至 `stderr`（非阻塞合約保證）。

**輪替策略**：按 UTC 日期輪替（`YYYY-MM-DD.jsonl`）+ 10 MB 大小上限，同日內重置 byte counter。

**寫入路徑**：`data/evolution/events/YYYY-MM-DD.jsonl`（可透過 `$EVOLUTION_EVENTS_DIR` 覆蓋）。

### 模組結構（Rust，`duduclaw-gateway`）

```
crates/duduclaw-gateway/src/evolution_events/
├── schema.rs    — AuditEvent struct + AuditEventType/Outcome enum + validate()
├── logger.rs    — EvolutionEventLogger（JSONL 追加寫入 + 輪替）
├── emitter.rs   — EvolutionEventEmitter（typed fire-and-forget + global singleton）
└── mod.rs       — 模組匯出 + Quick-start 文件
```

### 測試覆蓋（35 tests, 100% pass）

| 模組 | 測試數 | 涵蓋重點 |
|------|--------|---------|
| schema.rs | 12 | 序列化/反序列化、validate()、Display、null 欄位存在性 |
| logger.rs | 11 | 寫入、JSONL 格式、並發安全（50 tasks）、輪替、錯誤降級 |
| emitter.rs | 12 | 5 種 event type、並發（30 tasks）、P0 null generation 驗證 |

### 預留欄位（P2，勿提前合入）

`intent_category: "repair" | "optimize" | "innovate"` — P2 演化意圖分類。實作時機：P2 Sprint 批准後，新增至 `AuditEvent` struct 並更新本文件。

---

## Part 2：evolution_toggle 停滯閾值配置擴充（T3）

### 配置結構（`agent.toml`）

```toml
[evolution.stagnation_detection]
enabled           = true    # 主開關（預設 true）
window_seconds    = 21600   # 觀察視窗（秒，範圍 60–604800，預設 6h）
trigger_threshold = 3       # 視窗內觸發次數門檻（範圍 1–1000，預設 3）
action            = "log_only"  # P0: log_only | P1 reserved: suppress
```

所有欄位均有合理預設值，`agent.toml` 無此區塊時自動 fallback。

### MCP 工具操作（evolution_toggle）

新增 `field` 參數支援：

| field | 型別 | 驗證規則 |
|-------|------|---------|
| `stagnation_enabled` | bool | true / false |
| `stagnation_window_seconds` | int | 60–604800 |
| `stagnation_trigger_threshold` | int | 1–1000 |
| `stagnation_action` | string | `log_only` \| `suppress` |

範例：
```
evolution_toggle agent_id=duduclaw-main field=stagnation_window_seconds value=3600
evolution_toggle agent_id=duduclaw-main field=stagnation_trigger_threshold value=5
```

`evolution_status` 也已更新，顯示完整 stagnation_detection 現況。

### P1 擴充預留

`suppress` action 已在 `StagnationAction` enum 定義（P1 reserved）。P1 只需在 `channel_reply.rs` 的 TODO P1 標記處加入閾值 guard，**無需修改 Schema 或 API**。

---

**相關任務**：`[Sprint-N-P0-T1]` `[Sprint-N-P0-T3]`  
**實作者**：ENG-INFRA（duduclaw-eng-infra）  
**審查者**：QA 甲（awaiting T4 + 1 working day）
