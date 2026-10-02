//! Host-side attempt contract (design: DESIGN-discovery-runtime-parity-2026-10 §3).
//! Reads each attempt's NATIVE event stream (before normalisation, which drops
//! detail), checks every tool name against the family's allowlist and counts
//! steps for the families whose CLI has no native step ceiling. Pure: no IO.
//! A CLI's own restriction flags stay on as a second line; this is authoritative.
use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;
use super::attempt_adapter::{RuntimeFamily, GEMINI_TOOLS};

/// Codex item types that are tool executions (`codex exec --json`).
pub const CODEX_TOOL_ITEMS: &[&str] = &["command_execution", "file_change"];
/// Codex item types that are not tools. Anything else is a violation.
pub const CODEX_NON_TOOL_ITEMS: &[&str] = &["agent_message", "reasoning", "todo_list", "error"];
/// Antigravity (agy 1.2.14) file/shell tools; the hook script renders this list.
pub const ANTIGRAVITY_TOOLS: &[&str] = &["run_command", "command_status", "send_command_input", "view_file",
    "write_to_file", "replace_file_content", "multi_replace_file_content", "sed_file", "list_dir",
    "find_by_name", "grep_search", "finish", "wait", "wait_5_seconds"];
/// Grok (1.0.41) file/shell tools, passed as `--tools`.
pub const GROK_TOOLS: &[&str] = &["run_terminal_command", "read_file", "search_replace", "list_dir", "grep", "write"];
/// Tools Grok keeps in `init.tools` even when `--tools` is given (measured), passed as `--disallowed-tools`.
pub const GROK_DISALLOWED_TOOLS: &[&str] = &["todo_write", "monitor", "search_tool", "use_tool", "workflow",
    "enter_plan_mode", "exit_plan_mode", "ask_user_question", "send_feedback", "image_gen", "image_edit",
    "image_to_video", "reference_to_video", "spawn_subagent", "scheduler_create", "scheduler_delete",
    "scheduler_list", "kill_command_or_subagent", "get_command_or_subagent_output"];
/// Upper bound of a tool name inside an error message.
pub const TOOL_NAME_MAX_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict { Continue, StepLimit, ToolViolation { tool: String } }

/// Only ASCII alphanumerics and `_-.`, at most 64 bytes; empty ⇒ `unknown`.
pub fn sanitize_tool_name(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')).collect();
    let kept = duduclaw_core::truncate_bytes(&kept, TOOL_NAME_MAX_BYTES).to_owned();
    if kept.is_empty() { "unknown".into() } else { kept }
}

/// Exact prefix agy puts in `tool_info.error.message` for a hook-denied call.
pub const AGY_HOOK_DENIED_PREFIX: &str = "tool call denied by pre-tool hook";
/// Assistant content blocks that are not tool calls.
const NON_TOOL_BLOCKS: &[&str] = &["text", "thinking", "redacted_thinking"];

pub struct StreamGuard {
    family: RuntimeFamily,
    max_steps: u32,
    steps: u32,
    seen: BTreeSet<String>,
    /// agy: forbidden tool steps seen ACTIVE whose outcome is not known yet.
    pending: BTreeMap<u64, String>,
}
impl StreamGuard {
    pub fn new(family: RuntimeFamily, max_steps: u32) -> Self {
        Self { family, max_steps, steps: 0, seen: BTreeSet::new(), pending: BTreeMap::new() }
    }
    /// Steps counted so far (host-counted families only).
    pub fn steps(&self) -> u32 { self.steps }
    /// End of stream (or the agy `result`): a forbidden tool whose denial was
    /// never confirmed is treated as executed.
    pub fn finish(&mut self) -> GuardVerdict {
        match std::mem::take(&mut self.pending).into_values().next() {
            Some(tool) => violation(&tool),
            None => GuardVerdict::Continue,
        }
    }
    pub fn observe(&mut self, native: &Value) -> GuardVerdict {
        match self.family {
            RuntimeFamily::Claude => assistant_blocks(native, &|name| claude_allowed(name)),
            RuntimeFamily::Grok => {
                if native["type"].as_str() == Some("system") && native["subtype"].as_str() == Some("init")
                    && non_empty(&native["mcp_servers"]) {
                    return violation("mcp_servers");
                }
                assistant_blocks(native, &|name| GROK_TOOLS.contains(&name))
            }
            RuntimeFamily::Gemini => match native["type"].as_str() {
                Some("tool_use") => {
                    let name = native["tool_name"].as_str().unwrap_or("");
                    if GEMINI_TOOLS.contains(&name) { GuardVerdict::Continue } else { violation(name) }
                }
                _ => GuardVerdict::Continue,
            },
            RuntimeFamily::Codex => self.codex(native),
            RuntimeFamily::Antigravity => self.antigravity(native),
            RuntimeFamily::OpenAiCompat => GuardVerdict::Continue,
        }
    }
    fn codex(&mut self, native: &Value) -> GuardVerdict {
        if !matches!(native["type"].as_str(), Some("item.started" | "item.updated" | "item.completed")) { return GuardVerdict::Continue; }
        // Missing or unknown item types fail closed.
        let item = native["item"]["type"].as_str().unwrap_or("");
        if CODEX_NON_TOOL_ITEMS.contains(&item) { return GuardVerdict::Continue; }
        if !CODEX_TOOL_ITEMS.contains(&item) { return violation(item); }
        // First sighting counts: codex may emit a file_change as completed-only.
        // An id-less item counts every time: stricter, never looser.
        let fresh = match native["item"]["id"].as_str() { Some(id) => self.seen.insert(id.to_owned()), None => true };
        if fresh { self.step() } else { GuardVerdict::Continue }
    }
    fn antigravity(&mut self, native: &Value) -> GuardVerdict {
        match native["event"].as_str() {
            Some("result") => return self.finish(),
            Some("step_update") => {}
            _ => return GuardVerdict::Continue,
        }
        let step = &native["step_update"];
        let state = step["state"].as_str();
        let index = step["step_index"].as_u64();
        let name = step["tool_name"].as_str().filter(|n| !n.is_empty())
            .or_else(|| step["tool_info"]["name"].as_str().filter(|n| !n.is_empty()));
        if let Some(name) = name.filter(|name| !ANTIGRAVITY_TOOLS.contains(name)) {
            match state {
                Some("ACTIVE") => match index {
                    Some(index) => { self.pending.insert(index, name.to_owned()); }
                    None => return violation(name),
                },
                // Only the hook's own denial proves the call did not run.
                Some("ERROR") if step["tool_info"]["error"]["message"].as_str()
                    .is_some_and(|message| message.starts_with(AGY_HOOK_DENIED_PREFIX)) => {
                    if let Some(index) = index { self.pending.remove(&index); }
                }
                _ => return violation(name),
            }
        }
        if step["step_type"].as_str() == Some("agent_response") && state == Some("DONE") {
            let fresh = match index { Some(index) => self.seen.insert(index.to_string()), None => true };
            if fresh { return self.step(); }
        }
        GuardVerdict::Continue
    }
    fn step(&mut self) -> GuardVerdict {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.max_steps { GuardVerdict::StepLimit } else { GuardVerdict::Continue }
    }
}

fn violation(name: &str) -> GuardVerdict { GuardVerdict::ToolViolation { tool: sanitize_tool_name(name) } }
fn claude_allowed(name: &str) -> bool { super::agent_spawn::ATTEMPT_TOOLS.split(',').any(|tool| tool == name) }
fn non_empty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::String(text) => !text.is_empty(),
        Value::Bool(flag) => *flag,
        Value::Number(_) => true,
    }
}
/// Allowed blocks are text/thinking and allowlisted `tool_use`; any other
/// block type (server_tool_use, mcp_tool_use, …) is a violation.
fn assistant_blocks(native: &Value, allowed: &dyn Fn(&str) -> bool) -> GuardVerdict {
    if native["type"].as_str() != Some("assistant") { return GuardVerdict::Continue; }
    let Some(blocks) = native["message"]["content"].as_array() else { return GuardVerdict::Continue };
    for block in blocks {
        let kind = block["type"].as_str().unwrap_or("");
        if NON_TOOL_BLOCKS.contains(&kind) { continue; }
        if kind != "tool_use" { return violation(kind); }
        let name = block["name"].as_str().unwrap_or("");
        if !allowed(name) { return violation(name); }
    }
    GuardVerdict::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn run(family: RuntimeFamily, max: u32, events: &[Value]) -> Vec<GuardVerdict> {
        let mut guard = StreamGuard::new(family, max);
        events.iter().map(|event| guard.observe(event)).collect()
    }
    fn codex_item(kind: &str, id: &str, item: &str) -> Value { json!({"type":kind,"item":{"id":id,"type":item,"status":"in_progress"}}) }
    fn agy_step(index: u64, state: &str, step_type: &str, tool: Option<&str>) -> Value {
        let mut step = json!({"step_index":index,"state":state,"step_type":step_type});
        if let Some(tool) = tool { step["tool_name"] = json!(tool); step["tool_info"] = json!({"name":tool,"parameters":{}}); }
        if state == "ERROR" {
            step["tool_info"]["error"] = json!({"type":"TOOL_ERROR","message":"tool call denied by pre-tool hook: tool is outside the attempt tool surface"});
        }
        if step_type == "agent_response" && state == "DONE" {
            step["usage"] = json!({"input_tokens":10,"output_tokens":2,"thinking_tokens":1,"cache_read_tokens":0,"total_tokens":12});
        }
        json!({"event":"step_update","step_update":step})
    }

    #[test]
    fn claude_allows_attempt_tools_and_refuses_others() {
        let ok = json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"},{"type":"tool_use","name":"Bash"}]}});
        let bad = json!({"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","name":"WebFetch"}]}});
        assert_eq!(run(RuntimeFamily::Claude, 1, &[ok, bad]), vec![GuardVerdict::Continue, GuardVerdict::ToolViolation { tool: "WebFetch".into() }]);
        let mcp = json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__duduclaw__send"}]}});
        assert_eq!(run(RuntimeFamily::Claude, 1, &[mcp]), vec![GuardVerdict::ToolViolation { tool: "mcp__duduclaw__send".into() }]);
    }

    #[test]
    fn codex_real_stream_is_clean_and_counts_tool_items_once() {
        // Hand-minimised from runtime-parity/spot/codex.ndjson (codex-cli 0.159.2).
        let events = [json!({"type":"thread.started","thread_id":"t"}), json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I will read it."}}),
            codex_item("item.started", "item_1", "command_execution"), codex_item("item.completed", "item_1", "command_execution"),
            codex_item("item.started", "item_2", "command_execution"), codex_item("item.completed", "item_2", "command_execution"),
            codex_item("item.started", "item_3", "command_execution"), codex_item("item.completed", "item_3", "command_execution"),
            json!({"type":"item.completed","item":{"id":"item_4","type":"agent_message","text":"DONE"}}),
            json!({"type":"turn.completed","usage":{"input_tokens":76775,"cached_input_tokens":56832,"cache_write_input_tokens":0,"output_tokens":176,"reasoning_output_tokens":0}})];
        assert!(run(RuntimeFamily::Codex, 3, &events).iter().all(|v| *v == GuardVerdict::Continue));
        let verdicts = run(RuntimeFamily::Codex, 2, &events);
        assert_eq!(verdicts.iter().filter(|v| **v == GuardVerdict::StepLimit).count(), 1);
        assert_eq!(verdicts[7], GuardVerdict::StepLimit, "the third tool item start exceeds two steps");
        // A repeated started id is the same step.
        let repeated = [codex_item("item.started", "a", "file_change"), codex_item("item.started", "a", "file_change")];
        assert!(run(RuntimeFamily::Codex, 1, &repeated).iter().all(|v| *v == GuardVerdict::Continue));
    }

    #[test]
    fn codex_unknown_and_forbidden_item_types_are_violations() {
        for item in ["mcp_tool_call", "web_search", "collab_tool_call", "image_generation", ""] {
            let verdict = run(RuntimeFamily::Codex, 9, &[codex_item("item.completed", "x", item)]);
            assert!(matches!(&verdict[0], GuardVerdict::ToolViolation { .. }), "{item}");
        }
        let missing = json!({"type":"item.started","item":{"id":"x"}});
        assert_eq!(run(RuntimeFamily::Codex, 9, &[missing]), vec![GuardVerdict::ToolViolation { tool: "unknown".into() }]);
        for item in CODEX_NON_TOOL_ITEMS { assert_eq!(run(RuntimeFamily::Codex, 0, &[codex_item("item.completed", "x", item)]), vec![GuardVerdict::Continue]); }
        // Unknown top-level event types are not tools.
        assert_eq!(run(RuntimeFamily::Codex, 0, &[json!({"type":"future.event","item":{"type":"mcp_tool_call"}})]), vec![GuardVerdict::Continue]);
    }

    #[test]
    fn gemini_uses_the_settings_core_list() {
        let ok = json!({"type":"tool_use","tool_name":"run_shell_command","tool_id":"1","parameters":{}});
        let bad = json!({"type":"tool_use","tool_name":"google_web_search","tool_id":"2","parameters":{}});
        assert_eq!(run(RuntimeFamily::Gemini, 1, &[ok, bad]), vec![GuardVerdict::Continue, GuardVerdict::ToolViolation { tool: "google_web_search".into() }]);
        let settings = super::super::attempt_adapter::gemini_settings(3);
        let core = settings["tools"]["core"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(core, GEMINI_TOOLS);
    }

    #[test]
    fn antigravity_counts_generations_and_hook_denied_tools_are_not_violations() {
        // Shape of runtime-parity/spot/agy-v2-hook-stdin.ndjson (agy 1.2.14).
        let events = [json!({"event":"init","init":{"cwd":"/w","tools":["run_command","search_web"],"permission_mode":"always-proceed"}}),
            agy_step(0, "DONE", "user_input", None), agy_step(1, "DONE", "agent_response", None),
            agy_step(2, "ACTIVE", "tool", Some("search_web")), agy_step(2, "ERROR", "tool", Some("search_web")),
            agy_step(3, "DONE", "agent_response", None), agy_step(3, "DONE", "agent_response", None),
            agy_step(4, "ACTIVE", "tool", Some("run_command")), agy_step(4, "DONE", "tool", Some("run_command")),
            agy_step(5, "DONE", "agent_response", None),
            json!({"event":"result","result":{"status":"SUCCESS","response":"DONE\n","num_turns":1}})];
        assert!(run(RuntimeFamily::Antigravity, 3, &events).iter().all(|v| *v == GuardVerdict::Continue), "ERROR state was denied by the hook");
        let verdicts = run(RuntimeFamily::Antigravity, 2, &events);
        assert_eq!(verdicts[9], GuardVerdict::StepLimit, "third generation exceeds two, the duplicate step 3 is not counted");
        let done = agy_step(7, "DONE", "tool", Some("search_web"));
        assert_eq!(run(RuntimeFamily::Antigravity, 9, &[done]), vec![GuardVerdict::ToolViolation { tool: "search_web".into() }]);
        let info_only = json!({"event":"step_update","step_update":{"step_index":8,"state":"DONE","step_type":"tool","tool_info":{"name":"browser_subagent"}}});
        assert_eq!(run(RuntimeFamily::Antigravity, 9, &[info_only]), vec![GuardVerdict::ToolViolation { tool: "browser_subagent".into() }]);
    }

    #[test]
    fn grok_refuses_forbidden_tools_and_configured_mcp_servers() {
        // runtime-parity/spot/grok-v2-tools-only.ndjson: the model called search_tool.
        let call = json!({"type":"assistant","message":{"id":"msg_0","model":"grok-4.7","content":[{"type":"thinking"},
            {"type":"tool_use","name":"read_file","id":"c0"},{"type":"tool_use","name":"list_dir","id":"c1"},{"type":"tool_use","name":"search_tool","id":"c2"}]}});
        assert_eq!(run(RuntimeFamily::Grok, 9, &[call]), vec![GuardVerdict::ToolViolation { tool: "search_tool".into() }]);
        let clean_init = json!({"type":"system","subtype":"init","model":"grok-4.7","tools":GROK_TOOLS,"mcp_servers":[]});
        let host_init = json!({"type":"system","subtype":"init","mcp_servers":[{"name":"notion","status":"pending"}]});
        assert_eq!(run(RuntimeFamily::Grok, 1, &[clean_init, host_init]), vec![GuardVerdict::Continue, GuardVerdict::ToolViolation { tool: "mcp_servers".into() }]);
        let max_turns = json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":2,"total_cost_usd":0.03,"errors":["Reached the maximum number of turns"]});
        // Grok's own --max-turns is exact; the host does not count it.
        let write = json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"write"}]}});
        assert!(run(RuntimeFamily::Grok, 0, &[write.clone(), write, max_turns]).iter().all(|v| *v == GuardVerdict::Continue));
    }

    #[test]
    fn codex_counts_completed_only_items_and_checks_updates() {
        // A file_change emitted only as item.completed still counts as a step.
        let events = [codex_item("item.started", "c1", "command_execution"), codex_item("item.completed", "c1", "command_execution"),
            codex_item("item.completed", "f1", "file_change"), codex_item("item.updated", "f1", "file_change")];
        assert_eq!(run(RuntimeFamily::Codex, 1, &events), vec![GuardVerdict::Continue, GuardVerdict::Continue, GuardVerdict::StepLimit, GuardVerdict::Continue]);
        assert!(run(RuntimeFamily::Codex, 2, &events).iter().all(|v| *v == GuardVerdict::Continue));
        let unknown = codex_item("item.updated", "w1", "web_search");
        assert_eq!(run(RuntimeFamily::Codex, 9, &[unknown]), vec![GuardVerdict::ToolViolation { tool: "web_search".into() }]);
        let id_less = json!({"type":"item.completed","item":{"type":"file_change"}});
        assert_eq!(run(RuntimeFamily::Codex, 1, &[id_less.clone(), id_less]), vec![GuardVerdict::Continue, GuardVerdict::StepLimit]);
    }

    #[test]
    fn antigravity_forbidden_tool_needs_the_hook_denial_to_be_harmless() {
        let active = agy_step(2, "ACTIVE", "tool", Some("search_web"));
        let result = json!({"event":"result","result":{"status":"SUCCESS","response":"ok"}});
        // Hook-denied ⇒ fine.
        let denied = agy_step(2, "ERROR", "tool", Some("search_web"));
        assert!(run(RuntimeFamily::Antigravity, 9, &[active.clone(), denied, result.clone()]).iter().all(|v| *v == GuardVerdict::Continue));
        // ERROR for any other reason may have run: violation.
        let mut other = agy_step(2, "ERROR", "tool", Some("search_web"));
        other["step_update"]["tool_info"]["error"]["message"] = json!("network unreachable");
        assert_eq!(run(RuntimeFamily::Antigravity, 9, &[active.clone(), other])[1], GuardVerdict::ToolViolation { tool: "search_web".into() });
        let mut unprefixed = agy_step(2, "ERROR", "tool", Some("search_web"));
        unprefixed["step_update"]["tool_info"]["error"]["message"] = json!("x tool call denied by pre-tool hook");
        assert!(matches!(run(RuntimeFamily::Antigravity, 9, &[active.clone(), unprefixed])[1], GuardVerdict::ToolViolation { .. }));
        // Still ACTIVE when the result arrives ⇒ violation.
        assert_eq!(run(RuntimeFamily::Antigravity, 9, &[active.clone(), result])[1], GuardVerdict::ToolViolation { tool: "search_web".into() });
        // Unknown terminal state ⇒ violation.
        let cancelled = agy_step(2, "CANCELLED", "tool", Some("search_web"));
        assert!(matches!(run(RuntimeFamily::Antigravity, 9, &[active.clone(), cancelled])[1], GuardVerdict::ToolViolation { .. }));
        // End of stream without result: finish() reports the pending tool once.
        let mut guard = StreamGuard::new(RuntimeFamily::Antigravity, 9);
        assert_eq!(guard.observe(&active), GuardVerdict::Continue);
        assert_eq!(guard.finish(), GuardVerdict::ToolViolation { tool: "search_web".into() });
        assert_eq!(guard.finish(), GuardVerdict::Continue);
        // Allowed tools never become pending.
        let mut guard = StreamGuard::new(RuntimeFamily::Antigravity, 9);
        guard.observe(&agy_step(4, "ACTIVE", "tool", Some("run_command")));
        assert_eq!(guard.finish(), GuardVerdict::Continue);
    }

    #[test]
    fn claude_and_grok_refuse_unknown_block_types() {
        for family in [RuntimeFamily::Claude, RuntimeFamily::Grok] {
            for kind in ["server_tool_use", "mcp_tool_use", "web_search_tool_result"] {
                let event = json!({"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":kind,"name":"Read"}]}});
                assert_eq!(run(family, 9, &[event]), vec![GuardVerdict::ToolViolation { tool: kind.into() }], "{family:?} {kind}");
            }
            let fine = json!({"type":"assistant","message":{"content":[{"type":"thinking"},{"type":"redacted_thinking"},{"type":"text","text":"x"}]}});
            assert_eq!(run(family, 9, &[fine]), vec![GuardVerdict::Continue]);
        }
    }

    #[test]
    fn openai_compat_is_not_checked_and_names_are_sanitised() {
        assert_eq!(run(RuntimeFamily::OpenAiCompat, 0, &[json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"anything"}]}})]), vec![GuardVerdict::Continue]);
        assert_eq!(sanitize_tool_name("bad/../\u{0}name;rm -rf 字"), "bad..namerm-rf");
        assert_eq!(sanitize_tool_name(&"x".repeat(200)).len(), TOOL_NAME_MAX_BYTES);
        assert_eq!(sanitize_tool_name("字字"), "unknown");
    }
}
