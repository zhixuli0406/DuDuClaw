//! GVU stagnation detector (AVO §2.4; TODO-evolution-v3-2026-08.md WP0.5).
//!
//! ## The failure mode this closes
//!
//! Forensic inspection of a production install (the "B window", 2026-07-16
//! ~ 2026-08-03) found the `ceo-assistant` agent burned 20 GVU cycles with
//! `soul_versions = 0` ever applied — 14 blocked by the SOUL.md byte cap,
//! 6 rejected by the Verifier three generations in a row, each costing
//! ~4-5 minutes of LLM time. Nobody noticed until someone opened
//! `evolution.db` by hand. The evolution engine wasn't just failing, it was
//! **failing silently** — GVU can run forever without ever landing a
//! change, and nothing surfaces that fact to a human.
//!
//! This module gives that failure mode a heartbeat. It is read-only with
//! respect to GVU itself (it never blocks or mutates a run — see
//! `TODO-evolution-v3-2026-08.md` §6 "rulebase 受控放寬" for why automatic
//! intervention stays out of scope for P0) and only ever *observes*
//! `evolution.db`, then raises a signal a human can see.
//!
//! ## Signals
//!
//! Three independent, individually-sufficient stagnation signals, computed
//! from `gvu_experiment_log` (`outcome` column):
//!
//! 1. **Consecutive non-applied** — the most recent N attempts (scanning
//!    newest-first, default N=5) are all `skipped` / `deferred` /
//!    `abandoned` / `timed_out`. Catches an agent that's actively looping
//!    but never landing.
//! 2. **Zero-apply window** — at least one trigger fired in the last D days
//!    (default 14) but none of them resulted in `applied`. A slower-bleed
//!    variant of signal 1; catches an agent whose applies are rare enough
//!    that N consecutive failures never quite accumulates, but D days still
//!    pass with nothing landing. (An agent with *zero* triggers at all in
//!    the window is a *different* diagnosis — no input, not stagnation —
//!    so this signal requires `trigger_count > 0`.)
//! 3. **Repeated rejection reason** — among the most recent `lookback`
//!    non-applied experiment-log entries, the same description prefix
//!    recurs `>= min_occurrences` times. Reuses the spirit of
//!    `goal_loop`'s oscillation detector (two identical rejection payloads
//!    back-to-back means "stuck", not "still exploring").
//!
//! ## De-duplication
//!
//! Detection alone is useless if it re-alerts every 30-minute tick forever.
//! [`StagnationMonitor`] remembers the last-alerted signal *fingerprint*
//! (the sorted set of signal kinds, not their exact counts — counts grow
//! every tick while still stagnant) per agent and only re-emits when that
//! fingerprint changes: newly stagnant, a different signal combination, or
//! recovered-then-relapsed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::version_store::{ExperimentLogEntry, VersionStore};
use crate::prediction::engine::PredictionEngine;

// ── Config ───────────────────────────────────────────────────────────────

/// Per-agent thresholds for the stagnation detector, read from
/// `agent.toml [evolution]`. Self-contained raw-TOML parsing (mirrors
/// `gvu::trigger::agent_gvu_enabled`) rather than depending on the full
/// `AgentConfig`/`EvolutionConfig` struct, so a minimal/partial agent.toml
/// (common in tests and templates) still yields sane defaults instead of a
/// hard parse failure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GvuStagnationConfig {
    /// Master switch for this detector, per agent. Default `true` — unlike
    /// GVU itself (opt-in, WP0.1), *observing* whether GVU is stuck is safe
    /// to default on: it never mutates anything, only alerts.
    pub enabled: bool,
    /// Signal 1 threshold: consecutive non-applied outcomes.
    pub consecutive_non_applied_threshold: u32,
    /// Signal 2 threshold: window size in days.
    pub zero_apply_window_days: u32,
    /// Signal 3 threshold: minimum repeat count to flag a reason as "stuck".
    pub repeated_reason_min_occurrences: u32,
    /// Signal 3 window: how many recent non-applied entries to scan.
    pub repeated_reason_lookback: u32,
}

impl Default for GvuStagnationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            consecutive_non_applied_threshold: 5,
            zero_apply_window_days: 14,
            repeated_reason_min_occurrences: 3,
            repeated_reason_lookback: 8,
        }
    }
}

impl GvuStagnationConfig {
    /// Read `[evolution] gvu_stagnation_*` keys from `agent_dir/agent.toml`.
    ///
    /// Fail-open on read errors (missing file / malformed TOML / absent
    /// keys all fall back to [`Self::default`]): a detector whose entire
    /// job is surfacing silent failures must not itself become one by
    /// silently skipping every agent whose config it can't parse.
    pub fn from_agent_dir(agent_dir: &Path) -> Self {
        let default = Self::default();
        let path = agent_dir.join("agent.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return default;
        };
        let Ok(value) = raw.parse::<toml::Value>() else {
            return default;
        };
        let Some(evo) = value.get("evolution") else {
            return default;
        };
        let as_u32 = |key: &str, fallback: u32| -> u32 {
            evo.get(key)
                .and_then(|v| v.as_integer())
                .filter(|v| *v >= 1)
                .map(|v| v as u32)
                .unwrap_or(fallback)
        };
        Self {
            enabled: evo
                .get("gvu_stagnation_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(default.enabled),
            consecutive_non_applied_threshold: as_u32(
                "gvu_stagnation_consecutive_threshold",
                default.consecutive_non_applied_threshold,
            ),
            zero_apply_window_days: as_u32(
                "gvu_stagnation_zero_apply_days",
                default.zero_apply_window_days,
            ),
            repeated_reason_min_occurrences: as_u32(
                "gvu_stagnation_repeated_reason_min",
                default.repeated_reason_min_occurrences,
            ),
            repeated_reason_lookback: as_u32(
                "gvu_stagnation_repeated_reason_lookback",
                default.repeated_reason_lookback,
            ),
        }
    }
}

// ── Row classification ──────────────────────────────────────────────────

/// Outcome label `gvu/aee/run.rs` records for a round that exited at the
/// pre-flight stagnation check without attempting anything.
pub const ESCALATED_OUTCOME: &str = "escalated";

/// Description prefix `run.rs` stamps on those escalation records. Doubles
/// as the legacy filter: rows written before the `escalated` label existed
/// (≤ 2026-08-20) carry outcome `abandoned` with this prefix.
pub const ESCALATION_DESCRIPTION_PREFIX: &str = "AEE escalated to a human";

/// A monitor meta-record — the log echo of a stagnation escalation — not an
/// evolution attempt. Counting these as attempts/rejections is what armed
/// the 2026-08 trader-lead deadlock: each escalation record became the
/// "repeated rejection" that escalated the next round, forever.
fn is_escalation_meta(e: &ExperimentLogEntry) -> bool {
    e.outcome == ESCALATED_OUTCOME || e.description.starts_with(ESCALATION_DESCRIPTION_PREFIX)
}

/// A real evolution attempt: the inner loop generated candidate(s) that then
/// applied or failed. `skipped` rounds (cooldown, generator unavailable,
/// "proposed no change — a legitimate empty answer") and escalation
/// meta-records are neither attempts nor rejections, so no signal counts
/// them (2026-08-20 拍板 B：卡片上的「嘗試」只算真的試過的輪).
fn is_real_attempt(e: &ExperimentLogEntry) -> bool {
    e.outcome != "skipped" && !is_escalation_meta(e)
}

// ── Signals & snapshot ──────────────────────────────────────────────────

/// One fired stagnation signal, with the numbers that triggered it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StagnationSignal {
    /// Signal 1: `count` consecutive non-applied outcomes (newest-first),
    /// `count >= threshold`.
    ConsecutiveNonApplied { count: u32, threshold: u32 },
    /// Signal 2: `trigger_count` GVU attempts inside the last `days` days,
    /// zero of which applied.
    ZeroApplyWindow { days: u32, trigger_count: u32 },
    /// Signal 3: the same description prefix recurred `occurrences` times
    /// (>= `threshold`) among the most recent non-applied log entries.
    RepeatedRejectionReason {
        occurrences: u32,
        threshold: u32,
        reason_prefix: String,
    },
}

impl StagnationSignal {
    fn kind(&self) -> &'static str {
        match self {
            StagnationSignal::ConsecutiveNonApplied { .. } => "consecutive_non_applied",
            StagnationSignal::ZeroApplyWindow { .. } => "zero_apply_window",
            StagnationSignal::RepeatedRejectionReason { .. } => "repeated_rejection_reason",
        }
    }

    /// Human-readable (zh-TW) one-liner for Activity Feed / log messages.
    fn describe_zh(&self) -> String {
        match self {
            StagnationSignal::ConsecutiveNonApplied { count, threshold } => {
                format!("連續 {count} 次 GVU 皆未成功套用（門檻 {threshold}）")
            }
            StagnationSignal::ZeroApplyWindow {
                days,
                trigger_count,
            } => {
                format!("過去 {days} 天內觸發 {trigger_count} 次 GVU，但零次套用成功")
            }
            StagnationSignal::RepeatedRejectionReason {
                occurrences,
                threshold,
                reason_prefix,
            } => {
                format!("拒絕原因重複 {occurrences} 次（門檻 {threshold}）：「{reason_prefix}…」")
            }
        }
    }
}

/// The result of one detection pass for one agent.
#[derive(Debug, Clone, Serialize)]
pub struct StagnationSnapshot {
    pub agent_id: String,
    pub signals: Vec<StagnationSignal>,
    pub checked_at: DateTime<Utc>,
    /// Timestamp of the newest real non-applied attempt (see
    /// [`is_real_attempt`]), if any. Together with `latest_escalation_at`
    /// this lets the AEE pre-flight escalate once per rejection streak
    /// instead of once per round: it only escalates when rejection evidence
    /// NEWER than the last escalation exists (the deadlock exit).
    pub latest_real_rejection_at: Option<DateTime<Utc>>,
    /// Timestamp of the newest escalation meta-record, if any.
    pub latest_escalation_at: Option<DateTime<Utc>>,
}

impl StagnationSnapshot {
    /// True iff at least one signal fired.
    pub fn is_stagnant(&self) -> bool {
        !self.signals.is_empty()
    }

    /// Deterministic fingerprint of *which kinds* of signal are firing —
    /// deliberately excludes exact counts (which grow every tick while
    /// still stagnant and would defeat de-duplication). Empty signals ⇒
    /// `"none"`.
    pub fn fingerprint(&self) -> String {
        if self.signals.is_empty() {
            return "none".to_string();
        }
        let mut kinds: Vec<&str> = self.signals.iter().map(|s| s.kind()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.join(",")
    }

    /// zh-TW human summary for Activity Feed posts / evolution-event context.
    pub fn summary_zh(&self) -> String {
        let parts: Vec<String> = self.signals.iter().map(|s| s.describe_zh()).collect();
        format!(
            "GVU 進化迴圈疑似停滯（agent={}）：{}",
            self.agent_id,
            parts.join("；")
        )
    }
}

// ── Pure detection logic ────────────────────────────────────────────────

/// Signal 1: scan `experiments` (must be newest-first) from the front;
/// count consecutive non-`applied` outcomes among real attempts
/// ([`is_real_attempt`] — `skipped` rounds and escalation meta-records are
/// transparent: they neither count nor break the streak). Stops (returns
/// `None`) as soon as an `applied` row is hit before reaching `threshold`.
fn detect_consecutive_non_applied(
    experiments: &[ExperimentLogEntry],
    threshold: u32,
) -> Option<StagnationSignal> {
    if threshold == 0 {
        return None;
    }
    let mut count: u32 = 0;
    for e in experiments.iter().filter(|e| is_real_attempt(e)) {
        if e.outcome == "applied" {
            break;
        }
        count += 1;
        if count >= threshold {
            return Some(StagnationSignal::ConsecutiveNonApplied { count, threshold });
        }
    }
    None
}

/// Signal 2: among entries within the last `window_days`, require at least
/// one (else this is "no input", a different diagnosis) and zero `applied`.
fn detect_zero_apply_window(
    experiments: &[ExperimentLogEntry],
    window_days: u32,
) -> Option<StagnationSignal> {
    if window_days == 0 {
        return None;
    }
    let cutoff = Utc::now() - chrono::Duration::days(window_days as i64);
    let in_window: Vec<&ExperimentLogEntry> = experiments
        .iter()
        .filter(|e| e.timestamp >= cutoff && is_real_attempt(e))
        .collect();
    if in_window.is_empty() {
        return None;
    }
    let applied = in_window.iter().filter(|e| e.outcome == "applied").count();
    if applied == 0 {
        return Some(StagnationSignal::ZeroApplyWindow {
            days: window_days,
            trigger_count: in_window.len() as u32,
        });
    }
    None
}

/// Signal 3: among the most recent non-applied entries *since the last
/// `applied` row* (newest-first, capped at `lookback`), find a description
/// prefix (first 24 chars, CJK-safe) that recurs `>= min_occurrences` times.
/// Returns the most-repeated prefix when several qualify.
///
/// Scoped to "since last applied" (via `take_while`, mirroring signal 1's
/// stop-at-first-applied scan) rather than a flat most-recent-N-rows count:
/// once a change lands, the rejection streak that preceded it is a resolved
/// historical fact, not an ongoing stuck state — an agent that just recovered
/// must not be re-flagged purely because old rejected attempts are still
/// within an unscoped lookback window.
fn detect_repeated_rejection_reason(
    experiments: &[ExperimentLogEntry],
    lookback: u32,
    min_occurrences: u32,
) -> Option<StagnationSignal> {
    if min_occurrences == 0 || lookback == 0 {
        return None;
    }
    const PREFIX_CHARS: usize = 24;
    let mut counts: HashMap<String, u32> = HashMap::new();
    // Filter BEFORE take_while/take: `skipped` rounds and escalation
    // meta-records are not rejections — a "legitimate empty answer" repeated
    // N times is a quiet agent, and counting the escalation records
    // themselves made the signal self-feeding (2026-08 trader-lead deadlock:
    // "AEE escalated to a human" is exactly 24 chars, so every escalation
    // reinforced the very signal that caused it).
    for e in experiments
        .iter()
        .filter(|e| is_real_attempt(e))
        .take_while(|e| e.outcome != "applied")
        .take(lookback as usize)
    {
        let prefix = duduclaw_core::truncate_chars(&e.description, PREFIX_CHARS);
        *counts.entry(prefix).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c >= min_occurrences)
        // Deterministic tie-break: highest count, then lexicographic prefix.
        .max_by(|(pa, ca), (pb, cb)| ca.cmp(cb).then_with(|| pb.cmp(pa)))
        .map(
            |(reason_prefix, occurrences)| StagnationSignal::RepeatedRejectionReason {
                occurrences,
                threshold: min_occurrences,
                reason_prefix,
            },
        )
}

/// Run all three signals for one agent against its `gvu_experiment_log`
/// history. Exposed standalone (not just via [`StagnationMonitor`]) so a
/// future dashboard RPC can query on-demand without needing the alerting
/// machinery (WP0.5 item 5 — dashboard wiring is a follow-up WP).
pub fn stagnation_snapshot(
    vs: &VersionStore,
    agent_id: &str,
    cfg: &GvuStagnationConfig,
) -> StagnationSnapshot {
    if !cfg.enabled {
        return StagnationSnapshot {
            agent_id: agent_id.to_string(),
            signals: Vec::new(),
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
    }
    // One history fetch big enough to serve all three signals: signal 2
    // (time-windowed) needs enough rows to plausibly cover
    // `zero_apply_window_days` at realistic trigger volume (the B-window
    // forensic sample was ~20 triggers over 3 weeks); 100 comfortably covers
    // that with headroom while staying a cheap single query.
    let lookback = cfg
        .consecutive_non_applied_threshold
        .max(cfg.repeated_reason_lookback)
        .max(100);
    let experiments = vs.get_experiments(agent_id, lookback as usize);

    let mut signals = Vec::new();
    if let Some(s) =
        detect_consecutive_non_applied(&experiments, cfg.consecutive_non_applied_threshold)
    {
        signals.push(s);
    }
    if let Some(s) = detect_zero_apply_window(&experiments, cfg.zero_apply_window_days) {
        signals.push(s);
    }
    if let Some(s) = detect_repeated_rejection_reason(
        &experiments,
        cfg.repeated_reason_lookback,
        cfg.repeated_reason_min_occurrences,
    ) {
        signals.push(s);
    }

    // Streak bookkeeping for the AEE pre-flight's once-per-streak escalation
    // (see the field docs on `StagnationSnapshot`). `experiments` is
    // newest-first, so `find` returns the newest qualifying row.
    let latest_real_rejection_at = experiments
        .iter()
        .find(|e| is_real_attempt(e) && e.outcome != "applied")
        .map(|e| e.timestamp);
    let latest_escalation_at = experiments
        .iter()
        .find(|e| is_escalation_meta(e))
        .map(|e| e.timestamp);

    StagnationSnapshot {
        agent_id: agent_id.to_string(),
        signals,
        checked_at: Utc::now(),
        latest_real_rejection_at,
        latest_escalation_at,
    }
}

// ── Scheduled monitor (alerting + de-dup) ───────────────────────────────

/// Periodically sweeps every agent with GVU history, raises a de-duplicated
/// alert (Activity Feed + evolution event + `tracing::warn!`) the first
/// time an agent enters a given stagnation state, and stays silent on
/// unchanged subsequent ticks.
pub struct StagnationMonitor {
    version_store: VersionStore,
    agents_dir: PathBuf,
    home_dir: PathBuf,
    prediction_engine: Arc<PredictionEngine>,
    /// `agent_id` → fingerprint of the last state alerted on. Absence means
    /// "never alerted / currently not stagnant".
    last_alerted: Arc<Mutex<HashMap<String, String>>>,
}

impl StagnationMonitor {
    pub fn new(
        gvu_db_path: &Path,
        encryption_key: Option<[u8; 32]>,
        agents_dir: PathBuf,
        home_dir: PathBuf,
        prediction_engine: Arc<PredictionEngine>,
    ) -> Self {
        Self {
            version_store: VersionStore::with_crypto(gvu_db_path, encryption_key.as_ref()),
            agents_dir,
            home_dir,
            prediction_engine,
            last_alerted: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// One sweep: every agent_id with at least one `gvu_experiment_log` row
    /// gets checked. Returns every snapshot computed this tick (stagnant or
    /// not) for callers that want to inspect the full picture (tests,
    /// future dashboard RPC).
    pub async fn tick(&self) -> Vec<StagnationSnapshot> {
        let agent_ids = self.distinct_agent_ids();
        let mut snapshots = Vec::with_capacity(agent_ids.len());
        for agent_id in agent_ids {
            let agent_dir = self.agents_dir.join(&agent_id);
            // GVU-disabled agents cannot be "stuck evolving" — they opted out
            // of evolving at all. Scanning them anyway turned rejected
            // trigger attempts into a standing false alarm (LWM containers:
            // 152 stagnation events = 71% of the activity feed, for agents
            // with `gvu_enabled = false`). Alerts belong to opted-in agents.
            if !super::trigger::agent_gvu_enabled(&agent_dir) {
                continue;
            }
            let cfg = GvuStagnationConfig::from_agent_dir(&agent_dir);
            let snapshot = stagnation_snapshot(&self.version_store, &agent_id, &cfg);
            if snapshot.is_stagnant() {
                self.maybe_alert(&snapshot).await;
            } else {
                // Recovered (or never stagnant) — clear any remembered
                // fingerprint so a future relapse re-alerts even if it
                // relapses into the exact same signal combination.
                self.last_alerted.lock().await.remove(&agent_id);
            }
            snapshots.push(snapshot);
        }
        snapshots
    }

    /// Long-running task: tick on a fixed interval until cancelled. Mirrors
    /// `ObservationFinalizer::run` (same 30-minute cadence when wired up in
    /// `server.rs`).
    pub async fn run(self: Arc<Self>, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = self.tick().await;
        }
    }

    // ── Internal ──

    fn distinct_agent_ids(&self) -> Vec<String> {
        let Ok(conn) = rusqlite::Connection::open(self.version_store.db_path_ref()) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare("SELECT DISTINCT agent_id FROM gvu_experiment_log") else {
            return Vec::new();
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    async fn maybe_alert(&self, snapshot: &StagnationSnapshot) {
        let fingerprint = snapshot.fingerprint();
        {
            let mut seen = self.last_alerted.lock().await;
            if seen.get(&snapshot.agent_id) == Some(&fingerprint) {
                debug!(
                    agent = %snapshot.agent_id,
                    fingerprint = %fingerprint,
                    "GVU stagnation unchanged since last alert — staying quiet"
                );
                return;
            }
            seen.insert(snapshot.agent_id.clone(), fingerprint);
        }

        let summary = snapshot.summary_zh();
        warn!(
            agent = %snapshot.agent_id,
            signals = ?snapshot.signals,
            "GVU stagnation detected"
        );

        self.prediction_engine.log_evolution_event(
            "gvu_stagnation_detected",
            &snapshot.agent_id,
            None,
            None,
            Some(&summary),
            None,
            None,
        );

        self.post_activity(snapshot, &summary).await;

        // Wave 2a: a stuck evolution loop is worth an actual message, not just
        // a feed row — the B-window sample burned 20 cycles with zero applies
        // before a human noticed, from a manual DB inspection. De-duplicated
        // by the fingerprint check above, so a persistently stagnant agent
        // produces exactly one push per state change, not one per tick.
        // L1: a stalled evolution loop is a real finding, but nothing about
        // it is fixable at 03:00 and nothing degrades further by waiting.
        let outcome = crate::goal_notify::notify_agent_plain(
            &self.home_dir,
            &snapshot.agent_id,
            crate::notify_governance::NotifyLevel::Fyi,
            "evolution.stagnation",
            &summary,
        )
        .await;
        if matches!(outcome, crate::goal_notify::NotifyOutcome::SendFailed) {
            debug!(agent = %snapshot.agent_id, "stagnation monitor: channel push failed (non-fatal)");
        }
    }

    /// Best-effort append to the dashboard Activity Feed. Mirrors
    /// `goal_loop::GoalLoopDriver::post_activity` — a failure here must not
    /// break the monitor, it's telemetry, not control flow.
    async fn post_activity(&self, snapshot: &StagnationSnapshot, summary: &str) {
        let store = match crate::task_store::TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "stagnation monitor: failed to open task store (non-fatal)");
                return;
            }
        };
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "gvu_stagnation_detected".to_string(),
            agent_id: snapshot.agent_id.clone(),
            task_id: None,
            summary: summary.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            metadata: serde_json::to_string(&snapshot.signals).ok(),
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(error = %e, "stagnation monitor: activity append failed (non-fatal)");
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("evolution.db");
        (dir, path)
    }

    fn entry(
        agent_id: &str,
        outcome: &str,
        description: &str,
        timestamp: DateTime<Utc>,
    ) -> ExperimentLogEntry {
        ExperimentLogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            timestamp,
            generations_used: 3,
            generations_budget: 3,
            duration_secs: 42.0,
            outcome: outcome.to_string(),
            description: description.to_string(),
        }
    }

    // ── Signal 1: consecutive non-applied ──────────────────────────

    #[test]
    fn consecutive_non_applied_fires_at_threshold() {
        let now = Utc::now();
        let experiments: Vec<_> = (0..5)
            .map(|i| {
                entry(
                    "a",
                    "abandoned",
                    "generic failure",
                    now - chrono::Duration::hours(i),
                )
            })
            .collect();
        let sig = detect_consecutive_non_applied(&experiments, 5);
        assert!(matches!(
            sig,
            Some(StagnationSignal::ConsecutiveNonApplied {
                count: 5,
                threshold: 5
            })
        ));
    }

    #[test]
    fn consecutive_non_applied_stops_at_applied_row() {
        let now = Utc::now();
        let mut experiments: Vec<_> = (0..4)
            .map(|i| {
                entry(
                    "a",
                    "abandoned",
                    "generic failure",
                    now - chrono::Duration::hours(i),
                )
            })
            .collect();
        // 5th (older) row is applied — should break the streak below threshold 5.
        experiments.push(entry(
            "a",
            "applied",
            "approved",
            now - chrono::Duration::hours(4),
        ));
        assert!(detect_consecutive_non_applied(&experiments, 5).is_none());
    }

    #[test]
    fn consecutive_non_applied_below_threshold_is_none() {
        let now = Utc::now();
        let experiments: Vec<_> = (0..3)
            .map(|i| entry("a", "skipped", "cooldown", now - chrono::Duration::hours(i)))
            .collect();
        assert!(detect_consecutive_non_applied(&experiments, 5).is_none());
    }

    // ── Signal 2: zero-apply window ─────────────────────────────────

    #[test]
    fn zero_apply_window_fires_when_no_applies_in_window() {
        let now = Utc::now();
        let experiments = vec![
            entry("a", "abandoned", "x", now - chrono::Duration::days(1)),
            entry("a", "deferred", "y", now - chrono::Duration::days(5)),
            entry("a", "abandoned", "z", now - chrono::Duration::days(10)),
        ];
        let sig = detect_zero_apply_window(&experiments, 14);
        assert!(matches!(
            sig,
            Some(StagnationSignal::ZeroApplyWindow {
                days: 14,
                trigger_count: 3
            })
        ));
    }

    #[test]
    fn zero_apply_window_none_when_an_apply_landed() {
        let now = Utc::now();
        let experiments = vec![
            entry("a", "abandoned", "x", now - chrono::Duration::days(1)),
            entry("a", "applied", "approved", now - chrono::Duration::days(5)),
        ];
        assert!(detect_zero_apply_window(&experiments, 14).is_none());
    }

    #[test]
    fn zero_apply_window_none_when_no_triggers_at_all() {
        // No rows in-window at all is a different diagnosis ("no input"),
        // not stagnation — must not fire.
        let now = Utc::now();
        let experiments = vec![entry(
            "a",
            "abandoned",
            "x",
            now - chrono::Duration::days(90),
        )];
        assert!(detect_zero_apply_window(&experiments, 14).is_none());
    }

    #[test]
    fn zero_apply_window_ignores_rows_outside_window() {
        let now = Utc::now();
        let experiments = vec![
            entry("a", "applied", "approved", now - chrono::Duration::days(90)), // outside window
            entry("a", "abandoned", "x", now - chrono::Duration::days(2)),
        ];
        // Only the in-window abandoned row counts, and it has zero applies.
        let sig = detect_zero_apply_window(&experiments, 14);
        assert!(matches!(
            sig,
            Some(StagnationSignal::ZeroApplyWindow {
                days: 14,
                trigger_count: 1
            })
        ));
    }

    // ── Signal 3: repeated rejection reason ─────────────────────────

    #[test]
    fn repeated_reason_fires_on_shared_prefix() {
        let now = Utc::now();
        let experiments = vec![
            entry(
                "a",
                "abandoned",
                "L1 blocked: forbidden phrase detected in body",
                now,
            ),
            entry(
                "a",
                "deferred",
                "L1 blocked: forbidden phrase detected again",
                now - chrono::Duration::hours(1),
            ),
            entry(
                "a",
                "abandoned",
                "L1 blocked: forbidden phrase detected once more",
                now - chrono::Duration::hours(2),
            ),
        ];
        let sig = detect_repeated_rejection_reason(&experiments, 8, 3);
        match sig {
            Some(StagnationSignal::RepeatedRejectionReason {
                occurrences,
                threshold,
                reason_prefix,
            }) => {
                assert_eq!(occurrences, 3);
                assert_eq!(threshold, 3);
                assert!(reason_prefix.starts_with("L1 blocked"));
            }
            other => panic!("expected RepeatedRejectionReason, got {other:?}"),
        }
    }

    #[test]
    fn repeated_reason_none_when_reasons_differ() {
        let now = Utc::now();
        let experiments = vec![
            entry("a", "abandoned", "L1 blocked: reason A", now),
            entry(
                "a",
                "deferred",
                "L2 judge rejected: reason B",
                now - chrono::Duration::hours(1),
            ),
            entry(
                "a",
                "abandoned",
                "wall-clock timeout",
                now - chrono::Duration::hours(2),
            ),
        ];
        assert!(detect_repeated_rejection_reason(&experiments, 8, 3).is_none());
    }

    #[test]
    fn repeated_reason_ignores_applied_rows() {
        let now = Utc::now();
        // 3 identical descriptions, but on `applied` outcomes — must not count.
        let experiments = vec![
            entry("a", "applied", "approved at generation 1", now),
            entry(
                "a",
                "applied",
                "approved at generation 1",
                now - chrono::Duration::hours(1),
            ),
            entry(
                "a",
                "applied",
                "approved at generation 1",
                now - chrono::Duration::hours(2),
            ),
        ];
        assert!(detect_repeated_rejection_reason(&experiments, 8, 3).is_none());
    }

    // ── 2026-08-20 self-feeding escalation deadlock regression ──────

    #[test]
    fn skipped_rounds_fire_no_signal() {
        // 10 identical "no change" rounds = a quiet agent, not a stuck one.
        // Pre-fix, their shared description prefix tripped signal 3, which
        // armed the AEE pre-flight escalation and started the deadlock.
        let now = Utc::now();
        let experiments: Vec<_> = (0..10)
            .map(|i| {
                entry(
                    "a",
                    "skipped",
                    "AEE inner loop proposed no change (a legitimate empty answer)",
                    now - chrono::Duration::hours(i),
                )
            })
            .collect();
        assert!(detect_consecutive_non_applied(&experiments, 5).is_none());
        assert!(detect_zero_apply_window(&experiments, 14).is_none());
        assert!(detect_repeated_rejection_reason(&experiments, 8, 3).is_none());
    }

    #[test]
    fn escalation_meta_records_fire_no_signal() {
        // The exact trader-lead incident shape: 7 escalation records (legacy
        // label — outcome `abandoned` + the escalation description prefix)
        // stacked on 3 quiet skipped rounds. All three signals fired
        // pre-fix; none may fire now.
        let now = Utc::now();
        let mut experiments: Vec<_> = (0..7)
            .map(|i| {
                entry(
                    "a",
                    "abandoned",
                    "AEE escalated to a human: the same rejection has fired 3 times",
                    now - chrono::Duration::hours(i),
                )
            })
            .collect();
        experiments.extend((7..10).map(|i| {
            entry(
                "a",
                "skipped",
                "AEE inner loop proposed no change (a legitimate empty answer)",
                now - chrono::Duration::hours(i),
            )
        }));
        assert!(detect_consecutive_non_applied(&experiments, 5).is_none());
        assert!(detect_zero_apply_window(&experiments, 14).is_none());
        assert!(detect_repeated_rejection_reason(&experiments, 8, 3).is_none());
    }

    #[test]
    fn new_escalated_outcome_is_meta_regardless_of_description() {
        let now = Utc::now();
        let experiments: Vec<_> = (0..5)
            .map(|i| {
                entry(
                    "a",
                    ESCALATED_OUTCOME,
                    "some future wording",
                    now - chrono::Duration::hours(i),
                )
            })
            .collect();
        assert!(detect_consecutive_non_applied(&experiments, 5).is_none());
        assert!(detect_repeated_rejection_reason(&experiments, 8, 3).is_none());
    }

    #[test]
    fn meta_rows_are_transparent_not_streak_breaking() {
        // Real rejections interleaved with escalation meta rows: the meta
        // rows neither count nor reset the consecutive streak, and the REAL
        // repeated reason still surfaces through signal 3.
        let now = Utc::now();
        let mut experiments = Vec::new();
        for i in 0..5i64 {
            experiments.push(entry(
                "a",
                "abandoned",
                "L1 blocked: forbidden phrase",
                now - chrono::Duration::hours(2 * i),
            ));
            experiments.push(entry(
                "a",
                ESCALATED_OUTCOME,
                "AEE escalated to a human: x",
                now - chrono::Duration::hours(2 * i + 1),
            ));
        }
        assert!(matches!(
            detect_consecutive_non_applied(&experiments, 5),
            Some(StagnationSignal::ConsecutiveNonApplied { count: 5, .. })
        ));
        assert!(matches!(
            detect_repeated_rejection_reason(&experiments, 8, 3),
            Some(StagnationSignal::RepeatedRejectionReason { .. })
        ));
    }

    #[test]
    fn snapshot_streak_bookkeeping_via_real_db() {
        // Newest row is an escalation meta-record, below it a real
        // rejection: `latest_escalation_at >= latest_real_rejection_at`,
        // which is exactly the state where the AEE pre-flight must NOT
        // escalate again (once per streak).
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-w",
            3,
            3,
            StdDuration::from_secs(60),
            "abandoned",
            "L1 blocked: forbidden phrase",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-w",
            0,
            3,
            StdDuration::from_secs(1),
            ESCALATED_OUTCOME,
            "AEE escalated to a human: L1 blocked repeated",
        ));
        let cfg = GvuStagnationConfig::default();
        let snap = stagnation_snapshot(&vs, "agent-w", &cfg);
        let (esc, rej) = (
            snap.latest_escalation_at.expect("escalation recorded"),
            snap.latest_real_rejection_at.expect("rejection recorded"),
        );
        assert!(esc >= rej, "escalation row is newer than the rejection");
    }

    // ── Snapshot / fingerprint ───────────────────────────────────────

    #[test]
    fn snapshot_not_stagnant_when_no_signals() {
        let snap = StagnationSnapshot {
            agent_id: "a".to_string(),
            signals: vec![],
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
        assert!(!snap.is_stagnant());
        assert_eq!(snap.fingerprint(), "none");
    }

    #[test]
    fn fingerprint_stable_across_growing_counts() {
        let snap1 = StagnationSnapshot {
            agent_id: "a".to_string(),
            signals: vec![StagnationSignal::ConsecutiveNonApplied {
                count: 5,
                threshold: 5,
            }],
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
        let snap2 = StagnationSnapshot {
            agent_id: "a".to_string(),
            signals: vec![StagnationSignal::ConsecutiveNonApplied {
                count: 9,
                threshold: 5,
            }],
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
        // Same signal *kind*, different count — fingerprint must not change,
        // otherwise a standing stagnation would re-alert every tick.
        assert_eq!(snap1.fingerprint(), snap2.fingerprint());
    }

    #[test]
    fn fingerprint_changes_with_different_signal_set() {
        let snap1 = StagnationSnapshot {
            agent_id: "a".to_string(),
            signals: vec![StagnationSignal::ConsecutiveNonApplied {
                count: 5,
                threshold: 5,
            }],
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
        let snap2 = StagnationSnapshot {
            agent_id: "a".to_string(),
            signals: vec![StagnationSignal::ZeroApplyWindow {
                days: 14,
                trigger_count: 3,
            }],
            checked_at: Utc::now(),
            latest_real_rejection_at: None,
            latest_escalation_at: None,
        };
        assert_ne!(snap1.fingerprint(), snap2.fingerprint());
    }

    // ── Config parsing ───────────────────────────────────────────────

    #[test]
    fn config_defaults_when_agent_toml_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = GvuStagnationConfig::from_agent_dir(tmp.path());
        assert_eq!(cfg, GvuStagnationConfig::default());
    }

    #[test]
    fn config_reads_explicit_overrides() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("agent.toml"),
            "[evolution]\n\
             gvu_stagnation_enabled = false\n\
             gvu_stagnation_consecutive_threshold = 3\n\
             gvu_stagnation_zero_apply_days = 7\n\
             gvu_stagnation_repeated_reason_min = 2\n\
             gvu_stagnation_repeated_reason_lookback = 4\n",
        )
        .unwrap();
        let cfg = GvuStagnationConfig::from_agent_dir(tmp.path());
        assert!(!cfg.enabled);
        assert_eq!(cfg.consecutive_non_applied_threshold, 3);
        assert_eq!(cfg.zero_apply_window_days, 7);
        assert_eq!(cfg.repeated_reason_min_occurrences, 2);
        assert_eq!(cfg.repeated_reason_lookback, 4);
    }

    // ── stagnation_snapshot() against a real (temp) SQLite DB ─────────
    // WP0.5 explicitly asks for tests built against a temp SQLite DB, not
    // just the pure detection functions — this exercises the real
    // VersionStore -> gvu_experiment_log round trip.

    #[test]
    fn snapshot_detects_consecutive_non_applied_via_real_db() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        for _ in 0..5 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent-x",
                3,
                3,
                StdDuration::from_secs(120),
                "abandoned",
                "All generations rejected — deferred retry #1",
            ));
        }
        let cfg = GvuStagnationConfig::default();
        let snap = stagnation_snapshot(&vs, "agent-x", &cfg);
        assert!(snap.is_stagnant());
        assert!(
            snap.signals
                .iter()
                .any(|s| matches!(s, StagnationSignal::ConsecutiveNonApplied { .. }))
        );
    }

    #[test]
    fn snapshot_detects_zero_apply_window_via_real_db() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        // Only 2 rows — below the consecutive-threshold (5) but enough to
        // trip the zero-apply-window signal on its own.
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-y",
            3,
            3,
            StdDuration::from_secs(60),
            "deferred",
            "deferred retry #1",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-y",
            3,
            3,
            StdDuration::from_secs(60),
            "skipped",
            "GVU cooldown active",
        ));
        let cfg = GvuStagnationConfig {
            consecutive_non_applied_threshold: 5,
            ..GvuStagnationConfig::default()
        };
        let snap = stagnation_snapshot(&vs, "agent-y", &cfg);
        assert!(snap.is_stagnant());
        assert!(
            snap.signals
                .iter()
                .any(|s| matches!(s, StagnationSignal::ZeroApplyWindow { .. }))
        );
        assert!(
            !snap
                .signals
                .iter()
                .any(|s| matches!(s, StagnationSignal::ConsecutiveNonApplied { .. }))
        );
    }

    #[test]
    fn snapshot_detects_repeated_rejection_reason_via_real_db() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        for _ in 0..3 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent-z",
                3,
                3,
                StdDuration::from_secs(90),
                "abandoned",
                "L1 blocked: forbidden phrase detected — see CONTRACT.toml must_not",
            ));
        }
        let cfg = GvuStagnationConfig::default();
        let snap = stagnation_snapshot(&vs, "agent-z", &cfg);
        assert!(
            snap.signals
                .iter()
                .any(|s| matches!(s, StagnationSignal::RepeatedRejectionReason { .. }))
        );
    }

    #[test]
    fn snapshot_healthy_agent_has_no_signals() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-ok",
            2,
            3,
            StdDuration::from_secs(30),
            "applied",
            "Approved at generation 2",
        ));
        let cfg = GvuStagnationConfig::default();
        let snap = stagnation_snapshot(&vs, "agent-ok", &cfg);
        assert!(!snap.is_stagnant());
    }

    #[test]
    fn snapshot_disabled_config_yields_no_signals_even_when_stagnant() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        for _ in 0..5 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent-off",
                3,
                3,
                StdDuration::from_secs(120),
                "abandoned",
                "x",
            ));
        }
        let cfg = GvuStagnationConfig {
            enabled: false,
            ..GvuStagnationConfig::default()
        };
        let snap = stagnation_snapshot(&vs, "agent-off", &cfg);
        assert!(!snap.is_stagnant());
    }

    // ── StagnationMonitor: alert de-duplication ────────────────────────

    #[tokio::test]
    async fn monitor_alerts_once_then_stays_quiet_for_same_state() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        for _ in 0..5 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent-dup",
                3,
                3,
                StdDuration::from_secs(60),
                "abandoned",
                "same reason repeated",
            ));
        }
        let home = tempfile::TempDir::new().unwrap();
        let agents_dir = home.path().join("agents");
        std::fs::create_dir_all(agents_dir.join("agent-dup")).unwrap();
        // The tick-level gate skips GVU-disabled agents (fail-closed opt-in)
        // — this fixture's agent must be opted in to be scanned at all.
        std::fs::write(
            agents_dir.join("agent-dup").join("agent.toml"),
            "[evolution]\ngvu_enabled = true\n",
        )
        .unwrap();
        let pe = Arc::new(PredictionEngine::new(
            home.path().join("prediction.db"),
            Some(home.path().join("metacog.json")),
        ));
        let monitor =
            StagnationMonitor::new(&db_path, None, agents_dir, home.path().to_path_buf(), pe);

        let first = monitor.tick().await;
        assert_eq!(first.len(), 1);
        assert!(first[0].is_stagnant());
        {
            let seen = monitor.last_alerted.lock().await;
            assert!(
                seen.contains_key("agent-dup"),
                "first tick must record the alert fingerprint"
            );
        }

        // Second tick: identical DB state ⇒ identical fingerprint ⇒ the
        // monitor must recognise it already alerted (fingerprint unchanged)
        // even though it re-evaluates every tick.
        let fp_before = monitor.last_alerted.lock().await.get("agent-dup").cloned();
        let _second = monitor.tick().await;
        let fp_after = monitor.last_alerted.lock().await.get("agent-dup").cloned();
        assert_eq!(
            fp_before, fp_after,
            "fingerprint must not churn on an unchanged stagnant state"
        );
    }

    /// LWM D4 noise fix: a GVU-disabled agent (fail-closed opt-in default)
    /// must be skipped entirely — no snapshot, no alert. 152 stagnation
    /// events (71% of the activity feed) came from scanning agents whose
    /// GVU was off.
    #[tokio::test]
    async fn monitor_skips_gvu_disabled_agents() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        for _ in 0..5 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent-off",
                3,
                3,
                StdDuration::from_secs(60),
                "abandoned",
                "same reason repeated",
            ));
        }
        let home = tempfile::TempDir::new().unwrap();
        let agents_dir = home.path().join("agents");
        // No agent.toml at all (default) and an explicit false both gate out.
        std::fs::create_dir_all(agents_dir.join("agent-off")).unwrap();
        std::fs::write(
            agents_dir.join("agent-off").join("agent.toml"),
            "[evolution]\ngvu_enabled = false\n",
        )
        .unwrap();
        let pe = Arc::new(PredictionEngine::new(
            home.path().join("prediction.db"),
            Some(home.path().join("metacog.json")),
        ));
        let monitor =
            StagnationMonitor::new(&db_path, None, agents_dir, home.path().to_path_buf(), pe);
        let snapshots = monitor.tick().await;
        assert!(snapshots.is_empty(), "disabled agent must not be scanned");
        assert!(!monitor.last_alerted.lock().await.contains_key("agent-off"));
    }

    #[tokio::test]
    async fn monitor_clears_fingerprint_on_recovery() {
        let (_dir, db_path) = temp_db();
        let vs = VersionStore::new(&db_path);
        // Explicit, strictly-decreasing timestamps rather than
        // `ExperimentLogEntry::new`'s implicit `Utc::now()`. Every stagnation
        // signal scans the history newest-first and stops at the first
        // `applied` row, so this test's outcome depends entirely on the row
        // ordering; letting six rows race the wall clock (and, before the
        // `rowid` tiebreak in `get_experiments`, race SQLite's freedom to
        // order equal keys arbitrarily) made it pass alone and fail
        // intermittently under a loaded parallel run.
        let base = Utc::now();
        for i in 0..5 {
            vs.record_experiment(&entry(
                "agent-rec",
                "abandoned",
                "x",
                base - chrono::Duration::minutes(10 + i),
            ));
        }
        let home = tempfile::TempDir::new().unwrap();
        let agents_dir = home.path().join("agents");
        std::fs::create_dir_all(agents_dir.join("agent-rec")).unwrap();
        // Opt in past the tick-level gvu_enabled gate (see the sibling test).
        std::fs::write(
            agents_dir.join("agent-rec").join("agent.toml"),
            "[evolution]\ngvu_enabled = true\n",
        )
        .unwrap();
        let pe = Arc::new(PredictionEngine::new(
            home.path().join("prediction.db"),
            Some(home.path().join("metacog.json")),
        ));
        let monitor =
            StagnationMonitor::new(&db_path, None, agents_dir, home.path().to_path_buf(), pe);

        let _first = monitor.tick().await;
        assert!(monitor.last_alerted.lock().await.contains_key("agent-rec"));

        // Recovery: an `applied` row lands, breaking every signal. Stamped
        // strictly newer than all five rejections above.
        vs.record_experiment(&entry(
            "agent-rec",
            "applied",
            "Approved at generation 2",
            base,
        ));
        let second = monitor.tick().await;
        assert!(!second[0].is_stagnant());
        assert!(
            !monitor.last_alerted.lock().await.contains_key("agent-rec"),
            "recovered agent must have its fingerprint cleared so a future relapse re-alerts"
        );
    }
}
