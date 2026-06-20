---
title: "W19-P0 Rollout-to-Skill 自動抽取 Pipeline — 技術設計文件"
created: 2026-04-25T00:00:00Z
updated: 2026-04-27T00:00:00Z
author: duduclaw-eng-agent
status: in_progress
tags: [w19, p0, skill-synthesis, cosplay, evolution-events, pipeline]
layer: engineering
trust: 0.9
task_id: a0fbe561-2798-4e42-9e24-eec0026fad52
---

# W19-P0 Rollout-to-Skill 自動抽取 Pipeline

> **撰寫者**：duduclaw-eng-agent  
> **日期**：2026-04-25（最後更新：2026-04-27 — Phase 2 Blocker 解除，加入 ENG-INFRA 調查結果）  
> **Task ID**：`a0fbe561-2798-4e42-9e24-eec0026fad52`  
> **技術借鑑**：COSPLAY 框架（arXiv:2604.20987）

---

## 一、設計目標

實現「任務軌跡 → 自動技能抽取 → Skill Bank 累積」閉環，讓 Skill Bank 從 0 筆技能自動成長。

**成功條件（2 週後）**：
- `skill_bank_search` ≥ 10 筆技能
- 安全掃描通過率 ≥ 95%
- Pipeline 失敗不影響主流程（非阻塞）
- 單元測試覆蓋率 ≥ 80%

---

## 二、架構概覽

```
EvolutionEvents JSONL
data/evolution/events/YYYY-MM-DD.jsonl
         │
         ▼
┌─────────────────────┐
│  Phase 1            │
│  Quality Scorer     │  篩選 gvu_generation/Success
│  quality_scorer.rs  │  計算品質分數，取 top 20%
└────────┬────────────┘
         │ Vec<TrajectoryScore>
         ▼
┌─────────────────────┐
│  Phase 2            │
│  Orchestrator       │  validate → memory_search → skill_extract
│  orchestrator.rs    │  → security_scan → skill_graduate
└────────┬────────────┘
         │ EvolutionEvent (skill_graduate)
         ▼
┌─────────────────────┐
│  Global Skill Bank  │
│  ~/.duduclaw/skills/│
└─────────────────────┘

觸發機制 (Phase 3 / trigger.rs):
  ├── CronJob：每 6 小時
  └── EpisodicPressure：> 10.0 時立即觸發
```

---

## 三、模組結構

```
crates/duduclaw-gateway/src/
├── evolution_events/
│   ├── schema.rs    ← 新增 SkillGraduate event_type（W19）
│   └── emitter.rs   ← 新增 emit_skill_graduate() method（W19）
└── skill_synthesis/   ← 全新模組（W19）
    ├── mod.rs
    ├── quality_scorer.rs
    ├── orchestrator.rs
    ├── pipeline.rs
    └── trigger.rs
```

---

## 四、Phase 1：品質評分器（quality_scorer.rs）

### 4.1 設計說明

- 讀取所有 `YYYY-MM-DD.jsonl` 文件
- 篩選 `event_type=gvu_generation` + `outcome=Success` + `skill_id != null`
- 按 `(agent_id, skill_id)` 分組
- 品質分數公式：`score = success_rate × 0.4 + effectiveness_delta × 0.35 + task_complexity × 0.25`
- 回傳 top 20%（至少 1 筆）

### 4.2 程式碼

```rust
// crates/duduclaw-gateway/src/skill_synthesis/quality_scorer.rs

use crate::evolution_events::schema::{AuditEvent, AuditEventType, Outcome};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
};

/// 單一 skill 軌跡的品質評分結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryScore {
    pub agent_id: String,
    pub skill_id: String,
    /// 綜合品質分數（0.0 ~ 1.0+）
    pub score: f64,
    pub success_rate: f64,
    pub effectiveness_delta: f64,
    pub task_complexity: f64,
    pub event_count: usize,
    pub window_start: String,
    pub window_end: String,
}

/// 讀取 JSONL、評分、回傳 top 20% 高品質軌跡
pub async fn score_trajectories(events_dir: &Path) -> Result<Vec<TrajectoryScore>> {
    let events = load_qualified_events(events_dir).await?;
    if events.is_empty() {
        return Ok(vec![]);
    }

    let grouped = group_by_skill(&events);
    let mut scores: Vec<TrajectoryScore> = grouped
        .into_iter()
        .map(|(key, evts)| compute_score(key, evts))
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

async fn load_qualified_events(events_dir: &Path) -> Result<Vec<AuditEvent>> {
    let mut result = Vec::new();

    let mut dir = fs::read_dir(events_dir)
        .await
        .with_context(|| format!("Cannot read evolution events dir: {}", events_dir.display()))?;

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
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AuditEvent>(&line) else {
            continue; // 跳過格式錯誤的行，不中斷整體流程
        };
        if event.event_type == AuditEventType::GvuGeneration
            && event.outcome == Outcome::Success
            && event.skill_id.is_some()
        {
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

fn compute_score(
    (agent_id, skill_id): (String, String),
    events: Vec<&AuditEvent>,
) -> TrajectoryScore {
    let event_count = events.len();
    let success_rate = 1.0_f64;

    let effectiveness_delta = {
        let deltas: Vec<f64> = events
            .iter()
            .filter_map(|e| {
                e.metadata
                    .as_ref()
                    .and_then(|m| m.get("effectiveness_score_delta"))
                    .and_then(|v| v.as_f64())
            })
            .collect();
        if deltas.is_empty() {
            0.0
        } else {
            deltas.iter().sum::<f64>() / deltas.len() as f64
        }
    };

    let distinct_triggers: HashSet<&str> = events
        .iter()
        .filter_map(|e| e.trigger_signal.as_deref())
        .collect();
    let task_complexity = (distinct_triggers.len() as f64 / 10.0).min(1.0);

    let score = success_rate * 0.4
        + effectiveness_delta.clamp(0.0, 1.0) * 0.35
        + task_complexity * 0.25;

    let timestamps: Vec<&str> = events.iter().map(|e| e.timestamp.as_str()).collect();
    let window_start = timestamps.iter().min().copied().unwrap_or("").to_string();
    let window_end = timestamps.iter().max().copied().unwrap_or("").to_string();

    TrajectoryScore {
        agent_id,
        skill_id,
        score,
        success_rate,
        effectiveness_delta,
        task_complexity,
        event_count,
        window_start,
        window_end,
    }
}
```

---

## 五、Phase 2：技能抽取 Orchestration（orchestrator.rs）

> **2026-04-27 更新**：根據 ENG-INFRA 任務 `b48469dd` 調查結果，補充：  
> (1) `validate_skill_name()` 前置格式驗證  
> (2) 三級錯誤分類處理策略  
> (3) 確認無 quota/rate limit，移除節流邏輯必要性

### 5.1 `skill_extract` 錯誤行為規範（ENG-INFRA 調查結果）

| 錯誤情境 | 錯誤訊息 | Orchestrator 處置 |
|---------|---------|-----------------|
| skill 文件不存在 | `Skill file not found: {path}` | 記錄 warning，跳過，繼續下一個 |
| skill 文件空白 | `Skill file is empty` | 記錄 warning，跳過，繼續下一個 |
| 無可提取知識 | `No extractable knowledge found in skill.` | 記錄 warning，跳過，繼續下一個 |

**重要**：上述所有情境**不進行 retry**，直接回報失敗原因並繼續。

### 5.2 `skill_name` 格式驗證規則（ENG-INFRA 調查結果）

`skill_extract` 驗證順序（由前往後，遇錯即停）：
1. 格式驗證 → `Invalid skill_name: use alphanumeric, hyphens, underscores only`
2. Agent 不存在 → `Agent '{agent_id}' does not exist`
3. Skill 文件不存在 → `Skill file not found: ...`

**設計決策**：格式錯誤屬呼叫端 bug → **在 Orchestrator 層做前置驗證**，避免無效 MCP 呼叫。

### 5.3 Quota / Rate Limit

**無 quota，無 rate limit。** `skill_extract` 為 heuristic 模式，純本地操作，Zero LLM cost。  
可安全批次呼叫，**無需節流邏輯**。

### 5.4 流程圖

```
TrajectoryScore
    │
    ├── 0. validate_skill_name()       // 前置格式驗證（Orchestrator 層，避免無效呼叫）
    │       └── 格式錯誤 → FormatError（記錄 error，中止，屬呼叫端 bug）
    │
    ├── 1. memory_search(query)        // 取得 episodic context（失敗不阻斷）
    │
    ├── 2. skill_extract(skill_name)   // Haiku 4.5 抽取（MCP tool）
    │       ├── Agent 不存在 → 記錄 warning，跳過整個 agent 的提取
    │       └── Skill 不存在/空白/無知識 → 記錄 warning，跳過，繼續下一個 skill
    │
    ├── 3. skill_security_scan()       // 安全閘門（必須通過）
    │       └── emit security_scan EvolutionEvent
    │
    └── 4. skill_graduate()            // 寫入全局 Skill Bank
            └── emit skill_graduate EvolutionEvent
```

### 5.5 McpHandler Trait（可 Mock 用於測試）

```rust
// orchestrator.rs — McpHandler trait

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// MCP 工具呼叫抽象層，便於單元測試時注入 Mock
#[async_trait]
pub trait McpHandler: Send + Sync {
    async fn skill_extract(&self, agent_id: &str, skill_name: &str) -> Result<Value>;
    async fn skill_security_scan(&self, agent_id: &str, skill_name: &str) -> Result<Value>;
    async fn skill_graduate(&self, agent_id: &str, skill_name: &str) -> Result<Value>;
    async fn memory_search(&self, query: &str) -> Result<Value>;
    async fn episodic_pressure(&self, hours_ago: Option<u32>) -> Result<f64>;
}
```

### 5.6 前置格式驗證（validate_skill_name）

```rust
// orchestrator.rs — validate_skill_name()

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    // alphanumeric, hyphens, underscores only（對應 skill_extract 格式規則）
    static ref SKILL_NAME_RE: Regex = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
}

/// 前置格式驗證：在 Orchestrator 層攔截格式錯誤，避免無效 MCP 呼叫
///
/// 對應 skill_extract 第 1 步驗證規則（ENG-INFRA 調查結果）：
/// `Invalid skill_name: use alphanumeric, hyphens, underscores only`
fn validate_skill_name(name: &str) -> Result<(), ExtractionError> {
    if name.is_empty() || !SKILL_NAME_RE.is_match(name) {
        return Err(ExtractionError::FormatError {
            skill_id: name.to_string(),
            reason: format!(
                "Invalid skill_name '{}': use alphanumeric, hyphens, underscores only",
                name
            ),
        });
    }
    Ok(())
}
```

### 5.7 ExtractionResult / ExtractionError 類型

```rust
/// Phase 2 單次 skill 抽取的結果
#[derive(Debug)]
pub enum ExtractionResult {
    /// 成功寫入 Skill Bank
    Graduated { skill_id: String },
    /// 安全閘門阻擋（`passed=false` 或 scan 拋錯）
    SecurityBlocked { skill_id: String, reason: String },
    /// 提取失敗（skill_extract 失敗、skill_graduate 失敗）
    ExtractionFailed { skill_id: String, reason: String },
    /// 格式錯誤（呼叫端 bug，不應 retry）
    FormatError { skill_id: String, reason: String },
    /// Agent 不存在（記錄 warning，跳過整個 agent）
    AgentNotFound { agent_id: String, skill_id: String },
}

/// 內部錯誤分類，對應 ENG-INFRA 定義的三種錯誤類型
#[derive(Debug, thiserror::Error)]
enum ExtractionError {
    #[error("Format error for skill '{skill_id}': {reason}")]
    FormatError { skill_id: String, reason: String },

    #[error("Agent '{agent_id}' not found")]
    AgentNotFound { agent_id: String },

    #[error("Skill file not found or empty for skill '{skill_id}'")]
    SkillNotFound { skill_id: String },
}
```

### 5.8 Orchestration 核心邏輯

```rust
// orchestrator.rs — extract_and_graduate()
//
// 錯誤處理策略（ENG-INFRA b48469dd 調查結果）：
//   格式錯誤  → FormatError，不 retry，屬呼叫端 bug
//   Agent 不存在 → AgentNotFound，記錄 warning，跳過整個 agent
//   Skill 不存在/空白/無知識 → ExtractionFailed，記錄 warning，繼續下一個 skill

use crate::{
    evolution_events::{emitter::EvolutionEventEmitter, schema::Outcome},
    skill_synthesis::quality_scorer::TrajectoryScore,
};
use serde_json::json;
use tracing::{error, info, warn};

/// Phase 2 主流程
pub async fn extract_and_graduate(
    trajectory: &TrajectoryScore,
    handler: &dyn McpHandler,
    emitter: &EvolutionEventEmitter,
) -> ExtractionResult {
    let agent_id = &trajectory.agent_id;
    let skill_name = &trajectory.skill_id;

    // Step 0: 前置格式驗證（Orchestrator 層，避免無效 MCP 呼叫）
    if let Err(ExtractionError::FormatError { reason, .. }) = validate_skill_name(skill_name) {
        error!(
            skill_name = %skill_name,
            reason = %reason,
            "BUG: Invalid skill_name passed to orchestrator — this is a caller bug"
        );
        return ExtractionResult::FormatError {
            skill_id: skill_name.clone(),
            reason,
        };
    }

    info!(
        agent_id = %agent_id,
        skill_name = %skill_name,
        score = trajectory.score,
        "Starting extraction for skill"
    );

    // Step 1: 取得 episodic memory context（失敗不阻斷流程）
    let memory_hint = match handler
        .memory_search(&format!(
            "skill:{} agent:{} {} {}",
            skill_name, agent_id, trajectory.window_start, trajectory.window_end
        ))
        .await
    {
        Ok(v) => v.to_string(),
        Err(e) => {
            warn!(
                skill_name = %skill_name,
                error = %e,
                "memory_search failed — proceeding without episodic context"
            );
            String::new()
        }
    };

    // Step 2: skill_extract（無 rate limit，可直接呼叫）
    let extract_result = match handler.skill_extract(agent_id, skill_name).await {
        Ok(r) => r,
        Err(e) => {
            let err_msg = e.to_string();

            // 根據 ENG-INFRA 調查：Agent 不存在 → 跳過整個 agent
            if err_msg.contains("does not exist") {
                warn!(
                    agent_id = %agent_id,
                    skill_name = %skill_name,
                    "Agent not found — skipping all skills for this agent"
                );
                return ExtractionResult::AgentNotFound {
                    agent_id: agent_id.clone(),
                    skill_id: skill_name.clone(),
                };
            }

            // Skill 不存在 / 空白 / 無可提取知識 → warning，繼續下一個 skill
            warn!(
                skill_name = %skill_name,
                error = %err_msg,
                "skill_extract failed — skipping this skill"
            );
            return ExtractionResult::ExtractionFailed {
                skill_id: skill_name.clone(),
                reason: format!("skill_extract: {err_msg}"),
            };
        }
    };

    info!(
        skill_name = %skill_name,
        "skill_extract completed"
    );

    // Step 3: security_scan（強制通過才能繼續）
    let scan_result = match handler.skill_security_scan(agent_id, skill_name).await {
        Ok(r) => r,
        Err(e) => {
            emitter.emit_security_scan(
                agent_id,
                skill_name,
                Outcome::Failure,
                Some(json!({
                    "error": e.to_string(),
                    "memory_hint_len": memory_hint.len()
                })),
            );
            return ExtractionResult::SecurityBlocked {
                skill_id: skill_name.clone(),
                reason: format!("security_scan error: {e}"),
            };
        }
    };

    let passed = scan_result
        .get("passed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    emitter.emit_security_scan(
        agent_id,
        skill_name,
        if passed { Outcome::Success } else { Outcome::Failure },
        Some(scan_result.clone()),
    );

    if !passed {
        let reason = scan_result
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("security scan rejected")
            .to_string();
        warn!(
            skill_name = %skill_name,
            reason = %reason,
            "Skill blocked by security scan"
        );
        return ExtractionResult::SecurityBlocked {
            skill_id: skill_name.clone(),
            reason,
        };
    }

    // Step 4: skill_graduate — 寫入全局 Skill Bank
    match handler.skill_graduate(agent_id, skill_name).await {
        Ok(_) => {
            emitter.emit_skill_graduate(
                agent_id,
                skill_name,
                Outcome::Success,
                Some(json!({
                    "trajectory_score": trajectory.score,
                    "event_count": trajectory.event_count,
                    "window_start": trajectory.window_start,
                    "window_end": trajectory.window_end,
                })),
            );
            info!(skill_name = %skill_name, "Skill graduated to global Skill Bank ✅");
            ExtractionResult::Graduated {
                skill_id: skill_name.clone(),
            }
        }
        Err(e) => {
            emitter.emit_skill_graduate(
                agent_id,
                skill_name,
                Outcome::Failure,
                Some(json!({ "error": e.to_string() })),
            );
            error!(
                skill_name = %skill_name,
                error = %e,
                "skill_graduate failed"
            );
            ExtractionResult::ExtractionFailed {
                skill_id: skill_name.clone(),
                reason: format!("skill_graduate error: {e}"),
            }
        }
    }
}
```

---

## 六、Pipeline Driver（pipeline.rs）

> 注意：`skill_extract` 無 quota/rate limit，批次呼叫無需節流。

```rust
// crates/duduclaw-gateway/src/skill_synthesis/pipeline.rs

use crate::{
    evolution_events::emitter::EvolutionEventEmitter,
    skill_synthesis::{
        orchestrator::{extract_and_graduate, ExtractionResult, McpHandler},
        quality_scorer::score_trajectories,
    },
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
};
use tracing::{error, info, warn};

/// Pipeline 單次執行的統計結果
#[derive(Debug, Default, Clone)]
pub struct PipelineStats {
    pub trajectories_selected: usize,
    pub skills_graduated: usize,
    pub security_blocked: usize,
    pub extraction_failed: usize,
    pub format_errors: usize,
    pub agent_not_found: usize,
}

pub struct SkillSynthesisPipeline {
    events_dir: PathBuf,
    mcp_handler: Arc<dyn McpHandler>,
    emitter: Arc<EvolutionEventEmitter>,
}

impl SkillSynthesisPipeline {
    pub fn new(
        events_dir: PathBuf,
        mcp_handler: Arc<dyn McpHandler>,
        emitter: Arc<EvolutionEventEmitter>,
    ) -> Self {
        Self { events_dir, mcp_handler, emitter }
    }

    /// 執行一次完整的 Pipeline 週期。非阻塞：任何 phase 失敗只記錄 log。
    pub async fn run_once(&self) -> PipelineStats {
        let mut stats = PipelineStats::default();

        // Phase 1: 品質評分
        let top_trajectories = match score_trajectories(&self.events_dir).await {
            Ok(t) => t,
            Err(e) => {
                error!("Phase 1 quality_scorer failed: {}", e);
                return stats;
            }
        };

        stats.trajectories_selected = top_trajectories.len();
        info!(
            selected = stats.trajectories_selected,
            "Phase 1 complete — starting Phase 2 extraction"
        );

        // Phase 2: 批次抽取（無 rate limit，直接批次呼叫）
        // 追蹤「已跳過的 agent」，避免重複嘗試同一個不存在的 agent
        let mut skipped_agents: HashSet<String> = HashSet::new();

        for trajectory in &top_trajectories {
            // 若 agent 已知不存在，直接跳過
            if skipped_agents.contains(&trajectory.agent_id) {
                stats.agent_not_found += 1;
                continue;
            }

            let result = extract_and_graduate(
                trajectory,
                self.mcp_handler.as_ref(),
                self.emitter.as_ref(),
            )
            .await;

            match &result {
                ExtractionResult::Graduated { skill_id } => {
                    stats.skills_graduated += 1;
                    info!(skill_id = %skill_id, "✅ Graduated");
                }
                ExtractionResult::SecurityBlocked { skill_id, reason } => {
                    stats.security_blocked += 1;
                    warn!(skill_id = %skill_id, reason = %reason, "🔒 Security blocked");
                }
                ExtractionResult::ExtractionFailed { skill_id, reason } => {
                    stats.extraction_failed += 1;
                    warn!(skill_id = %skill_id, reason = %reason, "⚠️ Extraction failed — continuing");
                }
                ExtractionResult::FormatError { skill_id, reason } => {
                    stats.format_errors += 1;
                    // 格式錯誤屬呼叫端 bug → error 級別
                    error!(skill_id = %skill_id, reason = %reason, "❌ Format error — caller bug");
                }
                ExtractionResult::AgentNotFound { agent_id, skill_id } => {
                    stats.agent_not_found += 1;
                    // 記錄並標記，後續同 agent 的 skill 直接跳過
                    warn!(
                        agent_id = %agent_id,
                        skill_id = %skill_id,
                        "⚠️ Agent not found — skipping all skills for this agent"
                    );
                    skipped_agents.insert(agent_id.clone());
                }
            }
        }

        info!(
            graduated = stats.skills_graduated,
            blocked = stats.security_blocked,
            failed = stats.extraction_failed,
            format_err = stats.format_errors,
            agent_missing = stats.agent_not_found,
            "Pipeline cycle complete"
        );

        stats
    }
}
```

---

## 七、Phase 3：觸發機制（trigger.rs）

```rust
// crates/duduclaw-gateway/src/skill_synthesis/trigger.rs

use crate::skill_synthesis::pipeline::SkillSynthesisPipeline;
use std::{sync::Arc, time::Duration};
use tokio::time;
use tracing::{error, info};

const CRON_INTERVAL_SECS: u64 = 6 * 3600;     // 每 6 小時
const PRESSURE_CHECK_SECS: u64 = 300;          // 每 5 分鐘檢查
const PRESSURE_THRESHOLD: f64 = 10.0;          // > 10.0 立即觸發

pub fn start_triggers(pipeline: Arc<SkillSynthesisPipeline>) {
    // Trigger A: 6 小時 cron
    {
        let p = Arc::clone(&pipeline);
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(CRON_INTERVAL_SECS));
            interval.tick().await; // 跳過首次 tick
            loop {
                interval.tick().await;
                info!("[CronTrigger] Firing skill synthesis pipeline");
                let stats = p.run_once().await;
                info!(graduated = stats.skills_graduated, "[CronTrigger] Done");
            }
        });
    }

    // Trigger B: episodic pressure 監控
    {
        let p = Arc::clone(&pipeline);
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(PRESSURE_CHECK_SECS));
            loop {
                interval.tick().await;
                match p.mcp_handler.episodic_pressure(Some(24)).await {
                    Ok(pressure) if pressure > PRESSURE_THRESHOLD => {
                        info!(
                            pressure = pressure,
                            "[PressureTrigger] Threshold exceeded — firing pipeline"
                        );
                        let stats = p.run_once().await;
                        info!(graduated = stats.skills_graduated, "[PressureTrigger] Done");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!("[PressureTrigger] episodic_pressure check failed: {}", e);
                    }
                }
            }
        });
    }
}
```

---

## 八、模組入口（mod.rs）

```rust
pub mod orchestrator;
pub mod pipeline;
pub mod quality_scorer;
pub mod trigger;

pub use pipeline::{PipelineStats, SkillSynthesisPipeline};
pub use trigger::start_triggers;
```

---

## 九、EvolutionEvents Schema 擴充（W19 新增）

### 9.1 schema.rs — 新增 SkillGraduate 事件型別

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    SkillActivate,
    SkillDeactivate,
    SecurityScan,
    GvuGeneration,
    SignalSuppressed,
    SkillGraduate,   // W19 新增：技能從軌跡抽取後寫入 Skill Bank
}
```

**validate() 新增規則**：
- `skill_graduate` 事件中 `skill_id` 不得為 `null`
- `skill_graduate` 事件中 `outcome` 僅允許 `success` / `failure`

### 9.2 emitter.rs — 新增 emit_skill_graduate()

```rust
pub fn emit_skill_graduate(
    &self,
    agent_id: &str,
    skill_id: &str,
    outcome: Outcome,
    metadata: Option<serde_json::Value>,
) {
    let event = AuditEvent {
        timestamp: now_utc(),
        event_type: AuditEventType::SkillGraduate,
        agent_id: agent_id.to_string(),
        skill_id: Some(skill_id.to_string()),
        generation: None,
        outcome,
        trigger_signal: Some("skill_synthesis_pipeline".to_string()),
        metadata,
    };
    self.fire(event); // fire-and-forget，non-blocking
}
```

---

## 十、測試計畫（覆蓋率目標 ≥ 80%）

### 10.1 quality_scorer 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_score_empty_dir` | events 目錄為空 | 回傳 `Ok(vec![])` |
| `test_score_single_success_event` | 1 筆 gvu_generation/Success | 回傳 1 筆，score ≈ 0.4+ |
| `test_score_filters_non_gvu` | 混入 skill_activate 事件 | 只計 gvu_generation |
| `test_score_filters_failure` | gvu_generation/Failure | 不計入 |
| `test_score_top_20_percent` | 10 筆不同 skill | 回傳 top 2 筆（ceil(10×0.2)）|
| `test_score_top_20_minimum_one` | 3 筆 skill | 回傳至少 1 筆 |
| `test_score_with_effectiveness_delta` | metadata 含 delta | score 含 0.35 權重項 |
| `test_score_malformed_jsonl_skipped` | 部分 JSONL 格式錯誤 | 跳過錯誤行，不 panic |

### 10.2 orchestrator 測試（MockMcpHandler）

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_extract_success_full_flow` | 全部 MCP call 成功 | `Graduated`，emit 2 事件 |
| `test_extract_security_blocked` | `passed=false` | `SecurityBlocked`，emit security/Failure |
| `test_extract_skill_extract_fails_file_not_found` | `Skill file not found` 錯誤 | `ExtractionFailed`，記錄 warning |
| `test_extract_skill_extract_fails_empty` | `Skill file is empty` 錯誤 | `ExtractionFailed`，記錄 warning |
| `test_extract_skill_extract_fails_no_knowledge` | `No extractable knowledge` 錯誤 | `ExtractionFailed`，記錄 warning |
| `test_extract_agent_not_found` | 錯誤含 `does not exist` | `AgentNotFound`，記錄 warning |
| `test_extract_format_error_invalid_name` | skill_name 含特殊字元 | `FormatError`，不呼叫 MCP |
| `test_extract_format_error_empty_name` | skill_name 為空字串 | `FormatError`，不呼叫 MCP |
| `test_extract_memory_search_fails` | memory_search Err | 繼續流程，仍嘗試 skill_extract |
| `test_extract_graduate_fails` | skill_graduate Err | `ExtractionFailed`，emit skill_graduate/Failure |
| `test_extract_security_scan_errors` | security_scan 拋 Error | `SecurityBlocked`，emit security/Failure |

### 10.3 pipeline 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_pipeline_empty_events` | events 目錄無有效事件 | 不 panic，stats 全 0 |
| `test_pipeline_all_graduate` | 所有 skill 均畢業成功 | `stats.skills_graduated = N` |
| `test_pipeline_mixed_results` | 部分成功/阻擋/失敗 | 各計數正確 |
| `test_pipeline_phase1_error` | score_trajectories 失敗 | 靜默失敗，回傳空 stats |
| `test_pipeline_agent_not_found_skip_all` | agent 不存在時後續同 agent skill 直接跳過 | `skipped_agents` 生效 |
| `test_pipeline_no_rate_limit_needed` | 大量 skills（50+）批次呼叫 | 無節流，全部直接呼叫 |

### 10.4 trigger 測試

| 測試名稱 | 情境 | 驗收條件 |
|---------|------|---------|
| `test_pressure_trigger_fires_when_exceeded` | pressure = 15.0 | `run_once()` 被呼叫 |
| `test_pressure_trigger_no_fire_below_threshold` | pressure = 5.0 | `run_once()` 不被呼叫 |
| `test_pressure_check_error_not_panic` | episodic_pressure Err | 不 panic，繼續 loop |

### 10.5 schema 擴充測試

| 測試名稱 | 驗收條件 |
|---------|---------|
| `test_validate_skill_graduate_skill_id_required` | skill_id=null → validate() 拒絕 |
| `test_validate_skill_graduate_outcome_valid` | outcome=suppressed → validate() 拒絕 |

### 10.6 validate_skill_name 測試（新增）

| 測試名稱 | 驗收條件 |
|---------|---------|
| `test_validate_name_valid_alphanumeric` | `my-skill_v2` → Ok |
| `test_validate_name_empty` | `""` → Err |
| `test_validate_name_with_spaces` | `"my skill"` → Err |
| `test_validate_name_with_slash` | `"../evil"` → Err（path traversal 防護）|
| `test_validate_name_with_dot` | `"skill.name"` → Err |

---

## 十一、EvolutionEvents Spec 更新需求

> ⚠️ 需 TL 審核後正式更新 `specs/evolution-events-spec-v1.md` → v1.1

新增事件：

| event_type | 說明 | W19 狀態 |
|-----------|------|---------|
| `skill_graduate` | Skill 從軌跡抽取後成功/失敗寫入 Skill Bank | ✅ W19 新增 |

新增 validate() 規則（詳見 §九）。

---

## 十二、Sprint 點數估算（實作）

| 工作項目 | 點數 |
|---------|------|
| `quality_scorer.rs` + 8 tests | 2 SP |
| `orchestrator.rs`（含前置驗證、三級錯誤分類）+ 11 tests | 3 SP |
| `pipeline.rs` + `trigger.rs` + 9 tests | 2 SP |
| `schema.rs` + `emitter.rs` 擴充 + 7 tests | 1 SP |
| MCP gateway 整合（Real McpHandler impl） | 1 SP |
| **小計** | **9 SP** |

---

## 十三、Blockers 狀態

| # | 問題 | 狀態 | 解決方案 |
|---|------|------|---------|
| 1 | `skill_extract` 對不存在 skill_id 的行為 | ✅ **已解除**（2026-04-27，ENG-INFRA b48469dd）| 回傳 Error，不建立空 skill，錯誤優先順序已確認 |
| 2 | Spec v1.1 正式核准（`skill_graduate` event_type） | 🟡 待 TL/PM 核准 | 提案已就緒，待審核 |
| 3 | Real McpHandler 實作（與 `mcp.rs` 整合） | 🟡 待 infra 協助 | 依賴 gateway 內部 handler ref |

---

*本文件由 duduclaw-eng-agent 撰寫，W19-P0 Phase 2 實作依據*  
*2026-04-27：Phase 2 Blocker #1 解除，加入 ENG-INFRA 調查結果*
