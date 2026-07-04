//! Minimal SSE plumbing shared by the Anthropic and OpenAI-compat streaming
//! paths. Line assembly is byte-buffer based (no raw byte-index slicing of
//! `str` — project convention) and the per-provider event parsers are pure
//! state machines, unit-testable without HTTP.

use std::collections::VecDeque;

use futures_util::stream::BoxStream;
use futures_util::StreamExt;

use crate::error::{classify_transport, LlmError};
use crate::types::StreamEvent;

/// Assembles complete lines from arbitrary byte chunks.
#[derive(Default)]
pub(crate) struct SseLineBuffer {
    buf: Vec<u8>,
}

impl SseLineBuffer {
    /// Feed a chunk; returns every complete line (trimmed, no terminator).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            lines.push(String::from_utf8_lossy(&line_bytes).trim().to_string());
        }
        lines
    }

    /// Flush a trailing line that arrived without a newline terminator.
    pub fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.buf);
        let line = String::from_utf8_lossy(&rest).trim().to_string();
        (!line.is_empty()).then_some(line)
    }
}

/// Extract the payload of a `data: ...` SSE line. Comments (`:`) and other
/// fields (`event:`, `id:`) return `None`.
pub(crate) fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data: ")
        .or_else(|| line.strip_prefix("data:"))
        .map(str::trim)
}

/// A pure per-provider SSE state machine.
pub(crate) trait SseParser: Send + 'static {
    /// Process one SSE line, pushing zero or more events.
    fn on_line(&mut self, line: &str, out: &mut Vec<StreamEvent>);
    /// True once the provider signalled end-of-stream (`[DONE]` /
    /// `message_stop`). `finalize` is called at that point or at EOF.
    fn finished(&self) -> bool;
    /// Build the terminal `Done` event from accumulated state.
    fn finalize(&mut self) -> Result<StreamEvent, LlmError>;
}

/// Drive a `reqwest` byte stream through an [`SseParser`], yielding
/// normalized [`StreamEvent`]s and terminating with the parser's `Done`
/// event (or an error). Pending deltas always drain before finalization.
pub(crate) fn drive_sse<P: SseParser>(
    response: reqwest::Response,
    parser: P,
) -> BoxStream<'static, Result<StreamEvent, LlmError>> {
    struct Ctx<P> {
        bytes: BoxStream<'static, Result<Vec<u8>, reqwest::Error>>,
        lines: SseLineBuffer,
        parser: P,
        pending: VecDeque<StreamEvent>,
        eof: bool,
        terminated: bool,
    }

    let ctx = Ctx {
        bytes: Box::pin(response.bytes_stream().map(|r| r.map(|b| b.to_vec()))),
        lines: SseLineBuffer::default(),
        parser,
        pending: VecDeque::new(),
        eof: false,
        terminated: false,
    };

    Box::pin(futures_util::stream::unfold(ctx, |mut ctx| async move {
        loop {
            if let Some(ev) = ctx.pending.pop_front() {
                return Some((Ok(ev), ctx));
            }
            if ctx.terminated {
                return None;
            }
            if ctx.eof || ctx.parser.finished() {
                ctx.terminated = true;
                let done = ctx.parser.finalize();
                return Some((done, ctx));
            }
            match ctx.bytes.next().await {
                Some(Ok(chunk)) => {
                    let mut out = Vec::new();
                    for line in ctx.lines.push(&chunk) {
                        ctx.parser.on_line(&line, &mut out);
                        if ctx.parser.finished() {
                            break;
                        }
                    }
                    ctx.pending.extend(out);
                }
                Some(Err(e)) => {
                    ctx.terminated = true;
                    return Some((Err(classify_transport(&e)), ctx));
                }
                None => {
                    let mut out = Vec::new();
                    if let Some(line) = ctx.lines.flush() {
                        ctx.parser.on_line(&line, &mut out);
                    }
                    ctx.pending.extend(out);
                    ctx.eof = true;
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_buffer_assembles_split_chunks() {
        let mut b = SseLineBuffer::default();
        assert!(b.push(b"data: {\"a\":").is_empty());
        let lines = b.push(b"1}\n\ndata: [DONE]\n");
        assert_eq!(
            lines,
            vec![
                "data: {\"a\":1}".to_string(),
                String::new(),
                "data: [DONE]".to_string()
            ]
        );
        assert!(b.flush().is_none());
    }

    #[test]
    fn line_buffer_flush_returns_unterminated_tail() {
        let mut b = SseLineBuffer::default();
        assert!(b.push(b"data: tail-without-newline").is_empty());
        assert_eq!(b.flush().as_deref(), Some("data: tail-without-newline"));
    }

    #[test]
    fn sse_data_extraction() {
        assert_eq!(sse_data("data: {\"x\":1}"), Some("{\"x\":1}"));
        assert_eq!(sse_data("data:[DONE]"), Some("[DONE]"));
        assert_eq!(sse_data("event: message_start"), None);
        assert_eq!(sse_data(": keepalive"), None);
    }

    #[test]
    fn line_buffer_is_utf8_safe_across_chunk_splits() {
        // Split a 3-byte CJK char across chunks; the assembled full line must
        // decode correctly (from_utf8_lossy only runs on complete lines).
        let s = "data: 嘟嘟\n".as_bytes();
        let mut b = SseLineBuffer::default();
        assert!(b.push(&s[..8]).is_empty()); // cuts mid-codepoint
        let lines = b.push(&s[8..]);
        assert_eq!(lines, vec!["data: 嘟嘟".to_string()]);
    }
}
