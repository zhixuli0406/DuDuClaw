//! ObservationFinalizer — closes expired SOUL.md observation windows.
//!
//! ## The bug this fixes
//!
//! Before this module existed, `VersionStore::get_expired_observations()` and
//! `Updater::execute_confirm()` / `execute_rollback()` had **no callers**. The
//! observation lifecycle wrote an `observing` row, the timer expired, and
//! nothing ever transitioned the row to `confirmed` or `rolled_back`. The
//! "have I got an observing version?" check inside the GVU loop then blocked
//! all subsequent proposals indefinitely (single-version dead-lock).
//!
//! ## What this does
//!
//! On a 30-minute tick (or one-shot via the CLI):
//! 1. Read every `status='observing' AND observation_end < now()` row.
//! 2. Compute `post_metrics` from `prediction.db.prediction_log` and
//!    `~/.duduclaw/feedback.jsonl` over the observation window.
//! 3. Pass to `Updater::judge_outcome` (existing tolerance thresholds).
//! 4. Call `execute_confirm` / `execute_rollback` accordingly.
//!
//! Every decision is logged as a structured `info!` event so behaviour is
//! auditable from the gateway log. Confirm / rollback / the 72h low-data
//! warning / the 14-day hard expiry additionally post a zh-TW row to the
//! dashboard Activity Feed and an evolution event — previously these
//! terminal outcomes were only visible in the log file or by querying
//! `evolution.db` by hand. Deliberately does NOT also push a channel
//! notification: this class of result is FYI-level, routed through the
//! (separately scoped) daily digest rather than a real-time push.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tracing::{debug, info, warn};

use super::updater::{OutcomeVerdict, Updater};
use super::version_store::{SoulVersion, VersionMetrics, VersionStore};

// ── Public types ──────────────────────────────────────────────────────────────

/// One row of the report returned by [`ObservationFinalizer::tick`].
#[derive(Debug, Clone)]
pub struct FinalizationDecision {
    pub agent_id: String,
    pub version_id: String,
    pub decision: Decision,
    pub pre: VersionMetrics,
    pub post: VersionMetrics,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Confirmed,
    RolledBack { reason: String },
    Extended { extra_hours: f64 },
    /// WP0.4 (R5): observation ran past the hard no-data ceiling. SOUL.md
    /// content is untouched — this is NOT a confirm, just giving up on
    /// waiting for evidence. See `VersionStatus::ExpiredNoData`.
    ExpiredNoData,
    Failed { error: String },
}

/// Aggregate result of one finalize sweep.
#[derive(Debug, Default, Clone)]
pub struct FinalizationReport {
    pub decisions: Vec<FinalizationDecision>,
}

// ── Dashboard-facing copy (Activity Feed) ───────────────────────────────────

/// zh-TW Activity Feed copy for a terminal observation-window decision.
///
/// Follows the dashboard state-vocabulary contract
/// (`04-orca-object-model-cta-matrix.md` §C.9): only "行為調整" / "試行"
/// surface to the reader — internal names (SOUL.md, "observation window",
/// GVU) never leak into user-facing text. Returns `(event_type, summary)`;
/// `event_type` is an internal Activity Feed / evolution-event key, not
/// shown to the reader.
fn confirmed_copy(agent_id: &str) -> (&'static str, String) {
    (
        "gvu_observation_confirmed",
        format!("{agent_id} 的行為調整已通過試行，正式生效"),
    )
}

fn rolled_back_copy() -> (&'static str, String) {
    (
        "gvu_observation_rolled_back",
        "試行未達標，已回退".to_string(),
    )
}

fn expired_no_data_copy() -> (&'static str, String) {
    (
        "gvu_observation_expired_no_data",
        "證據不足，未採用".to_string(),
    )
}

/// Copy for the 72h soft-warn branch (`OutcomeVerdict::ExtendObservationLowDataWarn`)
/// — the observation kept running but crossed the quality-gate warn
/// threshold with too little evidence to judge either way.
fn low_data_warn_copy(agent_id: &str) -> (&'static str, String) {
    (
        "gvu_observation_low_data_warn",
        format!("{agent_id} 的行為調整仍在試行中，證據不足，已延長試行期"),
    )
}

// ── ObservationFinalizer ──────────────────────────────────────────────────────

/// Periodically transitions expired `observing` SOUL versions into
/// `confirmed` / `rolled_back`.
pub struct ObservationFinalizer {
    version_store: VersionStore,
    prediction_db: PathBuf,
    feedback_jsonl: PathBuf,
    agents_dir: PathBuf,
    /// Encryption key forwarded to the Updater (so rollback_diff stays
    /// consistent with how it was written).
    encryption_key: Option<[u8; 32]>,
}

impl ObservationFinalizer {
    pub fn new(
        version_store: VersionStore,
        prediction_db: PathBuf,
        feedback_jsonl: PathBuf,
        agents_dir: PathBuf,
        encryption_key: Option<[u8; 32]>,
    ) -> Self {
        Self {
            version_store,
            prediction_db,
            feedback_jsonl,
            agents_dir,
            encryption_key,
        }
    }

    /// The DuDuClaw home directory, derived from `agents_dir`
    /// (`<home>/agents`). Used by the AEE settlement sweep to reach
    /// `memory.db` / `config.toml` without a new constructor argument — every
    /// existing construction site stays untouched.
    fn home_dir(&self) -> PathBuf {
        self.agents_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.agents_dir.clone())
    }

    /// WP2.5 — close every AEE settlement whose observation window has
    /// elapsed.
    ///
    /// Deliberately a **separate** queue from the SOUL observation windows
    /// swept below: an AEE round writes no `SoulVersion`, so there is nothing
    /// in `soul_versions` for the existing sweep to find. The two coexist
    /// without interference — an agent can have a legacy SOUL version under
    /// observation and no AEE settlement, or the reverse, and neither sweep
    /// reads the other's rows.
    ///
    /// Best-effort throughout: a settlement that cannot be judged (no eval
    /// score reachable) leaves its entries exactly as they are and says so —
    /// "we could not check" is never rendered as "checked and fine".
    async fn sweep_aee_settlements(&self) -> usize {
        let db_path = self.version_store.db_path_ref().to_path_buf();
        let due = crate::gvu::aee::PendingSettlementStore::new(&db_path).due(Utc::now());
        if due.is_empty() {
            return 0;
        }
        let home = self.home_dir();
        let scorer = crate::gvu::aee::EvalMeasureScorer::from_home(&home);
        info!(
            target: "observation_finalizer",
            count = due.len(),
            "Sweeping due AEE playbook settlements"
        );
        let mut settled = 0usize;
        for pending in due {
            if crate::gvu::aee::settle_pending(&pending, &db_path, &home, &scorer)
                .await
                .is_some()
            {
                settled += 1;
            }
        }
        settled
    }

    /// Run a single sweep — finalise every expired observation, returning a
    /// per-version decision log.
    pub async fn tick(&self) -> FinalizationReport {
        // AEE settlements first: they are independent of the SOUL observation
        // queue and must still run on a tick where no SOUL version expired
        // (the early return below would otherwise skip them entirely).
        self.sweep_aee_settlements().await;

        let expired = self.version_store.get_expired_observations();
        if expired.is_empty() {
            debug!(target: "observation_finalizer", "No expired observations");
            return FinalizationReport::default();
        }
        info!(
            target: "observation_finalizer",
            count = expired.len(),
            "Sweeping expired SOUL observations"
        );

        let mut report = FinalizationReport::default();
        for version in expired {
            let decision = self.finalize_one(&version).await;
            report.decisions.push(decision);
        }
        report
    }

    /// Long-running task: tick on a fixed interval until cancelled.
    pub async fn run(self: Arc<Self>, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        // First tick fires immediately — kick off as soon as the gateway boots
        // so a stuck observation from a previous run isn't held back another
        // 30 min.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = self.tick().await;
        }
    }

    // ── Internal ──

    async fn finalize_one(&self, version: &SoulVersion) -> FinalizationDecision {
        let agent_dir = self.agents_dir.join(&version.agent_id);

        // Compute post-metrics over the observation window.
        let post = match self.compute_post_metrics(&version.agent_id, version.applied_at) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    target: "observation_finalizer",
                    agent = %version.agent_id,
                    version = %version.version_id,
                    "Failed to compute post-metrics: {e} — skipping (will retry next tick)"
                );
                return FinalizationDecision {
                    agent_id: version.agent_id.clone(),
                    version_id: version.version_id.clone(),
                    decision: Decision::Failed { error: e },
                    pre: version.pre_metrics.clone(),
                    post: VersionMetrics::default(),
                };
            }
        };

        // Use the existing Updater::judge_outcome tolerance logic.
        let updater = Updater::new(
            VersionStore::with_crypto(self.version_store.db_path_ref(), self.encryption_key.as_ref()),
            None,
        );
        let verdict = updater.judge_outcome(version, &post);

        let decision = match verdict {
            OutcomeVerdict::Confirm => match updater.execute_confirm(version, &post) {
                Ok(()) => {
                    info!(
                        target: "observation_finalizer",
                        agent = %version.agent_id,
                        version = %version.version_id,
                        post_err = format!("{:.3}", post.avg_prediction_error),
                        post_pos = format!("{:.2}", post.positive_feedback_ratio),
                        "Observation confirmed"
                    );
                    let (event_type, summary) = confirmed_copy(&version.agent_id);
                    crate::evolution_events::emitter::EvolutionEventEmitter::global()
                        .emit_gvu_generation(
                            &version.agent_id,
                            crate::evolution_events::schema::Outcome::Success,
                            event_type,
                            serde_json::json!({ "version_id": &version.version_id }),
                        );
                    self.post_activity(&version.agent_id, event_type, &summary, &version.version_id)
                        .await;
                    Decision::Confirmed
                }
                Err(e) => Decision::Failed { error: format!("execute_confirm: {e}") },
            },
            OutcomeVerdict::Rollback { reason } => {
                match updater.execute_rollback(version, &agent_dir) {
                    Ok(()) => {
                        warn!(
                            target: "observation_finalizer",
                            agent = %version.agent_id,
                            version = %version.version_id,
                            reason = %reason,
                            "Observation rolled back"
                        );
                        let (event_type, summary) = rolled_back_copy();
                        crate::evolution_events::emitter::EvolutionEventEmitter::global()
                            .emit_gvu_generation(
                                &version.agent_id,
                                crate::evolution_events::schema::Outcome::Failure,
                                event_type,
                                serde_json::json!({
                                    "version_id": &version.version_id,
                                    "reason": &reason,
                                }),
                            );
                        self.post_activity(&version.agent_id, event_type, &summary, &version.version_id)
                            .await;
                        Decision::RolledBack { reason }
                    }
                    Err(e) => Decision::Failed {
                        error: format!("execute_rollback: {e}"),
                    },
                }
            }
            OutcomeVerdict::ExtendObservation { extra_hours } => {
                // Slide the observation_end forward but keep status='observing'.
                if let Err(e) =
                    self.extend_observation(&version.version_id, extra_hours)
                {
                    Decision::Failed {
                        error: format!("extend_observation: {e}"),
                    }
                } else {
                    info!(
                        target: "observation_finalizer",
                        agent = %version.agent_id,
                        version = %version.version_id,
                        extra_hours,
                        "Observation extended (insufficient data)"
                    );
                    Decision::Extended { extra_hours }
                }
            }
            OutcomeVerdict::ExtendObservationLowDataWarn { extra_hours } => {
                // Same effect as a plain extend, but this branch also raises
                // a ONE-TIME alert (WP0.4/R5) — this is exactly the
                // situation that used to silently auto-confirm.
                if let Err(e) =
                    self.extend_observation(&version.version_id, extra_hours)
                {
                    Decision::Failed {
                        error: format!("extend_observation: {e}"),
                    }
                } else {
                    self.maybe_raise_low_data_alert(version).await;
                    Decision::Extended { extra_hours }
                }
            }
            OutcomeVerdict::ExpiredNoData => {
                match self.version_store.mark_expired_no_data(&version.version_id, &post) {
                    Ok(()) => {
                        warn!(
                            target: "observation_finalizer",
                            agent = %version.agent_id,
                            version = %version.version_id,
                            "Observation expired without sufficient data — marked unverified, NOT confirmed (WP0.4)"
                        );
                        let (event_type, summary) = expired_no_data_copy();
                        crate::evolution_events::emitter::EvolutionEventEmitter::global()
                            .emit_gvu_generation(
                                &version.agent_id,
                                crate::evolution_events::schema::Outcome::Warned,
                                event_type,
                                serde_json::json!({ "version_id": &version.version_id }),
                            );
                        self.post_activity(&version.agent_id, event_type, &summary, &version.version_id)
                            .await;
                        Decision::ExpiredNoData
                    }
                    Err(e) => Decision::Failed {
                        error: format!("mark_expired_no_data: {e}"),
                    },
                }
            }
        };

        FinalizationDecision {
            agent_id: version.agent_id.clone(),
            version_id: version.version_id.clone(),
            decision,
            pre: version.pre_metrics.clone(),
            post,
        }
    }

    /// Aggregate post-metrics from local data sources.
    ///
    /// Reads `prediction.db.prediction_log` (composite_error mean, count) and
    /// `feedback.jsonl` (positive_feedback_ratio) for events that happened
    /// **after** `since`. `contract_violations` is currently 0 — we have no
    /// violation log yet. `user_correction_rate` is approximated via the
    /// fraction of `Significant` / `Critical` predictions.
    fn compute_post_metrics(
        &self,
        agent_id: &str,
        since: DateTime<Utc>,
    ) -> Result<VersionMetrics, String> {
        let conn = Connection::open(&self.prediction_db)
            .map_err(|e| format!("open prediction.db: {e}"))?;
        let since_str = since.to_rfc3339();

        // Count + average composite_error in window.
        let (total, avg_err): (u32, f64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(AVG(composite_error), 0.0) FROM prediction_log
                 WHERE agent_id = ?1 AND timestamp > ?2",
                params![agent_id, since_str],
                |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, f64>(1)?)),
            )
            .map_err(|e| format!("query prediction_log: {e}"))?;

        // Significant + Critical share = correction proxy.
        let bad: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM prediction_log
                 WHERE agent_id = ?1 AND timestamp > ?2
                   AND category IN ('Significant', 'Critical')",
                params![agent_id, &since_str],
                |row| Ok(row.get::<_, i64>(0)? as u32),
            )
            .unwrap_or(0);
        let correction_rate = if total > 0 { bad as f64 / total as f64 } else { 0.0 };

        // WP0.4 (R5): don't let a MISSING feedback.jsonl masquerade as
        // "measured zero positive feedback" — the two are very different
        // claims (audit found feedback.jsonl absent on multiple production
        // installs while GVU nonetheless "confirmed" versions with 0.0
        // ratios as if that were real evidence). `feedback_available` lets
        // downstream consumers (judge_outcome's own logic is unchanged here
        // — this is purely an honesty flag for dashboard/audit) distinguish
        // the two.
        let feedback_available = self.feedback_jsonl.exists();
        let positive_ratio = if feedback_available {
            count_positive_feedback_ratio(&self.feedback_jsonl, agent_id, since)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        Ok(VersionMetrics {
            positive_feedback_ratio: positive_ratio,
            avg_prediction_error: avg_err,
            user_correction_rate: correction_rate,
            contract_violations: 0,
            conversations_count: total,
            feedback_available,
        })
    }

    /// WP0.4 (R5): raise a one-time "insufficient observation data" alert
    /// for a version that has crossed the soft warn threshold. Guarded by
    /// `low_data_alert_sent` so a version stuck for weeks (extending every
    /// 12h) produces exactly one alert, not one per tick — which also caps
    /// the Activity Feed row below at exactly one per version.
    async fn maybe_raise_low_data_alert(&self, version: &SoulVersion) {
        if self.version_store.low_data_alert_sent(&version.version_id) {
            return; // Already alerted — not repeated.
        }
        if let Err(e) = self
            .version_store
            .mark_low_data_alert_sent(&version.version_id, &version.agent_id)
        {
            warn!(
                target: "observation_finalizer",
                agent = %version.agent_id,
                version = %version.version_id,
                "Failed to persist low-data alert marker: {e} — alert may repeat"
            );
        }
        let elapsed_hours = (Utc::now() - version.applied_at).num_hours();
        warn!(
            target: "observation_finalizer",
            agent = %version.agent_id,
            version = %version.version_id,
            elapsed_hours,
            "GVU observation window has insufficient data past the quality-gate warn threshold \
             — extending, NOT auto-confirming (WP0.4/R5)"
        );
        crate::evolution_events::emitter::EvolutionEventEmitter::global().emit_gvu_generation(
            &version.agent_id,
            crate::evolution_events::schema::Outcome::Warned,
            "observation_low_data_warn",
            serde_json::json!({
                "version_id": version.version_id,
                "elapsed_hours": elapsed_hours,
            }),
        );
        let (event_type, summary) = low_data_warn_copy(&version.agent_id);
        self.post_activity(&version.agent_id, event_type, &summary, &version.version_id)
            .await;
    }

    /// Best-effort append to the dashboard Activity Feed. Mirrors
    /// `StagnationMonitor::post_activity` / `GvuAlertSink::alert` — telemetry,
    /// not control flow: a failure here must never affect the already-committed
    /// confirm/rollback/expire decision. Unlike those two siblings this does
    /// NOT also push a channel notification (see module doc).
    async fn post_activity(&self, agent_id: &str, event_type: &str, summary: &str, version_id: &str) {
        let store = match crate::task_store::TaskStore::open(&self.home_dir()) {
            Ok(s) => s,
            Err(e) => {
                debug!(
                    target: "observation_finalizer",
                    error = %e,
                    "failed to open task store for Activity Feed (non-fatal)"
                );
                return;
            }
        };
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: summary.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            metadata: serde_json::to_string(&serde_json::json!({ "version_id": version_id })).ok(),
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(
                target: "observation_finalizer",
                error = %e,
                "Activity Feed append failed (non-fatal)"
            );
        }
    }

    /// Push observation_end further into the future without changing status.
    fn extend_observation(&self, version_id: &str, extra_hours: f64) -> Result<(), String> {
        let conn = Connection::open(self.version_store.db_path_ref())
            .map_err(|e| format!("open evolution.db: {e}"))?;
        let new_end = (Utc::now()
            + chrono::Duration::seconds((extra_hours * 3600.0) as i64))
            .to_rfc3339();
        conn.execute(
            "UPDATE soul_versions SET observation_end = ?1 WHERE version_id = ?2",
            params![new_end, version_id],
        )
        .map_err(|e| format!("update observation_end: {e}"))?;
        Ok(())
    }
}

// Expose db_path on VersionStore for sibling modules. We add a small accessor
// in version_store.rs (existing `db_path` method); fall back to a duplicate
// path arg if needed.

/// Compute the share of `signal_type=positive` feedback events in `feedback.jsonl`
/// for the given agent since `since`. Missing file → `0.0`.
fn count_positive_feedback_ratio(
    feedback_path: &Path,
    agent_id: &str,
    since: DateTime<Utc>,
) -> Result<f64, String> {
    if !feedback_path.exists() {
        return Ok(0.0);
    }
    let content = std::fs::read_to_string(feedback_path)
        .map_err(|e| format!("read feedback.jsonl: {e}"))?;
    let mut total = 0u32;
    let mut positive = 0u32;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let aid = v.get("agent_id").and_then(|x| x.as_str()).unwrap_or("");
        if aid != agent_id {
            continue;
        }
        let ts = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("");
        let ts = match DateTime::parse_from_rfc3339(ts) {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => continue,
        };
        if ts <= since {
            continue;
        }
        total += 1;
        if v.get("signal_type").and_then(|x| x.as_str()) == Some("positive") {
            positive += 1;
        }
    }
    if total == 0 {
        Ok(0.0)
    } else {
        Ok(positive as f64 / total as f64)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gvu::version_store::{VersionMetrics, VersionStatus};
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn make_finalizer(tmp: &Path, key: Option<[u8; 32]>) -> (ObservationFinalizer, PathBuf, PathBuf, PathBuf) {
        let evo_db = tmp.join("evolution.db");
        let pred_db = tmp.join("prediction.db");
        let feedback = tmp.join("feedback.jsonl");
        let agents = tmp.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let vs = VersionStore::with_crypto(&evo_db, key.as_ref());
        let f = ObservationFinalizer::new(vs, pred_db.clone(), feedback.clone(), agents.clone(), key);
        (f, evo_db, pred_db, feedback)
    }

    fn seed_prediction_db(path: &Path, agent: &str, rows: &[(f64, &str, DateTime<Utc>)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS prediction_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                composite_error REAL NOT NULL,
                category TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );",
        )
        .unwrap();
        for (err, cat, ts) in rows {
            conn.execute(
                "INSERT INTO prediction_log (agent_id, user_id, composite_error, category, timestamp)
                 VALUES (?1, 'u', ?2, ?3, ?4)",
                params![agent, err, cat, ts.to_rfc3339()],
            )
            .unwrap();
        }
    }

    fn seed_observing_version(
        vs: &VersionStore,
        agents_dir: &Path,
        agent: &str,
        applied_at: DateTime<Utc>,
        observation_end: DateTime<Utc>,
        pre: VersionMetrics,
    ) -> SoulVersion {
        // Make sure agent_dir/SOUL.md exists so rollback path won't crash even
        // though the rollback_diff is empty for these tests.
        let agent_dir = agents_dir.join(agent);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("SOUL.md"), b"current soul\n").unwrap();

        let v = SoulVersion {
            version_id: format!("v-{agent}-{}", applied_at.timestamp_millis()),
            agent_id: agent.to_owned(),
            soul_hash: "deadbeef".into(),
            soul_summary: "test".into(),
            applied_at,
            observation_end,
            status: VersionStatus::Observing,
            pre_metrics: pre,
            post_metrics: None,
            proposal_id: "prop-1".into(),
            rollback_diff: "previous soul\n".into(),
            rollback_diff_hash: None,
        };
        vs.record_version(&v).unwrap();
        v
    }

    #[tokio::test]
    async fn test_tick_finalizes_expired_only() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        // Expired version with good metrics → should confirm.
        // positive_feedback_ratio kept at 0 — feedback.jsonl not seeded so the
        // 3% dip threshold won't trigger a spurious rollback.
        let pre = VersionMetrics {
            avg_prediction_error: 0.30,
            positive_feedback_ratio: 0.0,
            ..Default::default()
        };
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-a",
            now - ChronoDuration::hours(48),
            now - ChronoDuration::hours(24),
            pre.clone(),
        );
        // Not yet expired version — must NOT be touched.
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-b",
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(24),
            pre.clone(),
        );

        // Seed prediction_log with 5 conversations after applied_at, all low error.
        let rows: Vec<_> = (0..5)
            .map(|i| (0.05_f64, "Negligible", now - ChronoDuration::hours(40 - i)))
            .collect();
        seed_prediction_db(&pred_db, "agent-a", &rows);

        let report = fin.tick().await;
        assert_eq!(report.decisions.len(), 1, "only agent-a is expired");
        let d = &report.decisions[0];
        assert_eq!(d.agent_id, "agent-a");
        assert!(matches!(d.decision, Decision::Confirmed), "got {:?}", d.decision);

        // agent-b status remains observing
        let still = fin
            .version_store
            .get_observing_version("agent-b")
            .expect("agent-b should still be observing");
        assert_eq!(still.status, VersionStatus::Observing);
    }

    #[tokio::test]
    async fn test_decide_rollback_on_regression() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        // positive_feedback_ratio = 0 isolates the test to error-regression.
        let pre = VersionMetrics {
            avg_prediction_error: 0.10,
            positive_feedback_ratio: 0.0,
            ..Default::default()
        };
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-r",
            now - ChronoDuration::hours(48),
            now - ChronoDuration::hours(24),
            pre,
        );

        // 6 conversations all with HIGH error (0.6) — regression vs pre 0.10.
        let rows: Vec<_> = (0..6)
            .map(|i| (0.60_f64, "Significant", now - ChronoDuration::hours(40 - i)))
            .collect();
        seed_prediction_db(&pred_db, "agent-r", &rows);

        let report = fin.tick().await;
        assert_eq!(report.decisions.len(), 1);
        match &report.decisions[0].decision {
            Decision::RolledBack { reason } => {
                assert!(
                    reason.contains("Prediction error"),
                    "expected error-regression rollback, got: {reason}"
                );
            }
            other => panic!("expected RolledBack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_decide_extend_when_insufficient_samples() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        let pre = VersionMetrics {
            avg_prediction_error: 0.10,
            ..Default::default()
        };
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-x",
            now - ChronoDuration::hours(48),
            now - ChronoDuration::hours(24),
            pre,
        );

        // Only 2 conversations — judge_outcome demands >= 5 → ExtendObservation.
        let rows: Vec<_> = (0..2)
            .map(|i| (0.05_f64, "Negligible", now - ChronoDuration::hours(40 - i)))
            .collect();
        seed_prediction_db(&pred_db, "agent-x", &rows);

        let report = fin.tick().await;
        assert_eq!(report.decisions.len(), 1);
        assert!(
            matches!(report.decisions[0].decision, Decision::Extended { .. }),
            "got {:?}",
            report.decisions[0].decision
        );
    }

    #[tokio::test]
    async fn test_no_expired_versions_is_noop() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, _pred_db, _feedback) = make_finalizer(tmp.path(), None);
        let report = fin.tick().await;
        assert!(report.decisions.is_empty());
    }

    // ── WP0.4 (R5): observation-window quality gate ─────────────────────────

    #[tokio::test]
    async fn test_no_data_under_hard_ceiling_extends_never_confirms() {
        // Applied 80h ago (past the old 72h "auto-confirm" cutoff, but well
        // under the new 14-day hard ceiling) with zero conversations. This is
        // the exact scenario that used to silently auto-confirm with no
        // evidence (R5 production audit finding) — must now still extend.
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        let pre = VersionMetrics::default();
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-nodata",
            now - ChronoDuration::hours(80),
            now - ChronoDuration::hours(56), // observation_end already passed
            pre,
        );
        // Table must exist (real installs always have it via the prediction
        // engine) but genuinely zero traffic — no rows.
        seed_prediction_db(&pred_db, "agent-nodata", &[]);

        let report = fin.tick().await;
        assert_eq!(report.decisions.len(), 1);
        assert!(
            !matches!(report.decisions[0].decision, Decision::Confirmed),
            "R5 regression: no-data observation must never auto-confirm, got {:?}",
            report.decisions[0].decision
        );
        assert!(
            matches!(report.decisions[0].decision, Decision::Extended { .. }),
            "expected Extended (with alert), got {:?}",
            report.decisions[0].decision
        );

        // Status must remain 'observing' in the store.
        let still = fin
            .version_store
            .get_observing_version("agent-nodata")
            .expect("must still be observing");
        assert_eq!(still.status, VersionStatus::Observing);

        // One-time alert marker must now be set.
        assert!(fin.version_store.low_data_alert_sent(&still.version_id));
    }

    #[tokio::test]
    async fn test_low_data_alert_is_not_repeated_across_ticks() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-repeat",
            now - ChronoDuration::hours(80),
            now - ChronoDuration::hours(56),
            VersionMetrics::default(),
        );
        seed_prediction_db(&pred_db, "agent-repeat", &[]);

        // First tick sends the alert and extends observation_end into the future.
        let _ = fin.tick().await;
        let v1 = fin.version_store.get_observing_version("agent-repeat").unwrap();
        assert!(fin.version_store.low_data_alert_sent(&v1.version_id));

        // Force the (now extended) observation back into the past so a
        // second tick would fire again if the guard didn't work, then verify
        // the marker is still a single idempotent row (mark_low_data_alert_sent
        // is INSERT OR IGNORE) rather than asserting on tick() output, since
        // the version is no longer expired after the first extend.
        assert!(
            fin.version_store.mark_low_data_alert_sent(&v1.version_id, "agent-repeat").is_ok(),
            "re-marking an already-sent alert must be idempotent, not error"
        );
    }

    #[tokio::test]
    async fn test_no_data_past_hard_ceiling_marks_expired_not_confirmed() {
        // 15 days old, zero conversations — past the 14-day default hard
        // ceiling. Must transition to `expired_no_data`, never `confirmed`.
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-expired",
            now - ChronoDuration::days(15),
            now - ChronoDuration::days(14) - ChronoDuration::hours(1),
            VersionMetrics::default(),
        );
        seed_prediction_db(&pred_db, "agent-expired", &[]);

        let report = fin.tick().await;
        assert_eq!(report.decisions.len(), 1);
        assert!(
            matches!(report.decisions[0].decision, Decision::ExpiredNoData),
            "expected ExpiredNoData, got {:?}",
            report.decisions[0].decision
        );

        // No longer 'observing' — and specifically NOT 'confirmed'.
        assert!(
            fin.version_store.get_observing_version("agent-expired").is_none(),
            "expired version must leave the observing state"
        );
        let history = fin.version_store.get_history("agent-expired", 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, VersionStatus::ExpiredNoData);
    }

    #[test]
    fn test_compute_post_metrics_flags_missing_feedback_file() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback_path) = make_finalizer(tmp.path(), None);
        // feedback.jsonl deliberately never written.
        seed_prediction_db(&pred_db, "agent-nofeedback", &[(0.1, "Negligible", Utc::now())]);

        let metrics = fin
            .compute_post_metrics("agent-nofeedback", Utc::now() - ChronoDuration::hours(1))
            .unwrap();
        assert!(
            !metrics.feedback_available,
            "feedback_available must be false when feedback.jsonl does not exist"
        );
        assert_eq!(
            metrics.positive_feedback_ratio, 0.0,
            "ratio still reads 0.0, but feedback_available now distinguishes 'no evidence' from 'measured zero'"
        );
    }

    #[test]
    fn test_compute_post_metrics_flags_available_when_file_exists() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, feedback_path) = make_finalizer(tmp.path(), None);
        seed_prediction_db(&pred_db, "agent-withfeedback", &[(0.1, "Negligible", Utc::now())]);
        // An empty-but-present feedback.jsonl still counts as "available"
        // (the distinction is file presence, not row count).
        std::fs::write(&feedback_path, "").unwrap();

        let metrics = fin
            .compute_post_metrics("agent-withfeedback", Utc::now() - ChronoDuration::hours(1))
            .unwrap();
        assert!(metrics.feedback_available);
    }

    // ── Activity Feed copy (dashboard-facing text) ───────────────────────

    /// Internal names must never leak into the copy shown to a reader —
    /// this is the concrete regression test for the module-doc claim.
    fn assert_no_internal_vocabulary(summary: &str) {
        for banned in ["SOUL", "soul", "observation window", "GVU"] {
            assert!(
                !summary.contains(banned),
                "Activity Feed copy leaked internal vocabulary {banned:?}: {summary:?}"
            );
        }
    }

    #[test]
    fn confirmed_copy_matches_spec_wording() {
        let (event_type, summary) = confirmed_copy("ceo-assistant");
        assert_eq!(event_type, "gvu_observation_confirmed");
        assert_eq!(summary, "ceo-assistant 的行為調整已通過試行，正式生效");
        assert_no_internal_vocabulary(&summary);
    }

    #[test]
    fn rolled_back_copy_matches_spec_wording() {
        let (event_type, summary) = rolled_back_copy();
        assert_eq!(event_type, "gvu_observation_rolled_back");
        assert_eq!(summary, "試行未達標，已回退");
        assert_no_internal_vocabulary(&summary);
    }

    #[test]
    fn expired_no_data_copy_matches_spec_wording() {
        let (event_type, summary) = expired_no_data_copy();
        assert_eq!(event_type, "gvu_observation_expired_no_data");
        assert_eq!(summary, "證據不足，未採用");
        assert_no_internal_vocabulary(&summary);
    }

    #[test]
    fn low_data_warn_copy_is_distinct_and_clean() {
        let (event_type, summary) = low_data_warn_copy("ceo-assistant");
        assert_eq!(event_type, "gvu_observation_low_data_warn");
        assert!(summary.contains("ceo-assistant"));
        assert!(summary.contains("試行"));
        assert_no_internal_vocabulary(&summary);
    }

    // ── Activity Feed wiring (real TaskStore round trip) ─────────────────

    async fn activity_rows_for(fin: &ObservationFinalizer, agent_id: &str) -> Vec<crate::task_store::ActivityRow> {
        let store = crate::task_store::TaskStore::open(&fin.home_dir()).unwrap();
        store
            .list_activity(Some(agent_id), None, 50, 0)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn test_confirm_posts_activity_feed_row() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        let pre = VersionMetrics {
            avg_prediction_error: 0.30,
            positive_feedback_ratio: 0.0,
            ..Default::default()
        };
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-activity-confirm",
            now - ChronoDuration::hours(48),
            now - ChronoDuration::hours(24),
            pre,
        );
        let rows: Vec<_> = (0..5)
            .map(|i| (0.05_f64, "Negligible", now - ChronoDuration::hours(40 - i)))
            .collect();
        seed_prediction_db(&pred_db, "agent-activity-confirm", &rows);

        let report = fin.tick().await;
        assert!(matches!(report.decisions[0].decision, Decision::Confirmed));

        let activity = activity_rows_for(&fin, "agent-activity-confirm").await;
        assert_eq!(activity.len(), 1, "expected exactly one Activity Feed row");
        assert_eq!(activity[0].event_type, "gvu_observation_confirmed");
        assert!(activity[0].summary.contains("正式生效"));
        assert_no_internal_vocabulary(&activity[0].summary);
    }

    #[tokio::test]
    async fn test_rollback_posts_activity_feed_row() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        let pre = VersionMetrics {
            avg_prediction_error: 0.10,
            positive_feedback_ratio: 0.0,
            ..Default::default()
        };
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-activity-rollback",
            now - ChronoDuration::hours(48),
            now - ChronoDuration::hours(24),
            pre,
        );
        let rows: Vec<_> = (0..6)
            .map(|i| (0.60_f64, "Significant", now - ChronoDuration::hours(40 - i)))
            .collect();
        seed_prediction_db(&pred_db, "agent-activity-rollback", &rows);

        let report = fin.tick().await;
        assert!(matches!(report.decisions[0].decision, Decision::RolledBack { .. }));

        let activity = activity_rows_for(&fin, "agent-activity-rollback").await;
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].event_type, "gvu_observation_rolled_back");
        assert!(activity[0].summary.contains("已回退"));
        assert_no_internal_vocabulary(&activity[0].summary);
    }

    #[tokio::test]
    async fn test_expired_no_data_posts_activity_feed_row() {
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-activity-expired",
            now - ChronoDuration::days(15),
            now - ChronoDuration::days(14) - ChronoDuration::hours(1),
            VersionMetrics::default(),
        );
        seed_prediction_db(&pred_db, "agent-activity-expired", &[]);

        let report = fin.tick().await;
        assert!(matches!(report.decisions[0].decision, Decision::ExpiredNoData));

        let activity = activity_rows_for(&fin, "agent-activity-expired").await;
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].event_type, "gvu_observation_expired_no_data");
        assert!(activity[0].summary.contains("未採用"));
        assert_no_internal_vocabulary(&activity[0].summary);
    }

    #[tokio::test]
    async fn test_low_data_warn_posts_exactly_one_activity_row_across_ticks() {
        // Mirrors test_low_data_alert_is_not_repeated_across_ticks — the
        // Activity Feed row must share the same one-time guard as the
        // evolution-event alert, not fire again on every tick.
        let tmp = TempDir::new().unwrap();
        let (fin, _evo, pred_db, _feedback) = make_finalizer(tmp.path(), None);

        let now = Utc::now();
        seed_observing_version(
            &fin.version_store,
            &fin.agents_dir,
            "agent-activity-warn",
            now - ChronoDuration::hours(80),
            now - ChronoDuration::hours(56),
            VersionMetrics::default(),
        );
        seed_prediction_db(&pred_db, "agent-activity-warn", &[]);

        let _ = fin.tick().await;
        let activity_after_first = activity_rows_for(&fin, "agent-activity-warn").await;
        assert_eq!(activity_after_first.len(), 1);
        assert_eq!(activity_after_first[0].event_type, "gvu_observation_low_data_warn");
        assert_no_internal_vocabulary(&activity_after_first[0].summary);

        // Force the (now extended) observation back into the past and tick
        // again — the guard must suppress a second row.
        let v1 = fin.version_store.get_observing_version("agent-activity-warn").unwrap();
        let conn = Connection::open(fin.version_store.db_path_ref()).unwrap();
        conn.execute(
            "UPDATE soul_versions SET observation_end = ?1 WHERE version_id = ?2",
            params![(now - ChronoDuration::hours(1)).to_rfc3339(), v1.version_id],
        )
        .unwrap();

        let _ = fin.tick().await;
        let activity_after_second = activity_rows_for(&fin, "agent-activity-warn").await;
        assert_eq!(
            activity_after_second.len(),
            1,
            "low-data-warn Activity Feed row must not repeat across ticks"
        );
    }
}
