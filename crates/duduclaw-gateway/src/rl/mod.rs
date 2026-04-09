//! Reinforcement Learning infrastructure for agent trajectory collection and reward computation.
//!
//! Prepares DuDuClaw agent conversations as RL training data for fine-tuning
//! local tool-calling models (e.g., via GRPO or DAPO algorithms).
//!
//! References:
//! - RC-GRPO (arXiv 2602.03025): reward conditioning for multi-turn sparse reward
//! - Agent-R1 (arXiv 2511.14460): modular MDP framework, agent/environment token separation
//! - ToolRM (arXiv 2509.11963): tool-use specific outcome reward model
//! - OpenHands RL SWE (arXiv 2508.03501): soft overlong punishment
//! - Self-Play SWE-RL (arXiv 2512.18552): self-play zero-annotation training

pub mod reward;
pub mod trajectory_export;
pub mod types;

#[cfg(test)]
mod tests;
