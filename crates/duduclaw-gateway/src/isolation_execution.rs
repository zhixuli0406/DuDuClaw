//! Boot injection for Package D. No default launcher/backend and no uid fallback.
pub use duduclaw_mcp_boundary::factories::GatewayFactories;

/// Package E is present on the integration branch. The feature keeps this
/// Package D worktree buildable without importing E's implementation or schema.
/// The protected gateway boot enables it and calls install_worker exactly once,
/// before constructing dispatchers or admitting any employee run.
#[cfg(feature = "isolation-worker-integration")]
#[path = "isolation_execution_worker.rs"]
mod worker;
#[cfg(feature = "isolation-worker-integration")]
pub use worker::{install_worker, install_worker_services, InstalledWorkerServices};

#[cfg(target_os = "linux")]
#[path = "isolation_execution_program.rs"]
mod program;
#[cfg(target_os = "linux")]
pub use program::InstalledProgramPlans;
