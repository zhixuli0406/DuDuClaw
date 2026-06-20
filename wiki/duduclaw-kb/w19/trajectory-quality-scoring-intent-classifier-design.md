---
title: "W19-P1 軌跡品質評分 + Intent Category 分類系統 — 技術設計文件"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
author: duduclaw-eng-memory
status: in_progress
tags: [w19, p1, quality-scoring, intent-category, skill-synthesis, cosplay, feedback-loop]
layer: engineering
trust: 0.9
task_id: 4f8f71ab-37b1-4c8b-9e4e-bfb42bc32b96
---

# W19-P1 軌跡品質評分 + Intent Category 分類系統

> **撰寫者**：duduclaw-eng-memory（ENG-MEMORY）
> **日期**：2026-04-29
> **Task ID**：`4f8f71ab-37b1-4c8b-9e4e-bfb42bc32b96`
> **前置設計依賴**：
> - [W19-P0 Rollout-to-Skill Pipeline](./skill-synthesis-pipeline-design.md)
> - [EvolutionEvents 規格 v1.0](../specs/evolution-events-spec-v1.md)（§3 intent_category P2 預留）
> - [ADR-005 Evolution Feedback Source](../decisions/adr-005-evolution-feedback-source.md)

---

## 一、設計目標

Rollout-to-Skill Pipeline（W19-P0）的品質保證前置模組。核心洞察（COSPLAY 框架）：
**不是把所有軌跡都變成技能，而是選擇性抽取高價值 rollout**，避免低品質技能降低 Skill Bank 信噪比。

**成功條件**：
- 品質評分可從 EvolutionEvents JSONL 自動計算
- Intent Category 自動分類準確率（人工抽樣驗核）≥ 85%
- 與 W19-P0 Pipeline 完整整合
- 設計文件更新至 wiki ✅（本文件）

---

## 二、架構概覽

```
EvolutionEvents JSONL
data/evolution/events/YYYY-MM-DD.jsonl
         │
         ▼
┌──────────────────────────┐
│  Phase 1a（W19-P1 新增）  │
│  EnhancedQualityScorer   │  近 N 窗口分析 + 完整複雜度計算
│  quality_scorer_v2.rs    │
└────────┬─────────────────┘
         │ Vec<TrajectoryScore>（含 gvu_count、step_count）
         ▼
┌──────────────────────────┐
│  Phase 1b（W19-P1 新增）  │
│  IntentClassifier        │  rule-based 三類分類 + 信心值
│  intent_classifier.rs    │
└────────┬─────────────────┘
         │ Vec<ClassifiedTrajectory>（過濾掉未達門檻者）
         ▼
┌──────────────────────────┐
│  Phase 2（W19-P0 既有）   │
│  Orchestrator            │  validate → extract → scan → graduate
│  orchestrator.rs         │  metadata 附加 intent_category
└────────┬─────────────────┘
         │ EvolutionEvent（skill_graduate + intent metadata）
         ▼
┌──────────────────────────┐
│  Phase 4（W19-P1 新增）   │
│  FeedbackLoop            │  skill_bank_feedback → 權重調整
│  feedback_loop.rs        │
└──────────────────────────┘
```

---

## 三、模組結構

```
crates/duduclaw-gateway/src/skill_synthesis/
├── mod.rs                  ← 更新 re-export
├── quality_scorer.rs       ← W19-P0 既有（保持向後相容）
├── quality_scorer_v2.rs    ← W19-P1 新增：近 N 窗口 + 步驟數計算
├── intent_classifier.rs    ← W19-P1 新增：三類分類器
├── feedback_loop.rs        ← W19-P1 新增：skill_bank_feedback 整合
├── orchestrator.rs         ← 更新：加入 intent_category metadata
├── pipeline.rs             ← 更新：插入 Phase 1b（分類過濾）
└── trigger.rs              ← 不變
```

---

## 四、Phase 1a：增強版品質評分器（quality_scorer_v2.rs）

### 4.1 與 W19-P0 的差異

| 維度 | W19-P0 quality_scorer.rs | W19-P1 quality_scorer_v2.rs |
|------|--------------------------|------------------------------|
| success_rate | 僅篩選 Success 事件（固定 1.0）| 近 N 次滑動窗口：Applied 比例 |
| task_complexity | distinct_triggers / 10 | (gvu_count × avg_step_count) 正規化 |
| 窗口大小 | 全部歷史 | 可配置 N（預設 10）|
| 輸出 | TrajectoryScore | EnhancedTrajectoryScore（含 gvu_count、step_count）|

### 4.2 品質分數公式

```
品質分數 =
  成功率（近 N 次 outcome=Applied 比例）× 0.4
  + 效益提升（effectiveness_score delta）× 0.35
  + 任務複雜度（gvu_generation 次數 × 步驟數 正規化）× 0.25
```

**outcome=Applied 定義**：
- `event_type=gvu_generation` 且 `outcome=success` 的事件
- 若 `metadata.gvu_outcome` 存在且為 `"applied"`，優先使用
- 近 N 次窗口：取最近 N 筆 gvu_generation 事件（不限 outcome），計算 Applied 比例

**task_complexity 計算**：
- `gvu_count` = 該 (agent_id, skill_id) 對的全部 gvu_generation 事件數
- `avg_step_count` = 各事件 `metadata.step_count` 的平均值（缺失時預設 1）
- 正規化：`(gvu_count × avg_step_count).log2() / 10.0`，clamp 至 [0.0, 1.0]

### 4.3 程式碼

```rust
// crates/duduclaw-gateway/src/skill_synthesis/quality_scorer_v2.rs

use crate::evolution_events::schema::{AuditEvent, AuditEventType, Outcome};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
};

/// 近 N 次滑動窗口大小（可透過 ScoringConfig 覆蓋）
pub const DEFAULT_WINDOW_N: usize = 10;

/// 評分權重配置（支援 skill_bank_feedback 動態調整）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    pub success_rate: f64,      // 預設 0.4
    pub effectiveness: f64,     // 預設 0.35
    pub complexity: f64,        // 預設 0.25
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            success_rate: 0.4,
            effectiveness: 0.35,
            complexity: 0.25,
        }
    }
}

impl ScoringWeights {
    /// 驗證權重總和應近似 1.0
    pub fn validate(&self) -> Result<()> {
        let sum = self.success_rate + self.effectiveness + self.complexity;
        if (sum - 1.0).abs() > 0.01 {
            anyhow::bail!("ScoringWeights sum ({}) deviates from 1.0", sum);
        }
        Ok(())
    }
}

/// 增強版軌跡品質評分（W19-P1）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedTrajectoryScore {
    pub agent_id: String,
    pub skill_id: String,
    /// 綜合品質分數（0.0 ~ 1.0+）
    pub score: f64,
    /// 近 N 次 Applied 比例
    pub success_rate: f64,
    /// effectiveness_score delta 平均值
    pub effectiveness_delta: f64,
    /// 任務複雜度（正規化後）
    pub task_complexity: f64,
    /// GVU generation 事件總數（用於 intent 分類）
    pub gvu_count: usize,
    /// 平均每次 generation 的步驟數
    pub avg_step_count: f64,
    /// 最近 N 筆事件中 failure 事件數（用於 repair 分類）
    pub recent_failure_count: usize,
    /// 是否為首次出現的 skill_id（用於 innovate 分類）
    pub is_novel_skill: bool,
    pub event_count: usize,
    pub window_start: String,
    pub window_end: String,
    /// 所使用的評分窗口大小
    pub window_n: usize,
}

/// 計算所有 skill 的增強品質評分，回傳 top 20%
pub async fn score_trajectories_v2(
    events_dir: &Path,
    weights: &ScoringWeights,
    window_n: usize,
    known_skill_ids: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<EnhancedTrajectoryScore>> {
    weights.validate()?;
    let events = load_all_gvu_events(events_dir).await?;
    if events.is_empty() {
        return Ok(vec![]);
    }

    let grouped = group_by_skill(&events);
    let mut scores: Vec<EnhancedTrajectoryScore> = grouped
        .into_iter()
        .map(|(key, evts)| {
            let is_novel = known_skill_ids
                .map(|known| !known.contains(&key.1))
                .unwrap_or(false);
            compute_enhanced_score(key, evts, weights, window_n, is_novel)
        })
        .collect();

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top_n = ((scores.len() as f64 * 0.20).ceil() as usize).max(1);
    scores.truncate(top_n);
    Ok(scores)
}

/// 載入所有 gvu_generation 事件（不限 outcome，用於近 N 窗口分析）
async fn load_all_gvu_events(events_dir: &Path) -> Result<Vec<AuditEvent>> {
    let mut result = Vec::new();
    let mut dir = fs::read_dir(events_dir)
        .await
        .with_context(|| format!("Cannot read events dir: {}", events_dir.display()))?;

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        parse_jsonl_file(&path, &mut result).await?;
    }
    Ok(result)
}

async fn parse_jsonl_file(path: &Path, out: &mut Vec<AuditEvent>) -> Result<()> {
    let file = fs::File::open(path)
        .await
        .with_context(|| format!("Cannot open {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }
        let Ok(event) = serde_json::from_str::<AuditEvent>(&line) else { continue; };
        // 只收集 gvu_generation 且有 skill_id 的事件
        if event.event_type == AuditEventType::GvuGeneration && event.skill_id.is_some() {
            out.push(event);
        }
    }
    Ok(())
}

fn group_by_skill(events: &[AuditEvent]) -> HashMap<(String, String), Vec<&AuditEvent>> {
    let mut map: HashMap<(String, String), Vec<&AuditEvent>> = HashMap::new();
    for e in events {
        if let Some(skill_id) = &e.skill_id {
            map.entry((e.agent_id.clone(), skill_id.clone()))
                .or_default()
                .push(e);
        }
    }
    map
}

fn compute_enhanced_score(
    (agent_id, skill_id): (String, String),
    mut events: Vec<&AuditEvent>,
    weights: &ScoringWeights,
    window_n: usize,
    is_novel_skill: bool,
) -> EnhancedTrajectoryScore {
    // 按 timestamp 排序（最舊 → 最新）
    events.sort_by_key(|e| e.timestamp.as_str());
    let event_count = events.len();

    // 近 N 次滑動窗口（取最新 N 筆）
    let window = if events.len() > window_n {
        &events[events.len() - window_n..]
    } else {
        &events[..]
    };

    // 成功率：窗口內 outcome=success（或 metadata.gvu_outcome="applied"）的比例
    let applied_in_window = window.iter().filter(|e| is_applied(e)).count();
    let success_rate = if window.is_empty() {
        0.0
    } else {
        applied_in_window as f64 / window.len() as f64
    };

    // 近窗口內 failure 數（用於 repair 分類）
    let recent_failure_count = window.iter().filter(|e| e.outcome == Outcome::Failure).count();

    // effectiveness_delta：全部事件的平均 delta（較穩定）
    let effectiveness_deltas: Vec<f64> = events
        .iter()
        .filter_map(|e| {
            e.metadata
                .as_ref()
                .and_then(|m| m.get("effectiveness_score_delta"))
                .and_then(|v| v.as_f64())
        })
        .collect();
    let effectiveness_delta = if effectiveness_deltas.is_empty() {
        0.0
    } else {
        effectiveness_deltas.iter().sum::<f64>() / effectiveness_deltas.len() as f64
    };

    // 步驟數計算
    let step_counts: Vec<f64> = events
        .iter()
        .filter_map(|e| {
            e.metadata
                .as_ref()
                .and_then(|m| m.get("step_count"))
                .and_then(|v| v.as_f64())
        })
        .collect();
    let avg_step_count = if step_counts.is_empty() {
        1.0
    } else {
        step_counts.iter().sum::<f64>() / step_counts.len() as f64
    };

    // task_complexity：(gvu_count × avg_step_count) log2 正規化
    let gvu_count = event_count;
    let raw_complexity = (gvu_count as f64) * avg_step_count;
    let task_complexity = if raw_complexity <= 1.0 {
        0.0
    } else {
        (raw_complexity.log2() / 10.0).min(1.0)
    };

    let score = success_rate * weights.success_rate
        + effectiveness_delta.clamp(0.0, 1.0) * weights.effectiveness
        + task_complexity * weights.complexity;

    let timestamps: Vec<&str> = events.iter().map(|e| e.timestamp.as_str()).collect();
    let window_start = timestamps.iter().min().copied().unwrap_or("").to_string();
    let window_end = timestamps.iter().max().copied().unwrap_or("").to_string();

    EnhancedTrajectoryScore {
        agent_id,
        skill_id,
        score,
        success_rate,
        effectiveness_delta,
        task_complexity,
        gvu_count,
        avg_step_count,
        recent_failure_count,
        is_novel_skill,
        event_count,
        window_start,
        window_end,
        window_n,
    }
}

/// 判斷事件是否為 "Applied"（成功應用）
fn is_applied(event: &AuditEvent) -> bool {
    // 優先使用 metadata.gvu_outcome
    if let Some(meta) = &event.metadata {
        if let Some(gvu_outcome) = meta.get("gvu_outcome").and_then(|v| v.as_str()) {
            return gvu_outcome == "applied";
        }
    }
    // fallback：outcome=success
    event.outcome == Outcome::Success
}
```

---

## 五、Phase 1b：Intent Category 分類器（intent_classifier.rs）

### 5.1 分類規則與門檻

| 類別 | 語義 | 分類信號 | 品質門檻 | 保留優先級 |
|------|------|---------|---------|----------|
| `repair` | 修復能力缺口 | 近 N 窗口有 failure 信號（recent_failure_count > 0）且 success_rate 有改善 | ≥ 0.6 | 高優先保留 |
| `optimize` | 效能提升 | effectiveness_delta > 0.10 且 recent_failure_count = 0 | ≥ 0.7 | 中優先 |
| `innovate` | 全新能力擴張 | is_novel_skill = true | ≥ 0.8 | 保守保留 |

**分類優先序**（multi-signal 衝突解決）：
1. `innovate` 信號（is_novel_skill）優先評估，但門檻最高
2. `repair` 信號（recent_failure > 0）次之
3. `optimize` 為 default（以上都不符合時）

**信心值計算**：
- `innovate`：`is_novel_skill` 為 bool 信號，信心值 = min(1.0, score / 0.8)
- `repair`：`failure_recovery_rate`（近窗口 failure 後接 success 的轉換率）
- `optimize`：effectiveness_delta 正規化至 [0, 1]

### 5.2 程式碼

```rust
// crates/duduclaw-gateway/src/skill_synthesis/intent_classifier.rs

use crate::skill_synthesis::quality_scorer_v2::EnhancedTrajectoryScore;
use serde::{Deserialize, Serialize};

/// Intent Category（對應 EvolutionEvents Spec §3 P2 預留語義）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentCategory {
    /// 修復性演化：針對能力缺口的補救性技能抽取（門檻：≥ 0.6）
    Repair,
    /// 效能優化型演化：提升現有技能效率/準確率（門檻：≥ 0.7）
    Optimize,
    /// 創新型演化：全新能力邊界擴張（門檻：≥ 0.8）
    Innovate,
}

impl IntentCategory {
    /// 該類別的品質門檻
    pub fn quality_threshold(&self) -> f64 {
        match self {
            Self::Repair   => 0.6,
            Self::Optimize => 0.7,
            Self::Innovate => 0.8,
        }
    }

    /// 顯示名稱（用於 log）
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Repair   => "repair",
            Self::Optimize => "optimize",
            Self::Innovate => "innovate",
        }
    }
}

/// 分類決策結果
#[derive(Debug, Clone)]
pub struct ClassifiedTrajectory {
    pub trajectory: EnhancedTrajectoryScore,
    pub intent: IntentCategory,
    /// 分類信心值（0.0–1.0）
    pub confidence: f64,
    /// 是否通過該類別的品質門檻
    pub passes_threshold: bool,
    /// 信號證據（可解釋性）
    pub evidence: ClassificationEvidence,
}

/// 分類依據的信號證據
#[derive(Debug, Clone, Default)]
pub struct ClassificationEvidence {
    /// 近 N 窗口中 failure 事件數 > 0
    pub has_recent_failure: bool,
    /// effectiveness delta > 10%（repair 後的顯著改善）
    pub significant_effectiveness_gain: bool,
    /// skill_id 為首次出現
    pub is_novel_skill: bool,
    /// 近窗口 failure → success 的轉換率（repair 品質指標）
    pub failure_recovery_rate: f64,
}

/// 分類配置（可透過 FeedbackLoop 動態調整）
#[derive(Debug, Clone)]
pub struct ClassifierConfig {
    /// effectiveness gain 的顯著性門檻（預設 0.10 = 10%）
    pub effectiveness_gain_threshold: f64,
    /// 觸發 repair 判定的最低 failure 次數（預設 1）
    pub min_failure_for_repair: usize,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            effectiveness_gain_threshold: 0.10,
            min_failure_for_repair: 1,
        }
    }
}

/// 對一批 EnhancedTrajectoryScore 執行 Intent 分類
/// 回傳：通過門檻的 ClassifiedTrajectory（未通過者過濾掉）
pub fn classify_trajectories(
    scores: Vec<EnhancedTrajectoryScore>,
    config: &ClassifierConfig,
) -> Vec<ClassifiedTrajectory> {
    scores
        .into_iter()
        .filter_map(|score| classify_single(score, config))
        .collect()
}

/// 對單一軌跡進行分類
/// 若不通過門檻，回傳 None（由 classify_trajectories 過濾）
fn classify_single(
    score: EnhancedTrajectoryScore,
    config: &ClassifierConfig,
) -> Option<ClassifiedTrajectory> {
    let evidence = build_evidence(&score, config);

    // 分類優先序：innovate → repair → optimize
    let (intent, confidence) = determine_intent(&score, &evidence);

    let passes_threshold = score.score >= intent.quality_threshold();

    if !passes_threshold {
        tracing::debug!(
            skill_id = %score.skill_id,
            score = score.score,
            intent = %intent.display_name(),
            threshold = intent.quality_threshold(),
            "Trajectory filtered out: below quality threshold"
        );
        return None;
    }

    Some(ClassifiedTrajectory {
        trajectory: score,
        intent,
        confidence,
        passes_threshold,
        evidence,
    })
}

fn build_evidence(score: &EnhancedTrajectoryScore, config: &ClassifierConfig) -> ClassificationEvidence {
    let has_recent_failure = score.recent_failure_count >= config.min_failure_for_repair;
    let significant_effectiveness_gain = score.effectiveness_delta >= config.effectiveness_gain_threshold;
    let is_novel_skill = score.is_novel_skill;

    // 失敗後恢復率：近窗口 failure 數 vs. total 的補數（估算）
    // 精確版需要序列分析，此為快速近似
    let failure_recovery_rate = if has_recent_failure {
        score.success_rate
    } else {
        0.0
    };

    ClassificationEvidence {
        has_recent_failure,
        significant_effectiveness_gain,
        is_novel_skill,
        failure_recovery_rate,
    }
}

fn determine_intent(
    score: &EnhancedTrajectoryScore,
    evidence: &ClassificationEvidence,
) -> (IntentCategory, f64) {
    // 1. Innovate：全新 skill_id 首次出現
    if evidence.is_novel_skill {
        let confidence = (score.score / IntentCategory::Innovate.quality_threshold()).min(1.0);
        return (IntentCategory::Innovate, confidence);
    }

    // 2. Repair：近 N 窗口有 failure 信號
    if evidence.has_recent_failure {
        // 信心值依 failure_recovery_rate 決定（有 failure 且成功恢復才高信心）
        let confidence = evidence.failure_recovery_rate.clamp(0.0, 1.0);
        return (IntentCategory::Repair, confidence);
    }

    // 3. Optimize（預設）：效能提升或一般成功案例
    let confidence = if evidence.significant_effectiveness_gain {
        // 效益顯著提升：高信心
        (score.effectiveness_delta / 0.5).min(1.0)
    } else {
        // 一般成功案例：中等信心
        (score.score / IntentCategory::Optimize.quality_threshold()).min(0.8)
    };
    (IntentCategory::Optimize, confidence)
}
```

---

## 六、反饋閉環（feedback_loop.rs）

### 6.1 設計原則

整合 `skill_bank_feedback` MCP API，讓技能的**實際使用結果**反向調整品質評分權重：

```
skill_graduate → 寫入 Skill Bank
    ↓ (非同步，不阻塞主流程)
FeedbackCollector（每 1 小時輪詢）
    ↓ 監聽 skill_activate / skill_deactivate events
    ↓ 呼叫 skill_bank_feedback(skill_id, success=true/false)
    ↓
WeightAdjuster（貝葉斯更新）
    ↓ 更新 ScoringWeights（success_rate 權重上升/下降）
    ↓ 持久化至 ~/.duduclaw/skill_synthesis/weights.json
    ↓
下次 Pipeline 執行使用新權重
```

### 6.2 程式碼

```rust
// crates/duduclaw-gateway/src/skill_synthesis/feedback_loop.rs

use crate::skill_synthesis::quality_scorer_v2::ScoringWeights;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::RwLock;

/// 技能使用反饋記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFeedbackRecord {
    pub skill_id: String,
    pub agent_id: String,
    pub intent_category: String,
    pub trajectory_score: f64,
    /// 實際使用成功次數（貝葉斯先驗）
    pub success_count: u32,
    /// 實際使用失敗次數（貝葉斯先驗）
    pub failure_count: u32,
    /// 從 Skill Bank 取得的 confidence（0.0–1.0）
    pub bank_confidence: f64,
    pub last_updated: String,
}

impl SkillFeedbackRecord {
    /// 貝葉斯後驗成功率（Beta 分佈 mean）
    pub fn posterior_success_rate(&self) -> f64 {
        let alpha = self.success_count as f64 + 1.0; // 先驗 α=1
        let beta = self.failure_count as f64 + 1.0;  // 先驗 β=1
        alpha / (alpha + beta)
    }
}

/// MCP 反饋 API 抽象（便於測試注入 Mock）
#[async_trait]
pub trait FeedbackHandler: Send + Sync {
    async fn submit_feedback(&self, skill_id: &str, success: bool) -> Result<()>;
    /// 從 EvolutionEvents 取得技能的最新 activate/deactivate 事件
    async fn get_recent_skill_outcomes(
        &self,
        skill_id: &str,
        limit: usize,
    ) -> Result<Vec<SkillOutcome>>;
}

#[derive(Debug, Clone)]
pub struct SkillOutcome {
    pub skill_id: String,
    pub is_success: bool,
    pub timestamp: String,
}

/// 反饋閉環控制器
pub struct FeedbackLoop {
    feedback_handler: Arc<dyn FeedbackHandler>,
    weights: Arc<RwLock<ScoringWeights>>,
    weights_path: PathBuf,
    /// 貝葉斯更新步長（預設 0.01）
    learning_rate: f64,
}

impl FeedbackLoop {
    pub fn new(
        feedback_handler: Arc<dyn FeedbackHandler>,
        weights_path: PathBuf,
        initial_weights: ScoringWeights,
        learning_rate: f64,
    ) -> Self {
        Self {
            feedback_handler,
            weights: Arc::new(RwLock::new(initial_weights)),
            weights_path,
            learning_rate,
        }
    }

    /// 取得當前評分權重（供 Pipeline 使用）
    pub async fn current_weights(&self) -> ScoringWeights {
        self.weights.read().await.clone()
    }

    /// 提交技能使用反饋並更新權重
    ///
    /// 規則：
    /// - 高品質技能（score > 0.7）被使用後失敗 → success_rate 權重 +learning_rate
    /// - 低品質技能（score <= 0.5）被使用後成功 → effectiveness 權重 +learning_rate
    /// - 一般情況：維持當前權重
    pub async fn submit_and_adjust(
        &self,
        skill_id: &str,
        success: bool,
        trajectory_score: f64,
    ) -> Result<()> {
        // 1. 呼叫 MCP skill_bank_feedback
        self.feedback_handler.submit_feedback(skill_id, success).await?;

        // 2. 貝葉斯權重調整
        let mut weights = self.weights.write().await;
        adjust_weights(&mut weights, success, trajectory_score, self.learning_rate);

        // 3. 正規化確保總和為 1.0
        normalize_weights(&mut weights);

        // 4. 持久化
        let json = serde_json::to_string_pretty(&*weights)?;
        tokio::fs::write(&self.weights_path, json).await?;

        tracing::info!(
            skill_id = %skill_id,
            success = %success,
            trajectory_score = %trajectory_score,
            new_success_rate_weight = %weights.success_rate,
            "Feedback submitted and weights adjusted"
        );

        Ok(())
    }

    /// 批次收集並提交最近畢業技能的反饋（定期觸發，每 1 小時）
    pub async fn collect_and_submit_batch(
        &self,
        graduated_skills: &[(String, String, f64)], // (agent_id, skill_id, trajectory_score)
    ) -> FeedbackBatchStats {
        let mut stats = FeedbackBatchStats::default();

        for (_, skill_id, trajectory_score) in graduated_skills {
            match self
                .feedback_handler
                .get_recent_skill_outcomes(skill_id, 5)
                .await
            {
                Ok(outcomes) if !outcomes.is_empty() => {
                    for outcome in &outcomes {
                        let _ = self
                            .submit_and_adjust(skill_id, outcome.is_success, *trajectory_score)
                            .await;
                        if outcome.is_success {
                            stats.success_feedbacks += 1;
                        } else {
                            stats.failure_feedbacks += 1;
                        }
                    }
                    stats.skills_processed += 1;
                }
                Ok(_) => {
                    // 無最近使用記錄，跳過
                    tracing::debug!(skill_id = %skill_id, "No recent outcomes found, skipping feedback");
                }
                Err(e) => {
                    stats.errors += 1;
                    tracing::warn!(skill_id = %skill_id, error = %e, "Failed to get skill outcomes");
                }
            }
        }
        stats
    }
}

#[derive(Debug, Default, Clone)]
pub struct FeedbackBatchStats {
    pub skills_processed: usize,
    pub success_feedbacks: usize,
    pub failure_feedbacks: usize,
    pub errors: usize,
}

/// 貝葉斯風格的權重調整函式
fn adjust_weights(weights: &mut ScoringWeights, success: bool, score: f64, lr: f64) {
    if success && score <= 0.5 {
        // 低分技能成功使用：effectiveness 和 complexity 貢獻度可能被低估
        weights.effectiveness += lr;
        weights.success_rate -= lr * 0.5;
        weights.complexity -= lr * 0.5;
    } else if !success && score > 0.7 {
        // 高分技能使用失敗：success_rate 的預測力強化
        weights.success_rate += lr;
        weights.effectiveness -= lr * 0.5;
        weights.complexity -= lr * 0.5;
    }
    // 邊界保護：確保各權重 >= 0.1
    weights.success_rate = weights.success_rate.max(0.1);
    weights.effectiveness = weights.effectiveness.max(0.1);
    weights.complexity = weights.complexity.max(0.1);
}

/// 正規化至總和為 1.0
fn normalize_weights(weights: &mut ScoringWeights) {
    let sum = weights.success_rate + weights.effectiveness + weights.complexity;
    if sum > 0.0 {
        weights.success_rate /= sum;
        weights.effectiveness /= sum;
        weights.complexity /= sum;
    }
}
```

---

## 七、Pipeline 整合更新（pipeline.rs）

### 7.1 更新後的 Phase 流程

W19-P1 在 W19-P0 的 pipeline.rs 中插入 Phase 1b（Intent 分類過濾），
並在 Phase 4 啟動 FeedbackLoop 異步收集：

```rust
// pipeline.rs 修改摘要（完整 diff 參見 PR）

use crate::skill_synthesis::{
    intent_classifier::{classify_trajectories, ClassifiedTrajectory, ClassifierConfig},
    quality_scorer_v2::{score_trajectories_v2, ScoringWeights},
    feedback_loop::{FeedbackLoop, FeedbackBatchStats},
    // ... W19-P0 既有 imports
};

/// Pipeline 單次執行統計（W19-P1 擴充）
#[derive(Debug, Default, Clone)]
pub struct PipelineStats {
    // W19-P0 既有欄位
    pub trajectories_selected: usize,
    pub skills_graduated: usize,
    pub security_blocked: usize,
    pub extraction_failed: usize,
    pub format_errors: usize,
    pub agent_not_found: usize,
    // W19-P1 新增欄位
    pub repair_classified: usize,
    pub optimize_classified: usize,
    pub innovate_classified: usize,
    pub below_threshold_filtered: usize,
    pub feedback_submitted: usize,
}

pub async fn run_once(&self) -> PipelineStats {
    let mut stats = PipelineStats::default();

    // Phase 1a: 增強品質評分（W19-P1：使用 score_trajectories_v2）
    let current_weights = self.feedback_loop.current_weights().await;
    let top_trajectories = match score_trajectories_v2(
        &self.events_dir,
        &current_weights,
        DEFAULT_WINDOW_N,
        Some(&self.known_skill_ids),
    ).await {
        Ok(t) => t,
        Err(e) => { tracing::error!("Phase 1a failed: {}", e); return stats; }
    };

    let pre_filter_count = top_trajectories.len();

    // Phase 1b: Intent 分類 + 門檻過濾（W19-P1 新增）
    let classified = classify_trajectories(top_trajectories, &self.classifier_config);
    stats.below_threshold_filtered = pre_filter_count.saturating_sub(classified.len());

    // 統計分類結果
    for c in &classified {
        match c.intent {
            IntentCategory::Repair   => stats.repair_classified += 1,
            IntentCategory::Optimize => stats.optimize_classified += 1,
            IntentCategory::Innovate => stats.innovate_classified += 1,
        }
    }

    tracing::info!(
        total = pre_filter_count,
        passed = classified.len(),
        filtered = stats.below_threshold_filtered,
        repair = stats.repair_classified,
        optimize = stats.optimize_classified,
        innovate = stats.innovate_classified,
        "Phase 1b intent classification complete"
    );

    stats.trajectories_selected = classified.len();

    // Phase 2: 批次抽取（附加 intent_category 至 metadata）
    let mut graduated_skills: Vec<(String, String, f64)> = Vec::new();
    let mut skipped_agents = std::collections::HashSet::new();

    for classified_traj in &classified {
        let trajectory = &classified_traj.trajectory;
        if skipped_agents.contains(&trajectory.agent_id) {
            stats.agent_not_found += 1; continue;
        }

        let result = extract_and_graduate_with_intent(
            trajectory,
            &classified_traj.intent,
            classified_traj.confidence,
            self.mcp_handler.as_ref(),
            self.emitter.as_ref(),
        ).await;

        match &result {
            ExtractionResult::Graduated { skill_id } => {
                stats.skills_graduated += 1;
                graduated_skills.push((
                    trajectory.agent_id.clone(),
                    skill_id.clone(),
                    trajectory.score,
                ));
            }
            // ... 其餘分支與 W19-P0 相同
        }
    }

    // Phase 4: 異步反饋收集（不阻塞 pipeline 回傳）
    if !graduated_skills.is_empty() {
        let feedback_loop = Arc::clone(&self.feedback_loop);
        let skills_for_feedback = graduated_skills.clone();
        tokio::spawn(async move {
            let batch_stats = feedback_loop
                .collect_and_submit_batch(&skills_for_feedback)
                .await;
            tracing::info!(
                processed = batch_stats.skills_processed,
                success = batch_stats.success_feedbacks,
                failure = batch_stats.failure_feedbacks,
                "Phase 4 feedback batch complete"
            );
        });
        stats.feedback_submitted = graduated_skills.len();
    }

    stats
}
```

### 7.2 emit_skill_graduate 擴充 metadata（intent_category 附加）

```rust
// orchestrator.rs — extract_and_graduate_with_intent()
// 在 emit_skill_graduate 的 metadata 中附加 intent_category 和 confidence

emitter.emit_skill_graduate(
    agent_id,
    skill_name,
    Outcome::Success,
    Some(serde_json::json!({
        "trajectory_score": trajectory.score,
        "event_count": trajectory.event_count,
        "window_start": trajectory.window_start,
        "window_end": trajectory.window_end,
        "gvu_count": trajectory.gvu_count,
        "avg_step_count": trajectory.avg_step_count,
        // W19-P1 新增欄位：
        "intent_category": intent.display_name(),   // "repair" | "optimize" | "innovate"
        "intent_confidence": confidence,
        "quality_threshold": intent.quality_threshold(),
    })),
);
```

---

## 八、測試計畫（覆蓋率目標 ≥ 80%）

### 8.1 quality_scorer_v2 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_v2_success_rate_near_n_window` | 20 筆事件，近 10 筆有 8 Applied | success_rate = 0.8 |
| `test_v2_uses_gvu_outcome_metadata_first` | metadata 含 gvu_outcome=applied | 優先使用 metadata 判斷 |
| `test_v2_task_complexity_log2_normalized` | 100 個事件，avg step=5 | complexity = log2(500)/10 ≈ 0.90 |
| `test_v2_task_complexity_single_event` | 1 個事件，step=1 | complexity = 0.0（log2(1)=0）|
| `test_v2_novel_skill_flagged` | known_skill_ids 不含此 skill | is_novel_skill = true |
| `test_v2_recent_failure_count_correct` | 近 10 筆有 3 failure | recent_failure_count = 3 |
| `test_v2_weights_applied_correctly` | 自訂 weights | score = Σ(component × weight)|
| `test_v2_top_20_percent_selection` | 15 個 skills | 回傳 top 3（ceil(15×0.2)）|
| `test_v2_weights_validation_fails` | weights sum = 1.2 | validate() 回傳 Err |
| `test_v2_effectiveness_delta_from_metadata` | metadata effectiveness_score_delta=0.3 | effectiveness_delta = 0.3 |

### 8.2 intent_classifier 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_classify_innovate_novel_skill` | is_novel_skill=true, score=0.85 | IntentCategory::Innovate, passes=true |
| `test_classify_innovate_below_threshold` | is_novel_skill=true, score=0.75 | 過濾掉（None）|
| `test_classify_repair_has_failure` | recent_failure=2, score=0.65 | IntentCategory::Repair, passes=true |
| `test_classify_repair_below_threshold` | recent_failure=1, score=0.55 | 過濾掉（None）|
| `test_classify_optimize_default` | no failure, no novel, score=0.75 | IntentCategory::Optimize, passes=true |
| `test_classify_optimize_below_threshold` | score=0.65 | 過濾掉（None）|
| `test_classify_innovate_priority_over_repair` | is_novel=true AND recent_failure>0 | IntentCategory::Innovate（innovate 優先）|
| `test_classify_repair_confidence_is_recovery_rate` | failure=2, success_rate=0.7 | confidence = 0.7 |
| `test_classify_optimize_high_confidence_with_delta` | effectiveness_delta=0.3 | confidence > 0.5 |
| `test_classify_batch_filters_below_threshold` | 5 trajectories，2 未達門檻 | 回傳 3 ClassifiedTrajectory |

### 8.3 feedback_loop 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_submit_feedback_calls_mcp` | success=true | MockFeedbackHandler.submit_feedback 被呼叫 |
| `test_weights_increase_success_rate_when_high_score_fails` | score=0.8, success=false | success_rate 權重上升 |
| `test_weights_increase_effectiveness_when_low_score_succeeds` | score=0.4, success=true | effectiveness 權重上升 |
| `test_weights_normalized_after_adjustment` | 任意調整 | weights 總和 ≈ 1.0 |
| `test_weights_floor_at_0_1` | 極端調整 | 各權重 >= 0.1 |
| `test_weights_persisted_to_file` | 調整後 | weights.json 更新 |
| `test_feedback_batch_skip_no_outcomes` | 無近期使用記錄 | 不呼叫 submit_feedback |
| `test_feedback_batch_error_not_panic` | get_recent_skill_outcomes Err | errors += 1，不 panic |
| `test_posterior_success_rate_bayesian` | success=3, failure=1 | (3+1)/(3+1+1+1) = 0.667 |

### 8.4 pipeline 整合測試（新增）

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_pipeline_v2_intent_stats_correct` | 混合 repair/optimize/innovate | 各 stats 計數正確 |
| `test_pipeline_v2_below_threshold_filtered` | 1 optimize 分數 0.65 | below_threshold_filtered=1 |
| `test_pipeline_v2_feedback_spawned_async` | 3 skills 畢業 | feedback_submitted=3 |
| `test_pipeline_v2_uses_feedback_weights` | weights.json 已調整 | Phase 1a 使用更新後的 weights |

---

## 九、Sprint 點數估算（W19-P1 實作）

| 工作項目 | 點數 |
|---------|------|
| `quality_scorer_v2.rs` + 10 tests | 2 SP |
| `intent_classifier.rs` + 10 tests | 2 SP |
| `feedback_loop.rs` + 9 tests | 2 SP |
| `pipeline.rs` 更新 + 4 整合 tests | 1 SP |
| `orchestrator.rs` emit_skill_graduate metadata 擴充 | 0.5 SP |
| **小計** | **7.5 SP** |

---

## 十、與 W19-P0 的整合界面

| W19-P0 提供 | W19-P1 消費 |
|------------|------------|
| `TrajectoryScore` struct（quality_scorer.rs）| 向後相容，P1 使用 `EnhancedTrajectoryScore` 替代 |
| `EvolutionEventEmitter.emit_skill_graduate()` | P1 附加 `intent_category` metadata |
| `McpHandler` trait | P1 新增 `FeedbackHandler` trait（相似抽象層）|
| `PipelineStats` | P1 擴充欄位（向後相容 Default）|
| `SkillSynthesisPipeline` | P1 新增 `feedback_loop` + `classifier_config` 欄位 |

> ⚠️ **向後相容承諾**：`quality_scorer.rs`（W19-P0）保持不變。`quality_scorer_v2.rs` 為新模組，
> Pipeline 使用 `v2`，舊的 `score_trajectories` 函式繼續可用於測試/回歸。

---

## 十一、驗收標準核查表

- [ ] 品質評分可從 EvolutionEvents JSONL 自動計算（`score_trajectories_v2()`）
- [ ] Intent Category 分類規則文件化（本文件 §五）
- [ ] 人工抽樣準確率驗核 ≥ 85%（QA 前待準備 50 筆標注測試集）
- [ ] 與 W19-P0 Pipeline 完整整合（Phase 1a/1b/4 插入）
- [ ] `skill_bank_feedback` 反饋閉環實作（feedback_loop.rs）
- [ ] 設計文件更新至 wiki ✅（本文件）
- [ ] 單元測試覆蓋率 ≥ 80%（33 tests 計畫）

---

## 參照

- [W19-P0 Skill Synthesis Pipeline 設計](./skill-synthesis-pipeline-design.md)
- [EvolutionEvents 規格 v1.0](../specs/evolution-events-spec-v1.md)（§3 intent_category 語義）
- [ADR-005 Recursive Drift 防護](../decisions/adr-005-evolution-feedback-source.md)（feedback_source + 權重設計）
- [EvoAgent 競品分析](../competitive/evo-agent-analysis.md)（intent_category 設計靈感）

---

*本文件由 duduclaw-eng-memory（ENG-MEMORY）撰寫，W19-P1 技術設計依據*
*2026-04-29：初版發布，含 quality_scorer_v2、intent_classifier、feedback_loop 完整 Rust 設計*
