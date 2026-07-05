//! Stream-json transcript parsing for eval runs.
//!
//! Mirrors the event semantics of the gateway's
//! `channel_reply::parse_claude_stream_json_complete` (which is `pub(crate)`
//! and therefore not importable here), extended to *retain* the signals the
//! eval suite asserts on: the ordered list of `tool_use` blocks (name +
//! input) in addition to the final text and the diagnostic counters.
//!
//! Keep the error semantics in lockstep with the gateway parser: a `result`
//! event with `is_error: true` or an assistant-level `error` field is a hard
//! `Err` (fail closed), not a silently-empty transcript.

/// One `tool_use` content block observed in the transcript, in order.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    /// Tool name as reported by the CLI (e.g. `Bash`,
    /// `mcp__duduclaw__tasks_create`).
    pub name: String,
    /// The tool input payload (opaque JSON; kept for report context).
    pub input: serde_json::Value,
}

/// Everything the assertion layer needs from one agent run.
#[derive(Debug, Default)]
pub struct EvalTranscript {
    /// Final answer text (last assistant text block, overridden by a
    /// non-empty `result` event — same precedence as the gateway parser).
    pub final_text: String,
    /// Ordered `tool_use` blocks across all assistant events.
    pub tool_uses: Vec<ToolInvocation>,
    pub text_blocks: u32,
    pub thinking_blocks: u32,
    pub assistant_events: u32,
    pub result_events: u32,
    pub lines_seen: u32,
    pub events_parsed: u32,
    pub last_result_subtype: Option<String>,
    pub last_stop_reason: Option<String>,
}

impl EvalTranscript {
    /// Compact one-line diagnostics for reports (post-mortem parity with
    /// `StreamDiagnostics::render`).
    pub fn diagnostics(&self) -> String {
        format!(
            "lines={} events={} assistant={} text_blocks={} thinking={} \
             tool_use={} result_events={} result_subtype={:?} stop_reason={:?}",
            self.lines_seen,
            self.events_parsed,
            self.assistant_events,
            self.text_blocks,
            self.thinking_blocks,
            self.tool_uses.len(),
            self.result_events,
            self.last_result_subtype,
            self.last_stop_reason,
        )
    }
}

/// Parse a complete stream-json stdout buffer (newline-delimited JSON
/// events). Unparseable lines are skipped (the CLI interleaves banner /
/// progress noise on some paths); in-band errors fail closed.
pub fn parse_stream_json(stdout: &str) -> Result<EvalTranscript, String> {
    let mut t = EvalTranscript::default();

    for raw_line in stdout.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        t.lines_seen += 1;

        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        t.events_parsed += 1;

        match event.get("type").and_then(|v| v.as_str()) {
            Some("result") => {
                t.result_events += 1;
                t.last_result_subtype = event
                    .get("subtype")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                if event
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let err_text = event
                        .get("result")
                        .and_then(|r| r.as_str())
                        .unwrap_or("Unknown stream-json error");
                    return Err(format!("claude CLI stream error: {err_text}"));
                }
                if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                    // Non-empty result text wins; tool-use turns often carry
                    // an empty `result` while the real answer sits in the
                    // last assistant text block.
                    if !text.is_empty() {
                        t.final_text = text.to_string();
                    }
                }
            }
            Some("assistant") => {
                t.assistant_events += 1;
                if let Some(err) = event.get("error").and_then(|e| e.as_str()) {
                    return Err(format!("claude CLI assistant error: {err}"));
                }
                if let Some(sr) = event
                    .pointer("/message/stop_reason")
                    .and_then(|v| v.as_str())
                {
                    t.last_stop_reason = Some(sr.to_string());
                }
                let Some(content) = event
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                else {
                    continue;
                };
                for block in content {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            t.text_blocks += 1;
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                t.final_text = text.to_string();
                            }
                        }
                        Some("thinking") => t.thinking_blocks += 1,
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("<unnamed>")
                                .to_string();
                            let input = block
                                .get("input")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            t.tool_uses.push(ToolInvocation { name, input });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(blocks: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"stop_reason\":\"end_turn\",\"content\":[{blocks}]}}}}"
        )
    }

    #[test]
    fn extracts_text_tools_and_counters() {
        let stdout = [
            assistant(
                "{\"type\":\"thinking\",\"thinking\":\"...\"},\
                 {\"type\":\"tool_use\",\"name\":\"mcp__duduclaw__tasks_create\",\"input\":{\"title\":\"t\"}}",
            ),
            assistant("{\"type\":\"text\",\"text\":\"done, task created\"}"),
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}".into(),
        ]
        .join("\n");

        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.final_text, "done, task created");
        assert_eq!(t.tool_uses.len(), 1);
        assert_eq!(t.tool_uses[0].name, "mcp__duduclaw__tasks_create");
        assert_eq!(t.text_blocks, 1);
        assert_eq!(t.thinking_blocks, 1);
        assert_eq!(t.assistant_events, 2);
        assert_eq!(t.result_events, 1);
        assert_eq!(t.last_result_subtype.as_deref(), Some("success"));
        assert_eq!(t.last_stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn nonempty_result_text_wins() {
        let stdout = [
            assistant("{\"type\":\"text\",\"text\":\"draft\"}"),
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"final answer\"}".into(),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.final_text, "final answer");
    }

    #[test]
    fn result_error_fails_closed() {
        let stdout = "{\"type\":\"result\",\"is_error\":true,\"result\":\"boom\"}";
        let err = parse_stream_json(stdout).unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn assistant_error_fails_closed() {
        let stdout = "{\"type\":\"assistant\",\"error\":\"rate limited\"}";
        let err = parse_stream_json(stdout).unwrap_err();
        assert!(err.contains("rate limited"));
    }

    #[test]
    fn skips_noise_lines_and_empty_input() {
        let stdout = format!(
            "not json at all\n\n{}\n",
            assistant("{\"type\":\"text\",\"text\":\"ok\"}")
        );
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.final_text, "ok");
        assert_eq!(t.lines_seen, 2);
        assert_eq!(t.events_parsed, 1);

        let empty = parse_stream_json("").unwrap();
        assert_eq!(empty.final_text, "");
        assert_eq!(empty.lines_seen, 0);
    }
}
