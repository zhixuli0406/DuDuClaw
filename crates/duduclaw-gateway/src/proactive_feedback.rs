//! OS-native P2-3: four-quadrant proactive-notification outcome tracking.
//!
//! Backfills the `outcome` field [`crate::proactive_gate`] reserves in every
//! `<home>/proactive_gate.jsonl` decision line, aggregates the result into
//! ProactiveAgent's (arXiv:2410.12361) four-quadrant confusion matrix, and
//! feeds the aggregate back into the gate's `base_threshold` — the
//! `metacognition_base` calibration hook [`crate::proactive_gate`] documents
//! but leaves unwired.
//!
//! ## Outcome determination (deterministic, zero LLM)
//!
//! | decision | signal within the window                              | outcome              |
//! |----------|---------------------------------------------------------|-----------------------|
//! | allow    | explicit dismiss seen (`feedback.jsonl` type=`negative`) | `false_alarm`         |
//! | allow    | no dismiss + session activity for the agent in-window    | `correct_detection`   |
//! | allow    | no dismiss + no activity at all in-window                | `non_response`        |
//! | suppress | user later opened a related request (same agent, event-name keyword hit, CJK-safe) | `missed_need` |
//! | suppress | no related follow-up in-window                           | `correct_silence`     |
//! | any      | the signal source itself was unavailable (DB/file error) | `unknown`              |
//!
//! `correct_silence` is not one of ProactiveAgent's four named quadrants —
//! it's the "system correctly stayed quiet" true-negative that the paper's
//! evaluation implicitly needs a denominator from (missed-need rate = mn /
//! (mn + correct_silence)), so we track it alongside without over-claiming
//! it as a fifth *reward* quadrant.
//!
//! **Deliberately not implemented**: there is no proactive-notify-specific
//! "dismiss" UI wired anywhere yet (no channel button posts
//! `type=negative` for a specific gate decision). The explicit-dismiss probe
//! is a real, already-existing `feedback.jsonl` convention
//! ([`crate::external_factors::FeedbackSignal`]) and is wired end-to-end so a
//! future channel UI only needs to call `submit_feedback` — until then that
//! branch is simply never hit and allow decisions resolve to
//! `correct_detection` / `non_response` from session activity alone. This is
//! the "使用者在窗內對該 agent 有正向互動" signal.
//!
//! ## Backfill scaling policy
//!
//! [`run_backfill_once`] always reads the *entire* `proactive_gate.jsonl`
//! (unavoidable — the final write is a read-modify-write of the whole file
//! under [`duduclaw_core::with_file_lock`]), but only *attempts* signal
//! gathering (the session/feedback probes) for the last
//! [`BACKFILL_TAIL_LINES`] lines each tick. Since new decisions are always
//! appended at the end and the outcome window is short (default 30 min),
//! almost every still-`null` line is near the tail — a line only survives
//! past the tail horizon `null` if the process was down for a long stretch.
//! That's an accepted trade-off documented here rather than silently
//! papered over: such very old lines stay `null` forever (they still count
//! toward `unknown`-free scanning cost being O(tail), not O(file)).
//!
//! ## Calibration feedback
//!
//! [`run_calibration_tick`] computes False-Alarm rate (`fa / (cd + fa)`) and
//! Missed-Need rate (`mn / (mn + cs)`) over a trailing lookback window,
//! EMA-smooths each ([`CALIBRATION_EMA_ALPHA`]), and nudges a continuous
//! calibration value `t` (0.0–1.0, the same domain
//! [`crate::proactive_gate::metacognition_base`] maps from) by
//! [`CALIBRATION_STEP`] per tick whenever the smoothed rate crosses
//! [`FA_RATE_HIGH`] / [`MN_RATE_HIGH`]. The *published* base_threshold
//! derived from `t` is capped to move at most
//! [`CALIBRATION_MAX_DAILY_BASE_DELTA`] per UTC calendar day from whatever
//! was published at the start of that day — `t` itself is never clamped, so
//! sustained pressure is not lost, only rate-limited in its visible effect.
//! State persists to `<home>/proactive_calibration.json`
//! (`with_file_lock`). [`effective_proactive_config`] is the "equivalent
//! hook" [`crate::proactive_gate::read_proactive_config`]'s doc comment
//! promised P2-3 — it overlays the calibrated base onto whatever
//! `agent.toml` configured, once at least one calibration has run for that
//! agent. Deliberately **not** a change to `read_proactive_config` itself —
//! that function's 12 existing tests stay untouched.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::word_contains_ci;

use crate::proactive_gate::{self, DEFAULT_BASE_THRESHOLD, MAX_SCORE, MIN_SCORE, ProactiveConfig};
use crate::session::SessionManager;

// ── Tunables ─────────────────────────────────────────────────────────────

/// Default window a decision waits for a user reaction before its outcome is
/// considered "due" for backfill. ProactiveAgent-style four-quadrant scoring
/// needs *some* fixed horizon; 30 min matches the task-level spec.
pub const DEFAULT_OUTCOME_WINDOW: Duration = Duration::from_secs(30 * 60);
/// How often the background loop scans for due lines / recalibrates.
pub const BACKFILL_TICK_INTERVAL: Duration = Duration::from_secs(60);
/// Only the last N lines of `proactive_gate.jsonl` are candidates for
/// backfill signal-gathering each tick (see module doc "Backfill scaling
/// policy").
pub const BACKFILL_TAIL_LINES: usize = 500;
/// Trailing lookback window `run_calibration_tick` aggregates quadrant
/// counts over.
pub const CALIBRATION_LOOKBACK: Duration = Duration::from_secs(24 * 3600);
/// Minimum sample size per dimension (FA or MN) before that dimension is
/// allowed to move calibration — mirrors `MetaCognition::calibrate_proactive_threshold`'s
/// own `total < 5` guard.
pub const CALIBRATION_MIN_SAMPLES: u64 = 5;
/// EMA smoothing factor applied to each observed rate before comparing
/// against the high-rate thresholds.
pub const CALIBRATION_EMA_ALPHA: f64 = 0.3;
/// Fixed step applied to the continuous `t` (0.0–1.0) calibration value per
/// tick a threshold is crossed — same magnitude as MetaCognition's own
/// `calibrate_proactive_threshold` step.
pub const CALIBRATION_STEP: f64 = 0.05;
/// EMA-smoothed False-Alarm rate above which the gate becomes stricter
/// (raises `t`, thus the published base).
pub const FA_RATE_HIGH: f64 = 0.4;
/// EMA-smoothed Missed-Need rate above which the gate becomes looser
/// (lowers `t`, thus the published base).
pub const MN_RATE_HIGH: f64 = 0.4;
/// Max magnitude the *published* base_threshold may move per UTC calendar
/// day, regardless of how far `t` itself has moved.
pub const CALIBRATION_MAX_DAILY_BASE_DELTA: u8 = 1;

// ── Quadrant outcome ─────────────────────────────────────────────────────

/// ProactiveAgent's four-quadrant confusion matrix, plus `CorrectSilence`
/// (the suppress-side true negative — see module doc) and `Unknown` (signal
/// source unavailable — never guessed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadrantOutcome {
    CorrectDetection,
    FalseAlarm,
    MissedNeed,
    NonResponse,
    CorrectSilence,
    Unknown,
}

impl QuadrantOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CorrectDetection => "correct_detection",
            Self::FalseAlarm => "false_alarm",
            Self::MissedNeed => "missed_need",
            Self::NonResponse => "non_response",
            Self::CorrectSilence => "correct_silence",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify an **allow** decision from two independently-probed signals.
/// `None` means that probe's signal source was unavailable — the honest
/// result is `Unknown`, never a guess.
///
/// The dismiss probe is checked first and is authoritative on its own: an
/// explicit dismiss is a false alarm regardless of whether the positive-
/// interaction probe could even be evaluated (a user who dismissed the
/// notification and only *then* happened to talk to the agent about
/// something else is still a false alarm on the notification itself). The
/// positive-interaction probe is only consulted — and only then can this
/// return `Unknown` — once we know the decision was *not* explicitly
/// dismissed.
pub fn classify_allow(
    has_explicit_dismiss: Option<bool>,
    has_positive_interaction: Option<bool>,
) -> QuadrantOutcome {
    match has_explicit_dismiss {
        None => QuadrantOutcome::Unknown,
        Some(true) => QuadrantOutcome::FalseAlarm,
        Some(false) => match has_positive_interaction {
            None => QuadrantOutcome::Unknown,
            Some(true) => QuadrantOutcome::CorrectDetection,
            Some(false) => QuadrantOutcome::NonResponse,
        },
    }
}

/// Classify a **suppress** decision from the single follow-up-request probe.
pub fn classify_suppress(has_related_followup: Option<bool>) -> QuadrantOutcome {
    match has_related_followup {
        None => QuadrantOutcome::Unknown,
        Some(true) => QuadrantOutcome::MissedNeed,
        Some(false) => QuadrantOutcome::CorrectSilence,
    }
}

/// Derive CJK-safe ASCII match keywords from an autopilot `event_name`
/// (e.g. `os_file` → `["file"]`, `agent_idle` → `["agent", "idle"]`). The
/// leading `os` token is dropped as noise (every OS-perception event starts
/// with it). This is the only text we have to match against — the raw
/// perceived event text is never persisted (P2-5 minimization), so
/// "missed_need" detection is keyword-coarse by design, not full-text.
pub fn event_keywords(event_name: &str) -> Vec<&str> {
    event_name
        .split('_')
        .filter(|t| !t.is_empty() && *t != "os")
        .collect()
}

// ── Signal probes (I/O) ─────────────────────────────────────────────────

/// Did an explicit dismiss (`feedback.jsonl` `type=negative` for this agent)
/// land inside `[since, until]`? `Some(false)` on a clean read that found
/// none; `None` only on a genuine read/parse failure of an *existing* file
/// (a missing file is a legitimate "no dismiss recorded", not unknown).
async fn probe_explicit_dismiss(
    feedback_path: &Path,
    agent_id: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Option<bool> {
    let content = match tokio::fs::read_to_string(feedback_path).await {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(false),
        Err(e) => {
            warn!(error = %e, "proactive_feedback: feedback.jsonl read failed — dismiss probe unknown");
            return None;
        }
    };
    let hit = content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| {
            v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
                && v.get("type").and_then(|t| t.as_str()) == Some("negative")
                && v.get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|d| d.with_timezone(&Utc))
                    .is_some_and(|ts| ts >= since && ts <= until)
        });
    Some(hit)
}

/// Did the agent have any session activity (any session's `last_active`) in
/// `[since, until]`? `None` when the session store itself couldn't be
/// queried (no `SessionManager` wired, or the query errored).
async fn probe_positive_interaction(
    session_mgr: Option<&SessionManager>,
    agent_id: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Option<bool> {
    let sm = session_mgr?;
    match sm.list_sessions(Some(agent_id), 50).await {
        Ok(sessions) => Some(sessions.iter().any(|s| {
            DateTime::parse_from_rfc3339(&s.last_active)
                .ok()
                .map(|d| d.with_timezone(&Utc))
                .is_some_and(|ts| ts >= since && ts <= until)
        })),
        Err(e) => {
            warn!(agent = agent_id, error = %e, "proactive_feedback: list_sessions failed — interaction probe unknown");
            None
        }
    }
}

/// Did the user, within `[since, until]`, send a message to this agent whose
/// text CJK-safely word-matches one of `event_keywords(event_name)`? `None`
/// when the session store couldn't be queried at all.
async fn probe_related_followup(
    session_mgr: Option<&SessionManager>,
    agent_id: &str,
    event_name: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Option<bool> {
    let sm = session_mgr?;
    let sessions = match sm.list_sessions(Some(agent_id), 50).await {
        Ok(s) => s,
        Err(e) => {
            warn!(agent = agent_id, error = %e, "proactive_feedback: list_sessions failed — followup probe unknown");
            return None;
        }
    };
    let keywords = event_keywords(event_name);
    if keywords.is_empty() {
        return Some(false);
    }
    let candidates = sessions.iter().filter(|s| {
        DateTime::parse_from_rfc3339(&s.last_active)
            .ok()
            .map(|d| d.with_timezone(&Utc))
            .is_some_and(|ts| ts >= since && ts <= until)
    });
    for s in candidates {
        let Ok(messages) = sm.get_messages(&s.id).await else {
            continue; // best-effort per-session; a single session's read failure doesn't flip the whole probe to Unknown
        };
        for m in messages {
            if m.role != "user" {
                continue;
            }
            let Some(ts) = DateTime::parse_from_rfc3339(&m.timestamp)
                .ok()
                .map(|d| d.with_timezone(&Utc))
            else {
                continue;
            };
            if ts < since || ts > until {
                continue;
            }
            if keywords.iter().any(|k| word_contains_ci(&m.content, k)) {
                return Some(true);
            }
        }
    }
    Some(false)
}

// ── Backfill ─────────────────────────────────────────────────────────────

/// Result of one [`run_backfill_once`] pass.
#[derive(Debug, Clone, Default)]
pub struct BackfillReport {
    /// Total lines in the file at read time.
    pub scanned: usize,
    /// Lines whose `outcome` was written this pass.
    pub backfilled: u64,
    /// Backfilled count grouped by outcome tag.
    pub by_outcome: HashMap<&'static str, u64>,
}

/// Scan the tail of `<home>/proactive_gate.jsonl` for `outcome=null` lines
/// whose window has elapsed, determine their outcome, and rewrite them in
/// place. Idempotent: a line is only ever touched while its `outcome` is
/// still `null` (checked both when a line is selected as a candidate and
/// again just before the write, so a line already backfilled by a
/// concurrent/earlier pass is never re-counted or overwritten).
pub async fn run_backfill_once(
    home_dir: &Path,
    session_mgr: Option<&SessionManager>,
    window: Duration,
) -> io::Result<BackfillReport> {
    let path = home_dir.join("proactive_gate.jsonl");
    let feedback_path = home_dir.join("feedback.jsonl");

    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BackfillReport::default()),
        Err(e) => return Err(e),
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let tail_start = total.saturating_sub(BACKFILL_TAIL_LINES);
    let now = Utc::now();
    let chrono_window =
        chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::minutes(30));

    let mut computed: HashMap<usize, QuadrantOutcome> = HashMap::new();
    for (idx, line) in lines.iter().enumerate().skip(tail_start) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if !v.get("outcome").is_some_and(|o| o.is_null()) {
            continue; // already backfilled or missing the field entirely
        }
        let (Some(agent), Some(event), Some(decision), Some(ts)) = (
            v.get("agent").and_then(|a| a.as_str()),
            v.get("event").and_then(|a| a.as_str()),
            v.get("decision").and_then(|a| a.as_str()),
            v.get("ts")
                .and_then(|t| t.as_str())
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|d| d.with_timezone(&Utc)),
        ) else {
            continue;
        };
        if now - ts < chrono_window {
            continue; // window not elapsed yet — not due
        }
        let since = ts;
        let until = ts + chrono_window;
        let outcome = if decision == "allow" {
            let dismiss = probe_explicit_dismiss(&feedback_path, agent, since, until).await;
            let positive = probe_positive_interaction(session_mgr, agent, since, until).await;
            classify_allow(dismiss, positive)
        } else {
            let related = probe_related_followup(session_mgr, agent, event, since, until).await;
            classify_suppress(related)
        };
        computed.insert(idx, outcome);
    }

    if computed.is_empty() {
        return Ok(BackfillReport {
            scanned: total,
            backfilled: 0,
            by_outcome: HashMap::new(),
        });
    }

    let report = duduclaw_core::with_file_lock(&path, || apply_backfill(&path, &computed))?;
    Ok(report)
}

/// Sync read-modify-write of `path` applying `computed` (line index → new
/// outcome). Always called from inside [`duduclaw_core::with_file_lock`].
/// Re-reads the file fresh so lines appended after the async gathering phase
/// are preserved untouched; only re-verifies + overwrites indices present in
/// `computed` whose outcome is *still* `null` at write time.
fn apply_backfill(
    path: &Path,
    computed: &HashMap<usize, QuadrantOutcome>,
) -> io::Result<BackfillReport> {
    let text = std::fs::read_to_string(path)?;
    let total = text.lines().count();
    let mut out_lines: Vec<String> = Vec::with_capacity(total);
    let mut backfilled = 0u64;
    let mut by_outcome: HashMap<&'static str, u64> = HashMap::new();

    for (idx, line) in text.lines().enumerate() {
        let mut wrote = false;
        if let Some(outcome) = computed.get(&idx) {
            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(line) {
                if val.get("outcome").is_some_and(|o| o.is_null()) {
                    val["outcome"] = serde_json::Value::String(outcome.as_str().to_string());
                    out_lines.push(val.to_string());
                    backfilled += 1;
                    *by_outcome.entry(outcome.as_str()).or_insert(0) += 1;
                    wrote = true;
                }
            }
        }
        if !wrote {
            out_lines.push(line.to_string());
        }
    }

    let mut content = out_lines.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    std::fs::write(path, content)?;
    Ok(BackfillReport {
        scanned: total,
        backfilled,
        by_outcome,
    })
}

// ── Aggregation ──────────────────────────────────────────────────────────

/// Four-quadrant counts for one agent over a trailing `lookback` window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct QuadrantStats {
    pub cd: u64,
    pub fa: u64,
    pub mn: u64,
    pub nr: u64,
    pub cs: u64,
    pub unknown: u64,
}

impl QuadrantStats {
    /// False-Alarm rate among definitively-resolved allow decisions
    /// (excludes `non_response`, which is ambiguous — see module doc).
    /// `None` when there isn't enough data to trust the rate.
    pub fn fa_rate(&self) -> Option<f64> {
        let denom = self.cd + self.fa;
        (denom >= CALIBRATION_MIN_SAMPLES).then(|| self.fa as f64 / denom as f64)
    }

    /// Missed-Need rate among definitively-resolved suppress decisions.
    pub fn mn_rate(&self) -> Option<f64> {
        let denom = self.mn + self.cs;
        (denom >= CALIBRATION_MIN_SAMPLES).then(|| self.mn as f64 / denom as f64)
    }
}

/// Aggregate `<home>/proactive_gate.jsonl` outcomes for `agent_id` over the
/// trailing `lookback` window. Pure read — never mutates the file. O(file
/// size); acceptable for a reporting/calibration API that runs once a
/// minute, not a hot path. Revisit with an index if the file grows into the
/// tens of MB (same ceiling `external_factors::collect_user_feedback`
/// already documents for `feedback.jsonl`).
pub fn quadrant_stats(
    home_dir: &Path,
    agent_id: &str,
    lookback: Duration,
) -> io::Result<QuadrantStats> {
    let path = home_dir.join("proactive_gate.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(QuadrantStats::default()),
        Err(e) => return Err(e),
    };
    let chrono_lookback =
        chrono::Duration::from_std(lookback).unwrap_or_else(|_| chrono::Duration::hours(24));
    let cutoff = Utc::now() - chrono_lookback;
    let mut stats = QuadrantStats::default();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("agent").and_then(|a| a.as_str()) != Some(agent_id) {
            continue;
        }
        let Some(ts) = v
            .get("ts")
            .and_then(|t| t.as_str())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc))
        else {
            continue;
        };
        if ts < cutoff {
            continue;
        }
        match v.get("outcome").and_then(|o| o.as_str()) {
            Some("correct_detection") => stats.cd += 1,
            Some("false_alarm") => stats.fa += 1,
            Some("missed_need") => stats.mn += 1,
            Some("non_response") => stats.nr += 1,
            Some("correct_silence") => stats.cs += 1,
            Some("unknown") => stats.unknown += 1,
            _ => {} // still null (not yet backfilled) — excluded from the count
        }
    }
    Ok(stats)
}

/// [`quadrant_stats`] plus a non-blocking evolution event carrying the
/// aggregate as metadata (existing [`crate::evolution_events::emitter`]).
pub fn quadrant_stats_and_emit(
    home_dir: &Path,
    agent_id: &str,
    lookback: Duration,
) -> io::Result<QuadrantStats> {
    let stats = quadrant_stats(home_dir, agent_id, lookback)?;
    crate::evolution_events::emitter::EvolutionEventEmitter::global().emit_proactive_quadrant(
        agent_id,
        serde_json::json!({
            "correct_detection": stats.cd,
            "false_alarm": stats.fa,
            "missed_need": stats.mn,
            "non_response": stats.nr,
            "correct_silence": stats.cs,
            "unknown": stats.unknown,
            "fa_rate": stats.fa_rate(),
            "mn_rate": stats.mn_rate(),
            "lookback_secs": lookback.as_secs(),
        }),
    );
    Ok(stats)
}

// ── Calibration ──────────────────────────────────────────────────────────

/// Per-agent calibration state persisted in
/// `<home>/proactive_calibration.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCalibrationState {
    /// EMA-smoothed False-Alarm rate.
    #[serde(default)]
    pub ema_fa_rate: f64,
    /// EMA-smoothed Missed-Need rate.
    #[serde(default)]
    pub ema_mn_rate: f64,
    /// Continuous calibration signal (0.0–1.0), the same domain
    /// `metacognition_base` maps from. Never clamped by the daily publish
    /// cap — only the derived `published_base` is.
    #[serde(default = "default_t")]
    pub t: f64,
    /// UTC calendar date (`YYYY-MM-DD`) the daily publish cap is anchored to.
    #[serde(default)]
    pub day: String,
    /// `published_base` snapshotted at the start of `day` — the anchor the
    /// ±[`CALIBRATION_MAX_DAILY_BASE_DELTA`] cap is measured against.
    #[serde(default = "default_base")]
    pub base_at_day_start: u8,
    /// The base_threshold `effective_proactive_config` overlays right now
    /// (already day-capped).
    #[serde(default = "default_base")]
    pub published_base: u8,
    #[serde(default)]
    pub last_updated: String,
}

fn default_t() -> f64 {
    0.5
}
fn default_base() -> u8 {
    DEFAULT_BASE_THRESHOLD
}

impl Default for AgentCalibrationState {
    fn default() -> Self {
        Self {
            ema_fa_rate: 0.0,
            ema_mn_rate: 0.0,
            t: default_t(),
            day: String::new(),
            base_at_day_start: default_base(),
            published_base: default_base(),
            last_updated: String::new(),
        }
    }
}

/// Pure calibration step — no I/O, directly unit-testable.
///
/// `fa_rate` / `mn_rate` are `None` when that dimension doesn't have enough
/// samples this tick ([`QuadrantStats::fa_rate`] / `mn_rate` already apply
/// [`CALIBRATION_MIN_SAMPLES`]); a `None` dimension contributes no EMA
/// update and no directional pressure this tick, but does not reset the
/// dimension's existing EMA.
pub fn step_calibration(
    prev: Option<AgentCalibrationState>,
    fa_rate: Option<f64>,
    mn_rate: Option<f64>,
    now: DateTime<Utc>,
) -> AgentCalibrationState {
    let mut state = prev.unwrap_or_default();
    let today = now.format("%Y-%m-%d").to_string();
    if state.day != today {
        state.day = today;
        state.base_at_day_start = state.published_base;
    }

    if let Some(fa) = fa_rate {
        state.ema_fa_rate =
            CALIBRATION_EMA_ALPHA * fa + (1.0 - CALIBRATION_EMA_ALPHA) * state.ema_fa_rate;
    }
    if let Some(mn) = mn_rate {
        state.ema_mn_rate =
            CALIBRATION_EMA_ALPHA * mn + (1.0 - CALIBRATION_EMA_ALPHA) * state.ema_mn_rate;
    }

    let mut direction = 0.0;
    if fa_rate.is_some() && state.ema_fa_rate > FA_RATE_HIGH {
        direction += CALIBRATION_STEP; // more false alarms → stricter (higher base)
    }
    if mn_rate.is_some() && state.ema_mn_rate > MN_RATE_HIGH {
        direction -= CALIBRATION_STEP; // more missed needs → looser (lower base)
    }
    state.t = (state.t + direction).clamp(0.0, 1.0);

    let candidate_base = proactive_gate::metacognition_base(state.t);
    let lo = state
        .base_at_day_start
        .saturating_sub(CALIBRATION_MAX_DAILY_BASE_DELTA)
        .max(MIN_SCORE);
    let hi = state
        .base_at_day_start
        .saturating_add(CALIBRATION_MAX_DAILY_BASE_DELTA)
        .min(MAX_SCORE);
    state.published_base = candidate_base.clamp(lo, hi);
    state.last_updated = now.to_rfc3339();
    state
}

/// Load the whole calibration map. Missing/corrupt file → empty map
/// (fail-open — this is a soft self-tuning knob, not a security gate; the
/// gate itself stays deny-by-default regardless of calibration state).
fn load_calibration_map(home_dir: &Path) -> HashMap<String, AgentCalibrationState> {
    let path = home_dir.join("proactive_calibration.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read-modify-write `<home>/proactive_calibration.json`, applying one
/// [`step_calibration`] for `agent_id`. Returns the agent's new state.
pub fn calibrate_agent(
    home_dir: &Path,
    agent_id: &str,
    fa_rate: Option<f64>,
    mn_rate: Option<f64>,
    now: DateTime<Utc>,
) -> io::Result<AgentCalibrationState> {
    let path = home_dir.join("proactive_calibration.json");
    duduclaw_core::with_file_lock(&path, || {
        let mut map = load_calibration_map(home_dir);
        let prev = map.get(agent_id).cloned();
        let next = step_calibration(prev, fa_rate, mn_rate, now);
        map.insert(agent_id.to_string(), next.clone());
        let json = serde_json::to_string_pretty(&map)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(next)
    })
}

/// Aggregate + emit + calibrate for one agent. Silently returns `Ok(None)`
/// when neither dimension has enough samples yet — deliberate quiet, no
/// per-tick noise for agents with no proactive traffic (playbook §6
/// "回報降噪").
pub fn run_calibration_tick(
    home_dir: &Path,
    agent_id: &str,
) -> io::Result<Option<AgentCalibrationState>> {
    let stats = quadrant_stats_and_emit(home_dir, agent_id, CALIBRATION_LOOKBACK)?;
    let fa_rate = stats.fa_rate();
    let mn_rate = stats.mn_rate();
    if fa_rate.is_none() && mn_rate.is_none() {
        return Ok(None);
    }
    let state = calibrate_agent(home_dir, agent_id, fa_rate, mn_rate, Utc::now())?;
    info!(
        agent = agent_id,
        base = state.published_base,
        t = format!("{:.2}", state.t),
        fa_rate = ?fa_rate,
        mn_rate = ?mn_rate,
        "proactive gate: calibration tick"
    );
    Ok(Some(state))
}

/// Read the calibrated base_threshold for `agent_id`, if it has ever been
/// calibrated. Best-effort — a missing/corrupt calibration file or an
/// agent with no entry yet returns `None`.
pub fn read_calibrated_base(home_dir: &Path, agent_id: &str) -> Option<u8> {
    load_calibration_map(home_dir)
        .get(agent_id)
        .map(|s| s.published_base)
}

/// Overlay the calibrated base_threshold (if any) onto an already-read
/// [`ProactiveConfig`]. The "equivalent hook" to
/// `proactive_gate::read_proactive_config` P2-2's doc comment reserved for
/// P2-3 — see module doc for why this is a separate function rather than a
/// change to `read_proactive_config` itself.
pub fn effective_proactive_config(
    mut cfg: ProactiveConfig,
    home_dir: &Path,
    agent_id: &str,
) -> ProactiveConfig {
    if let Some(base) = read_calibrated_base(home_dir, agent_id) {
        cfg.base_threshold = base;
    }
    cfg
}

// ── Background loop ──────────────────────────────────────────────────────

/// Spawn the periodic backfill + calibration loop. Ticks every
/// [`BACKFILL_TICK_INTERVAL`]; each tick backfills due `proactive_gate.jsonl`
/// lines, then runs a calibration tick for every agent with `[proactive]
/// enabled = true`. Runs for the process lifetime (mirrors the frontmost
/// polling / session-cleanup loops in `server.rs` — no stop handle, matches
/// existing P2-4 precedent for process-lifetime background tasks).
pub fn spawn_feedback_loop(
    home_dir: PathBuf,
    session_mgr: Arc<SessionManager>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BACKFILL_TICK_INTERVAL);
        loop {
            interval.tick().await;

            match run_backfill_once(&home_dir, Some(&session_mgr), DEFAULT_OUTCOME_WINDOW).await {
                Ok(report) if report.backfilled > 0 => {
                    info!(backfilled = report.backfilled, ?report.by_outcome, "proactive gate: outcomes backfilled");
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "proactive gate: backfill pass failed"),
            }

            let enabled_agents: Vec<String> = {
                let reg = agent_registry.read().await;
                reg.list()
                    .iter()
                    .filter_map(|a| {
                        let id = a
                            .dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(String::from)?;
                        proactive_gate::read_proactive_config(&a.dir)
                            .enabled
                            .then_some(id)
                    })
                    .collect()
            };
            for agent_id in enabled_agents {
                if let Err(e) = run_calibration_tick(&home_dir, &agent_id) {
                    warn!(agent = %agent_id, error = %e, "proactive gate: calibration tick failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pf-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append_line(path: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn gate_line(ts: DateTime<Utc>, agent: &str, event: &str, decision: &str) -> String {
        serde_json::json!({
            "ts": ts.to_rfc3339(),
            "agent": agent,
            "event": event,
            "score": 5,
            "threshold": 3,
            "interruptibility": 0.2,
            "decision": decision,
            "reason": "allowed",
            "latency_ms": 10,
            "outcome": null,
        })
        .to_string()
    }

    // ── classify_allow / classify_suppress (5 classes + unknown) ─────────

    #[test]
    fn classify_allow_correct_detection() {
        assert_eq!(
            classify_allow(Some(false), Some(true)),
            QuadrantOutcome::CorrectDetection
        );
    }

    #[test]
    fn classify_allow_false_alarm_explicit_dismiss() {
        assert_eq!(
            classify_allow(Some(true), Some(false)),
            QuadrantOutcome::FalseAlarm
        );
        // Dismiss wins even if a later unrelated positive interaction happened.
        assert_eq!(
            classify_allow(Some(true), Some(true)),
            QuadrantOutcome::FalseAlarm
        );
        // Dismiss is authoritative on its own — false alarm even when the
        // positive-interaction probe itself couldn't be evaluated.
        assert_eq!(
            classify_allow(Some(true), None),
            QuadrantOutcome::FalseAlarm
        );
    }

    #[test]
    fn classify_allow_non_response_no_interaction() {
        assert_eq!(
            classify_allow(Some(false), Some(false)),
            QuadrantOutcome::NonResponse
        );
    }

    #[test]
    fn classify_allow_unknown_when_either_probe_unavailable() {
        assert_eq!(classify_allow(None, Some(true)), QuadrantOutcome::Unknown);
        assert_eq!(classify_allow(Some(false), None), QuadrantOutcome::Unknown);
        assert_eq!(classify_allow(None, None), QuadrantOutcome::Unknown);
    }

    #[test]
    fn classify_suppress_missed_need_and_correct_silence() {
        assert_eq!(classify_suppress(Some(true)), QuadrantOutcome::MissedNeed);
        assert_eq!(
            classify_suppress(Some(false)),
            QuadrantOutcome::CorrectSilence
        );
        assert_eq!(classify_suppress(None), QuadrantOutcome::Unknown);
    }

    #[test]
    fn event_keywords_strips_os_prefix() {
        assert_eq!(event_keywords("os_file"), vec!["file"]);
        assert_eq!(event_keywords("os_frontmost"), vec!["frontmost"]);
        assert_eq!(event_keywords("agent_idle"), vec!["agent", "idle"]);
        assert_eq!(event_keywords("unknown"), vec!["unknown"]);
    }

    // ── run_backfill_once ─────────────────────────────────────────────

    #[tokio::test]
    async fn backfill_skips_lines_within_window() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        append_line(&path, &gate_line(Utc::now(), "a1", "os_file", "suppress"));

        let report = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(report.backfilled, 0);
        let body = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert!(v["outcome"].is_null());
    }

    #[tokio::test]
    async fn backfill_marks_unknown_when_session_mgr_absent() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let old_ts = Utc::now() - chrono::Duration::hours(2);
        append_line(&path, &gate_line(old_ts, "a1", "os_file", "suppress"));
        append_line(&path, &gate_line(old_ts, "a2", "os_file", "allow"));

        let report = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(report.backfilled, 2);
        assert_eq!(report.by_outcome.get("unknown"), Some(&2));
    }

    #[tokio::test]
    async fn backfill_allow_resolves_non_response_with_no_signals() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let old_ts = Utc::now() - chrono::Duration::hours(1);
        append_line(&path, &gate_line(old_ts, "a1", "os_file", "allow"));
        // feedback.jsonl exists but has no dismiss for this agent.
        std::fs::write(home.join("feedback.jsonl"), "").unwrap();

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let report = run_backfill_once(&home, Some(&sm), DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(report.backfilled, 1);
        assert_eq!(report.by_outcome.get("non_response"), Some(&1));
    }

    #[tokio::test]
    async fn backfill_allow_resolves_false_alarm_on_explicit_dismiss() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let old_ts = Utc::now() - chrono::Duration::hours(1);
        append_line(&path, &gate_line(old_ts, "a1", "os_file", "allow"));
        let dismiss_ts = old_ts + chrono::Duration::minutes(5);
        let fb = serde_json::json!({
            "agent_id": "a1",
            "type": "negative",
            "channel": "webchat",
            "detail": "dismissed",
            "timestamp": dismiss_ts.to_rfc3339(),
        })
        .to_string();
        std::fs::write(home.join("feedback.jsonl"), format!("{fb}\n")).unwrap();

        let report = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(report.backfilled, 1);
        assert_eq!(report.by_outcome.get("false_alarm"), Some(&1));
    }

    #[tokio::test]
    async fn backfill_is_idempotent() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let old_ts = Utc::now() - chrono::Duration::hours(1);
        append_line(&path, &gate_line(old_ts, "a1", "os_file", "suppress"));

        let first = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(first.backfilled, 1);
        let after_first = std::fs::read_to_string(&path).unwrap();

        let second = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        assert_eq!(
            second.backfilled, 0,
            "already-backfilled line must not be reprocessed"
        );
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "file content must not change on a no-op pass"
        );
    }

    #[tokio::test]
    async fn backfill_only_scans_tail_window() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let now = Utc::now();
        // Many old (within-window, not due) lines beyond the tail horizon,
        // followed by one genuinely due line at the very end.
        for _ in 0..(BACKFILL_TAIL_LINES + 5) {
            append_line(&path, &gate_line(now, "a-old", "os_file", "suppress"));
        }
        let due_ts = now - chrono::Duration::hours(1);
        append_line(&path, &gate_line(due_ts, "a-due", "os_file", "suppress"));

        let report = run_backfill_once(&home, None, DEFAULT_OUTCOME_WINDOW)
            .await
            .unwrap();
        // Only the due line (within the tail window) gets backfilled.
        assert_eq!(report.backfilled, 1);
    }

    // ── quadrant_stats ───────────────────────────────────────────────

    #[test]
    fn quadrant_stats_counts_by_outcome_within_lookback() {
        let home = temp_home();
        let path = home.join("proactive_gate.jsonl");
        let now = Utc::now();
        let mk = |outcome: &str, ts: DateTime<Utc>| {
            let mut v: serde_json::Value =
                serde_json::from_str(&gate_line(ts, "a1", "os_file", "allow")).unwrap();
            v["outcome"] = serde_json::Value::String(outcome.to_string());
            v.to_string()
        };
        append_line(&path, &mk("correct_detection", now));
        append_line(&path, &mk("false_alarm", now));
        append_line(&path, &mk("false_alarm", now));
        append_line(&path, &mk("missed_need", now));
        append_line(&path, &mk("non_response", now));
        append_line(&path, &mk("correct_silence", now));
        // Out of lookback — excluded.
        append_line(&path, &mk("false_alarm", now - chrono::Duration::days(2)));
        // Different agent — excluded.
        append_line(&path, &gate_line(now, "a2", "os_file", "allow"));

        let stats = quadrant_stats(&home, "a1", Duration::from_secs(24 * 3600)).unwrap();
        assert_eq!(stats.cd, 1);
        assert_eq!(stats.fa, 2);
        assert_eq!(stats.mn, 1);
        assert_eq!(stats.nr, 1);
        assert_eq!(stats.cs, 1);
    }

    #[test]
    fn quadrant_stats_empty_when_file_missing() {
        let home = temp_home();
        let stats = quadrant_stats(&home, "a1", Duration::from_secs(3600)).unwrap();
        assert_eq!(stats, QuadrantStats::default());
    }

    #[test]
    fn quadrant_rates_require_min_samples() {
        let mut stats = QuadrantStats {
            cd: 2,
            fa: 1,
            ..Default::default()
        };
        assert!(
            stats.fa_rate().is_none(),
            "only 3 samples, below CALIBRATION_MIN_SAMPLES"
        );
        stats.cd = 10;
        assert!(stats.fa_rate().is_some());
    }

    // ── step_calibration (pure) ────────────────────────────────────────

    #[test]
    fn calibration_high_fa_rate_raises_base() {
        let now = Utc::now();
        // Seed a state with an already-high EMA so a single tick crosses the threshold.
        let prev = AgentCalibrationState {
            ema_fa_rate: 0.6,
            t: 0.5,
            day: now.format("%Y-%m-%d").to_string(),
            base_at_day_start: 3,
            published_base: 3,
            ..Default::default()
        };
        let next = step_calibration(Some(prev), Some(0.6), None, now);
        assert!(next.t > 0.5, "t must move up on high FA rate");
        assert!(
            next.published_base >= 3,
            "base must not decrease on high FA rate"
        );
    }

    #[test]
    fn calibration_high_mn_rate_lowers_base() {
        let now = Utc::now();
        let prev = AgentCalibrationState {
            ema_mn_rate: 0.6,
            t: 0.5,
            day: now.format("%Y-%m-%d").to_string(),
            base_at_day_start: 3,
            published_base: 3,
            ..Default::default()
        };
        let next = step_calibration(Some(prev), None, Some(0.6), now);
        assert!(next.t < 0.5, "t must move down on high MN rate");
        assert!(
            next.published_base <= 3,
            "base must not increase on high MN rate"
        );
    }

    #[test]
    fn calibration_clamps_to_1_5() {
        let now = Utc::now();
        let mut state = AgentCalibrationState {
            t: 1.0,
            ema_fa_rate: 0.9,
            day: now.format("%Y-%m-%d").to_string(),
            base_at_day_start: 5,
            published_base: 5,
            ..Default::default()
        };
        for _ in 0..5 {
            state = step_calibration(Some(state), Some(0.9), None, now);
        }
        assert!(state.published_base <= MAX_SCORE);
        assert!(state.t <= 1.0);
    }

    #[test]
    fn calibration_daily_cap_limits_published_base_within_same_day() {
        let now = Utc::now();
        let mut state = AgentCalibrationState {
            day: now.format("%Y-%m-%d").to_string(),
            base_at_day_start: 3,
            published_base: 3,
            ..Default::default()
        };
        // Hammer with strongly high FA rate for many ticks the same day.
        for _ in 0..30 {
            state = step_calibration(Some(state), Some(0.95), None, now);
        }
        assert!(
            state.published_base <= 3 + CALIBRATION_MAX_DAILY_BASE_DELTA,
            "published base must not exceed the ±{} daily cap from day-start (got {})",
            CALIBRATION_MAX_DAILY_BASE_DELTA,
            state.published_base
        );
        // `t` itself is not capped — pressure is retained for the next day.
        assert!(state.t > 0.5);
    }

    #[test]
    fn calibration_day_rollover_resets_the_daily_anchor() {
        let day1 = Utc::now();
        let mut state = AgentCalibrationState {
            day: day1.format("%Y-%m-%d").to_string(),
            base_at_day_start: 3,
            published_base: 3,
            ..Default::default()
        };
        for _ in 0..30 {
            state = step_calibration(Some(state), Some(0.95), None, day1);
        }
        let capped_base = state.published_base;
        assert!(capped_base <= 3 + CALIBRATION_MAX_DAILY_BASE_DELTA);

        let day2 = day1 + chrono::Duration::days(1);
        let next_day_state = step_calibration(Some(state), Some(0.95), None, day2);
        assert_eq!(
            next_day_state.base_at_day_start, capped_base,
            "new day snapshots the previous published base"
        );
        // With sustained pressure (t already high) the new day can move further.
        assert!(next_day_state.published_base >= capped_base);
    }

    #[test]
    fn calibration_no_data_keeps_state_unchanged_besides_day() {
        let now = Utc::now();
        let prev = AgentCalibrationState {
            ema_fa_rate: 0.1,
            ema_mn_rate: 0.1,
            t: 0.5,
            day: now.format("%Y-%m-%d").to_string(),
            base_at_day_start: 3,
            published_base: 3,
            ..Default::default()
        };
        let next = step_calibration(Some(prev.clone()), None, None, now);
        assert_eq!(next.t, prev.t);
        assert_eq!(next.published_base, prev.published_base);
    }

    // ── calibrate_agent / read_calibrated_base / effective_proactive_config ──

    #[test]
    fn calibrate_agent_persists_and_reads_back() {
        let home = temp_home();
        let state = calibrate_agent(&home, "agent-x", Some(0.9), None, Utc::now()).unwrap();
        assert!(state.published_base >= DEFAULT_BASE_THRESHOLD || state.t >= 0.5);
        let read_back = read_calibrated_base(&home, "agent-x");
        assert_eq!(read_back, Some(state.published_base));
        assert_eq!(read_calibrated_base(&home, "agent-never-calibrated"), None);
    }

    #[test]
    fn effective_config_overlays_calibrated_base_only_when_present() {
        let home = temp_home();
        let base_cfg = ProactiveConfig {
            enabled: true,
            base_threshold: 3,
            max_per_hour: 4,
        };

        // No calibration yet → unchanged.
        let cfg = effective_proactive_config(base_cfg.clone(), &home, "agent-y");
        assert_eq!(cfg.base_threshold, 3);

        let _ = calibrate_agent(&home, "agent-y", Some(0.9), None, Utc::now()).unwrap();
        let cfg2 = effective_proactive_config(base_cfg, &home, "agent-y");
        let calibrated = read_calibrated_base(&home, "agent-y").unwrap();
        assert_eq!(cfg2.base_threshold, calibrated);
    }

    // ── quadrant_stats_and_emit smoke (does not panic / errors) ───────

    #[tokio::test]
    async fn quadrant_stats_and_emit_runs_without_panicking() {
        // `emit_proactive_quadrant` fire-and-forgets via `tokio::spawn`, which
        // requires an active runtime — hence `#[tokio::test]` even though
        // `quadrant_stats_and_emit` itself is a sync function.
        let home = temp_home();
        let stats = quadrant_stats_and_emit(&home, "agent-z", Duration::from_secs(3600)).unwrap();
        assert_eq!(stats, QuadrantStats::default());
    }
}
