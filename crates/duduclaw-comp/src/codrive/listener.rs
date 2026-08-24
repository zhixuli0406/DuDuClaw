// CD-0 codrive spike — the agent injection socket's accept/read loop.
// DESIGN-codrive-desktop-2026-08.md §3.3.1 + §6 (safety redlines): a
// private Unix socket at `$XDG_RUNTIME_DIR/duduclaw-codrive.sock`, JSON
// lines in, one JSON-line ack out per command.
//
// **CD-1 update (task brief req 1, DESIGN §3.3.1 "注入通道比照 KWin EIS
// 界線" / §6 red line 2): this channel is now authenticated.** Every new
// connection's first line must be `{"op":"auth","token":"<hex>"}`, checked
// against a fresh 32-byte token this process generated at startup and
// wrote to `$XDG_RUNTIME_DIR/duduclaw-codrive.token` (mode 0600) — see
// `codrive::init`'s token-generation code and `CodriveShared::
// check_token`. Only a caller that can read that file (i.e. the gateway
// process, running as the same user, or root) can drive the agent seat.
// The CD-0-era mitigations below are now defense-in-depth on top of that,
// not the only protection:
//
// The socket file is chmod 0600 and lives inside `$XDG_RUNTIME_DIR` (which
// the OS/session-manager already keeps 0700 per-user), and only one
// connection is accepted at a time (a second connect attempt just queues
// in the kernel backlog until the first disconnects).
//
// **CD-2 update (task brief req 1, DESIGN §9 CD-1 carry-forward "socket
// rotation"): the token can now be rotated without restarting this
// process.** Two triggers, both funnelling into `CodriveShared::
// rotate_token`: an authenticated connection sending `{"op":
// "rotate_token"}` (handled below, alongside `status`/`resume`), or this
// process receiving `SIGHUP` (see `mod.rs`'s `block_sighup_on_current_
// thread`/`spawn_sighup_rotation_thread`). Either way the OLD token stops
// authenticating new connections immediately (the in-memory value
// `check_token` reads is swapped under a mutex) while any connection that
// is already past `authenticate()` below — including the one that may have
// just requested the rotation — is completely unaffected, since
// `check_token` is only ever consulted once, right here, at the very start
// of a connection's life.
//
// **WP-CD2-freeze-scope update (DESIGN §3.1 point 3): the frozen check
// below is no longer an unconditional deny.** It stays an OPTIMISTIC
// pre-check (this thread has no `self.space`/seat access — see `mod.rs`'s
// "Authority for the freeze gate" note), but now forwards a command to the
// main thread instead of denying outright when a shadow session might make
// it eligible for a freeze bypass; the main thread's `handle_agent_inject`
// (`shadow::is_freeze_bypass_eligible`) makes the real, per-op decision.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
};

use smithay::reexports::calloop;

use super::{
    protocol::{AuthLine, InjectCmd},
    CodriveShared,
};

/// Generous but bounded — this is a local control channel, not a network
/// API, but an unbounded `BufRead::lines()` loop on a line nobody ever
/// terminates would still be an easy local DoS against this one thread.
const MAX_LINE_BYTES: usize = 8192;

pub fn spawn(
    sock_path: PathBuf,
    shared: Arc<CodriveShared>,
    tx: calloop::channel::Sender<InjectCmd>,
) -> std::io::Result<()> {
    // Stale socket file from a previous crashed run of this same binary —
    // `bind` would otherwise fail with AddrInUse.
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path)?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;

    if let Some(parent) = sock_path.parent() {
        if let Ok(meta) = std::fs::metadata(parent) {
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o700 {
                tracing::warn!(
                    dir = %parent.display(),
                    mode = format!("{mode:o}"),
                    "codrive: injection socket's parent directory is not 0700 — socket \
                     privacy depends on the caller-provided $XDG_RUNTIME_DIR perms too"
                );
            }
        }
    }

    tracing::info!(
        path = %sock_path.display(),
        "codrive: agent injection socket listening (single connection, token-authenticated \
         — see this module's doc comment)"
    );

    std::thread::Builder::new()
        .name("codrive-inject".into())
        .spawn(move || accept_loop(listener, shared, tx))
        .map(|_handle| ())
}

fn accept_loop(listener: UnixListener, shared: Arc<CodriveShared>, tx: calloop::channel::Sender<InjectCmd>) {
    // Single connection at a time by construction: `handle_conn` below only
    // returns once its connection disconnects (client EOF, read error, or a
    // force-close from `emergency_stop`), and we don't call `accept()` again
    // until it does. A second concurrent connect attempt just sits in the
    // kernel's listen backlog until then.
    loop {
        let (stream, _addr) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(error = %e, "codrive: injection socket accept() failed — listener thread exiting");
                return;
            }
        };

        // Session bookkeeping (clearing `terminated`, recording
        // `session_started`, publishing `active_conn`) happens INSIDE
        // `handle_conn`, gated behind a successful auth handshake — see
        // that function's doc comment. A connection that never
        // authenticates never touches any of that state.
        handle_conn(stream, &shared, &tx);
    }
}

/// First-line auth gate (CD-1, DESIGN §3.3.1 "注入通道比照 KWin EIS 界線" +
/// task brief req 1). Every newly accepted connection must present this
/// run's token as its very first line, `{"op":"auth","token":"<hex>"}`,
/// before anything else — including the session-lifecycle bookkeeping that,
/// before this round, ran unconditionally on every `accept()` (see
/// `handle_conn`'s comment right after this function is called). Returns
/// `true` iff the caller may proceed to the authenticated command loop; on
/// any failure this has already written the failure ack and recorded an
/// `auth_fail` audit line. The *presented* token value is deliberately
/// never included in that audit line or any log line — it's untrusted
/// caller-supplied DATA on a channel that exists specifically to resist an
/// unauthenticated local process, so echoing it back into a persisted
/// audit trail would undercut the point of having a secret.
fn authenticate(reader: &mut BufReader<UnixStream>, writer: &mut UnixStream, shared: &Arc<CodriveShared>) -> bool {
    fn deny(shared: &Arc<CodriveShared>, writer: &mut UnixStream, reason: &str) -> bool {
        shared.record("auth_fail", None, None, None, Some(reason.to_string()));
        let _ = writeln!(writer, r#"{{"ok":false,"error":"auth_failed"}}"#);
        false
    }

    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return deny(shared, writer, "connection closed before an auth line was sent"),
        Ok(_) => {}
        Err(e) => return deny(shared, writer, &format!("read error before auth: {e}")),
    }

    let trimmed = line.trim();
    if trimmed.len() > MAX_LINE_BYTES {
        return deny(shared, writer, "auth line exceeded the max line length");
    }

    let presented_token = match serde_json::from_str::<AuthLine>(trimmed) {
        Ok(AuthLine { op, token }) if op == "auth" => token,
        Ok(_) => return deny(shared, writer, "first line's \"op\" was not \"auth\""),
        Err(_) => return deny(shared, writer, "first line was not valid auth JSON"),
    };

    if !shared.check_token(&presented_token) {
        return deny(shared, writer, "token mismatch");
    }

    let _ = writeln!(writer, r#"{{"ok":true,"authenticated":true}}"#);
    true
}

fn handle_conn(stream: UnixStream, shared: &Arc<CodriveShared>, tx: &calloop::channel::Sender<InjectCmd>) {
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "codrive: could not clone the connection for writing acks — closing");
            return;
        }
    };
    let mut reader = BufReader::new(stream);

    if !authenticate(&mut reader, &mut writer, shared) {
        return;
    }

    // Auth succeeded — only now does this connection get to affect
    // compositor-visible session state (task brief req 1 "安全順序修正" /
    // DESIGN §6 red line 2). Before this round, this bookkeeping ran
    // unconditionally on every `accept()`, before a single byte had been
    // read from the client — an unauthenticated connection could clear a
    // real emergency-stop's `terminated` flag just by existing. See this
    // file's `unauthenticated_connection_does_not_clear_terminated` test.
    shared.terminated.store(false, Ordering::SeqCst);
    shared.record("session_started", None, None, None, None);
    if let Ok(clone) = reader.get_ref().try_clone() {
        if let Ok(mut guard) = shared.active_conn.lock() {
            *guard = Some(clone);
        }
    }
    // A2 (`codrive/mode.rs`): the lock-free twin of `active_conn`, set HERE
    // and nowhere earlier — an unauthenticated connection must not be able to
    // flip the compositor into `codrive` mode (the same "all session
    // bookkeeping lives behind the auth gate" rule the `terminated` store
    // above already follows, and which `tests_listener.rs`'s
    // `unauthenticated_connection_does_not_*` pair pins).
    shared.session_active.store(true, Ordering::SeqCst);

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                tracing::info!("codrive: agent connection closed (EOF)");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "codrive: injection socket read error — closing connection");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_LINE_BYTES {
            let _ = writeln!(writer, r#"{{"ok":false,"error":"line_too_long"}}"#);
            continue;
        }

        // Every field the wire protocol accepts is DATA from an untrusted
        // local caller — parsed and range-checked here, never trusted to
        // already be well-formed by the time it would reach seat calls on
        // the main thread.
        let cmd: InjectCmd = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(e) => {
                shared.record("inject_parse_error", None, None, None, Some(e.to_string()));
                let _ = writeln!(writer, r#"{{"ok":false,"error":"parse_error"}}"#);
                continue;
            }
        };

        if let Err(reason) = validate(&cmd) {
            shared.record("inject_parse_error", None, None, None, Some(reason.clone()));
            let _ = writeln!(writer, "{{\"ok\":false,\"error\":{}}}", json_str(&reason));
            continue;
        }

        // `status` and `resume` never touch the seat or the injection
        // channel — handled directly here, before the `terminated` gate
        // below (task brief req 3: status is read-only and answered "even
        // while frozen"; req 2: resume is now unconditionally denied, so
        // its answer doesn't need to depend on `terminated` either).
        match cmd {
            InjectCmd::Status => {
                // CD-3: `takeover` distinguishes an agent-initiated hand-off
                // from an ordinary human-triggered freeze — both read
                // `frozen:true`, but only a take_over also flips this. Still
                // answered here, unconditionally (task brief item 2:
                // "status 除外" — the one query op that always answers, even
                // mid-takeover, so the driver's `wait_for_resume` poll keeps
                // working).
                //
                // A2: the reply gained `mode`/`handover_reason` plus the
                // `shadow`/`watch_active`/`watch_paused` mirrors. The three
                // pre-A2 fields keep their exact spelling AND position — see
                // `mode::status_reply_line`, which is the one place that
                // formats this and is pinned byte-for-byte by its own tests.
                // Still a pure atomic read needing no seat access, so it is
                // still answered on this thread.
                let snap = super::mode::status_snapshot(shared);
                let _ = writeln!(writer, "{}", super::mode::status_reply_line(&snap));
                continue;
            }
            InjectCmd::Resume => {
                // CD-1 (task brief req 2 / DESIGN §3.1 "交還是明確動作
                // （按鈕/Super+Enter）"): resume/"交還" is human-side only
                // now — see `DuduclawComp::human_resume` (codrive/mod.rs),
                // wired to Super+Enter in `input.rs`. The variant stays
                // (not removed) so a caller still trying the CD-0-era
                // socket-resume path gets a specific, named denial instead
                // of a generic parse error.
                shared.record(
                    "resume_denied",
                    Some("resume"),
                    None,
                    None,
                    Some(
                        "resume is human-side only (Super+Enter) — socket resume requests are \
                         always denied"
                            .into(),
                    ),
                );
                let _ = writeln!(writer, r#"{{"ok":false,"error":"resume_is_human_only"}}"#);
                continue;
            }
            InjectCmd::WindowGeometry { ref app_id, pid } => {
                // WP-CD4b-fix (B3). Grouped with `status`/`resume`/
                // `rotate_token` — before the `terminated`/`frozen` gates —
                // because it is a READ-ONLY query, not an action: it moves
                // no window, touches no seat, changes no pixel. Denying it
                // under freeze would make the gateway MORE likely to click
                // the wrong place, not less: a failed locate falls back to
                // the step's literal C-L1 coordinate, while the click that
                // follows stays gated by these same checks exactly as
                // before. See `window_geometry.rs`'s "Not an action"
                // section.
                //
                // Unlike its three neighbours, this one cannot be answered
                // from `CodriveShared` alone — it needs `self.space`, which
                // only the calloop main thread may touch — so it round
                // trips over the oneshot bridge (bounded by
                // `QUERY_REPLY_TIMEOUT`; a stalled main loop degrades this
                // one query, it does not wedge this thread).
                let reply = shared.query_window_geometry(super::window_geometry::WindowGeometryRequest {
                    pid,
                    app_id: app_id.clone(),
                });
                let line = serde_json::to_string(&reply)
                    .unwrap_or_else(|_| r#"{"ok":false,"error":"internal_serialize_error"}"#.to_string());
                let _ = writeln!(writer, "{line}");
                continue;
            }
            InjectCmd::RotateToken => {
                // CD-2 (task brief item 1): control-plane, like Status/
                // Resume above — never touches the seat, so it's handled
                // synchronously here rather than going through the
                // channel. Reaching this arm already proves the caller
                // held the token being replaced (it's past `authenticate`
                // above); see `CodriveShared::rotate_token`'s doc for why
                // this connection stays unaffected by its own request.
                match shared.rotate_token("socket_op") {
                    Ok(()) => {
                        let _ = writeln!(writer, r#"{{"ok":true,"rotated":true}}"#);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "codrive: socket-triggered token rotation failed");
                        let _ = writeln!(writer, r#"{{"ok":false,"error":"rotate_failed"}}"#);
                    }
                }
                continue;
            }
            _ => {}
        }

        if shared.terminated.load(Ordering::SeqCst) {
            let _ = writeln!(writer, r#"{{"ok":false,"error":"session_terminated"}}"#);
            continue;
        }

        // D3-c backstop. An input method holding the agent seat's keyboard
        // grab eats every injected key, so a "success" ack here would be a
        // lie — the caller would see `type_text` succeed and nothing typed.
        // Same optimistic-mirror shape as the `shadow_active`/
        // `takeover_active` pre-checks below: the main thread re-checks
        // authoritatively in `handle_agent_inject` and drops there too, so a
        // race can lose a keystroke but can never let one through silently.
        // The mirror is refreshed once per housekeeping tick as well as per
        // command, so it clears on its own when the input method exits.
        if shared.ime_paused.load(Ordering::SeqCst) && cmd.is_keyboard_op() {
            let (op, x, y) = cmd.describe();
            shared.record(
                "inject_dropped",
                Some(op),
                x,
                y,
                Some("paused_by_ime: an input method holds the agent seat's keyboard grab".into()),
            );
            let _ = writeln!(
                writer,
                r#"{{"ok":false,"error":"paused_by_ime","reason":"input_method_holds_agent_seat_keyboard"}}"#
            );
            continue;
        }

        let frozen = shared.frozen.load(Ordering::SeqCst);
        if frozen {
            // Freeze policy (DESIGN §3.1, "作用域" note + task brief):
            // dropped, not buffered. A buffered command executes at an
            // unpredictable later moment the human never agreed to — after
            // a takeover, the desktop may look completely different, so
            // replaying a stale click/keystroke is a worse surprise than
            // simply losing that one intent. The agent finds out via this
            // ack (`"frozen":true`) and can re-issue the command after a
            // human resume.
            //
            // WP-CD2-freeze-scope: this thread has no `self.space`/seat
            // access to confirm a specific op's target is inside the
            // shadow output — only the main thread's `handle_agent_inject`
            // can make that precise, authoritative call (`shadow::
            // is_freeze_bypass_eligible`). So this is only an OPTIMISTIC
            // pre-check: if a shadow session is active at all AND `cmd`
            // isn't the `Shadow` toggle itself (never bypass-eligible, in
            // either direction — DESIGN's "凍結中不可進入 shadow"), forward
            // it and let the main thread decide for real; it may still get
            // dropped there. Everything else denies here exactly as
            // before this WP — byte-identical for the non-shadow case.
            // CD-3 (takeover.rs module doc): an active takeover is a TOTAL
            // freeze — no shadow-bypass exception, unlike an ordinary
            // human-triggered freeze. Mirrored here as an atomic (like
            // `shadow_active`) purely for this optimistic pre-check; the
            // main thread's `handle_agent_inject` makes the authoritative
            // call the same way.
            let shadow_bypass_candidate = shared.shadow_active.load(Ordering::SeqCst)
                && !shared.takeover_active.load(Ordering::SeqCst)
                && !matches!(cmd, InjectCmd::Shadow { .. });

            if !shadow_bypass_candidate {
                let (op, x, y) = cmd.describe();
                shared.record(
                    "inject_dropped",
                    Some(op),
                    x,
                    y,
                    Some("agent seat frozen (human input active) — dropped, not buffered".into()),
                );
                let _ = writeln!(writer, r#"{{"ok":false,"frozen":true,"reason":"agent_seat_frozen"}}"#);
                continue;
            }
        }

        if tx.send(cmd).is_err() {
            tracing::error!("codrive: injection channel closed — compositor event loop gone");
            let _ = writeln!(writer, r#"{{"ok":false,"error":"compositor_unavailable"}}"#);
            break;
        }
        // `frozen` here may be `true` (a shadow-bypass candidate just got
        // forwarded above) — the ack's `frozen` field always reflects
        // reality now rather than being hardcoded `false`, matching the
        // gateway client's own doc'd contract (`{"ok":true,"frozen":bool}`,
        // `duduclaw-gateway/src/codrive/client.rs`).
        let _ = writeln!(writer, r#"{{"ok":true,"frozen":{frozen}}}"#);
    }

    // Cleanup for every exit path above (EOF, read error, or the injection
    // channel going away) — this only ever runs for a connection that
    // authenticated (everything above is past the `authenticate` gate),
    // mirroring `session_started`'s placement.
    if let Ok(mut guard) = shared.active_conn.lock() {
        *guard = None;
    }
    // A2: cleared in lockstep with `active_conn` above. The main thread's
    // per-frame `codrive_sync_mode` is what turns this into an observable
    // `driving_mode` transition back to `human` — this thread cannot touch
    // the audit-worthy transition itself, only the flag it derives from.
    shared.session_active.store(false, Ordering::SeqCst);
    shared.record("session_ended", None, None, None, None);
}

/// Field-level validation for the variants that carry free-form strings.
/// Rejecting here (not just in `codrive::exec` on the main thread) means a
/// malformed command never even reaches the channel — the socket thread is
/// the trust boundary, `codrive::exec` is trusted executor. `pub(super)` (not
/// private) so `codrive/tests_takeover.rs`/`codrive/tests_listener.rs` can
/// exercise it directly — see those files' module docs for why their
/// scenarios live outside this file's own (now removed, WP-CD4a-COMP —
/// moved wholesale to `tests_listener.rs` to stay under the 800-line
/// per-file cap) `#[cfg(test)]` block.
pub(super) fn validate(cmd: &InjectCmd) -> Result<(), String> {
    match cmd {
        InjectCmd::Button { btn, state } => {
            super::protocol::parse_button_code(btn)?;
            super::protocol::parse_press_state(state)?;
            Ok(())
        }
        InjectCmd::Key { state, .. } => {
            super::protocol::parse_press_state(state)?;
            Ok(())
        }
        InjectCmd::KeyName { name, state } => {
            super::protocol::parse_press_state(state)?;
            if super::keymap_ascii::key_name_to_xkb(name).is_none() {
                return Err(format!(
                    "unknown key_name {name:?} — see the allowlist in keymap_ascii.rs"
                ));
            }
            Ok(())
        }
        InjectCmd::Move { x, y } => {
            if !x.is_finite() || !y.is_finite() {
                return Err("x/y must be finite".into());
            }
            Ok(())
        }
        InjectCmd::Highlight { x, y, w, h, ms: _ } => {
            if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
                return Err("highlight x/y/w/h must be finite".into());
            }
            if *w <= 0.0 || *h <= 0.0 {
                return Err("highlight w/h must be > 0".into());
            }
            Ok(())
        }
        InjectCmd::TakeOver { reason } => {
            if reason.len() > super::protocol::MAX_TAKE_OVER_REASON_BYTES {
                return Err(format!(
                    "take_over reason exceeds {} bytes",
                    super::protocol::MAX_TAKE_OVER_REASON_BYTES
                ));
            }
            Ok(())
        }
        // WP-CD4a-COMP: empty rejected outright (an empty query would
        // title-prefix-match every window in `window_target.rs`'s fallback
        // — see that file's `find_target_window`), oversized rejected same
        // shape as `TakeOver`'s reason cap above.
        InjectCmd::ActivateWindow { app_id } => {
            if app_id.is_empty() {
                return Err("activate_window app_id must not be empty".into());
            }
            if app_id.len() > super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES {
                return Err(format!(
                    "activate_window app_id exceeds {} bytes",
                    super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES
                ));
            }
            Ok(())
        }
        // WP-CD4b-fix (B3): an identity-less query must be refused here, not
        // "helpfully" matched against whatever single window happens to be
        // mapped — see `window_geometry::resolve_window`'s fail-closed
        // policy. Field caps reuse `activate_window`'s query cap (same
        // vocabulary, same reject-don't-truncate reasoning).
        InjectCmd::WindowGeometry { app_id, pid } => {
            if app_id.is_none() && pid.is_none() {
                return Err("window_geometry requires at least one of app_id/pid".into());
            }
            if let Some(app_id) = app_id {
                if app_id.is_empty() {
                    return Err("window_geometry app_id must not be empty".into());
                }
                if app_id.len() > super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES {
                    return Err(format!(
                        "window_geometry app_id exceeds {} bytes",
                        super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES
                    ));
                }
            }
            // pid 0 is the kernel's swapper/idle task — never a Wayland
            // client, so a caller sending it is confused, not unlucky.
            if *pid == Some(0) {
                return Err("window_geometry pid must be > 0".into());
            }
            Ok(())
        }
        InjectCmd::Text { .. }
        | InjectCmd::Resume
        | InjectCmd::Status
        | InjectCmd::RotateToken
        | InjectCmd::Shadow { .. }
        | InjectCmd::Watch { .. } => Ok(()),
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"invalid\"".to_string())
}

// This file's own `#[cfg(test)] mod tests` moved to `codrive/tests_listener.rs`
// in the WP-CD4a-COMP round — this file was over the project's 800-line
// per-file cap once this round's `activate_window` `validate()` arm above
// was added. See that file's module doc for the full reasoning (same
// "new/split scenarios get their own `tests_<topic>.rs`" pattern
// `tests_takeover.rs` already established for CD-3).
