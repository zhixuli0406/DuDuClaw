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

/// Host-owned authority for one transport operation. Validation must be
/// read-only and idempotent: the transport calls it before each POST and after
/// receiving its body. Implementations must not refresh credentials or acquire
/// another copy of the lease while validating.
///
/// The value is owned by the ephemeral transport, not a long-lived interceptor.
/// Dropping a canceled future releases it through ordinary Rust ownership.
#[async_trait]
pub trait ToolCallLease: Send {
    async fn validate_before_send(&mut self) -> Result<(), String>;
}

/// The operation a host is authorizing. This is never deserialized from MCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientOperation<'a> {
    Discover,
    Tool { name: &'a str },
}

/// A single-use bearer owned by a scoped transport. It deliberately has no
/// Clone, Debug or serialization implementation. Drop best-effort overwrites
/// this owned buffer; HTTP-library copies are outside that guarantee.
pub struct SecretBearer {
    bytes: Vec<u8>,
}

impl SecretBearer {
    pub fn new(value: String) -> Self {
        Self {
            bytes: value.into_bytes(),
        }
    }

    #[cfg(feature = "mcp-client")]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(feature = "mcp-client")]
    pub(crate) fn redact_value(&self, value: &mut Value) {
        let Ok(secret) = std::str::from_utf8(&self.bytes) else {
            return;
        };
        if secret.is_empty() {
            return;
        }
        fn redact(value: &mut Value, secret: &str) {
            match value {
                Value::String(text) => *text = text.replace(secret, "[REDACTED]"),
                Value::Array(items) => items.iter_mut().for_each(|item| redact(item, secret)),
                Value::Object(items) => {
                    let original = std::mem::take(items);
                    for (key, mut value) in original {
                        redact(&mut value, secret);
                        items.insert(key.replace(secret, "[REDACTED]"), value);
                    }
                }
                _ => {}
            }
        }
        redact(value, secret);
    }
}

impl Drop for SecretBearer {
    fn drop(&mut self) {
        self.bytes.fill(0);
        std::hint::black_box(&mut self.bytes);
    }
}

/// Non-serializable host result for one operation. Scoped transports require
/// both authority fields; an ordinary interceptor's default cannot grant them.
pub struct PreparedClientCall {
    pub arguments: Value,
    pub lease: Option<Box<dyn ToolCallLease>>,
    pub fresh_http_bearer: Option<SecretBearer>,
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
/// The original hooks stay synchronous for the RFC-23 pipeline. A registry
/// host guard may override `before_call_async` to capture task-local authority
/// and move OS credential checks off the async worker.
#[async_trait]
pub trait ToolInterceptor: Send + Sync {
    /// Called before dispatch. Returning [`InterceptDecision::Deny`] means the
    /// tool is **never invoked**.
    fn before_call(&self, server: &str, tool: &str, args: Value) -> InterceptDecision;

    /// Registry-bound host guards may offload blocking credential checks while
    /// first capturing their caller's task-local authority. Existing hooks keep
    /// their synchronous behavior through this default implementation.
    async fn before_call_async(&self, server: &str, tool: &str, args: Value) -> InterceptDecision {
        self.before_call(server, tool, args)
    }

    /// Prepare one scoped HTTP operation. Existing interceptors remain usable
    /// for persistent clients, but cannot accidentally authorize scoped HTTP:
    /// discovery is refused and the Tool default supplies no lease or bearer.
    async fn prepare_client_call(
        &self,
        server: &str,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<PreparedClientCall, String> {
        let ClientOperation::Tool { name } = operation else {
            return Err("Scoped MCP discovery requires a host authority".into());
        };
        match self.before_call_async(server, name, args).await {
            InterceptDecision::Allow(arguments) => Ok(PreparedClientCall {
                arguments,
                lease: None,
                fresh_http_bearer: None,
            }),
            InterceptDecision::Deny(reason) => Err(reason),
        }
    }

    /// Called after a dispatched tool returned, with the parsed result so
    /// structured (JSON-path) rules can see keys rather than raw text.
    ///
    /// `result` is the tool's `content` parsed as JSON when it parses, and
    /// `Value::String(content)` otherwise; the loop converts whatever is left
    /// in it back into the string the model sees.
    fn after_call(&self, server: &str, tool: &str, args: &Value, result: &mut Value);
}
