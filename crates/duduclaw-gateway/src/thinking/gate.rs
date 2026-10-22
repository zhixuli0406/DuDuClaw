//! The per-turn gate between producers and the viewer.
//!
//! Producers emit raw [`ThinkingEvent`]s (deltas, full texts). The gate
//! decides, per [`SinkLevel`], what leaves:
//!
//! - `Off`: nothing.
//! - `Signal` / `Status`: timing and token events without any text; no
//!   masking work is done at all.
//! - `Summary`: per block, the accumulated text is re-masked on every flush
//!   ([`super::sanitize`]); the tail after the last whitespace (≤ 64 chars)
//!   is held back until the block ends so a secret still being streamed is
//!   never shown half-written; `Update`s are throttled to one per 400 ms per
//!   block (`Start` / `End` pass at once); text is capped per block (1,500
//!   chars, raw reasoning 1,200) and per turn (`[thinking] max_chars`).
//!   Raw reasoning is shown only with redaction enabled — otherwise the text
//!   is dropped and the event says so (`raw_needs_redaction`). Once the
//!   outbound checks refuse any text, that block and every later block of
//!   the turn become `Redacted` with no text (sticky).
//! - With redaction enabled, an `Update` additionally stops at the last line
//!   or sentence end (`\n。！？.!?`), so a rule-engine match that spans
//!   whitespace (a spaced phone number) is never shown half-masked. Each
//!   `Update` / `End` carries the whole block so far: the viewer replaces the
//!   block's text with the latest snapshot (never appends).
//! - The per-turn budget counts what every block currently shows (open and
//!   finished), so many blocks at once cannot exceed `max_chars`. A finished
//!   block's buffer is dropped.

use std::collections::HashMap;

use super::event::{ThinkingEvent, ThinkingKind, ThinkingPhase};
use super::policy::SinkLevel;
use super::sanitize::{
    SanitizeCtx, Sanitized, sanitize_for_display, split_at_sentence, split_hold_back,
};
use crate::channel_reply::OutboundBlock;

/// Text cap for one block (chars).
pub const BLOCK_CAP_CHARS: usize = 1_500;
/// Text cap for one block of raw (open-model) reasoning (chars).
pub const RAW_BLOCK_CAP_CHARS: usize = 1_200;
/// Minimum gap between two `Update`s of the same block.
pub const UPDATE_THROTTLE_MS: u64 = 400;
/// Longest tail held back from a growing block.
pub const HOLD_BACK_TAIL_CHARS: usize = 64;
/// Most text kept per block before masking (chars). Display is capped far
/// lower; this bounds memory and per-flush masking work for runaway blocks.
pub const MAX_RAW_CHARS: usize = 4 * BLOCK_CAP_CHARS;

#[derive(Debug, Default)]
struct BlockState {
    raw: String,
    raw_chars: usize,
    /// More text arrived than [`MAX_RAW_CHARS`] keeps.
    overflowed: bool,
    started_at: Option<u64>,
    last_update_ms: Option<u64>,
    /// Chars of the largest snapshot shown for this block so far.
    shown_chars: usize,
}

impl BlockState {
    fn push(&mut self, delta: &str) {
        let room = MAX_RAW_CHARS.saturating_sub(self.raw_chars);
        let n = delta.chars().count();
        if n <= room {
            self.raw.push_str(delta);
            self.raw_chars += n;
        } else {
            self.raw
                .push_str(&duduclaw_core::truncate_chars(delta, room));
            self.raw_chars = MAX_RAW_CHARS;
            self.overflowed = true;
        }
    }
}

/// What one [`Gate::process`] call produced.
#[derive(Debug, Default)]
pub struct GateOutput {
    /// Events to forward, in order.
    pub events: Vec<ThinkingEvent>,
    /// Set the first time the outbound checks refuse this turn's thinking —
    /// the caller writes the audit row (rule names only).
    pub newly_blocked: Option<OutboundBlock>,
}

/// Per-turn state. Not shared across turns.
#[derive(Debug)]
pub struct Gate {
    level: SinkLevel,
    max_turn_chars: usize,
    redaction_on: bool,
    blocks: HashMap<String, BlockState>,
    sticky_redacted: bool,
    /// Chars shown by blocks that already ended.
    closed_chars: usize,
}

impl Gate {
    pub fn new(level: SinkLevel, max_turn_chars: usize, redaction_on: bool) -> Self {
        Self {
            level,
            max_turn_chars,
            redaction_on,
            blocks: HashMap::new(),
            sticky_redacted: false,
            closed_chars: 0,
        }
    }

    /// Process one producer event at gateway time `now_ms`.
    pub fn process(&mut self, ev: ThinkingEvent, now_ms: u64, ctx: &SanitizeCtx<'_>) -> GateOutput {
        self.process_capped(ev, now_ms, ctx, SinkLevel::Summary)
    }

    /// [`Self::process`] with a further cap for this event — the stricter of
    /// the turn's level and `cap` applies (e.g. the employee that actually
    /// answers allows less than the one the turn started with).
    pub fn process_capped(
        &mut self,
        ev: ThinkingEvent,
        now_ms: u64,
        ctx: &SanitizeCtx<'_>,
        cap: SinkLevel,
    ) -> GateOutput {
        let out = match self.level.min(cap) {
            SinkLevel::Off => GateOutput::default(),
            SinkLevel::Signal | SinkLevel::Status => self.textless(&ev, now_ms),
            SinkLevel::Summary => self.summary(&ev, now_ms, ctx),
        };
        if ev.phase == ThinkingPhase::End && !ev.is_turn_usage() {
            if let Some(done) = self.blocks.remove(&ev.block_id) {
                self.closed_chars += done.shown_chars;
            }
        }
        out
    }

    /// Track the block and apply the `Update` throttle. `false` ⇒ drop.
    fn admit(&mut self, ev: &ThinkingEvent, now_ms: u64) -> bool {
        let state = self.blocks.entry(ev.block_id.clone()).or_default();
        if state.started_at.is_none() {
            state.started_at = ev.started_at.or(Some(now_ms));
        }
        if ev.phase != ThinkingPhase::Update {
            return true;
        }
        let due = state
            .last_update_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= UPDATE_THROTTLE_MS);
        if due {
            state.last_update_ms = Some(now_ms);
        }
        due
    }

    fn with_duration(&self, ev: &ThinkingEvent, now_ms: u64) -> ThinkingEvent {
        let mut ev = ev.clone();
        let started = self.blocks.get(&ev.block_id).and_then(|s| s.started_at);
        if ev.started_at.is_none() {
            ev.started_at = started;
        }
        if ev.phase == ThinkingPhase::End && ev.duration_ms.is_none() {
            ev.duration_ms = ev.started_at.map(|s| now_ms.saturating_sub(s));
        }
        ev
    }

    fn textless(&mut self, ev: &ThinkingEvent, now_ms: u64) -> GateOutput {
        let mut out = GateOutput::default();
        if self.admit(ev, now_ms) {
            out.events
                .push(self.with_duration(ev, now_ms).without_text());
        }
        out
    }

    /// Chars still available to `block` this turn.
    fn budget_for(&self, block: &str) -> usize {
        let others: usize = self
            .blocks
            .iter()
            .filter(|(id, _)| id.as_str() != block)
            .map(|(_, s)| s.shown_chars)
            .sum();
        self.max_turn_chars
            .saturating_sub(self.closed_chars + others)
    }

    /// The raw text this event may show, before masking.
    fn candidate_text(&self, ev: &ThinkingEvent) -> Option<String> {
        match ev.phase {
            ThinkingPhase::Start => None,
            ThinkingPhase::Update => {
                let raw = self
                    .blocks
                    .get(&ev.block_id)
                    .map(|s| s.raw.as_str())
                    .unwrap_or("");
                let held = split_hold_back(raw, HOLD_BACK_TAIL_CHARS).0;
                let shown = if self.redaction_on {
                    split_at_sentence(held).0
                } else {
                    held
                };
                Some(shown.to_string())
            }
            ThinkingPhase::End => {
                let full = ev
                    .text
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| duduclaw_core::truncate_chars(t, MAX_RAW_CHARS).to_string());
                let acc = self.blocks.get(&ev.block_id).map(|s| s.raw.clone());
                full.or(acc)
            }
        }
    }

    fn summary(&mut self, ev: &ThinkingEvent, now_ms: u64, ctx: &SanitizeCtx<'_>) -> GateOutput {
        let mut out = GateOutput::default();
        if ev.is_turn_usage() {
            out.events.push(ev.clone().without_text());
            return out;
        }
        // Accumulate first, so a throttled delta still counts toward the
        // next snapshot — but only when the text could ever be shown.
        let will_show =
            !self.sticky_redacted && !(ev.kind == ThinkingKind::Raw && !self.redaction_on);
        if ev.phase == ThinkingPhase::Update && will_show {
            if let Some(delta) = ev.text.as_deref() {
                self.blocks
                    .entry(ev.block_id.clone())
                    .or_default()
                    .push(delta);
            }
        }
        if !self.admit(ev, now_ms) {
            return out;
        }
        let timed = self.with_duration(ev, now_ms);
        if self.sticky_redacted {
            out.events.push(redacted(timed));
            return out;
        }
        if timed.kind == ThinkingKind::Raw && !self.redaction_on {
            let mut e = timed.without_text();
            e.raw_needs_redaction = true;
            out.events.push(e);
            return out;
        }
        let shown = self.candidate_text(&timed);
        let overflowed = self
            .blocks
            .get(&timed.block_id)
            .is_some_and(|s| s.overflowed)
            || timed
                .text
                .as_deref()
                .is_some_and(|t| t.chars().count() > MAX_RAW_CHARS);
        let block_id = timed.block_id.clone();
        let bare = timed.without_text();
        let Some(shown) = shown.filter(|t| !t.trim().is_empty()) else {
            out.events.push(bare);
            return out;
        };
        let block_cap = if bare.kind == ThinkingKind::Raw {
            RAW_BLOCK_CAP_CHARS
        } else {
            BLOCK_CAP_CHARS
        };
        let cap = block_cap.min(self.budget_for(&block_id));
        if cap == 0 {
            out.events.push(ThinkingEvent {
                truncated: true,
                ..bare
            });
            return out;
        }
        match sanitize_for_display(&shown, ctx, cap) {
            Sanitized::Blocked(block) => {
                self.sticky_redacted = true;
                out.newly_blocked = Some(block);
                out.events.push(redacted(bare));
            }
            Sanitized::Text {
                text,
                truncated,
                line,
            } => {
                if let Some(state) = self.blocks.get_mut(&block_id) {
                    state.shown_chars = state.shown_chars.max(text.chars().count());
                }
                out.events.push(ThinkingEvent {
                    text: (!text.is_empty()).then_some(text),
                    line,
                    truncated: truncated || overflowed,
                    ..bare
                });
            }
        }
        out
    }
}

fn redacted(ev: ThinkingEvent) -> ThinkingEvent {
    ThinkingEvent {
        kind: ThinkingKind::Redacted,
        ..ev.without_text()
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
