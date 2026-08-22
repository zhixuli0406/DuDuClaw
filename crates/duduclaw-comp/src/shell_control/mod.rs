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
//! line is in, not a field inside a shared, disputable file. Only the
//! ACTIONS — `focus_window` and CUR-2's `set_cursor_source` — are audited,
//! mirroring the established "queries aren't audited, actions are"
//! precedent `codrive::listener`'s own `status`/`resume` handling already
//! set — `list_windows` and `get_cursor_source` are read-only and,
//! realistically, polled every few seconds by a live dock / re-read every
//! time a settings page opens, so auditing them would mostly be noise, not
//! evidence.
//!
//! ## CUR-2 (2026-08-22): this socket also carries appearance preferences
//! `get_cursor_source` / `set_cursor_source` let the shell switch the human
//! pointer between the machine's system cursors and the DuDuClaw brand paw
//! **without restarting the compositor**. They live here rather than on
//! `codrive`'s socket for exactly the reason `focus_window` does: choosing
//! how your own pointer looks is a HUMAN act, and attributing it to the
//! agent in the codrive trail would be the audit poisoning this module
//! exists to prevent. The same-uid `SO_PEERCRED` boundary is also precisely
//! the right authority for it — only a process running as this kiosk
//! session's own user may change how that session looks, and an agent CLI
//! subprocess (a DIFFERENT system user) structurally cannot reach the
//! socket at all.
//!
//! ### How the shell calls them
//! ```text
//! -> {"op":"get_cursor_source"}
//! <- {"ok":true,"cursor":{"source":"system","requested":"system",
//!                         "theme":"Adwaita","origin":"default",
//!                         "env_pinned":false}}
//!
//! -> {"op":"set_cursor_source","params":{"source":"brand"}}
//! <- {"ok":true,"cursor":{"source":"brand","requested":"brand",
//!                         "theme":"DuDuClaw","origin":"runtime",
//!                         "env_pinned":false,"persisted":true}}
//!
//! -> {"op":"set_cursor_source","params":{"source":"brnad"}}
//! <- {"ok":false,"error":"invalid_cursor_source"}
//! ```
//! A settings page drives its radio state from `requested` (the user's own
//! choice) and can warn from two honest signals: `source != requested`
//! means the brand theme package is not installed and system cursors are
//! being drawn instead; `env_pinned: true` means an operator pinned
//! `DUDUCLAW_COMP_CURSOR_SOURCE` in comp's spawn environment, so the stored
//! preference will not apply at the next start. Building that page is
//! deliberately NOT part of CUR-2 — the op shape is.

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
            ShellControlRequest::GetCursorSource => ShellControlResponse::cursor(self.cursor_source_info()),
            ShellControlRequest::SetCursorSource { source } => self.shell_control_set_cursor_source(&source),
        }
    }

    /// CUR-2: switch the human pointer's artwork source live, then persist
    /// the choice.
    ///
    /// Ordering matters and is deliberate: **apply first, persist second.**
    /// The switch is what the caller asked for and it cannot fail; writing
    /// the preference file can (read-only home, full disk, a service account
    /// with no `$HOME`). Persisting first would mean a failed write blocked a
    /// switch that would otherwise have worked. Doing it this way, a write
    /// failure degrades to "live now, gone after a restart" — reported
    /// honestly as `persisted: false` + `persist_error`, never swallowed.
    ///
    /// `source` has already been through `listener::validate`, so
    /// `parse_strict` here cannot fail; it is re-parsed rather than passed as
    /// an enum because the wire type is a string and the parse is the
    /// boundary. The `unreachable`-style fallback is written as a real error
    /// response, not a panic — a validation gap must not take the compositor
    /// down.
    fn shell_control_set_cursor_source(&mut self, source: &str) -> ShellControlResponse {
        let Some(requested) = crate::cursor::source::CursorSource::parse_strict(source) else {
            tracing::error!(
                "shell_control: set_cursor_source reached the main thread with a value \
                 listener::validate should have refused — refusing here too"
            );
            self.shell_control.record("set_cursor_source_failed", Some("invalid_cursor_source".to_string()));
            return ShellControlResponse::err("invalid_cursor_source");
        };

        let changed = self.set_cursor_source(requested);

        let (persisted, persist_error) = match crate::cursor::persist::store(requested) {
            Ok(path) => {
                tracing::debug!(path = %path.display(), "cursor: preference stored");
                (true, None)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cursor: the source switch is live but could not be persisted — it will \
                     revert at the next compositor start"
                );
                (false, Some(e))
            }
        };

        let mut info = self.cursor_source_info();
        info.persisted = Some(persisted);
        info.persist_error = persist_error.clone();

        // Audited: this is an ACTION with a real, user-visible effect, unlike
        // `list_windows`/`get_cursor_source`. The detail records the outcome
        // (including the fail-safe's effective value and a persistence
        // failure), not just the request — "what did the machine actually do"
        // is what an audit line is for.
        self.shell_control.record(
            "set_cursor_source",
            Some(format!(
                "requested={requested} effective={} theme={:?} changed={changed} persisted={persisted}{}",
                info.source,
                info.theme,
                match &persist_error {
                    Some(e) => format!(" persist_error={e:?}"),
                    None => String::new(),
                }
            )),
        );

        ShellControlResponse::cursor(info)
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
