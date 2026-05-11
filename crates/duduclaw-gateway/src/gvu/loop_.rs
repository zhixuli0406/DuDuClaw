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
use tracing::{info, warn};

use super::generator::{Generator, GeneratorInput};
use super::mistake_notebook::MistakeEntry;
use super::proposal::{EvolutionProposal, ProposalStatus, ProposalType};
use super::text_gradient::TextGradient;
use super::updater::Updater;
use super::verifier::{self, VerificationResult};
use super::version_store::{ExperimentLogEntry, SoulVersion, VersionMetrics, VersionStore};

use crate::prediction::metacognition::MetaCognition;

/// Maximum number of generation attempts per GVU cycle.
/// Adaptive depth: up to 7 rounds for thorough evolution.
const DEFAULT_MAX_GENERATIONS: u32 = 7;

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
}

impl GvuLoop {
    /// Create a new GVU loop with shared VersionStore.
    ///
    /// If `encryption_key` is provided, rollback_diff is encrypted at rest.
    pub fn new(db_path: &Path, observation_hours: Option<f64>, max_generations: Option<u32>) -> Self {
        Self::with_encryption(db_path, observation_hours, max_generations, None)
    }

    /// Create with optional AES-256-GCM encryption for rollback_diff.
    pub fn with_encryption(
        db_path: &Path,
        observation_hours: Option<f64>,
        max_generations: Option<u32>,
        encryption_key: Option<&[u8; 32]>,
    ) -> Self {
        Self::with_options(db_path, observation_hours, max_generations, encryption_key, None)
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
        }
    }

    /// Access the updater (for observation checking from heartbeat).
    pub fn updater(&self) -> &Updater {
        &self.updater
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
            agent_id, agent_dir, trigger_context, pre_metrics,
            must_not, must_always, call_llm, None, Vec::new(), 0,
        ).await
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
            agent_id, agent_dir, trigger_context, pre_metrics,
            must_not, must_always, call_llm, metacognition, relevant_mistakes, 0,
        ).await
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
            locks.entry(agent_id.to_string())
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

        let mut proposal = EvolutionProposal::new(
            agent_id.to_string(),
            ProposalType::SoulPatch,
            trigger_context.to_string(),
        );

        let mut gradients: Vec<TextGradient> = Vec::new();
        let mut last_gradient: Option<TextGradient> = None;

        // Adaptive depth: use MetaCognition if available, else fixed max_generations
        let effective_max = metacognition
            .map(|mc| mc.adaptive_max_generations())
            .unwrap_or(self.max_generations);

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
                    if let Err(e) = vs.store_deferred(agent_id, &gradients, retry_after_hours, next_retry) {
                        warn!(agent = agent_id, "Failed to store deferred GVU after timeout: {e}");
                    }
                }

                return GvuOutcome::TimedOut {
                    elapsed,
                    generations_completed,
                    accumulated_gradients: gradients,
                };
            }

            info!(agent = agent_id, generation = attempt, "GVU loop generation {attempt}");

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

            // 2. Verify (L1, L2, L4 — deterministic)
            // For L3 (LLM Judge), make a separate call
            let judge_prompt = verifier::build_judge_prompt(
                &proposal, &current_soul, must_not, must_always,
            );
            let judge_result = match call_llm(judge_prompt).await {
                Ok(r) => Some(verifier::parse_judge_response(&r)),
                Err(e) => {
                    warn!(agent = agent_id, "Judge LLM call failed: {e}, proceeding without L3");
                    None
                }
            };

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

            let result = verifier::verify_all_with_mistakes(
                &proposal,
                &current_soul,
                must_not,
                must_always,
                &vs,
                judge_result.as_ref(),
                &relevant_mistakes,
            );

            generations_completed = attempt;

            match result {
                VerificationResult::Approved { confidence, advisories } => {
                    info!(
                        agent = agent_id,
                        generation = attempt,
                        confidence = format!("{confidence:.2}"),
                        advisories = advisories.len(),
                        "Proposal approved"
                    );
                    proposal.status = ProposalStatus::Approved;

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
                                            warn!(agent = agent_id, "Failed to apply wiki proposals: {e}");
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
                                &format!("Approved at generation {attempt} (confidence: {confidence:.2})"),
                            ));

                            return GvuOutcome::Applied(version);
                        }
                        Err(e) => {
                            warn!(agent = agent_id, "Failed to apply proposal: {e}");
                            let elapsed = deadline.elapsed();
                            vs.record_experiment(&ExperimentLogEntry::new(
                                agent_id, attempt, effective_max, elapsed,
                                "skipped", &format!("Apply failed: {e}"),
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
                warn!(agent = agent_id, "Failed to store deferred GVU: {e} — deferral lost");
            }

            // Log the deferred experiment
            vs.record_experiment(&ExperimentLogEntry::new(
                agent_id, effective_max, effective_max, elapsed,
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
                agent_id, effective_max, effective_max, elapsed,
                "abandoned",
                &format!("Permanently abandoned after {deferred_retry_count} deferred retries"),
            ));

            GvuOutcome::Abandoned {
                last_gradient: last_gradient.unwrap_or_else(|| {
                    TextGradient::blocking("GVU", "loop", "All attempts exhausted after deferred retries", "Consider manual SOUL.md review")
                }),
            }
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
        let confidence = json.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);

        if verdict.contains("APPROVED") {
            return verifier::VerificationResult::Approved {
                confidence,
                advisories: Vec::new(),
            };
        } else {
            let critique = json.get("gradient")
                .and_then(|g| g.get("critique"))
                .and_then(|c| c.as_str())
                .unwrap_or("Evaluator rejected the proposal");
            let suggestion = json.get("gradient")
                .and_then(|g| g.get("suggestion"))
                .and_then(|s| s.as_str())
                .unwrap_or("Address the evaluator's concerns");
            return verifier::VerificationResult::Rejected {
                gradient: TextGradient::blocking(
                    "L-Evaluator",
                    "proposal",
                    critique,
                    suggestion,
                ),
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
