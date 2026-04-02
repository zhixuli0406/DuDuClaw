//! Experiment data collection for paper experiments and ROI analytics.
//!
//! Records per-conversation metrics (prediction, error, evolution action, cost)
//! and GVU round details for academic paper experiments and product ROI reports.
//!
//! See `docs/TODO-paper-experiments.md` for experiment design.

mod logger;
mod types;

pub use logger::{csv_escape, ExperimentLogger};
pub use types::{ExperimentRecord, GvuRecord, VersionOutcomeRecord};
