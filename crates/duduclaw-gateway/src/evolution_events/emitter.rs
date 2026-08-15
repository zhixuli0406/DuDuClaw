//! EvolutionEventEmitter — non-blocking typed wrapper around [`EvolutionEventLogger`].
//!
//! ## Design
//! All `emit_*` methods fire-and-forget via [`tokio::spawn`] so they **never block**
//! the caller's async task.  Logging errors degrade to stderr (same as the logger).
//!
//! ## Usage — owned instance
//! ```rust,ignore
//! let emitter = EvolutionEventEmitter::from_env();
//! emitter.emit_skill_activate("my-agent", "python-patterns", "prediction_error_diagnosis");
//! ```
//!
//! ## Usage — process-global singleton
//! ```rust,ignore
//! // In MCP handlers or other sites where threading the emitter is impractical:
//! EvolutionEventEmitter::global()
//!     .emit_security_scan("my-agent", "my-skill", true, serde_json::json!({}));
//! ```
//!
//! ## P0 / P1 notes
//! - `generation` is always `None` in P0.  GVU world-generation tracking is reserved for P1.
//! - `emit_signal_suppressed_stub` is a **P0 stub**: the event is emitted unconditionally
//!   so the JSONL schema is exercised end-to-end, but the stagnation-detection *condition*
//!   (when to call this) lives in P1.

use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde_json::Value as Json;
use tokio::task::JoinHandle;

use super::{
    logger::EvolutionEventLogger,
    schema::{AuditEvent, AuditEventType, Outcome},
};

// ── Emitter ───────────────────────────────────────────────────────────────────

/// Default bound for [`EvolutionEventEmitter::wait_pending_default`] — long
/// enough for a cold-start `create_dir_all` + file open on a healthy
/// filesystem, short enough that a genuinely stuck write can't hang a CLI
/// command's shutdown indefinitely.
const DEFAULT_WAIT_PENDING_TIMEOUT: Duration = Duration::from_secs(3);

/// Non-blocking emitter of [`AuditEvent`] records.
///
/// Wraps an [`EvolutionEventLogger`] and exposes typed helpers for each of the
/// five `event_type` variants.  Every emit call spawns a detached Tokio task so
/// the caller is never blocked by I/O.
///
/// ## B3 — one-shot CLI shutdown race
/// `emit_*` calls are fire-and-forget: they never block the caller, which is
/// exactly right for the long-running gateway (the process — and its Tokio
/// `Runtime` — stays alive for the write's whole lifetime). It is **not**
/// safe for a one-shot CLI command (`duduclaw evolution finalize` and
/// similar): `#[tokio::main]` drops the `Runtime` as soon as `main()`
/// returns, which can abort an in-flight `tokio::spawn`'d write mid
/// `create_dir_all`/`open`. The aborted `spawn_blocking` join surfaces as
/// `io::Error::new(Other, "background task failed")`, logged by
/// [`EvolutionEventLogger::log`] as `Failed to open audit log file: ...` even
/// though nothing is actually broken — the directory/file get created fine
/// on a second run because there's no race the second time. One-shot callers
/// must call [`Self::wait_pending`] (or [`Self::wait_pending_default`])
/// after their last `emit_*` call and before returning, so the spawned write
/// tasks are joined instead of racing process exit.
pub struct EvolutionEventEmitter {
    logger: Arc<EvolutionEventLogger>,
    /// In-flight audit-write tasks spawned by [`Self::spawn`], tracked solely
    /// so [`Self::wait_pending`] has something to join. Opportunistically
    /// pruned of already-finished handles on every `spawn()` call so a
    /// long-lived process (the gateway, which never calls `wait_pending`)
    /// doesn't accumulate an unbounded `Vec`.
    inflight: Arc<StdMutex<Vec<JoinHandle<()>>>>,
}

impl EvolutionEventEmitter {
    /// Create an emitter backed by the provided logger.
    pub fn new(logger: Arc<EvolutionEventLogger>) -> Self {
        Self {
            logger,
            inflight: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    /// Create an emitter using the environment-configured logger.
    ///
    /// Reads `$EVOLUTION_EVENTS_DIR`; falls back to `data/evolution/events/`.
    pub fn from_env() -> Self {
        Self::new(Arc::new(EvolutionEventLogger::from_env()))
    }

    /// Return the process-global singleton emitter.
    ///
    /// Initialised lazily on first access via [`from_env`](Self::from_env).
    /// Intended for call sites (e.g. MCP handlers) where threading the emitter
    /// through the call chain is not practical.
    pub fn global() -> &'static EvolutionEventEmitter {
        static INSTANCE: OnceLock<EvolutionEventEmitter> = OnceLock::new();
        INSTANCE.get_or_init(Self::from_env)
    }

    // ── Typed emit methods ────────────────────────────────────────────────────

    /// Emit a `skill_activate` event (non-blocking).
    ///
    /// Call immediately after [`SkillActivationController::activate`] returns.
    pub fn emit_skill_activate(&self, agent_id: &str, skill_id: &str, trigger_signal: &str) {
        self.spawn(
            AuditEvent::now(AuditEventType::SkillActivate, agent_id, Outcome::Success)
                .with_skill_id(skill_id)
                .with_trigger_signal(trigger_signal),
        );
    }

    /// Emit a `skill_deactivate` event (non-blocking).
    ///
    /// Call at each deactivation site: effectiveness evaluation, sandbox trial
    /// DISCARD, and capacity eviction.  The `trigger_signal` distinguishes them:
    /// - `"effectiveness_evaluation"` — periodic evaluation of underperforming skills
    /// - `"sandbox_trial_discard"` — sandbox trial decision was DISCARD
    /// - `"capacity_eviction"` — skill evicted to make room for a new one
    pub fn emit_skill_deactivate(
        &self,
        agent_id: &str,
        skill_id: &str,
        trigger_signal: &str,
        metadata: Json,
    ) {
        self.spawn(
            AuditEvent::now(AuditEventType::SkillDeactivate, agent_id, Outcome::Success)
                .with_skill_id(skill_id)
                .with_trigger_signal(trigger_signal)
                .with_metadata(metadata),
        );
    }

    /// Emit a `security_scan` event (non-blocking).
    ///
    /// Call immediately after `security_scanner::scan_skill()` returns.
    /// `passed` maps to `Outcome::Success`; `!passed` maps to `Outcome::Failure`.
    pub fn emit_security_scan(
        &self,
        agent_id: &str,
        skill_id: &str,
        passed: bool,
        metadata: Json,
    ) {
        let outcome = if passed { Outcome::Success } else { Outcome::Failure };
        self.spawn(
            AuditEvent::now(AuditEventType::SecurityScan, agent_id, outcome)
                .with_skill_id(skill_id)
                .with_trigger_signal("skill_security_scan")
                .with_metadata(metadata),
        );
    }

    /// Emit a `gvu_generation` event (non-blocking).
    ///
    /// Call inside the `match outcome { ... }` block after `gvu.run_with_context()`.
    /// Map each [`GvuOutcome`] variant to an [`Outcome`]:
    /// - `Applied` → `Outcome::Success`
    /// - `Abandoned | Skipped | Deferred | TimedOut` → `Outcome::Failure`
    ///
    /// `generation` is always `None` in P0.  GVU world-generation tracking is P1.
    pub fn emit_gvu_generation(
        &self,
        agent_id: &str,
        outcome: Outcome,
        trigger_signal: &str,
        metadata: Json,
    ) {
        // generation = null per P0 spec — world-generation tracking is P1
        self.spawn(
            AuditEvent::now(AuditEventType::GvuGeneration, agent_id, outcome)
                .with_trigger_signal(trigger_signal)
                .with_metadata(metadata),
        );
    }

    /// Emit a `skill_graduate` event (non-blocking).
    ///
    /// Called by the Rollout-to-Skill pipeline (W19-P0) after a skill has passed
    /// the quality threshold, security scan, and been written to the Skill Bank.
    ///
    /// ## Required metadata fields
    /// - `quality_score` (f64 in [0,1]) — composite quality score from the scorer
    /// - `source_trajectories` (u64) — number of trajectories that contributed
    /// - `pipeline_version` (str) — pipeline version tag, e.g. `"W19-P0"`
    pub fn emit_skill_graduate(
        &self,
        agent_id: &str,
        skill_id: &str,
        metadata: Json,
    ) {
        self.spawn(
            AuditEvent::now(AuditEventType::SkillGraduate, agent_id, Outcome::Success)
                .with_skill_id(skill_id)
                .with_trigger_signal("rollout_to_skill_pipeline")
                .with_metadata(metadata),
        );
    }

    /// Emit a `signal_suppressed` stub event (non-blocking).
    ///
    /// **P0 stub** — the *condition* for calling this (stagnation detection) is
    /// not implemented in P0.  The emit call itself is wired and works end-to-end
    /// so P1 only needs to add the condition guard.
    ///
    /// ## Canonical P0 metadata (Spec §1.1 — Option C, null placeholders)
    ///
    /// Pass the following stub metadata in P0 calls to preserve schema
    /// forward-compatibility.  P1 replaces `null` values with real data:
    ///
    /// ```json
    /// { "suppressed_signal": null, "trigger_count": null, "window_seconds": null }
    /// ```
    ///
    /// Field semantics (from Spec §1.1):
    /// - `suppressed_signal` — P1 will set to the upstream signal name that was suppressed
    ///                         (e.g. `"prediction_error_diagnosis"`).
    /// - `trigger_count`     — P1 will set to the consecutive failure count that crossed the threshold.
    /// - `window_seconds`    — P1 will set to the observation window size (mirrors `stagnation_detection.window_seconds`).
    ///
    /// TODO P1: evaluate `evolution_toggle.stagnation_detection` config before
    /// calling this.  See T3 for the config schema.
    pub fn emit_signal_suppressed_stub(&self, agent_id: &str, metadata: Json) {
        // TODO P1: wrap this call with a stagnation_detection threshold check.
        // e.g.:  if consecutive_triggers >= stagnation_cfg.trigger_threshold { ... }
        self.spawn(
            AuditEvent::now(AuditEventType::SignalSuppressed, agent_id, Outcome::Suppressed)
                .with_trigger_signal("stagnation_detection")
                .with_metadata(metadata),
        );
    }

    // ── W19-P1: Governance domain helpers ────────────────────────────────────

    /// Emit a `governance_violation` event (non-blocking).
    ///
    /// Call from `PolicyEvaluator` after a violation is detected and recorded.
    ///
    /// ## metadata fields
    /// `{"policy_id", "policy_type", "violation_detail", "operation_type"}`
    pub fn emit_governance_violation(
        &self,
        agent_id: &str,
        outcome: Outcome, // Blocked | Warned | Throttled
        metadata: Json,
    ) {
        self.spawn(
            AuditEvent::now(AuditEventType::GovernanceViolation, agent_id, outcome)
                .with_trigger_signal("policy_evaluator")
                .with_metadata(metadata),
        );
    }

    /// Emit a `governance_approval_requested` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"approval_request_id", "operation_type", "justification"}`
    pub fn emit_governance_approval_requested(
        &self,
        agent_id: &str,
        metadata: Json,
    ) {
        self.spawn(
            AuditEvent::now(
                AuditEventType::GovernanceApprovalRequested,
                agent_id,
                Outcome::Pending,
            )
            .with_trigger_signal("approval_workflow")
            .with_metadata(metadata),
        );
    }

    /// Emit a `governance_approval_decided` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"approval_request_id", "approver_id", "reason"}`
    pub fn emit_governance_approval_decided(
        &self,
        agent_id: &str,
        outcome: Outcome, // Approved | Rejected
        metadata: Json,
    ) {
        self.spawn(
            AuditEvent::now(AuditEventType::GovernanceApprovalDecided, agent_id, outcome)
                .with_trigger_signal("approval_workflow")
                .with_metadata(metadata),
        );
    }

    /// Emit a `governance_policy_changed` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"policy_id", "policy_type", "change_type": "create|update|delete"}`
    pub fn emit_governance_policy_changed(
        &self,
        agent_id: &str,
        success: bool,
        metadata: Json,
    ) {
        let outcome = if success { Outcome::Success } else { Outcome::Failure };
        self.spawn(
            AuditEvent::now(AuditEventType::GovernancePolicyChanged, agent_id, outcome)
                .with_trigger_signal("policy_registry")
                .with_metadata(metadata),
        );
    }

    /// Emit a `governance_quota_reset` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"policy_id", "agents_affected": N}`
    pub fn emit_governance_quota_reset(&self, agent_id: &str, metadata: Json) {
        self.spawn(
            AuditEvent::now(AuditEventType::GovernanceQuotaReset, agent_id, Outcome::Success)
                .with_trigger_signal("quota_manager")
                .with_metadata(metadata),
        );
    }

    // ── W19-P1: Durability domain helpers ────────────────────────────────────

    /// Emit a `durability_retry_attempt` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"attempt_number", "max_attempts", "delay_ms", "error_code"}`
    pub fn emit_durability_retry_attempt(
        &self,
        agent_id: &str,
        success: bool,
        metadata: Json,
    ) {
        let outcome = if success { Outcome::Success } else { Outcome::Failure };
        self.spawn(
            AuditEvent::now(AuditEventType::DurabilityRetryAttempt, agent_id, outcome)
                .with_trigger_signal("retry_engine")
                .with_metadata(metadata),
        );
    }

    /// Emit a `durability_retry_exhausted` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"operation_type", "max_attempts", "dlq_id", "last_error"}`
    pub fn emit_durability_retry_exhausted(&self, agent_id: &str, metadata: Json) {
        self.spawn(
            AuditEvent::now(AuditEventType::DurabilityRetryExhausted, agent_id, Outcome::Failure)
                .with_trigger_signal("retry_engine")
                .with_metadata(metadata),
        );
    }

    /// Emit a `durability_circuit_opened` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"dependency", "failure_rate", "request_count", "reset_timeout_seconds"}`
    pub fn emit_durability_circuit_opened(&self, agent_id: &str, metadata: Json) {
        self.spawn(
            AuditEvent::now(
                AuditEventType::DurabilityCircuitOpened,
                agent_id,
                Outcome::Triggered,
            )
            .with_trigger_signal("circuit_breaker")
            .with_metadata(metadata),
        );
    }

    /// Emit a `durability_circuit_recovered` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"dependency", "probe_success_count"}`
    pub fn emit_durability_circuit_recovered(&self, agent_id: &str, metadata: Json) {
        self.spawn(
            AuditEvent::now(
                AuditEventType::DurabilityCircuitRecovered,
                agent_id,
                Outcome::Recovered,
            )
            .with_trigger_signal("circuit_breaker")
            .with_metadata(metadata),
        );
    }

    /// Emit a `durability_checkpoint_saved` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"checkpoint_id", "phase", "ttl_seconds"}`
    pub fn emit_durability_checkpoint_saved(
        &self,
        agent_id: &str,
        success: bool,
        metadata: Json,
    ) {
        let outcome = if success { Outcome::Success } else { Outcome::Failure };
        self.spawn(
            AuditEvent::now(AuditEventType::DurabilityCheckpointSaved, agent_id, outcome)
                .with_trigger_signal("checkpoint_manager")
                .with_metadata(metadata),
        );
    }

    /// Emit a `durability_dlq_replayed` event (non-blocking).
    ///
    /// ## metadata fields
    /// `{"dlq_id", "operation_type", "replayed_by"}`
    pub fn emit_durability_dlq_replayed(
        &self,
        agent_id: &str,
        success: bool,
        metadata: Json,
    ) {
        let outcome = if success { Outcome::Success } else { Outcome::Failure };
        self.spawn(
            AuditEvent::now(AuditEventType::DurabilityDlqReplayed, agent_id, outcome)
                .with_trigger_signal("dead_letter_queue")
                .with_metadata(metadata),
        );
    }

    // ── OS-native P2-3: proactive four-quadrant domain helper ───────────────────

    /// Emit a `proactive_quadrant` event (non-blocking).
    ///
    /// Call from `proactive_feedback::quadrant_stats_and_emit` after
    /// aggregating `<home>/proactive_gate.jsonl` outcomes for one agent.
    /// Always `Outcome::Success` — this reports a measurement, not a pass/fail
    /// action.
    pub fn emit_proactive_quadrant(&self, agent_id: &str, metadata: Json) {
        self.spawn(
            AuditEvent::now(AuditEventType::ProactiveQuadrant, agent_id, Outcome::Success)
                .with_trigger_signal("proactive_gate_calibration")
                .with_metadata(metadata),
        );
    }

    // ── Evolution v3 / AEE: per-round audit trail ────────────────────────────

    /// Emit an `aee_round` event (non-blocking).
    ///
    /// Call once per [`crate::gvu::aee::AeeRoundRecord`] — the WP2.3 §3.2.4
    /// audit record already produced at the end of every AEE round
    /// (committed, abandoned, or skipped). Previously this record was only
    /// `tracing::debug!`-logged (`gvu/loop_.rs`), so it never reached any
    /// durable, queryable sink; wiring it here also carries the WP-6A / A2
    /// harness-knob snapshot the record now includes
    /// (`crate::gvu::knob_snapshot`) into the same durable trail, so a later
    /// retrospective read has both "what happened" and "under which
    /// settings" in one row. See [`super::schema::AuditEventType::AeeRound`]
    /// for the metadata shape this schema variant was already declared for.
    ///
    /// `outcome`: `Success` when the round committed, `Failure` when it ran
    /// but nothing committed (gate/commit-gate rejection), `Suppressed` when
    /// the round never started (cooldown / no material) — matching the
    /// contract the `AeeRound` schema doc comment already specified.
    pub fn emit_aee_round(&self, record: &crate::gvu::aee::AeeRoundRecord, outcome: Outcome) {
        self.spawn(
            AuditEvent::now(AuditEventType::AeeRound, record.agent_id.as_str(), outcome)
                .with_trigger_signal(record.trigger.as_str())
                .with_metadata(record.to_payload()),
        );
    }

    // ── B3: one-shot CLI shutdown synchronisation ────────────────────────────

    /// Wait (bounded by `timeout`) for every audit-write task spawned so far
    /// to finish, then return.
    ///
    /// One-shot CLI commands must call this after their last `emit_*` call
    /// and before returning — see the "B3" note on [`EvolutionEventEmitter`]
    /// for why. The long-running gateway never needs to call this.
    ///
    /// On timeout, any still-running tasks are left to finish in the
    /// background on a best-effort basis (they are simply no longer
    /// tracked) rather than hanging CLI shutdown forever.
    pub async fn wait_pending(&self, timeout: Duration) {
        let handles: Vec<JoinHandle<()>> = {
            let mut guard = match self.inflight.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::take(&mut *guard)
        };
        if handles.is_empty() {
            return;
        }
        let join_all = async {
            for h in handles {
                // A join error here means the write task panicked — nothing
                // more to do; the panic itself would already have been
                // reported by the default Tokio panic hook.
                let _ = h.await;
            }
        };
        let _ = tokio::time::timeout(timeout, join_all).await;
    }

    /// [`Self::wait_pending`] with the crate-default timeout
    /// ([`DEFAULT_WAIT_PENDING_TIMEOUT`]). Convenience for the common
    /// one-shot-CLI-shutdown call site.
    pub async fn wait_pending_default(&self) {
        self.wait_pending(DEFAULT_WAIT_PENDING_TIMEOUT).await;
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    /// Fire-and-forget: log `event` via [`tokio::spawn`].
    ///
    /// The spawned task holds a clone of the logger `Arc` and runs to completion
    /// independently of the caller.  Write errors degrade to `stderr`. The
    /// `JoinHandle` is tracked in `inflight` so [`Self::wait_pending`] can
    /// synchronise on it later (B3); already-finished handles are pruned on
    /// every call so long-lived processes don't leak memory here.
    fn spawn(&self, event: AuditEvent) {
        let logger = Arc::clone(&self.logger);
        let handle = tokio::spawn(async move {
            logger.log(event).await;
        });
        if let Ok(mut guard) = self.inflight.lock() {
            guard.retain(|h| !h.is_finished());
            guard.push(handle);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::time::sleep;

    use super::*;
    use crate::evolution_events::logger::EvolutionEventLogger;
    use crate::evolution_events::schema::AuditEventType;

    // Helper: build an emitter backed by a temp directory.
    fn make_emitter(dir: &std::path::Path) -> EvolutionEventEmitter {
        let logger = Arc::new(EvolutionEventLogger::new(dir));
        EvolutionEventEmitter::new(logger)
    }

    // Helper: read all JSONL lines written today.
    async fn read_lines(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let path = dir.join(format!("{today}.jsonl"));
        if !path.exists() {
            return Vec::new();
        }
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("valid JSON line"))
            .collect()
    }

    // ── skill_activate ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_emit_skill_activate_writes_event() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_skill_activate("agent-1", "python-patterns", "prediction_error_diagnosis");
        sleep(Duration::from_millis(50)).await; // let spawn finish

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 1);
        let ev = &lines[0];
        assert_eq!(ev["event_type"], "skill_activate");
        assert_eq!(ev["agent_id"], "agent-1");
        assert_eq!(ev["skill_id"], "python-patterns");
        assert_eq!(ev["trigger_signal"], "prediction_error_diagnosis");
        assert_eq!(ev["outcome"], "success");
        // P0: generation must be null
        assert_eq!(ev["generation"], serde_json::Value::Null);
    }

    // ── skill_deactivate ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_emit_skill_deactivate_effectiveness_evaluation() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_skill_deactivate(
            "agent-2",
            "slow-skill",
            "effectiveness_evaluation",
            serde_json::json!({"conversations": 20}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 1);
        let ev = &lines[0];
        assert_eq!(ev["event_type"], "skill_deactivate");
        assert_eq!(ev["trigger_signal"], "effectiveness_evaluation");
        assert_eq!(ev["outcome"], "success");
    }

    #[tokio::test]
    async fn test_emit_skill_deactivate_sandbox_trial_discard() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_skill_deactivate(
            "agent-3",
            "trial-skill",
            "sandbox_trial_discard",
            serde_json::json!({"reason": "no lift after 30 conversations"}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines[0]["trigger_signal"], "sandbox_trial_discard");
        assert_eq!(ev_type(&lines[0]), "skill_deactivate");
    }

    #[tokio::test]
    async fn test_emit_skill_deactivate_capacity_eviction() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_skill_deactivate(
            "agent-cap",
            "worst-skill",
            "capacity_eviction",
            serde_json::json!({
                "reason": "max_active_capacity_exceeded",
                "new_skill": "new-skill",
            }),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 1);
        let ev = &lines[0];
        assert_eq!(ev["event_type"], "skill_deactivate");
        assert_eq!(ev["trigger_signal"], "capacity_eviction");
        assert_eq!(ev["outcome"], "success");
        assert_eq!(ev["skill_id"], "worst-skill");
        assert_eq!(ev["agent_id"], "agent-cap");
    }

    // ── security_scan ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_emit_security_scan_passed() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_security_scan(
            "agent-4",
            "my-skill",
            true,
            serde_json::json!({"risk_level": "Clean", "findings": 0}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        let ev = &lines[0];
        assert_eq!(ev_type(ev), "security_scan");
        assert_eq!(ev["outcome"], "success");
        assert_eq!(ev["trigger_signal"], "skill_security_scan");
        assert_eq!(ev["skill_id"], "my-skill");
    }

    #[tokio::test]
    async fn test_emit_security_scan_failed() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_security_scan(
            "agent-5",
            "bad-skill",
            false,
            serde_json::json!({"risk_level": "High", "findings": 3}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines[0]["outcome"], "failure");
    }

    // ── gvu_generation ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_emit_gvu_generation_applied() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_gvu_generation(
            "agent-6",
            Outcome::Success,
            "gvu_trigger",
            serde_json::json!({"gvu_outcome": "applied", "version_id": "v42"}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        let ev = &lines[0];
        assert_eq!(ev_type(ev), "gvu_generation");
        assert_eq!(ev["outcome"], "success");
        // P0: generation must be null
        assert_eq!(ev["generation"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_emit_gvu_generation_abandoned() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_gvu_generation(
            "agent-7",
            Outcome::Failure,
            "gvu_trigger",
            serde_json::json!({"gvu_outcome": "abandoned"}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines[0]["outcome"], "failure");
    }

    // ── signal_suppressed (P0 stub) ───────────────────────────────────────────

    #[tokio::test]
    async fn test_emit_signal_suppressed_stub_writes_event() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        // P0 canonical stub metadata: null placeholders for P1 fields.
        emitter.emit_signal_suppressed_stub(
            "agent-8",
            serde_json::json!({"suppressed_signal": null, "trigger_count": null, "window_seconds": null}),
        );
        sleep(Duration::from_millis(50)).await;

        let lines = read_lines(tmp.path()).await;
        let ev = &lines[0];
        assert_eq!(ev_type(ev), "signal_suppressed");
        assert_eq!(ev["outcome"], "suppressed");
        assert_eq!(ev["trigger_signal"], "stagnation_detection");
        // P0: generation must be null
        assert_eq!(ev["generation"], serde_json::Value::Null);
        // P0 stub metadata (Spec §2.2): all three P1 fields present but null until P1 fills them in.
        assert_eq!(ev["metadata"]["suppressed_signal"], serde_json::Value::Null);
        assert_eq!(ev["metadata"]["trigger_count"], serde_json::Value::Null);
        assert_eq!(ev["metadata"]["window_seconds"], serde_json::Value::Null);
    }

    // ── Multiple events in sequence ───────────────────────────────────────────

    #[tokio::test]
    async fn test_multiple_emit_calls_all_persisted() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        emitter.emit_skill_activate("a", "s1", "prediction_error_diagnosis");
        emitter.emit_skill_deactivate("a", "s2", "effectiveness_evaluation", serde_json::json!({}));
        emitter.emit_security_scan("a", "s3", true, serde_json::json!({}));
        emitter.emit_gvu_generation("a", Outcome::Success, "gvu_trigger", serde_json::json!({}));
        emitter.emit_signal_suppressed_stub("a", serde_json::json!({"suppressed_signal": null, "trigger_count": null, "window_seconds": null}));
        sleep(Duration::from_millis(100)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 5, "all 5 events must be persisted");

        let types: Vec<&str> = lines.iter()
            .map(|ev| ev["event_type"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"skill_activate"));
        assert!(types.contains(&"skill_deactivate"));
        assert!(types.contains(&"security_scan"));
        assert!(types.contains(&"gvu_generation"));
        assert!(types.contains(&"signal_suppressed"));
    }

    // ── Non-blocking: emit never panics even without a runtime ────────────────

    /// Verify emit functions return immediately (non-blocking contract).
    /// We call them in a single-threaded context but on a multi-thread runtime.
    #[tokio::test]
    async fn test_emit_is_non_blocking() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        // These calls must return instantly without awaiting.
        emitter.emit_skill_activate("x", "s", "t");
        emitter.emit_skill_deactivate("x", "s", "t", serde_json::json!({}));
        emitter.emit_security_scan("x", "s", true, serde_json::json!({}));
        emitter.emit_gvu_generation("x", Outcome::Success, "t", serde_json::json!({}));
        emitter.emit_signal_suppressed_stub("x", serde_json::json!({"suppressed_signal": null, "trigger_count": null, "window_seconds": null}));

        // All returned before this point — no blocking.
        sleep(Duration::from_millis(100)).await;
        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 5);
    }

    // ── Concurrent emit calls ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_concurrent_emit_no_corruption() {
        let tmp = TempDir::new().unwrap();
        let emitter = Arc::new(make_emitter(tmp.path()));
        const N: usize = 30;

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let e = Arc::clone(&emitter);
            handles.push(tokio::spawn(async move {
                e.emit_skill_activate(
                    &format!("agent-{i}"),
                    "concurrent-skill",
                    "prediction_error_diagnosis",
                );
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        sleep(Duration::from_millis(150)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), N, "all {N} concurrent events must be persisted");
        for line in &lines {
            // Every line must be valid JSON with correct event_type.
            assert_eq!(ev_type(line), "skill_activate");
        }
    }

    // ── Null fields in P0 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_generation_is_always_null_in_p0() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        let types_and_outcomes = [
            (AuditEventType::SkillActivate, Outcome::Success),
            (AuditEventType::SkillDeactivate, Outcome::Success),
            (AuditEventType::SecurityScan, Outcome::Success),
            (AuditEventType::GvuGeneration, Outcome::Success),
            (AuditEventType::SignalSuppressed, Outcome::Suppressed),
            (AuditEventType::SkillGraduate, Outcome::Success),
        ];

        for (t, o) in types_and_outcomes {
            match t {
                AuditEventType::SkillActivate =>
                    emitter.emit_skill_activate("a", "s", "t"),
                AuditEventType::SkillDeactivate =>
                    emitter.emit_skill_deactivate("a", "s", "t", serde_json::json!({})),
                AuditEventType::SecurityScan =>
                    emitter.emit_security_scan("a", "s", true, serde_json::json!({})),
                AuditEventType::GvuGeneration =>
                    emitter.emit_gvu_generation("a", o, "t", serde_json::json!({})),
                AuditEventType::SignalSuppressed =>
                    emitter.emit_signal_suppressed_stub("a", serde_json::json!({"suppressed_signal": null, "trigger_count": null, "window_seconds": null})),
                AuditEventType::SkillGraduate =>
                    emitter.emit_skill_graduate("a", "test-skill", serde_json::json!({"quality_score": 0.8, "source_trajectories": 1, "pipeline_version": "W19-P0"})),
                // W19-P1 variants — not tested here (generation=null is a P0 guarantee only)
                _ => {}
            }
        }
        sleep(Duration::from_millis(100)).await;

        let lines = read_lines(tmp.path()).await;
        assert_eq!(lines.len(), 6);
        for line in &lines {
            assert_eq!(
                line["generation"],
                serde_json::Value::Null,
                "generation must be null in P0 for event_type={}",
                ev_type(line)
            );
        }
    }

    // ── B3: cold-directory race + wait_pending synchronisation ────────────────

    /// Regression test for the "first `duduclaw evolution finalize` run
    /// prints an ERROR" bug: emitting into a directory that does not exist
    /// yet, then immediately (no `sleep` compensation) awaiting
    /// `wait_pending`, must leave the event durably written. Before the B3
    /// fix there was no way to synchronise on the detached `tokio::spawn`
    /// task at all, so a caller that didn't sleep an arbitrary amount of
    /// time could observe the write mid-flight — and in the real one-shot
    /// CLI, the process's Runtime could be dropped before the write ever
    /// completed.
    #[tokio::test]
    async fn test_wait_pending_flushes_cold_directory_emit() {
        let tmp = TempDir::new().unwrap();
        // Deliberately nested + not pre-created: this is the "first ever
        // run" shape, where `~/.duduclaw/evolution/events` doesn't exist.
        let cold_dir = tmp.path().join("brand-new-agent-home/evolution/events");
        assert!(!cold_dir.exists(), "precondition: directory must not exist yet");
        let emitter = make_emitter(&cold_dir);

        emitter.emit_skill_activate("agent-cold", "python-patterns", "prediction_error_diagnosis");
        // No `sleep()` here — `wait_pending` itself must be the
        // synchronisation point, not luck-based timing.
        emitter.wait_pending(Duration::from_secs(2)).await;

        let lines = read_lines(&cold_dir).await;
        assert_eq!(lines.len(), 1, "cold-directory emit must be durably written after wait_pending");
        assert_eq!(lines[0]["event_type"], "skill_activate");
        assert_eq!(lines[0]["agent_id"], "agent-cold");
    }

    /// Same as above but for multiple concurrent emits into a cold
    /// directory — simulates `ObservationFinalizer::tick()` firing one
    /// `emit_gvu_generation` per expired version before the one-shot CLI
    /// returns.
    #[tokio::test]
    async fn test_wait_pending_flushes_multiple_cold_directory_emits() {
        let tmp = TempDir::new().unwrap();
        let cold_dir = tmp.path().join("cold/evolution/events");
        let emitter = make_emitter(&cold_dir);

        for i in 0..5 {
            emitter.emit_gvu_generation(
                &format!("agent-{i}"),
                Outcome::Success,
                "gvu_trigger",
                serde_json::json!({"version_id": format!("v{i}")}),
            );
        }
        emitter.wait_pending(Duration::from_secs(2)).await;

        let lines = read_lines(&cold_dir).await;
        assert_eq!(
            lines.len(),
            5,
            "all emits into the cold directory must land before wait_pending returns"
        );
    }

    /// `wait_pending` on an emitter with nothing in flight must return
    /// immediately (fast path) rather than waiting out the timeout.
    #[tokio::test]
    async fn test_wait_pending_with_no_pending_tasks_returns_immediately() {
        let tmp = TempDir::new().unwrap();
        let emitter = make_emitter(tmp.path());

        let started = std::time::Instant::now();
        emitter.wait_pending(Duration::from_secs(5)).await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "wait_pending with nothing in flight must not block for anywhere near the timeout"
        );
    }

    /// `wait_pending_default` (the convenience one-shot-CLI-shutdown call)
    /// must behave like `wait_pending` with the crate's default timeout.
    #[tokio::test]
    async fn test_wait_pending_default_flushes_cold_directory_emit() {
        let tmp = TempDir::new().unwrap();
        let cold_dir = tmp.path().join("cold2/evolution/events");
        let emitter = make_emitter(&cold_dir);

        emitter.emit_skill_activate("agent-default", "s", "t");
        emitter.wait_pending_default().await;

        let lines = read_lines(&cold_dir).await;
        assert_eq!(lines.len(), 1);
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    fn ev_type(v: &serde_json::Value) -> &str {
        v["event_type"].as_str().unwrap_or("")
    }
}
