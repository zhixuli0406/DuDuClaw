//! The thinking summary a WebChat `assistant_done` frame carries.
//!
//! Folded from the `thinking` events the socket actually sent this turn, so a
//! viewer never learns more here than the frames already told it: token count,
//! number of thinking blocks and which kinds they ended as. No text.

use serde::{Deserialize, Serialize};

use super::event::ThinkingEvent;

/// `assistant_done.thinking`: `{"tokens":1240,"blocks":2,"kinds":["summary"]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneMeta {
    /// Whole-turn usage when reported, else the sum of each block's largest
    /// count. Absent when nothing reported tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Distinct thinking blocks (the whole-turn usage event is not one).
    pub blocks: u32,
    /// Each block's last kind, deduplicated, in the order blocks started.
    pub kinds: Vec<String>,
}

/// Accumulates [`DoneMeta`] over one turn.
#[derive(Debug, Default)]
pub struct DoneMetaBuilder {
    /// (block id, last kind, largest token count) in first-seen order.
    blocks: Vec<(String, &'static str, Option<u64>)>,
    turn_tokens: Option<u64>,
    seen: bool,
}

impl DoneMetaBuilder {
    /// Note one event the viewer was sent.
    pub fn observe(&mut self, ev: &ThinkingEvent) {
        self.seen = true;
        if ev.is_turn_usage() {
            if ev.tokens.is_some() {
                self.turn_tokens = ev.tokens;
            }
            return;
        }
        let kind = ev.kind.as_str();
        match self.blocks.iter_mut().find(|(id, _, _)| *id == ev.block_id) {
            Some(entry) => {
                entry.1 = kind;
                entry.2 = max_opt(entry.2, ev.tokens);
            }
            None => self.blocks.push((ev.block_id.clone(), kind, ev.tokens)),
        }
    }

    /// The summary, or `None` when no thinking event was sent this turn
    /// (the frame then has no `thinking` key, as before).
    pub fn build(&self) -> Option<DoneMeta> {
        if !self.seen {
            return None;
        }
        let block_sum: Option<u64> = self
            .blocks
            .iter()
            .filter_map(|(_, _, t)| *t)
            .reduce(|a, b| a.saturating_add(b));
        let mut kinds: Vec<String> = Vec::new();
        for (_, kind, _) in &self.blocks {
            if !kinds.iter().any(|k| k == kind) {
                kinds.push((*kind).to_string());
            }
        }
        Some(DoneMeta {
            tokens: self.turn_tokens.or(block_sum),
            blocks: u32::try_from(self.blocks.len()).unwrap_or(u32::MAX),
            kinds,
        })
    }
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking::{ThinkingKind, ThinkingPhase};

    fn ev(block: &str, kind: ThinkingKind, tokens: Option<u64>) -> ThinkingEvent {
        ThinkingEvent {
            tokens,
            ..ThinkingEvent::new(block, ThinkingPhase::Update, kind, "claude_cli")
        }
    }

    #[test]
    fn nothing_seen_builds_none() {
        assert_eq!(DoneMetaBuilder::default().build(), None);
    }

    #[test]
    fn counts_blocks_kinds_and_block_token_sum() {
        let mut b = DoneMetaBuilder::default();
        b.observe(&ev("t0", ThinkingKind::Summary, Some(100)));
        b.observe(&ev("t0", ThinkingKind::Summary, Some(300)));
        b.observe(&ev("t1", ThinkingKind::Summary, Some(50)));
        b.observe(&ev("t2", ThinkingKind::Omitted, None));
        assert_eq!(
            b.build(),
            Some(DoneMeta {
                tokens: Some(350),
                blocks: 3,
                kinds: vec!["summary".into(), "omitted".into()],
            })
        );
    }

    #[test]
    fn turn_usage_wins_and_is_not_a_block() {
        let mut b = DoneMetaBuilder::default();
        b.observe(&ev("t0", ThinkingKind::Summary, Some(100)));
        b.observe(&ev("turn", ThinkingKind::Progress, Some(1240)));
        let meta = b.build().unwrap();
        assert_eq!(meta.tokens, Some(1240));
        assert_eq!(meta.blocks, 1);
        assert_eq!(meta.kinds, vec!["summary".to_string()]);
    }

    #[test]
    fn a_block_refused_later_reports_its_final_kind() {
        let mut b = DoneMetaBuilder::default();
        b.observe(&ev("t0", ThinkingKind::Summary, None));
        b.observe(&ev("t0", ThinkingKind::Redacted, None));
        let meta = b.build().unwrap();
        assert_eq!(meta.kinds, vec!["redacted".to_string()]);
        assert_eq!(meta.tokens, None);
    }

    #[test]
    fn wire_shape() {
        let meta = DoneMeta {
            tokens: None,
            blocks: 0,
            kinds: vec![],
        };
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            serde_json::json!({"blocks": 0, "kinds": []})
        );
    }
}
