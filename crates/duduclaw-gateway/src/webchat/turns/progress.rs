//! Lossy progress with a non-lossy, bounded thinking-redaction barrier.
use super::*;
use crate::channel_reply::ProgressEvent;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MAX_VISIBLE_BLOCKS: usize = 64;

#[derive(Default)]
pub(super) struct ProgressInbox {
    latest: Latest<ProgressEvent>,
    redacted: AtomicBool,
    model: Mutex<Option<String>>,
}

impl ProgressInbox {
    pub(super) fn push(&self, event: ProgressEvent, dropped: &AtomicU64) {
        if let ProgressEvent::ModelInfo { model } = event {
            *self.model.lock().unwrap() = Some(model);
            return;
        }
        if matches!(&event, ProgressEvent::Thinking(t) if t.kind == crate::thinking::ThinkingKind::Redacted) {
            // Never lose a revocation when a later progress event replaces it.
            self.redacted.store(true, Ordering::Release);
        }
        if self.latest.replace(event).is_some() { dropped.fetch_add(1, Ordering::Relaxed); }
    }

    pub(super) async fn recv(&self) -> ProgressEvent { self.latest.recv().await }

    pub(super) fn finish(&self, dropped: &AtomicU64) -> Option<String> {
        if self.latest.take().is_some() { dropped.fetch_add(1, Ordering::Relaxed); }
        self.model.lock().unwrap().take()
    }
}

#[derive(Default)]
pub(super) struct ThinkingVisibility {
    blocks: Vec<String>,
    revoked: bool,
}

impl ThinkingVisibility {
    pub(super) fn admit(&mut self, inbox: &ProgressInbox, block: &str) -> bool {
        if self.revoked || inbox.redacted.load(Ordering::Acquire) { return false; }
        if !self.blocks.iter().any(|seen| seen == block) {
            if self.blocks.len() == MAX_VISIBLE_BLOCKS { return false; }
            self.blocks.push(block.into());
        }
        true
    }

    pub(super) async fn flush_redaction(&mut self, inbox: &ProgressInbox, sink: &mut TurnSink) {
        if self.revoked || !inbox.redacted.load(Ordering::Acquire) { return; }
        self.revoked = true;
        // Every previously admitted block is bounded and cleared, including a
        // snapshot already in the writer. Later thinking is suppressed entirely.
        for block in &self.blocks {
            let frame = serde_json::json!({
                "type":"thinking", "phase":"end", "kind":"redacted", "block":block,
            });
            let _ = sink.send(Message::Text(frame.to_string().into())).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking::{ThinkingEvent, ThinkingKind, ThinkingPhase};

    #[tokio::test]
    async fn overwritten_redaction_is_drained_before_terminal_even_when_work_finishes() {
        let (tx, mut rx) = outbound_channel();
        let mut sink = TurnSink::new(tx, "s", None, "m");
        let inbox = ProgressInbox::default();
        let mut visible = ThinkingVisibility::default();
        assert!(visible.admit(&inbox, "t0"));
        // A previously visible snapshot is also pending in the outbound slot.
        sink.send(Message::Text(r#"{"type":"thinking","block":"t0","kind":"summary","text":"old"}"#.into())).await.unwrap();
        inbox.push(ProgressEvent::Thinking(ThinkingEvent::new(
            "t0", ThinkingPhase::End, ThinkingKind::Redacted, "test",
        )), &sink.progress_dropped);
        for _ in 0..1000 { inbox.push(ProgressEvent::Keepalive, &sink.progress_dropped); }
        // Exercise the same final drain used when work wins the select race.
        inbox.finish(&sink.progress_dropped);
        let mut protected_sink = sink.clone().acknowledged();
        tokio::time::timeout(std::time::Duration::from_millis(100),
            visible.flush_redaction(&inbox, &mut protected_sink)).await
            .expect("redaction must enqueue as control without waiting for ack");
        assert!(!visible.admit(&inbox, "t1"));
        sink.send(Message::Text(r#"{"type":"progress"}"#.into())).await.unwrap();
        sink.send(Message::Text(r#"{"type":"assistant_done"}"#.into())).await.unwrap();
        let frame = rx.recv().await.unwrap();
        assert!(frame.ack.is_none());
        let Message::Text(text) = frame.message else { panic!("text"); };
        let clear: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(clear["kind"], "redacted");
        assert_eq!(clear["block"], "t0");
        let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
        assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"], "assistant_done");
    }

    #[test]
    fn tracked_thinking_blocks_are_bounded() {
        let inbox = ProgressInbox::default();
        let mut visible = ThinkingVisibility::default();
        for index in 0..MAX_VISIBLE_BLOCKS { assert!(visible.admit(&inbox, &index.to_string())); }
        assert!(!visible.admit(&inbox, "overflow"));
        assert!(visible.admit(&inbox, "0"));
        assert_eq!(visible.blocks.len(), MAX_VISIBLE_BLOCKS);
    }
}
