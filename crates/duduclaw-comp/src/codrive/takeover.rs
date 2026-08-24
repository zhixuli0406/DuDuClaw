//! CD-3 codrive — `take_over` (DESIGN-codrive-desktop-2026-08.md §3.1 "agent
//! 出 take_over 動作 → 同上（主動交棒）" / §3.4 "登入/密碼/付款輸入一律
//! take_over 交人…接手期間 agent 感知同步凍結"). Task brief item 1's comp-side
//! half: `{"op":"take_over","reason":"…"}` is a one-level agent action (the
//! same "換人" concept Open-AutoGLM models as a first-class action in its
//! action space, per the design doc's research citation) that hands the
//! shared desktop to the human WITHOUT the agent needing to have done
//! anything wrong first — a login step, a payment field, a CAPTCHA.
//!
//! ## Relationship to an ordinary human-triggered freeze
//! `take_over` reuses the EXACT SAME `frozen` flag and human-side "交還"
//! exit (`human_resume`, Super+Enter) CD-0/CD-1 already built — no new
//! state machine, just a stronger variant of the one that exists:
//! - **Freezes the seat** exactly like `on_human_input` does (`self.codrive.
//!   frozen`), so every existing frozen-gate check (listener.rs's socket-
//!   thread pre-check, `handle_agent_inject`'s authoritative re-check)
//!   already denies injection/query ops during a takeover with ZERO new
//!   code — see `mod.rs`'s `InjectCmd::Status` remaining the one op
//!   answered regardless (task brief item 2: "status 除外").
//! - **ALSO disables the shadow-bypass exception entirely** — `mod.rs`'s
//!   `handle_agent_inject` computes `shadow_bypass = frozen &&
//!   !self.codrive_takeover_active && …`, and `listener.rs`'s optimistic
//!   pre-check mirrors the same gate via `CodriveShared::takeover_active`.
//!   This is the one place a takeover is STRICTER than an ordinary freeze:
//!   §3.1 point 2 says shadow work is normally unaffected by a freeze
//!   ("並行零干擾"), but a credential window is a total sensory blackout —
//!   there is no legitimate reason for the agent's perception to stay live
//!   via a shadow-output side door while the human is typing a password on
//!   the SAME seat's takeover. `shadow.rs`'s `freeze_bypass_decision` match
//!   already has an explicit `InjectCmd::TakeOver { .. } => false` arm too
//!   (the takeover op itself is never bypass-eligible, same reasoning as
//!   the `Shadow` toggle).
//! - **Ends via `human_resume`** (Super+Enter) — the same "交還是明確動作"
//!   exit every frozen state already has. No new resume mechanism; this
//!   file only adds an extra `takeover_ended` audit line + clearing
//!   `codrive_takeover_active` alongside whatever `human_resume`/
//!   `emergency_stop` already do.
//!
//! ## Item 2: honest frame-feed audit (task brief "感知/事件流停止（稽核可
//! 證）")
//! DESIGN §3.4's strongest claim is that credential windows get a
//! compositor-ENFORCED sensory blackout, not just an app-level promise —
//! "compositor 停止對 agent 供應畫面流與事件流". This crate was checked
//! (`Cargo.toml`'s enabled smithay features, and a repo-wide grep for
//! `screencopy`/`screenshot`/`dmabuf`/`wlr_export`) before writing this
//! doc: **there is currently no screen-copy/screenshot protocol implemented
//! at all** (`backend_winit`/`wayland_frontend`/`desktop` features only —
//! no `wlr-screencopy-unstable-v1` or any export mechanism). So at C-L1
//! there is no separate "frame feed" to additionally cut off beyond what
//! the freeze already blocks — every query/injection op is already denied
//! (task brief item 2's first half is satisfied for free by reusing
//! `frozen`). What this file does is record that honestly rather than
//! silently: every `takeover_started` audit line's `detail` includes
//! `frame_feed:none` — NOT a claim that frame supply was actively stopped
//! (there was nothing to stop), but an explicit, greppable statement that
//! this stage has zero frame-supply mechanism at all, reserving the audit
//! vocabulary (`frame_feed:<state>`) for CD-4's perception upgrade (C-L2/
//! C-L3, DESIGN §5) to fill in honestly once one exists.

use std::sync::atomic::Ordering;

use crate::state::DuduclawComp;

/// Task brief item 2: this stage has no screen-copy/screenshot mechanism at
/// all (see module doc) — recorded verbatim in every `takeover_started`
/// audit line so a reader never has to infer "was the frame feed actually
/// cut" from silence.
const FRAME_FEED_NOTE: &str = "frame_feed:none (no screencopy/screenshot mechanism implemented at this stage — see DESIGN §3.4, reserved for CD-4)";

impl DuduclawComp {
    /// `{"op":"take_over","reason":"…"}` (this file's module doc). Runs on
    /// the calloop main thread (reached via `codrive::handle_agent_inject`'s
    /// `InjectCmd::TakeOver` arm — same routing as `Shadow`). Idempotent
    /// re-assertion (a script sending `take_over` twice in a row without an
    /// intervening resume) is a no-op past the first call, matching
    /// `on_human_input`'s own `!was_frozen` idempotency shape.
    pub fn codrive_take_over(&mut self, reason: String) {
        // Defensive, not load-bearing: `mod.rs`'s `handle_agent_inject`
        // only reaches this match arm when `frozen` was already `false`
        // (`TakeOver` is never shadow-bypass-eligible — see `shadow.rs` —
        // so a `TakeOver` command arriving while already frozen is dropped
        // BEFORE this function is ever called, whether the freeze predates
        // it or is a repeat take_over). `swap` + the `was_frozen` guard
        // below is still the right shape to write — same "never trust an
        // upstream check alone" convention `handle_agent_inject` itself
        // documents — it just currently never takes the `true` branch.
        // A2: recorded before the freeze so `codrive_sync_mode` below can
        // attach the right trigger the moment the mode becomes `handover`.
        self.codrive_note_handover_reason(super::mode::HandoverReason::AgentTakeOver);
        let was_frozen = self.codrive.frozen.swap(true, Ordering::SeqCst);
        self.codrive_takeover_active = true;
        self.codrive.takeover_active.store(true, Ordering::SeqCst);
        if was_frozen {
            self.codrive_sync_mode();
            return;
        }
        self.codrive_freeze_set_at = Some(std::time::Instant::now());
        tracing::info!(reason = %reason, "codrive: agent take_over — freezing the seat and handing the desktop to the human");
        self.codrive.record(
            "takeover_started",
            Some("take_over"),
            None,
            None,
            Some(format!("reason={reason}; {FRAME_FEED_NOTE}")),
        );
        self.codrive.push_event(r#"{"event":"takeover_started"}"#);
        // A2: an agent-initiated hand-off IS a `codrive -> handover`
        // transition, with `reason=agent_take_over`.
        self.codrive_sync_mode();
    }

    /// Shared "a takeover ends" transition — called from both `human_resume`
    /// (Super+Enter, the normal path) and `emergency_stop` (Super+Esc, the
    /// global-kill path), unconditionally, mirroring `codrive_handback_
    /// shadow_if_active`'s own "always check, it's separate state" shape.
    /// No-op (no audit line) if no takeover was active — matches `human_
    /// resume`'s existing `if was_frozen` no-op precedent for "nothing to
    /// undo" cases.
    pub fn codrive_end_takeover_if_active(&mut self, trigger: &'static str) {
        if !self.codrive_takeover_active {
            return;
        }
        self.codrive_takeover_active = false;
        self.codrive.takeover_active.store(false, Ordering::SeqCst);
        tracing::info!(trigger, "codrive: takeover ended");
        self.codrive.record("takeover_ended", None, None, None, Some(trigger.to_string()));
        self.codrive.push_event(r#"{"event":"takeover_ended"}"#);
        // A2: observes whatever the caller (`human_resume`/`emergency_stop`)
        // already did to `frozen`/`terminated`. A no-op when nothing moved.
        self.codrive_sync_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codrive::InjectCmd;

    /// Pins the module doc's `FRAME_FEED_NOTE` claim — a future round that
    /// adds a real screen-copy protocol MUST touch this constant (and this
    /// test), not silently leave a stale "none" claim in the audit trail.
    #[test]
    fn frame_feed_note_is_the_honest_none_claim() {
        assert!(FRAME_FEED_NOTE.starts_with("frame_feed:none"));
    }

    /// `take_over` reaches the main thread via the same `InjectCmd` channel
    /// every other seat-touching op uses — asserted here as a compile-time/
    /// exhaustiveness pin (the real routing is exercised live, per this
    /// crate's usual "pure logic unit-tested, seat/space state live-run
    /// tested" split — see BUILD.md's many "Honest stub" notes).
    #[test]
    fn take_over_cmd_describes_as_take_over() {
        let cmd = InjectCmd::TakeOver { reason: "login".to_string() };
        assert_eq!(cmd.describe().0, "take_over");
    }
}
