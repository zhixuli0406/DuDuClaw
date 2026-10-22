//! Inline `<think>…</think>` reasoning from open models, as thinking text.
//!
//! Open models served over an OpenAI-compatible API often put their
//! reasoning at the start of `content` inside `<think>` tags. The answer
//! path already strips that block (`duduclaw_core::think_tags`); this module
//! only reads it, so it can be reported as `Raw` thinking:
//!
//! - [`leading_think_text`] for a complete answer;
//! - [`ThinkSplitter`] for a stream of content deltas. It only starts when
//!   the content, after leading whitespace, begins with `<think>`; a missing
//!   closing tag is tolerated (everything is reasoning). A `<think>` later in
//!   the text is ordinary content.
//!
//! Neither changes the answer text in any way.

use duduclaw_core::think_tags::{THINK_CLOSE, THINK_OPEN};

/// The reasoning inside a leading `<think>` block, if the text has one
/// (unterminated ⇒ everything after the tag).
pub fn leading_think_text(text: &str) -> Option<&str> {
    let after_open = text.trim_start().strip_prefix(THINK_OPEN)?;
    let inner = match after_open.find(THINK_CLOSE) {
        Some(end) => &after_open[..end],
        None => after_open,
    };
    (!inner.trim().is_empty()).then_some(inner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Only whitespace (or a prefix of `<think>`) so far.
    Undecided,
    InThink,
    /// Past the block, or the content never had one.
    Done,
}

/// Streaming splitter for content deltas. Returns the reasoning part of each
/// delta (possibly empty); the content part is not needed by any caller and
/// is not returned.
#[derive(Debug)]
pub struct ThinkSplitter {
    state: State,
    buf: String,
}

impl Default for ThinkSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl ThinkSplitter {
    pub fn new() -> Self {
        Self {
            state: State::Undecided,
            buf: String::new(),
        }
    }

    /// Feed one content delta; returns the reasoning text it completes.
    pub fn push(&mut self, delta: &str) -> String {
        match self.state {
            State::Done => String::new(),
            State::Undecided => {
                self.buf.push_str(delta);
                let trimmed = self.buf.trim_start();
                if let Some(rest) = trimmed.strip_prefix(THINK_OPEN) {
                    let rest = rest.to_string();
                    self.state = State::InThink;
                    self.buf.clear();
                    self.push_in_think(&rest)
                } else {
                    if trimmed.is_empty() || THINK_OPEN.starts_with(trimmed) {
                        return String::new();
                    }
                    self.state = State::Done;
                    self.buf.clear();
                    String::new()
                }
            }
            State::InThink => self.push_in_think(delta),
        }
    }

    /// End of the stream: whatever is still held inside an unterminated
    /// block is reasoning.
    pub fn finish(&mut self) -> String {
        let out = match self.state {
            State::InThink => std::mem::take(&mut self.buf),
            _ => String::new(),
        };
        self.buf.clear();
        self.state = State::Done;
        out
    }

    fn push_in_think(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        if let Some(end) = self.buf.find(THINK_CLOSE) {
            let reasoning = self.buf[..end].to_string();
            self.buf.clear();
            self.state = State::Done;
            return reasoning;
        }
        // Hold back a tail that could be the start of `</think>`. The tag is
        // ASCII, so the cut is always on a char boundary.
        let keep = (1..THINK_CLOSE.len())
            .rev()
            .find(|&k| self.buf.ends_with(&THINK_CLOSE[..k]))
            .unwrap_or(0);
        let cut = self.buf.len() - keep;
        let reasoning = self.buf[..cut].to_string();
        self.buf.drain(..cut);
        reasoning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(deltas: &[&str]) -> String {
        let mut s = ThinkSplitter::new();
        let mut out: String = deltas.iter().map(|d| s.push(d)).collect();
        out.push_str(&s.finish());
        out
    }

    #[test]
    fn leading_block_text() {
        assert_eq!(leading_think_text("\n <think>計畫</think>答案"), Some("計畫"));
        assert_eq!(leading_think_text("<think>沒有收尾"), Some("沒有收尾"));
        assert_eq!(leading_think_text("<think>  </think>答案"), None);
        assert_eq!(leading_think_text("答案 <think>x</think>"), None);
        assert_eq!(leading_think_text(""), None);
    }

    #[test]
    fn splitter_handles_tags_split_across_deltas() {
        assert_eq!(run(&["  <th", "ink>先想", "一下</th", "ink>答案"]), "先想一下");
        assert_eq!(run(&["<think>", "嘟嘟🐾", "</think>", "<think>not reasoning"]), "嘟嘟🐾");
        // Unterminated: everything after the tag.
        assert_eq!(run(&["<think>only ", "reasoning </thi"]), "only reasoning </thi");
        // Content that does not start with the tag is never reasoning.
        assert_eq!(run(&["答案 ", "<think>x</think>"]), "");
        assert_eq!(run(&["<thin", "g>"]), "");
        assert_eq!(run(&["   ", "\n"]), "");
    }

    #[test]
    fn splitter_emits_reasoning_incrementally() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<think>ab"), "ab");
        assert_eq!(s.push("c</"), "c");
        assert_eq!(s.push("think>rest"), "");
        assert_eq!(s.push("more"), "");
        assert_eq!(s.finish(), "");
    }
}
