//! Version-bound workflows share one durable run and the existing operation ledger.
pub mod activation;
pub mod artifact;
pub mod cost_ledger;
pub mod effect_targets;
mod boot_reconcile;
pub mod executor;
pub mod handoff;
pub mod queue_task;
pub mod run_control;
pub mod runner;
pub mod schema;
pub mod service;
pub mod staging;
pub mod store;
pub mod suspension;
pub mod workflow_notify;
pub use activation::*;
pub use boot_reconcile::BootReconcileReport;
pub use handoff::{DispatchOutcome, SweepReport};
pub use run_control::{consecutive_failure_sql, resume_outbox};
pub use schema::*;
pub use service::WorkflowService;
pub use store::WorkflowStore;
#[cfg(test)]
mod tests;

pub mod draft_store;

#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod future_size_tests;
#[cfg(test)]
mod run_control_tests;
#[cfg(test)]
mod f1b_unit_tests;
#[cfg(test)]
mod f5a_unit_tests;

#[cfg(test)]
pub(crate) mod pilot_test_factory;
#[cfg(test)]
mod pilot_tests;
#[cfg(test)]
mod resume_tests;
#[cfg(test)]
mod f1b_e2e_tests;
#[cfg(test)]
mod f5a_e2e_tests;

#[cfg(test)]
mod security_race_tests;
