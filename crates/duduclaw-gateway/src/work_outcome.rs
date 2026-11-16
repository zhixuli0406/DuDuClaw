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
    let subtype = event.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
    if subtype == "error_max_turns" {
        return Some(WorkOutcome::MaxTurns { limit, partial: partial.into() }.encode());
    }
    if !event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) { return None; }
    let subtype = clean_label(subtype);
    let text = event.get("result").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .or_else(|| event.get("error").and_then(|v| v.as_str())).unwrap_or("stream-json failure");
    Some(format!("claude CLI stream error [{subtype}]: {text}"))
}
fn clean_label(text: &str) -> String {
    duduclaw_core::truncate_chars(&text.chars().filter(|c| !c.is_control()).collect::<String>(), 120)
}
/// Only parsed events reset this clock; progress notifications never call observe.
pub(crate) struct IdleWatch {
    pub(crate) seconds: u64,
    last_event: tokio::time::Instant,
    pending: Vec<(String, String)>,
}
impl IdleWatch {
    pub(crate) fn new(seconds: u64) -> Self { Self { seconds, last_event: tokio::time::Instant::now(), pending: vec![] } }
    pub(crate) async fn expired(&self) {
        if self.seconds == 0 { std::future::pending::<()>().await; }
        tokio::time::sleep_until(self.last_event + std::time::Duration::from_secs(self.seconds)).await;
    }
    pub(crate) fn observe(&mut self, event: &serde_json::Value) {
        self.last_event = tokio::time::Instant::now();
        if let Some(blocks) = event.pointer("/message/content").and_then(|v| v.as_array()) {
            for block in blocks {
                match block.get("type").and_then(|v| v.as_str()) {
                    Some("tool_use") => {
                        if let (Some(id), Some(name)) = (block.get("id").and_then(|v| v.as_str()), block.get("name").and_then(|v| v.as_str())) {
                            self.pending.push((id.into(), clean_label(name)));
                        }
                    }
                    Some("tool_result") => {
                        if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) { self.pending.retain(|(key, _)| key != id); }
                    }
                    _ => {}
                }
            }
        }
    }
    pub(crate) fn outcome(&self, partial: String) -> WorkOutcome {
        WorkOutcome::TimeoutNoProgress { seconds: self.seconds, tool: self.pending.last().map(|(_, name)| name.clone()), partial }
    }
}

/// Compatibility for runtimes whose total timeout predates typed work outcomes.
pub(crate) fn is_work_result(error: &str) -> bool {
    use crate::channel_reply::FailureReason;
    matches!(crate::channel_reply::classify_cli_failure(error), FailureReason::MaxTurns
        | FailureReason::Timeout | FailureReason::TimeoutTotal | FailureReason::TimeoutNoProgress)
}

impl IdleWatch {
    pub(crate) fn observe_codex(&mut self, event: &serde_json::Value) {
        self.observe(event);
        let kind = event.get("type").and_then(|v| v.as_str());
        if let Some(item) = event.get("item") {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                if kind == Some("item.started") {
                    if let Some(name) = item.get("tool").or_else(|| item.get("type")).and_then(|v| v.as_str()) {
                        self.pending.push((id.into(), clean_label(name)));
                    }
                } else if kind == Some("item.completed") { self.pending.retain(|(key, _)| key != id); }
            }
        }
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
        assert!(error.contains("[error_future]"));
        assert!(!error.contains("Unknown stream-json"));
        assert!(!error.contains('\n'));
        assert_eq!(clean_label(&"字".repeat(200)).chars().count(), 120);
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
            idle.observe(&json!({"type":"stream_event"}));
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
