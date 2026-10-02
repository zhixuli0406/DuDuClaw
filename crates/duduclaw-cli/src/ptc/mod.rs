//! PTC (Process-to-Claude) — sandboxed script execution (`execute_program`).
//!
//! Entry point: `PtcSandbox::run_program` — container-isolated; when the
//! container cannot run, `config.toml [container.sandbox]
//! script_when_unavailable` decides between refusing (default) and an
//! audited host subprocess (`PtcSandbox::execute`).
//!
//! Scripts cannot call MCP tools: `PtcRpcServer` is an unserved descriptor.

pub mod sandbox;
pub mod types;
