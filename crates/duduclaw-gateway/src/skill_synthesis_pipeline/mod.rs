//! Rollout-to-Skill Synthesis Pipeline — W19-P0.
//!
//! Implements the COSPLAY-inspired feedback loop that automatically distills
//! high-quality task execution trajectories (from EvolutionEvents JSONL) into
//! reusable skills stored in the Skill Bank.
//!
//! ## Module structure
//!
//! - [`quality_scorer`] — Phase 1: parse JSONL, score trajectories, filter top-20%
//! - [`pipeline`] — Orchestration: dry-run (Week 1) and full graduation (Week 2+)
//!
//! ## Quick start (dry-run)
//!
//! ```rust,ignore
//! use duduclaw_gateway::skill_synthesis_pipeline::pipeline::{PipelineConfig, run};
//!
//! let config = PipelineConfig::default(); // dry_run = true by default
//! let result = run(&config).await;
//! println!("{}", result.summary());
//! ```
//!
//! ## References
//! - arXiv:2604.20987 — COSPLAY: Skill-augmented Agent Self-Play (+25.1% perf)
//! - W18 Tech Memo: Agent Handoff 2.0 — gap analysis & sprint estimates
//! - EvolutionEvents Spec v1.0 — 8-field JSONL schema

pub mod pipeline;
pub mod quality_scorer;
