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
//! auditable from the gateway log.

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
    Failed { error: String },
}

/// Aggregate result of one finalize sweep.
#[derive(Debug, Default, Clone)]
pub struct FinalizationReport {
    pub decisions: Vec<FinalizationDecision>,
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

    /// Run a single sweep — finalise every expired observation, returning a
    /// per-version decision log.
    pub async fn tick(&self) -> FinalizationReport {
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

        let positive_ratio =
            count_positive_feedback_ratio(&self.feedback_jsonl, agent_id, since)
                .unwrap_or(0.0);

        Ok(VersionMetrics {
            positive_feedback_ratio: positive_ratio,
            avg_prediction_error: avg_err,
            user_correction_rate: correction_rate,
            contract_violations: 0,
            conversations_count: total,
        })
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
}
