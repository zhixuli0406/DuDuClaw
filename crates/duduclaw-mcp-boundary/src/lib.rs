//! Package D: connection-bound authority; no identity is accepted from MCP data.
pub mod authority;
pub mod io_authority;
pub mod config;
pub mod execution;
pub mod execution_jobs;
pub mod fork_policy;
#[cfg(all(target_os = "linux", feature = "execution-integration"))]
pub mod protected_launch;
#[cfg(all(target_os = "linux", feature = "execution-integration"))]
pub mod work_bridge;
pub mod factories;
pub mod worker_execution;
pub mod resources;
#[cfg(feature = "binding-store")]
pub mod forks;
pub mod parsing;
pub mod tool_policy;
pub mod server;
#[cfg(feature = "binding-store")]
pub mod bindings;
#[cfg(target_os = "linux")]
pub mod socket;

pub use authority::{Boundary, Connection, GrantPolicy, LiveAuthority, LiveSnapshot, Reservation};
pub use duduclaw_isolation::capability::{RequestContext, VerificationError};

/// Identity/path authority is host-owned. Ordinary tool data is not recursively
/// inspected: a document containing these words is still just a document.
pub fn check_arguments(arguments: &serde_json::Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    if ["agent_id", "agent", "principal", "root", "cwd", "run", "run_id", "employee_id"]
        .iter().any(|key| object.contains_key(*key)) {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

#[cfg(feature = "binding-store")]
pub mod fork_queue;
