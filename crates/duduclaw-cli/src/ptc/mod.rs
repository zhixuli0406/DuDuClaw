//! PTC (Process-to-Claude) — sandbox script execution with MCP tool access.
//!
//! Provides two execution modes:
//! - Direct subprocess (`PtcSandbox::execute`)
//! - Container-isolated (`PtcSandbox::execute_in_container`) with fallback
//!
//! Scripts communicate with the host via a Unix Domain Socket RPC server
//! (`PtcRpcServer`) to invoke MCP tools.

pub mod rpc_bridge;
pub mod router;
pub mod sandbox;
pub mod types;

pub use sandbox::{PtcRpcServer, PtcSandbox};
pub use rpc_bridge::PtcUdsServer;
pub use types::{ScriptLanguage, ScriptRequest, ScriptResult};
