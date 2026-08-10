//! Prediction-error-driven evolution engine (Phase 1).
//!
//! Replaces fixed heartbeat scheduling with an event-driven system inspired by:
//! - Active Inference / Free Energy Principle (Friston)
//! - Dual Process Theory (Kahneman) — System 1 (fast rules) / System 2 (LLM)
//! - Metacognitive Learning (ICML 2025) — self-calibrating thresholds
//!
//! 90% of conversations complete via the System 1 path (zero LLM cost).
//! Only genuine prediction errors trigger expensive LLM reflections.

pub mod calibration;
pub mod engine;
pub mod feedback_bus;
pub mod forced_reflection;
pub mod metacognition;
pub mod metrics;
pub mod outcome;
pub mod router;
pub mod rule_lifecycle;
pub mod subagent_prediction;
pub mod user_model;

// ── A3 task-forward-model (design-task-forward-model-2026-08-06.md) ──
// Parallel to the conversational PredictionEngine above (design §4.3) —
// predicts what a goal-loop dispatch round will DO (tool classes, call
// volume, outcome, artifact shape), not user reaction. Not wired into any
// hot path yet (WP-A9 is out of scope for this change).
pub mod tool_class;
pub mod task_forward;
pub mod task_forward_store;
pub mod task_observe;
pub mod transition;
pub mod foresight_gate;
pub mod rule_gate;
// ── A4 rule induce/update/prune (design §6.5) ──
pub mod task_rule_induce;

#[cfg(test)]
mod tests;
