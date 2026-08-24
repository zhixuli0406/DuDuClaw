//! A2 共駕復活 (2026-08-24) — the DRIVING-MODE state machine.
//!
//! DESIGN-codrive-desktop-2026-08.md §3.1, scoped exactly as the A2 wire
//! contract states it: the mode describes **who is driving THIS shared
//! desktop**, nothing else.
//!
//! | mode       | meaning                                                     |
//! |------------|-------------------------------------------------------------|
//! | `human`    | no co-drive session (or it was emergency-stopped). The agent |
//! |            | has zero driving authority over the shared desktop.          |
//! | `codrive`  | an authenticated session exists and the agent seat is NOT    |
//! |            | frozen — **the agent is driving, the human is watching**.    |
//! | `handover` | a session exists but the agent seat is frozen — **the human  |
//! |            | holds the wheel**; a pause, not a termination.               |
//!
//! ## Shadow is deliberately NOT a fourth mode
//! While the agent works on the CD-2 shadow output (`shadow.rs`) the human's
//! own shared desktop is still theirs, so the mode stays `human` and the
//! status block reports `shadow: true` alongside it. Folding shadow into the
//! mode would make "who is driving the screen in front of me" unanswerable
//! from a single field, which is the one question this enum exists to answer.
//!
//! ## One truth, derived — no second state machine
//! [`derive_mode`] is a pure function of three flags this module did not
//! invent: `session_active` / `terminated` / `frozen`. Nothing anywhere
//! stores "the mode" as an independent, separately-mutated field that could
//! drift from those flags. [`CodriveModeCache`] on `DuduclawComp` is a
//! CACHE — its only job is to let [`DuduclawComp::codrive_sync_mode`] tell a
//! real transition from a no-op, so an audit line and a push event happen
//! once per transition rather than once per frame. Every *reader* that needs
//! the current mode (both backends' render paths, both sockets' status ops)
//! calls [`derive_mode`] fresh instead of trusting the cache.
//!
//! ## `session_active`
//! "A codrive connection that got past the auth gate exists." `CodriveShared::
//! active_conn` already implied it, but it is a `Mutex<Option<UnixStream>>` —
//! taking that lock once per composited frame to colour a cursor would be a
//! lock on the render hot path for a boolean. So `CodriveShared::
//! session_active` is an `AtomicBool` written in LOCKSTEP with `active_conn`
//! at all three sites that touch it (`listener.rs`'s post-auth publish and
//! its connection-teardown cleanup, `mod.rs`'s `emergency_stop`), the same
//! mirror discipline `shadow_active`/`takeover_active` already follow.
//!
//! ## Where a `handover_reason` comes from
//! The trigger site records WHY (`CodriveModeCache::pending_reason`) and
//! `codrive_sync_mode` consumes it when the derived mode actually becomes
//! `Handover`. It is never inferred after the fact from flag shapes — two
//! different triggers can leave identical flags behind, so guessing would
//! produce a confident wrong answer in an audit trail.

use std::sync::atomic::Ordering;

use crate::state::DuduclawComp;

use super::shared::CodriveShared;

/// Who is driving the shared desktop right now. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrivingMode {
    /// No co-drive session (or it was emergency-stopped).
    #[default]
    Human,
    /// Session live, agent seat not frozen — the agent is driving.
    CoDrive,
    /// Session live, agent seat frozen — the human holds the wheel.
    Handover,
}

impl DrivingMode {
    /// The exact wire token (A2 contract §1). Lower-snake-case, matching
    /// every other enum-string convention in this crate
    /// (`shell_control::protocol::ShellIntent::as_str`,
    /// `crate::cursor::source::CursorSource::as_str`).
    pub fn as_str(self) -> &'static str {
        match self {
            DrivingMode::Human => "human",
            DrivingMode::CoDrive => "codrive",
            DrivingMode::Handover => "handover",
        }
    }
}

/// Why the wheel is currently in the human's hands. A CLOSED set — a
/// free-form string here would end up carrying caller-supplied text into an
/// audit trail and a shell UI, which this crate refuses everywhere else
/// (`shell_control::listener::validate`'s no-echo discipline).
///
/// Only meaningful while [`DrivingMode::Handover`]; serialized as `null`
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverReason {
    /// The human touched a real input device (`on_human_input`).
    HumanInput,
    /// The agent handed the desktop over itself (`take_over`, `takeover.rs`).
    AgentTakeOver,
    /// Watch mode's idle timeout fired — nobody is supervising (`watch.rs`).
    WatchIdle,
    /// A human pressed 接管 in the shell (`shell_control::codrive_ops`).
    ShellTakeWheel,
}

impl HandoverReason {
    /// The exact wire token (A2 contract §2).
    pub fn as_str(self) -> &'static str {
        match self {
            HandoverReason::HumanInput => "human_input",
            HandoverReason::AgentTakeOver => "agent_take_over",
            HandoverReason::WatchIdle => "watch_idle",
            HandoverReason::ShellTakeWheel => "shell_take_wheel",
        }
    }

    /// Encoding for the `AtomicU8` mirror on `CodriveShared` (the socket
    /// thread answers `status` without touching the main thread — see
    /// `listener.rs`). `0` is reserved for `None`, so no real reason may
    /// ever encode to it.
    pub(super) fn to_wire_u8(self) -> u8 {
        match self {
            HandoverReason::HumanInput => 1,
            HandoverReason::AgentTakeOver => 2,
            HandoverReason::WatchIdle => 3,
            HandoverReason::ShellTakeWheel => 4,
        }
    }

    /// Inverse of [`Self::to_wire_u8`]. Any unknown byte decodes to `None`
    /// rather than a guessed variant — an unreadable mirror must read as
    /// "no reason recorded", never as a confidently wrong one.
    pub(super) fn from_wire_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(HandoverReason::HumanInput),
            2 => Some(HandoverReason::AgentTakeOver),
            3 => Some(HandoverReason::WatchIdle),
            4 => Some(HandoverReason::ShellTakeWheel),
            _ => None,
        }
    }
}

/// The A2 contract §2 derivation, as one pure function.
///
/// ```text
/// !session_active || terminated -> Human
/// frozen                        -> Handover
/// otherwise                     -> CoDrive
/// ```
///
/// The first line comes first on purpose: an emergency-stopped or
/// session-less compositor is `human` **regardless of `frozen`**. A stopped
/// session leaves `frozen` latched `true` by design (§6 red line 3 — a fresh
/// connection must not clear it), and reporting that as `handover` would
/// claim there is a session to hand back to when there is not.
pub fn derive_mode(session_active: bool, terminated: bool, frozen: bool) -> DrivingMode {
    if !session_active || terminated {
        DrivingMode::Human
    } else if frozen {
        DrivingMode::Handover
    } else {
        DrivingMode::CoDrive
    }
}

/// Main-thread-only transition bookkeeping. NOT the source of truth for the
/// mode — see this module's doc.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodriveModeCache {
    /// The last mode [`DuduclawComp::codrive_sync_mode`] observed, so it can
    /// tell a real transition from a no-op.
    pub current: DrivingMode,
    /// The reason attached to the CURRENT handover, `None` in every other
    /// mode.
    pub reason: Option<HandoverReason>,
    /// Recorded by whichever call site is about to cause a handover, and
    /// consumed by the next `codrive_sync_mode`. Cleared whenever the derived
    /// mode is not `Handover`, so a hint can never survive to mislabel a
    /// later, unrelated handover.
    pub pending_reason: Option<HandoverReason>,
}

/// Everything both sockets' status answers are built from, read once from
/// the `CodriveShared` atomics so a reply can never mix two different
/// instants of the same field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodriveStatusSnapshot {
    pub(crate) mode: DrivingMode,
    pub(crate) handover_reason: Option<HandoverReason>,
    pub(crate) session_active: bool,
    pub(crate) frozen: bool,
    pub(crate) terminated: bool,
    pub(crate) takeover: bool,
    pub(crate) shadow: bool,
    pub(crate) watch_active: bool,
    pub(crate) watch_paused: bool,
}

/// Reads one consistent-enough snapshot of the co-drive state.
///
/// Callable from EITHER thread: every field is an atomic on `CodriveShared`
/// (the main-thread `bool` twins on `DuduclawComp` are written in lockstep
/// with them — `shadow.rs`/`takeover.rs`/`watch.rs`). `mode` is derived here
/// rather than read from any stored field, and `handover_reason` is forced to
/// `None` whenever the derived mode is not `Handover` — so a reply is always
/// internally consistent even if it lands in the microseconds between a flag
/// flipping and `codrive_sync_mode` running.
pub(crate) fn status_snapshot(shared: &CodriveShared) -> CodriveStatusSnapshot {
    let session_active = shared.session_active.load(Ordering::SeqCst);
    let terminated = shared.terminated.load(Ordering::SeqCst);
    let frozen = shared.frozen.load(Ordering::SeqCst);
    let mode = derive_mode(session_active, terminated, frozen);
    CodriveStatusSnapshot {
        mode,
        handover_reason: match mode {
            DrivingMode::Handover => shared.load_handover_reason(),
            _ => None,
        },
        session_active,
        frozen,
        terminated,
        takeover: shared.takeover_active.load(Ordering::SeqCst),
        shadow: shared.shadow_active.load(Ordering::SeqCst),
        watch_active: shared.watch_active.load(Ordering::SeqCst),
        watch_paused: shared.watch_paused.load(Ordering::SeqCst),
    }
}

/// `handover_reason`'s JSON value — a quoted token, or the bare `null` the
/// A2 contract §2 requires outside `handover`.
fn reason_json(reason: Option<HandoverReason>) -> String {
    match reason {
        Some(r) => format!("\"{}\"", r.as_str()),
        None => "null".to_string(),
    }
}

/// The codrive injection socket's `{"op":"status"}` reply line (A2 contract
/// §3.1).
///
/// **The first three fields keep their CD-1/CD-3 spelling and position
/// byte-for-byte** — the gateway's own client parses this reply, and every
/// already-shipped caller must keep reading exactly what it read before.
/// Everything after `takeover` is additive.
pub(super) fn status_reply_line(snap: &CodriveStatusSnapshot) -> String {
    format!(
        concat!(
            r#"{{"ok":true,"frozen":{},"terminated":{},"takeover":{},"#,
            r#""mode":"{}","handover_reason":{},"#,
            r#""shadow":{},"watch_active":{},"watch_paused":{}}}"#
        ),
        snap.frozen,
        snap.terminated,
        snap.takeover,
        snap.mode.as_str(),
        reason_json(snap.handover_reason),
        snap.shadow,
        snap.watch_active,
        snap.watch_paused,
    )
}

/// The additive `driving_mode` push event (A2 contract §3.2). Pushed only on
/// a real transition — never once per frame, never once per human keystroke.
pub(super) fn driving_mode_event_line(mode: DrivingMode, reason: Option<HandoverReason>) -> String {
    format!(
        r#"{{"event":"driving_mode","mode":"{}","reason":{}}}"#,
        mode.as_str(),
        reason_json(reason)
    )
}

/// The `driving_mode` audit line's `detail` (A2 contract §3.3):
/// `from=<a>; to=<b>; reason=<r>`, with `none` for the absent reason.
fn driving_mode_audit_detail(
    from: DrivingMode,
    to: DrivingMode,
    reason: Option<HandoverReason>,
) -> String {
    format!(
        "from={}; to={}; reason={}",
        from.as_str(),
        to.as_str(),
        reason.map(HandoverReason::as_str).unwrap_or("none")
    )
}

impl DuduclawComp {
    /// The current driving mode, DERIVED fresh (never the cache). This is
    /// what every renderer and every status answer should call.
    pub fn codrive_driving_mode(&self) -> DrivingMode {
        derive_mode(
            self.codrive.session_active.load(Ordering::SeqCst),
            self.codrive.terminated.load(Ordering::SeqCst),
            self.codrive.frozen.load(Ordering::SeqCst),
        )
    }

    /// Re-derives the mode and, **only if it actually changed**, records one
    /// `driving_mode` audit line, pushes one `driving_mode` event, and queues
    /// a redraw (the mode drives both the ghost cursor's colour and the
    /// screen-edge indicator, so a transition changes real pixels).
    ///
    /// No change ⇒ a true no-op: no audit line, no push event, no redraw.
    /// That matters because this runs once per composited frame on both
    /// backends — it is the ONLY place the main thread can observe the socket
    /// thread flipping `session_active` (a connection arriving or dropping is
    /// not otherwise a main-thread event at all).
    ///
    /// Main thread only: every call site is either the calloop event-source
    /// callback for an injected command / a human input event, or a backend's
    /// render/housekeeping tick.
    pub fn codrive_sync_mode(&mut self) {
        let next = self.codrive_driving_mode();

        if next != DrivingMode::Handover {
            // A trigger hint that never turned into a handover must not
            // survive to mislabel a later one (module doc, "Where a
            // `handover_reason` comes from"). Deliberately outside the
            // early-return below: this is internal bookkeeping with no
            // observable effect, not a state transition.
            self.codrive_mode.pending_reason = None;
        }

        let from = self.codrive_mode.current;
        if next == from {
            return;
        }

        let reason = if next == DrivingMode::Handover {
            self.codrive_mode.pending_reason.take()
        } else {
            None
        };
        self.codrive_mode.current = next;
        self.codrive_mode.reason = reason;
        self.codrive.store_handover_reason(reason);

        tracing::info!(
            from = from.as_str(),
            to = next.as_str(),
            reason = reason.map(HandoverReason::as_str).unwrap_or("none"),
            "codrive: driving mode changed"
        );
        self.codrive.record(
            "driving_mode",
            None,
            None,
            None,
            Some(driving_mode_audit_detail(from, next, reason)),
        );
        self.codrive
            .push_event(&driving_mode_event_line(next, reason));
        self.queue_redraw();
    }

    /// A2 `codrive_drive`'s `take_wheel` half — freeze the agent seat with
    /// `reason=shell_take_wheel`. Reached only from
    /// `shell_control::codrive_ops` (the human-side socket); lives HERE
    /// because the co-drive state machine owns its own transitions, its own
    /// audit vocabulary, and its own push events.
    ///
    /// A deliberate near-copy of `on_human_input`'s freeze branch rather than
    /// a call to it: that function begins by treating any human event as
    /// proof of presence and LIFTING a watch-mode idle pause, which would
    /// turn this button into an un-freeze in exactly the situation (nobody
    /// was watching) where a person is most likely to press it.
    ///
    /// Reuses the existing `freeze` audit kind and `{"event":"frozen"}` push
    /// rather than inventing parallel ones — the state being entered is the
    /// same, only the trigger differs (recorded as the `op` field and as the
    /// `driving_mode` line's `reason`), and a second vocabulary would split
    /// every existing freeze query in two.
    pub(crate) fn codrive_shell_take_wheel(&mut self) {
        self.codrive_note_handover_reason(HandoverReason::ShellTakeWheel);
        let was_frozen = self.codrive.frozen.swap(true, Ordering::SeqCst);
        if !was_frozen {
            self.codrive_freeze_set_at = Some(std::time::Instant::now());
            tracing::info!("codrive: shell take_wheel — freezing agent seat");
            self.codrive
                .record("freeze", Some("shell_take_wheel"), None, None, None);
            self.codrive.push_event(r#"{"event":"frozen"}"#);
        }
        self.codrive_sync_mode();
    }

    /// Records WHY the wheel is about to change hands, for the next
    /// [`Self::codrive_sync_mode`] to consume. Called by the four trigger
    /// sites named in [`HandoverReason`]; a trigger that does not end up
    /// producing a handover simply has its hint discarded.
    pub(crate) fn codrive_note_handover_reason(&mut self, reason: HandoverReason) {
        self.codrive_mode.pending_reason = Some(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── `derive_mode`: the full 2³ truth table, exhaustively ──────────────

    #[test]
    fn derive_mode_truth_table_is_exhaustive() {
        // (session_active, terminated, frozen) -> expected
        let table = [
            ((false, false, false), DrivingMode::Human),
            ((false, false, true), DrivingMode::Human),
            ((false, true, false), DrivingMode::Human),
            ((false, true, true), DrivingMode::Human),
            ((true, false, false), DrivingMode::CoDrive),
            ((true, false, true), DrivingMode::Handover),
            ((true, true, false), DrivingMode::Human),
            ((true, true, true), DrivingMode::Human),
        ];
        assert_eq!(table.len(), 8, "all 2^3 flag combinations must be pinned");
        for ((session_active, terminated, frozen), expected) in table {
            assert_eq!(
                derive_mode(session_active, terminated, frozen),
                expected,
                "session_active={session_active} terminated={terminated} frozen={frozen}"
            );
        }
    }

    #[test]
    fn a_terminated_session_reads_human_even_though_frozen_stays_latched() {
        // §6 red line 3: an emergency stop leaves `frozen` true on purpose.
        // Reporting that as `handover` would claim there is a session to hand
        // back to.
        assert_eq!(derive_mode(true, true, true), DrivingMode::Human);
    }

    #[test]
    fn no_session_is_human_regardless_of_the_other_two_flags() {
        for terminated in [false, true] {
            for frozen in [false, true] {
                assert_eq!(derive_mode(false, terminated, frozen), DrivingMode::Human);
            }
        }
    }

    // ── Wire tokens ──────────────────────────────────────────────────────

    #[test]
    fn driving_mode_tokens_are_the_contract_spelling() {
        assert_eq!(DrivingMode::Human.as_str(), "human");
        assert_eq!(DrivingMode::CoDrive.as_str(), "codrive");
        assert_eq!(DrivingMode::Handover.as_str(), "handover");
    }

    #[test]
    fn handover_reason_tokens_are_the_contract_spelling() {
        assert_eq!(HandoverReason::HumanInput.as_str(), "human_input");
        assert_eq!(HandoverReason::AgentTakeOver.as_str(), "agent_take_over");
        assert_eq!(HandoverReason::WatchIdle.as_str(), "watch_idle");
        assert_eq!(HandoverReason::ShellTakeWheel.as_str(), "shell_take_wheel");
    }

    #[test]
    fn the_default_mode_is_human() {
        assert_eq!(DrivingMode::default(), DrivingMode::Human);
        assert_eq!(CodriveModeCache::default().current, DrivingMode::Human);
        assert_eq!(CodriveModeCache::default().reason, None);
        assert_eq!(CodriveModeCache::default().pending_reason, None);
    }

    // ── The `AtomicU8` mirror encoding ───────────────────────────────────

    #[test]
    fn handover_reason_wire_u8_round_trips_for_every_variant() {
        for r in [
            HandoverReason::HumanInput,
            HandoverReason::AgentTakeOver,
            HandoverReason::WatchIdle,
            HandoverReason::ShellTakeWheel,
        ] {
            assert_ne!(r.to_wire_u8(), 0, "0 is reserved for None");
            assert_eq!(HandoverReason::from_wire_u8(r.to_wire_u8()), Some(r));
        }
    }

    #[test]
    fn an_unknown_reason_byte_decodes_to_none_not_a_guess() {
        assert_eq!(HandoverReason::from_wire_u8(0), None);
        for v in 5u8..=255 {
            assert_eq!(HandoverReason::from_wire_u8(v), None, "byte {v}");
        }
    }

    // ── The status reply line (byte-for-byte, the shipped-caller contract) ─

    fn snap(mode: DrivingMode, reason: Option<HandoverReason>) -> CodriveStatusSnapshot {
        CodriveStatusSnapshot {
            mode,
            handover_reason: reason,
            session_active: mode != DrivingMode::Human,
            frozen: mode == DrivingMode::Handover,
            terminated: false,
            takeover: false,
            shadow: false,
            watch_active: false,
            watch_paused: false,
        }
    }

    #[test]
    fn status_reply_keeps_the_pre_a2_three_fields_first_and_verbatim() {
        let line = status_reply_line(&snap(DrivingMode::Human, None));
        assert!(
            line.starts_with(r#"{"ok":true,"frozen":false,"terminated":false,"takeover":false,"#),
            "the CD-1/CD-3 prefix must be byte-identical: {line}"
        );
    }

    #[test]
    fn status_reply_is_the_exact_contract_shape() {
        assert_eq!(
            status_reply_line(&snap(DrivingMode::Human, None)),
            r#"{"ok":true,"frozen":false,"terminated":false,"takeover":false,"mode":"human","handover_reason":null,"shadow":false,"watch_active":false,"watch_paused":false}"#
        );
    }

    #[test]
    fn status_reply_carries_a_handover_reason_as_a_quoted_token() {
        let line = status_reply_line(&snap(
            DrivingMode::Handover,
            Some(HandoverReason::ShellTakeWheel),
        ));
        assert!(line.contains(r#""mode":"handover""#), "{line}");
        assert!(
            line.contains(r#""handover_reason":"shell_take_wheel""#),
            "{line}"
        );
        assert!(line.contains(r#""frozen":true"#), "{line}");
    }

    #[test]
    fn the_status_reply_is_valid_json_in_every_mode() {
        for (mode, reason) in [
            (DrivingMode::Human, None),
            (DrivingMode::CoDrive, None),
            (DrivingMode::Handover, Some(HandoverReason::WatchIdle)),
            (DrivingMode::Handover, None),
        ] {
            let line = status_reply_line(&snap(mode, reason));
            let v: serde_json::Value = serde_json::from_str(&line).expect(&line);
            assert_eq!(v["mode"], mode.as_str());
            match reason {
                Some(r) if mode == DrivingMode::Handover => {
                    assert_eq!(v["handover_reason"], r.as_str())
                }
                _ => assert!(v["handover_reason"].is_null(), "{line}"),
            }
        }
    }

    // ── The push event ───────────────────────────────────────────────────

    #[test]
    fn driving_mode_event_matches_the_contract_example() {
        assert_eq!(
            driving_mode_event_line(DrivingMode::CoDrive, Some(HandoverReason::HumanInput)),
            r#"{"event":"driving_mode","mode":"codrive","reason":"human_input"}"#
        );
    }

    #[test]
    fn driving_mode_event_uses_a_null_reason_outside_handover() {
        assert_eq!(
            driving_mode_event_line(DrivingMode::Human, None),
            r#"{"event":"driving_mode","mode":"human","reason":null}"#
        );
    }

    #[test]
    fn driving_mode_event_is_one_valid_json_line() {
        for (mode, reason) in [
            (DrivingMode::Human, None),
            (DrivingMode::CoDrive, None),
            (DrivingMode::Handover, Some(HandoverReason::AgentTakeOver)),
        ] {
            let line = driving_mode_event_line(mode, reason);
            assert!(
                !line.contains('\n'),
                "an event must be exactly one line: {line}"
            );
            let v: serde_json::Value = serde_json::from_str(&line).expect(&line);
            assert_eq!(v["event"], "driving_mode");
        }
    }

    // ── The audit detail ─────────────────────────────────────────────────

    #[test]
    fn audit_detail_is_the_contract_shape() {
        assert_eq!(
            driving_mode_audit_detail(
                DrivingMode::CoDrive,
                DrivingMode::Handover,
                Some(HandoverReason::HumanInput)
            ),
            "from=codrive; to=handover; reason=human_input"
        );
    }

    #[test]
    fn audit_detail_says_none_rather_than_omitting_the_reason() {
        assert_eq!(
            driving_mode_audit_detail(DrivingMode::Handover, DrivingMode::CoDrive, None),
            "from=handover; to=codrive; reason=none"
        );
    }

    // ── `status_snapshot`: mode is derived, never a stored field ─────────

    #[test]
    fn snapshot_derives_the_mode_from_the_live_atomics() {
        let shared = CodriveShared::for_test(Some("tok".to_string()));

        // No session yet.
        assert_eq!(status_snapshot(&shared).mode, DrivingMode::Human);

        // A session appears.
        shared.session_active.store(true, Ordering::SeqCst);
        assert_eq!(status_snapshot(&shared).mode, DrivingMode::CoDrive);

        // The human takes the wheel.
        shared.frozen.store(true, Ordering::SeqCst);
        shared.store_handover_reason(Some(HandoverReason::HumanInput));
        let s = status_snapshot(&shared);
        assert_eq!(s.mode, DrivingMode::Handover);
        assert_eq!(s.handover_reason, Some(HandoverReason::HumanInput));

        // Emergency stop: `frozen` stays latched, the mode does not.
        shared.terminated.store(true, Ordering::SeqCst);
        let s = status_snapshot(&shared);
        assert_eq!(s.mode, DrivingMode::Human);
        assert!(s.frozen, "the raw flag is still reported honestly");
    }

    #[test]
    fn snapshot_never_reports_a_reason_outside_handover() {
        let shared = CodriveShared::for_test(Some("tok".to_string()));
        // A stale mirror value must not leak into a non-handover answer.
        shared.store_handover_reason(Some(HandoverReason::WatchIdle));
        shared.session_active.store(true, Ordering::SeqCst);
        assert_eq!(status_snapshot(&shared).mode, DrivingMode::CoDrive);
        assert_eq!(status_snapshot(&shared).handover_reason, None);
    }

    // ── Source-structure pins ────────────────────────────────────────────
    // A runtime test for these would need a live `DuduclawComp` — a real
    // seat and a GL context, which this suite does not build — so they are
    // pinned against the source text, the same way `human_seat.rs` pins its
    // own self-freeze invariants. They are not decoration: the
    // `session_active` clear in `emergency_stop` was accidentally deleted
    // once while this round's comments were being trimmed, and every other
    // test still passed.

    #[test]
    fn emergency_stop_clears_session_active_in_lockstep_with_the_connection() {
        let src = include_str!("mod.rs");
        let body = src
            .split("pub fn emergency_stop")
            .nth(1)
            .expect("emergency_stop was renamed — re-point this invariant");
        let take = body
            .find("active_conn.lock()")
            .expect("emergency_stop no longer force-closes the connection");
        let clear = body.find("session_active.store(false").expect(
            "emergency_stop no longer clears `session_active` — a killed session would keep \
             reporting `codrive`/`handover` until the socket thread happened to notice",
        );
        assert!(
            take < clear,
            "the flag must be cleared with, not before, the connection"
        );
    }

    #[test]
    fn the_listener_publishes_session_active_only_behind_the_auth_gate() {
        let src = include_str!("listener.rs");
        let body = src
            .split("fn handle_conn(stream")
            .nth(1)
            .expect("handle_conn was renamed — re-point this invariant");
        let auth = body
            .find("if !authenticate(")
            .expect("handle_conn no longer gates on authenticate()");
        let publish = body
            .find("session_active.store(true")
            .expect("handle_conn no longer publishes the session");
        let clear = body
            .find("session_active.store(false")
            .expect("handle_conn no longer clears the session on teardown");
        assert!(
            auth < publish,
            "an unauthenticated connection must never publish a live co-drive session"
        );
        assert!(
            publish < clear,
            "teardown must clear what the post-auth publish set"
        );
    }

    #[test]
    fn every_handover_trigger_records_its_reason_before_it_freezes() {
        // Recorded at the trigger, never inferred afterwards (module doc) —
        // so the record has to happen BEFORE the freeze that `codrive_sync_
        // mode` then observes.
        for (src, marker) in [
            (include_str!("mod.rs"), "pub fn on_human_input"),
            (include_str!("takeover.rs"), "pub fn codrive_take_over"),
            (include_str!("watch.rs"), "pub fn codrive_check_watch_idle"),
            (
                include_str!("mode.rs"),
                "pub(crate) fn codrive_shell_take_wheel",
            ),
        ] {
            let body = src
                .split(marker)
                .nth(1)
                .unwrap_or_else(|| panic!("{marker} was renamed — re-point this invariant"));
            let note = body
                .find("codrive_note_handover_reason")
                .unwrap_or_else(|| panic!("{marker} no longer records a handover reason"));
            let freeze = body
                .find("frozen.swap(true")
                .unwrap_or_else(|| panic!("{marker} no longer freezes the seat"));
            assert!(
                note < freeze,
                "{marker}: the reason must be recorded before the freeze"
            );
        }
    }

    #[test]
    fn snapshot_mirrors_the_shadow_and_watch_flags() {
        let shared = CodriveShared::for_test(None);
        shared.shadow_active.store(true, Ordering::SeqCst);
        shared.watch_active.store(true, Ordering::SeqCst);
        shared.watch_paused.store(true, Ordering::SeqCst);
        shared.takeover_active.store(true, Ordering::SeqCst);
        let s = status_snapshot(&shared);
        assert!(s.shadow && s.watch_active && s.watch_paused && s.takeover);
    }
}
