//! The [`ToolExecutor`] seam and its PolicyKernel decorator.
//! Moved verbatim out of `tool_loop.rs`.

use super::*;

/// A set of callable tools — the seam between the pure loop and the MCP
/// transport.
///
/// Production wiring is [`crate::ToolRegistry`] (aggregates N `McpClient`s);
/// tests use a mock returning canned outcomes. `call` returns `Err(String)`
/// only for a *dispatch* failure the executor could not turn into a tool
/// result (unknown tool, transport dead); the loop converts that into an
/// error `ToolResult` all the same, so a bad tool name cannot wedge the loop.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Tool definitions used to seed [`ChatRequest::tools`].
    fn defs(&self) -> Vec<ToolDef>;

    /// Dispatch one tool call by name with parsed JSON arguments.
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String>;

    /// Which MCP server owns `tool`, when the executor knows.
    ///
    /// Only used to give a [`ToolInterceptor`] the `<server>.<tool>`
    /// namespace the RFC-23 redaction rules match on — the loop itself never
    /// routes on it. Default `None` keeps every existing executor (and every
    /// mock) source-compatible; [`crate::ToolRegistry`] overrides it when it
    /// was built with server names.
    fn server_of(&self, _tool: &str) -> Option<String> {
        None
    }

    /// Recheck an executor-attested source against the exact, post-redaction
    /// bytes about to enter CCR. The default deliberately refuses a claimed
    /// binding: only a connector with an independent source authority may
    /// implement this. A registered connector is checked before and after
    /// the CCR write. A change observed at either check cannot emit a
    /// retrieval marker; connector revocation must also tombstone its CCR
    /// version before a concurrent source mutation commits.
    async fn verify_ccr_source(
        &self,
        _name: &str,
        _args: &Value,
        _content: &str,
        _artifact: &CcrSourceArtifact,
        _retention_at: Option<i64>,
    ) -> bool {
        false
    }
}

/// A [`ToolExecutor`] decorator that runs the PolicyKernel reference monitor
/// before delegating to the inner executor — bringing the direct-API and
/// local-inference tool-loop under the same deterministic policy as the MCP
/// dispatch path (invariant I3, complete mediation).
///
/// Behaviour per [`policy_kernel::Decision`]:
/// - `Allow` → delegate unchanged.
/// - `AllowRewritten(args)` → delegate with the rewritten arguments.
/// - `Deny` → do NOT dispatch; return an `is_error` [`ToolOutcome`] so the model
///   sees the refusal and can react (the loop keeps going, I5 fail-closed).
/// - `Ask` → this path has no interactive approver (no ApprovalBroker wired),
///   so an escalation is treated as a refusal (fail-closed) with an explanatory
///   error outcome.
///
/// The inner `run_tool_loop` body is untouched: pass a `PolicyExecutor` as the
/// `&dyn ToolExecutor`.
pub struct PolicyExecutor<'a> {
    inner: &'a dyn ToolExecutor,
    policy: &'a [duduclaw_core::types::ToolPolicy],
    agent_id: &'a str,
}

impl<'a> PolicyExecutor<'a> {
    pub fn new(
        inner: &'a dyn ToolExecutor,
        policy: &'a [duduclaw_core::types::ToolPolicy],
        agent_id: &'a str,
    ) -> Self {
        Self {
            inner,
            policy,
            agent_id,
        }
    }
}

#[async_trait]
impl ToolExecutor for PolicyExecutor<'_> {
    fn defs(&self) -> Vec<ToolDef> {
        self.inner.defs()
    }

    /// Delegate so a decorated registry still tells a [`ToolInterceptor`]
    /// which server owns the tool.
    fn server_of(&self, tool: &str) -> Option<String> {
        self.inner.server_of(tool)
    }

    async fn verify_ccr_source(
        &self,
        name: &str,
        args: &Value,
        content: &str,
        artifact: &CcrSourceArtifact,
        retention_at: Option<i64>,
    ) -> bool {
        // A policy rewrite will be verified against the original arguments
        // supplied to this hook and therefore fail closed unless both point
        // to the same independently checked source.
        self.inner
            .verify_ccr_source(name, args, content, artifact, retention_at)
            .await
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String> {
        use duduclaw_security::policy_kernel::{Decision, ToolCallEvent, evaluate};
        let event = ToolCallEvent {
            tool_name: name,
            arguments: &args,
            agent_id: self.agent_id,
        };
        match evaluate(&event, self.policy) {
            Decision::Allow => self.inner.call(name, args).await,
            Decision::AllowRewritten(new_args) => self.inner.call(name, new_args).await,
            Decision::Deny { reason } => {
                Ok(ToolOutcome::error(format!("blocked by policy: {reason}")))
            }
            Decision::Ask { risk } => Ok(ToolOutcome::error(format!(
                "blocked by policy (approval required, no interactive approver on this path): {risk}"
            ))),
        }
    }
}
