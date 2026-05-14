//! MCP-layer integration of the RFC-23 redaction pipeline.
//!
//! Two concerns:
//!
//! 1. **Outgoing (tool result → LLM)**: tool result JSON returned from
//!    `handle_tools_call` is walked; every string value is run through
//!    [`RedactionPipeline::redact`] with `Source::ToolResult { tool_name }`.
//!    The vault stores `(agent_id, session_id, token)` keyed on the values
//!    so the gateway's channel-reply layer can later restore them.
//!
//! 2. **Incoming (tool args restoration)**: before a tool is executed,
//!    arguments that contain `<REDACT:...>` tokens are decided by
//!    [`EgressEvaluator`]. Whitelisted tools get real values; non-whitelisted
//!    tools (or args containing hallucinated tokens) are denied.
//!
//! Both paths key off two env vars set by the gateway when it spawns the
//! Claude CLI subprocess: `DUDUCLAW_AGENT_ID` and `DUDUCLAW_SESSION_ID`.
//! If either is missing the integration falls back to a sensible default
//! (default agent / "mcp-session") but cross-layer restoration may not
//! work end-to-end in that case.

use std::sync::Arc;

use duduclaw_redaction::{
    EgressDecision, RedactionConfig, RedactionManager, RestoreScope, Source,
};
use serde_json::Value;

/// Per-MCP-server-process redaction state.
///
/// Built once at server startup from `config.toml [redaction]`. `None` ⇒
/// pipeline disabled at this layer (zero overhead path).
pub struct McpRedactionLayer {
    pub manager: Arc<RedactionManager>,
    pub agent_id: String,
    pub session_id: String,
}

impl McpRedactionLayer {
    /// Try to build the layer. Returns `Ok(None)` when redaction is not
    /// enabled in `config.toml` — that's the normal "off" path and not
    /// an error. Returns `Err` only if the config explicitly enabled the
    /// pipeline but it failed to initialise — in which case the caller
    /// should fail-closed (refuse to start the MCP server) or log loudly.
    pub fn try_init(
        home_dir: &std::path::Path,
        default_agent: &str,
    ) -> Result<Option<Self>, duduclaw_redaction::RedactionError> {
        let cfg_path = home_dir.join("config.toml");
        let parsed: Option<RedactionConfig> = std::fs::read_to_string(&cfg_path)
            .ok()
            .and_then(|s| {
                #[derive(serde::Deserialize)]
                struct Wrap {
                    #[serde(default)]
                    redaction: RedactionConfig,
                }
                toml::from_str::<Wrap>(&s).ok().map(|w| w.redaction)
            });

        let Some(rcfg) = parsed.filter(|c| c.enabled) else {
            return Ok(None);
        };

        let paths = duduclaw_redaction::ManagerPaths::under_home(home_dir);
        let manager = Arc::new(RedactionManager::open(rcfg, paths)?);

        let agent_id = std::env::var(duduclaw_core::ENV_AGENT_ID)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default_agent.to_string());
        let session_id = std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "mcp-session".to_string());

        Ok(Some(Self {
            manager,
            agent_id,
            session_id,
        }))
    }

    /// Apply redaction to every string in a tool-call result Value.
    ///
    /// Walks recursively through arrays / objects. Strings get rewritten
    /// in place. Tokens hit the shared vault keyed on
    /// `(self.agent_id, self.session_id)` so the channel-reply layer can
    /// restore them when this turn's final text reaches the user.
    pub fn redact_tool_result(&self, tool_name: &str, value: &mut Value) {
        let source = Source::ToolResult {
            tool_name: tool_name.to_string(),
        };
        walk_strings(value, &mut |s| {
            // Only run through the pipeline if it actually has potential PII —
            // very cheap pre-filter, the engine itself is the source of truth.
            if s.is_empty() {
                return;
            }
            let pipeline = match self
                .manager
                .pipeline(&self.agent_id, Some(self.session_id.clone()))
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        target: "duduclaw_cli::mcp_redaction",
                        error = %e,
                        agent = %self.agent_id,
                        "redact_tool_result: pipeline build failed; passthrough"
                    );
                    return;
                }
            };
            match pipeline.redact(s, &source) {
                Ok(out) => {
                    if !out.tokens_written.is_empty() {
                        *s = out.redacted_text;
                    }
                }
                Err(e) => {
                    // Fail-closed: replace text with an explicit placeholder
                    // so the LLM cannot see raw PII on a vault write failure.
                    tracing::error!(
                        target: "duduclaw_cli::mcp_redaction",
                        error = %e,
                        agent = %self.agent_id,
                        "redact_tool_result: redact failed; emitting placeholder"
                    );
                    *s = "[redaction failed — value withheld]".to_string();
                }
            }
        });
    }

    /// Decide what to do with a tool call whose arguments may contain
    /// `<REDACT:...>` tokens.
    pub fn decide_tool_args(&self, tool_name: &str, args: &Value) -> EgressDecision {
        self.manager
            .decide_tool_call(tool_name, args, &self.agent_id, Some(&self.session_id))
            .unwrap_or_else(|e| {
                tracing::error!(
                    target: "duduclaw_cli::mcp_redaction",
                    error = %e,
                    "decide_tool_args failed; denying"
                );
                EgressDecision::Deny {
                    reason: format!("redaction error: {e}"),
                    tokens_seen: 0,
                }
            })
    }

    /// Quick scan: does any string in this Value contain a token-shaped
    /// substring? Used as a hot-path optimisation so we only invoke
    /// `decide_tool_args` when there's actually something to restore.
    pub fn args_contain_tokens(args: &Value) -> bool {
        match args {
            Value::String(s) => s.contains(duduclaw_redaction::token::TOKEN_PREFIX),
            Value::Array(arr) => arr.iter().any(Self::args_contain_tokens),
            Value::Object(map) => map.values().any(Self::args_contain_tokens),
            _ => false,
        }
    }
}

/// Recursive in-place walk over every string leaf of a `serde_json::Value`.
fn walk_strings(v: &mut Value, f: &mut dyn FnMut(&mut String)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(arr) => {
            for x in arr.iter_mut() {
                walk_strings(x, f);
            }
        }
        Value::Object(map) => {
            for x in map.values_mut() {
                walk_strings(x, f);
            }
        }
        _ => {}
    }
}

/// Convenience: produce a JSON-RPC error for a denied tool call.
pub fn egress_deny_response(id: &Value, tool: &str, reason: &str, tokens_seen: usize) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32007,
            "message": format!(
                "egress denied for '{tool}': {reason} (tokens_seen={tokens_seen})"
            ),
            "data": {
                "kind": "redaction_egress_deny",
                "tool": tool,
                "tokens_seen": tokens_seen,
            }
        }
    })
}

// silence unused-import warning when RestoreScope is referenced only in tests
#[allow(dead_code)]
fn _scope_marker(_s: &RestoreScope) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_strings_visits_nested() {
        let mut v = serde_json::json!({
            "a": "hello",
            "b": ["world", {"c": "deep"}],
        });
        let mut seen: Vec<String> = Vec::new();
        walk_strings(&mut v, &mut |s| seen.push(s.clone()));
        seen.sort();
        assert_eq!(seen, vec!["deep", "hello", "world"]);
    }

    #[test]
    fn args_contain_tokens_detects_at_depth() {
        let no = serde_json::json!({"a": "plain", "b": ["x"]});
        let yes = serde_json::json!({"a": "plain", "b": ["<REDACT:E:abcdef01>"]});
        assert!(!McpRedactionLayer::args_contain_tokens(&no));
        assert!(McpRedactionLayer::args_contain_tokens(&yes));
    }

    #[test]
    fn deny_response_has_expected_shape() {
        let r = egress_deny_response(
            &serde_json::json!(7),
            "web_fetch",
            "not whitelisted",
            2,
        );
        assert_eq!(r["error"]["code"], -32007);
        assert_eq!(r["error"]["data"]["tool"], "web_fetch");
        assert_eq!(r["error"]["data"]["tokens_seen"], 2);
    }
}
