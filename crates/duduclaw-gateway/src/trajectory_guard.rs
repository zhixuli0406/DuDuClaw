//! Lightweight, deterministic agent-trajectory anomaly detection (R1).
//!
//! Inspired by *Trajectory Guard* (arXiv:2601.00516), which uses a 32 ms
//! non-LLM sequence model to catch task-trajectory misalignment 17-27× faster
//! than an LLM judge. This module ships the **heuristic** first cut that fits
//! DuDuClaw's "deterministic, zero-LLM-cost" philosophy: a set of pure rules
//! over the structured tool-step stream ([`crate::channel_reply::StepEvent`])
//! plus a cost-slope signal. No model, no tokens, no new storage.
//!
//! ## What it catches
//! - **Repeated tool loop** — the same tool with a similar input hammered N
//!   times in a small window (the classic runaway-loop fingerprint).
//! - **Excessive depth** — outstanding (unfinished) tool calls nested past a
//!   threshold (a plan that keeps opening sub-work without closing any).
//! - **Cost-slope spike** — cumulative spend accelerating past a multiple of a
//!   configured per-minute baseline.
//! - **Trajectory stall** — many steps accumulate but they are overwhelmingly
//!   read-only (information gathering) with no productive output.
//!
//! ## Fail-safe, not fail-closed
//! The guard **never kills** a task. By default (`enabled = true`,
//! `intervene = false`) it only *reports* — high-severity signals are appended
//! to `channel_failures.jsonl` (with an `anomaly` classification) and logged.
//! Tripping an existing circuit breaker is an explicit operator opt-in
//! (`intervene = true`) and is expressed as a pure [`TrajectoryGuard::should_intervene`]
//! decision so the caller stays in control of the (reversible) action.
//!
//! Everything here is a pure function or a small deterministic state machine —
//! fully unit-tested, no wall-clock dependence except the timestamps the caller
//! supplies inside each event.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

// ─── Config ─────────────────────────────────────────────────

/// Tunables for the trajectory guard, read from `[trajectory_guard]` in
/// `<home>/config.toml`. Every field has a conservative default so an absent or
/// partial section still yields a working (report-only) guard.
#[derive(Debug, Clone, PartialEq)]
pub struct TrajectoryGuardConfig {
    /// Master switch. When `false`, every `observe_*` call is a no-op.
    pub enabled: bool,
    /// When `true`, a High-severity signal makes [`TrajectoryGuard::should_intervene`]
    /// return `true` so the caller may trip a circuit breaker. Default `false`
    /// (report-only): the guard alone never changes execution.
    pub intervene: bool,
    /// Number of most-recent tool *starts* examined for the repeated-loop rule.
    pub repeat_window: usize,
    /// Same `(tool, normalized-input)` occurring at least this many times inside
    /// `repeat_window` trips the repeated-loop rule.
    pub repeat_threshold: usize,
    /// Outstanding tool-call depth at or above this trips the depth rule.
    pub max_depth: usize,
    /// Baseline spend rate (in the same unit as
    /// [`crate::cost_telemetry::TokenUsage::estimated_cost_millicents`]) per
    /// minute. The slope rule compares the observed rate against this.
    pub cost_baseline_per_min: f64,
    /// Observed cost slope above `cost_baseline_per_min × this` trips the slope
    /// rule.
    pub cost_slope_multiplier: f64,
    /// Number of most-recent tool starts examined for the stall rule.
    pub stall_window: usize,
    /// Minimum starts observed before the stall rule may fire (avoids judging a
    /// short, legitimately read-heavy opening).
    pub stall_min_steps: usize,
    /// Read-only-tool ratio (over `stall_window`) at or above this trips stall.
    pub stall_readonly_ratio: f64,
}

impl Default for TrajectoryGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            intervene: false,
            repeat_window: 8,
            repeat_threshold: 4,
            max_depth: 6,
            cost_baseline_per_min: 5_000.0,
            cost_slope_multiplier: 5.0,
            stall_window: 12,
            stall_min_steps: 8,
            stall_readonly_ratio: 0.85,
        }
    }
}

impl TrajectoryGuardConfig {
    /// Read `<home>/config.toml` and parse the `[trajectory_guard]` section.
    /// Fail-safe: a missing file, malformed TOML, or absent section yields
    /// [`TrajectoryGuardConfig::default`] (enabled, report-only) — never a panic.
    pub fn from_home(home_dir: &Path) -> Self {
        match std::fs::read_to_string(home_dir.join("config.toml")) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    /// Pure parser over a TOML string — the unit-tested config core. Unknown or
    /// malformed keys fall back to their default; only the keys actually present
    /// override. A negative/zero where a positive is required is clamped up to
    /// the default to keep the detectors well-defined.
    pub fn parse(raw: &str) -> Self {
        let mut cfg = Self::default();
        let Ok(value) = raw.parse::<toml::Value>() else {
            return cfg;
        };
        let Some(t) = value.get("trajectory_guard").and_then(|v| v.as_table()) else {
            return cfg;
        };
        if let Some(v) = t.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = t.get("intervene").and_then(|v| v.as_bool()) {
            cfg.intervene = v;
        }
        if let Some(v) = t.get("repeat_window").and_then(toml_usize) {
            cfg.repeat_window = v.max(1);
        }
        if let Some(v) = t.get("repeat_threshold").and_then(toml_usize) {
            cfg.repeat_threshold = v.max(2);
        }
        if let Some(v) = t.get("max_depth").and_then(toml_usize) {
            cfg.max_depth = v.max(1);
        }
        if let Some(v) = t.get("cost_baseline_per_min").and_then(toml_f64) {
            if v > 0.0 {
                cfg.cost_baseline_per_min = v;
            }
        }
        if let Some(v) = t.get("cost_slope_multiplier").and_then(toml_f64) {
            if v > 1.0 {
                cfg.cost_slope_multiplier = v;
            }
        }
        if let Some(v) = t.get("stall_window").and_then(toml_usize) {
            cfg.stall_window = v.max(1);
        }
        if let Some(v) = t.get("stall_min_steps").and_then(toml_usize) {
            cfg.stall_min_steps = v.max(1);
        }
        if let Some(v) = t.get("stall_readonly_ratio").and_then(toml_f64) {
            if (0.0..=1.0).contains(&v) {
                cfg.stall_readonly_ratio = v;
            }
        }
        cfg
    }
}

fn toml_usize(v: &toml::Value) -> Option<usize> {
    v.as_integer()
        .and_then(|i| if i >= 0 { Some(i as usize) } else { None })
}

fn toml_f64(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

// ─── Signals ────────────────────────────────────────────────

/// Category of trajectory anomaly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnomalyKind {
    /// Same tool + similar input repeated within the window.
    RepeatedToolLoop,
    /// Outstanding tool-call nesting past the depth threshold.
    ExcessiveDepth,
    /// Cost accumulating faster than the baseline multiple.
    CostSlopeSpike,
    /// Many steps, overwhelmingly read-only, no productive output.
    TrajectoryStall,
}

impl AnomalyKind {
    /// Stable wire token for the `channel_failures.jsonl` record.
    pub fn as_str(self) -> &'static str {
        match self {
            AnomalyKind::RepeatedToolLoop => "repeated_tool_loop",
            AnomalyKind::ExcessiveDepth => "excessive_depth",
            AnomalyKind::CostSlopeSpike => "cost_slope_spike",
            AnomalyKind::TrajectoryStall => "trajectory_stall",
        }
    }
}

/// Severity of an anomaly signal. Ordered: `Low < Medium < High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    /// Monotonic rank used for signal de-duplication / escalation.
    fn rank(self) -> u8 {
        match self {
            Severity::Low => 1,
            Severity::Medium => 2,
            Severity::High => 3,
        }
    }

    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
        }
    }
}

/// One detected anomaly. `evidence` is a zh-TW human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnomalySignal {
    pub kind: AnomalyKind,
    pub severity: Severity,
    pub evidence: String,
}

// ─── Inputs ─────────────────────────────────────────────────

/// A single tool-step observation fed to the guard. Decoupled from
/// [`crate::channel_reply::StepEvent`] so the detectors are testable without
/// constructing stream-json events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStep {
    /// Tool name (e.g. `Bash`, `Read`).
    pub tool: String,
    /// CJK-safe args summary; `None` on an end boundary.
    pub summary: Option<String>,
    /// `true` = a tool started (`tool_use`); `false` = it ended (`tool_result`).
    pub phase_start: bool,
    /// Raw nesting depth from the step event (outstanding calls before a start /
    /// after an end).
    pub depth: usize,
}

impl From<&crate::channel_reply::StepEvent> for ToolStep {
    fn from(ev: &crate::channel_reply::StepEvent) -> Self {
        ToolStep {
            tool: ev.tool.clone(),
            summary: ev.summary.clone(),
            phase_start: matches!(ev.phase, crate::channel_reply::StepPhase::Start),
            depth: ev.depth,
        }
    }
}

/// A cumulative-cost sample fed to the guard. `cumulative` is in the same unit
/// as [`crate::cost_telemetry::TokenUsage::estimated_cost_millicents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostSample {
    /// Wall-clock timestamp, unix epoch milliseconds.
    pub ts_ms: u64,
    /// Cumulative cost so far (monotonic non-decreasing).
    pub cumulative: u64,
}

// ─── Helpers (pure) ─────────────────────────────────────────

/// Tools that gather information without producing a side effect / artifact.
/// Everything not listed here is treated as *productive* — a conservative bias
/// so the stall rule never fires on unfamiliar/side-effecting tools.
const READONLY_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "NotebookRead",
    "TodoWrite", // planning, not an artifact
];

/// Case-insensitive exact match against [`READONLY_TOOLS`] (never substring —
/// project convention 2).
pub fn is_readonly_tool(name: &str) -> bool {
    READONLY_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(name))
}

/// Lowercase + collapse whitespace so "similar input" is a deterministic
/// equality check. CJK-safe (no byte slicing).
fn normalize_summary(summary: &Option<String>) -> String {
    match summary {
        Some(s) => s
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase(),
        None => String::new(),
    }
}

/// Loop key = tool name + normalized input.
fn loop_key(step: &ToolStep) -> (String, String) {
    (step.tool.clone(), normalize_summary(&step.summary))
}

// ─── Detectors (pure) ───────────────────────────────────────

/// Repeated-tool-loop rule: over the last `repeat_window` starts, does any
/// `(tool, normalized-input)` occur at least `repeat_threshold` times?
///
/// Severity: `>= 2× threshold` → High, else Medium.
pub fn detect_repeated_loop(
    starts: &VecDeque<ToolStep>,
    cfg: &TrajectoryGuardConfig,
) -> Option<AnomalySignal> {
    if cfg.repeat_window == 0 || cfg.repeat_threshold < 2 {
        return None;
    }
    let window: Vec<&ToolStep> = starts.iter().rev().take(cfg.repeat_window).collect();
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    let mut worst: Option<((String, String), usize)> = None;
    for step in window {
        let key = loop_key(step);
        let c = counts.entry(key.clone()).or_insert(0);
        *c += 1;
        let c = *c;
        if worst.as_ref().map(|(_, w)| c > *w).unwrap_or(true) {
            worst = Some((key, c));
        }
    }
    let (key, count) = worst?;
    if count < cfg.repeat_threshold {
        return None;
    }
    let severity = if count >= cfg.repeat_threshold.saturating_mul(2) {
        Severity::High
    } else {
        Severity::Medium
    };
    let (tool, input) = key;
    let input_hint = if input.is_empty() {
        String::new()
    } else {
        format!("（輸入「{}」）", duduclaw_core::truncate_chars(&input, 40))
    };
    Some(AnomalySignal {
        kind: AnomalyKind::RepeatedToolLoop,
        severity,
        evidence: format!(
            "工具 `{tool}`{input_hint} 在最近 {} 步內重複 {count} 次，疑似 runaway 迴圈",
            cfg.repeat_window
        ),
    })
}

/// Excessive-depth rule: is the current outstanding tool-call depth at or above
/// `max_depth`?
///
/// Severity: `>= max_depth + 3` → High, else Medium.
pub fn detect_excessive_depth(
    open_depth: usize,
    cfg: &TrajectoryGuardConfig,
) -> Option<AnomalySignal> {
    if cfg.max_depth == 0 || open_depth < cfg.max_depth {
        return None;
    }
    let severity = if open_depth >= cfg.max_depth.saturating_add(3) {
        Severity::High
    } else {
        Severity::Medium
    };
    Some(AnomalySignal {
        kind: AnomalyKind::ExcessiveDepth,
        severity,
        evidence: format!(
            "未結束的工具呼叫巢狀深度達 {open_depth}（閾值 {}），計畫可能失控展開",
            cfg.max_depth
        ),
    })
}

/// Cost-slope rule: over all held samples, is the average spend rate greater
/// than `cost_baseline_per_min × cost_slope_multiplier`?
///
/// Needs at least two samples spanning a positive time delta. Severity:
/// `> 2×` the trip threshold → High, else Medium.
pub fn detect_cost_slope(
    samples: &VecDeque<CostSample>,
    cfg: &TrajectoryGuardConfig,
) -> Option<AnomalySignal> {
    if samples.len() < 2 || cfg.cost_baseline_per_min <= 0.0 {
        return None;
    }
    let first = samples.front()?;
    let last = samples.back()?;
    let dt_ms = last.ts_ms.checked_sub(first.ts_ms)?;
    if dt_ms == 0 {
        return None;
    }
    let d_cost = last.cumulative.saturating_sub(first.cumulative) as f64;
    let minutes = dt_ms as f64 / 60_000.0;
    let slope = d_cost / minutes; // units per minute
    let threshold = cfg.cost_baseline_per_min * cfg.cost_slope_multiplier;
    if slope <= threshold {
        return None;
    }
    let severity = if slope > threshold * 2.0 {
        Severity::High
    } else {
        Severity::Medium
    };
    Some(AnomalySignal {
        kind: AnomalyKind::CostSlopeSpike,
        severity,
        evidence: format!(
            "成本累積速率 {slope:.0}/分鐘 超過基線 {:.0} 的 {:.1} 倍（閾值 {threshold:.0}）",
            cfg.cost_baseline_per_min, cfg.cost_slope_multiplier
        ),
    })
}

/// Stall rule: once at least `stall_min_steps` starts have been seen, is the
/// read-only ratio over the last `stall_window` starts at or above
/// `stall_readonly_ratio`?
///
/// Severity: all read-only (ratio == 1.0) → High, else Medium.
pub fn detect_stall(
    starts: &VecDeque<ToolStep>,
    cfg: &TrajectoryGuardConfig,
) -> Option<AnomalySignal> {
    if cfg.stall_window == 0 || starts.len() < cfg.stall_min_steps {
        return None;
    }
    let window: Vec<&ToolStep> = starts.iter().rev().take(cfg.stall_window).collect();
    if window.len() < cfg.stall_min_steps {
        return None;
    }
    let total = window.len();
    let readonly = window.iter().filter(|s| is_readonly_tool(&s.tool)).count();
    let ratio = readonly as f64 / total as f64;
    if ratio < cfg.stall_readonly_ratio {
        return None;
    }
    let severity = if (ratio - 1.0).abs() < f64::EPSILON {
        Severity::High
    } else {
        Severity::Medium
    };
    Some(AnomalySignal {
        kind: AnomalyKind::TrajectoryStall,
        severity,
        evidence: format!(
            "最近 {total} 步有 {readonly} 步為唯讀工具（比例 {:.0}%），步數增加但無實質產出",
            ratio * 100.0
        ),
    })
}

// ─── Stateful guard ─────────────────────────────────────────

/// Upper bound on retained cost samples (bounds memory on long streams).
const COST_SAMPLE_CAP: usize = 64;

/// Stateful, deterministic accumulator that feeds the pure detectors and
/// emits *new* signals (de-duplicated by kind, re-emitted only on escalation).
#[derive(Debug)]
pub struct TrajectoryGuard {
    cfg: TrajectoryGuardConfig,
    /// Recent tool *starts*, bounded to the larger of the two windows.
    starts: VecDeque<ToolStep>,
    /// Current outstanding tool-call depth.
    open_depth: usize,
    /// Recent cumulative-cost samples.
    cost_samples: VecDeque<CostSample>,
    /// Highest severity rank already emitted per kind (de-dup / escalation).
    fired: HashMap<AnomalyKind, u8>,
}

impl TrajectoryGuard {
    /// Build from an explicit config.
    pub fn new(cfg: TrajectoryGuardConfig) -> Self {
        Self {
            cfg,
            starts: VecDeque::new(),
            open_depth: 0,
            cost_samples: VecDeque::new(),
            fired: HashMap::new(),
        }
    }

    /// Build from `<home>/config.toml` (fail-safe defaults on any error).
    pub fn from_home(home_dir: &Path) -> Self {
        Self::new(TrajectoryGuardConfig::from_home(home_dir))
    }

    /// Whether the guard is active.
    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Current outstanding depth (test/inspection helper).
    pub fn open_depth(&self) -> usize {
        self.open_depth
    }

    fn window_cap(&self) -> usize {
        self.cfg.repeat_window.max(self.cfg.stall_window).max(1)
    }

    /// Ingest one tool step, returning any *newly* fired signals. No-op (empty)
    /// when disabled.
    pub fn observe_step(&mut self, step: &ToolStep) -> Vec<AnomalySignal> {
        if !self.cfg.enabled {
            return Vec::new();
        }
        // Update outstanding depth from this boundary.
        self.open_depth = if step.phase_start {
            step.depth.saturating_add(1)
        } else {
            step.depth
        };
        if step.phase_start {
            self.starts.push_back(step.clone());
            let cap = self.window_cap();
            while self.starts.len() > cap {
                self.starts.pop_front();
            }
        }
        let mut candidates = Vec::new();
        if let Some(s) = detect_repeated_loop(&self.starts, &self.cfg) {
            candidates.push(s);
        }
        if let Some(s) = detect_excessive_depth(self.open_depth, &self.cfg) {
            candidates.push(s);
        }
        if let Some(s) = detect_stall(&self.starts, &self.cfg) {
            candidates.push(s);
        }
        self.emit_new(candidates)
    }

    /// Ingest one cumulative-cost sample, returning any newly fired signals.
    pub fn observe_cost(&mut self, sample: CostSample) -> Vec<AnomalySignal> {
        if !self.cfg.enabled {
            return Vec::new();
        }
        self.cost_samples.push_back(sample);
        while self.cost_samples.len() > COST_SAMPLE_CAP {
            self.cost_samples.pop_front();
        }
        let mut candidates = Vec::new();
        if let Some(s) = detect_cost_slope(&self.cost_samples, &self.cfg) {
            candidates.push(s);
        }
        self.emit_new(candidates)
    }

    /// Keep only signals whose severity strictly exceeds what we already emitted
    /// for that kind, then record the new high-water mark.
    fn emit_new(&mut self, candidates: Vec<AnomalySignal>) -> Vec<AnomalySignal> {
        let mut out = Vec::new();
        for sig in candidates {
            let rank = sig.severity.rank();
            let prev = self.fired.get(&sig.kind).copied().unwrap_or(0);
            if rank > prev {
                self.fired.insert(sig.kind, rank);
                out.push(sig);
            }
        }
        out
    }

    /// Pure intervention decision: should the caller trip an (existing) circuit
    /// breaker for this signal? Only when the operator opted in
    /// (`intervene = true`) **and** the signal is High severity. Report-only
    /// deployments always get `false` — the guard never changes execution on
    /// its own (fail-safe).
    pub fn should_intervene(&self, signal: &AnomalySignal) -> bool {
        self.cfg.intervene && signal.severity == Severity::High
    }
}

/// Build the structured `channel_failures.jsonl` record for an anomaly signal.
/// Pure — the caller performs the (locked) append.
pub fn anomaly_record(
    agent: &str,
    session_id: &str,
    signal: &AnomalySignal,
    intervene: bool,
) -> serde_json::Value {
    // R3: annotate the record with its MAST failure-taxonomy label
    // (arXiv:2503.13657). Deterministic; ambiguous kinds stay `unclassified`.
    let mast = crate::mast::classify(&crate::mast::FailureEvidence {
        anomaly: Some(signal.kind.as_str()),
        ..Default::default()
    });
    serde_json::json!({
        "event": "trajectory_anomaly",
        "agent": agent,
        "session_id": session_id,
        "anomaly": signal.kind.as_str(),
        "severity": signal.severity.as_str(),
        "evidence": duduclaw_core::truncate_chars(&signal.evidence, 300),
        "intervene": intervene,
        "mast": mast.as_str(),
        "mast_category": mast.category_str(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

/// Append one anomaly record to `<home>/channel_failures.jsonl` under an
/// advisory file lock (project convention 3 — the file is appended by both the
/// gateway and Python adapters). Best-effort: returns the io result so the
/// caller can log-and-continue; a failure here must never break a reply.
pub fn append_anomaly(home_dir: &Path, record: &serde_json::Value) -> std::io::Result<()> {
    let path = home_dir.join("channel_failures.jsonl");
    let line = format!("{record}\n");
    duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(tool: &str, summary: Option<&str>, depth: usize) -> ToolStep {
        ToolStep {
            tool: tool.to_string(),
            summary: summary.map(String::from),
            phase_start: true,
            depth,
        }
    }

    fn end(tool: &str, depth: usize) -> ToolStep {
        ToolStep {
            tool: tool.to_string(),
            summary: None,
            phase_start: false,
            depth,
        }
    }

    // ── Config parsing ──────────────────────────────────

    #[test]
    fn config_default_is_report_only_and_enabled() {
        let cfg = TrajectoryGuardConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.intervene);
    }

    #[test]
    fn config_absent_section_yields_default() {
        let cfg = TrajectoryGuardConfig::parse("[other]\nx = 1\n");
        assert_eq!(cfg, TrajectoryGuardConfig::default());
    }

    #[test]
    fn config_malformed_toml_yields_default() {
        let cfg = TrajectoryGuardConfig::parse("this is = = not toml [[[");
        assert_eq!(cfg, TrajectoryGuardConfig::default());
    }

    #[test]
    fn config_partial_override_keeps_other_defaults() {
        let cfg = TrajectoryGuardConfig::parse(
            "[trajectory_guard]\nenabled = false\nintervene = true\nmax_depth = 10\n",
        );
        assert!(!cfg.enabled);
        assert!(cfg.intervene);
        assert_eq!(cfg.max_depth, 10);
        // untouched fields retain defaults
        assert_eq!(cfg.repeat_window, 8);
        assert_eq!(cfg.repeat_threshold, 4);
    }

    #[test]
    fn config_clamps_out_of_range_values() {
        let cfg = TrajectoryGuardConfig::parse(
            "[trajectory_guard]\nrepeat_threshold = 0\nmax_depth = 0\nstall_readonly_ratio = 5.0\ncost_slope_multiplier = 0.5\n",
        );
        assert_eq!(cfg.repeat_threshold, 2, "clamped up to minimum 2");
        assert_eq!(cfg.max_depth, 1, "clamped up to minimum 1");
        assert_eq!(
            cfg.stall_readonly_ratio,
            TrajectoryGuardConfig::default().stall_readonly_ratio,
            "out-of-range ratio ignored"
        );
        assert_eq!(
            cfg.cost_slope_multiplier,
            TrajectoryGuardConfig::default().cost_slope_multiplier,
            "multiplier <= 1.0 ignored"
        );
    }

    #[test]
    fn config_accepts_integer_for_float_fields() {
        let cfg = TrajectoryGuardConfig::parse(
            "[trajectory_guard]\ncost_baseline_per_min = 10000\ncost_slope_multiplier = 3\n",
        );
        assert_eq!(cfg.cost_baseline_per_min, 10_000.0);
        assert_eq!(cfg.cost_slope_multiplier, 3.0);
    }

    // ── Repeated-loop rule ──────────────────────────────

    #[test]
    fn repeat_loop_normal_varied_tools_no_signal() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        for t in ["Read", "Grep", "Bash", "Edit", "Read"] {
            q.push_back(start(t, Some("x"), 0));
        }
        assert!(detect_repeated_loop(&q, &cfg).is_none());
    }

    #[test]
    fn repeat_loop_same_tool_same_input_trips_medium() {
        let cfg = TrajectoryGuardConfig::default(); // threshold 4
        let mut q = VecDeque::new();
        for _ in 0..4 {
            q.push_back(start("Bash", Some("curl x"), 0));
        }
        let sig = detect_repeated_loop(&q, &cfg).expect("should trip");
        assert_eq!(sig.kind, AnomalyKind::RepeatedToolLoop);
        assert_eq!(sig.severity, Severity::Medium);
    }

    #[test]
    fn repeat_loop_high_when_double_threshold() {
        let cfg = TrajectoryGuardConfig {
            repeat_window: 16,
            ..Default::default()
        };
        let mut q = VecDeque::new();
        for _ in 0..8 {
            q.push_back(start("Bash", Some("curl x"), 0));
        }
        let sig = detect_repeated_loop(&q, &cfg).unwrap();
        assert_eq!(sig.severity, Severity::High);
    }

    #[test]
    fn repeat_loop_similar_input_normalized_matches() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        // Whitespace/case differences must still count as the same input.
        for s in ["curl  X", "CURL x", "curl x", "curl   x"] {
            q.push_back(start("Bash", Some(s), 0));
        }
        let sig = detect_repeated_loop(&q, &cfg).expect("normalized identical");
        assert_eq!(sig.kind, AnomalyKind::RepeatedToolLoop);
    }

    #[test]
    fn repeat_loop_below_threshold_no_signal() {
        let cfg = TrajectoryGuardConfig::default(); // threshold 4
        let mut q = VecDeque::new();
        for _ in 0..3 {
            q.push_back(start("Bash", Some("curl x"), 0));
        }
        assert!(detect_repeated_loop(&q, &cfg).is_none());
    }

    #[test]
    fn repeat_loop_window_boundary_excludes_old_repeats() {
        let cfg = TrajectoryGuardConfig {
            repeat_window: 4,
            repeat_threshold: 4,
            ..Default::default()
        };
        let mut q = VecDeque::new();
        // 3 old repeats then 3 unrelated: only last 4 examined → max count < 4.
        for _ in 0..3 {
            q.push_back(start("Bash", Some("curl x"), 0));
        }
        for t in ["Read", "Grep", "Edit"] {
            q.push_back(start(t, Some("y"), 0));
        }
        assert!(
            detect_repeated_loop(&q, &cfg).is_none(),
            "old repeats fall outside the window"
        );
    }

    // ── Excessive-depth rule ────────────────────────────

    #[test]
    fn depth_below_threshold_no_signal() {
        let cfg = TrajectoryGuardConfig::default(); // max_depth 6
        assert!(detect_excessive_depth(5, &cfg).is_none());
    }

    #[test]
    fn depth_at_threshold_trips_medium() {
        let cfg = TrajectoryGuardConfig::default();
        let sig = detect_excessive_depth(6, &cfg).unwrap();
        assert_eq!(sig.kind, AnomalyKind::ExcessiveDepth);
        assert_eq!(sig.severity, Severity::Medium);
    }

    #[test]
    fn depth_far_past_threshold_trips_high() {
        let cfg = TrajectoryGuardConfig::default();
        let sig = detect_excessive_depth(9, &cfg).unwrap();
        assert_eq!(sig.severity, Severity::High);
    }

    // ── Cost-slope rule ─────────────────────────────────

    #[test]
    fn cost_slope_single_sample_no_signal() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        q.push_back(CostSample {
            ts_ms: 0,
            cumulative: 100,
        });
        assert!(detect_cost_slope(&q, &cfg).is_none());
    }

    #[test]
    fn cost_slope_zero_time_delta_no_panic_no_signal() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        q.push_back(CostSample {
            ts_ms: 500,
            cumulative: 100,
        });
        q.push_back(CostSample {
            ts_ms: 500,
            cumulative: 9_999_999,
        });
        assert!(detect_cost_slope(&q, &cfg).is_none());
    }

    #[test]
    fn cost_slope_within_baseline_no_signal() {
        let cfg = TrajectoryGuardConfig::default(); // baseline 5000 * 5 = 25000/min
        let mut q = VecDeque::new();
        // 10000 units over 1 minute = 10000/min < 25000 threshold.
        q.push_back(CostSample {
            ts_ms: 0,
            cumulative: 0,
        });
        q.push_back(CostSample {
            ts_ms: 60_000,
            cumulative: 10_000,
        });
        assert!(detect_cost_slope(&q, &cfg).is_none());
    }

    #[test]
    fn cost_slope_spike_trips_medium() {
        let cfg = TrajectoryGuardConfig::default(); // threshold 25000/min
        let mut q = VecDeque::new();
        // 30000 over 1 min = 30000/min > 25000, but < 2× (50000) → Medium.
        q.push_back(CostSample {
            ts_ms: 0,
            cumulative: 0,
        });
        q.push_back(CostSample {
            ts_ms: 60_000,
            cumulative: 30_000,
        });
        let sig = detect_cost_slope(&q, &cfg).unwrap();
        assert_eq!(sig.kind, AnomalyKind::CostSlopeSpike);
        assert_eq!(sig.severity, Severity::Medium);
    }

    #[test]
    fn cost_slope_spike_trips_high_when_double() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        // 120000 over 1 min = 120000/min > 2×25000 = 50000 → High.
        q.push_back(CostSample {
            ts_ms: 0,
            cumulative: 0,
        });
        q.push_back(CostSample {
            ts_ms: 60_000,
            cumulative: 120_000,
        });
        let sig = detect_cost_slope(&q, &cfg).unwrap();
        assert_eq!(sig.severity, Severity::High);
    }

    #[test]
    fn cost_slope_uses_first_and_last_over_window() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        q.push_back(CostSample {
            ts_ms: 0,
            cumulative: 0,
        });
        q.push_back(CostSample {
            ts_ms: 30_000,
            cumulative: 20_000,
        });
        q.push_back(CostSample {
            ts_ms: 120_000,
            cumulative: 60_000,
        });
        // 60000 over 2 min = 30000/min > 25000 → Medium.
        let sig = detect_cost_slope(&q, &cfg).unwrap();
        assert_eq!(sig.severity, Severity::Medium);
    }

    // ── Stall rule ──────────────────────────────────────

    #[test]
    fn stall_below_min_steps_no_signal() {
        let cfg = TrajectoryGuardConfig::default(); // min 8
        let mut q = VecDeque::new();
        for _ in 0..7 {
            q.push_back(start("Read", Some("f"), 0));
        }
        assert!(detect_stall(&q, &cfg).is_none());
    }

    #[test]
    fn stall_all_readonly_trips_high() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        for _ in 0..10 {
            q.push_back(start("Read", Some("f"), 0));
        }
        let sig = detect_stall(&q, &cfg).unwrap();
        assert_eq!(sig.kind, AnomalyKind::TrajectoryStall);
        assert_eq!(sig.severity, Severity::High);
    }

    #[test]
    fn stall_mixed_but_mostly_readonly_trips_medium() {
        let cfg = TrajectoryGuardConfig {
            stall_window: 10,
            stall_min_steps: 8,
            stall_readonly_ratio: 0.85,
            ..Default::default()
        };
        let mut q = VecDeque::new();
        for _ in 0..9 {
            q.push_back(start("Grep", Some("f"), 0));
        }
        q.push_back(start("Edit", Some("x"), 0)); // 9/10 = 0.9 >= 0.85
        let sig = detect_stall(&q, &cfg).unwrap();
        assert_eq!(sig.severity, Severity::Medium);
    }

    #[test]
    fn stall_productive_work_no_signal() {
        let cfg = TrajectoryGuardConfig::default();
        let mut q = VecDeque::new();
        // Half writing/running → ratio 0.5 < 0.85.
        for t in [
            "Read", "Edit", "Read", "Bash", "Read", "Edit", "Grep", "Write", "Read", "Bash",
        ] {
            q.push_back(start(t, Some("f"), 0));
        }
        assert!(detect_stall(&q, &cfg).is_none());
    }

    #[test]
    fn is_readonly_tool_exact_case_insensitive() {
        assert!(is_readonly_tool("Read"));
        assert!(is_readonly_tool("read"));
        assert!(is_readonly_tool("GREP"));
        assert!(!is_readonly_tool("Bash"));
        assert!(!is_readonly_tool("Write"));
        // Never substring — "Reader" must not match "Read".
        assert!(!is_readonly_tool("Reader"));
    }

    // ── Stateful guard ──────────────────────────────────

    #[test]
    fn guard_disabled_is_noop() {
        let cfg = TrajectoryGuardConfig {
            enabled: false,
            ..Default::default()
        };
        let mut g = TrajectoryGuard::new(cfg);
        for _ in 0..20 {
            assert!(g.observe_step(&start("Bash", Some("x"), 0)).is_empty());
        }
    }

    #[test]
    fn guard_depth_tracking_from_start_end() {
        let mut g = TrajectoryGuard::new(TrajectoryGuardConfig::default());
        g.observe_step(&start("Task", Some("a"), 0)); // open = 1
        assert_eq!(g.open_depth(), 1);
        g.observe_step(&start("Read", Some("b"), 1)); // open = 2
        assert_eq!(g.open_depth(), 2);
        g.observe_step(&end("Read", 1)); // open = 1
        assert_eq!(g.open_depth(), 1);
    }

    #[test]
    fn guard_emits_depth_signal_and_dedups() {
        let cfg = TrajectoryGuardConfig {
            max_depth: 3,
            ..Default::default()
        };
        let mut g = TrajectoryGuard::new(cfg);
        // Build nesting depth up to 3 (start depths 0,1,2 → open 1,2,3).
        assert!(g.observe_step(&start("Task", Some("a"), 0)).is_empty());
        assert!(g.observe_step(&start("Task", Some("b"), 1)).is_empty());
        let sigs = g.observe_step(&start("Read", Some("c"), 2)); // open = 3
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].kind, AnomalyKind::ExcessiveDepth);
        assert_eq!(sigs[0].severity, Severity::Medium);
        // A second step at the same depth does NOT re-emit (same severity).
        let again = g.observe_step(&start("Read", Some("d"), 2));
        assert!(again.iter().all(|s| s.kind != AnomalyKind::ExcessiveDepth));
    }

    #[test]
    fn guard_re_emits_on_escalation() {
        let cfg = TrajectoryGuardConfig {
            max_depth: 3,
            ..Default::default()
        };
        let mut g = TrajectoryGuard::new(cfg);
        g.observe_step(&start("Task", Some("a"), 0));
        g.observe_step(&start("Task", Some("b"), 1));
        let med = g.observe_step(&start("Task", Some("c"), 2)); // open 3 → Medium
        assert_eq!(med[0].severity, Severity::Medium);
        g.observe_step(&start("Task", Some("d"), 3));
        g.observe_step(&start("Task", Some("e"), 4));
        let high = g.observe_step(&start("Read", Some("f"), 5)); // open 6 → High
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].severity, Severity::High);
    }

    #[test]
    fn guard_from_step_event_conversion() {
        use crate::channel_reply::{StepEvent, StepPhase};
        let ev = StepEvent {
            phase: StepPhase::Start,
            tool: "Bash".to_string(),
            summary: Some("curl x".to_string()),
            depth: 2,
            ts_ms: 123,
        };
        let ts: ToolStep = (&ev).into();
        assert_eq!(ts.tool, "Bash");
        assert!(ts.phase_start);
        assert_eq!(ts.depth, 2);
        assert_eq!(ts.summary.as_deref(), Some("curl x"));
    }

    #[test]
    fn guard_should_intervene_gated_by_config_and_severity() {
        let report_only = TrajectoryGuard::new(TrajectoryGuardConfig::default());
        let high = AnomalySignal {
            kind: AnomalyKind::ExcessiveDepth,
            severity: Severity::High,
            evidence: String::new(),
        };
        assert!(
            !report_only.should_intervene(&high),
            "report-only never intervenes"
        );

        let active = TrajectoryGuard::new(TrajectoryGuardConfig {
            intervene: true,
            ..Default::default()
        });
        assert!(active.should_intervene(&high));
        let medium = AnomalySignal {
            severity: Severity::Medium,
            ..high.clone()
        };
        assert!(
            !active.should_intervene(&medium),
            "even when intervening, only High trips"
        );
    }

    #[test]
    fn guard_cost_observe_trips() {
        let mut g = TrajectoryGuard::new(TrajectoryGuardConfig::default());
        assert!(g
            .observe_cost(CostSample {
                ts_ms: 0,
                cumulative: 0
            })
            .is_empty());
        let sigs = g.observe_cost(CostSample {
            ts_ms: 60_000,
            cumulative: 120_000,
        });
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].kind, AnomalyKind::CostSlopeSpike);
        assert_eq!(sigs[0].severity, Severity::High);
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
    }

    #[test]
    fn anomaly_record_shape() {
        let sig = AnomalySignal {
            kind: AnomalyKind::RepeatedToolLoop,
            severity: Severity::High,
            evidence: "工具 `Bash` 重複 8 次".to_string(),
        };
        let rec = anomaly_record("agnes", "sess-1", &sig, true);
        assert_eq!(rec["event"], "trajectory_anomaly");
        assert_eq!(rec["agent"], "agnes");
        assert_eq!(rec["session_id"], "sess-1");
        assert_eq!(rec["anomaly"], "repeated_tool_loop");
        assert_eq!(rec["severity"], "high");
        assert_eq!(rec["intervene"], true);
        assert!(rec["evidence"].as_str().unwrap().contains("Bash"));
        // R3: MAST annotation — repeated loop is a definitional FM-1.3.
        assert_eq!(rec["mast"], "FM-1.3");
        assert_eq!(rec["mast_category"], "specification_issues");
    }

    #[test]
    fn anomaly_record_ambiguous_kind_is_unclassified() {
        let sig = AnomalySignal {
            kind: AnomalyKind::TrajectoryStall,
            severity: Severity::Medium,
            evidence: "stall".to_string(),
        };
        let rec = anomaly_record("a", "s", &sig, false);
        assert_eq!(rec["mast"], "unclassified");
    }

    #[test]
    fn append_anomaly_writes_line() {
        let dir = std::env::temp_dir().join(format!("dudu-traj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sig = AnomalySignal {
            kind: AnomalyKind::TrajectoryStall,
            severity: Severity::Medium,
            evidence: "stall".to_string(),
        };
        let rec = anomaly_record("a", "s", &sig, false);
        append_anomaly(&dir, &rec).unwrap();
        let body = std::fs::read_to_string(dir.join("channel_failures.jsonl")).unwrap();
        assert!(body.contains("trajectory_anomaly"));
        assert!(body.contains("trajectory_stall"));
        assert_eq!(body.lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
