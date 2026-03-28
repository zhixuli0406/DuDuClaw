//! Updater — applies verified proposals with versioning and rollback capability.
//!
//! After a proposal passes verification:
//! 1. Read current SOUL.md → compute rollback content
//! 2. Apply changes → write new SOUL.md
//! 3. Update soul_guard hash (accept_soul_change)
//! 4. Record version in VersionStore with observation period
//!
//! After observation period ends:
//! 5. Collect post-metrics → compare with pre-metrics
//! 6. Confirm or rollback based on tolerance thresholds

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::proposal::EvolutionProposal;
use super::version_store::{SoulVersion, VersionMetrics, VersionStatus, VersionStore};

/// Default observation period: 24 hours.
const DEFAULT_OBSERVATION_HOURS: f64 = 24.0;

/// Outcome of judging an observation period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutcomeVerdict {
    /// Metrics within tolerance — confirm the change.
    Confirm,
    /// Metrics degraded — rollback.
    Rollback { reason: String },
    /// Not enough data — extend observation.
    ExtendObservation { extra_hours: f64 },
}

/// Updater applies proposals and manages the observation lifecycle.
pub struct Updater {
    version_store: VersionStore,
    observation_hours: f64,
}

impl Updater {
    pub fn new(version_store: VersionStore, observation_hours: Option<f64>) -> Self {
        Self {
            version_store,
            observation_hours: observation_hours.unwrap_or(DEFAULT_OBSERVATION_HOURS),
        }
    }

    /// Access the version store (for heartbeat observation checking).
    pub fn version_store(&self) -> &VersionStore {
        &self.version_store
    }

    /// Apply a verified proposal to SOUL.md.
    ///
    /// Returns the created SoulVersion for tracking.
    pub fn apply(
        &self,
        proposal: &EvolutionProposal,
        agent_dir: &Path,
        pre_metrics: VersionMetrics,
    ) -> Result<SoulVersion, String> {
        let soul_path = agent_dir.join("SOUL.md");

        // Read current SOUL.md (for rollback)
        let current_content = std::fs::read_to_string(&soul_path)
            .map_err(|e| format!("Failed to read SOUL.md: {e}"))?;

        // Build new SOUL.md content by appending the proposed changes.
        // Always append rather than replace — this prevents a malicious or
        // broken LLM output from wiping out the entire SOUL.md.
        let new_content = format!(
            "{}\n\n<!-- Evolution update ({}) -->\n{}",
            current_content,
            Utc::now().format("%Y-%m-%d"),
            proposal.content,
        );

        // Validate new content
        if new_content.trim().is_empty() {
            return Err("Resulting SOUL.md would be empty".to_string());
        }
        if new_content.len() > 50_000 {
            return Err("Resulting SOUL.md exceeds 50KB limit".to_string());
        }

        // Atomic write: write to temp file, then rename (prevents truncated SOUL.md on crash)
        let tmp_path = soul_path.with_extension("md.gvu_tmp");
        std::fs::write(&tmp_path, &new_content)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to write temp SOUL.md: {e}")
            })?;

        // Record version BEFORE rename — if this fails, temp file is cleaned up and SOUL.md is untouched
        // (version recording moved here, before the actual file swap)

        // Compute hash of new content
        let soul_hash = {
            use ring::digest;
            let d = digest::digest(&digest::SHA256, new_content.as_bytes());
            d.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

        let now = Utc::now();
        let observation_end = now + chrono::Duration::seconds((self.observation_hours * 3600.0) as i64);

        let version = SoulVersion {
            version_id: uuid::Uuid::new_v4().to_string(),
            agent_id: proposal.agent_id.clone(),
            soul_hash,
            soul_summary: new_content.chars().take(200).collect(),
            applied_at: now,
            observation_end,
            status: VersionStatus::Observing,
            pre_metrics,
            post_metrics: None,
            proposal_id: proposal.id.clone(),
            rollback_diff: current_content,
        };

        // Step 1: Record version to SQLite (if this fails, SOUL.md is untouched)
        if let Err(e) = self.version_store.record_version(&version) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("Failed to record version: {e}"));
        }

        // Step 2: Atomic rename (if this fails, version record exists but SOUL.md unchanged — safe)
        if let Err(e) = std::fs::rename(&tmp_path, &soul_path) {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = self.version_store.mark_rolled_back(&version.version_id, "rename failed");
            return Err(format!("Failed to rename SOUL.md: {e}"));
        }

        // Step 3: Update soul_guard hash (if this fails, next heartbeat detects drift — recoverable)
        if let Err(e) = duduclaw_security::soul_guard::accept_soul_change(&proposal.agent_id, agent_dir) {
            warn!(
                agent = %proposal.agent_id,
                "Failed to update soul hash after apply: {e} — soul_guard will detect drift on next heartbeat"
            );
        }

        info!(
            agent = %proposal.agent_id,
            version = %version.version_id,
            observation_end = %observation_end.to_rfc3339(),
            "SOUL.md updated atomically, observation period started"
        );

        Ok(version)
    }

    /// Judge whether an observation period passed or failed.
    pub fn judge_outcome(
        &self,
        version: &SoulVersion,
        post_metrics: &VersionMetrics,
    ) -> OutcomeVerdict {
        // Not enough data → extend
        if post_metrics.conversations_count < 5 {
            return OutcomeVerdict::ExtendObservation { extra_hours: 12.0 };
        }

        let pre = &version.pre_metrics;

        // Check feedback ratio: tolerate 3% dip
        let feedback_delta = post_metrics.positive_feedback_ratio - pre.positive_feedback_ratio;
        if feedback_delta < -0.03 && pre.positive_feedback_ratio > 0.0 {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Feedback ratio dropped {:.1}% (from {:.2} to {:.2})",
                    feedback_delta.abs() * 100.0,
                    pre.positive_feedback_ratio,
                    post_metrics.positive_feedback_ratio,
                ),
            };
        }

        // Check prediction error: tolerate 5% increase
        let error_delta = post_metrics.avg_prediction_error - pre.avg_prediction_error;
        if error_delta > 0.05 && pre.avg_prediction_error > 0.0 {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Prediction error increased {:.1}% (from {:.3} to {:.3})",
                    error_delta * 100.0,
                    pre.avg_prediction_error,
                    post_metrics.avg_prediction_error,
                ),
            };
        }

        // Check contract violations: must not increase
        if post_metrics.contract_violations > pre.contract_violations {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Contract violations increased from {} to {}",
                    pre.contract_violations, post_metrics.contract_violations,
                ),
            };
        }

        OutcomeVerdict::Confirm
    }

    /// Execute a rollback: restore SOUL.md from the version's stored content.
    pub fn execute_rollback(
        &self,
        version: &SoulVersion,
        agent_dir: &Path,
    ) -> Result<(), String> {
        let soul_path = agent_dir.join("SOUL.md");

        // Atomic rollback: write to temp file, then rename (same pattern as apply)
        let tmp_path = soul_path.with_extension("md.rollback_tmp");
        std::fs::write(&tmp_path, &version.rollback_diff)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to write rollback tmp: {e}")
            })?;
        std::fs::rename(&tmp_path, &soul_path)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to rename rollback: {e}")
            })?;

        // Update soul_guard hash
        if let Err(e) = duduclaw_security::soul_guard::accept_soul_change(&version.agent_id, agent_dir) {
            warn!(agent = %version.agent_id, "Failed to update soul hash after rollback: {e}");
        }

        self.version_store.mark_rolled_back(&version.version_id, "observation_failed")?;

        warn!(
            agent = %version.agent_id,
            version = %version.version_id,
            "SOUL.md rolled back to previous version"
        );

        Ok(())
    }

    /// Confirm a version: mark it as permanent.
    pub fn execute_confirm(
        &self,
        version: &SoulVersion,
        post_metrics: &VersionMetrics,
    ) -> Result<(), String> {
        self.version_store.mark_confirmed(&version.version_id, post_metrics)?;

        info!(
            agent = %version.agent_id,
            version = %version.version_id,
            "SOUL.md version confirmed as permanent"
        );

        Ok(())
    }
}
