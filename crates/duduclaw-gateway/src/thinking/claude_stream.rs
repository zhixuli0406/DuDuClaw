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

/// Per-spawn state.
pub struct ClaudeStreamThinking {
    rec: Recorder,
    /// Content-block index of the thinking block being streamed.
    open_index: Option<u64>,
    open_has_text: bool,
    /// Thinking blocks already reported from `stream_event`s that the
    /// complete `assistant` message will repeat (skip that many).
    streamed_blocks: usize,
    /// Any `stream_event` seen (partial messages are on).
    streaming: bool,
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
            streaming: false,
        }
    }

    /// Look at one parsed stream-json line.
    pub fn observe(&mut self, event: &Value) -> Flow {
        match event.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                self.streaming = true;
                if let Some(inner) = event.get("event") {
                    self.on_stream_event(inner);
                }
                Flow::Consumed
            }
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
                if let Some(content) = event.pointer("/message/content").and_then(Value::as_array)
                {
                    for block in content {
                        self.on_assistant_block(block);
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
            Some("content_block_start") => {
                match ev.pointer("/content_block/type").and_then(Value::as_str) {
                    Some("thinking") => {
                        self.rec.start(ThinkingKind::Summary);
                        self.open_index = index;
                        self.open_has_text = false;
                        self.streamed_blocks += 1;
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
                        self.streamed_blocks += 1;
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

    fn on_assistant_block(&mut self, block: &Value) {
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
        if self.streamed_blocks > 0 {
            // Already reported live from the partial stream.
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
