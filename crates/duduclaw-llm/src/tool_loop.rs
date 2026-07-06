//! Provider-agnostic agentic tool-use loop.
//!
//! When an agent routes to the direct-API path (`openai` / `gemini` /
//! `anthropic` providers) or local inference, it has no CLI backend to broker
//! MCP tools. This module closes that gap: given any [`ChatProvider`] and a
//! [`ToolExecutor`] (the MCP-backed [`crate::ToolRegistry`] in production, a
//! mock in tests), [`run_tool_loop`] drives the model → tool → model cycle
//! until the model stops asking for tools.
//!
//! The loop is deliberately decoupled from MCP: it depends only on the
//! [`ToolExecutor`] trait, so it is fully unit-testable offline without
//! spawning child processes or making HTTP calls.
//!
//! ## Contract
//!
//! 1. If `req.tools` is empty it is seeded from `tools.defs()`; a caller that
//!    pre-populated `req.tools` keeps control.
//! 2. Each turn calls `provider.complete(&req)`. On [`StopReason::ToolUse`]
//!    every [`ContentPart::ToolCall`] is dispatched through the executor, the
//!    assistant turn (verbatim, preserving [`ContentPart::Reasoning`]
//!    signatures for replay) plus a `User` turn of matching
//!    [`ContentPart::ToolResult`] parts are appended, and the loop repeats.
//! 3. Any other stop reason returns the response as-is.
//! 4. **Guard rails.** At most `max_iters` (default via
//!    [`DEFAULT_MAX_TOOL_ITERS`]) tool-dispatch rounds run; on exhaustion the
//!    last response is returned with its stop reason rewritten to
//!    `StopReason::Other("max_tool_iters")`. A per-call executor error is fed
//!    back as a `ToolResult { is_error: true }` so the model can recover — it
//!    never aborts the whole loop (fail-soft for tools, fail-closed only on
//!    provider transport errors, which propagate).

use async_trait::async_trait;
use serde_json::Value;

use crate::error::LlmError;
use crate::provider::ChatProvider;
use crate::types::{ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, StopReason};
use crate::types::ToolDef;

/// Default cap on tool-dispatch rounds before the loop gives up.
pub const DEFAULT_MAX_TOOL_ITERS: usize = 10;

/// Stop-reason marker set when the iteration cap is hit.
pub const MAX_ITERS_STOP: &str = "max_tool_iters";

/// The outcome of one tool invocation, as seen by the loop.
///
/// `is_error` maps straight onto [`ContentPart::ToolResult::is_error`] — a
/// tool that ran but failed (validation, upstream 500, `isError` from an MCP
/// server) sets `is_error = true` while still returning descriptive
/// `content`, so the model gets a chance to react rather than the loop
/// aborting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

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
}

/// Extract the `(id, name, args)` of every tool call in a response, in order.
fn tool_calls_of(resp: &ChatResponse) -> Vec<(String, String, Value)> {
    resp.parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolCall { id, name, args } => {
                Some((id.clone(), name.clone(), args.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Run the provider-agnostic agentic tool-use loop. See the module docs for
/// the full contract.
pub async fn run_tool_loop(
    provider: &dyn ChatProvider,
    mut req: ChatRequest,
    tools: &dyn ToolExecutor,
    max_iters: usize,
) -> Result<ChatResponse, LlmError> {
    // Seed the tool schemas unless the caller supplied their own.
    if req.tools.is_empty() {
        req.tools = tools.defs();
    }
    // A zero cap is meaningless; clamp to at least one round.
    let cap = max_iters.max(1);

    let mut last = provider.complete(&req).await?;

    for _ in 0..cap {
        if last.stop != StopReason::ToolUse {
            return Ok(last);
        }
        let calls = tool_calls_of(&last);
        if calls.is_empty() {
            // Model signalled ToolUse but emitted no ToolCall parts — nothing
            // to dispatch; surface as-is rather than spin.
            return Ok(last);
        }

        // Echo the assistant turn verbatim (keeps Reasoning signatures for
        // providers that require thinking replay), then answer with results.
        req.messages
            .push(ChatMessage { role: Role::Assistant, parts: last.parts.clone() });

        let mut result_parts = Vec::with_capacity(calls.len());
        for (id, name, args) in calls {
            let (content, is_error) = match tools.call(&name, args).await {
                Ok(outcome) => (outcome.content, outcome.is_error),
                // Dispatch failure → feed back as an error result, not a loop
                // abort, so the model can pick a different tool.
                Err(reason) => (format!("tool dispatch failed: {reason}"), true),
            };
            result_parts.push(ContentPart::ToolResult { call_id: id, content, is_error });
        }
        req.messages
            .push(ChatMessage { role: Role::User, parts: result_parts });

        last = provider.complete(&req).await?;
    }

    // Cap exhausted while still asking for tools: return the last response but
    // flag it so callers don't mistake it for a clean end-of-turn.
    if last.stop == StopReason::ToolUse {
        last.stop = StopReason::Other(MAX_ITERS_STOP.to_string());
    }
    Ok(last)
}

// ---------------------------------------------------------------------------
// PolicyKernel enforcement decorator (P1-4)
// ---------------------------------------------------------------------------

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
        Self { inner, policy, agent_id }
    }
}

#[async_trait]
impl ToolExecutor for PolicyExecutor<'_> {
    fn defs(&self) -> Vec<ToolDef> {
        self.inner.defs()
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String> {
        use duduclaw_security::policy_kernel::{evaluate, Decision, ToolCallEvent};
        let event = ToolCallEvent { tool_name: name, arguments: &args, agent_id: self.agent_id };
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

// ---------------------------------------------------------------------------
// Tests — offline, mock provider + mock executor, no processes / HTTP.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmError;
    use crate::types::{NormalizedUsage, StreamEvent};
    use futures_util::stream::BoxStream;
    use std::sync::Mutex;

    /// A provider that replays a canned script of responses and records every
    /// request it received (so tests can assert what was fed back).
    struct ScriptedProvider {
        script: Mutex<std::collections::VecDeque<ChatResponse>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                script: Mutex::new(responses.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
        fn last_request(&self) -> ChatRequest {
            self.seen.lock().unwrap().last().cloned().unwrap()
        }
    }

    #[async_trait]
    impl ChatProvider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
            self.seen.lock().unwrap().push(req.clone());
            // If the script runs dry, keep returning the final entry so a
            // runaway loop is bounded by max_iters, not by a panic.
            let mut s = self.script.lock().unwrap();
            if s.len() > 1 {
                Ok(s.pop_front().unwrap())
            } else {
                Ok(s.front().cloned().unwrap())
            }
        }
        async fn stream(
            &self,
            _req: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::InvalidRequest("stream unused in tests".into()))
        }
    }

    /// A tool executor with a scripted response and a call counter.
    struct MockExecutor {
        defs: Vec<ToolDef>,
        behavior: MockBehavior,
        calls: Mutex<Vec<(String, Value)>>,
    }

    enum MockBehavior {
        Ok(String),
        Error(String),
        Dispatch(String),
    }

    impl MockExecutor {
        fn new(behavior: MockBehavior) -> Self {
            Self {
                defs: vec![ToolDef {
                    name: "search".into(),
                    description: "search the web".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                }],
                behavior,
                calls: Mutex::new(Vec::new()),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ToolExecutor for MockExecutor {
        fn defs(&self) -> Vec<ToolDef> {
            self.defs.clone()
        }
        async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String> {
            self.calls.lock().unwrap().push((name.to_string(), args));
            match &self.behavior {
                MockBehavior::Ok(s) => Ok(ToolOutcome::ok(s.clone())),
                MockBehavior::Error(s) => Ok(ToolOutcome::error(s.clone())),
                MockBehavior::Dispatch(s) => Err(s.clone()),
            }
        }
    }

    fn tool_use_resp(id: &str, name: &str) -> ChatResponse {
        ChatResponse {
            parts: vec![ContentPart::ToolCall {
                id: id.into(),
                name: name.into(),
                args: serde_json::json!({"q": "rust"}),
            }],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        }
    }

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            parts: vec![ContentPart::Text(text.into())],
            stop: StopReason::EndTurn,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        }
    }

    #[tokio::test]
    async fn dispatches_tool_then_terminates_on_end_turn() {
        let provider = ScriptedProvider::new(vec![
            tool_use_resp("call-1", "search"),
            final_resp("here is the answer"),
        ]);
        let exec = MockExecutor::new(MockBehavior::Ok("result payload".into()));
        let req = ChatRequest::new("anthropic/claude-haiku-4-5");

        let resp = run_tool_loop(&provider, req, &exec, DEFAULT_MAX_TOOL_ITERS)
            .await
            .unwrap();

        assert_eq!(resp.text(), "here is the answer");
        assert_eq!(resp.stop, StopReason::EndTurn);
        assert_eq!(exec.call_count(), 1);
        // Provider called twice: initial + after tool result.
        assert_eq!(provider.calls(), 2);

        // The last request must carry the echoed assistant tool-call turn plus
        // a matching User tool-result turn.
        let last = provider.last_request();
        assert_eq!(last.messages.len(), 2);
        assert_eq!(last.messages[0].role, Role::Assistant);
        assert!(matches!(last.messages[0].parts[0], ContentPart::ToolCall { .. }));
        assert_eq!(last.messages[1].role, Role::User);
        match &last.messages[1].parts[0] {
            ContentPart::ToolResult { call_id, content, is_error } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(content, "result payload");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn seeds_tools_from_executor_when_request_has_none() {
        let provider = ScriptedProvider::new(vec![final_resp("done")]);
        let exec = MockExecutor::new(MockBehavior::Ok("x".into()));
        let req = ChatRequest::new("m");
        assert!(req.tools.is_empty());

        run_tool_loop(&provider, req, &exec, 5).await.unwrap();

        let seen = provider.last_request();
        assert_eq!(seen.tools.len(), 1);
        assert_eq!(seen.tools[0].name, "search");
    }

    #[tokio::test]
    async fn max_iters_exhaustion_returns_marker() {
        // Provider always asks for a tool → loop can never end naturally.
        let provider = ScriptedProvider::new(vec![tool_use_resp("c", "search")]);
        let exec = MockExecutor::new(MockBehavior::Ok("again".into()));
        let req = ChatRequest::new("m");

        let resp = run_tool_loop(&provider, req, &exec, 2).await.unwrap();

        assert_eq!(resp.stop, StopReason::Other(MAX_ITERS_STOP.into()));
        // Exactly `max_iters` dispatch rounds executed.
        assert_eq!(exec.call_count(), 2);
        // Provider called 1 (initial) + 2 (per round) = 3 times.
        assert_eq!(provider.calls(), 3);
    }

    #[tokio::test]
    async fn tool_error_outcome_feeds_is_error_and_continues() {
        let provider = ScriptedProvider::new(vec![
            tool_use_resp("call-e", "search"),
            final_resp("recovered"),
        ]);
        let exec = MockExecutor::new(MockBehavior::Error("upstream 500".into()));
        let req = ChatRequest::new("m");

        let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();

        assert_eq!(resp.text(), "recovered");
        let last = provider.last_request();
        match &last.messages[1].parts[0] {
            ContentPart::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert_eq!(content, "upstream 500");
            }
            other => panic!("expected error ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_failure_is_fed_back_not_aborted() {
        let provider = ScriptedProvider::new(vec![
            tool_use_resp("call-x", "missing"),
            final_resp("ok anyway"),
        ]);
        let exec = MockExecutor::new(MockBehavior::Dispatch("unknown tool: missing".into()));
        let req = ChatRequest::new("m");

        let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();

        assert_eq!(resp.text(), "ok anyway");
        let last = provider.last_request();
        match &last.messages[1].parts[0] {
            ContentPart::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("tool dispatch failed"));
                assert!(content.contains("unknown tool"));
            }
            other => panic!("expected error ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_use_without_calls_returns_immediately() {
        // A ToolUse stop reason with only text parts must not spin the loop.
        let odd = ChatResponse {
            parts: vec![ContentPart::Text("thinking...".into())],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        };
        let provider = ScriptedProvider::new(vec![odd]);
        let exec = MockExecutor::new(MockBehavior::Ok("unused".into()));
        let req = ChatRequest::new("m");

        let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();
        assert_eq!(resp.stop, StopReason::ToolUse);
        assert_eq!(exec.call_count(), 0);
        assert_eq!(provider.calls(), 1);
    }

    // ── P1-4: PolicyExecutor decorator ────────────────────────────────────────

    #[tokio::test]
    async fn policy_executor_denies_forbidden_tool_without_calling_inner() {
        use duduclaw_core::types::{PolicyEffect, ToolPolicy};
        let inner = MockExecutor::new(MockBehavior::Ok("should not run".into()));
        let policy = vec![ToolPolicy {
            tool: "search".into(),
            effect: PolicyEffect::Forbid,
            when: vec![],
        }];
        let guarded = PolicyExecutor::new(&inner, &policy, "agent-x");

        let out = guarded.call("search", serde_json::json!({"q": "x"})).await.unwrap();
        assert!(out.is_error, "forbidden tool must return an error outcome");
        assert!(out.content.contains("blocked by policy"), "got: {}", out.content);
        assert_eq!(inner.call_count(), 0, "forbidden tool must not reach inner");
    }

    #[tokio::test]
    async fn policy_executor_passes_allowed_tool_through() {
        use duduclaw_core::types::ToolPolicy;
        let inner = MockExecutor::new(MockBehavior::Ok("ran".into()));
        // Empty policy → kernel abstains → passthrough.
        let policy: Vec<ToolPolicy> = vec![];
        let guarded = PolicyExecutor::new(&inner, &policy, "agent-x");

        let out = guarded.call("search", serde_json::json!({"q": "x"})).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "ran");
        assert_eq!(inner.call_count(), 1);
    }

    #[tokio::test]
    async fn policy_executor_ask_is_fail_closed_refusal() {
        use duduclaw_core::types::{PolicyEffect, ToolPolicy};
        let inner = MockExecutor::new(MockBehavior::Ok("should not run".into()));
        let policy = vec![ToolPolicy {
            tool: "search".into(),
            effect: PolicyEffect::Ask,
            when: vec![],
        }];
        let guarded = PolicyExecutor::new(&inner, &policy, "agent-x");

        let out = guarded.call("search", serde_json::json!({"q": "x"})).await.unwrap();
        assert!(out.is_error, "Ask with no approver must fail closed");
        assert!(out.content.contains("approval required"), "got: {}", out.content);
        assert_eq!(inner.call_count(), 0);
    }

    #[tokio::test]
    async fn policy_executor_integrates_with_run_tool_loop() {
        use duduclaw_core::types::{PolicyEffect, ToolPolicy};
        // Model asks for `search`, policy forbids it → the loop feeds an error
        // result back and the model then ends the turn. Inner never runs.
        let provider = ScriptedProvider::new(vec![
            tool_use_resp("call-1", "search"),
            final_resp("ok, I won't use that tool"),
        ]);
        let inner = MockExecutor::new(MockBehavior::Ok("secret".into()));
        let policy = vec![ToolPolicy {
            tool: "search".into(),
            effect: PolicyEffect::Forbid,
            when: vec![],
        }];
        let guarded = PolicyExecutor::new(&inner, &policy, "agent-x");
        let req = ChatRequest::new("m");

        let resp = run_tool_loop(&provider, req, &guarded, DEFAULT_MAX_TOOL_ITERS)
            .await
            .unwrap();
        assert_eq!(resp.text(), "ok, I won't use that tool");
        assert_eq!(inner.call_count(), 0, "forbidden tool must never dispatch");
    }
}
