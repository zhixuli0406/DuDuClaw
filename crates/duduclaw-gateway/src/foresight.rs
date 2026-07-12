//! Early failure warning from a trajectory *prefix* (R2).
//!
//! Inspired by *AgentForesight* (arXiv:2605.08715) and its weak-supervision
//! variant (arXiv:2606.05414): predict that a run is heading toward failure
//! **before it finishes**, from prefix features only. DuDuClaw's cut is —
//! per platform philosophy — deterministic and zero-LLM-cost: a
//! [`ForesightScorer`] updates a 0–100 `ForesightScore` incrementally per
//! stream-json event from four prefix features:
//!
//! 1. **Early repeat ratio** — the same `(tool, normalized-input)` hammered
//!    inside the first `prefix_steps` tool starts.
//! 2. **Tool-error density** — fraction of `tool_result` blocks with
//!    `is_error: true` among the first `prefix_steps` results.
//! 3. **Cost slope** — cumulative spend rate vs. a configured per-minute
//!    baseline, biased by a per-agent historical prior read from
//!    `channel_failures.jsonl` (agents with recent failures alarm earlier —
//!    the weak-supervision signal).
//! 4. **Todo-progress stagnation** — consecutive `TodoWrite` updates whose
//!    completed count does not increase.
//!
//! ## Two thresholds, zero enforcement
//! - `warning` → `tracing::warn!` + an Activity Feed row +
//!   a `foresight_alarm` record in `channel_failures.jsonl`.
//! - `critical` → additionally appends a `run.at_risk` event to
//!   `<home>/events.db`, which the gateway's events-db poll bridge re-emits
//!   as [`crate::autopilot_engine::AutopilotEvent::RunAtRisk`] on the
//!   tokio::broadcast bus — so operators write **autopilot rules** that
//!   decide the intervention (notify / delegate / run_skill).
//!
//! The scorer itself **never blocks or kills anything** (same report-only
//! doctrine as R1 `trajectory_guard`). Every failure path inside this
//! module is fail-safe: scorer/config/history errors yield *no alarm*,
//! never a blocked run.
//!
//! Config: `[foresight]` in `<home>/config.toml`; `enabled = false` is the
//! kill switch. Absent/malformed config ⇒ defaults.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;
use tracing::warn;

// ─── Config ─────────────────────────────────────────────────

/// Tunables for the foresight scorer (`[foresight]` in `config.toml`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForesightConfig {
    /// Master kill switch. `false` ⇒ every observe/check call is a no-op.
    pub enabled: bool,
    /// Prefix window: number of tool starts / tool results examined.
    pub prefix_steps: usize,
    /// Score at or above this emits a Warning alarm (before prior bias).
    pub warning_threshold: f64,
    /// Score at or above this emits a Critical alarm (before prior bias).
    pub critical_threshold: f64,
    /// Baseline spend rate per minute (same unit as
    /// `TokenUsage::estimated_cost_millicents`). The cost feature saturates
    /// at 4× this rate.
    pub cost_baseline_per_min: f64,
    /// Historical prior: look-back window over `channel_failures.jsonl`.
    pub history_window_hours: i64,
    /// Threshold reduction per recent recorded failure of this agent.
    pub history_bias_per_failure: f64,
    /// Cap on the total threshold reduction from history.
    pub history_bias_max: f64,
}

impl Default for ForesightConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prefix_steps: 20,
            warning_threshold: 45.0,
            critical_threshold: 75.0,
            cost_baseline_per_min: 5_000.0,
            history_window_hours: 24,
            history_bias_per_failure: 2.0,
            history_bias_max: 10.0,
        }
    }
}

impl ForesightConfig {
    /// Read `<home>/config.toml` `[foresight]`. Fail-safe: any error ⇒
    /// defaults (enabled, report-only).
    pub fn from_home(home_dir: &Path) -> Self {
        match std::fs::read_to_string(home_dir.join("config.toml")) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    /// Pure parser — unknown/malformed keys keep their defaults; out-of-range
    /// values are ignored so the scorer stays well-defined.
    pub fn parse(raw: &str) -> Self {
        let mut cfg = Self::default();
        let Ok(value) = raw.parse::<toml::Value>() else {
            return cfg;
        };
        let Some(t) = value.get("foresight").and_then(|v| v.as_table()) else {
            return cfg;
        };
        if let Some(v) = t.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = t.get("prefix_steps").and_then(|v| v.as_integer()) {
            if v >= 4 {
                cfg.prefix_steps = v as usize;
            }
        }
        if let Some(v) = t.get("warning_threshold").and_then(toml_f64) {
            if (1.0..=100.0).contains(&v) {
                cfg.warning_threshold = v;
            }
        }
        if let Some(v) = t.get("critical_threshold").and_then(toml_f64) {
            if (1.0..=100.0).contains(&v) {
                cfg.critical_threshold = v;
            }
        }
        if cfg.critical_threshold < cfg.warning_threshold {
            // Nonsensical ordering ⇒ restore both defaults (fail-safe).
            let d = Self::default();
            cfg.warning_threshold = d.warning_threshold;
            cfg.critical_threshold = d.critical_threshold;
        }
        if let Some(v) = t.get("cost_baseline_per_min").and_then(toml_f64) {
            if v > 0.0 {
                cfg.cost_baseline_per_min = v;
            }
        }
        if let Some(v) = t.get("history_window_hours").and_then(|v| v.as_integer()) {
            if v > 0 {
                cfg.history_window_hours = v;
            }
        }
        if let Some(v) = t.get("history_bias_per_failure").and_then(toml_f64) {
            if (0.0..=20.0).contains(&v) {
                cfg.history_bias_per_failure = v;
            }
        }
        if let Some(v) = t.get("history_bias_max").and_then(toml_f64) {
            if (0.0..=30.0).contains(&v) {
                cfg.history_bias_max = v;
            }
        }
        cfg
    }
}

fn toml_f64(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

// ─── Historical prior (weak supervision) ────────────────────

/// Bytes read from the tail of `channel_failures.jsonl` when computing the
/// per-agent prior. Bounds I/O regardless of file growth.
const HISTORY_TAIL_BYTES: u64 = 512 * 1024;

/// Count this agent's recorded `channel_reply_fallback` failures inside the
/// look-back window and convert them into a threshold-reduction bias.
///
/// Fail-safe: unreadable file / bad lines / bad timestamps ⇒ bias `0.0`.
/// Only the last [`HISTORY_TAIL_BYTES`] of the file are examined; the tail
/// is aligned to the next newline so no partial (or mid-UTF-8) line is
/// ever parsed.
pub fn load_history_bias(home_dir: &Path, agent: &str, cfg: &ForesightConfig) -> f64 {
    if !cfg.enabled || cfg.history_bias_per_failure <= 0.0 {
        return 0.0;
    }
    let path = home_dir.join("channel_failures.jsonl");
    let Ok(mut f) = std::fs::File::open(&path) else {
        return 0.0;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(HISTORY_TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return 0.0;
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return 0.0;
    }
    let text = String::from_utf8_lossy(&buf);
    // Skip the (possibly partial) first line when we started mid-file.
    let body: &str = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => return 0.0,
        }
    } else {
        &text
    };
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(cfg.history_window_hours);
    let mut count: usize = 0;
    for line in body.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if rec.get("event").and_then(|v| v.as_str()) != Some("channel_reply_fallback") {
            continue;
        }
        if rec.get("agent").and_then(|v| v.as_str()) != Some(agent) {
            continue;
        }
        let in_window = rec
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .is_some_and(|dt| dt.with_timezone(&chrono::Utc) >= cutoff);
        if in_window {
            count += 1;
        }
    }
    (count as f64 * cfg.history_bias_per_failure).min(cfg.history_bias_max)
}

// ─── Alarm types ────────────────────────────────────────────

/// Alarm severity. Each level fires at most once per run (latched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ForesightLevel {
    Warning,
    Critical,
}

impl ForesightLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ForesightLevel::Warning => "warning",
            ForesightLevel::Critical => "critical",
        }
    }
}

/// One emitted alarm: the score at emission time plus the dominant
/// contributing features (zh-TW, human-readable).
#[derive(Debug, Clone, PartialEq)]
pub struct ForesightAlarm {
    pub level: ForesightLevel,
    pub score: f64,
    pub reasons: Vec<String>,
}

// ─── Scorer ─────────────────────────────────────────────────

/// Feature weights (must sum to 100 so the score is 0–100).
const W_REPEAT: f64 = 30.0;
const W_ERR: f64 = 25.0;
const W_COST: f64 = 25.0;
const W_TODO: f64 = 20.0;

/// Minimum tool starts before the repeat feature contributes (noise floor).
const MIN_STARTS_FOR_REPEAT: usize = 4;
/// Minimum tool results before the error-density feature contributes.
const MIN_RESULTS_FOR_ERR: usize = 3;
/// Stagnant `TodoWrite` updates that saturate the todo feature.
const TODO_STAGNATION_SATURATION: f64 = 4.0;
/// Cost slope (as a multiple of baseline) that saturates the cost feature.
const COST_SATURATION_MULTIPLIER: f64 = 4.0;
/// Effective thresholds never drop below this, whatever the history bias.
const MIN_EFFECTIVE_THRESHOLD: f64 = 10.0;
/// Cap on normalized-input length retained per start (chars, CJK-safe).
const INPUT_KEY_MAX_CHARS: usize = 200;

/// Incremental, deterministic prefix scorer. Feed it raw stream-json events
/// (`observe_event`) and cumulative cost samples (`observe_cost`), then ask
/// `check()` whether a new alarm crossed a threshold.
#[derive(Debug)]
pub struct ForesightScorer {
    cfg: ForesightConfig,
    /// Threshold reduction from the per-agent failure history.
    prior_bias: f64,
    /// `(tool, normalized input)` of the first `prefix_steps` tool starts.
    starts: Vec<(String, String)>,
    results_seen: u32,
    results_err: u32,
    todo_updates: u32,
    todo_stagnant_run: u32,
    last_todo_completed: Option<usize>,
    first_cost: Option<(u64, u64)>,
    last_cost: Option<(u64, u64)>,
    /// 0 = nothing emitted, 1 = warning emitted, 2 = critical emitted.
    emitted: u8,
}

impl ForesightScorer {
    pub fn new(cfg: ForesightConfig, prior_bias: f64) -> Self {
        let prior_bias = if prior_bias.is_finite() && prior_bias >= 0.0 {
            prior_bias.min(cfg.history_bias_max)
        } else {
            0.0
        };
        Self {
            cfg,
            prior_bias,
            starts: Vec::new(),
            results_seen: 0,
            results_err: 0,
            todo_updates: 0,
            todo_stagnant_run: 0,
            last_todo_completed: None,
            first_cost: None,
            last_cost: None,
            emitted: 0,
        }
    }

    /// Build from `<home>/config.toml` + the agent's failure history.
    /// Every failure path is fail-safe (defaults / zero bias).
    pub fn from_home(home_dir: &Path, agent: &str) -> Self {
        let cfg = ForesightConfig::from_home(home_dir);
        let bias = load_history_bias(home_dir, agent, &cfg);
        Self::new(cfg, bias)
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Ingest one raw stream-json event. Extracts `tool_use` starts (incl.
    /// `TodoWrite` progress) from `assistant` events and `tool_result`
    /// error flags from `user` events. Unknown shapes are ignored.
    pub fn observe_event(&mut self, event: &Value) {
        if !self.cfg.enabled {
            return;
        }
        match event.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array())
                else {
                    return;
                };
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        continue;
                    }
                    let tool = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    if tool == "TodoWrite" {
                        self.observe_todo(&input);
                    }
                    if self.starts.len() < self.cfg.prefix_steps {
                        self.starts
                            .push((tool.to_string(), normalize_input(&input)));
                    }
                }
            }
            Some("user") => {
                let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array())
                else {
                    return;
                };
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    if self.results_seen as usize >= self.cfg.prefix_steps {
                        continue;
                    }
                    self.results_seen += 1;
                    if block
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        self.results_err += 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn observe_todo(&mut self, input: &Value) {
        let Some(todos) = input.get("todos").and_then(|t| t.as_array()) else {
            return;
        };
        let completed = todos
            .iter()
            .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed"))
            .count();
        self.todo_updates += 1;
        if let Some(prev) = self.last_todo_completed {
            if completed <= prev {
                self.todo_stagnant_run += 1;
            } else {
                self.todo_stagnant_run = 0;
            }
        }
        self.last_todo_completed = Some(completed);
    }

    /// Ingest one cumulative-cost sample (unix-ms timestamp, monotonic
    /// cumulative cost in millicents).
    pub fn observe_cost(&mut self, ts_ms: u64, cumulative: u64) {
        if !self.cfg.enabled {
            return;
        }
        if self.first_cost.is_none() {
            self.first_cost = Some((ts_ms, cumulative));
        }
        self.last_cost = Some((ts_ms, cumulative));
    }

    /// Normalized features, each in `[0, 1]`, ordered
    /// `(repeat, err, cost, todo)`.
    fn features(&self) -> (f64, f64, f64, f64) {
        let f_repeat = if self.starts.len() >= MIN_STARTS_FOR_REPEAT {
            let mut counts: HashMap<&(String, String), usize> = HashMap::new();
            let mut max_c: usize = 0;
            for key in &self.starts {
                let c = counts.entry(key).or_insert(0);
                *c += 1;
                max_c = max_c.max(*c);
            }
            if max_c >= 2 && self.starts.len() > 1 {
                (max_c - 1) as f64 / (self.starts.len() - 1) as f64
            } else {
                0.0
            }
        } else {
            0.0
        };

        let f_err = if self.results_seen >= MIN_RESULTS_FOR_ERR as u32 {
            f64::from(self.results_err) / f64::from(self.results_seen)
        } else {
            0.0
        };

        let f_cost = match (self.first_cost, self.last_cost) {
            (Some((t0, c0)), Some((t1, c1))) if t1 > t0 && self.cfg.cost_baseline_per_min > 0.0 => {
                let minutes = (t1 - t0) as f64 / 60_000.0;
                let slope = c1.saturating_sub(c0) as f64 / minutes;
                (slope / (self.cfg.cost_baseline_per_min * COST_SATURATION_MULTIPLIER))
                    .clamp(0.0, 1.0)
            }
            _ => 0.0,
        };

        let f_todo = if self.todo_updates >= 2 {
            (f64::from(self.todo_stagnant_run) / TODO_STAGNATION_SATURATION).clamp(0.0, 1.0)
        } else {
            0.0
        };

        (f_repeat, f_err, f_cost, f_todo)
    }

    /// Current foresight score in `[0, 100]`.
    pub fn score(&self) -> f64 {
        let (r, e, c, t) = self.features();
        (W_REPEAT * r + W_ERR * e + W_COST * c + W_TODO * t).clamp(0.0, 100.0)
    }

    /// Threshold check with per-level latching. Returns at most one *new*
    /// alarm; a Critical emission supersedes (and suppresses) any further
    /// alarms for the run. Disabled scorer always returns `None`.
    pub fn check(&mut self) -> Option<ForesightAlarm> {
        if !self.cfg.enabled || self.emitted >= 2 {
            return None;
        }
        let score = self.score();
        let warn_at = (self.cfg.warning_threshold - self.prior_bias).max(MIN_EFFECTIVE_THRESHOLD);
        let crit_at = (self.cfg.critical_threshold - self.prior_bias).max(MIN_EFFECTIVE_THRESHOLD);
        if score >= crit_at {
            self.emitted = 2;
            return Some(ForesightAlarm {
                level: ForesightLevel::Critical,
                score,
                reasons: self.top_reasons(),
            });
        }
        if score >= warn_at && self.emitted < 1 {
            self.emitted = 1;
            return Some(ForesightAlarm {
                level: ForesightLevel::Warning,
                score,
                reasons: self.top_reasons(),
            });
        }
        None
    }

    /// Up to three dominant features, by weighted contribution (desc),
    /// rendered as short zh-TW explanations. Deterministic tie-break by
    /// fixed feature order.
    fn top_reasons(&self) -> Vec<String> {
        let (r, e, c, t) = self.features();
        let mut parts: Vec<(f64, String)> = vec![
            (
                W_REPEAT * r,
                format!("前綴內重複工具呼叫比例 {:.0}%", r * 100.0),
            ),
            (W_ERR * e, format!("工具錯誤密度 {:.0}%", e * 100.0)),
            (
                W_COST * c,
                format!("成本斜率達基線飽和值的 {:.0}%", c * 100.0),
            ),
            (
                W_TODO * t,
                format!("Todo 進度停滯 {} 次未推進", self.todo_stagnant_run),
            ),
        ];
        // Stable sort keeps the fixed feature order on ties.
        parts.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        parts
            .into_iter()
            .filter(|(w, _)| *w > 0.0)
            .take(3)
            .map(|(_, s)| s)
            .collect()
    }
}

/// Lowercase + collapse whitespace over the serialized tool input, capped at
/// [`INPUT_KEY_MAX_CHARS`] chars (CJK-safe — no byte slicing).
fn normalize_input(input: &Value) -> String {
    let raw = match input {
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let collapsed = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    duduclaw_core::truncate_chars(&collapsed, INPUT_KEY_MAX_CHARS)
}

// ─── Emission ───────────────────────────────────────────────

/// Build the `channel_failures.jsonl` record for an alarm. Pure.
pub fn alarm_record(agent: &str, session_id: &str, alarm: &ForesightAlarm) -> Value {
    serde_json::json!({
        "event": "foresight_alarm",
        "agent": agent,
        "session_id": session_id,
        "level": alarm.level.as_str(),
        "score": (alarm.score * 10.0).round() / 10.0,
        "reasons": alarm.reasons,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

/// Emit one alarm through every configured sink. **Never blocks the reply
/// path**: the JSONL append is best-effort, and the Activity Feed / events.db
/// writes run on detached tasks. Must be called from a tokio runtime.
///
/// - Warning + Critical: `tracing::warn!`, `channel_failures.jsonl` record,
///   Activity Feed row (`foresight_warning` / `foresight_critical`).
/// - Critical only: `run.at_risk` appended to `<home>/events.db` for the
///   autopilot bridge (→ `AutopilotEvent::RunAtRisk`).
pub fn emit_alarm(home_dir: &Path, agent: &str, session_id: &str, alarm: &ForesightAlarm) {
    warn!(
        agent = %agent,
        level = alarm.level.as_str(),
        score = alarm.score,
        reasons = ?alarm.reasons,
        "foresight: 軌跡前綴預測此輪任務有失敗風險（僅告警，不中止任務）"
    );

    let rec = alarm_record(agent, session_id, alarm);
    if let Err(e) = crate::trajectory_guard::append_anomaly(home_dir, &rec) {
        warn!(error = %e, "foresight: 寫入 channel_failures.jsonl 失敗");
    }

    // Activity Feed row — detached, fail-safe.
    {
        let home = home_dir.to_path_buf();
        let agent = agent.to_string();
        let session = session_id.to_string();
        let level = alarm.level;
        let score = alarm.score;
        let reasons = alarm.reasons.clone();
        tokio::spawn(async move {
            let store = match crate::task_store::TaskStore::open(&home) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "foresight: 開啟 task store 失敗（略過 Activity Feed）");
                    return;
                }
            };
            let row = crate::task_store::ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: match level {
                    ForesightLevel::Warning => "foresight_warning".into(),
                    ForesightLevel::Critical => "foresight_critical".into(),
                },
                agent_id: agent,
                task_id: None,
                summary: format!(
                    "任務失敗預警（{}）：風險分數 {:.0}/100 — {}",
                    match level {
                        ForesightLevel::Warning => "警告",
                        ForesightLevel::Critical => "嚴重",
                    },
                    score,
                    reasons.join("；")
                ),
                timestamp: chrono::Utc::now().to_rfc3339(),
                metadata: Some(
                    serde_json::json!({
                        "session_id": session,
                        "score": score,
                        "level": level.as_str(),
                    })
                    .to_string(),
                ),
            };
            if let Err(e) = store.append_activity(&row).await {
                warn!(error = %e, "foresight: Activity Feed 寫入失敗");
            }
        });
    }

    // Critical → run.at_risk onto the autopilot event bridge (events.db).
    if alarm.level == ForesightLevel::Critical {
        let home = home_dir.to_path_buf();
        let payload = serde_json::json!({
            "agent_id": agent,
            "session_id": session_id,
            "score": alarm.score,
            "level": alarm.level.as_str(),
            "reasons": alarm.reasons,
        })
        .to_string();
        tokio::spawn(async move {
            match crate::events_store::EventBusStore::open(&home) {
                Ok(store) => {
                    if let Err(e) = store.append("run.at_risk", &payload).await {
                        warn!(error = %e, "foresight: run.at_risk 事件寫入失敗");
                    }
                }
                Err(e) => warn!(error = %e, "foresight: 開啟 events.db 失敗（略過 run.at_risk）"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use_event(tool: &str, input: Value) -> Value {
        serde_json::json!({
            "type": "assistant",
            "message": { "content": [ { "type": "tool_use", "name": tool, "input": input } ] }
        })
    }

    fn tool_result_event(is_error: bool) -> Value {
        serde_json::json!({
            "type": "user",
            "message": { "content": [ { "type": "tool_result", "tool_use_id": "x", "is_error": is_error } ] }
        })
    }

    fn todo_event(completed: usize, pending: usize) -> Value {
        let mut todos = Vec::new();
        for _ in 0..completed {
            todos.push(serde_json::json!({"content": "c", "status": "completed"}));
        }
        for _ in 0..pending {
            todos.push(serde_json::json!({"content": "p", "status": "pending"}));
        }
        tool_use_event("TodoWrite", serde_json::json!({ "todos": todos }))
    }

    // ── Config ──────────────────────────────────────────

    #[test]
    fn config_defaults_and_kill_switch() {
        let d = ForesightConfig::default();
        assert!(d.enabled);
        assert!(d.warning_threshold < d.critical_threshold);

        let cfg = ForesightConfig::parse("[foresight]\nenabled = false\n");
        assert!(!cfg.enabled);
    }

    #[test]
    fn config_malformed_and_out_of_range_fall_back() {
        assert_eq!(
            ForesightConfig::parse("not [[ toml"),
            ForesightConfig::default()
        );
        let cfg = ForesightConfig::parse(
            "[foresight]\nwarning_threshold = 500\nprefix_steps = 1\ncost_baseline_per_min = -3\n",
        );
        assert_eq!(cfg, ForesightConfig::default());
    }

    #[test]
    fn config_inverted_thresholds_reset_to_default() {
        let cfg = ForesightConfig::parse(
            "[foresight]\nwarning_threshold = 80\ncritical_threshold = 20\n",
        );
        let d = ForesightConfig::default();
        assert_eq!(cfg.warning_threshold, d.warning_threshold);
        assert_eq!(cfg.critical_threshold, d.critical_threshold);
    }

    #[test]
    fn config_partial_override() {
        let cfg =
            ForesightConfig::parse("[foresight]\nwarning_threshold = 30\nprefix_steps = 10\n");
        assert_eq!(cfg.warning_threshold, 30.0);
        assert_eq!(cfg.prefix_steps, 10);
        assert_eq!(
            cfg.critical_threshold,
            ForesightConfig::default().critical_threshold
        );
    }

    // ── Scorer features ─────────────────────────────────

    #[test]
    fn disabled_scorer_is_noop() {
        let cfg = ForesightConfig {
            enabled: false,
            ..Default::default()
        };
        let mut s = ForesightScorer::new(cfg, 0.0);
        for _ in 0..30 {
            s.observe_event(&tool_use_event("Bash", serde_json::json!({"c": "curl x"})));
            s.observe_event(&tool_result_event(true));
        }
        assert_eq!(s.score(), 0.0);
        assert!(s.check().is_none());
    }

    #[test]
    fn benign_varied_trajectory_scores_low_and_never_alarms() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        for (i, tool) in ["Read", "Grep", "Edit", "Bash", "Write", "Read2"]
            .iter()
            .enumerate()
        {
            s.observe_event(&tool_use_event(tool, serde_json::json!({ "i": i })));
            s.observe_event(&tool_result_event(false));
        }
        assert!(s.score() < 10.0, "score={}", s.score());
        assert!(s.check().is_none());
    }

    #[test]
    fn heavy_repeat_plus_errors_reach_critical() {
        // prefix_steps = 12 so the 12 identical Bash starts saturate the
        // repeat feature; the later TodoWrite events still feed the todo
        // feature but no longer enter the (full) start prefix.
        let cfg = ForesightConfig {
            prefix_steps: 12,
            ..Default::default()
        };
        let mut s = ForesightScorer::new(cfg, 0.0);
        for _ in 0..12 {
            s.observe_event(&tool_use_event(
                "Bash",
                serde_json::json!({"cmd": "curl x"}),
            ));
            s.observe_event(&tool_result_event(true));
        }
        // repeat 1.0 (30) + error density 1.0 (25) = 55 ⇒ warning band...
        // add todo stagnation (20) to cross critical (75).
        s.observe_event(&todo_event(1, 3));
        for _ in 0..5 {
            s.observe_event(&todo_event(1, 3));
        }
        let score = s.score();
        assert!(score >= 75.0, "score={score}");
        let alarm = s.check().expect("critical alarm");
        assert_eq!(alarm.level, ForesightLevel::Critical);
        assert!(!alarm.reasons.is_empty() && alarm.reasons.len() <= 3);
        // Latched — no further alarms.
        assert!(s.check().is_none());
    }

    #[test]
    fn warning_latches_then_escalates_to_critical_once() {
        let cfg = ForesightConfig {
            warning_threshold: 20.0,
            critical_threshold: 50.0,
            ..Default::default()
        };
        let mut s = ForesightScorer::new(cfg, 0.0);
        // Repeats only → repeat feature ≈ 30 ⇒ warning band.
        for _ in 0..8 {
            s.observe_event(&tool_use_event("Bash", serde_json::json!({"cmd": "x"})));
        }
        let a1 = s.check().expect("warning");
        assert_eq!(a1.level, ForesightLevel::Warning);
        assert!(s.check().is_none(), "warning latched");
        // Now add errors → cross critical.
        for _ in 0..6 {
            s.observe_event(&tool_result_event(true));
        }
        let a2 = s.check().expect("critical");
        assert_eq!(a2.level, ForesightLevel::Critical);
        assert!(s.check().is_none());
    }

    #[test]
    fn error_density_needs_minimum_samples() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        s.observe_event(&tool_result_event(true));
        s.observe_event(&tool_result_event(true));
        assert_eq!(s.score(), 0.0, "below MIN_RESULTS_FOR_ERR");
        s.observe_event(&tool_result_event(true));
        assert!(s.score() > 0.0);
    }

    #[test]
    fn repeat_only_counts_prefix_window() {
        let cfg = ForesightConfig {
            prefix_steps: 5,
            ..Default::default()
        };
        let mut s = ForesightScorer::new(cfg, 0.0);
        // 5 varied starts fill the prefix; later repeats must not change it.
        for tool in ["A", "B", "C", "D", "E"] {
            s.observe_event(&tool_use_event(tool, Value::Null));
        }
        let before = s.score();
        for _ in 0..20 {
            s.observe_event(&tool_use_event("Bash", serde_json::json!({"cmd": "x"})));
        }
        assert_eq!(s.score(), before, "post-prefix starts are ignored");
    }

    #[test]
    fn cost_slope_feature_saturates() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        s.observe_cost(0, 0);
        // 100k/min vs baseline 5k → far past 4× saturation ⇒ f_cost = 1.
        s.observe_cost(60_000, 100_000);
        let (_, _, f_cost, _) = s.features();
        assert_eq!(f_cost, 1.0);
        assert!((s.score() - W_COST).abs() < 1e-9);
    }

    #[test]
    fn cost_zero_time_delta_is_safe() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        s.observe_cost(500, 0);
        s.observe_cost(500, 9_999_999);
        assert_eq!(s.score(), 0.0);
    }

    #[test]
    fn todo_progress_resets_stagnation() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        s.observe_event(&todo_event(0, 4));
        s.observe_event(&todo_event(0, 4)); // stagnant 1
        s.observe_event(&todo_event(0, 4)); // stagnant 2
        assert!(s.todo_stagnant_run == 2);
        s.observe_event(&todo_event(1, 3)); // progress → reset
        assert_eq!(s.todo_stagnant_run, 0);
    }

    #[test]
    fn prior_bias_lowers_thresholds_but_is_floored() {
        let cfg = ForesightConfig {
            warning_threshold: 32.0,
            critical_threshold: 90.0,
            history_bias_max: 10.0,
            ..Default::default()
        };
        // Score from pure repeats ≈ 30 < 32, but bias 5 lowers warning to 27.
        let mut s = ForesightScorer::new(cfg.clone(), 5.0);
        for _ in 0..8 {
            s.observe_event(&tool_use_event("Bash", serde_json::json!({"cmd": "x"})));
        }
        let alarm = s.check().expect("bias-lowered warning fires");
        assert_eq!(alarm.level, ForesightLevel::Warning);

        // Bias is clamped to history_bias_max and non-finite is rejected.
        let s2 = ForesightScorer::new(cfg.clone(), 999.0);
        assert!(s2.prior_bias <= cfg.history_bias_max);
        let s3 = ForesightScorer::new(cfg, f64::NAN);
        assert_eq!(s3.prior_bias, 0.0);
    }

    #[test]
    fn malformed_events_are_ignored() {
        let mut s = ForesightScorer::new(ForesightConfig::default(), 0.0);
        s.observe_event(&serde_json::json!("just a string"));
        s.observe_event(&serde_json::json!({"type": "assistant"}));
        s.observe_event(&serde_json::json!({"type": "user", "message": {"content": "text"}}));
        s.observe_event(&tool_use_event(
            "TodoWrite",
            serde_json::json!({"todos": "oops"}),
        ));
        assert_eq!(s.score(), 0.0);
        assert!(s.check().is_none());
    }

    #[test]
    fn normalize_input_is_cjk_safe_and_collapses() {
        let v = serde_json::json!({"q": "查詢  台北   天氣"});
        let n = normalize_input(&v);
        assert!(n.contains("查詢 台北 天氣"));
        // Very long CJK input must not panic on the char cap.
        let long = "繁".repeat(1000);
        let n2 = normalize_input(&serde_json::json!({ "q": long }));
        assert!(n2.chars().count() <= INPUT_KEY_MAX_CHARS);
    }

    // ── History prior ───────────────────────────────────

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("dudu-foresight-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn history_bias_counts_recent_agent_failures_only() {
        let home = fresh_home();
        let cfg = ForesightConfig::default();
        let now = chrono::Utc::now().to_rfc3339();
        let old = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        let lines = [
            format!(r#"{{"event":"channel_reply_fallback","agent":"agnes","timestamp":"{now}"}}"#),
            format!(r#"{{"event":"channel_reply_fallback","agent":"agnes","timestamp":"{now}"}}"#),
            format!(r#"{{"event":"channel_reply_fallback","agent":"other","timestamp":"{now}"}}"#),
            format!(r#"{{"event":"channel_reply_fallback","agent":"agnes","timestamp":"{old}"}}"#),
            format!(r#"{{"event":"trajectory_anomaly","agent":"agnes","timestamp":"{now}"}}"#),
            "not json at all".to_string(),
        ];
        std::fs::write(home.join("channel_failures.jsonl"), lines.join("\n") + "\n").unwrap();
        let bias = load_history_bias(&home, "agnes", &cfg);
        assert_eq!(bias, 2.0 * cfg.history_bias_per_failure);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn history_bias_missing_file_is_zero() {
        let home = fresh_home();
        assert_eq!(
            load_history_bias(&home, "agnes", &ForesightConfig::default()),
            0.0
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn history_bias_is_capped() {
        let home = fresh_home();
        let cfg = ForesightConfig::default();
        let now = chrono::Utc::now().to_rfc3339();
        let line =
            format!(r#"{{"event":"channel_reply_fallback","agent":"agnes","timestamp":"{now}"}}"#);
        let body = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        std::fs::write(home.join("channel_failures.jsonl"), body + "\n").unwrap();
        assert_eq!(
            load_history_bias(&home, "agnes", &cfg),
            cfg.history_bias_max
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // ── Records ─────────────────────────────────────────

    #[test]
    fn alarm_record_shape() {
        let alarm = ForesightAlarm {
            level: ForesightLevel::Critical,
            score: 81.25,
            reasons: vec!["工具錯誤密度 100%".into()],
        };
        let rec = alarm_record("agnes", "sess-1", &alarm);
        assert_eq!(rec["event"], "foresight_alarm");
        assert_eq!(rec["agent"], "agnes");
        assert_eq!(rec["level"], "critical");
        assert_eq!(rec["score"], 81.3);
        assert!(rec["reasons"].as_array().unwrap().len() == 1);
    }
}
