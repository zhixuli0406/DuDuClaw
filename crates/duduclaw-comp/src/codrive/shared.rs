//! `CodriveShared` — cross-thread state shared between the calloop main
//! thread and the injection-socket thread (`listener.rs`).
//!
//! Split out of `mod.rs` in the WP-A1 multi-window round: `mod.rs` had
//! reached exactly the 800-line file-size cap (a debt flagged back in the
//! CD-3 round's own notes — see the removed "file-size note" comment this
//! module's doc replaces), so this round's task brief required paying that
//! down *before* adding anything else to it. Everything here moved
//! verbatim from `mod.rs` (struct + its constructor family + the plain
//! read/record/auth methods); `rotate_token` stays in its own `rotation`
//! submodule's separate `impl CodriveShared` block — Rust allows an
//! inherent type's methods to be split across multiple `impl` blocks in
//! different files of the same crate, which is exactly what made this
//! split possible without touching that submodule's own file-size
//! reasoning.
//!
//! Field visibility: `frozen`/`terminated`/`shadow_active`/`takeover_active`
//! were already fully `pub` before this split (read directly from several
//! sibling submodules — `shadow.rs`, `takeover.rs`, `listener.rs` — and
//! from `mod.rs`/`state.rs`) and stay that way unchanged. `active_conn`/
//! `auth_token`/`token_path` were plain-private (visible only within the
//! old single-file `codrive` module and its descendants); moving the
//! struct one level down to `codrive::shared` would have made a
//! module-private field invisible to `codrive::mod`, `codrive::listener`,
//! and `codrive::rotation` (siblings/parent, not descendants, of
//! `codrive::shared`), so those three became `pub(super)` — `pub(in
//! codrive)` — which restores the *exact* same effective reachability
//! those call sites had before the split, no wider. `audit` needed no
//! visibility change at all: grepping the whole `codrive/` tree confirmed
//! it is touched only inside this file's own methods (`record`'s callers
//! everywhere else go through the `record()` method, never the field
//! directly), so it stays fully private.

use std::{
    io::Write,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Mutex,
    },
    time::Duration,
};

use smithay::reexports::calloop;

use super::audit::AuditLog;
use super::mode::HandoverReason;
use super::window_geometry::{CodriveQuery, WindowGeometryReply, WindowGeometryRequest};

/// Bounds how long the socket thread will block waiting for the calloop
/// main thread to answer a `window_geometry` query. Same reasoning (and
/// same value) as `shell_control::listener::MAIN_THREAD_REPLY_TIMEOUT`: a
/// stalled/overloaded main loop must degrade this ONE query to an honest
/// timeout error, never wedge the single injection-socket thread — which
/// would starve every later command on the same connection.
const QUERY_REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub struct CodriveShared {
    /// True while the agent seat is frozen (human input observed). Set on
    /// any human-seat event; cleared ONLY by human-side resume
    /// (`DuduclawComp::human_resume`, Super+Enter) — never by connection
    /// lifecycle and never by a socket `resume` op (CD-1: DESIGN-codrive-
    /// desktop §6 red line 3 + §3.1 "交還是明確動作"). Gate checked
    /// authoritatively in `DuduclawComp::handle_agent_inject`.
    pub frozen: AtomicBool,
    /// True after a Super+Esc emergency stop, until a *new* connection is
    /// accepted (see `listener.rs::handle_conn`).
    pub terminated: AtomicBool,
    /// A2 (`mode.rs`): true while a connection that got PAST the auth gate
    /// exists. Written in lockstep with [`Self::active_conn`] at all three
    /// sites that touch it — `listener.rs`'s post-auth publish and its
    /// connection-teardown cleanup, and `mod.rs`'s `emergency_stop`. It is an
    /// `AtomicBool` and not just "is `active_conn` `Some`" because both
    /// backends read it once per composited frame (to derive the driving
    /// mode), and taking a `Mutex` on the render hot path to answer a boolean
    /// would be the wrong trade — same mirror discipline `shadow_active` /
    /// `takeover_active` already follow.
    pub session_active: AtomicBool,
    /// WP-CD2-freeze-scope: mirror of `DuduclawComp::codrive_shadow_active`,
    /// kept ONLY for `listener.rs`'s optimistic pre-check (no `self.space`
    /// access there). Never authoritative — see `shadow::
    /// is_freeze_bypass_eligible`. Written in lockstep by `shadow.rs`.
    pub shadow_active: AtomicBool,
    /// CD-3 mirror of `codrive_takeover_active` — see `takeover.rs`.
    pub takeover_active: AtomicBool,
    /// A2 mirror of `DuduclawComp::codrive_watch_active` (`watch.rs`), kept
    /// so the socket thread can answer `status`'s `watch_active` field
    /// without a main-thread round trip — `status` is the one op that must
    /// answer even mid-takeover (see `listener.rs`), so it must never be
    /// allowed to depend on the main loop being responsive.
    pub watch_active: AtomicBool,
    /// A2 mirror of `DuduclawComp::codrive_watch_paused` — same reasoning as
    /// [`Self::watch_active`].
    pub watch_paused: AtomicBool,
    /// A2 mirror of the CURRENT handover's reason, encoded by
    /// [`HandoverReason::to_wire_u8`] with `0` meaning "none recorded".
    /// Written only by `DuduclawComp::codrive_sync_mode` on the main thread,
    /// read by both sockets' status answers. An `AtomicU8` rather than a
    /// `Mutex<Option<_>>` for the same render-hot-path reason as
    /// [`Self::session_active`], and because a closed 4-value enum encodes
    /// into one byte with no allocation.
    ///
    /// Readers must not report it outside [`super::mode::DrivingMode::
    /// Handover`] — `mode::status_snapshot` enforces that in one place so a
    /// stale value cannot leak into a `codrive`/`human` answer.
    handover_reason: AtomicU8,
    /// D3-c backstop mirror: true while an input method holds a keyboard grab
    /// on the AGENT seat, which makes every injected keystroke disappear into
    /// a composition nobody reads (`crate::ime::seat_filter`'s module doc has
    /// the chain). Kept ONLY for `listener.rs`'s optimistic pre-check, exactly
    /// like `shadow_active`/`takeover_active` above; the authoritative test is
    /// `DuduclawComp::codrive_ime_grab_active` on the main thread. Refreshed
    /// once per housekeeping tick as well as per command, so it can never
    /// latch `true` after the input method exits.
    pub ime_paused: AtomicBool,
    /// The currently-connected agent's stream, kept so `emergency_stop`
    /// can force-close it and so state-transition events (`frozen`/
    /// `resumed`) can be pushed to it — both from the main thread.
    pub(super) active_conn: Mutex<Option<UnixStream>>,
    /// WP-CD4b-fix (B3): the socket thread's end of the READ-ONLY
    /// `window_geometry` query bridge to the calloop main thread. `None`
    /// until `codrive::init` installs it (and permanently `None` in the
    /// `disabled*`/`for_test*` constructors), in which case
    /// [`Self::query_window_geometry`] answers `compositor_unavailable`
    /// rather than pretending — a query that cannot be answered must never
    /// yield a made-up coordinate (see `window_geometry.rs`'s module doc).
    ///
    /// A `Mutex<Option<…>>` rather than a constructor parameter for one
    /// concrete reason: the sender only exists after `init` has built the
    /// calloop source, which is after `CodriveShared` is already wrapped in
    /// its `Arc` — and threading a 4th parameter through `listener::spawn`
    /// would have churned 14 existing test call sites for no behavioral
    /// gain. The lock is only ever held long enough to *clone* the sender
    /// out (never across the blocking `recv_timeout`).
    pub(super) query_tx: Mutex<Option<calloop::channel::Sender<super::window_geometry::CodriveQuery>>>,
    audit: Option<AuditLog>,
    /// This run's hex-encoded 32-byte socket-auth token (CD-1, DESIGN
    /// §3.3.1 "EIS 界線"). `None` means token generation/write failed at
    /// startup — see `init` — in which case `check_token` always returns
    /// `false` (fail-closed: no listener is even spawned in that case, but
    /// the field stays `None` rather than some sentinel value so there's
    /// no "empty token" to accidentally match against).
    ///
    /// A `Mutex` (CD-2, was a plain `Option<String>` at CD-1) so
    /// `rotate_token` can swap it while the process keeps running — see
    /// that method's doc. Read once per connection, inside
    /// `authenticate()`'s `check_token` call, never anywhere else, so this
    /// lock is never held across a blocking or long-running operation.
    pub(super) auth_token: Mutex<Option<String>>,
    /// Where `auth_token`'s value lives on disk (CD-1:
    /// `$XDG_RUNTIME_DIR/duduclaw-codrive.token`). `None` in lockstep with
    /// `auth_token` being permanently unusable — mirrors it rather than
    /// being derived from it so `rotate_token` can fail fast with a clear
    /// reason instead of re-deriving "do we even have a token" from the
    /// mutex's contents.
    pub(super) token_path: Option<PathBuf>,
}

impl CodriveShared {
    /// The happy-path constructor (CD-1 `init()`'s socket-auth-token +
    /// audit-log success case). `pub(super)` — only `codrive::init` (the
    /// parent module) calls this; everything else goes through
    /// `disabled()`/`disabled_keep_audit()`/the `#[cfg(test)]` builders
    /// below.
    pub(super) fn new(audit: Option<AuditLog>, auth_token: String, token_path: PathBuf) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            watch_active: AtomicBool::new(false),
            watch_paused: AtomicBool::new(false),
            handover_reason: AtomicU8::new(0),
            ime_paused: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            query_tx: Mutex::new(None),
            audit,
            auth_token: Mutex::new(Some(auth_token)),
            token_path: Some(token_path),
        }
    }

    pub(super) fn disabled() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            watch_active: AtomicBool::new(false),
            watch_paused: AtomicBool::new(false),
            handover_reason: AtomicU8::new(0),
            ime_paused: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            query_tx: Mutex::new(None),
            audit: None,
            auth_token: Mutex::new(None),
            token_path: None,
        }
    }

    /// Same "no listener will ever run" shape as `disabled()`, but keeps an
    /// audit log that was already successfully opened before the failure
    /// forcing this fallback (token generation/write failure) — so
    /// freeze/emergency-stop events from human input alone (which need no
    /// socket at all) still get audited.
    pub(super) fn disabled_keep_audit(audit: Option<AuditLog>) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            watch_active: AtomicBool::new(false),
            watch_paused: AtomicBool::new(false),
            handover_reason: AtomicU8::new(0),
            ime_paused: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            query_tx: Mutex::new(None),
            audit,
            auth_token: Mutex::new(None),
            token_path: None,
        }
    }

    /// No-ops (besides a log line, on first use) when the audit log failed
    /// to open — a broken audit trail must never become a reason to block
    /// or crash the injection path (fail-open on *logging*, fail-closed on
    /// *authorization*, per repo security convention #4 — those are two
    /// different gates).
    pub fn record(&self, kind: &'static str, op: Option<&'static str>, x: Option<f64>, y: Option<f64>, detail: Option<String>) {
        if let Some(audit) = &self.audit {
            audit.record(kind, op, x, y, detail, self.frozen.load(Ordering::SeqCst));
        }
    }

    /// Convenience read of the freeze flag for callers outside this module.
    ///
    /// A2 note: this is NO LONGER what the render path uses to colour the
    /// agent cursor — `frozen` alone cannot tell "frozen because a human
    /// touched it, session still live" from "frozen and the session is gone",
    /// and those are different pixels now. Both backends call
    /// `DuduclawComp::codrive_driving_mode` (`mode.rs`) instead. This
    /// accessor stays for the udev backend's housekeeping tick, which uses it
    /// to notice a watch-idle freeze flip.
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
    }

    /// A2: publishes the current handover reason to the socket thread. Called
    /// only from `DuduclawComp::codrive_sync_mode`; `None` clears it.
    pub(super) fn store_handover_reason(&self, reason: Option<HandoverReason>) {
        self.handover_reason
            .store(reason.map(HandoverReason::to_wire_u8).unwrap_or(0), Ordering::SeqCst);
    }

    /// A2: the mirrored handover reason, or `None` if nothing was recorded.
    /// Callers must gate this on the derived mode actually being `Handover` —
    /// `mode::status_snapshot` is the one place that does, deliberately.
    pub(super) fn load_handover_reason(&self) -> Option<HandoverReason> {
        HandoverReason::from_wire_u8(self.handover_reason.load(Ordering::SeqCst))
    }

    /// Best-effort constant-time-*ish* comparison against this run's
    /// socket-auth token (CD-1, DESIGN §3.3.1 "EIS 界線"). Folds every byte
    /// position of both operands with XOR rather than returning as soon as
    /// a mismatch is found, so a naive `==`'s "how many leading bytes
    /// matched" timing signal isn't there to lean on. Not a
    /// cryptographic-grade constant-time primitive (no SIMD/compiler-
    /// barrier guarantees, and `.get(i)` bounds checks branch on length) —
    /// sized to this channel's actual threat model (a same-host Unix
    /// socket, not a network timing-attack surface), not claimed to be
    /// more than that. `None` (token generation/write failed at startup)
    /// always fails: with no token durably shared with a legitimate
    /// caller, nothing should ever authenticate.
    pub(crate) fn check_token(&self, presented: &str) -> bool {
        let guard = match self.auth_token.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(expected) = guard.as_deref() else {
            return false;
        };
        let a = expected.as_bytes();
        let b = presented.as_bytes();
        let max_len = a.len().max(b.len());
        let mut diff: u8 = (a.len() != b.len()) as u8;
        for i in 0..max_len {
            diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
        }
        diff == 0
    }

    /// WP-CD4b-fix (B3): installs the main-thread query bridge. Called once
    /// from `codrive::init`, before `listener::spawn`, so no connection can
    /// ever observe a half-built bridge.
    pub(super) fn set_query_channel(&self, tx: calloop::channel::Sender<CodriveQuery>) {
        if let Ok(mut guard) = self.query_tx.lock() {
            *guard = Some(tx);
        }
    }

    /// WP-CD4b-fix (B3): run one READ-ONLY `window_geometry` query on the
    /// calloop main thread and return its answer. Called from the socket
    /// thread (`listener.rs`), never from the main thread.
    ///
    /// Every failure mode resolves to an explicit `ok:false` reply — no
    /// bridge installed, the event loop gone, or the main thread not
    /// answering within [`QUERY_REPLY_TIMEOUT`]. Nothing here can produce a
    /// *plausible-looking* coordinate out of a failure, which is the whole
    /// safety property this op exists to preserve.
    pub(super) fn query_window_geometry(&self, req: WindowGeometryRequest) -> WindowGeometryReply {
        // Clone the sender out and drop the lock immediately — the blocking
        // `recv_timeout` below must never run while holding it.
        let tx = match self.query_tx.lock() {
            Ok(guard) => (*guard).clone(),
            Err(poisoned) => (*poisoned.into_inner()).clone(),
        };
        let Some(tx) = tx else {
            return WindowGeometryReply::err("compositor_unavailable");
        };

        let (reply_tx, reply_rx) = mpsc::channel::<WindowGeometryReply>();
        if tx.send(CodriveQuery { req, reply_tx }).is_err() {
            tracing::error!("codrive: window_geometry query channel closed — compositor event loop gone");
            return WindowGeometryReply::err("compositor_unavailable");
        }
        match reply_rx.recv_timeout(QUERY_REPLY_TIMEOUT) {
            Ok(reply) => reply,
            Err(_) => {
                tracing::warn!("codrive: window_geometry — main thread did not answer within the timeout");
                WindowGeometryReply::err("timeout")
            }
        }
    }

    // `rotate_token` (CD-2 task item 1) lives in a SECOND `impl
    // CodriveShared` block in the `rotation` submodule, not here — see that
    // module's doc comment.

    /// Best-effort push of a one-line JSON event to the currently connected
    /// agent client (task brief req 3; mirrors `emergency_stop`'s existing
    /// push of `{"event":"emergency_stop"}`). Silently does nothing if
    /// there's no active connection or the write fails — this is a
    /// courtesy notification, not a reliable channel; the client can
    /// always poll via `{"op":"status"}`. Clones the stream (rather than
    /// writing through the `&UnixStream` borrow directly) to reuse the
    /// exact write pattern `emergency_stop` already had proven to work,
    /// without introducing a second borrow-through-Mutex shape. `pub(super)`
    /// — called from `on_human_input`/`human_resume` in the parent
    /// `codrive` module (`emergency_stop` there writes to `active_conn`
    /// directly instead, unchanged by this split).
    pub(super) fn push_event(&self, event_line: &str) {
        if let Ok(guard) = self.active_conn.lock() {
            if let Some(stream) = guard.as_ref() {
                if let Ok(clone) = stream.try_clone() {
                    let _ = writeln!(&clone, "{event_line}");
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(auth_token: Option<String>) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            watch_active: AtomicBool::new(false),
            watch_paused: AtomicBool::new(false),
            handover_reason: AtomicU8::new(0),
            ime_paused: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            query_tx: Mutex::new(None),
            audit: None,
            auth_token: Mutex::new(auth_token),
            token_path: None,
        }
    }

    /// Same as [`Self::for_test`], but with a real `token_path` set — for
    /// `rotate_token` tests, which need somewhere on disk to actually write
    /// the rotated token (mirrors `init`'s real construction; plain
    /// `for_test` deliberately has no path, matching the "listener never
    /// started" `rotate_token` failure mode).
    #[cfg(test)]
    pub(crate) fn for_test_with_token_path(auth_token: Option<String>, token_path: PathBuf) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            watch_active: AtomicBool::new(false),
            watch_paused: AtomicBool::new(false),
            handover_reason: AtomicU8::new(0),
            ime_paused: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            query_tx: Mutex::new(None),
            audit: None,
            auth_token: Mutex::new(auth_token),
            token_path: Some(token_path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_token_accepts_matching_token() {
        let shared = CodriveShared::for_test(Some("abc123".to_string()));
        assert!(shared.check_token("abc123"));
    }

    #[test]
    fn check_token_rejects_wrong_or_partial_token() {
        let shared = CodriveShared::for_test(Some("abc123".to_string()));
        assert!(!shared.check_token("abc124"));
        assert!(!shared.check_token("abc12"));
        assert!(!shared.check_token("abc1234"));
        assert!(!shared.check_token(""));
    }

    #[test]
    fn check_token_rejects_everything_when_none() {
        let shared = CodriveShared::for_test(None);
        assert!(!shared.check_token(""));
        assert!(!shared.check_token("anything"));
    }
}
