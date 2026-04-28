//! Prediction-error-driven evolution engine (Phase 1).
//!
//! Replaces fixed heartbeat scheduling with an event-driven system inspired by:
//! - Active Inference / Free Energy Principle (Friston)
//! - Dual Process Theory (Kahneman) — System 1 (fast rules) / System 2 (LLM)
//! - Metacognitive Learning (ICML 2025) — self-calibrating thresholds
//!
//! 90% of conversations complete via the System 1 path (zero LLM cost).
//! Only genuine prediction errors trigger expensive LLM reflections.

pub mod engine;
pub mod forced_reflection;
pub mod metacognition;
pub mod metrics;
pub mod outcome;
pub mod router;
pub mod subagent_prediction;
pub mod user_model;

#[cfg(test)]
mod tests;
