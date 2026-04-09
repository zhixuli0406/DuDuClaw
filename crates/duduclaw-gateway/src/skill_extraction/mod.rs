//! Skill Extraction — records conversation trajectories and extracts reusable skills.
//!
//! Phase 3 of the skill lifecycle: detects user feedback, extracts skills
//! from successful conversation trajectories, and persists them to the SkillCache.
//!
//! Note: `extractor.rs`, `bank.rs`, and `tests.rs` were orphan modules merged
//! into `recorder.rs` as the single source of truth (Bayesian update, SQLite
//! persistence, contrastive refinement features incorporated).

pub mod recorder;
