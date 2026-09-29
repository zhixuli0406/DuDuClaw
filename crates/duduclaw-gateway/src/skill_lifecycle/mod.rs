//! Skill Lifecycle Pipeline — Progressive Injection + Feedback-Driven + Distillation.
//!
//! Phase A: Progressive injection (Layer 0/1/2) for token efficiency
//! Phase B: Feedback-driven activation via PredictionEngine errors
//! Phase C: Skill distillation into SOUL.md for long-term convergence

pub mod activation;
pub mod compression;
pub mod curator;
pub mod diagnostician;
pub mod distillation;
pub mod extraction;
pub mod gap;
pub mod gap_accumulator;
pub mod graduation;
pub mod hub_install;
pub mod lift;
pub mod relevance;
pub mod sandbox_trial;
pub mod security_scanner;
pub mod sensitive_patterns;
/// H2 — consumes a `SynthesisTrigger`: generate → vet → sandbox trial →
/// `confirm_synthesis` / `cancel_pending`. Without it the chain dead-ends.
pub mod synthesis_runner;
pub mod synthesizer;
pub mod vetting;

#[cfg(test)]
mod tests;
