//! GVU Loop orchestrator — the convergent Generator→Verifier→Updater cycle.
//!
//! Runs up to `max_generations` rounds. Each round:
//! 1. Generator produces a proposal (with TextGradient feedback if retrying)
//! 2. Verifier evaluates through 4 layers
//! 3. If approved → Updater applies with observation period
//! 4. If rejected → TextGradient fed back to Generator for next round

use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{info, warn};

use super::generator::{Generator, GeneratorInput};
use super::proposal::{EvolutionProposal, ProposalStatus, ProposalType};
use super::text_gradient::TextGradient;
use super::updater::Updater;
use super::verifier::{self, VerificationResult};
use super::version_store::{SoulVersion, VersionMetrics, VersionStore};

/// Maximum number of generation attempts per GVU cycle.
const DEFAULT_MAX_GENERATIONS: u32 = 3;

/// Outcome of a complete GVU loop execution.
#[derive(Debug, Clone)]
pub enum GvuOutcome {
    /// A proposal was approved and applied — observation period started.
    Applied(SoulVersion),
    /// All generation attempts were rejected.
    Abandoned { last_gradient: TextGradient },
    /// Loop was skipped (e.g., active observation already in progress).
    Skipped { reason: String },
}

/// The GVU loop orchestrator.
pub struct GvuLoop {
    generator: Generator,
    updater: Updater,
    max_generations: u32,
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
        let version_store_gen = VersionStore::with_crypto(db_path, encryption_key);
        let version_store_upd = VersionStore::with_crypto(db_path, encryption_key);

        Self {
            generator: Generator::new(version_store_gen),
            updater: Updater::new(version_store_upd, observation_hours),
            max_generations: max_generations.unwrap_or(DEFAULT_MAX_GENERATIONS),
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

        for attempt in 1..=self.max_generations {
            info!(agent = agent_id, generation = attempt, "GVU loop generation {attempt}");

            // 1. Generate
            let input = GeneratorInput {
                agent_id: agent_id.to_string(),
                agent_soul: current_soul.clone(),
                trigger_context: trigger_context.to_string(),
                previous_gradients: gradients.clone(),
                generation: attempt,
            };

            let prompt = self.generator.generate(&input, &mut proposal);

            // Call LLM for generation
            let llm_response = match call_llm(prompt).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(agent = agent_id, "Generator LLM call failed: {e}");
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

            let result = verifier::verify_all(
                &proposal,
                &current_soul,
                must_not,
                must_always,
                &vs,
                judge_result.as_ref(),
            );

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

                    // 3. Apply
                    match self.updater.apply(&proposal, agent_dir, pre_metrics) {
                        Ok(version) => {
                            proposal.status = ProposalStatus::Observing;
                            return GvuOutcome::Applied(version);
                        }
                        Err(e) => {
                            warn!(agent = agent_id, "Failed to apply proposal: {e}");
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

        // All generations exhausted
        warn!(
            agent = agent_id,
            generations = self.max_generations,
            "GVU loop exhausted all attempts"
        );

        GvuOutcome::Abandoned {
            last_gradient: last_gradient.unwrap_or_else(|| {
                TextGradient::blocking("GVU", "loop", "All attempts exhausted", "Try with different trigger context")
            }),
        }
    }
}
