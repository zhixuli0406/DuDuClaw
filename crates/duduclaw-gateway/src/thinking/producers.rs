//! Small per-runtime thinking producers that need no state of their own:
//! Codex `exec --json` output and the tools-less Anthropic Direct API call.
//! (Claude CLI: [`super::claude_stream`]; tool loops: [`super::llm_tap`].)
//!
//! Every request-side helper returns "nothing" unless the turn's viewer
//! reads thinking text ([`super::ThinkingPolicy::wants_text`]), so a call
//! outside a reply turn is byte-identical to before.

use serde_json::{Value, json};

use super::event::ThinkingKind;
use super::policy::ThinkingPolicy;
use super::recorder::Recorder;

/// `-c model_reasoning_summary=auto` for one `codex exec`, only when the
/// viewer reads thinking text (Codex then writes `reasoning` items).
pub fn codex_reasoning_summary_args(policy: &ThinkingPolicy) -> Vec<String> {
    if policy.wants_text() {
        vec!["-c".to_string(), "model_reasoning_summary=auto".to_string()]
    } else {
        Vec::new()
    }
}

/// What a finished `codex exec --json` run said about its reasoning.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CodexThinking {
    /// `item.completed` items of type `reasoning`, in order (may be empty
    /// strings when Codex wrote none).
    pub summaries: Vec<String>,
    /// `turn.completed.usage.reasoning_output_tokens` (last turn wins).
    pub reasoning_tokens: Option<u64>,
}

/// Read the reasoning items and usage from Codex JSONL output. Unknown
/// shapes are skipped, never guessed.
pub fn parse_codex_thinking(stdout: &str) -> CodexThinking {
    let mut out = CodexThinking::default();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev.get("type").and_then(Value::as_str) {
            Some("item.completed")
                if ev.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
            {
                let text = ev
                    .pointer("/item/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                out.summaries.push(text.to_string());
            }
            Some("turn.completed") => {
                if let Some(n) = ev
                    .pointer("/usage/reasoning_output_tokens")
                    .and_then(Value::as_u64)
                {
                    out.reasoning_tokens = Some(n);
                }
            }
            _ => {}
        }
    }
    out
}

/// Report a Codex run after the fact: one block per reasoning item, then the
/// turn's reasoning tokens (only when non-zero).
pub fn report_codex(thinking: &CodexThinking, rec: &mut Recorder) {
    for text in &thinking.summaries {
        let shown = Some(text.as_str()).filter(|t| !t.trim().is_empty());
        let kind = if shown.is_some() {
            ThinkingKind::Summary
        } else {
            ThinkingKind::Omitted
        };
        rec.oneshot(kind, shown, None);
    }
    if let Some(n) = thinking.reasoning_tokens.filter(|n| *n > 0) {
        rec.turn_usage(n);
    }
}

/// The `thinking` field for a tools-less Direct API request: a summarized
/// display only when the viewer reads thinking text AND the model thinks on
/// every request anyway (so the turn costs the same). `None` ⇒ the field is
/// absent and the body is byte-identical to before.
pub fn direct_api_thinking(policy: &ThinkingPolicy, model: &str) -> Option<Value> {
    use duduclaw_llm::providers::anthropic_thinking::{ThinkingFamily, thinking_family};
    (policy.wants_text() && thinking_family(model) == ThinkingFamily::AdaptiveAlwaysOn)
        .then(|| json!({"type": "adaptive", "display": "summarized"}))
}

/// One Direct API response content block, as far as thinking goes.
pub enum AnthropicBlock<'a> {
    Thinking(&'a str),
    Redacted,
}

/// Report a Direct API response's thinking blocks after the fact.
pub fn report_anthropic_blocks<'a>(
    blocks: impl IntoIterator<Item = AnthropicBlock<'a>>,
    rec: &mut Recorder,
) {
    for block in blocks {
        match block {
            AnthropicBlock::Thinking(text) => {
                let shown = Some(text).filter(|t| !t.trim().is_empty());
                let kind = if shown.is_some() {
                    ThinkingKind::Summary
                } else {
                    ThinkingKind::Omitted
                };
                rec.oneshot(kind, shown, None);
            }
            AnthropicBlock::Redacted => rec.oneshot(ThinkingKind::Redacted, None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking::ThinkingEvent;
    use crate::thinking::policy::SinkLevel;
    use std::sync::{Arc, Mutex};

    fn at(level: SinkLevel) -> ThinkingPolicy {
        ThinkingPolicy {
            level,
            ..ThinkingPolicy::INACTIVE
        }
    }

    fn rig() -> (Recorder, Arc<Mutex<Vec<ThinkingEvent>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        (
            Recorder::with_emitter("codex", Arc::new(move |ev| g.lock().unwrap().push(ev))),
            got,
        )
    }

    #[test]
    fn codex_flag_only_for_text_viewers() {
        assert_eq!(
            codex_reasoning_summary_args(&at(SinkLevel::Summary)),
            ["-c", "model_reasoning_summary=auto"]
        );
        for level in [SinkLevel::Off, SinkLevel::Signal, SinkLevel::Status] {
            assert!(codex_reasoning_summary_args(&at(level)).is_empty());
        }
        assert!(codex_reasoning_summary_args(&ThinkingPolicy::INACTIVE).is_empty());
    }

    #[test]
    fn codex_reasoning_items_and_usage() {
        let out = [
            r#"{"type":"thread.started","thread_id":"x"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"**Planning** the comparison"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"reasoning","text":""}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"done"}}"#,
            "not json",
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5,"reasoning_output_tokens":64}}"#,
        ]
        .join("\n");
        let t = parse_codex_thinking(&out);
        assert_eq!(t.summaries, ["**Planning** the comparison", ""]);
        assert_eq!(t.reasoning_tokens, Some(64));
        let (mut rec, got) = rig();
        report_codex(&t, &mut rec);
        let got = got.lock().unwrap();
        let shape: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|e| (e.block_id.as_str(), e.phase.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                ("t0", "start", "summary"),
                ("t0", "end", "summary"),
                ("t1", "start", "omitted"),
                ("t1", "end", "omitted"),
                ("turn", "end", "omitted"),
            ]
        );
        assert_eq!(got[1].text.as_deref(), Some("**Planning** the comparison"));
        assert_eq!(got[4].tokens, Some(64));
    }

    #[test]
    fn codex_without_reasoning_reports_nothing() {
        let out = r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1,"reasoning_output_tokens":0}}"#;
        let (mut rec, got) = rig();
        report_codex(&parse_codex_thinking(out), &mut rec);
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn direct_api_thinking_only_for_always_on_models_and_text_viewers() {
        let on = at(SinkLevel::Summary);
        assert_eq!(
            direct_api_thinking(&on, "claude-opus-5-5"),
            Some(json!({"type": "adaptive", "display": "summarized"}))
        );
        // Opt-in and legacy families never get thinking turned on for display.
        for model in ["claude-opus-4-7", "claude-sonnet-4-6", "claude-haiku-4-5", "claude-3-7-sonnet"] {
            assert_eq!(direct_api_thinking(&on, model), None, "{model}");
        }
        for level in [SinkLevel::Off, SinkLevel::Signal, SinkLevel::Status] {
            assert_eq!(direct_api_thinking(&at(level), "claude-opus-5-5"), None);
        }
        assert_eq!(direct_api_thinking(&ThinkingPolicy::INACTIVE, "claude-opus-5-5"), None);
    }

    #[test]
    fn anthropic_blocks_map_to_kinds() {
        let (mut rec, got) = rig();
        report_anthropic_blocks(
            [
                AnthropicBlock::Thinking("摘要"),
                AnthropicBlock::Thinking(" "),
                AnthropicBlock::Redacted,
            ],
            &mut rec,
        );
        let kinds: Vec<&str> = got
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.phase.as_str() == "end")
            .map(|e| e.kind.as_str())
            .collect();
        assert_eq!(kinds, ["summary", "omitted", "redacted"]);
    }
}
