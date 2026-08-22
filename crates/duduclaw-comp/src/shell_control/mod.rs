//! WP-comp-shell-ipc (2026-08-22) — shell↔comp window query/control IPC.
//!
//! ## Why this exists (A3's architecture finding)
//! A3's dock integration needed comp to answer "what windows are running"
//! and "switch to this one", but `codrive`'s injection socket
//! (`duduclaw-codrive.sock`) is structurally the wrong channel for that:
//! it is the AGENT's private, token-authenticated seat-injection channel —
//! every command through it is attributed to the agent seat, drives
//! `codrive`'s own freeze/takeover/shadow state machine, and lands in the
//! `codrive` audit trail as an agent action (`codrive/mod.rs`'s module doc
//! has the full state machine). Routing a human's dock click through it
//! would misattribute a human action as an agent action in that trail —
//! exactly the kind of audit-poisoning DESIGN-codrive-desktop-2026-08.md's
//! safety redlines exist to prevent. So this module is a SECOND, entirely
//! separate socket/protocol/audit-trail: `duduclaw-shell.sock` (task
//! brief), reusing only the pure window-matching/focus LOGIC
//! `codrive::window_target`/`DuduclawComp::focus_window` already proved
//! live (WP-A1/WP-CD4a-COMP), never the socket, auth, or audit machinery.
//!
//! ## Trust boundary — same-uid `SO_PEERCRED`, not a bearer token
//! `codrive`'s token exists because an agent CLI subprocess and this
//! compositor are NOT the same trust domain in general (DESIGN §3.3.1's
//! "EIS 界線"). `duduclaw-shell` and `duduclaw-comp`, by contrast, are:
//! on the appliance both run under the SAME kiosk-session system user
//! (`duduclaw-kiosk`, `appliance/mkosi.extra/etc/systemd/system/
//! duduclaw-kiosk.service`), while agent CLI subprocesses run under a
//! DIFFERENT user (`duduclaw`, `duduclaw-gateway.service` — see
//! `appliance/postinst.d/20-users-and-units.sh`'s own useradd comments).
//! That is a real, kernel-enforced boundary a bearer-token file can't
//! improve on and a same-uid `SO_PEERCRED` check (`listener::
//! is_authorized_peer`) uses directly — mirroring the exact pattern
//! `duduclaw-sysd` already established for its own root-daemon socket
//! (`duduclaw-sysd/src/server.rs`'s `handle_connection`), simplified from
//! "an externally configured allowed uid" to "my own uid" since this
//! process and its legitimate caller are always the same user by
//! construction, unlike sysd's caller (a different, non-root user calling
//! into a root daemon). See `listener.rs`'s own doc comment for the exact
//! check and its tests for the "agent cannot reach this socket" proof.
//!
//! ## No freeze gate — this is what a human operating their own desktop
//! looks like
//! `codrive`'s freeze/terminated/takeover state machine governs when the
//! AGENT seat may act — DESIGN §3.1's "人輸入永遠優先". A dock click is
//! human input BY DEFINITION (this socket cannot be reached by anything
//! that isn't already authenticated as the same uid as this kiosk session
//! — see above), so it is never gated behind `codrive.frozen`/
//! `terminated`/`takeover_active` at all: a human can always operate their
//! own desktop, exactly like every other human-input path in this crate
//! (`input.rs`'s real seat, `codrive::human_resume`/`emergency_stop`).
//! This is intentional, not an oversight — gating a human action behind
//! the AGENT's freeze state would have the exact backwards semantics of
//! DESIGN §3.1. It is also not a red-line-3 backdoor for the AGENT to
//! bypass its own freeze: the same-uid auth above means the agent seat's
//! own operator (an agent CLI subprocess) structurally cannot open this
//! socket in the first place, so there is no path from "agent wants to
//! act while frozen" through this module at all.
//!
//! ## One-shot RPC, not a persistent session
//! Unlike `codrive`'s long-lived, stateful, single-connection-at-a-time
//! session (`codrive::listener`'s module doc), this socket is a plain
//! connect → one request line → one response line → close round trip per
//! call (`protocol.rs`'s own module doc has the full reasoning) — the
//! natural shape for a dock that polls `list_windows` on an interval and
//! fires `focus_window` on a click, with no session state to keep in sync
//! across calls.
//!
//! ## Independent audit trail
//! `audit.rs` writes its own JSONL file (`duduclaw-shell-control-audit.
//! jsonl`), never `codrive`'s — a reader who needs to tell "a human
//! switched windows" apart from "the agent did" can do so by WHICH FILE a
//! line is in, not a field inside a shared, disputable file. Only
//! `focus_window` (an action with a real effect) is audited, mirroring the
//! established "queries aren't audited, actions are" precedent
//! `codrive::listener`'s own `status`/`resume` handling already set —
//! `list_windows` is read-only and, realistically, polled every few
//! seconds by a live dock, so auditing it would mostly be noise, not
//! evidence.

mod audit;
mod listener;
mod protocol;

pub(crate) use protocol::{ShellControlRequest, ShellControlResponse, ShellWindowInfo};

use std::{path::PathBuf, sync::Arc};

use smithay::{
    reexports::calloop::{self, EventLoop},
    utils::SERIAL_COUNTER,
};

use crate::{
    codrive::{self, window_target::WindowMatch},
    state::DuduclawComp,
    CalloopData,
};
use audit::ShellControlAuditLog;

/// Cross-thread state shared between the calloop main thread and the
/// shell-control socket thread — today just the audit log (unlike
/// `codrive::CodriveShared`, there is no frozen/terminated/auth-token
/// state here at all, per this module's own doc comment on why: no freeze
/// gate, and auth is a stateless per-connection `SO_PEERCRED` check that
/// needs no shared mutable state to enforce). `pub` (not `pub(crate)`) —
/// mirrors `codrive::CodriveShared`'s own visibility, needed because
/// `DuduclawComp::shell_control` (`state.rs`) exposes this type through a
/// `pub` struct field, same as `DuduclawComp::codrive` already does for
/// `CodriveShared`.
pub struct ShellControlShared {
    audit: Option<ShellControlAuditLog>,
}

impl ShellControlShared {
    fn new(audit: Option<ShellControlAuditLog>) -> Self {
        Self { audit }
    }

    fn disabled() -> Self {
        Self { audit: None }
    }

    /// No-ops (fail-open on *logging*, same repo convention #4 split
    /// `codrive::CodriveShared::record` already documents) when the audit
    /// log failed to open at startup.
    pub(crate) fn record(&self, kind: &'static str, detail: Option<String>) {
        if let Some(audit) = &self.audit {
            audit.record(kind, detail);
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self { audit: None }
    }
}

/// One request in flight from the socket thread to the calloop main
/// thread, carrying its own oneshot reply channel — see `listener.rs`'s
/// module doc for why this round trip exists (unlike `codrive`'s
/// fire-and-forget `InjectCmd` channel, every shell-control caller needs
/// the REAL computed answer, not an immediately-knowable ack).
pub(crate) struct ShellControlMsg {
    pub(crate) req: ShellControlRequest,
    pub(crate) reply_tx: std::sync::mpsc::Sender<ShellControlResponse>,
}

/// Starts the shell-control socket listener + audit log and wires its
/// request channel into the event loop. Called from `DuduclawComp::new`,
/// mirroring `codrive::init`'s own call shape — but this needs no
/// `SeatState`/`DisplayHandle` at all (no new `wl_seat` is created; every
/// op here acts on the EXISTING human seat, `DuduclawComp::seat`).
pub fn init(event_loop: &mut EventLoop<CalloopData>) -> Arc<ShellControlShared> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        tracing::error!(
            "shell_control: XDG_RUNTIME_DIR is not set — the shell-control socket is disabled \
             for this run"
        );
        return Arc::new(ShellControlShared::disabled());
    };

    let sock_path = PathBuf::from(&runtime_dir).join(protocol::SOCKET_FILE_NAME);
    let audit_path = PathBuf::from(&runtime_dir).join(protocol::AUDIT_FILE_NAME);

    let audit = match ShellControlAuditLog::open(&audit_path) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::error!(error = %e, path = %audit_path.display(), "shell_control: failed to open audit log — continuing without one");
            None
        }
    };

    let shared = Arc::new(ShellControlShared::new(audit));

    // This process's own uid — the entire auth policy (see this module's
    // doc comment for why "same uid as me" is sufficient and correct
    // here, unlike `codrive`'s bearer token).
    // SAFETY: `getuid()` has no preconditions and cannot fail.
    let own_uid = unsafe { libc::getuid() };

    let (tx, rx) = calloop::channel::channel::<ShellControlMsg>();

    if let Err(e) = listener::spawn(sock_path, own_uid, Arc::clone(&shared), tx) {
        tracing::error!(error = %e, "shell_control: failed to start the shell-control socket listener — dock/window queries will fail this run");
    }

    event_loop
        .handle()
        .insert_source(rx, |event, _, data: &mut CalloopData| {
            if let calloop::channel::Event::Msg(msg) = event {
                let resp = data.state.handle_shell_control_request(msg.req);
                // Best-effort: if the socket thread already gave up
                // waiting (`listener::MAIN_THREAD_REPLY_TIMEOUT` elapsed)
                // the receiver is gone and this send simply fails — no
                // panic, no retry, matching `codrive::CodriveShared::
                // push_event`'s own "courtesy, not guaranteed delivery"
                // posture for a dropped peer.
                let _ = msg.reply_tx.send(resp);
            }
        })
        .expect("shell_control: failed to insert the request channel into the event loop");

    shared
}

impl DuduclawComp {
    /// Executes one already-validated (by `listener::validate`) shell-
    /// control request on the calloop main thread — the only thread
    /// allowed to touch `self.space`/`self.seat`, same reasoning as
    /// `codrive::handle_agent_inject`. No freeze/terminated check here at
    /// all — see this module's own doc comment for why that is correct,
    /// not an oversight.
    pub(crate) fn handle_shell_control_request(&mut self, req: ShellControlRequest) -> ShellControlResponse {
        match req {
            ShellControlRequest::ListWindows => ShellControlResponse::windows(self.shell_control_list_windows()),
            ShellControlRequest::FocusWindow { query } => self.shell_control_focus_window(query),
        }
    }

    /// Read-only — never audited, see this module's own doc comment.
    /// `focused` reports the HUMAN seat's (`self.seat`) current keyboard
    /// focus, never the agent seat's: this is a human-facing surface (a
    /// dock), so "which window is focused" must answer the human's own
    /// question.
    fn shell_control_list_windows(&self) -> Vec<ShellWindowInfo> {
        let focused_surface = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.space
            .elements()
            .map(|w| {
                let (app_id, title) = codrive::window_target::window_identity(w);
                let focused = focused_surface.as_ref() == Some(w.toplevel().unwrap().wl_surface());
                ShellWindowInfo { app_id, title, focused }
            })
            .collect()
    }

    /// Reuses `codrive::window_target::find_target_window` — the EXACT
    /// same matching policy (exact app_id first, title-prefix fallback,
    /// lowest-z-order tie-break) `codrive`'s own `activate_window` op
    /// already proved live (BUILD.md's "WP-CD4a-COMP" section) — but
    /// applies the result to the HUMAN seat (`self.seat`, never
    /// `self.agent_seat`) and audits to THIS module's own log, not
    /// `codrive`'s (see this module's own doc comment for both reasons).
    fn shell_control_focus_window(&mut self, query: String) -> ShellControlResponse {
        match codrive::window_target::find_target_window(&self.space, &query) {
            Some((window, matched)) => {
                let seat = self.seat.clone();
                let serial = SERIAL_COUNTER.next_serial();
                self.focus_window(&seat, Some(&window), serial);

                let (resp, detail) = match &matched {
                    WindowMatch::AppId(id) => (
                        ShellControlResponse::focused_by_app_id(id.clone()),
                        format!("query={query:?} matched_app_id={id:?}"),
                    ),
                    WindowMatch::TitlePrefix(title) => (
                        ShellControlResponse::focused_by_title_prefix(title.clone()),
                        format!("query={query:?} matched_via=title_prefix matched_title={title:?}"),
                    ),
                };
                tracing::info!(query = %query, detail = %detail, "shell_control: focus_window — window focused");
                self.shell_control.record("focus_window", Some(detail));
                resp
            }
            None => {
                tracing::info!(query = %query, "shell_control: focus_window — no toplevel matched by app_id or title prefix");
                self.shell_control.record("focus_window_failed", Some(format!("query={query:?}")));
                ShellControlResponse::err("not_found")
            }
        }
    }
}
