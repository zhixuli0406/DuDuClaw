//! GVU Loop orchestrator — the convergent Generator→Verifier→Updater cycle.
//!
//! Runs up to `max_generations` rounds. Each round:
//! 1. Generator produces a proposal (with TextGradient feedback if retrying)
//! 2. Verifier evaluates through 4 layers
//! 3. If approved → Updater applies with observation period
//! 4. If rejected → TextGradient fed back to Generator for next round

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use duduclaw_core::truncate_bytes;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::generator::{Generator, GeneratorInput};
use super::mistake_notebook::MistakeEntry;
use super::proposal::{EvolutionProposal, ProposalStatus, ProposalType};
use super::text_gradient::TextGradient;
use super::trigger::agent_gvu_cooldown_minutes;
use super::updater::Updater;
use super::verifier::{self, VerificationResult};
use super::version_store::{ExperimentLogEntry, SoulVersion, VersionMetrics, VersionStore};

use crate::prediction::metacognition::MetaCognition;

/// Maximum number of generation attempts per GVU cycle.
/// Adaptive depth: up to 7 rounds for thorough evolution.
const DEFAULT_MAX_GENERATIONS: u32 = 7;

/// Free / CE baseline generation budget.
///
/// M1 moat-gate (features.toml `industry_evolution_params`): tiers **without**
/// the flag (OpenSource / Hobby / Solo / Studio / self-host base) run a fixed
/// 3-round GVU cycle — the historical free behaviour. Only Business /
/// SelfHostPro / OEM unlock the full adaptive 3–7 depth.
const FREE_MAX_GENERATIONS: u32 = 3;

/// Apply the M1 moat-gate to a resolved generation budget.
///
/// `base` is the pre-gate budget (MetaCognition's adaptive 3–7, or the fixed
/// `max_generations`). When `industry_evolution` is `false` the budget is
/// capped at [`FREE_MAX_GENERATIONS`]; when `true` the full adaptive depth
/// passes through. Non-breaking by construction: the default resolution path
/// (no license runtime published → `industry_evolution = false`) yields the
/// free 3-round cap, matching prior free-tier behaviour.
pub(crate) fn gated_max_generations(base: u32, industry_evolution: bool) -> u32 {
    if industry_evolution {
        base
    } else {
        base.min(FREE_MAX_GENERATIONS)
    }
}

/// WP2.4 / B10 — the legacy SOUL path's explicit judge floor.
///
/// The 0.7 threshold used to live in **two** places inside `verifier.rs`
/// (`parse_judge_response` AND `verify_all`), where it acted as a silent veto:
/// a quality heuristic nobody ever calibrated could destroy a candidate
/// outright. Both copies are gone — the judge is now a Measure dimension and
/// the accept/reject call belongs to the champion comparison
/// (`verifier_measure::commit_verdict`).
///
/// This constant is what remains, and it is deliberately *here* rather than
/// inside the verifier: the AEE playbook path does not use it at all, while
/// the legacy SOUL path — which has no champion to compare against and, until
/// the eval bridge lands, no `cases` dimension either — would otherwise lose
/// its only quality signal. Keeping the floor visible at the call site makes
/// that trade-off legible instead of buried, and makes it a one-line deletion
/// once the SOUL write path is fully retired.
pub(crate) const LEGACY_JUDGE_FLOOR: f64 = 0.7;

/// WP2.6 — is this agent pinned to the legacy SOUL.md evolution path?
///
/// AEE is the **default** evolution path (D1). `agent.toml [evolution]
/// legacy_soul_evolution = true` is the escape hatch that routes the agent
/// back through the historical Generator→Verifier→Updater SOUL patch cycle,
/// byte-for-byte unchanged including [`LEGACY_JUDGE_FLOOR`].
///
/// Fail-safe posture, chosen deliberately in the *opposite* direction from
/// the other `[evolution]` readers in this file: a missing / malformed
/// `agent.toml` yields `false` (AEE), because AEE is the path that cannot
/// write SOUL.md at all. Falling back to the legacy writer on an unreadable
/// config would mean a config typo silently re-enables the write surface
/// WP1.1 closed.
pub fn agent_legacy_soul_evolution(agent_dir: &Path) -> bool {
    duduclaw_core::agent_toml::load(agent_dir)
        .evolution
        .legacy_soul_evolution
        .unwrap_or(false)
}

/// Default wall-clock timeout per GVU cycle (5 minutes).
///
/// Inspired by autoresearch's fixed time budget: each evolution attempt
/// must complete within a bounded duration. This prevents runaway loops
/// (e.g., slow LLM responses) from blocking the agent indefinitely.
const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(5 * 60);

/// Outcome of a complete GVU loop execution.
#[derive(Debug, Clone)]
pub enum GvuOutcome {
    /// A proposal was approved and applied — observation period started.
    Applied(SoulVersion),
    /// All generation attempts were rejected — permanently abandoned.
    Abandoned { last_gradient: TextGradient },
    /// Loop was skipped (e.g., active observation already in progress).
    Skipped { reason: String },
    /// All attempts rejected but gradients accumulated for deferred retry.
    ///
    /// The accumulated gradients will be stored in the VersionStore and
    /// retried after `retry_after` duration (typically 24h). Max 2 deferrals
    /// (= 3 rounds × 3 deferrals = 9 effective attempts spread over 72h).
    Deferred {
        accumulated_gradients: Vec<TextGradient>,
        retry_after_hours: f64,
        retry_count: u32,
    },
    /// Wall-clock timeout exceeded before completing all generations.
    ///
    /// Accumulated gradients are preserved for deferred retry (same as
    /// generation exhaustion). The `elapsed` field records actual duration.
    TimedOut {
        elapsed: Duration,
        generations_completed: u32,
        accumulated_gradients: Vec<TextGradient>,
    },
    /// The AEE path committed playbook deltas.
    ///
    /// Deliberately NOT folded into [`Self::Applied`]: that variant carries a
    /// `SoulVersion`, and an AEE round writes no SOUL.md version at all —
    /// synthesizing one would put a fictitious row in `soul_versions` and
    /// open an observation window on a document that never changed. AEE's own
    /// observation window lives in `aee_pending_settlement` and is closed by
    /// [`crate::gvu::observation_finalizer::ObservationFinalizer`].
    PlaybookEvolved {
        /// Applied delta ops.
        applied: usize,
        /// Storage ids of the entries the round committed.
        entry_ids: Vec<String>,
        /// `improves` / `matches` — the commit-gate verdict (§2.4).
        verdict: String,
    },
}

/// The GVU loop orchestrator.
pub struct GvuLoop {
    generator: Generator,
    updater: Updater,
    max_generations: u32,
    /// Wall-clock timeout for the entire GVU cycle.
    ///
    /// Borrowed from autoresearch's fixed time-budget design: each evolution
    /// attempt is bounded, ensuring fair comparison and preventing runaway loops.
    max_duration: Duration,
    /// Per-agent lock to prevent concurrent GVU loops.
    agent_locks: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
    /// Stored encryption key for creating consistent VersionStore instances.
    encryption_key: Option<[u8; 32]>,
    /// WP0.3 (2026-08-06, root cause R4): per-agent cooldown — `agent_id` →
    /// wall-clock instant the last GVU attempt was let through this gate.
    /// Enforced in [`Self::run_with_context_and_retry`], the single entry
    /// point every public `run*` method funnels through, so channel-reply's
    /// ε-exploration/silence triggers and the dispatcher's forced-reflection
    /// path share one throttle instead of each needing its own check.
    /// In-memory only — resets on gateway restart, which is an accepted
    /// trade-off (see TODO-evolution-v3-2026-08.md WP0.3).
    cooldown_state: Arc<Mutex<std::collections::HashMap<String, Instant>>>,
    /// WP0.2 (2026-08-06, root cause R2): where consolidate-mode alerts go.
    /// Optional so unit tests and non-gateway callers can build a loop without
    /// a dashboard; `None` degrades to `tracing::warn!` only. Wired in
    /// `server.rs` via [`Self::with_alert_sink`].
    alert_sink: Option<super::consolidate::GvuAlertSink>,
}

impl GvuLoop {
    /// Create a new GVU loop with shared VersionStore.
    ///
    /// If `encryption_key` is provided, rollback_diff is encrypted at rest.
    pub fn new(
        db_path: &Path,
        observation_hours: Option<f64>,
        max_generations: Option<u32>,
    ) -> Self {
        Self::with_encryption(db_path, observation_hours, max_generations, None)
    }

    /// Create with optional AES-256-GCM encryption for rollback_diff.
    pub fn with_encryption(
        db_path: &Path,
        observation_hours: Option<f64>,
        max_generations: Option<u32>,
        encryption_key: Option<&[u8; 32]>,
    ) -> Self {
        Self::with_options(
            db_path,
            observation_hours,
            max_generations,
            encryption_key,
            None,
        )
    }

    /// Create with all options including wall-clock timeout.
    pub fn with_options(
        db_path: &Path,
        observation_hours: Option<f64>,
        max_generations: Option<u32>,
        encryption_key: Option<&[u8; 32]>,
        max_duration: Option<Duration>,
    ) -> Self {
        let version_store_gen = VersionStore::with_crypto(db_path, encryption_key);
        let version_store_upd = VersionStore::with_crypto(db_path, encryption_key);

        Self {
            generator: Generator::new(version_store_gen),
            updater: Updater::new(version_store_upd, observation_hours),
            max_generations: max_generations.unwrap_or(DEFAULT_MAX_GENERATIONS),
            max_duration: max_duration.unwrap_or(DEFAULT_MAX_DURATION),
            agent_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            encryption_key: encryption_key.copied(),
            cooldown_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            alert_sink: None,
        }
    }

    /// WP0.2: attach the dashboard/evolution-event alert sink used when a
    /// SOUL.md cap deadlock cannot be cleared automatically. Additive builder —
    /// existing construction sites are unaffected and simply get no alerts.
    pub fn with_alert_sink(mut self, sink: super::consolidate::GvuAlertSink) -> Self {
        self.alert_sink = Some(sink);
        self
    }

    /// Access the updater (for observation checking from heartbeat).
    pub fn updater(&self) -> &Updater {
        &self.updater
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

    /// Run the full GVU loop for an agent.
    ///
    /// `call_llm` is an async closure that calls Claude and returns the response text.
    /// This allows the loop to be LLM-backend agnostic (can use CLI, API, or mock).
    ///
    /// Optional parameters (Phase 1 GVU²):
    /// - `metacognition`: If provided, uses adaptive depth instead of fixed `max_generations`.
    /// - `relevant_mistakes`: Pre-queried MistakeNotebook entries for grounded generation.
    pub async fn run<F, Fut>(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        trigger_context: &str,
        pre_metrics: VersionMetrics,
        must_not: &[String],
        must_always: &[String],
        call_llm: F,
    ) -> GvuOutcome
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        self.run_with_context_and_retry(
            agent_id,
            agent_dir,
            trigger_context,
            pre_metrics,
            must_not,
            must_always,
            call_llm,
            None,
            Vec::new(),
            0,
        )
        .await
    }

    /// Run with optional metacognition and mistake notebook context.
    ///
    /// `deferred_retry_count`: How many times this agent has already been deferred.
    /// Callers should pass 0 for fresh runs, or the retry_count from a `DeferredGvu`
    /// entry when retrying. This is NOT queried from the DB (see review issue #16).
    pub async fn run_with_context<F, Fut>(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        trigger_context: &str,
        pre_metrics: VersionMetrics,
        must_not: &[String],
        must_always: &[String],
        call_llm: F,
        metacognition: Option<&MetaCognition>,
        relevant_mistakes: Vec<MistakeEntry>,
    ) -> GvuOutcome
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        self.run_with_context_and_retry(
            agent_id,
            agent_dir,
            trigger_context,
            pre_metrics,
            must_not,
            must_always,
            call_llm,
            metacognition,
            relevant_mistakes,
            0,
        )
        .await
    }

    /// Full run with explicit deferred retry count tracking.
    pub async fn run_with_context_and_retry<F, Fut>(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        trigger_context: &str,
        pre_metrics: VersionMetrics,
        must_not: &[String],
        must_always: &[String],
        call_llm: F,
        metacognition: Option<&MetaCognition>,
        relevant_mistakes: Vec<MistakeEntry>,
        deferred_retry_count: u32,
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

        // Check for active observation period
        let vs = VersionStore::with_crypto(
            self.updater.version_store().db_path_ref(),
            self.encryption_key.as_ref(),
        );
        if let Some(observing) = vs.get_observing_version(agent_id) {
            return GvuOutcome::Skipped {
                reason: format!(
                    "Active observation period until {}",
                    observing.observation_end.to_rfc3339()
                ),
            };
        }

        // WP0.3 (2026-08-06, root cause R4): per-agent cooldown gate. Sits
        // before any LLM call so a caller that fires repeatedly (channel
        // reply's ε-exploration / silence-timer path had no throttle at all)
        // can't burn a fresh GVU cycle every time it's invoked. Recorded on
        // *entry* (not on completion) so even a run that fails downstream
        // (missing SOUL.md, generator error, …) still starts the clock —
        // the cost we're guarding against is LLM calls attempted, not just
        // LLM calls that succeeded.
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

        // Read current SOUL.md
        let soul_path = agent_dir.join("SOUL.md");
        let current_soul = match std::fs::read_to_string(&soul_path) {
            Ok(s) => s,
            Err(e) => {
                warn!(agent = agent_id, "Cannot read SOUL.md: {e}");
                return GvuOutcome::Skipped {
                    reason: format!("Cannot read SOUL.md: {e}"),
                };
            }
        };

        // WP0.2 (2026-08-06, root cause R2): cap-deadlock breaker.
        //
        // Every write path in `Updater::apply` is additive, so once SOUL.md is
        // over a hard cap NO ordinary proposal can ever be applied again — the
        // apply gate rejects with "Manual review required" into a log line and
        // the agent is silently frozen forever. The B-window sample
        // (ceo-assistant, 9,552 bytes against an 8,192-byte cap) burned 20 full
        // GVU cycles that way without changing one line of SOUL.md.
        //
        // Detected here, before the generation loop, because generating an
        // ordinary patch first would be pure waste: it is guaranteed to die at
        // the apply gate. Consolidation replaces the whole cycle for this run;
        // ordinary evolution resumes on the next trigger, now with headroom.
        if let Some(breach) = super::consolidate::detect_cap_breach(&current_soul) {
            return self
                .run_consolidation(
                    agent_id,
                    agent_dir,
                    &current_soul,
                    &breach,
                    trigger_context,
                    pre_metrics,
                    must_not,
                    must_always,
                    &relevant_mistakes,
                    &vs,
                    &call_llm,
                )
                .await;
        }

        // ── WP2.6: mode branch ────────────────────────────────────────────
        //
        // Everything above this line is shared by both paths on purpose:
        // the per-agent lock, the observation check, the WP0.3 cooldown, and
        // the SOUL cap-deadlock breaker apply whichever engine is driving.
        // (Cap consolidation in particular MUST stay shared — an over-cap
        // SOUL.md freezes the agent's prompt regardless of where its
        // evolution artefacts are being written.)
        //
        // Below it, the two paths diverge completely: AEE evolves the
        // playbook and never touches SOUL.md; the legacy path is the
        // historical SOUL patch cycle, unchanged.
        if !agent_legacy_soul_evolution(agent_dir) {
            let home_dir = vs
                .db_path_ref()
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
            // snapshot) to the durable evolution-events sink — previously the
            // `AeeRound` schema variant was declared but never emitted, so this
            // record was only ever visible via `debug!` above. Non-blocking,
            // purely observational — does not participate in the verdict below.
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
            // signal, not a log line: it means the same wall was hit twice and
            // no amount of further rounds will help. Routed through the same
            // sink as the consolidate alerts (Activity Feed + evolution event +
            // the agent's own channel).
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

            return match result.verdict {
                super::aee::AeeVerdict::Committed { applied, entry_ids, verdict } => {
                    // W3-2: a committed round is the 「採用」 half of D.14's
                    // 試行結果通知. Recorded (not pushed) so the daily digest
                    // can count it — the user-facing wording is 經驗法則,
                    // never the internal artifact name.
                    self.record_activity(
                        agent_id,
                        "playbook_rules_updated",
                        &format!("AI 員工「{agent_id}」更新了 {applied} 條經驗法則"),
                    )
                    .await;
                    GvuOutcome::PlaybookEvolved { applied, entry_ids, verdict }
                }
                super::aee::AeeVerdict::NotCommitted { gradient, .. } => {
                    GvuOutcome::Abandoned { last_gradient: gradient }
                }
                super::aee::AeeVerdict::Skipped { reason } => GvuOutcome::Skipped { reason },
            };
        }

        let mut proposal = EvolutionProposal::new(
            agent_id.to_string(),
            ProposalType::SoulPatch,
            trigger_context.to_string(),
        );

        let mut gradients: Vec<TextGradient> = Vec::new();
        let mut last_gradient: Option<TextGradient> = None;

        // Adaptive depth: use MetaCognition if available, else fixed max_generations
        let base_max = metacognition
            .map(|mc| mc.adaptive_max_generations())
            .unwrap_or(self.max_generations);

        // M1 moat-gate: `industry_evolution_params` (Business / SelfHostPro / OEM)
        // unlocks the full adaptive depth; free / CE / self-host base tiers are
        // capped at the 3-round free baseline. Resolved live from the
        // process-global license runtime so a dashboard tier upgrade takes effect
        // without a restart. Absent runtime (unit tests, non-gateway subcommands)
        // → free baseline — the non-breaking default.
        let industry_evolution = match crate::license_runtime::global() {
            Some(rt) => rt.check_feature("industry_evolution_params").await,
            None => false,
        };
        let effective_max = gated_max_generations(base_max, industry_evolution);

        // Wall-clock budget — the entire GVU cycle must complete within this duration.
        let deadline = Instant::now();
        let mut generations_completed: u32 = 0;

        for attempt in 1..=effective_max {
            // Check wall-clock budget before starting a new generation
            let elapsed = deadline.elapsed();
            if elapsed >= self.max_duration {
                warn!(
                    agent = agent_id,
                    elapsed_secs = elapsed.as_secs(),
                    budget_secs = self.max_duration.as_secs(),
                    generations_completed,
                    "GVU loop timed out — wall-clock budget exceeded"
                );

                // Log the timed-out experiment
                vs.record_experiment(&ExperimentLogEntry::new(
                    agent_id,
                    generations_completed,
                    effective_max,
                    elapsed,
                    "timed_out",
                    &format!("Wall-clock timeout after {generations_completed}/{effective_max} generations"),
                ));

                // Store deferred for later retry (same logic as generation exhaustion)
                if deferred_retry_count < 2 {
                    let retry_after_hours = 24.0;
                    let next_retry = deferred_retry_count + 1;
                    if let Err(e) =
                        vs.store_deferred(agent_id, &gradients, retry_after_hours, next_retry)
                    {
                        warn!(
                            agent = agent_id,
                            "Failed to store deferred GVU after timeout: {e}"
                        );
                    }
                }

                return GvuOutcome::TimedOut {
                    elapsed,
                    generations_completed,
                    accumulated_gradients: gradients,
                };
            }

            info!(
                agent = agent_id,
                generation = attempt,
                "GVU loop generation {attempt}"
            );

            // 1. Generate
            // Load wiki index if the agent has a wiki directory
            let wiki_index = {
                let wiki_index_path = agent_dir.join("wiki").join("_index.md");
                std::fs::read_to_string(&wiki_index_path).ok()
            };

            let input = GeneratorInput {
                agent_id: agent_id.to_string(),
                agent_soul: current_soul.clone(),
                trigger_context: trigger_context.to_string(),
                previous_gradients: gradients.clone(),
                generation: attempt,
                relevant_mistakes: relevant_mistakes.clone(),
                wiki_index,
                must_always: must_always.to_vec(),
                must_not: must_not.to_vec(),
            };

            let prompt = self.generator.generate(&input, &mut proposal);

            // Call LLM for generation
            let llm_response = match call_llm(prompt).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(agent = agent_id, "Generator LLM call failed: {e}");
                    generations_completed = attempt;
                    continue;
                }
            };

            let output = Generator::parse_response(&llm_response);
            self.generator.apply_output(&mut proposal, &output);

            // Verify wiki proposals (L1b) if present — strip invalid ones
            let verified_wiki_proposals: Vec<duduclaw_memory::wiki::WikiProposal> =
                if !output.wiki_proposals.is_empty() {
                    match verifier::verify_wiki_proposals(&output.wiki_proposals) {
                        Ok(()) => output.wiki_proposals.clone(),
                        Err(gradient) => {
                            warn!(
                                agent = agent_id,
                                generation = attempt,
                                critique = %gradient.critique,
                                "Wiki proposals stripped due to validation failure"
                            );
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };

            // 2. Verify — B2 (§5.5 執行期裁定, 2026-08-06): deterministic
            //    layers run FIRST, the paid L3 LLM judge only after they all
            //    pass.
            //
            //    The pre-B2 order built the judge prompt and burned an LLM
            //    call before `verify_all_with_mistakes` ever ran, so a
            //    proposal that L1 was always going to reject on a pure string
            //    check still cost a full judge round-trip. The B-window
            //    forensic sample (ceo-assistant, 2026-08) shows 6 all-rejected
            //    cycles × 4–5 minutes each where that judge spend was
            //    entirely wasted.
            //
            //    Implementation note: this is a pure ORDER change, not a
            //    logic change. The pre-gate is the SAME
            //    `verify_all_with_mistakes` call with `judge_result = None`,
            //    which is the already-supported "no judge available" mode
            //    (L3 is skipped, confidence defaults to 0.75). Every
            //    deterministic layer — L-Safety, L1, L2, L2.5, L3.5
            //    anti-sycophancy, L-Canary, L4 — therefore evaluates exactly
            //    as before, just earlier. If the pre-gate approves we make
            //    the judge call and re-run the full stack WITH the judge
            //    result, so the final verdict is byte-identical to the old
            //    ordering. Re-running the deterministic layers is free (no
            //    LLM, read-only SQLite) and keeps each layer's decision logic
            //    untouched.
            //
            //    Telemetry follows the order: a deterministic rejection is
            //    recorded once, by the pre-gate, and no `L3-LLMJudge` row is
            //    written because the judge never ran.
            let pre_gate = verifier::verify_all_with_mistakes(
                &proposal,
                &current_soul,
                must_not,
                must_always,
                &vs,
                None, // deterministic layers only — no judge spend yet
                &relevant_mistakes,
            );

            let result = match pre_gate {
                VerificationResult::Rejected { gradient } => {
                    debug!(
                        agent = agent_id,
                        generation = attempt,
                        layer = %gradient.source_layer,
                        "Deterministic gate rejected before judge — L3 LLM call skipped (B2)"
                    );
                    VerificationResult::Rejected { gradient }
                }
                VerificationResult::Approved { .. } => {
                    // Deterministic layers all clear — now (and only now) pay
                    // for the L3 judge.
                    let judge_prompt = verifier::build_judge_prompt(
                        &proposal,
                        &current_soul,
                        must_not,
                        must_always,
                    );
                    let judge_result = match call_llm(judge_prompt).await {
                        Ok(r) => Some(verifier::parse_judge_response(&r)),
                        Err(e) => {
                            warn!(
                                agent = agent_id,
                                "Judge LLM call failed: {e}, proceeding without L3"
                            );
                            None
                        }
                    };
                    let verdict = verifier::verify_all_with_mistakes(
                        &proposal,
                        &current_soul,
                        must_not,
                        must_always,
                        &vs,
                        judge_result.as_ref(),
                        &relevant_mistakes,
                    );

                    // WP2.4 / B10: the judge no longer vetoes inside the
                    // verifier. On this legacy SOUL path there is no champion
                    // to compare a judge score against, so the historical
                    // absolute floor is re-applied here — explicitly, at the
                    // call site, with its own constant — rather than left
                    // implicit two layers down. The AEE playbook path skips
                    // this entirely and uses the champion comparison instead.
                    match (&verdict, judge_result.as_ref()) {
                        (
                            VerificationResult::Approved { .. },
                            Some(j),
                        ) if !j.approved || j.score < LEGACY_JUDGE_FLOOR => {
                            let gradient = TextGradient::blocking(
                                "L3-LLMJudge",
                                "proposal",
                                &format!("LLM Judge rejected (score: {:.2})", j.score),
                                &j.feedback,
                            );
                            super::telemetry::record_rejection_from_store(
                                &vs,
                                agent_id,
                                "verify",
                                &gradient.source_layer,
                                &gradient.critique,
                                &proposal.content,
                                proposal.generation,
                            );
                            VerificationResult::Rejected { gradient }
                        }
                        _ => verdict,
                    }
                }
            };

            generations_completed = attempt;

            match result {
                VerificationResult::Approved {
                    confidence,
                    advisories,
                } => {
                    info!(
                        agent = agent_id,
                        generation = attempt,
                        confidence = format!("{confidence:.2}"),
                        advisories = advisories.len(),
                        "Proposal approved"
                    );
                    proposal.status = ProposalStatus::Approved;

                    // WP0.2, second face of R2: the proposal is fine, but
                    // applying it would push SOUL.md past a hard cap. Before
                    // this, that produced yet another "Manual review required"
                    // log line and the agent was one cycle away from the
                    // permanent deadlock. Project the write with the SAME
                    // function `apply` uses, and if it breaches, spend the
                    // cycle making room instead.
                    //
                    // Guarded on there being room to reclaim: if SOUL.md is
                    // already at or under the consolidation target, the excess
                    // comes from the proposal itself, not from accumulated
                    // bloat, and compressing the file cannot fix it — that case
                    // falls through to the ordinary rejection, which now also
                    // raises an alert (see the `Err` arm below).
                    if let Ok((projected, _)) =
                        super::updater::project_applied_soul(&current_soul, &proposal)
                    {
                        if super::consolidate::is_over_cap(&projected)
                            && current_soul.len() > super::consolidate::target_bytes()
                        {
                            let breach = super::consolidate::CapBreach::projected(
                                &current_soul,
                                &projected,
                            );
                            info!(
                                agent = agent_id,
                                projected_lines = projected.lines().count(),
                                projected_bytes = projected.len(),
                                "Approved proposal would breach the SOUL.md cap — \
                                 consolidating first to make room"
                            );
                            return self
                                .run_consolidation(
                                    agent_id,
                                    agent_dir,
                                    &current_soul,
                                    &breach,
                                    trigger_context,
                                    pre_metrics,
                                    must_not,
                                    must_always,
                                    &relevant_mistakes,
                                    &vs,
                                    &call_llm,
                                )
                                .await;
                        }
                    }

                    // 3. Apply SOUL.md
                    match self.updater.apply(&proposal, agent_dir, pre_metrics) {
                        Ok(version) => {
                            // 3b. Apply verified wiki proposals (non-blocking)
                            if !verified_wiki_proposals.is_empty() {
                                let wiki_dir = agent_dir.join("wiki");
                                let wiki_store = duduclaw_memory::WikiStore::new(wiki_dir);
                                if let Err(e) = wiki_store.ensure_scaffold() {
                                    warn!(agent = agent_id, "Failed to create wiki scaffold: {e}");
                                } else {
                                    match wiki_store.apply_proposals(&verified_wiki_proposals) {
                                        Ok(count) => {
                                            info!(
                                                agent = agent_id,
                                                applied = count,
                                                total = verified_wiki_proposals.len(),
                                                "Wiki proposals applied alongside SOUL.md update"
                                            );
                                        }
                                        Err(e) => {
                                            warn!(
                                                agent = agent_id,
                                                "Failed to apply wiki proposals: {e}"
                                            );
                                        }
                                    }
                                }
                            }

                            proposal.status = ProposalStatus::Observing;
                            let elapsed = deadline.elapsed();

                            // Log the successful experiment
                            vs.record_experiment(&ExperimentLogEntry::new(
                                agent_id,
                                attempt,
                                effective_max,
                                elapsed,
                                "applied",
                                &format!(
                                    "Approved at generation {attempt} (confidence: {confidence:.2})"
                                ),
                            ));

                            return GvuOutcome::Applied(version);
                        }
                        Err(e) => {
                            warn!(agent = agent_id, "Failed to apply proposal: {e}");

                            // WP0.2: a cap rejection that consolidation cannot
                            // fix (SOUL.md already at/under the consolidation
                            // target — the excess is the proposal's own bulk)
                            // used to end here as a log line only. It is a
                            // frozen agent either way, so it gets an operator
                            // alert.
                            if let Ok((projected, _)) =
                                super::updater::project_applied_soul(&current_soul, &proposal)
                            {
                                if super::consolidate::is_over_cap(&projected) {
                                    let breach = super::consolidate::CapBreach::projected(
                                        &current_soul,
                                        &projected,
                                    );
                                    self.raise_alert(
                                        agent_id,
                                        "gvu_cap_blocked",
                                        &breach.summary_zh(agent_id),
                                    )
                                    .await;
                                }
                            }

                            let elapsed = deadline.elapsed();
                            vs.record_experiment(&ExperimentLogEntry::new(
                                agent_id,
                                attempt,
                                effective_max,
                                elapsed,
                                "skipped",
                                &format!("Apply failed: {e}"),
                            ));
                            return GvuOutcome::Skipped { reason: e };
                        }
                    }
                }
                VerificationResult::Rejected { gradient } => {
                    info!(
                        agent = agent_id,
                        generation = attempt,
                        layer = %gradient.source_layer,
                        "Proposal rejected: {}",
                        gradient.critique,
                    );
                    gradients.push(gradient.clone());
                    proposal.status = ProposalStatus::Rejected {
                        gradient: gradient.clone(),
                    };
                    last_gradient = Some(gradient);
                }
            }
        }

        let elapsed = deadline.elapsed();

        // All generations exhausted — decide whether to defer or abandon.
        // Max 2 deferrals (retry_count 0→1→2) = up to 3 rounds × 3 = 9 effective attempts over 72h.
        // `deferred_retry_count` is passed by the caller, NOT queried from DB (review issue #16).
        if deferred_retry_count < 2 {
            let retry_after_hours = 24.0;
            let next_retry = deferred_retry_count + 1;
            info!(
                agent = agent_id,
                generations = effective_max,
                retry_count = next_retry,
                "GVU loop deferred — will retry in {retry_after_hours}h with accumulated gradients"
            );

            // Store deferred for later retry
            if let Err(e) = vs.store_deferred(agent_id, &gradients, retry_after_hours, next_retry) {
                warn!(
                    agent = agent_id,
                    "Failed to store deferred GVU: {e} — deferral lost"
                );
            }

            // Log the deferred experiment
            vs.record_experiment(&ExperimentLogEntry::new(
                agent_id,
                effective_max,
                effective_max,
                elapsed,
                "deferred",
                &format!("All {effective_max} generations rejected — deferred retry #{next_retry}"),
            ));

            GvuOutcome::Deferred {
                accumulated_gradients: gradients,
                retry_after_hours,
                retry_count: next_retry,
            }
        } else {
            warn!(
                agent = agent_id,
                generations = effective_max,
                total_retries = deferred_retry_count,
                "GVU loop permanently abandoned after all deferred retries"
            );

            // Log the abandoned experiment
            vs.record_experiment(&ExperimentLogEntry::new(
                agent_id,
                effective_max,
                effective_max,
                elapsed,
                "abandoned",
                &format!("Permanently abandoned after {deferred_retry_count} deferred retries"),
            ));

            GvuOutcome::Abandoned {
                last_gradient: last_gradient.unwrap_or_else(|| {
                    TextGradient::blocking(
                        "GVU",
                        "loop",
                        "All attempts exhausted after deferred retries",
                        "Consider manual SOUL.md review",
                    )
                }),
            }
        }
    }

    /// WP0.2 (R2): one consolidation attempt, replacing the ordinary generation
    /// loop for this run.
    ///
    /// Ordering of the gates below is deliberate and each step is fail-closed —
    /// every failure leaves SOUL.md byte-for-byte untouched and raises a visible
    /// alert, because a stuck agent (the status quo) is strictly better than an
    /// agent whose instructions were destroyed by a bad compression:
    ///
    /// 1. **Frequency lock** — at most one attempt per
    ///    [`super::consolidate::CONSOLIDATE_MIN_INTERVAL_DAYS`] days per agent.
    /// 2. **Budget consumed up front** — the audit row is written *before* the
    ///    LLM call, so a crash or a repeatedly-failing consolidation cannot loop
    ///    on the most expensive call in the stack. Same "record on entry"
    ///    discipline as the WP0.3 cooldown.
    /// 3. **Deterministic acceptance first** — the collapse guard and the
    ///    content-safety scans run before the paid Verifier judge, matching the
    ///    B2 ordering used by the ordinary path.
    /// 4. **Full Verifier Gate** — the candidate document is placed in
    ///    `proposal.content`, so L-Safety / L1 / L2 / L2.5 / L3 judge / L3.5 /
    ///    canary / L4 all evaluate the artifact that will actually be written.
    ///    (For consolidation this is *stricter* than the ordinary path: L1's
    ///    `must_not` and sensitive-pattern scans normally see only the small
    ///    patch body, here they see the entire resulting document.)
    /// 5. **Write + observation window** — a normal `SoulVersion`, so the
    ///    existing 24 h metric-based auto-rollback covers it.
    #[allow(clippy::too_many_arguments)]
    async fn run_consolidation<F, Fut>(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        current_soul: &str,
        breach: &super::consolidate::CapBreach,
        trigger_context: &str,
        pre_metrics: VersionMetrics,
        must_not: &[String],
        must_always: &[String],
        relevant_mistakes: &[MistakeEntry],
        vs: &VersionStore,
        call_llm: F,
    ) -> GvuOutcome
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        use super::consolidate;

        let started = Instant::now();

        // ── 1. Frequency lock ──────────────────────────────────────────
        if let Some(last) = vs.last_consolidation_at(agent_id) {
            let next_allowed =
                last + chrono::Duration::days(consolidate::CONSOLIDATE_MIN_INTERVAL_DAYS);
            if chrono::Utc::now() < next_allowed {
                debug!(
                    agent = agent_id,
                    last_attempt = %last.to_rfc3339(),
                    next_allowed = %next_allowed.to_rfc3339(),
                    "SOUL.md over cap but consolidation is rate-limited — staying quiet"
                );
                return GvuOutcome::Skipped {
                    reason: format!(
                        "SOUL.md over cap ({} lines / {} bytes); consolidation last attempted {} — \
                         next attempt allowed {}",
                        breach.lines,
                        breach.bytes,
                        last.to_rfc3339(),
                        next_allowed.to_rfc3339(),
                    ),
                };
            }
        }

        // ── 2. Consume the budget before spending anything ─────────────
        let Some(audit_id) = vs.record_consolidation_attempt(agent_id, breach.bytes) else {
            warn!(
                agent = agent_id,
                "Failed to record consolidation attempt — refusing to run unthrottled"
            );
            return GvuOutcome::Skipped {
                reason: "Could not record the consolidation attempt; refusing to proceed \
                         without a working frequency lock"
                    .to_string(),
            };
        };

        // Shared failure exit: close the audit row, log a rejected experiment,
        // alert, and leave SOUL.md untouched.
        macro_rules! abandon {
            ($outcome:expr, $detail:expr) => {{
                let detail: String = $detail;
                vs.finish_consolidation(&audit_id, $outcome, None, &detail);
                vs.record_experiment(&ExperimentLogEntry::new(
                    agent_id,
                    1,
                    1,
                    started.elapsed(),
                    "skipped",
                    &format!("Consolidation {}: {}", $outcome, detail),
                ));
                self.raise_alert(
                    agent_id,
                    "gvu_consolidate_failed",
                    &format!("{} 原因：{}", breach.summary_zh(agent_id), detail),
                )
                .await;
                return GvuOutcome::Skipped {
                    reason: format!("Consolidation {}: {}", $outcome, detail),
                };
            }};
        }

        info!(
            agent = agent_id,
            lines = breach.lines,
            bytes = breach.bytes,
            target_lines = consolidate::target_lines(),
            target_bytes = consolidate::target_bytes(),
            "SOUL.md over cap — entering consolidate mode"
        );

        // ── 3a. Generate the whole-file rewrite ────────────────────────
        let prompt =
            consolidate::build_consolidate_prompt(agent_id, current_soul, breach, must_always);
        let response = match call_llm(prompt).await {
            Ok(r) => r,
            Err(e) => abandon!("generation_failed", format!("整併模型呼叫失敗（{e}）")),
        };

        let Some(candidate) = consolidate::parse_consolidate_response(&response) else {
            abandon!(
                "unparseable",
                "整併模型未回傳可用的 consolidated_soul 內容".to_string()
            )
        };

        // ── 3b. Deterministic acceptance (collapse guard) ──────────────
        let report = match consolidate::verify_consolidation(current_soul, &candidate, must_always) {
            Ok(r) => r,
            Err(e) => abandon!("collapse_guard", e),
        };
        if let Err(e) = consolidate::scan_consolidated_soul(agent_id, current_soul, &candidate) {
            abandon!("content_safety", e)
        }

        // ── 4. Full Verifier Gate on the real artifact ─────────────────
        let mut proposal = EvolutionProposal::new(
            agent_id.to_string(),
            ProposalType::SoulPatch,
            trigger_context.to_string(),
        );
        // `content` carries the candidate document itself so every verifier
        // layer inspects what will actually be written. `patch` stays None —
        // this is not a section edit, and `Updater::apply` is never reached
        // (we call `apply_consolidation` instead).
        proposal.content = candidate.clone();
        proposal.rationale = consolidate::parse_consolidate_summary(&response).unwrap_or_else(|| {
            format!(
                "Consolidate SOUL.md from {} to {} bytes to clear the size cap",
                report.original_bytes, report.new_bytes
            )
        });
        proposal.status = ProposalStatus::Verifying;

        // B2 ordering: deterministic layers first, judge only if they pass.
        let pre_gate = verifier::verify_all_with_mistakes(
            &proposal,
            current_soul,
            must_not,
            must_always,
            vs,
            None,
            relevant_mistakes,
        );
        let verdict = match pre_gate {
            VerificationResult::Rejected { gradient } => VerificationResult::Rejected { gradient },
            VerificationResult::Approved { .. } => {
                let judge_prompt =
                    verifier::build_judge_prompt(&proposal, current_soul, must_not, must_always);
                let judge_result = match call_llm(judge_prompt).await {
                    Ok(r) => Some(verifier::parse_judge_response(&r)),
                    Err(e) => {
                        warn!(
                            agent = agent_id,
                            "Consolidation judge LLM call failed: {e}, proceeding without L3"
                        );
                        None
                    }
                };
                verifier::verify_all_with_mistakes(
                    &proposal,
                    current_soul,
                    must_not,
                    must_always,
                    vs,
                    judge_result.as_ref(),
                    relevant_mistakes,
                )
            }
        };

        if let VerificationResult::Rejected { gradient } = verdict {
            abandon!(
                "verifier_rejected",
                format!("{}：{}", gradient.source_layer, gradient.critique)
            )
        }
        proposal.status = ProposalStatus::Approved;

        // ── 5. Write + observation window ──────────────────────────────
        match self
            .updater
            .apply_consolidation(&proposal, agent_dir, &candidate, pre_metrics)
        {
            Ok((version, asi)) => {
                vs.finish_consolidation(
                    &audit_id,
                    "applied",
                    Some(report.new_bytes),
                    &format!(
                        "version={} asi={asi:.3} sections={} identity_locked={}",
                        version.version_id, report.sections_preserved,
                        report.identity_sections_locked,
                    ),
                );
                vs.record_experiment(&ExperimentLogEntry::new(
                    agent_id,
                    1,
                    1,
                    started.elapsed(),
                    "applied",
                    &format!(
                        "Consolidated SOUL.md {} → {} bytes ({} → {} lines)",
                        report.original_bytes,
                        report.new_bytes,
                        report.original_lines,
                        report.new_lines,
                    ),
                ));
                // A whole-file rewrite is a big enough event that the operator
                // should see it even when it succeeds.
                self.raise_alert(
                    agent_id,
                    "gvu_consolidated",
                    &report.summary_zh(agent_id),
                )
                .await;
                GvuOutcome::Applied(version)
            }
            Err(e) => abandon!("apply_failed", e),
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluator delegation helpers (Phase 2 GVU²)
// ---------------------------------------------------------------------------

/// Build a DelegationEnvelope for sending a verification request to an evaluator agent.
///
/// The evaluator receives the proposal content, current SOUL.md, and acceptance criteria,
/// then returns a structured verdict (APPROVED/REJECTED with evidence).
pub fn build_evaluator_request(
    proposal: &super::proposal::EvolutionProposal,
    current_soul: &str,
    must_not: &[String],
    must_always: &[String],
) -> crate::delegation::DelegationEnvelope {
    let criteria: Vec<String> = must_not
        .iter()
        .map(|s| format!("Must NOT: {s}"))
        .chain(must_always.iter().map(|s| format!("Must ALWAYS: {s}")))
        .collect();

    crate::delegation::DelegationEnvelope {
        task: format!(
            "Verify the following SOUL.md evolution proposal.\n\n\
             ## Current SOUL.md (excerpt)\n{}\n\n\
             ## Proposed Changes\n{}\n\n\
             ## Rationale\n{}",
            truncate_bytes(&current_soul, 2000),
            proposal.content,
            proposal.rationale,
        ),
        context: crate::delegation::DelegationContext {
            briefing: format!(
                "Agent '{}' triggered evolution due to: {}",
                proposal.agent_id, proposal.trigger_context
            ),
            constraints: criteria,
            ..Default::default()
        },
        expected_output: crate::delegation::OutputSpec {
            format: crate::delegation::OutputFormat::Decision,
            max_length: Some(2000),
        },
    }
}

/// Parse an evaluator agent's response into a VerificationResult.
///
/// Expects the evaluator to respond with "APPROVED" or "REJECTED" (possibly
/// wrapped in JSON). Falls back to keyword detection if JSON parsing fails.
pub fn parse_evaluator_response(response: &str) -> verifier::VerificationResult {
    // Try JSON parse first
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
        let verdict = json.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
        let confidence = json
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);

        if verdict.contains("APPROVED") {
            return verifier::VerificationResult::Approved {
                confidence,
                advisories: Vec::new(),
            };
        } else {
            let critique = json
                .get("gradient")
                .and_then(|g| g.get("critique"))
                .and_then(|c| c.as_str())
                .unwrap_or("Evaluator rejected the proposal");
            let suggestion = json
                .get("gradient")
                .and_then(|g| g.get("suggestion"))
                .and_then(|s| s.as_str())
                .unwrap_or("Address the evaluator's concerns");
            return verifier::VerificationResult::Rejected {
                gradient: TextGradient::blocking("L-Evaluator", "proposal", critique, suggestion),
            };
        }
    }

    // Fallback: keyword detection
    let lower = response.to_lowercase();
    if lower.contains("approved") && !lower.contains("rejected") {
        verifier::VerificationResult::Approved {
            confidence: 0.6,
            advisories: Vec::new(),
        }
    } else {
        verifier::VerificationResult::Rejected {
            gradient: TextGradient::blocking(
                "L-Evaluator",
                "proposal",
                &format!("Evaluator response: {}", truncate_bytes(response, 200)),
                "Revise proposal based on evaluator feedback",
            ),
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use super::{FREE_MAX_GENERATIONS, gated_max_generations};

    #[test]
    fn free_tier_is_capped_at_three_rounds() {
        // No industry_evolution_params → fixed 3-round free behaviour, whatever
        // the pre-gate adaptive budget was.
        assert_eq!(gated_max_generations(7, false), FREE_MAX_GENERATIONS);
        assert_eq!(gated_max_generations(5, false), FREE_MAX_GENERATIONS);
        assert_eq!(gated_max_generations(3, false), 3);
        // A base already below the cap is left untouched (min, not clamp-up).
        assert_eq!(gated_max_generations(2, false), 2);
    }

    #[test]
    fn business_tier_unlocks_adaptive_depth() {
        // industry_evolution_params → full adaptive 3–7 passes through.
        assert_eq!(gated_max_generations(7, true), 7);
        assert_eq!(gated_max_generations(5, true), 5);
        assert_eq!(gated_max_generations(3, true), 3);
    }

    #[test]
    fn free_baseline_constant_is_three() {
        assert_eq!(FREE_MAX_GENERATIONS, 3);
    }
}

/// R5: `[evolution] legacy_soul_evolution` direction, pinned.
///
/// absent / malformed / wrong-typed ⇒ `false` ⇒ AEE. This points the
/// **opposite** way from the other `[evolution]` gates in this file, and the
/// asymmetry is deliberate: AEE is the path that cannot write SOUL.md at all,
/// so falling back to the legacy writer on an unreadable config would let a
/// config typo silently re-open the write surface WP1.1 closed. Only an
/// explicit `true` selects the legacy path.
#[cfg(test)]
mod legacy_soul_evolution_tests {
    use super::agent_legacy_soul_evolution;

    fn dir_with(body: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("agent.toml"), body).unwrap();
        dir
    }

    #[test]
    fn default_direction_legacy_soul_evolution_is_off_unless_explicit() {
        for body in [
            "",                                            // empty file
            "[evolution]\n",                               // section, no key
            "[evolution]\ngvu_enabled = true\n",           // sibling only
            "[evolution]\nlegacy_soul_evolution = false\n", // explicit off
            "[evolution]\nlegacy_soul_evolution = \"true\"\n", // wrong type
            "[evolution]\nlegacy_soul_evolution = 1\n",     // wrong type
            "evolution = \"scalar\"\n",                    // wrong-typed section
            "not toml [[[",                                // malformed file
        ] {
            let dir = dir_with(body);
            assert!(
                !agent_legacy_soul_evolution(dir.path()),
                "the SOUL.md write path must stay closed for {body:?}"
            );
        }

        // Missing file entirely — same direction.
        let empty = tempfile::TempDir::new().unwrap();
        assert!(!agent_legacy_soul_evolution(empty.path()));

        let on = dir_with("[evolution]\nlegacy_soul_evolution = true\n");
        assert!(agent_legacy_soul_evolution(on.path()));
    }
}

/// WP0.3 (2026-08-06, root cause R4): per-agent GVU cooldown, enforced in
/// `run_with_context_and_retry` — the single entry point every public `run*`
/// method funnels through, so these tests exercise the gate the same way
/// every real caller (channel reply, dispatcher) does.
///
/// Every case here uses an agent directory with no `SOUL.md`, so a call that
/// gets past the cooldown gate fails fast on the SOUL.md read (before ever
/// reaching `call_llm`) — `never_called` panics if invoked, proving no LLM
/// budget is spent by a cooldown-gated (or otherwise short-circuited) run.
#[cfg(test)]
mod cooldown_tests {
    use super::*;

    fn agent_dir_with_cooldown(minutes: Option<u64>) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = match minutes {
            Some(m) => format!("[evolution]\ngvu_cooldown_minutes = {m}\n"),
            None => String::new(),
        };
        std::fs::write(dir.path().join("agent.toml"), toml).unwrap();
        dir
    }

    async fn never_called(_prompt: String) -> Result<String, String> {
        panic!("call_llm must not be invoked when the run short-circuits before generation");
    }

    #[tokio::test]
    async fn cooldown_blocks_second_call_within_window() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"), None, None);
        let agent_dir = agent_dir_with_cooldown(Some(60));

        // First call: no SOUL.md present, so it short-circuits with a
        // non-cooldown Skipped reason — but the cooldown clock still starts
        // (recorded on gate entry, before the SOUL.md read).
        let first = gvu
            .run_with_context(
                "agent-cd",
                agent_dir.path(),
                "ctx",
                VersionMetrics::default(),
                &[],
                &[],
                never_called,
                None,
                Vec::new(),
            )
            .await;
        match first {
            GvuOutcome::Skipped { reason } => assert!(
                !reason.contains("cooldown"),
                "first call should fail on missing SOUL.md, not cooldown: {reason}"
            ),
            other => panic!("expected Skipped, got {other:?}"),
        }

        // Second call, same agent, immediately after — must be blocked by
        // the cooldown gate before even attempting the SOUL.md read.
        let second = gvu
            .run_with_context(
                "agent-cd",
                agent_dir.path(),
                "ctx",
                VersionMetrics::default(),
                &[],
                &[],
                never_called,
                None,
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
    }

    #[tokio::test]
    async fn cooldown_zero_disables_throttling() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"), None, None);
        let agent_dir = agent_dir_with_cooldown(Some(0));

        for attempt in 0..2 {
            let outcome = gvu
                .run_with_context(
                    "agent-nocd",
                    agent_dir.path(),
                    "ctx",
                    VersionMetrics::default(),
                    &[],
                    &[],
                    never_called,
                    None,
                    Vec::new(),
                )
                .await;
            match outcome {
                GvuOutcome::Skipped { reason } => assert!(
                    !reason.contains("cooldown"),
                    "attempt {attempt}: gvu_cooldown_minutes=0 must never skip for cooldown, got: {reason}"
                ),
                other => panic!("attempt {attempt}: expected Skipped, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn cooldown_is_per_agent_not_global() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let gvu = GvuLoop::new(&db_dir.path().join("evolution.db"), None, None);
        let agent_a = agent_dir_with_cooldown(Some(60));
        let agent_b = agent_dir_with_cooldown(Some(60));

        let _ = gvu
            .run_with_context(
                "agent-a",
                agent_a.path(),
                "ctx",
                VersionMetrics::default(),
                &[],
                &[],
                never_called,
                None,
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
                VersionMetrics::default(),
                &[],
                &[],
                never_called,
                None,
                Vec::new(),
            )
            .await;
        match outcome_b {
            GvuOutcome::Skipped { reason } => assert!(
                !reason.contains("cooldown"),
                "agent-b must not inherit agent-a's cooldown: {reason}"
            ),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
