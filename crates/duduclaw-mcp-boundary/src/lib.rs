//! Package D: connection-bound authority; no identity is accepted from MCP data.
pub mod authority;
pub mod config;
pub mod execution;
pub mod factories;
pub mod worker_execution;
pub mod resources;
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
