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

use std::collections::HashMap;

use super::event::{ThinkingEvent, ThinkingKind, ThinkingPhase};
use super::policy::SinkLevel;
use super::sanitize::{SanitizeCtx, Sanitized, last_line, sanitize_for_display, split_hold_back};
use crate::channel_reply::OutboundBlock;

/// Text cap for one block (chars).
pub const BLOCK_CAP_CHARS: usize = 1_500;
/// Text cap for one block of raw (open-model) reasoning (chars).
pub const RAW_BLOCK_CAP_CHARS: usize = 1_200;
/// Minimum gap between two `Update`s of the same block.
pub const UPDATE_THROTTLE_MS: u64 = 400;
/// Longest tail held back from a growing block.
pub const HOLD_BACK_TAIL_CHARS: usize = 64;

#[derive(Debug, Default)]
struct BlockState {
    raw: String,
    started_at: Option<u64>,
    last_update_ms: Option<u64>,
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
    used_chars: usize,
}

impl Gate {
    pub fn new(level: SinkLevel, max_turn_chars: usize, redaction_on: bool) -> Self {
        Self {
            level,
            max_turn_chars,
            redaction_on,
            blocks: HashMap::new(),
            sticky_redacted: false,
            used_chars: 0,
        }
    }

    /// Process one producer event at gateway time `now_ms`.
    pub fn process(&mut self, ev: ThinkingEvent, now_ms: u64, ctx: &SanitizeCtx<'_>) -> GateOutput {
        match self.level {
            SinkLevel::Off => GateOutput::default(),
            SinkLevel::Signal | SinkLevel::Status => self.textless(ev, now_ms),
            SinkLevel::Summary => self.summary(ev, now_ms, ctx),
        }
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

    fn with_duration(&self, mut ev: ThinkingEvent, now_ms: u64) -> ThinkingEvent {
        let started = self.blocks.get(&ev.block_id).and_then(|s| s.started_at);
        if ev.started_at.is_none() {
            ev.started_at = started;
        }
        if ev.phase == ThinkingPhase::End && ev.duration_ms.is_none() {
            ev.duration_ms = ev.started_at.map(|s| now_ms.saturating_sub(s));
        }
        ev
    }

    fn textless(&mut self, ev: ThinkingEvent, now_ms: u64) -> GateOutput {
        let mut out = GateOutput::default();
        if self.admit(&ev, now_ms) {
            let ev = self.with_duration(ev, now_ms).without_text();
            out.events.push(ev);
        }
        out
    }

    fn summary(&mut self, ev: ThinkingEvent, now_ms: u64, ctx: &SanitizeCtx<'_>) -> GateOutput {
        let mut out = GateOutput::default();
        if ev.is_turn_usage() {
            out.events.push(ev.without_text());
            return out;
        }
        // Accumulate first, so a throttled delta still counts toward the
        // next snapshot.
        if ev.phase == ThinkingPhase::Update {
            if let Some(delta) = ev.text.as_deref() {
                self.blocks
                    .entry(ev.block_id.clone())
                    .or_default()
                    .raw
                    .push_str(delta);
            }
        }
        if !self.admit(&ev, now_ms) {
            return out;
        }
        let ev = self.with_duration(ev, now_ms);
        if self.sticky_redacted {
            out.events.push(redacted(ev));
            return out;
        }
        if ev.kind == ThinkingKind::Raw && !self.redaction_on {
            let mut ev = ev.without_text();
            ev.raw_needs_redaction = true;
            out.events.push(ev);
            return out;
        }
        let shown = match ev.phase {
            ThinkingPhase::Start => None,
            ThinkingPhase::Update => {
                let raw = self
                    .blocks
                    .get(&ev.block_id)
                    .map(|s| s.raw.as_str())
                    .unwrap_or("");
                Some(split_hold_back(raw, HOLD_BACK_TAIL_CHARS).0.to_string())
            }
            ThinkingPhase::End => {
                let full = ev.text.clone().filter(|t| !t.trim().is_empty());
                let acc = self.blocks.get(&ev.block_id).map(|s| s.raw.clone());
                full.or(acc)
            }
        };
        let is_end = ev.phase == ThinkingPhase::End;
        let ev = ev.without_text();
        let Some(shown) = shown.filter(|t| !t.trim().is_empty()) else {
            out.events.push(ev);
            return out;
        };
        let block_cap = if ev.kind == ThinkingKind::Raw {
            RAW_BLOCK_CAP_CHARS
        } else {
            BLOCK_CAP_CHARS
        };
        let remaining = self.max_turn_chars.saturating_sub(self.used_chars);
        let cap = block_cap.min(remaining);
        if cap == 0 {
            out.events.push(ThinkingEvent {
                truncated: true,
                ..ev
            });
            return out;
        }
        match sanitize_for_display(&shown, ctx, cap) {
            Sanitized::Blocked(block) => {
                self.sticky_redacted = true;
                out.newly_blocked = Some(block);
                out.events.push(redacted(ev));
            }
            Sanitized::Text { text, truncated } => {
                if is_end {
                    self.used_chars += text.chars().count();
                }
                let line = last_line(&text);
                out.events.push(ThinkingEvent {
                    text: (!text.is_empty()).then_some(text),
                    line,
                    truncated,
                    ..ev
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
