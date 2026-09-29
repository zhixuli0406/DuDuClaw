//! Burn-rate cost anomaly detection.
//!
//! Fixed spend thresholds (the [`budget`](crate::budget) circuit breaker)
//! catch "you hit your cap", but 2026 FinOps guidance is that most real runaway
//! spend is caught earlier by a *relative* signal: today is burning far faster
//! than this agent's own recent baseline. This module computes that with plain
//! statistics (rolling mean + standard deviation over the agent's per-day spend
//! history) — no ML, no new storage. It reads the per-day series from
//! [`CostTelemetry::daily_cost_millicents`](crate::cost_telemetry::CostTelemetry::daily_cost_millicents).
//!
//! Complements the hard breaker: the breaker *blocks*, this *warns* (a soft
//! signal routed to logs / the notify path) so a spend spike is visible before
//! it reaches the cap.
//!
//! ## Wiring (D12, 2026-09 feature audit)
//!
//! Until this round the module doc above described a notify path that did not
//! exist: [`detect`] had no caller outside its own tests. It is now reached
//! from [`crate::budget::check_agent_budget`] — the dispatch choke point every
//! LLM call already passes through — via [`maybe_scan`], which
//!
//! - **throttles** to one SQL round-trip per agent per [`SCAN_INTERVAL`]
//!   (a process-local timestamp map; the hot path pays a mutex lookup),
//! - runs **independently of whether a budget cap is configured** — a relative
//!   burn-rate signal is most valuable exactly for the agents that have no
//!   absolute cap to trip,
//! - and **alerts at most once per agent per UTC day**, de-duplicated through
//!   a file-backed marker (`cost_anomaly_state.json`) for the same reason
//!   `budget::record_breaker_transition` is file-backed: this is a plain
//!   function re-entered from scratch on every call, possibly after a restart.
//!
//! An alert does three things and blocks nothing: an `warn!` log, a row on the
//! dashboard Activity Feed, and an L1 (FYI) push through the same
//! `goal_notify` path the budget breaker uses. Every step is best-effort.

/// Result of an anomaly check for one agent's current-window spend.
#[derive(Debug, Clone, PartialEq)]
pub struct AnomalyVerdict {
    /// True when `current` exceeds `mean + sigma·std` over the baseline history.
    pub is_anomaly: bool,
    /// Current-window spend (cents).
    pub current_cents: u64,
    /// Baseline mean (cents).
    pub mean_cents: f64,
    /// Baseline standard deviation (cents).
    pub std_cents: f64,
    /// Z-score of `current` against the baseline (`(current − mean) / std`);
    /// `0.0` when std is zero.
    pub z: f64,
}

/// Detect a burn-rate anomaly: is `current_cents` a statistical outlier above
/// the `history` baseline (mean + `sigma`·stddev)?
///
/// Returns a non-anomalous verdict when there are fewer than `min_samples`
/// baseline points (too little history to judge) or when the baseline has zero
/// variance AND `current` is within it. `history` should be the agent's prior
/// per-day spend (cents), excluding the current day.
pub fn detect(
    history: &[u64],
    current_cents: u64,
    sigma: f64,
    min_samples: usize,
) -> AnomalyVerdict {
    let n = history.len();
    let base = AnomalyVerdict {
        is_anomaly: false,
        current_cents,
        mean_cents: 0.0,
        std_cents: 0.0,
        z: 0.0,
    };
    if n < min_samples.max(1) {
        return base;
    }
    let mean = history.iter().map(|x| *x as f64).sum::<f64>() / n as f64;
    let variance = history
        .iter()
        .map(|x| (*x as f64 - mean).powi(2))
        .sum::<f64>()
        / n as f64;
    let std = variance.sqrt();
    let cur = current_cents as f64;

    // Zero-variance baseline: any spend strictly above the flat baseline is
    // anomalous; equal/below is not (avoids div-by-zero z-score).
    if std == 0.0 {
        return AnomalyVerdict {
            is_anomaly: cur > mean,
            current_cents,
            mean_cents: mean,
            std_cents: 0.0,
            z: 0.0,
        };
    }
    let z = (cur - mean) / std;
    AnomalyVerdict {
        is_anomaly: cur > mean + sigma * std,
        current_cents,
        mean_cents: mean,
        std_cents: std,
        z,
    }
}

/// Default sensitivity: 3σ above baseline (classic outlier threshold).
pub const DEFAULT_SIGMA: f64 = 3.0;
/// Default minimum baseline days before we'll judge an anomaly.
pub const DEFAULT_MIN_SAMPLES: usize = 5;

// ── Live wiring (D12) ───────────────────────────────────────────

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How much history the baseline is drawn from. 30 days covers a monthly
/// rhythm without letting a single old outlier dominate the variance.
pub const BASELINE_DAYS: u64 = 30;

/// Minimum gap between two scans of the same agent. The scan is one grouped
/// SQL query, but `check_agent_budget` runs on every LLM call, so it is gated.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(3600);

/// Process-local "last scanned at" per agent. Only a throttle — losing it on
/// restart costs at most one extra query, and the *alert* de-dup is durable
/// (see [`claim_daily_alert`]).
static LAST_SCAN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Split a labelled day series into `(baseline, today)`.
///
/// Days with no spend are absent from the series, so "today" is taken by
/// label, never by position: an agent that has spent nothing today gets
/// `today = 0` (and can never be flagged), while its earlier days still form
/// the baseline.
pub fn split_today(series: &[(String, u64)], today: &str) -> (Vec<u64>, u64) {
    let mut baseline = Vec::with_capacity(series.len());
    let mut current = 0u64;
    for (day, cents) in series {
        if day == today {
            current = *cents;
        } else {
            baseline.push(*cents);
        }
    }
    (baseline, current)
}

/// Throttled burn-rate scan for one agent, called from the budget gate.
///
/// Never blocks and never returns a verdict the caller must act on — an
/// anomaly is a warning, not a decision. Silently no-ops when: the agent id is
/// empty, the throttle window has not elapsed, telemetry is unavailable, the
/// query fails, there is too little history, or today's spend is within the
/// baseline.
pub async fn maybe_scan(home_dir: &Path, agent_dir: Option<&Path>, agent_id: &str) {
    if agent_id.is_empty() || !claim_scan_slot(agent_id) {
        return;
    }
    let Some(tel) = crate::cost_telemetry::get_telemetry() else {
        return;
    };
    let series = match tel.daily_cost_series(agent_id, BASELINE_DAYS).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(agent_id, "cost anomaly: daily series query failed: {e}");
            return;
        }
    };
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (baseline, current) = split_today(&series, &today);
    let verdict = detect(&baseline, current, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES);
    if !verdict.is_anomaly {
        return;
    }
    if !claim_daily_alert(home_dir, agent_id, &today) {
        return; // already alerted for this agent today
    }
    report(home_dir, agent_dir, agent_id, &verdict).await;
}

/// Emit the three best-effort outputs of one anomaly: log, Activity Feed, push.
async fn report(home_dir: &Path, agent_dir: Option<&Path>, agent_id: &str, v: &AnomalyVerdict) {
    // The series is in millicents; render the human-facing number in cents so
    // it lines up with the budget breaker's wording.
    let spent_cents = v.current_cents / 1000;
    let baseline_cents = (v.mean_cents / 1000.0).round() as u64;
    tracing::warn!(
        agent_id,
        current_millicents = v.current_cents,
        mean_millicents = v.mean_cents,
        z = v.z,
        "cost anomaly: today's burn rate is a statistical outlier above this agent's own baseline"
    );

    // Activity Feed — best-effort, mirrors `gvu::stagnation::post_activity`.
    if let Ok(store) = crate::task_store::TaskStore::open(home_dir) {
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "cost_anomaly_detected".to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: format!(
                "花費異常：今日 {spent_cents} 分，約為近期基線 {baseline_cents} 分的 {:.1} 倍（z={:.1}）",
                if v.mean_cents > 0.0 {
                    v.current_cents as f64 / v.mean_cents
                } else {
                    0.0
                },
                v.z
            ),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::to_string(&serde_json::json!({
                "current_millicents": v.current_cents,
                "mean_millicents": v.mean_cents,
                "std_millicents": v.std_cents,
                "z": v.z,
                "sigma": DEFAULT_SIGMA,
                "baseline_days": BASELINE_DAYS,
            }))
            .ok(),
        };
        if let Err(e) = store.append_activity(&row).await {
            tracing::debug!(agent_id, "cost anomaly: activity append failed: {e}");
        }
    }

    // Channel push — L1 (FYI): this is "look at this", not "act now"; the
    // hard breaker is what escalates to L3.
    let name = crate::budget::agent_display_name(agent_dir, agent_id);
    let link = crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Billing, agent_id)
        .map(|url| format!("\n👉 {url}"))
        .unwrap_or_default();
    let text = format!(
        "📈 {name} 今日花費異常：{spent_cents} 分，明顯高於自己近 {BASELINE_DAYS} 天的基線（約 {baseline_cents} 分）。{link}"
    );
    let outcome = crate::goal_notify::notify_agent_plain(
        home_dir,
        agent_id,
        crate::notify_governance::NotifyLevel::Fyi,
        "budget.anomaly",
        &text,
    )
    .await;
    if matches!(outcome, crate::goal_notify::NotifyOutcome::SendFailed) {
        tracing::debug!(agent_id, "cost anomaly: push failed (non-fatal)");
    }
}

/// Process-local throttle. `true` ⇒ this call owns the scan.
fn claim_scan_slot(agent_id: &str) -> bool {
    let now = Instant::now();
    let Ok(mut map) = LAST_SCAN.lock() else {
        return false; // poisoned ⇒ skip; telemetry must never panic the gate
    };
    match map.get(agent_id) {
        Some(prev) if now.duration_since(*prev) < SCAN_INTERVAL => false,
        _ => {
            map.insert(agent_id.to_string(), now);
            true
        }
    }
}

/// Durable once-per-UTC-day alert de-dup. `true` ⇒ this call owns the alert.
///
/// Fail-open in the *quiet* direction: any read/write failure returns `false`
/// (no alert) rather than risking a per-call alert storm on an unwritable
/// home. A missed warning is cheaper than a notification flood.
fn claim_daily_alert(home_dir: &Path, agent_id: &str, today: &str) -> bool {
    let path = home_dir.join("cost_anomaly_state.json");
    duduclaw_core::with_file_lock(&path, || {
        let mut states: HashMap<String, String> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if states.get(agent_id).map(String::as_str) == Some(today) {
            return Ok(false);
        }
        states.insert(agent_id.to_string(), today.to_string());
        let json = serde_json::to_string_pretty(&states)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)?;
        Ok(true)
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_spike_above_baseline() {
        // Steady ~100/day for a week, then 1000 today → anomaly.
        let hist = [100, 110, 90, 105, 95, 100, 100];
        let v = detect(&hist, 1000, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES);
        assert!(v.is_anomaly, "10x spike must flag: z={}", v.z);
        assert!(v.z > 3.0);
    }

    #[test]
    fn normal_variation_not_flagged() {
        let hist = [100, 110, 90, 105, 95, 100, 100];
        assert!(!detect(&hist, 115, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn too_little_history_never_flags() {
        // 2 samples < min 5 → cannot judge, even a huge value is not flagged.
        assert!(!detect(&[10, 10], 100_000, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn zero_variance_baseline() {
        // Flat 50/day baseline: 51 is above → anomaly; 50 is not.
        let hist = [50, 50, 50, 50, 50, 50];
        assert!(detect(&hist, 51, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, 50, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, 49, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    // ── D12: the wiring, not just the statistic ─────────────────────────────

    fn series(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
        pairs.iter().map(|(d, c)| (d.to_string(), *c)).collect()
    }

    #[test]
    fn today_is_taken_by_label_not_by_position() {
        // Regression for the defect that made a naive `last()` wrong: the
        // series omits days with no spend, so an agent that spent nothing
        // today would otherwise have its last *active* day read as "today"
        // and get flagged for spending it had already been judged on.
        let s = series(&[
            ("2026-09-20", 100),
            ("2026-09-21", 110),
            ("2026-09-27", 9_000),
        ]);
        let (baseline, current) = split_today(&s, "2026-09-29");
        assert_eq!(baseline, vec![100, 110, 9_000]);
        assert_eq!(current, 0, "no rows today ⇒ nothing spent today");

        let (baseline, current) = split_today(&s, "2026-09-27");
        assert_eq!(baseline, vec![100, 110]);
        assert_eq!(current, 9_000);
    }

    #[test]
    fn a_quiet_today_can_never_be_flagged() {
        let s = series(&[
            ("2026-09-20", 100),
            ("2026-09-21", 110),
            ("2026-09-22", 90),
            ("2026-09-23", 105),
            ("2026-09-24", 95),
            ("2026-09-25", 100),
        ]);
        let (baseline, current) = split_today(&s, "2026-09-29");
        assert!(!detect(&baseline, current, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn a_spike_today_against_its_own_baseline_is_flagged() {
        let mut s = series(&[
            ("2026-09-20", 100),
            ("2026-09-21", 110),
            ("2026-09-22", 90),
            ("2026-09-23", 105),
            ("2026-09-24", 95),
            ("2026-09-25", 100),
        ]);
        s.push(("2026-09-29".into(), 5_000));
        let (baseline, current) = split_today(&s, "2026-09-29");
        assert_eq!(baseline.len(), 6, "today must not be part of its own baseline");
        assert!(detect(&baseline, current, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn the_scan_throttle_lets_exactly_one_call_through_per_window() {
        // Unique id so parallel tests in this binary cannot collide on the
        // process-global map.
        let agent = format!("throttle-probe-{}", uuid::Uuid::new_v4());
        assert!(claim_scan_slot(&agent), "first call owns the scan");
        assert!(!claim_scan_slot(&agent), "second call inside the window skips");
        assert!(!claim_scan_slot(&agent));
    }

    #[test]
    fn the_daily_alert_marker_is_durable_and_rolls_over_at_midnight() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(claim_daily_alert(tmp.path(), "sales-rep", "2026-09-29"));
        assert!(
            !claim_daily_alert(tmp.path(), "sales-rep", "2026-09-29"),
            "at most one alert per agent per UTC day"
        );
        // A different agent on the same day is independent.
        assert!(claim_daily_alert(tmp.path(), "support", "2026-09-29"));
        // Next day the same agent may alert again.
        assert!(claim_daily_alert(tmp.path(), "sales-rep", "2026-09-30"));

        // The marker survives a "restart" — it is a file, not a memory map.
        assert!(tmp.path().join("cost_anomaly_state.json").is_file());
        assert!(!claim_daily_alert(tmp.path(), "sales-rep", "2026-09-30"));
    }

    #[tokio::test]
    async fn maybe_scan_is_silent_without_telemetry_and_for_an_empty_agent() {
        // The gate calls this on every LLM call; it must be a no-op (not a
        // panic, not an error) in the states a fresh process is in.
        let tmp = tempfile::tempdir().unwrap();
        maybe_scan(tmp.path(), None, "").await;
        maybe_scan(tmp.path(), None, &format!("probe-{}", uuid::Uuid::new_v4())).await;
        assert!(
            !tmp.path().join("cost_anomaly_state.json").exists(),
            "no telemetry ⇒ no alert state written"
        );
    }

    #[test]
    fn sigma_controls_sensitivity() {
        // mean=100, std≈12.9 → 1σ≈112.9, 3σ≈138.7.
        let hist = [100, 120, 80, 110, 90, 100];
        // A moderate bump between 1σ and 3σ: flagged at 1σ, not at 3σ.
        let cur = 130;
        assert!(detect(&hist, cur, 1.0, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, cur, 3.0, DEFAULT_MIN_SAMPLES).is_anomaly);
    }
}
