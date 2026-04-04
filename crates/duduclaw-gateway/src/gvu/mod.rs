//! GVU (Generator-Verifier-Updater) self-play evolution loop (Phase 2).
//!
//! Replaces single-pass reflection with a convergent loop:
//! 1. **Generator** proposes SOUL.md changes (OPRO-style, history-aware)
//! 2. **Verifier** evaluates proposals through 4+ layers (deterministic → LLM judge → anti-sycophancy → canary)
//! 3. **Updater** applies with versioning, observation period, and rollback
//!
//! Failed verification produces TextGradients (concrete fix suggestions, not scores)
//! that feed back into the Generator for re-generation (max 3 rounds).
//!
//! ## Hardening (2025-Q2)
//!
//! - **Lexicographic safety ordering** (arXiv:2507.20964): strict priority P0-P6
//! - **Anti-sycophancy L3.5 check** (Sharma ICLR 2024): pattern + assertiveness detection
//! - **Canary/tripwire tests** (Carnegie Endowment 2024): regression detection
//! - **SOUL.md partitioning**: [identity] immutable + [behaviors] evolvable + [observations] TTL
//! - **Shadow-mode observation**: parallel old+new comparison
//! - **Diversity metrics**: proposal variety and response diversity tracking
//!
//! Theoretical foundations:
//! - GVU Self-Play (arXiv 2512.02731)
//! - OPRO prompt optimization (arXiv 2309.03409)
//! - TextGrad (arXiv 2406.07496, Nature)
//! - OpenAI Self-Evolving Agents Cookbook

pub mod diversity;
pub mod generator;
pub mod loop_;
pub mod proposal;
pub mod shadow_mode;
pub mod soul_partition;
pub mod text_gradient;
pub mod updater;
pub mod verifier;
pub mod version_store;

#[cfg(test)]
mod tests;
