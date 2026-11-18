//! Boot injection for Package D. No default launcher/backend and no uid fallback.
pub use duduclaw_mcp_boundary::factories::GatewayFactories;

/// Package E's worker integration is always compiled.
/// The protected gateway boot calls install_worker exactly once,
/// before constructing dispatchers or admitting any employee run.
#[path = "isolation_execution_worker.rs"]
mod worker;
pub use worker::{install_worker, install_worker_services, InstalledWorkerServices};

#[cfg(target_os = "linux")]
#[path = "isolation_execution_program.rs"]
mod program;
#[cfg(target_os = "linux")]
pub use program::InstalledProgramPlans;
