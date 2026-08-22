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
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use super::audit::AuditLog;

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
    /// WP-CD2-freeze-scope: mirror of `DuduclawComp::codrive_shadow_active`,
    /// kept ONLY for `listener.rs`'s optimistic pre-check (no `self.space`
    /// access there). Never authoritative — see `shadow::
    /// is_freeze_bypass_eligible`. Written in lockstep by `shadow.rs`.
    pub shadow_active: AtomicBool,
    /// CD-3 mirror of `codrive_takeover_active` — see `takeover.rs`.
    pub takeover_active: AtomicBool,
    /// The currently-connected agent's stream, kept so `emergency_stop`
    /// can force-close it and so state-transition events (`frozen`/
    /// `resumed`) can be pushed to it — both from the main thread.
    pub(super) active_conn: Mutex<Option<UnixStream>>,
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
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            audit,
            auth_token: Mutex::new(Some(auth_token)),
            token_path: Some(token_path),
        }
    }

    pub(super) fn disabled() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            active_conn: Mutex::new(None),
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
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            active_conn: Mutex::new(None),
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

    /// Convenience read of the freeze flag for callers outside this module
    /// (e.g. `winit_backend.rs`'s redraw path, which needs it to pick the
    /// agent cursor's frozen-vs-live color — see `cursor.rs`).
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
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
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            active_conn: Mutex::new(None),
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
            shadow_active: AtomicBool::new(false),
            takeover_active: AtomicBool::new(false),
            active_conn: Mutex::new(None),
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
