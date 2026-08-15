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
pub mod mistake_notebook;
pub mod observation_finalizer;
pub mod proposal;
pub mod shadow_mode;
pub mod soul_partition;
pub mod text_gradient;
pub mod trigger;
pub mod updater;
pub mod verifier;
pub mod version_store;

#[cfg(test)]
mod tests;

// WP0.5 (TODO-evolution-v3-2026-08.md, 2026-08-06): stagnation detector —
// appended at the end of this module list per task instructions (another
// session is concurrently editing several sibling gvu/*.rs files).
pub mod stagnation;

// WP0.6 (TODO-evolution-v3-2026-08.md, 2026-08-06): Verifier/Updater
// rejection telemetry — appended at the end for the same reason as above.
pub mod reward_hack;
pub mod telemetry;

// WP0.2 (TODO-evolution-v3-2026-08.md, 2026-08-06): SOUL.md cap-deadlock
// breaker (consolidate mode) — appended at the end for the same reason.
pub mod consolidate;

// ---------------------------------------------------------------------------
// WP2.3 / WP2.4 / WP2.5 (2026-08-06) — AEE (Agentic Evolution Engine)
// `commercial/docs/DESIGN-evolution-v3-aee.md` chapters 2 and 3.
// ---------------------------------------------------------------------------

/// WP2.4 §2.2 — deterministic, zero-LLM gates that keep their veto.
pub mod verifier_gate;
/// WP2.4 §2.3/§2.4 — score dimensions (no veto) + the matches-or-improves
/// commit gate.
pub mod verifier_measure;
/// WP2.4 §2.4.1 — the reigning playbook snapshot a candidate is compared to.
pub mod champion;
/// WP2.3 §3 + WP2.5 — the agentic inner loop, strategy mix, prompt assembly
/// and entry-level accept/rollback.
pub mod aee;

#[cfg(test)]
mod tests_gate;
#[cfg(test)]
mod tests_measure;

// WP-6A / A2 (2026-08-15, `commercial/docs/DESIGN-evolution-harness-knobs-2026-08.md`
// §7.2-A2) — read-only harness-knob telemetry snapshot, appended at the end
// for the same reason as the WP0.x modules above (concurrent sibling edits).
pub mod knob_snapshot;
