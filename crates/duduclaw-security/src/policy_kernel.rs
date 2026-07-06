//! PolicyKernel — deterministic, zero-LLM reference monitor for tool calls.
//!
//! This is the L3 choke point of the runtime-agnostic security redesign
//! (`commercial/docs/TODO-security-runtime-agnostic-redesign.md`). Every
//! runtime's tool call — Claude / codex / gemini / antigravity via MCP dispatch,
//! plus the direct-API / local-inference tool loop — is evaluated here BEFORE
//! the tool executes.
//!
//! Invariants (from the design):
//!   - **I1**: [`evaluate`] is pure, synchronous, zero-LLM — no I/O, no network,
//!     no clock. A given `(event, policies)` always yields the same [`Decision`].
//!   - **I3**: complete mediation — every path routes tool calls through this
//!     single function.
//!   - **I5**: fail-closed — when a policy set is active, a call matching no
//!     `allow` rule is denied.
//!   - Tool names are canonicalized so a rule written for `shell_exec` applies
//!     to Bash / shell / run_shell_command / run_command uniformly.

use serde_json::Value;

use duduclaw_core::types::{ArgCondition, ArgOp, PolicyEffect, ToolPolicy};

/// The decision returned by [`evaluate`].
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Permit the call unchanged.
    Allow,
    /// Permit the call, but with rewritten arguments.
    ///
    /// Reserved for argument sanitization / the P2 dynamic policy layer; the
    /// static kernel does not currently emit it, but P1-2's dispatch handling
    /// honours it so the plumbing is ready.
    AllowRewritten(Value),
    /// Block the call.
    Deny { reason: String },
    /// Escalate the call to a human approval (ApprovalBroker) before proceeding.
    Ask { risk: String },
}

/// One tool call to evaluate.
#[derive(Debug, Clone)]
pub struct ToolCallEvent<'a> {
    /// Runtime/tool name as invoked (e.g. "Bash", "apply_patch", "memory_store").
    pub tool_name: &'a str,
    /// The tool's `arguments` object (JSON). `Value::Null` when absent.
    pub arguments: &'a Value,
    /// Invoking agent id (for audit / future per-agent policy).
    pub agent_id: &'a str,
}

/// Map a runtime-specific tool name to its canonical family.
///
/// Three families (design §1): `fs_write`, `shell_exec`, `mcp_call`. Anything
/// not a known native file-write or shell tool is treated as `mcp_call` — in
/// every runtime the non-native tools reach the platform through MCP, so a rule
/// written for `mcp_call` is a catch-all for MCP-exposed tools while an exact
/// tool name (`memory_store`) still targets one specific tool.
pub fn canonical_tool(name: &str) -> &'static str {
    match name {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" | "write_file"
        | "write_to_file" => "fs_write",
        "Bash" | "shell" | "run_shell_command" | "run_command" => "shell_exec",
        _ => "mcp_call",
    }
}

/// Does this rule's `tool` selector apply to the event's tool?
///
/// Matches on `"*"` (any), exact name, or canonical family.
fn rule_matches_tool(rule: &ToolPolicy, event_tool: &str) -> bool {
    rule.tool == "*" || rule.tool == event_tool || rule.tool == canonical_tool(event_tool)
}

/// Stringify a JSON argument value for comparison (strings verbatim, others via
/// their JSON representation).
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Do ALL of a rule's argument conditions hold (logical AND)? Empty → true.
fn arg_conditions_match(conds: &[ArgCondition], args: &Value) -> bool {
    conds.iter().all(|c| {
        let actual = args.get(&c.arg).map(value_to_string).unwrap_or_default();
        match c.op {
            ArgOp::Equals => actual == c.value,
            ArgOp::Contains => actual.contains(&c.value),
            ArgOp::StartsWith => actual.starts_with(&c.value),
        }
    })
}

/// Whether a rule matches an event (tool selector AND all arg conditions).
fn rule_matches(rule: &ToolPolicy, event: &ToolCallEvent) -> bool {
    rule_matches_tool(rule, event.tool_name) && arg_conditions_match(&rule.when, event.arguments)
}

/// Evaluate a tool call against a static policy set (Progent-style).
///
/// Precedence: `Forbid` > `Ask` > `Allow`. With an active (non-empty) policy
/// set, a call matching no `Allow` rule is denied (fail-closed, I5). An empty
/// policy set means the kernel abstains ([`Decision::Allow`]) and the other
/// enforcement layers (scope check, injection scan, `denied_tools`) still apply.
pub fn evaluate(event: &ToolCallEvent, policies: &[ToolPolicy]) -> Decision {
    if policies.is_empty() {
        return Decision::Allow;
    }

    // 1. Forbid wins outright.
    if policies
        .iter()
        .any(|r| r.effect == PolicyEffect::Forbid && rule_matches(r, event))
    {
        return Decision::Deny {
            reason: format!("tool '{}' matched a forbid rule", event.tool_name),
        };
    }

    // 2. Ask escalates (more restrictive than Allow).
    if policies
        .iter()
        .any(|r| r.effect == PolicyEffect::Ask && rule_matches(r, event))
    {
        return Decision::Ask {
            risk: format!("tool '{}' requires approval by policy", event.tool_name),
        };
    }

    // 3. Allow permits.
    if policies
        .iter()
        .any(|r| r.effect == PolicyEffect::Allow && rule_matches(r, event))
    {
        return Decision::Allow;
    }

    // 4. Active policy set, nothing matched → fail-closed deny (I5).
    Decision::Deny {
        reason: format!(
            "tool '{}' matched no allow rule (default-deny under active policy)",
            event.tool_name
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(tool: &str, effect: PolicyEffect, when: Vec<ArgCondition>) -> ToolPolicy {
        ToolPolicy { tool: tool.to_string(), effect, when }
    }

    fn cond(arg: &str, op: ArgOp, value: &str) -> ArgCondition {
        ArgCondition { arg: arg.to_string(), op, value: value.to_string() }
    }

    fn event<'a>(tool: &'a str, args: &'a Value) -> ToolCallEvent<'a> {
        ToolCallEvent { tool_name: tool, arguments: args, agent_id: "test-agent" }
    }

    #[test]
    fn empty_policy_abstains_with_allow() {
        let args = json!({});
        assert_eq!(evaluate(&event("Bash", &args), &[]), Decision::Allow);
    }

    #[test]
    fn active_policy_default_denies_unmatched() {
        // A policy set that only allows fs_write must deny an unrelated shell call.
        let policies = vec![policy("fs_write", PolicyEffect::Allow, vec![])];
        let args = json!({ "command": "ls" });
        assert!(matches!(
            evaluate(&event("Bash", &args), &policies),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn forbid_beats_allow() {
        let policies = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy("shell_exec", PolicyEffect::Forbid, vec![]),
        ];
        let args = json!({ "command": "ls" });
        assert!(matches!(
            evaluate(&event("run_shell_command", &args), &policies),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn ask_beats_allow_but_not_forbid() {
        let allow_and_ask = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy("mcp_call", PolicyEffect::Ask, vec![]),
        ];
        let args = json!({});
        assert!(matches!(
            evaluate(&event("memory_store", &args), &allow_and_ask),
            Decision::Ask { .. }
        ));

        let forbid_and_ask = vec![
            policy("mcp_call", PolicyEffect::Ask, vec![]),
            policy("memory_store", PolicyEffect::Forbid, vec![]),
        ];
        assert!(matches!(
            evaluate(&event("memory_store", &args), &forbid_and_ask),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn canonical_family_matches_all_runtime_spellings() {
        assert_eq!(canonical_tool("Write"), "fs_write");
        assert_eq!(canonical_tool("apply_patch"), "fs_write");
        assert_eq!(canonical_tool("write_to_file"), "fs_write");
        assert_eq!(canonical_tool("Bash"), "shell_exec");
        assert_eq!(canonical_tool("run_command"), "shell_exec");
        assert_eq!(canonical_tool("memory_store"), "mcp_call");

        // One `shell_exec` forbid covers every runtime's shell tool.
        let policies = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy("shell_exec", PolicyEffect::Forbid, vec![]),
        ];
        let args = json!({});
        for t in ["Bash", "shell", "run_shell_command", "run_command"] {
            assert!(
                matches!(evaluate(&event(t, &args), &policies), Decision::Deny { .. }),
                "{t} should be forbidden via canonical shell_exec"
            );
        }
    }

    #[test]
    fn arg_condition_contains_matches() {
        let policies = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy(
                "shell_exec",
                PolicyEffect::Forbid,
                vec![cond("command", ArgOp::Contains, "rm -rf")],
            ),
        ];
        let danger = json!({ "command": "sudo rm -rf /" });
        let benign = json!({ "command": "ls -la" });
        assert!(matches!(
            evaluate(&event("Bash", &danger), &policies),
            Decision::Deny { .. }
        ));
        // Benign shell call: not forbidden, and the `*` allow permits it.
        assert_eq!(evaluate(&event("Bash", &benign), &policies), Decision::Allow);
    }

    #[test]
    fn arg_condition_equals_and_starts_with() {
        let policies = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy(
                "mcp_call",
                PolicyEffect::Forbid,
                vec![cond("path", ArgOp::StartsWith, "/etc")],
            ),
        ];
        let sys = json!({ "path": "/etc/passwd" });
        let ok = json!({ "path": "/home/user/x" });
        assert!(matches!(
            evaluate(&event("fs_read", &sys), &policies),
            Decision::Deny { .. }
        ));
        assert_eq!(evaluate(&event("fs_read", &ok), &policies), Decision::Allow);
    }

    #[test]
    fn missing_arg_does_not_match_condition() {
        // A forbid conditioned on an absent arg must NOT fire (empty ≠ value).
        let policies = vec![
            policy("*", PolicyEffect::Allow, vec![]),
            policy(
                "shell_exec",
                PolicyEffect::Forbid,
                vec![cond("command", ArgOp::Contains, "rm")],
            ),
        ];
        let no_command = json!({ "other": "x" });
        assert_eq!(
            evaluate(&event("Bash", &no_command), &policies),
            Decision::Allow
        );
    }
}
