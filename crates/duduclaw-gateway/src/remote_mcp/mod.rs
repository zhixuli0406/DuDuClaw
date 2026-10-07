//! Native remote MCP servers (Streamable HTTP) with bearer or OAuth 2.1
//! sign-in, replacing the `npx mcp-remote` bridge.
//!
//! - [`store`]: per-(employee, server) records, secrets encrypted with the
//!   per-machine keyfile, file 0600 under a cross-process lock.
//! - [`oauth`]: RFC 9728 / RFC 8414 discovery, RFC 7591 registration,
//!   authorization code + PKCE S256 with RFC 8707 `resource`, refresh with
//!   rotation.
//! - [`connect`]: the dashboard flows (connect, callback, disconnect, status)
//!   and the token hand-off to the bridge (refresh under a lock).
//! - [`bridge`]: the stdio ⇄ Streamable HTTP forwarder behind the hidden
//!   `duduclaw mcp-remote-bridge` subcommand.
//! - [`bridge_def`]: the `.mcp.json` entry (`<duduclaw> mcp-remote-bridge
//!   --agent <id> --server <name>`; no URL or token in argv or env).
//! - [`url_policy`] / [`http`]: https-only (loopback http for local servers),
//!   public-address screening with pinned resolution, bounded bodies, no
//!   unchecked redirects.
//!
//! Operator guide: `docs/guides/mcp-ecosystem.md`.

pub mod bridge;
pub mod bridge_def;
pub mod connect;
pub mod http;
pub mod oauth;
pub mod store;
pub mod url_policy;

/// Audit event types written by the dashboard flows.
pub const AUDIT_CONNECT_STARTED: &str = "remote_mcp_connect_started";
pub const AUDIT_CONNECTED: &str = "remote_mcp_connected";
pub const AUDIT_CONNECT_FAILED: &str = "remote_mcp_connect_failed";
pub const AUDIT_DISCONNECTED: &str = "remote_mcp_disconnected";

/// Write one audit row (agent id, server, host; never URL paths or tokens).
pub fn audit(home: &std::path::Path, event: &str, agent_id: &str, details: serde_json::Value) {
    let severity = if event == AUDIT_CONNECT_FAILED {
        duduclaw_security::audit::Severity::Warning
    } else {
        duduclaw_security::audit::Severity::Info
    };
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(event, agent_id, severity, details),
    );
}
