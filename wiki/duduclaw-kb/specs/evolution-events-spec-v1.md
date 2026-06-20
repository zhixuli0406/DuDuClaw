---
title: "EvolutionEvents 技術規格 v1.0"
created: 2026-04-25T00:00:00Z
updated: 2026-04-25T02:10:00Z
status: approved
approver: Agnes (TL)
tags: [sprint-n, p0, spec, evolution-events, jsonl, schema, intent-category, stagnation, p2-prereq]
layer: spec
trust: 1.0
changelog:
  - version: v1.0.2
    date: 2026-04-25
    author: duduclaw-eng-infra
    changes: "QA1 行動項目 S2：確立 signal_suppressed P0 stub canonical metadata 格式（選項 C，Spec §1.1 欄位 null placeholder），更新 §1.1 metadata 說明 + 新增 §2.2 stub 說明"
  - version: v1.0.1
    date: 2026-04-25
    author: duduclaw-pm
    changes: "QA1 補充輪 M1+M2：signal_suppressed skill_id 語義（必須 null）+ outcome 強制對應規則（必須 suppressed）"
---

# EvolutionEvents 技術規格 v1.0

> **審批狀態**：Agnes 最終確認 ✅
> **適用範圍**：Sprint N P0（Schema 基礎 + logger）至 P4（跨 Agent 演化協調）
> **配套 ADR**：[decisions/adr-001-evolution-events-audit-log.md](../decisions/adr-001-evolution-events-audit-log.md)
> **QA 前置依賴**：本文件為 QA1（duduclaw-qa-1）審查的前置條件
>
> **📋 v1.0.1 補充（QA1 行動項目 M1/M2）**：補充 `signal_suppressed` 事件的 `skill_id` 語義規範（M1）及 `outcome` 強制對應規則（M2）。詳見 §1.2 validate() 與 §2.1。
>
> **📋 v1.0.2 補充（QA1 行動項目 S2）**：確立 `emit_signal_suppressed_stub()` 的 P0 canonical stub metadata 格式（選項 C — null placeholders）。詳見 §2.2。

---

## 1. 核心 Schema（Agnes 最終確認，8 欄位）

每筆記錄為單行 JSON，文件為 JSONL 格式（每行一筆，`\n` 結尾）。

**範例記錄：**

```json
{
  "timestamp": "2026-04-25T10:30:00Z",
  "event_type": "skill_activate",
  "agent_id": "duduclaw-main",
  "skill_id": "python-review",
  "generation": null,
  "outcome": "success",
  "trigger_signal": "prediction_error_diagnosis",
  "metadata": {"confidence": 0.87}
}
```

**signal_suppressed P0 stub 範例記錄（Option C — null placeholders）：**

```json
{
  "timestamp": "2026-04-25T10:45:00Z",
  "event_type": "signal_suppressed",
  "agent_id": "duduclaw-main",
  "skill_id": null,
  "generation": null,
  "outcome": "suppressed",
  "trigger_signal": "stagnation_detection",
  "metadata": {"suppressed_signal": null, "trigger_count": null, "window_seconds": null}
}
```

**signal_suppressed P1 範例記錄（真實資料填入）：**

```json
{
  "timestamp": "2026-04-25T10:45:00Z",
  "event_type": "signal_suppressed",
  "agent_id": "duduclaw-main",
  "skill_id": null,
  "generation": null,
  "outcome": "suppressed",
  "trigger_signal": "stagnation_detection",
  "metadata": {"suppressed_signal": "prediction_error_diagnosis", "trigger_count": 3, "window_seconds": 21600}
}
```

> ⚠️ **注意**：`signal_suppressed` 事件中 `skill_id` **必須為 `null`**，`outcome` **必須為 `suppressed`**。詳見 §1.2 與 §2.1。

### 1.1 欄位定義

| 欄位 | 型別 | 必填 | P0 說明 |
|------|------|------|---------|
| `timestamp` | ISO8601 string（RFC3339 UTC） | ✅ | 事件記錄時的 UTC 時間 |
| `event_type` | enum（見 §2） | ✅ | 事件類型，5 種 |
| `agent_id` | string（非空） | ✅ | 觸發或受影響的 Agent 識別碼 |
| `skill_id` | string \| null | — | 涉及的 Skill ID；不適用時為 `null`。⚠️ **`signal_suppressed` 事件中必須為 `null`**（見 §2.1 M1 規範）。信號抑制操作發生在信號層，與特定 Skill 無關 |
| `generation` | int \| null | — | GVU 世代編號（1-based）；**P0 固定為 `null`**，P2 啟用 |
| `outcome` | enum | ✅ | `success` \| `failure` \| `suppressed`。⚠️ **`signal_suppressed` 事件中必須為 `suppressed`**（見 §2.1 M2 規範） |
| `trigger_signal` | string \| null | — | 觸發事件的上游信號名稱 |
| `metadata` | JSON object | — | 任意結構化診斷資料（建議 < 1 KB）。`signal_suppressed` 事件使用以下欄位（見 §2.2 for P0 stub 格式）：<br>• `suppressed_signal`：被抑制的信號名稱（P0 stub = `null`，P1 填入真實值）<br>• `trigger_count`：視窗內觸發次數（P0 stub = `null`，P1 填入真實值）<br>• `window_seconds`：觀察視窗大小秒數（P0 stub = `null`，P1 填入真實值） |

### 1.2 validate() 規則

- `timestamp`：必須為合法 RFC3339 UTC 字串
- `agent_id`：不可為空字串
- `event_type`：必須為已定義的 5 種值之一
- `outcome`：必須為 `success` / `failure` / `suppressed`
- `metadata`：序列化後不超過 1 KB（軟性警告，不阻塞寫入）

#### ⚠️ signal_suppressed 強制規則（M1 + M2，P1 前必執行）

> **這兩條規則為 QA1 補充輪行動項目 M1/M2，validate() 實作必須強制執行。**

**【M1】`skill_id` 必須為 `null`（當 `event_type = signal_suppressed`）**

```
IF event_type == "signal_suppressed" AND skill_id != null
  THEN reject: "signal_suppressed 事件中 skill_id 必須為 null"
```

**語義說明**：`signal_suppressed` 代表 stagnation detection 機制抑制了一個上游信號（由 `trigger_signal` 欄位標識）。此操作發生在**信號層**，並非針對某個具體 Skill 的操作。即使抑制發生的上下文中存在關聯 Skill，也不應將其填入 `skill_id`——被抑制的實體是「信號」，不是「Skill」。如需記錄關聯信號，應使用 `metadata.suppressed_signal` 欄位。

**【M2】`outcome` 必須為 `suppressed`（當 `event_type = signal_suppressed`）**

```
IF event_type == "signal_suppressed" AND outcome != "suppressed"
  THEN reject: "signal_suppressed 事件中 outcome 必須為 suppressed，不得為 success 或 failure"
```

**語義說明**：`signal_suppressed` 事件的語義是「信號被成功抑制」，這本身就是唯一合法的結果。
- `outcome = success`：**非法**。`success` 用於 Skill 啟用/停用等操作的成功結果，不適用於信號抑制語境
- `outcome = failure`：**非法**。若抑制機制本身失敗，系統應拋出 error 而非記錄 `signal_suppressed` 事件
- `outcome = suppressed`：**唯一合法值**。表示信號已被成功攔截，不再傳播至下游

**實作要求**：`validate()` 函式必須在 P1 啟動前加入以上兩條 guard。違反任一規則的記錄必須被拒絕（返回 `Err`），並記錄至 stderr（不寫入 JSONL）。

---

## 2. EventType 枚舉（5 種，P0 全部定義）

| 值 | 說明 | P0 實作狀態 | 主要 trigger_signal |
|----|------|------------|-------------------|
| `skill_activate` | 技能被啟用 | ✅ 已實作 | `prediction_error_diagnosis` |
| `skill_deactivate` | 技能被停用 | ✅ 已實作（含 3 種觸發路徑） | `effectiveness_evaluation` / `sandbox_trial_discard` / `capacity_eviction` |
| `security_scan` | 安全掃描執行 | ✅ 已實作 | `skill_security_scan` |
| `gvu_generation` | GVU 世代循環（Generator-Verifier-Updater） | ✅ 已實作（5 種 outcome 全覆蓋） | dynamic（按 GvuOutcome 變體） |
| `signal_suppressed` | 信號被抑制（Repair Loop 防護審計基礎）| 🔶 Stub（P1 條件觸發） | `stagnation_detection` |

> ⚠️ **`signal_suppressed` P0 說明**：Schema 型別已穩定定義，P0 中 `_signal_should_suppress = false` 為 placeholder；`emit_signal_suppressed_stub()` 方法已完整實作。P1 只需在 `channel_reply.rs` 的 `TODO P1` 處加入閾值 guard，**無需修改 Schema**。

---

### 2.1 signal_suppressed 語義規範（QA1 M1/M2 補充）

> **本節為 M1 + M2 行動項目的正式 Spec 文字，作為實作規範的單一來源。**

#### 2.1.1 event_type 特性摘要

`signal_suppressed` 是 DuDuClaw Repair-Loop 防護機制的核心審計事件。與其他 4 種 event_type 的主要差異如下：

| 特性 | skill_activate / skill_deactivate / security_scan / gvu_generation | signal_suppressed |
|------|-------------------------------------------------------------------|-------------------|
| 操作對象 | Skill（`skill_id` 有值） | 信號（`skill_id` = **null**） |
| 合法 outcome | `success` / `failure` | **僅 `suppressed`** |
| trigger_signal 語義 | 觸發此操作的上游信號 | 抑制機制的啟動信號（通常為 `stagnation_detection`） |
| metadata 建議內容 | 操作特定診斷資料 | `suppressed_signal`、`trigger_count`、`window_seconds` |

#### 2.1.2 skill_id 規範（M1）

**規則**：`signal_suppressed` 事件中，`skill_id` **必須（MUST）為 `null`**。

**原因**：

1. **抑制的對象是信號，不是 Skill**：stagnation detection 監測的是 `trigger_signal` 的觸發頻率，當同一 `trigger_signal` 在時間視窗內超過 `trigger_threshold` 次失敗，系統判定停滯並抑制該信號。被操作的實體是「信號傳播路徑」，Skill 並未在此過程中被直接操作。

2. **避免語義錯誤引用**：即使在抑制發生時存在一個「最近觸發的 Skill」，填入該 Skill 的 `skill_id` 會造成誤解——讀者可能誤以為該 Skill 被停用或受到直接影響，實際上並非如此。

3. **一致性**：`trigger_signal` 欄位已足以標識「哪個信號被抑制」，`metadata.suppressed_signal` 可補充記錄被抑制的具體信號名稱。無需也不應使用 `skill_id` 重複記錄。

**合規範例**：
```json
{
  "event_type": "signal_suppressed",
  "skill_id": null,          ✅ 正確
  "trigger_signal": "stagnation_detection",
  "metadata": {"suppressed_signal": "prediction_error_diagnosis"}
}
```

**非法範例**：
```json
{
  "event_type": "signal_suppressed",
  "skill_id": "python-review", ❌ 非法：validate() 必須拒絕
  "outcome": "suppressed"
}
```

#### 2.1.3 outcome 強制對應規則（M2）

**規則**：`signal_suppressed` 事件中，`outcome` **必須（MUST）為 `suppressed`**。其他 outcome 值在此 event_type 下視為**非法**。

| outcome 值 | signal_suppressed 中的合法性 | 說明 |
|-----------|---------------------------|------|
| `suppressed` | ✅ **唯一合法值** | 信號已被成功攔截，不再傳播 |
| `success` | ❌ **非法，validate() 拒絕** | `success` 語義屬於 Skill 操作成功，不適用信號抑制語境 |
| `failure` | ❌ **非法，validate() 拒絕** | 若抑制機制本身發生錯誤，應由系統 error logging 處理，不應發出 `signal_suppressed` 事件 |

**強制理由**：`signal_suppressed` 事件的唯一語義是「信號已被成功抑制並進入審計日誌」。此事件本身的存在即代表抑制成功——允許 `success` 或 `failure` 會產生語義矛盾（「一個代表抑制的事件，卻說抑制失敗了？」），嚴重破壞審計日誌的可讀性與可信度。

**validate() 實作規範**：
```rust
// M2 強制規則：signal_suppressed 必須搭配 outcome = suppressed
if self.event_type == AuditEventType::SignalSuppressed
    && self.outcome != Outcome::Suppressed
{
    return Err(ValidationError::InvalidOutcomeForEventType {
        event_type: "signal_suppressed",
        got: self.outcome,
        expected: "suppressed",
    });
}

// M1 強制規則：signal_suppressed 的 skill_id 必須為 null
if self.event_type == AuditEventType::SignalSuppressed
    && self.skill_id.is_some()
{
    return Err(ValidationError::SkillIdMustBeNullForEventType {
        event_type: "signal_suppressed",
    });
}
```

---

### 2.2 signal_suppressed P0 Stub Metadata 規範（QA1 S2 — v1.0.2）

> **本節為 S2 行動項目的正式 Spec 文字。決策：採用選項 C（null placeholders）。**

#### 2.2.1 決策背景

QA1 補充輪審查（2026-04-25）指出 `emit_signal_suppressed_stub()` 的 `metadata` 欄位在 P0 stub 呼叫中未統一定義，存在 3 種可能方案：

| 選項 | metadata 格式 | 評估 |
|------|-------------|------|
| **A** | `{}` 空物件 | ❌ 缺乏自我描述能力，P1 工程師不知道應填入什麼 |
| **B** | `{ stub: true, reason: "P1 not implemented" }` | ⚠️ 具自我描述，但 P1 需清除 stub 旗標，且欄位名稱與 P1 實際欄位不一致 |
| **C** | 使用 §1.1 定義的 P1 欄位，值設為 null | ✅ **採用** — 欄位名稱與 P1 完全一致，P1 只需填值，零欄位名稱破壞 |

**決定**：採用 **選項 C**，使用 Spec §1.1 已定義的 P1 欄位名稱（`suppressed_signal`、`trigger_count`、`window_seconds`），P0 stub 時設為 `null`。

#### 2.2.2 P0 Canonical Stub Metadata

`emit_signal_suppressed_stub()` 在 P0 中**必須**傳入以下 metadata（使用 Spec §1.1 欄位的 null 版本）：

```json
{ "suppressed_signal": null, "trigger_count": null, "window_seconds": null }
```

對應 Rust 程式碼：
```rust
emitter.emit_signal_suppressed_stub(
    agent_id,
    serde_json::json!({
        "suppressed_signal": null,
        "trigger_count": null,
        "window_seconds": null
    }),
);
```

#### 2.2.3 P1 遷移路徑

P1 工程師接手時，只需將 null 替換為真實值，**零欄位重命名**：

```rust
// P1: 解封閾值 guard 後，填入真實資料
emitter.emit_signal_suppressed_stub(
    agent_id,
    serde_json::json!({
        "suppressed_signal": "prediction_error_diagnosis",  // 被抑制的信號名稱
        "trigger_count": consecutive,                        // 視窗內失敗次數
        "window_seconds": stagnation_cfg.window_seconds,    // 觀察視窗大小
    }),
);
```

#### 2.2.4 測試要求

`emitter.rs` 的主要 stub 測試（`test_emit_signal_suppressed_stub_writes_event`）需驗證 metadata 欄位結構：

```rust
// P0 stub metadata assertions:
assert_eq!(ev["metadata"]["suppressed_signal"], serde_json::Value::Null);
assert_eq!(ev["metadata"]["trigger_count"], serde_json::Value::Null);
assert_eq!(ev["metadata"]["window_seconds"], serde_json::Value::Null);
```

---

## 3. intent_category Enum 預定義（P2 預留）

> **Agnes 強制要求**：P0 文件必須鎖定語義，避免 P2 實作時改 Schema 破壞已有 JSONL 資料。
> **⚠️ 重要**：此欄位 **P0 不進入 JSONL**，僅在此預定義語義；P2 Sprint 批准後方可加入 `AuditEvent` struct。

### 3.1 定義

```
intent_category: "repair" | "optimize" | "innovate"
```

| 值 | 語義 | 典型觸發場景 | 對應 GVU/Skill 狀態 |
|----|------|------------|-------------------|
| `"repair"` | **修復性演化**：針對 Skill 執行失敗後的補救性演化，目標是恢復能力完整性 | GVU `Abandoned`／`Skipped` 後立即觸發新 Skill 啟用；`skill_deactivate` + 同週期 `skill_activate` 同一領域 | `outcome=failure` 後緊接的 `outcome=success` |
| `"optimize"` | **效能優化型演化**：提升現有 Skill 的執行效率或準確率，非因失敗觸發，屬於主動改善 | GVU `Applied` 且 effectiveness_score 提升 > 10%；定期評估週期觸發的 Skill 版本升級 | `outcome=success`，且前序無 `failure` 信號 |
| `"innovate"` | **創新型演化**：生成全新 Skill 路徑，非基於現有 Skill 的改良，屬於能力邊界擴張 | GVU 週期完成 Skill synthesis，`skill_activate` 對應前所未有的 `skill_id`；探索性任務觸發 | 全新 `skill_id` 首次出現 |

### 3.2 P2 實作指引

P2 Sprint 批准後，工程師需執行以下變更（**僅此範圍，不可提前合入**）：

1. 在 `schema.rs` 的 `AuditEvent` struct 新增欄位：
   ```rust
   pub intent_category: Option<IntentCategory>,
   ```
2. 新增 `IntentCategory` enum 並實作 `Serialize`/`Deserialize`：
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "snake_case")]
   pub enum IntentCategory {
       Repair,
       Optimize,
       Innovate,
   }
   ```
3. P0/P1 歷史 JSONL 記錄中此欄位缺失為**預期行為**（`Option<>` 反序列化為 `None`，零破壞）
4. 更新 `emitter.rs` 的 typed methods，新增 `intent_category: Option<IntentCategory>` 參數（預設 `None`）
5. 更新本文件 §3.1 的 P2 狀態標記

---

## 4. stagnation_detection 配置說明

### 4.1 配置結構（agent.toml）

```toml
[evolution.stagnation_detection]
enabled           = true        # 主開關（預設 true）
window_seconds    = 21600       # 觀察視窗（秒，預設 21600 = 6h）
trigger_threshold = 3           # 視窗內觸發次數門檻（預設 3）
action            = "log_only"  # P0: log_only | P1 reserved: suppress
```

所有欄位均有合理預設值，`agent.toml` 無此區塊時自動 fallback 至預設。

### 4.2 欄位語義

| 欄位 | 型別 | 有效範圍 | 語義說明 |
|------|------|---------|---------|
| `enabled` | bool | true / false | 整個停滯偵測模組的主開關。`false` 時不計數、不觸發任何 action |
| `window_seconds` | int | 60–604800 | 滑動時間視窗大小（秒）。在此視窗內，若同一 `trigger_signal` 的 `failure` 觸發次數達到 `trigger_threshold`，則判定為停滯狀態 |
| `trigger_threshold` | int | 1–1000 | 停滯判定門檻次數。預設值 3 意為：6 小時內同一信號觸發失敗 ≥3 次 → 判定停滯 |
| `action` | enum | `log_only` \| `suppress` | 停滯偵測觸發後的行為。P0 僅支援 `log_only`（記錄 `signal_suppressed` 事件至 JSONL，不阻斷流程）；P1 啟用 `suppress`（主動抑制信號傳播至下游） |

### 4.3 P1 擴充路徑（零 API 破壞）

`StagnationAction::Suppress` 已在 enum 預定義（`// P1 reserved`）。P1 實作只需：

1. 在 `channel_reply.rs` 的 `TODO P1` 標記處加入閾值 guard 邏輯
2. 將 `_signal_should_suppress` placeholder 替換為真實的閾值判斷
3. **無需修改 Schema、agent.toml API 或任何其他模組**

> **P1 實作注意**：啟用 suppress 路徑後所觸發的 `signal_suppressed` 事件，必須遵守 §2.1 規範（`skill_id = null`，`outcome = suppressed`）。validate() 的 M1/M2 規則在 P0 期間即應已實作，P1 無需額外修改。

### 4.4 MCP 工具操作（evolution_toggle）

透過 `evolution_toggle` MCP tool 可動態調整配置：

| field | 型別 | 驗證規則 | 範例值 |
|-------|------|---------|-------|
| `stagnation_enabled` | bool | true / false | `true` |
| `stagnation_window_seconds` | int | 60–604800 | `3600` |
| `stagnation_trigger_threshold` | int | 1–1000 | `5` |
| `stagnation_action` | string | `log_only` \| `suppress` | `"log_only"` |

---

## 5. P0→P4 完整路線圖

```
P0（Sprint N — 當前）
├── 8 欄位 JSONL Schema 穩定鎖定
│   ├── generation = null（P2 啟用）
│   └── intent_category = 文件預定義（P2 加入 JSONL）
├── 5 種 EventType 全部定義（含 signal_suppressed stub）
├── JSONL logger（UTC 日期輪替 + 10 MB 大小上限）
├── stagnation_detection 配置（action = log_only）
├── 5 個發射點完整埋點（含 capacity_eviction fix）
├── 35 tests, 100% pass
├── validate() M1+M2 規則（signal_suppressed 語義強制）✅ v1.0.1 補充
└── stub metadata canonical format 確立（Option C）✅ v1.0.2 補充

P1（Anti-Repair-Loop 主動抑制）
├── signal_suppressed 條件觸發（閾值 guard 解封）
├── metadata 欄位填入真實值（suppressed_signal / trigger_count / window_seconds）
├── stagnation_action = suppress 支援
├── Schema 零破壞性變更
├── 依賴：P0 Schema 穩定 + M1/M2 validate() 規則實作 + S2 stub metadata 格式
└── 前置條件：M1/M2/S2 已完成（本文件 v1.0.2）

P2（演化意圖分類 + GVU 世代追蹤）
├── intent_category 加入 JSONL（repair / optimize / innovate）
├── generation 欄位啟用（GVU 世代編號 1-based）
├── P0/P1 歷史資料向後相容（Option<> 欄位缺失為 None）
├── 混合資料查詢約定章節（M3，待 P2 Spec 起草）
└── 依賴：P0 Schema 穩定

P3（查詢 API + 可視化）
├── evolution_query MCP tool（按 event_type / agent_id / 時間範圍查詢）
├── 趨勢分析（停滯頻率、Skill 存活率、GVU 成功率）
├── JSONL 索引最佳化（反向索引或 SQLite 快取）
└── 依賴：P1 + P2

P4（跨 Agent 演化協調）
├── EvolutionEvents 跨 instance 同步協議
├── 多 Agent 演化協調（避免重複演化同一 Skill）
├── 分散式審計日誌聚合
└── 依賴：P3
```

---

## 6. 不引入項目清單（GEP 概念明確排除）

> **目的**：明確記錄 GEP 哪些概念不借鑑，避免未來工程師誤引入，或提出「為何不使用 GEP 的 X」等問題。

| GEP 概念 | GEP 中的意涵 | 不引入 DuDuClaw 的理由 |
|---------|------------|----------------------|
| **Genes（基因）** | 最小可複用的 Prompt 修復模式，對應特定錯誤信號的 Prompt 片段 | DuDuClaw Skills 是可執行能力單元（含程式碼、sandbox、安全邊界），非 Prompt 碎片。引入 Genes 概念會混淆兩者，造成核心語義退步 |
| **Capsules（膠囊）** | 封裝多個 Genes 的複合修復單元，管理 Gene 生命週期 | DuDuClaw 無對應需求；Skill 的組合與依賴由 SkillBank 管理，不需要額外封裝層，引入只增複雜度 |
| **四種全域演化策略**（balanced / innovate / harden / repair-only） | 全域的 Agent 演化行為導向配置 | `intent_category` 在事件粒度（per-event）捕捉演化意圖，比全域策略更精確。全域策略無法反映單一 Skill 的演化脈絡 |
| **GEP Hub 網路同步** | 可選的中央 Hub，跨實例同步 Genes/Capsules | P4 的跨 Agent 協調將以 DuDuClaw 原生協議設計，不引入外部 Hub 依賴，避免單點故障與廠商鎖定 |
| **JavaScript/JSON 可變資產格式**（.gene.json / .capsule.json） | 以可讀寫的 JSON 檔案儲存演化資產，支援原地修改 | DuDuClaw 採用 JSONL append-only 審計日誌，讀取模式根本不同（audit log vs. mutable assets）。兩者混用會破壞審計一致性 |
| **Shell 命令白名單控管** | GEP 的安全邊界機制（僅允許 node/npm/npx 前綴的 Shell 命令） | DuDuClaw 安全邊界由 `skill_security_scan`（`security_scan` 事件）獨立處理，有更強的安全模型，不需要 GEP 的 Shell 白名單 |
| **`--review` 人機協作模式** | GEP 的互動式確認流程，人工審核每次演化決策 | DuDuClaw 演化為全自動（可透過 `evolution_toggle` 關閉），目標是自主演化而非人工確認循環 |

---

## 7. 模組結構（Rust，duduclaw-gateway）

```
crates/duduclaw-gateway/src/evolution_events/
├── schema.rs    — AuditEvent struct + AuditEventType/Outcome enum + validate()
├── logger.rs    — EvolutionEventLogger（JSONL append + 日期/大小輪替）
├── emitter.rs   — EvolutionEventEmitter（typed fire-and-forget + global singleton）
└── mod.rs       — 模組 re-export + Quick-start 文件
```

**寫入路徑**：`data/evolution/events/YYYY-MM-DD.jsonl`
**環境變數覆蓋**：`$EVOLUTION_EVENTS_DIR`

**並發安全設計**：Tokio `Mutex` 保護單一 file handle；跨 process 依賴 `O_APPEND` 原子性。
**非阻塞保證**：寫入失敗降級至 stderr，絕不拋出 panic 或阻塞呼叫端。

---

## 8. 測試覆蓋（P0 基準，35 tests 100% pass）

| 模組 | 測試數 | 關鍵涵蓋點 |
|------|--------|----------|
| `schema.rs` | 12 | 序列化/反序列化、`validate()`、null 欄位存在性、Display、非法值拒絕 |
| `logger.rs` | 11 | 寫入正確性、JSONL 格式（每行合法 JSON）、並發安全（50 tasks）、日期輪替觸發、10MB 大小輪替、錯誤降級 |
| `emitter.rs` | 12 | 5 種 event_type typed methods、並發發射（30 tasks）、P0 null generation 強制驗證、global singleton 唯一性 |
| **合計** | **35** | **100% pass（P0 驗收基準）** |

> **v1.0.1 新增測試要求**：P1 啟動前，`schema.rs` 測試需補充以下 2 個 validate() 測試 case：
> 1. `test_validate_signal_suppressed_skill_id_must_be_null`：驗證 `signal_suppressed + skill_id = Some(...)` 被拒絕
> 2. `test_validate_signal_suppressed_outcome_must_be_suppressed`：驗證 `signal_suppressed + outcome = success/failure` 被拒絕
>
> **v1.0.2 新增測試要求（S2）**：`emitter.rs` 的 `test_emit_signal_suppressed_stub_writes_event` 需驗證 P0 stub metadata 欄位結構（`suppressed_signal`、`trigger_count`、`window_seconds` 皆為 `null`）。✅ 已更新。

---

## 參照

- 設計決策：[decisions/adr-001-evolution-events-audit-log.md](../decisions/adr-001-evolution-events-audit-log.md)
- GEP 競品研究：[research/ai-repos/entities/2026-04-24-evolver-gep.md](../research/ai-repos/entities/2026-04-24-evolver-gep.md)
- T1+T3 Schema 技術說明：[sprint-n/evolution-events-schema-adr.md](../sprint-n/evolution-events-schema-adr.md)
- T2 Agent 事件發射點盤點：[sprint-n/t2-emission-point-audit.md](../sprint-n/t2-emission-point-audit.md)

---

*規格作者：PM-DuDuClaw（duduclaw-pm）*
*審批人：Agnes（TL）*
*版本：v1.0.2 | 日期：2026-04-25（QA1 S2 — stub metadata 標準化）*
*前版本：v1.0.1 | 日期：2026-04-25（QA1 M1/M2）*
*原始版本：v1.0 | 日期：2026-04-25*
