//! Terminal work results transported losslessly across legacy String APIs.
use serde::{Deserialize, Serialize};
const PREFIX: &str = "DUDUCLAW_WORK_OUTCOME:";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum WorkOutcome {
    MaxTurns { limit: u64, partial: String },
    TimeoutTotal { seconds: u64, partial: String },
    TimeoutNoProgress { seconds: u64, tool: Option<String>, partial: String },
}
impl WorkOutcome {
    pub(crate) fn encode(&self) -> String {
        let mut value = serde_json::to_value(self).expect("work outcome serialization");
        if let Some((_, reference)) = crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten() {
            value["reference_id"] = serde_json::Value::String(reference);
        }
        format!("{PREFIX}{value}")
    }
    pub(crate) fn reference(error: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(error.strip_prefix(PREFIX)?).ok()?;
        value.get("reference_id").and_then(|v| v.as_str()).map(str::to_owned)
    }
    pub(crate) fn decode(error: &str) -> Option<Self> { serde_json::from_str(error.strip_prefix(PREFIX)?).ok() }
    pub(crate) fn code(&self) -> &'static str { match self { Self::MaxTurns { .. } => "max_turns", Self::TimeoutTotal { .. } => "timeout_total", Self::TimeoutNoProgress { .. } => "timeout_no_progress" } }
    pub(crate) fn partial(&self) -> &str { match self { Self::MaxTurns { partial, .. } | Self::TimeoutTotal { partial, .. } | Self::TimeoutNoProgress { partial, .. } => partial } }
}
/// The subtype is diagnostic data, never a routing substring.
pub(crate) fn result_error(event: &serde_json::Value, limit: u64, partial: &str) -> Option<String> {
    if event.get("is_error").and_then(|v| v.as_bool()) != Some(true) { return None; }
    let subtype = event.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
    if subtype == "error_max_turns" {
        return Some(WorkOutcome::MaxTurns { limit, partial: partial.into() }.encode());
    }
    let subtype = clean_label(subtype);
    let text = event.get("result").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .or_else(|| event.get("error").and_then(|v| v.as_str())).unwrap_or("stream-json failure");
    Some(format!("claude CLI stream error [{subtype}]: {text}"))
}
fn clean_label(text: &str) -> String {
    duduclaw_core::truncate_chars(&text.chars().filter(|c| !c.is_control()).collect::<String>(), 120)
}
/// Only meaningful runtime progress resets this clock; terminal results stop it.
pub(crate) struct IdleWatch {
    pub(crate) seconds: u64,
    last_event: tokio::time::Instant,
    terminal: bool,
    initialized: bool,
    pending: Vec<(String, String)>,
}
impl IdleWatch {
    pub(crate) fn new(seconds: u64) -> Self { Self { seconds, last_event: tokio::time::Instant::now(), pending: vec![], terminal: false, initialized: false } }
    pub(crate) fn is_terminal(&self) -> bool { self.terminal }
    pub(crate) async fn expired(&self) {
        if self.seconds == 0 || self.terminal { std::future::pending::<()>().await; }
        tokio::time::sleep_until(self.last_event + std::time::Duration::from_secs(self.seconds)).await;
    }
    pub(crate) fn observe(&mut self, event: &serde_json::Value) {
        let kind = event.get("type").and_then(|v| v.as_str());
        let mut progress = false;
        if kind == Some("system") && event.get("subtype").and_then(|v| v.as_str()) == Some("init") && !self.initialized {
            self.initialized = true;
            progress = true;
        }
        if kind == Some("result") && (event.get("is_error").and_then(|v| v.as_bool()).is_some()
            || event.get("result").and_then(|v| v.as_str()).is_some()) {
            self.terminal = true;
        }
        if kind == Some("stream_event") {
            if let Some(inner) = event.get("event") {
                if inner.get("type").and_then(|v| v.as_str()) == Some("content_block_delta") {
                    progress |= ["/delta/text", "/delta/thinking", "/delta/reasoning"]
                        .iter().any(|path| nonempty(inner.pointer(path)));
                }
                if inner.get("type").and_then(|v| v.as_str()) == Some("content_block_start") {
                    progress |= inner.pointer("/content_block/type").and_then(|v| v.as_str()) == Some("tool_use");
                }
            }
        }
        if matches!(kind, Some("assistant" | "user")) {
            if let Some(blocks) = event.pointer("/message/content").and_then(|v| v.as_array()) {
                for block in blocks {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("tool_use") => {
                            progress = true;
                            if let (Some(id), Some(name)) = (block.get("id").and_then(|v| v.as_str()), block.get("name").and_then(|v| v.as_str())) {
                                self.pending.push((id.into(), clean_label(name)));
                            }
                        }
                        Some("tool_result") => {
                            progress = true;
                            if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) { self.pending.retain(|(key, _)| key != id); }
                        }
                        Some("text") if kind == Some("assistant") => { progress |= nonempty(block.get("text")); }
                        Some("thinking" | "reasoning") if kind == Some("assistant") => {
                            progress |= nonempty(block.get("thinking")) || nonempty(block.get("reasoning"));
                        }
                        _ => {}
                    }
                }
            }
        }
        if progress && !self.terminal { self.last_event = tokio::time::Instant::now(); }
    }
    pub(crate) fn outcome(&self, partial: String) -> WorkOutcome {
        WorkOutcome::TimeoutNoProgress { seconds: self.seconds, tool: self.pending.last().map(|(_, name)| name.clone()), partial }
    }
}

/// Routing accepts only the program's typed marker, never provider error text.
pub(crate) fn is_work_result(error: &str) -> bool {
    WorkOutcome::decode(error).is_some()
}

fn nonempty(value: Option<&serde_json::Value>) -> bool {
    value.and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
}

impl IdleWatch {
    pub(crate) fn observe_codex(&mut self, event: &serde_json::Value) {
        let kind = event.get("type").and_then(|v| v.as_str());
        if kind == Some("turn.completed") { self.terminal = true; return; }
        let mut progress = false;
        if matches!(kind, Some("item.started" | "item.updated" | "item.completed")) {
            if let Some(item) = event.get("item") {
                let item_type = item.get("type").and_then(|v| v.as_str());
                let tool = matches!(item_type, Some("command_execution" | "mcp_tool_call" | "function_call" | "tool_call" | "web_search" | "file_change"))
                    && ["id", "command", "tool", "name"].iter().any(|key| nonempty(item.get(*key)));
                progress = tool && matches!(kind, Some("item.started" | "item.completed"));
                progress |= matches!(item_type, Some("agent_message" | "message" | "reasoning"))
                    && (nonempty(item.get("text")) || item.get("content").and_then(|v| v.as_array()).is_some_and(|blocks|
                        blocks.iter().any(|block| nonempty(block.get("text")))));
                if tool {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        if kind == Some("item.started") {
                            let name = item.get("tool").and_then(|v| v.as_str()).or(item_type).unwrap_or("tool");
                            self.pending.push((id.into(), clean_label(name)));
                        } else if kind == Some("item.completed") { self.pending.retain(|(key, _)| key != id); }
                    }
                }
            }
        }
        if progress && !self.terminal { self.last_event = tokio::time::Instant::now(); }
    }

    pub(crate) fn observe_openai(&mut self, event: &serde_json::Value) {
        let Some(choices) = event.get("choices").and_then(|v| v.as_array()) else { return; };
        let mut progress = false;
        for choice in choices {
            if choice.get("finish_reason").and_then(|v| v.as_str()) == Some("stop") {
                self.terminal = true;
            }
            if let Some(delta) = choice.get("delta") {
                progress |= ["content", "reasoning_content", "reasoning"].iter().any(|key| nonempty(delta.get(*key)));
                progress |= nonempty(delta.pointer("/function_call/name"));
                progress |= delta.get("tool_calls").and_then(|v| v.as_array()).is_some_and(|calls|
                    calls.iter().any(|call| nonempty(call.get("id")) || nonempty(call.pointer("/function/name"))));
            }
        }
        if progress && !self.terminal { self.last_event = tokio::time::Instant::now(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn terminal_subtypes_preserve_budget_partial_and_unknown_label() {
        for result in [json!({"type":"result","subtype":"error_max_turns","is_error":true}),
                       json!({"type":"result","subtype":"error_max_turns","is_error":true,"result":""})] {
            let error = result_error(&result, 17, "已完成第一步").unwrap();
            assert_eq!(WorkOutcome::decode(&error), Some(WorkOutcome::MaxTurns { limit:17, partial:"已完成第一步".into() }));
        }
        let error = result_error(&json!({"is_error":true,"subtype":"error_future\n"}), 1, "").unwrap();
        assert!(WorkOutcome::decode(&error).is_none());
        assert!(error.contains("[error_future]"));
        assert!(!error.contains("Unknown stream-json"));
        assert!(!error.contains('\n'));
        assert_eq!(clean_label(&"字".repeat(200)).chars().count(), 120);
    }
    #[test]
    fn max_turns_requires_an_explicit_error_flag() {
        for event in [json!({"subtype":"error_max_turns","is_error":false}),
                      json!({"subtype":"error_max_turns"})] {
            assert_eq!(result_error(&event, 7, "partial"), None);
        }
    }

    #[test]
    fn untyped_timeouts_never_route_as_work_results() {
        for error in ["connection timed out", "CLI pipe timed out", "MCP hard timeout",
                      "process wait timed out", "DUDUCLAW_WORK_OUTCOME:invalid"] {
            assert!(!is_work_result(error), "{error}");
        }
        let typed = WorkOutcome::TimeoutTotal { seconds:60, partial:String::new() }.encode();
        assert!(!is_work_result(&format!("network error: {typed}")));
    }

    #[tokio::test(start_paused = true)]
    async fn claude_telemetry_does_not_reset_progress() {
        let mut idle = IdleWatch::new(60);
        idle.observe(&json!({"type":"system","subtype":"init"}));
        let initial = idle.last_event;
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        for event in [json!({"type":"rate_limit_event"}), json!({"type":"system","subtype":"status"}), json!({"type":"system","subtype":"init"}),
            json!({"type":"stream_event"}), json!({"type":"assistant","message":{"content":[]}}),
            json!({"type":"keepalive"})] {
            idle.observe(&event);
            assert_eq!(idle.last_event, initial);
        }
        idle.observe(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"thinking":"reason"}}}));
        assert!(idle.last_event > initial);
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        idle.observe(&json!({"type":"result","subtype":"success","is_error":false}));
        assert!(idle.terminal);
        assert!(tokio::time::timeout(std::time::Duration::from_secs(600), idle.expired()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn codex_empty_events_do_not_reset_progress() {
        let mut idle = IdleWatch::new(60);
        let initial = idle.last_event;
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        for event in [json!({"type":"thread.started"}), json!({"type":"item.started"}),
            json!({"type":"item.started","item":{"type":"command_execution"}}),
            json!({"type":"item.completed","item":{"id":"a","type":"agent_message","text":""}})] {
            idle.observe_codex(&event);
            assert_eq!(idle.last_event, initial);
        }
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        idle.expired().await;
        idle.observe_codex(&json!({"type":"item.started","item":{"id":"a","type":"command_execution","command":"pwd"}}));
        assert!(idle.last_event > initial);
        assert_eq!(idle.pending.last().unwrap().0, "a");
        tokio::time::advance(std::time::Duration::from_secs(10)).await;
        idle.observe_codex(&json!({"type":"item.completed","item":{"id":"a","type":"command_execution","aggregated_output":""}}));
        assert!(idle.pending.is_empty());
        idle.observe_codex(&json!({"type":"turn.completed","usage":{}}));
        assert!(idle.terminal);
    }

    #[tokio::test(start_paused = true)]
    async fn openai_compat_requires_content_tool_or_terminal() {
        let mut idle = IdleWatch::new(60);
        let initial = idle.last_event;
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        for event in [json!({}), json!({"type":"ping"}), json!({"usage":{"completion_tokens":3}}),
            json!({"choices":[]}), json!({"choices":[{"delta":{"content":""}}]}),
            json!({"choices":[{"finish_reason":"ping","delta":{}}]})] {
            idle.observe_openai(&event);
            assert_eq!(idle.last_event, initial);
            assert!(!idle.terminal);
        }
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        idle.expired().await;
        for delta in [json!({"content":"answer"}), json!({"reasoning_content":"reason"}),
            json!({"reasoning":"reason"}), json!({"tool_calls":[{"id":"call","function":{"name":"search"}}]})] {
            let before = idle.last_event;
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            idle.observe_openai(&json!({"choices":[{"delta":delta}]}));
            assert!(idle.last_event > before);
        }
        idle.observe_openai(&json!({"choices":[{"finish_reason":"stop","delta":{}}]}));
        assert!(idle.terminal);
    }

    #[test]
    fn classification_and_four_languages_cover_work_results() {
        use crate::channel_reply::{FailureReason, TurnFailureReason};
        for (outcome, reason) in [
            (WorkOutcome::MaxTurns { limit:17, partial:String::new() }, FailureReason::MaxTurns),
            (WorkOutcome::TimeoutTotal { seconds:60, partial:String::new() }, FailureReason::TimeoutTotal),
            (WorkOutcome::TimeoutNoProgress { seconds:900, tool:Some("Bash".into()), partial:String::new() }, FailureReason::TimeoutNoProgress),
        ] {
            assert_eq!(crate::channel_reply::classify_cli_failure(&outcome.encode()), reason);
            assert!(is_work_result(&outcome.encode()));
            assert!(crate::channel_reply::retry_hint_for(&outcome.encode()).is_none());
            assert_eq!(outcome.code(), reason.as_str());
            let mut translations = std::collections::HashSet::new();
            for lang in ["zh-TW", "en", "ja-JP", "zh-CN"] {
                let text = crate::channel_reply::failure_notice::work_notice(&outcome, Some(lang));
                assert!(!text.is_empty());
                translations.insert(text);
                assert!(!TurnFailureReason::from(reason).text(Some(lang)).is_empty());
            }
            assert_eq!(translations.len(), 4);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn event_silence_tracks_pending_tool_and_zero_disables() {
        let mut idle = IdleWatch::new(60);
        idle.observe(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Bash"}]}}));
        for _ in 0..3 {
            tokio::time::advance(std::time::Duration::from_secs(59)).await;
            assert!(tokio::time::timeout(std::time::Duration::ZERO, idle.expired()).await.is_err());
            idle.observe(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"next"}}}));
        }
        // A generated 90-second keep-alive never calls observe.
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        idle.expired().await;
        assert_eq!(idle.outcome(String::new()), WorkOutcome::TimeoutNoProgress { seconds:60, tool:Some("Bash".into()), partial:String::new() });
        idle.observe(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a"}]}}));
        assert!(matches!(idle.outcome(String::new()), WorkOutcome::TimeoutNoProgress { tool:None, .. }));
        let disabled = IdleWatch::new(0);
        assert!(tokio::time::timeout(std::time::Duration::from_secs(100_000), disabled.expired()).await.is_err());
    }
}
