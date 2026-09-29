//! The per-call [`ToolInterceptor`] hook (RFC-23 §13.6).
//! Moved verbatim out of `tool_loop.rs`.

use super::*;

// ---------------------------------------------------------------------------
// Tool interceptor (RFC-23 §13.6 — redaction for the non-CLI tool surface)
// ---------------------------------------------------------------------------

/// What an interceptor decided about a pending tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum InterceptDecision {
    /// Dispatch with these arguments (possibly rewritten — e.g. `<REDACT:…>`
    /// tokens restored to real values for a whitelisted tool).
    Allow(Value),
    /// Do not dispatch. The reason is fed back to the model as an `is_error`
    /// tool result so it can re-plan, exactly like a policy denial.
    Deny(String),
}

/// A per-call hook around tool dispatch, sitting *inside* the loop so it
/// covers every provider.
///
/// This is the direct-API twin of the MCP choke point: the CLI backends get
/// redaction from `duduclaw mcp-server` / `duduclaw mcp-proxy`, but a model
/// driven through [`run_tool_loop`] talks to the `ToolRegistry` in-process,
/// where no such choke point exists. An interceptor closes that gap without
/// the loop knowing anything about redaction.
///
/// `server` is `""` when the executor cannot attribute the tool to a server
/// (see [`ToolExecutor::server_of`]) — implementations must treat that as
/// "unknown", never as a server literally named the empty string.
///
/// Both hooks are synchronous on purpose: they run between two provider
/// round-trips on the loop's own task, and the RFC-23 pipeline they wrap is
/// itself synchronous (SQLite vault + in-memory rule engine).
pub trait ToolInterceptor: Send + Sync {
    /// Called before dispatch. Returning [`InterceptDecision::Deny`] means the
    /// tool is **never invoked**.
    fn before_call(&self, server: &str, tool: &str, args: Value) -> InterceptDecision;

    /// Called after a dispatched tool returned, with the parsed result so
    /// structured (JSON-path) rules can see keys rather than raw text.
    ///
    /// `result` is the tool's `content` parsed as JSON when it parses, and
    /// `Value::String(content)` otherwise; the loop converts whatever is left
    /// in it back into the string the model sees.
    fn after_call(&self, server: &str, tool: &str, args: &Value, result: &mut Value);
}
