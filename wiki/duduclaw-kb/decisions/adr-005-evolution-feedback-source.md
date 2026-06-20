---
title: "ADR-005: Evolution System 外部反饋錨點設計（Recursive Drift 防護）"
created: 2026-04-27T00:00:00Z
updated: 2026-04-27T10:30:00Z
status: approved
approver: Agnes
tags: [adr, evolution, feedback-source, recursive-drift, p2, intent-category, anti-drift]
layer: deep
trust: 1.0
---

# ADR-005: Evolution System 外部反饋錨點設計（Recursive Drift 防護）

> **審批狀態**：Agnes 指令核准 ✅（2026-04-27）
> **觸發事件**：SkillLearnBench 論文發現 self-feedback 持續迭代產生 recursive drift 風險
> **關聯規格**：[specs/evolution-events-spec-v1.md](../specs/evolution-events-spec-v1.md)（§3 intent_category P2）
> **適用階段**：P2 設計鎖定（P2 Sprint 批准後方可實作，今日進設計文件）
> **緊急程度**：P0 — Agnes 裁定今日必須進設計文件，暫緩任何純 self-feedback P2 新設計

---

## 1. 背景與問題陳述

### 1.1 觸發論文：SkillLearnBench

SkillLearnBench（PM-DuDuClaw 2026-04-27 每日研究報告）揭示一個對 DuDuClaw Evolution System 有直接影響的風險：

> **Recursive Drift（遞歸漂移）**：AI Agent 在純 self-feedback 迴圈中持續迭代，會因為缺乏外部錨點而造成評分標準逐漸偏移。每次迭代的誤差會成為下次迭代的基準，形成遞歸累積效應，最終使 Agent 的演化偏離真實使用者需求。

### 1.2 DuDuClaw 當前風險評估

**受影響模組**：Evolution Events P2 — `intent_category` 評分架構

當前 P2 設計（見 [specs/evolution-events-spec-v1.md](../specs/evolution-events-spec-v1.md) §3）中：
- `intent_category`（`repair` | `optimize` | `innovate`）的分類邏輯主要依靠 Agent 自身對事件序列的判斷
- 無外部反饋輸入機制
- **風險**：若演化決策純依賴自我評估，隨時間累積的 drift 可能使 `innovate` 事件增加而 `repair` 事件被系統性低估，或評分閾值逐漸偏移

### 1.3 決策上下文

Agnes 於 2026-04-27 裁定：
- 暫緩任何純 self-feedback 的 Evolution P2 新設計
- 今日在 `intent_category` 評分架構加入 `feedback_source` 欄位設計
- 此設計為 P2 實作的必要前置條件，不可跳過

---

## 2. 設計決策

### 2.1 新增欄位：`feedback_source`

在 P2 的 `AuditEvent` struct 中，與 `intent_category` 並行新增 `feedback_source` 欄位：

```json
{
  "timestamp": "2026-04-27T10:00:00Z",
  "event_type": "skill_activate",
  "agent_id": "duduclaw-main",
  "skill_id": "python-review",
  "generation": 3,
  "outcome": "success",
  "trigger_signal": "prediction_error_diagnosis",
  "intent_category": "optimize",
  "feedback_source": "external_user",
  "metadata": {"confidence": 0.91, "feedback_weight": 0.7}
}
```

### 2.2 `feedback_source` 枚舉定義

| 值 | 語義 | 典型來源 |
|----|------|---------|
| `self` | **自我反饋**：Agent 純依賴自身評估觸發演化 | GVU 內部評估、stagnation detection 自動觸發 |
| `external_user` | **使用者外部反饋**：真實使用者的明確或隱性反饋 | 使用者評分、使用者修正、任務完成率信號 |
| `external_peer` | **同儕 Agent 反饋**：其他 Agent 實例的交叉驗證評估 | 跨 Agent 演化協調（P4 預留）、Peer Review 信號 |

### 2.3 加權係數設計（初始值）

Agnes 裁定的初始加權係數：

```toml
[evolution.feedback_weighting]
self          = 0.3   # 自我反饋：較低權重，避免 recursive drift
external_user = 0.7   # 使用者反饋：最高權重，最接近真實需求
external_peer = 0.5   # 同儕反饋：中等權重，交叉驗證但仍需警戒 peer drift
```

**加權計算規則**：
- `effective_score = Σ(score_i × weight_i) / Σ(weight_i)`
- 多個反饋來源同時存在時，使用加權平均
- 加權係數可透過 `evolution_toggle` 動態調整（P2 MCP 擴充）

### 2.4 外部錨點強制要求

> **核心防護規則**：任何演化決策（`intent_category` 確認）**必須包含至少一個 `external` 信號才能觸發**。

```
RULE anti_recursive_drift_guard:
  IF evolution_decision.feedback_sources = [self]  (僅有自我反饋)
    THEN BLOCK evolution trigger
    AND  emit signal_suppressed(trigger_signal="recursive_drift_guard")
    AND  log "Insufficient external anchor: evolution blocked"

  IF any(feedback_source in [external_user, external_peer])
    THEN ALLOW evolution trigger
    AND  apply weighted scoring
```

**實作影響**：
- P1 的 `signal_suppressed` stub 機制需在 P2 新增 `recursive_drift_guard` 作為有效的 `trigger_signal` 值
- `validate()` 需新增 `feedback_diversity_check()`：確保演化觸發時至少有一個 external 來源

---

## 3. P2 實作規格

### 3.1 Schema 變更（`schema.rs`）

```rust
/// P2 新增欄位（與 intent_category 同步加入）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSource {
    /// 自我反饋：Agent 純依賴自身評估
    #[serde(rename = "self")]
    Self_,
    /// 使用者外部反饋：真實使用者的明確或隱性反饋
    ExternalUser,
    /// 同儕 Agent 反饋：其他 Agent 實例的交叉驗證
    ExternalPeer,
}

// AuditEvent struct 新增欄位（P2，與 intent_category 同批加入）
pub struct AuditEvent {
    // ... 現有 8 欄位不變 ...
    pub intent_category: Option<IntentCategory>,   // P2 同批加入
    pub feedback_source: Option<FeedbackSource>,   // P2 同批加入（本 ADR）
}
```

> ⚠️ **P0/P1 向後相容**：`feedback_source` 為 `Option<>`，歷史 JSONL 記錄中缺失此欄位時反序列化為 `None`，零破壞。

### 3.2 `validate()` 新增規則

```rust
// P2 新增：Recursive Drift Guard
// 演化觸發事件（skill_activate + intent_category 非 None）必須有 external 來源
if self.intent_category.is_some() {
    let has_external = matches!(
        self.feedback_source,
        Some(FeedbackSource::ExternalUser) | Some(FeedbackSource::ExternalPeer)
    );
    if !has_external && !cold_start_config.is_in_warmup() {
        return Err(ValidationError::RecursiveDriftGuardTriggered {
            message: "Evolution decision requires at least one external feedback source",
        });
    }
}
```

### 3.3 `evolution_toggle` MCP 擴充（P2）

新增動態調整加權係數的 MCP 操作：

| field | 型別 | 有效範圍 | 說明 |
|-------|------|---------|------|
| `feedback_weight_self` | float | 0.0–1.0 | 自我反饋權重 |
| `feedback_weight_external_user` | float | 0.0–1.0 | 使用者反饋權重 |
| `feedback_weight_external_peer` | float | 0.0–1.0 | 同儕反饋權重 |
| `feedback_diversity_required` | bool | true/false | 是否強制要求外部錨點（預設 true） |
| `warmup_evolution_count` | u32 | 0–100 | 冷啟動豁免期閾值（預設 10） |

```
evolution_toggle agent_id=duduclaw-main field=feedback_weight_self value=0.3
evolution_toggle agent_id=duduclaw-main field=feedback_diversity_required value=true
evolution_toggle agent_id=duduclaw-main field=warmup_evolution_count value=10
```

### 3.4 metadata 建議欄位

```json
{
  "metadata": {
    "feedback_weight": 0.7,
    "feedback_diversity_score": 0.85,
    "external_signals_count": 2,
    "drift_risk_score": 0.12,
    "cold_start_exempt": false
  }
}
```

---

## 4. P4 展望：同儕反饋協議

`external_peer` 的具體實作依賴 P4 跨 Agent 演化協調：

- P2：定義 `FeedbackSource::ExternalPeer` enum，schema 已就位，`feedback_source = "external_peer"` 可接受但 P2 不實作同儕信號收集機制
- P4：跨 Agent Peer Review 協議設計，`external_peer` 信號正式啟用
- P2 期間若無 P4 同儕信號，演化決策可以 `external_user` 作為唯一外部錨點

---

## 5. 不引入項目

| 概念 | 不引入原因 |
|------|----------|
| **完全禁止 self-feedback** | 自我反饋在初始啟動期（無使用者互動前）是必要的，完全禁止會阻塞 Agent 冷啟動 |
| **固定不可調整的加權係數** | 不同部署場景（企業 vs. 個人）對反饋來源的信任度不同，需動態調整 |
| **單一全域反饋源切換** | 粒度過粗，應在每個演化事件層級記錄反饋來源，而非全域配置 |

---

## 6. 風險與緩解措施

| 風險 | 緩解措施 |
|------|---------|
| 外部反饋稀疏（新 Agent 部署初期使用者互動少） | `feedback_diversity_required` 可暫時設為 `false`；設計冷啟動豁免期（前 N 次演化不強制外部錨點）—— 詳見 §8.1 |
| `external_peer` 同儕反饋的 peer drift 問題 | 同儕反饋加權係數（0.5）低於使用者反饋（0.7），且 P4 設計時需引入多數決機制 |
| `feedback_source` 欄位被惡意填入 `external_user` 以繞過 guard | P4 的身份治理框架（ADR-003，`owner_id + audit_log`）提供驗證層；P2 先以 schema 層 declare-only，不做反欺詐 |

---

## 7. 決策歷程

| 日期 | 決策 |
|------|------|
| 2026-04-27 | SkillLearnBench 論文觸發風險評估（PM-DuDuClaw） |
| 2026-04-27 | Agnes 裁定升級為 P0，今日必須進設計文件 |
| 2026-04-27 | TL-DuDuClaw 完成 ADR-005 設計文件主體（§1–§6） |
| 2026-04-27 | ENG-AGENT 補充工程實作細節（§8：冷啟動豁免、Unit Test 規格、P1 介面對齊）|
| P2 Sprint | 工程師依本 ADR 實作 `feedback_source` 欄位，配合 `intent_category` 同批加入 |

---

## 8. 工程實作補充（ENG-AGENT 審閱，2026-04-27）

### 8.1 冷啟動豁免期詳細設計

對於新部署 Agent 在初期缺乏外部反饋信號的情況，設計如下分段豁免協議，**防止 Drift Guard 在冷啟動期阻塞演化**：

```rust
/// 冷啟動豁免期配置（持久化至 agent metadata store）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColdStartConfig {
    /// 豁免期閾值：前 N 次演化觸發不強制外部錨點（建議初始值：10）
    pub warmup_evolution_count: u32,
    /// 當前已觸發演化次數（跨 session 持久化）
    pub current_evolution_count: u32,
}

impl ColdStartConfig {
    /// 是否仍在冷啟動豁免期內
    pub fn is_in_warmup(&self) -> bool {
        self.current_evolution_count < self.warmup_evolution_count
    }

    /// 記錄一次演化觸發（豁免期計數遞增）
    pub fn record_evolution(&mut self) {
        self.current_evolution_count = self.current_evolution_count.saturating_add(1);
    }
}
```

**豁免期行為規則**：

| 狀態 | `feedback_diversity_required` 全域設定 | 實際行為 |
|------|----------------------------------------|---------|
| 豁免期內（`is_in_warmup = true`） | `true` 或 `false` | **覆蓋**：允許 self-only 演化，metadata 標注 `cold_start_exempt: true` |
| 豁免期結束（`is_in_warmup = false`） | `true` | 強制要求至少一個 external 信號 |
| 豁免期結束 | `false` | 不強制（手動關閉 Drift Guard） |

**豁免期重置（特殊場景）**：
- Agent 大幅重新部署（Soul 重大修改）時，可透過 `evolution_toggle` 重置計數
- 重置操作須在 audit_log 留下完整記錄（重置原因、操作者 agent_id）

```
# 重置豁免期計數
evolution_toggle agent_id=duduclaw-main field=current_evolution_count value=0
```

---

### 8.2 Unit Test 規格（目標覆蓋率 ≥ 80%）

P2 實作時須確保以下 10 個測試案例全數通過：

```rust
#[cfg(test)]
mod feedback_source_tests {
    use super::*;

    // TC-001: self-only 演化事件應被 Drift Guard 阻擋
    #[test]
    fn test_self_only_feedback_blocked_post_warmup() {
        let event = AuditEvent {
            intent_category: Some(IntentCategory::Optimize),
            feedback_source: Some(FeedbackSource::Self_),
            ..Default::default()
        };
        let cold_start = ColdStartConfig { warmup_evolution_count: 10, current_evolution_count: 10 };
        assert!(matches!(
            event.validate_with_cold_start(&cold_start),
            Err(ValidationError::RecursiveDriftGuardTriggered { .. })
        ));
    }

    // TC-002: external_user 信號允許演化觸發
    #[test]
    fn test_external_user_allows_evolution() {
        let event = AuditEvent {
            intent_category: Some(IntentCategory::Optimize),
            feedback_source: Some(FeedbackSource::ExternalUser),
            ..Default::default()
        };
        assert!(event.validate().is_ok());
    }

    // TC-003: external_peer 信號允許演化觸發
    #[test]
    fn test_external_peer_allows_evolution() {
        let event = AuditEvent {
            intent_category: Some(IntentCategory::Innovate),
            feedback_source: Some(FeedbackSource::ExternalPeer),
            ..Default::default()
        };
        assert!(event.validate().is_ok());
    }

    // TC-004: intent_category = None 時不觸發 Drift Guard（非演化決策事件）
    #[test]
    fn test_no_intent_category_bypasses_drift_guard() {
        let event = AuditEvent {
            intent_category: None,
            feedback_source: Some(FeedbackSource::Self_),
            ..Default::default()
        };
        assert!(event.validate().is_ok());
    }

    // TC-005: feedback_source = None（歷史記錄）反序列化為 None，無 panic
    #[test]
    fn test_missing_feedback_source_deserializes_to_none() {
        let json = r#"{"event_type":"skill_activate","agent_id":"test","outcome":"success"}"#;
        let event: AuditEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.feedback_source, None);
    }

    // TC-006: 冷啟動豁免期內，self-only 事件被允許並標注 cold_start_exempt
    #[test]
    fn test_cold_start_exempt_allows_self_only() {
        let event = AuditEvent {
            intent_category: Some(IntentCategory::Repair),
            feedback_source: Some(FeedbackSource::Self_),
            ..Default::default()
        };
        let cold_start = ColdStartConfig { warmup_evolution_count: 10, current_evolution_count: 5 };
        let result = event.validate_with_cold_start(&cold_start);
        assert!(result.is_ok());
        // metadata 須標注 cold_start_exempt: true
        assert_eq!(event.metadata.get("cold_start_exempt"), Some(&true.into()));
    }

    // TC-007: 豁免期恰好結束時（count == warmup），self-only 恢復阻擋
    #[test]
    fn test_warmup_boundary_self_only_blocked() {
        let cold_start = ColdStartConfig { warmup_evolution_count: 10, current_evolution_count: 10 };
        assert!(!cold_start.is_in_warmup());
    }

    // TC-008: 加權分數計算正確性（self=0.3, external_user=0.7）
    #[test]
    fn test_weighted_score_calculation_mixed_sources() {
        let weights = FeedbackWeights::default(); // self=0.3, external_user=0.7, external_peer=0.5
        let scores = vec![
            (FeedbackSource::Self_, 0.6_f32),
            (FeedbackSource::ExternalUser, 0.9_f32),
        ];
        let effective = calculate_effective_score(&scores, &weights);
        // (0.6*0.3 + 0.9*0.7) / (0.3 + 0.7) = (0.18 + 0.63) / 1.0 = 0.81
        assert!((effective - 0.81).abs() < 1e-5);
    }

    // TC-009: signal_suppressed(recursive_drift_guard) 事件在阻擋時正確發射
    #[test]
    fn test_drift_guard_emits_signal_suppressed() {
        let mut emitter = MockEvolutionEmitter::new();
        let blocked_event = build_self_only_evolution_event();
        let cold_start = ColdStartConfig { warmup_evolution_count: 10, current_evolution_count: 10 };

        drift_guard_check(&blocked_event, &cold_start, &mut emitter);

        let emitted = emitter.last_emitted().unwrap();
        assert_eq!(emitted.event_type, EventType::SignalSuppressed);
        assert_eq!(emitted.trigger_signal, TriggerSignal::RecursiveDriftGuard);
    }

    // TC-010: FeedbackSource JSON 序列化格式（snake_case 驗證）
    #[test]
    fn test_feedback_source_serialization_format() {
        assert_eq!(serde_json::to_string(&FeedbackSource::Self_).unwrap(), r#""self""#);
        assert_eq!(serde_json::to_string(&FeedbackSource::ExternalUser).unwrap(), r#""external_user""#);
        assert_eq!(serde_json::to_string(&FeedbackSource::ExternalPeer).unwrap(), r#""external_peer""#);
    }
}
```

---

### 8.3 P2 Sprint 實作排程建議

| Day | 工作項目 | 交付物 |
|-----|---------|--------|
| Day 1 | `FeedbackSource` enum + `AuditEvent` schema 擴充（`Option<>` 向後兼容） | `schema.rs` diff |
| Day 2 | `feedback_diversity_check()` validation + `ColdStartConfig` 豁免期邏輯 | `validation.rs` diff |
| Day 3 | 加權分數計算引擎 + `evolution_toggle` 新 field 擴充 | `scoring.rs`, `toggle.rs` diff |
| Day 4 | Unit Tests TC-001 ~ TC-010 + 整合測試（歷史 JSONL 反序列化驗證） | `feedback_source_tests.rs` |
| Day 5 | Code Review 提交 QA-1/QA-2 審閱 | PR + Review Request |

---

### 8.4 與 P1 `signal_suppressed` 的介面對齊確認

P2 實作前，須確認 P1 的 `TriggerSignal` 枚舉**已預留或可擴充** `recursive_drift_guard` 值，避免破壞現有 JSONL schema：

```rust
// P1 已實作的枚舉（P2 實作前向 eng-infra 確認）
pub enum TriggerSignal {
    // P1 已有值（不可異動）
    StagnationDetected,
    PredictionErrorDiagnosis,
    // P2 新增（本 ADR 要求 eng-infra 協助確認擴充方式）
    RecursiveDriftGuard,
}
```

> ⚠️ **行動項目（ENG-AGENT → eng-infra）**：P2 Sprint 啟動前須確認：
> 1. `TriggerSignal` 枚舉是否允許追加新值（open enum vs. closed enum）
> 2. 若 closed enum，新增 `recursive_drift_guard` 是否需要 JSONL schema 版本號升級
> 3. 確認後在本 ADR 補充確認結論（預計 W18 Day3 前完成）

---

## 參照

- 觸發論文：PM 每日研究報告 2026-04-27 — `research/daily/2026-04-27-pm-daily-research.md`
- Agnes 指令：`research/daily/2026-04-27-agnes-response.md` §1
- Evolution Events 規格：`specs/evolution-events-spec-v1.md`（§3 intent_category P2 預留）
- ADR-003：Agent 身份治理（`owner_id + audit_log`）— P4 驗證層前置依賴
- SkillLearnBench 競品分析：待 PM 整理至 `shared/wiki/competitive/evo-agent-analysis.md`（W18-P1 任務）

---

*主設計者：TL-DuDuClaw*
*工程實作補充：duduclaw-eng-agent（ENG-AGENT）*
*審批人：Agnes（指令核准，2026-04-27）*
*版本：v1.1 | 日期：2026-04-27（ENG-AGENT 補充 §8 冷啟動豁免、Unit Test 規格、P1 介面對齊）*
*下次評審：P2 Sprint kickoff*
