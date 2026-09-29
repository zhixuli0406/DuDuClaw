//! GVU loop orchestrator — the outer loop that drives one AEE round.
//!
//! **S11 (2026-09-29): the legacy SOUL.md rewrite cycle was removed.** What
//! this file used to hold — the Generator→Verifier→Updater patch cycle, the
//! `[evolution] legacy_soul_evolution` escape hatch, deferred-retry /
//! wall-clock-timeout bookkeeping and the SOUL cap-deadlock consolidation
//! branch — is gone together with the modules it drove. `SOUL.md` has been
//! read-only for agents since Evolution v3 (WP1.1), so the write path those
//! guards protected no longer exists.
//!
//! What remains is the machinery every evolution trigger shares — the
//! per-agent run lock, the WP0.3 cooldown gate and the alert sink — plus the
//! hand-off to [`crate::gvu::aee::run_aee_round`], which evolves the
//! playbook.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::mistake_notebook::MistakeEntry;
use super::text_gradient::TextGradient;
use super::trigger::agent_gvu_cooldown_minutes;
use super::version_store::VersionStore;

/// Outcome of a complete GVU loop execution.
#[derive(Debug, Clone)]
pub enum GvuOutcome {
    /// All generation attempts were rejected — permanently abandoned.
    Abandoned { last_gradient: TextGradient },
    /// Loop was skipped (e.g. cooldown active, opt-out, settlement window).
    Skipped { reason: String },
    /// The AEE path committed playbook deltas.
    PlaybookEvolved {
        /// Applied delta ops.
        applied: usize,
        /// Storage ids of the entries the round committed.
        entry_ids: Vec<String>,
        /// `improves` / `matches` — the commit-gate verdict (§2.4).
        verdict: String,
    },
}

/// Where evolution alerts and FYI-grade records go.
///
/// Moved here from the removed `gvu::consolidate` (S11, 2026-09-29): the
/// cap-deadlock breaker that used to own it is gone, but the AEE
/// escalate-to-human alert and the 「採用經驗法則」 Activity Feed row both still
/// need it.
pub struct GvuAlertSink {
    pub home_dir: PathBuf,
    pub prediction_engine: std::sync::Arc<crate::prediction::engine::PredictionEngine>,
}

impl GvuAlertSink {
    /// W3-2 — record an evolution outcome **without** paging anyone.
    ///
    /// [`Self::alert`] is for things a human should look at now, so it also
    /// pushes to the agent's channel. A committed set of experience rules is
    /// not that: D.14 classes 「試行結果通知(採用/回退)」 as L1 FYI — it belongs in
    /// the daily digest, not in a per-event push. Without this row, though, a
    /// rule adoption left no Activity Feed trace at all, so the digest's
    /// 「學習事件」 line could never see it (`notify_digest::LEARNING_PREFIXES`
    /// matches on `playbook_`, which until now had zero producers).
    ///
    /// Best-effort throughout, exactly like `alert`.
    pub async fn record_activity(&self, agent_id: &str, event_type: &str, summary: &str) {
        debug!(agent = %agent_id, event = event_type, "{summary}");

        self.prediction_engine.log_evolution_event(
            event_type,
            agent_id,
            None,
            None,
            Some(summary),
            None,
            None,
        );

        let store = match crate::task_store::TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "evolution activity: failed to open task store (non-fatal)");
                return;
            }
        };
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: summary.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(error = %e, "evolution activity: append failed (non-fatal)");
        }
    }

    /// Raise an evolution alert: `tracing::warn!` + evolution event +
    /// dashboard Activity Feed row. Mirrors
    /// [`super::stagnation::StagnationMonitor`]'s alerting so the WP0.5
    /// signals surface in the same place, and is best-effort throughout — a
    /// failure to alert must never abort the caller.
    pub async fn alert(&self, agent_id: &str, event_type: &str, summary: &str) {
        warn!(agent = %agent_id, event = event_type, "{summary}");

        self.prediction_engine.log_evolution_event(
            event_type,
            agent_id,
            None,
            None,
            Some(summary),
            None,
            None,
        );

        let store = match crate::task_store::TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "evolution alert: failed to open task store (non-fatal)");
                return;
            }
        };
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: summary.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(error = %e, "evolution alert: append failed (non-fatal)");
        }
    }
}

/// The GVU loop orchestrator.
pub struct GvuLoop {
    /// `<home>/evolution.db` — the AEE experiment log, champion snapshots and
    /// pending settlements all live here.
    db_path: PathBuf,
    /// Per-agent lock to prevent concurrent GVU loops.
    agent_locks: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
    /// Stored encryption key for creating consistent VersionStore instances.
    encryption_key: Option<[u8; 32]>,
    /// WP0.3 (2026-08-06, root cause R4): per-agent cooldown — `agent_id` →
    /// wall-clock instant the last GVU attempt was let through this gate.
    /// Enforced in [`Self::run_with_context`], the single entry point every
    /// caller funnels through, so channel-reply's ε-exploration/silence
    /// triggers and the dispatcher's forced-reflection path share one
    /// throttle instead of each needing its own check.
    /// In-memory only — resets on gateway restart, which is an accepted
    /// trade-off (see TODO-evolution-v3-2026-08.md WP0.3).
    cooldown_state: Arc<Mutex<std::collections::HashMap<String, Instant>>>,
    /// Where evolution alerts go. Optional so unit tests and non-gateway
    /// callers can build a loop without a dashboard; `None` degrades to
    /// `tracing::warn!` only. Wired in `server.rs` via
    /// [`Self::with_alert_sink`].
    alert_sink: Option<GvuAlertSink>,
}

impl GvuLoop {
    /// Create a new GVU loop over `<home>/evolution.db`.
    pub fn new(db_path: &Path) -> Self {
        Self::with_encryption(db_path, None)
    }

    /// Create with the home keyfile so every `VersionStore` this loop opens
    /// reads and writes encrypted columns consistently.
    pub fn with_encryption(db_path: &Path, encryption_key: Option<&[u8; 32]>) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            agent_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            encryption_key: encryption_key.copied(),
            cooldown_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            alert_sink: None,
        }
    }

    /// Attach the dashboard/evolution-event alert sink. Additive builder —
    /// existing construction sites are unaffected and simply get no alerts.
    pub fn with_alert_sink(mut self, sink: GvuAlertSink) -> Self {
        self.alert_sink = Some(sink);
        self
    }

    /// Best-effort alert. `None` sink → `tracing::warn!` only (see
    /// [`Self::with_alert_sink`]).
    async fn raise_alert(&self, agent_id: &str, event_type: &str, summary: &str) {
        match &self.alert_sink {
            Some(sink) => sink.alert(agent_id, event_type, summary).await,
            None => warn!(agent = %agent_id, event = event_type, "{summary}"),
        }
    }

    /// W3-2 — FYI-grade evolution record (Activity Feed + evolution event, no
    /// channel push). See `GvuAlertSink::record_activity` for why this is
    /// deliberately not [`Self::raise_alert`].
    async fn record_activity(&self, agent_id: &str, event_type: &str, summary: &str) {
        match &self.alert_sink {
            Some(sink) => sink.record_activity(agent_id, event_type, summary).await,
            None => debug!(agent = %agent_id, event = event_type, "{summary}"),
        }
    }

    /// Run one AEE round for an agent.
    ///
    /// `call_llm` is an async closure that calls the agent's utility model and
    /// returns the response text, so the loop stays LLM-backend agnostic (CLI,
    /// direct API or a mock).
    ///
    /// `relevant_mistakes` are pre-queried `MistakeNotebook` entries; they are
    /// the round's grounded failure evidence and also decide the typed AEE
    /// trigger.
    pub async fn run_with_context<F, Fut>(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        trigger_context: &str,
        must_not: &[String],
        must_always: &[String],
        call_llm: F,
        relevant_mistakes: Vec<MistakeEntry>,
    ) -> GvuOutcome
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        // Acquire per-agent lock
        let lock = {
            let mut locks = self.agent_locks.lock().await;
            locks
                .entry(agent_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = match lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                return GvuOutcome::Skipped {
                    reason: "GVU loop already running for this agent".to_string(),
                };
            }
        };

        let vs = VersionStore::with_crypto(&self.db_path, self.encryption_key.as_ref());

        // WP0.3 (2026-08-06, root cause R4): per-agent cooldown gate. Sits
        // before any LLM call so a caller that fires repeatedly (channel
        // reply's ε-exploration / silence-timer path had no throttle at all)
        // can't burn a fresh GVU cycle every time it's invoked. Recorded on
        // *entry* (not on completion) so even a run that fails downstream
        // still starts the clock — the cost we're guarding against is LLM
        // calls attempted, not just LLM calls that succeeded.
        let cooldown_minutes = agent_gvu_cooldown_minutes(agent_dir);
        if cooldown_minutes > 0 {
            let cooldown_dur = Duration::from_secs(cooldown_minutes * 60);
            let mut cooldowns = self.cooldown_state.lock().await;
            if let Some(last_started) = cooldowns.get(agent_id) {
                let elapsed = last_started.elapsed();
                if elapsed < cooldown_dur {
                    let remaining = cooldown_dur - elapsed;
                    debug!(
                        agent = agent_id,
                        elapsed_secs = elapsed.as_secs(),
                        cooldown_minutes,
                        remaining_secs = remaining.as_secs(),
                        "GVU cooldown active — trigger skipped"
                    );
                    return GvuOutcome::Skipped {
                        reason: format!(
                            "GVU cooldown active: last run started {}s ago (< {cooldown_minutes}m cooldown, {}s remaining)",
                            elapsed.as_secs(),
                            remaining.as_secs(),
                        ),
                    };
                }
            }
            cooldowns.insert(agent_id.to_string(), Instant::now());
        }

        // SOUL.md is read-only for the agent (WP1.1) and AEE never writes it,
        // but it is still the persona the playbook has to stay consistent
        // with, so the round is grounded in it. Missing file ⇒ empty persona,
        // not a skipped round: a blank SOUL.md is a legitimate agent shape now
        // that nothing rewrites it.
        let current_soul = std::fs::read_to_string(agent_dir.join("SOUL.md")).unwrap_or_default();

        let home_dir = self
            .db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| agent_dir.to_path_buf());
        let result = super::aee::run_aee_round(
            super::aee::AeeRoundInput {
                agent_id,
                agent_dir,
                home_dir,
                trigger_context,
                must_not,
                must_always,
                relevant_mistakes: &relevant_mistakes,
                current_soul: &current_soul,
                now: chrono::Utc::now(),
            },
            &vs,
            call_llm,
        )
        .await;

        // The round record is the audit trail §3.2.4 asks for: without the
        // intent recorded, a strategy-mix misconfiguration is invisible in
        // hindsight.
        debug!(
            agent = agent_id,
            record = %result.record.to_payload(),
            "AEE round finished"
        );

        // WP-6A / A2: persist the same record (now including a harness-knob
        // snapshot) to the durable evolution-events sink. Non-blocking, purely
        // observational — does not participate in the verdict below.
        {
            use crate::evolution_events::{
                emitter::EvolutionEventEmitter, schema::Outcome as EvtOutcome,
            };
            let evt_outcome = match &result.verdict {
                super::aee::AeeVerdict::Committed { .. } => EvtOutcome::Success,
                super::aee::AeeVerdict::NotCommitted { .. } => EvtOutcome::Failure,
                super::aee::AeeVerdict::Skipped { .. } => EvtOutcome::Suppressed,
            };
            EvolutionEventEmitter::global().emit_aee_round(&result.record, evt_outcome);
        }

        // An inner loop that gave up and asked for a human is an operator
        // signal, not a log line: it means the same wall was hit twice and no
        // amount of further rounds will help.
        if result.record.exit == "escalate_to_human" {
            if let super::aee::AeeVerdict::NotCommitted { ref reason, .. } = result.verdict {
                self.raise_alert(
                    agent_id,
                    "gvu_escalated",
                    &format!("AI 員工「{agent_id}」的自我進化卡住需人工介入：{reason}"),
                )
                .await;
            }
        }

        match result.verdict {
            super::aee::AeeVerdict::Committed {
                applied,
                entry_ids,
                verdict,
            } => {
                // W3-2: a committed round is the 「採用」 half of D.14's
                // 試行結果通知. Recorded (not pushed) so the daily digest can
                // count it — the user-facing wording is 經驗法則, never the
                // internal artifact name.
                self.record_activity(
                    agent_id,
                    "playbook_rules_updated",
                    &format!("AI 員工「{agent_id}」更新了 {applied} 條經驗法則"),
                )
                .await;
                GvuOutcome::PlaybookEvolved {
                    applied,
                    entry_ids,
                    verdict,
                }
            }
            super::aee::AeeVerdict::NotCommitted { gradient, .. } => GvuOutcome::Abandoned {
                last_gradient: gradient,
            },
            super::aee::AeeVerdict::Skipped { reason } => GvuOutcome::Skipped { reason },
        }
    }
}

/// WP0.3 (2026-08-06, root cause R4): per-agent GVU cooldown, enforced in
/// `run_with_context` — the single entry point every public caller funnels
/// through, so these tests exercise the gate the same way every real caller
/// (channel reply, dispatcher) does.
///
/// The load-bearing assertion in each case is **whether the LLM was reached
/// at all**: a cooldown-gated run must spend zero budget, so the counting
/// closure below must show no new call. The AEE round a non-gated run enters
/// is allowed to fail however it likes — the round's own outcome is
/// `gvu::aee`'s business, not this gate's.
#[cfg(test)]
mod cooldown_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn agent_dir_with_cooldown(minutes: Option<u64>) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = match minutes {
            Some(m) => format!("[evolution]\ngvu_cooldown_minutes = {m}\n"),
            None => String::new(),
        };
        std::fs::write(dir.path().join("agent.toml"), toml).unwrap();
        dir
    }

    /// Counts how many times the loop reached the model, and always fails the
    /// call so no round can commit anything.
    fn counting_llm(
        counter: Arc<AtomicUsize>,
    ) -> impl Fn(String) -> std::future::Ready<Result<String, String>> {
        move |_prompt: String| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err::<String, String>("no creds (test)".to_string()))
        }
    }

    #[tokio::test]
    async fn cooldown_blocks_second_call_within_window() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"));
        let agent_dir = agent_dir_with_cooldown(Some(60));
        let calls = Arc::new(AtomicUsize::new(0));

        let first = gvu
            .run_with_context(
                "agent-cd",
                agent_dir.path(),
                "ctx",
                &[],
                &[],
                counting_llm(calls.clone()),
                Vec::new(),
            )
            .await;
        assert!(
            !matches!(&first, GvuOutcome::Skipped { reason } if reason.contains("cooldown")),
            "first call must not be gated by the cooldown: {first:?}"
        );
        let after_first = calls.load(Ordering::SeqCst);

        // Second call, same agent, immediately after — must be blocked by the
        // cooldown gate before the AEE round is even attempted, so the LLM
        // call count must not move.
        let second = gvu
            .run_with_context(
                "agent-cd",
                agent_dir.path(),
                "ctx",
                &[],
                &[],
                counting_llm(calls.clone()),
                Vec::new(),
            )
            .await;
        match second {
            GvuOutcome::Skipped { reason } => assert!(
                reason.contains("cooldown"),
                "second call within the cooldown window must be skipped for cooldown, got: {reason}"
            ),
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_first,
            "a cooldown-gated run must spend zero LLM budget"
        );
    }

    #[tokio::test]
    async fn cooldown_zero_disables_throttling() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"));
        let agent_dir = agent_dir_with_cooldown(Some(0));
        let calls = Arc::new(AtomicUsize::new(0));

        for attempt in 0..2 {
            let outcome = gvu
                .run_with_context(
                    "agent-nocd",
                    agent_dir.path(),
                    "ctx",
                    &[],
                    &[],
                    counting_llm(calls.clone()),
                    Vec::new(),
                )
                .await;
            assert!(
                !matches!(&outcome, GvuOutcome::Skipped { reason } if reason.contains("cooldown")),
                "attempt {attempt}: gvu_cooldown_minutes=0 must never skip for cooldown, got: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn cooldown_is_per_agent_not_global() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"));
        let agent_a = agent_dir_with_cooldown(Some(60));
        let agent_b = agent_dir_with_cooldown(Some(60));
        let calls = Arc::new(AtomicUsize::new(0));

        let _ = gvu
            .run_with_context(
                "agent-a",
                agent_a.path(),
                "ctx",
                &[],
                &[],
                counting_llm(calls.clone()),
                Vec::new(),
            )
            .await;

        // A different agent (different id AND different agent_dir) must not
        // be throttled by agent-a's cooldown timestamp.
        let outcome_b = gvu
            .run_with_context(
                "agent-b",
                agent_b.path(),
                "ctx",
                &[],
                &[],
                counting_llm(calls.clone()),
                Vec::new(),
            )
            .await;
        assert!(
            !matches!(&outcome_b, GvuOutcome::Skipped { reason } if reason.contains("cooldown")),
            "agent-b must not inherit agent-a's cooldown: {outcome_b:?}"
        );
    }

    /// Regression (S11, 2026-09-29): the `[evolution] legacy_soul_evolution`
    /// escape hatch is gone — an agent that still carries the key in its
    /// `agent.toml` must take the AEE path like everyone else, and `SOUL.md`
    /// must come back byte-identical (there is no longer any code that could
    /// rewrite it).
    #[tokio::test]
    async fn legacy_soul_evolution_key_no_longer_rewrites_soul() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"));
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[evolution]\ngvu_enabled = true\ngvu_cooldown_minutes = 0\nlegacy_soul_evolution = true\n",
        )
        .unwrap();
        let soul = "# persona\n\n## \u{884C}\u{70BA}\n- be kind\n";
        std::fs::write(dir.path().join("SOUL.md"), soul).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));

        let outcome = gvu
            .run_with_context(
                "agent-legacy",
                dir.path(),
                "ctx",
                &[],
                &[],
                counting_llm(calls.clone()),
                Vec::new(),
            )
            .await;

        // Whatever the round decided, it cannot have been a SOUL rewrite —
        // there is no outcome variant for one any more, and the file itself
        // is the proof.
        let _ = outcome;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("SOUL.md")).unwrap(),
            soul,
            "SOUL.md must be byte-identical after a round, legacy key or not"
        );
    }
}
