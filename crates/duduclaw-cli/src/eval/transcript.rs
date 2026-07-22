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
//!
//! ## `tool_result` pairing (WP4 GroundEval, arXiv:2606.22737)
//!
//! `user` events carry `tool_result` blocks that answer a prior `tool_use`.
//! Pairing mirrors the gateway's `StepTracker::ingest`
//! (`channel_reply.rs`): match by `tool_use_id` when present, otherwise fall
//! back to the most recently opened, still-unresolved call. This keeps the
//! shipped `evals/examples/greeting-replay.transcript.jsonl` fixture (which
//! predates ids on either block) working unchanged.

/// One `tool_use` content block observed in the transcript, in order.
#[derive(Debug, Clone, Default)]
pub struct ToolInvocation {
    /// Tool name as reported by the CLI (e.g. `Bash`,
    /// `mcp__duduclaw__tasks_create`).
    pub name: String,
    /// The tool input payload (opaque JSON; kept for report context).
    pub input: serde_json::Value,
    /// `tool_use` block id (`message.content[].id`), when the CLI emitted
    /// one. Used to pair this call with its `tool_result`; retained after
    /// pairing so reports can reference the originating block.
    #[allow(dead_code)]
    pub id: Option<String>,
    /// The paired `tool_result` content, when the transcript has one. `None`
    /// for legacy transcripts recorded before result capture existed, or
    /// when no `tool_result` block ever referenced this call.
    pub result_text: Option<String>,
    /// Whether the paired `tool_result` carried `is_error: true`. `false`
    /// when unpaired (never treat an unproven call as an error).
    pub is_error: bool,
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
    // Outstanding (unresolved) tool calls, innermost last:
    // (tool_use_id-or-empty, index into `t.tool_uses`). Mirrors
    // `channel_reply::StepTracker::open`.
    let mut open: Vec<(String, usize)> = Vec::new();

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
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let idx = t.tool_uses.len();
                            open.push((id.clone().unwrap_or_default(), idx));
                            t.tool_uses.push(ToolInvocation {
                                name,
                                input,
                                id,
                                result_text: None,
                                is_error: false,
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let Some(content) = event
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                else {
                    continue;
                };
                for block in content {
                    if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let id = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    // Match by id when present; otherwise (or when the id
                    // doesn't match any outstanding call) fall back to the
                    // most recently opened call — same tolerant fallback as
                    // `StepTracker::ingest`.
                    let popped = if !id.is_empty() {
                        open.iter()
                            .rposition(|(oid, _)| oid == id)
                            .map(|pos| open.remove(pos))
                    } else {
                        open.pop()
                    }
                    .or_else(|| open.pop());
                    if let Some((_, idx)) = popped {
                        if let Some(inv) = t.tool_uses.get_mut(idx) {
                            inv.result_text = extract_tool_result_text(block);
                            inv.is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                        }
                    }
                    // No outstanding call to match (empty stack) — ignore;
                    // never panics on a malformed/reordered transcript.
                }
            }
            _ => {}
        }
    }

    Ok(t)
}

/// Extract the human-readable text of a `tool_result` block. `content` is
/// either a plain string or an array of content blocks (Anthropic Messages
/// API `tool_result` shape); text blocks are concatenated in order.
/// Anything else (missing, non-text array, etc.) is `None` — never a panic.
fn extract_tool_result_text(block: &serde_json::Value) -> Option<String> {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(items)) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .map(String::from)
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
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

    // ── WP4 GroundEval: tool_result pairing ─────────────────────

    fn user_tool_result(inner: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"content\":[{inner}]}}}}")
    }

    #[test]
    fn pairs_tool_result_by_id() {
        let stdout = [
            assistant(
                "{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"mcp__duduclaw__memory_search\",\"input\":{}}",
            ),
            user_tool_result(
                "{\"type\":\"tool_result\",\"tool_use_id\":\"tu_1\",\"content\":\"policy: 30 days\",\"is_error\":false}",
            ),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.tool_uses.len(), 1);
        assert_eq!(t.tool_uses[0].id.as_deref(), Some("tu_1"));
        assert_eq!(t.tool_uses[0].result_text.as_deref(), Some("policy: 30 days"));
        assert!(!t.tool_uses[0].is_error);
    }

    #[test]
    fn pairs_tool_result_by_fallback_when_id_absent() {
        // Mirrors the shipped `greeting-replay.transcript.jsonl` fixture:
        // neither block carries an id.
        let stdout = [
            assistant("{\"type\":\"tool_use\",\"name\":\"mcp__duduclaw__tasks_create\",\"input\":{}}"),
            user_tool_result("{\"type\":\"tool_result\",\"content\":\"task created: #42\"}"),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(
            t.tool_uses[0].result_text.as_deref(),
            Some("task created: #42")
        );
        assert!(!t.tool_uses[0].is_error);
    }

    #[test]
    fn tool_result_content_array_of_text_blocks_is_concatenated() {
        let stdout = [
            assistant(
                "{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"Read\",\"input\":{}}",
            ),
            user_tool_result(
                "{\"type\":\"tool_result\",\"tool_use_id\":\"tu_1\",\"content\":[{\"type\":\"text\",\"text\":\"line1\"},{\"type\":\"text\",\"text\":\"line2\"}]}",
            ),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.tool_uses[0].result_text.as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn tool_result_is_error_flag_is_captured() {
        let stdout = [
            assistant("{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"Bash\",\"input\":{}}"),
            user_tool_result(
                "{\"type\":\"tool_result\",\"tool_use_id\":\"tu_1\",\"content\":\"command not found\",\"is_error\":true}",
            ),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert!(t.tool_uses[0].is_error);
    }

    #[test]
    fn unmatched_tool_result_does_not_panic_or_pair() {
        // A `tool_result` with no outstanding open call (empty stack) —
        // must be silently ignored, not panic.
        let stdout = user_tool_result(
            "{\"type\":\"tool_result\",\"tool_use_id\":\"ghost\",\"content\":\"x\"}",
        );
        let t = parse_stream_json(&stdout).unwrap();
        assert!(t.tool_uses.is_empty());
    }

    #[test]
    fn legacy_transcript_without_user_events_leaves_result_text_none() {
        // Old-format transcripts (no `user`/tool_result events at all) must
        // parse cleanly with result_text=None, never panic.
        let stdout = assistant(
            "{\"type\":\"tool_use\",\"name\":\"mcp__duduclaw__memory_search\",\"input\":{}}",
        );
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.tool_uses.len(), 1);
        assert!(t.tool_uses[0].result_text.is_none());
        assert!(!t.tool_uses[0].is_error);
    }

    #[test]
    fn nested_parallel_calls_pair_by_id_independently() {
        // Two tool_use blocks in one assistant turn (parallel calls), each
        // with a distinct id — results must not cross-pair.
        let stdout = [
            assistant(
                "{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"Read\",\"input\":{}},\
                 {\"type\":\"tool_use\",\"id\":\"b\",\"name\":\"Bash\",\"input\":{}}",
            ),
            user_tool_result(
                "{\"type\":\"tool_result\",\"tool_use_id\":\"b\",\"content\":\"bash out\"},\
                 {\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"content\":\"read out\"}",
            ),
        ]
        .join("\n");
        let t = parse_stream_json(&stdout).unwrap();
        assert_eq!(t.tool_uses[0].result_text.as_deref(), Some("read out"));
        assert_eq!(t.tool_uses[1].result_text.as_deref(), Some("bash out"));
    }
}
