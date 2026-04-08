//! Skill Lifecycle Pipeline — Progressive Injection + Feedback-Driven + Distillation.
//!
//! Phase A: Progressive injection (Layer 0/1/2) for token efficiency
//! Phase B: Feedback-driven activation via PredictionEngine errors
//! Phase C: Skill distillation into SOUL.md for long-term convergence

pub mod activation;
pub mod compression;
pub mod diagnostician;
pub mod distillation;
pub mod extraction;
pub mod gap;
pub mod lift;
pub mod reconstruction;
pub mod relevance;

#[cfg(test)]
mod tests;
