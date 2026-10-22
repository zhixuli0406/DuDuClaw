//! Claude CLI `--output-format stream-json` thinking producer, shared by the
//! channel reply parser (`channel_reply/cli_env.rs`) and the dispatch parser
//! (`claude_runner.rs::call_claude_streaming`).
//!
//! What the CLI gives us:
//!
//! - With `--include-partial-messages` (added only when the turn's viewer
//!   reads thinking text live, see [`extra_args`]): `stream_event` lines
//!   carrying the raw Messages API stream — `content_block_start{thinking}`,
//!   `thinking_delta`s, a `signature_delta`, `content_block_stop`. These
//!   lines are consumed here and never reach the caller's other collectors
//!   (foresight, step tracker, operator capture), and their `text_delta`s are
//!   never added to the answer (the full `assistant` message still carries
//!   the text, exactly as without the flag).
//! - `system` / `thinking_tokens` progress frames with `estimated_tokens`
//!   (newer CLIs) — a running estimate, available even when no text is.
//! - The complete `assistant` message: a `thinking` block (its text is often
//!   empty on new models ⇒ `Omitted`) or a `redacted_thinking` block
//!   (⇒ `Redacted`; its ciphertext is never read).
//! - `result.usage.output_tokens_details.thinking_tokens`: the turn's total.
//!
//! Signatures and redacted data are never copied out of the event.

use std::borrow::Cow;

use serde_json::Value;

use super::event::ThinkingKind;
use super::policy::ThinkingPolicy;
use super::recorder::Recorder;

/// The CLI flag that turns on `stream_event` lines.
pub const PARTIAL_MESSAGES_FLAG: &str = "--include-partial-messages";

/// Extra argv for one `claude -p` spawn. Empty unless the turn's viewer reads
/// thinking text live, so a spawn outside a reply turn (cron, dispatch) or
/// for a channel / status-only viewer is byte-identical to before.
pub fn extra_args(policy: &ThinkingPolicy) -> &'static [&'static str] {
    if policy.wants_live() {
        &[PARTIAL_MESSAGES_FLAG]
    } else {
        &[]
    }
}

/// What the caller does with the line after [`ClaudeStreamThinking::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// A `stream_event` line: skip every other handler (`continue`).
    Consumed,
    /// Handle the line as before.
    Continue,
}

/// Is this parsed line a partial-message `stream_event`?
pub fn is_stream_event(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("stream_event")
}

/// The line to write to `claude_stream.log` (`DUDUCLAW_STREAM_DEBUG=1`):
/// a `thinking_delta` line is replaced by a marker, every other line is
/// written as it came.
pub fn debug_log_line<'a>(line: &'a str, parsed: Option<&Value>) -> Cow<'a, str> {
    let is_thinking_delta = parsed.is_some_and(|e| {
        is_stream_event(e)
            && e.pointer("/event/delta/type").and_then(Value::as_str) == Some("thinking_delta")
    });
    if is_thinking_delta {
        Cow::Borrowed(r#"{"type":"stream_event","elided":"thinking_delta"}"#)
    } else {
        Cow::Borrowed(line)
    }
}

/// The text a failure diagnostic may keep for this line, or `None` to keep
/// nothing: partial `stream_event` lines are dropped, and an `assistant`
/// line's `thinking` / `redacted_thinking` blocks are reduced to their type
/// before the line is kept, so thinking text never reaches an error message.
pub fn diagnostic_line(line: &str, parsed: Option<&Value>) -> Option<String> {
    let Some(event) = parsed else {
        return Some(line.to_string());
    };
    if is_stream_event(event) {
        return None;
    }
    let has_thinking = event
        .pointer("/message/content")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().any(is_thinking_block));
    if !has_thinking {
        return Some(line.to_string());
    }
    let mut cleaned = event.clone();
    if let Some(content) = cleaned
        .pointer_mut("/message/content")
        .and_then(Value::as_array_mut)
    {
        for block in content.iter_mut().filter(|b| is_thinking_block(b)) {
            let kind = block.get("type").cloned().unwrap_or(Value::Null);
            *block = serde_json::json!({"type": kind, "elided": true});
        }
    }
    Some(cleaned.to_string())
}

fn is_thinking_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking")
    )
}

/// Per-spawn state.
pub struct ClaudeStreamThinking {
    rec: Recorder,
    /// Content-block index of the thinking block being streamed.
    open_index: Option<u64>,
    open_has_text: bool,
    /// Thinking blocks of the current streamed message already reported from
    /// `stream_event`s — used for an `assistant` repeat that carries no
    /// message id; reset at every new message.
    streamed_blocks: usize,
    /// Message ids whose thinking was streamed (their `assistant` repeat is
    /// skipped by id, so an aborted stream never suppresses a later block).
    streamed_messages: std::collections::HashSet<String>,
    /// Id of the message being streamed (`message_start`).
    current_message: Option<String>,
    /// Any `stream_event` seen (partial messages are on).
    streaming: bool,
}

impl Drop for ClaudeStreamThinking {
    /// An early return (stream error, timeout, a dropped future) still closes
    /// an open block with the right kind.
    fn drop(&mut self) {
        self.finish();
    }
}

impl Default for ClaudeStreamThinking {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeStreamThinking {
    /// Emit into the current turn's sink (a no-op outside a reply turn).
    pub fn new() -> Self {
        Self::with_recorder(Recorder::new("claude_cli"))
    }

    pub fn with_recorder(rec: Recorder) -> Self {
        Self {
            rec,
            open_index: None,
            open_has_text: false,
            streamed_blocks: 0,
            streamed_messages: std::collections::HashSet::new(),
            current_message: None,
            streaming: false,
        }
    }

    /// Look at one parsed stream-json line.
    pub fn observe(&mut self, event: &Value) -> Flow {
        // A subagent's lines (Task tool, `parent_tool_use_id` set) belong to
        // another conversation thread: never mix them into the main blocks.
        let subagent = event
            .get("parent_tool_use_id")
            .is_some_and(|v| !v.is_null());
        match event.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                if !subagent {
                    self.streaming = true;
                    if let Some(inner) = event.get("event") {
                        self.on_stream_event(inner);
                    }
                }
                Flow::Consumed
            }
            Some("assistant") if subagent => Flow::Continue,
            Some("system")
                if event.get("subtype").and_then(Value::as_str) == Some("thinking_tokens") =>
            {
                if let Some(n) = event.get("estimated_tokens").and_then(Value::as_u64) {
                    // With partial messages on, an estimate only belongs to a
                    // block that is being streamed right now.
                    if !self.streaming || self.rec.is_open() {
                        self.rec.tokens_estimate(n);
                    }
                }
                Flow::Continue
            }
            Some("assistant") => {
                let id = event.pointer("/message/id").and_then(Value::as_str);
                // Streamed live already (matched by message id): skip.
                if id.is_some_and(|id| self.streamed_messages.contains(id)) {
                    return Flow::Continue;
                }
                let by_counter = id.is_none();
                if let Some(content) = event.pointer("/message/content").and_then(Value::as_array)
                {
                    for block in content {
                        self.on_assistant_block(block, by_counter);
                    }
                }
                Flow::Continue
            }
            Some("result") => {
                if let Some(n) = event
                    .pointer("/usage/output_tokens_details/thinking_tokens")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
                {
                    self.rec.turn_usage(n);
                }
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    /// End of the stream: close a block that never got its stop event.
    pub fn finish(&mut self) {
        if self.rec.is_open() {
            let kind = if self.open_has_text {
                ThinkingKind::Summary
            } else {
                ThinkingKind::Omitted
            };
            self.rec.end_as(kind, None, None);
        }
        self.open_index = None;
    }

    fn on_stream_event(&mut self, ev: &Value) {
        let index = ev.get("index").and_then(Value::as_u64);
        match ev.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                // A new message: leftovers of an aborted one must not
                // suppress blocks that were never streamed.
                self.finish();
                self.streamed_blocks = 0;
                self.current_message = ev
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("content_block_start") => {
                match ev.pointer("/content_block/type").and_then(Value::as_str) {
                    Some("thinking") => {
                        self.rec.start(ThinkingKind::Summary);
                        self.open_index = index;
                        self.open_has_text = false;
                        self.note_streamed();
                        if let Some(t) = ev
                            .pointer("/content_block/thinking")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            self.open_has_text = true;
                            self.rec.delta(t);
                        }
                    }
                    Some("redacted_thinking") => {
                        self.rec.oneshot(ThinkingKind::Redacted, None, None);
                        self.note_streamed();
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") if index.is_some() && index == self.open_index => {
                // `signature_delta` and `text_delta` are ignored on purpose.
                if ev.pointer("/delta/type").and_then(Value::as_str) == Some("thinking_delta") {
                    if let Some(t) = ev
                        .pointer("/delta/thinking")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                    {
                        self.open_has_text = true;
                        self.rec.delta(t);
                    }
                }
            }
            Some("content_block_stop") if index.is_some() && index == self.open_index => {
                self.finish();
            }
            _ => {}
        }
    }

    fn note_streamed(&mut self) {
        if let Some(id) = &self.current_message {
            self.streamed_messages.insert(id.clone());
        }
        self.streamed_blocks += 1;
    }

    fn on_assistant_block(&mut self, block: &Value, by_counter: bool) {
        let kind = match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty());
                Some((
                    if text.is_some() {
                        ThinkingKind::Summary
                    } else {
                        ThinkingKind::Omitted
                    },
                    text,
                ))
            }
            Some("redacted_thinking") => Some((ThinkingKind::Redacted, None)),
            _ => None,
        };
        let Some((kind, text)) = kind else {
            return;
        };
        if by_counter && self.streamed_blocks > 0 {
            // Already reported live from the partial stream (no message ids).
            self.streamed_blocks -= 1;
            return;
        }
        if self.rec.is_open() {
            // A block opened by a `thinking_tokens` estimate: this is it.
            self.rec.end_as(kind, text, None);
        } else {
            self.rec.oneshot(kind, text, None);
        }
    }
}

#[cfg(test)]
#[path = "claude_stream_tests.rs"]
pub(crate) mod tests;
