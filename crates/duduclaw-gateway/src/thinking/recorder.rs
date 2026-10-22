//! Producer helper: turn a runtime's thinking signals into
//! [`ThinkingEvent`]s with block ids and gateway-clock timing.
//!
//! No vendor puts timing in its stream, so the recorder stamps `started_at`
//! when a block starts and the gate derives `duration_ms` at its end. Each
//! call goes straight to [`super::emit`] (a no-op outside a turn scope);
//! tests pass their own emitter.

use std::sync::Arc;

use super::event::{TURN_BLOCK_ID, ThinkingEvent, ThinkingKind, ThinkingPhase};

type Emitter = Arc<dyn Fn(ThinkingEvent) + Send + Sync>;

/// One runtime call's thinking, block by block.
pub struct Recorder {
    source: &'static str,
    emit: Emitter,
    next_block: usize,
    open: Option<(String, ThinkingKind)>,
}

impl Recorder {
    /// Emit into the current turn's sink (if any).
    pub fn new(source: &'static str) -> Self {
        Self::with_emitter(source, Arc::new(super::emit))
    }

    /// Emit through `emit` (tests, or a producer that buffers).
    pub fn with_emitter(source: &'static str, emit: Emitter) -> Self {
        Self {
            source,
            emit,
            next_block: 0,
            open: None,
        }
    }

    fn event(&self, block: &str, phase: ThinkingPhase, kind: ThinkingKind) -> ThinkingEvent {
        ThinkingEvent::new(block, phase, kind, self.source)
    }

    /// Open a new block (closing a still-open one first, without text).
    pub fn start(&mut self, kind: ThinkingKind) {
        if self.open.is_some() {
            self.end(None, None);
        }
        let id = format!("t{}", self.next_block);
        self.next_block += 1;
        let ev = ThinkingEvent {
            started_at: Some(super::now_ms()),
            ..self.event(&id, ThinkingPhase::Start, kind)
        };
        self.open = Some((id, kind));
        (self.emit)(ev);
    }

    /// Append text to the open block (opens a `Summary` block if none).
    pub fn delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (id, kind) = self.ensure_open(ThinkingKind::Summary);
        (self.emit)(ThinkingEvent {
            text: Some(text.to_string()),
            ..self.event(&id, ThinkingPhase::Update, kind)
        });
    }

    /// A running token estimate for the open block.
    pub fn tokens_estimate(&mut self, tokens: u64) {
        let (id, kind) = self.ensure_open(ThinkingKind::Omitted);
        (self.emit)(ThinkingEvent {
            tokens: Some(tokens),
            ..self.event(&id, ThinkingPhase::Update, kind)
        });
    }

    /// Close the open block. `full_text` replaces the accumulated deltas when
    /// the vendor sends the whole block at the end.
    pub fn end(&mut self, full_text: Option<&str>, tokens: Option<u64>) {
        let Some((id, kind)) = self.open.take() else {
            return;
        };
        (self.emit)(ThinkingEvent {
            text: full_text.map(str::to_string),
            tokens,
            ..self.event(&id, ThinkingPhase::End, kind)
        });
    }

    /// A whole block known only after the fact (non-streaming runtimes).
    pub fn oneshot(&mut self, kind: ThinkingKind, text: Option<&str>, tokens: Option<u64>) {
        self.start(kind);
        self.end(text, tokens);
    }

    /// The turn's total thinking tokens (vendor usage), closing any open
    /// block first.
    pub fn turn_usage(&mut self, tokens: u64) {
        self.end(None, None);
        (self.emit)(ThinkingEvent {
            tokens: Some(tokens),
            ..self.event(TURN_BLOCK_ID, ThinkingPhase::End, ThinkingKind::Omitted)
        });
    }

    fn ensure_open(&mut self, kind: ThinkingKind) -> (String, ThinkingKind) {
        if self.open.is_none() {
            self.start(kind);
        }
        self.open
            .clone()
            .unwrap_or_else(|| ("t0".to_string(), kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn rig() -> (Recorder, Arc<Mutex<Vec<ThinkingEvent>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&got);
        let rec = Recorder::with_emitter(
            "claude_cli",
            Arc::new(move |ev| sink.lock().unwrap().push(ev)),
        );
        (rec, got)
    }

    #[test]
    fn blocks_get_ids_timing_and_phases() {
        let (mut rec, got) = rig();
        rec.start(ThinkingKind::Summary);
        rec.delta("first ");
        rec.delta("");
        rec.delta("second");
        rec.end(None, Some(12));
        rec.oneshot(ThinkingKind::Omitted, None, Some(40));
        rec.turn_usage(52);
        let got = got.lock().unwrap();
        let shape: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|e| (e.block_id.as_str(), e.phase.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                ("t0", "start", "summary"),
                ("t0", "update", "summary"),
                ("t0", "update", "summary"),
                ("t0", "end", "summary"),
                ("t1", "start", "omitted"),
                ("t1", "end", "omitted"),
                ("turn", "end", "omitted"),
            ]
        );
        assert!(got[0].started_at.is_some());
        assert_eq!(got[1].text.as_deref(), Some("first "));
        assert_eq!(got[3].tokens, Some(12));
        assert_eq!(got[6].tokens, Some(52));
        assert!(got.iter().all(|e| e.source == "claude_cli"));
    }

    #[test]
    fn delta_and_tokens_open_a_block_and_start_closes_the_previous() {
        let (mut rec, got) = rig();
        rec.delta("x");
        rec.start(ThinkingKind::Raw);
        rec.tokens_estimate(5);
        rec.end(Some("full"), None);
        rec.end(None, None); // nothing open: no event
        let got = got.lock().unwrap();
        let shape: Vec<(&str, &str)> = got
            .iter()
            .map(|e| (e.block_id.as_str(), e.phase.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                ("t0", "start"),
                ("t0", "update"),
                ("t0", "end"),
                ("t1", "start"),
                ("t1", "update"),
                ("t1", "end")
            ]
        );
        assert_eq!(got[4].tokens, Some(5));
        assert_eq!(got[5].text.as_deref(), Some("full"));
    }

    #[test]
    fn default_recorder_outside_a_turn_is_a_no_op() {
        // No THINKING_SINK scope: emit() must silently do nothing.
        let mut rec = Recorder::new("codex");
        rec.oneshot(ThinkingKind::Summary, Some("text"), Some(1));
        rec.turn_usage(1);
    }
}
