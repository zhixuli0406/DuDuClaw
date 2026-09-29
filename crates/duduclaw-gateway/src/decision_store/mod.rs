//! Scoped, immutable inputs for replaying exploratory decision simulations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use duduclaw_memory::causal::{CausalStore, CausalStoreError, EvidenceScope, SourceArtifact};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::decision_calibration::{
    CapacityFit, ForecastBacktestResult, ForecastPoint, IntervalDiagnostic, KnownDayInputs,
    ObservedSupportDay, ProspectiveForecast, backtest_one_step_forecast, calibration_engine_sha256,
    diagnose_fixed_forecast_intervals, diagnose_forecast_intervals, forecast_next_day,
    validate as validate_observed_days,
};
use crate::decision_empirical::{
    EmpiricalError, EmpiricalParameterFit, EmpiricalSensitivityPlan, EmpiricalSensitivityReport,
    fit_empirical_parameters, simulate_empirical_sensitivity,
};
use crate::decision_event::{
    EventQueueConfig, EventSimulationError, EventSimulationResult, event_engine_sha256,
    simulate_ticket_events,
};
use crate::decision_ingest::{
    DailyStaffing, ImportedSupportPilot, KnownSlaDayInputs, ProspectiveSlaForecast,
    SlaHoldoutDiagnostic, SlaHoldoutError, SupportPilotExport, TicketEvent, build_support_pilot,
    derive_ticket_sla_labels, evaluate_ticket_sla_holdout, forecast_next_day_sla,
    ticket_sla_label_engine_sha256,
};
use crate::decision_outcome_calibration::{
    CapacityHoldoutDiagnostic, engine_sha256 as outcome_fit_engine_sha256, fit_and_score,
};
use crate::decision_policy::{
    JointRiskScreenCriteria, JointRiskScreenReport, StaffingResourcePlan,
    policy_screen_hash_matches,
};
use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationResult, StaffingScenario, engine_code_sha256, simulate,
};

const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const STORE_SCHEMA_VERSION: i64 = 1;
/// SQLite `user_version` written once the table set and every migration step
/// in `open()` have been applied. **Bump this whenever the DDL batch or a
/// migration in `open()` changes** — a file already reporting this version
/// skips both, so a silently added table would never be created.
const SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionScope {
    pub tenant_id: String,
    pub acl: String,
}

impl DecisionScope {
    pub(crate) fn valid(&self) -> bool {
        !self.tenant_id.trim().is_empty() && !self.acl.trim().is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionStoreError {
    #[error("invalid decision input or scope")]
    Invalid,
    #[error("decision input not found in scope")]
    NotFound,
    #[error("immutable decision input already exists with different content")]
    VersionConflict,
    #[error("decision input failed digest validation")]
    Corrupt,
    #[error("decision input was revoked with its source version")]
    Revoked,
    #[error("causal source store is required to verify this snapshot")]
    CausalStoreRequired,
    #[error("causal source verification failed: {0}")]
    Causal(#[from] CausalStoreError),
    #[error("CCR source revocation failed: {0}")]
    Ccr(#[from] duduclaw_llm::CcrError),
    #[error("decision input exceeds size limit")]
    TooLarge,
    #[error("decision serialization failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("decision storage failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("decision filesystem failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("decision simulation failure: {0}")]
    Simulation(#[from] crate::decision_sim::SimulationError),
    #[error("decision event simulation failure: {0}")]
    Event(#[from] EventSimulationError),
    #[error("decision empirical simulation failure: {0}")]
    Empirical(#[from] EmpiricalError),
    #[error("decision policy sweep failure: {0}")]
    PolicySweep(#[from] crate::decision_policy::PolicySweepError),
    #[error("observed outcome failed validation: {0}")]
    Observation(#[from] crate::decision_calibration::CalibrationError),
    #[error("ticket-level SLA holdout failed: {0}")]
    SlaHoldout(#[from] SlaHoldoutError),
    #[error("decision review was not approved or is no longer valid")]
    ReviewDenied,
    #[error("decision review broker failure: {0}")]
    ReviewBroker(String),
}

#[derive(Debug, Clone)]
pub struct DecisionStore {
    pub(super) path: PathBuf,
    pub(super) causal_store: Option<CausalStore>,
    // Only the import writer can read its own staged snapshot before the
    // source-bound completion receipt is committed.
    pub(super) allow_pending_operator_import: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CausalInvalidationResult {
    pub demoted_claims: usize,
    pub scrubbed_snapshots: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausalSourceRemoval {
    Invalidate,
    Erase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CausalSourceRemovalResult {
    pub demoted_claims: usize,
    pub scrubbed_snapshots: usize,
    pub scrubbed_ccr_originals: Option<usize>,
}

/// Sanitized operational inventory for the admin Decision Lab. Raw source
/// bytes and stored payloads are deliberately excluded from this projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionDashboardOverview {
    pub status: String,
    pub generated_at_utc: String,
    pub tenant_id: String,
    pub acl: String,
    pub counts: BTreeMap<String, u64>,
    pub invalidated_inputs: u64,
    pub ticket_sources: DecisionDashboardTicketSources,
    pub artifacts: Vec<DecisionDashboardArtifact>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionDashboardTicketSources {
    pub active: u64,
    pub expired: u64,
    pub revoked: u64,
    pub next_expiry_utc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionDashboardArtifact {
    pub kind: String,
    pub id: String,
    pub created_at_unix: i64,
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub target_snapshot_id: Option<String>,
    pub scenario_id: Option<String>,
    pub outcome_id: Option<String>,
    pub ticket_backed: bool,
    pub replay_hash: Option<String>,
}


// ── Submodules (audit O6 file split; pure code motion) ──────
//
// This module was one 12,476-line file. It is now a directory module
// whose submodules hold the same code verbatim; every path that used to
// resolve through `crate::decision_store::…` still does, via the
// re-exports below.

mod assess;
mod blob;
mod dashboard;
mod empirical;
mod forecast;
mod outcome;
mod replay;
mod retention;
mod review;
mod run;
mod schema;
mod score;
mod shadow_policy;
mod shadow_validate;
mod sla_forecast;
mod sla_score;
mod snapshot;
mod types;
mod types_run;
mod validation;

pub use shadow_validate::*;
pub use types::*;
pub use types_run::*;

impl DecisionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            causal_store: None,
            allow_pending_operator_import: false,
        }
    }

    /// Enable exact-scope source checks for snapshots bound to causal artifacts.
    pub fn with_causal_store(path: impl Into<PathBuf>, causal_store: CausalStore) -> Self {
        Self {
            path: path.into(),
            causal_store: Some(causal_store),
            allow_pending_operator_import: false,
        }
    }

    pub(crate) fn causal_source_store(&self) -> Result<&CausalStore, DecisionStoreError> {
        self.causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)
    }

    pub(crate) fn operator_import_writer(&self) -> Self {
        let mut writer = self.clone();
        writer.allow_pending_operator_import = true;
        writer
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn causal_store_path(&self) -> Option<&Path> {
        self.causal_store.as_ref().map(CausalStore::path)
    }

    pub(crate) fn causal_store(&self) -> Option<&CausalStore> {
        self.causal_store.as_ref()
    }

}

/// Every source file of this module, for the engine fingerprints that used to
/// read `include_str!("decision_store.rs")` before the audit O6 file split.
///
/// The fingerprint's contract is "hash the whole store", so a file added under
/// `decision_store/` MUST be added here too — otherwise a change to it would
/// silently leave stored records looking current.
pub(crate) const ENGINE_SOURCES: &[&str] = &[
    include_str!("mod.rs"),
    include_str!("assess.rs"),
    include_str!("blob.rs"),
    include_str!("dashboard.rs"),
    include_str!("empirical.rs"),
    include_str!("forecast.rs"),
    include_str!("outcome.rs"),
    include_str!("replay.rs"),
    include_str!("retention.rs"),
    include_str!("review.rs"),
    include_str!("run.rs"),
    include_str!("schema.rs"),
    include_str!("score.rs"),
    include_str!("shadow_policy.rs"),
    include_str!("shadow_validate.rs"),
    include_str!("sla_forecast.rs"),
    include_str!("sla_score.rs"),
    include_str!("snapshot.rs"),
    include_str!("test_support.rs"),
    include_str!("types.rs"),
    include_str!("types_run.rs"),
    include_str!("validation.rs"),
    include_str!("tests/mod.rs"),
    include_str!("tests/causal.rs"),
    include_str!("tests/observed.rs"),
    include_str!("tests/outcome.rs"),
    include_str!("tests/replay.rs"),
    include_str!("tests/shadow_forecast.rs"),
    include_str!("tests/shadow_reserve.rs"),
    include_str!("tests/shadow_screen.rs"),
    include_str!("tests/shadow_window.rs"),
    include_str!("tests/store_basics.rs"),
];

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "../decision_c7_synthetic_harness.rs"]
pub(crate) mod c7_synthetic_shadow_harness;

#[cfg(test)]
mod tests;
