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
                let frozen = shared.frozen.load(Ordering::SeqCst);
                let terminated = shared.terminated.load(Ordering::SeqCst);
                let _ = writeln!(writer, r#"{{"ok":true,"frozen":{frozen},"terminated":{terminated}}}"#);
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
            _ => {}
        }

        if shared.terminated.load(Ordering::SeqCst) {
            let _ = writeln!(writer, r#"{{"ok":false,"error":"session_terminated"}}"#);
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

        if tx.send(cmd).is_err() {
            tracing::error!("codrive: injection channel closed — compositor event loop gone");
            let _ = writeln!(writer, r#"{{"ok":false,"error":"compositor_unavailable"}}"#);
            break;
        }
        let _ = writeln!(writer, r#"{{"ok":true,"frozen":false}}"#);
    }

    // Cleanup for every exit path above (EOF, read error, or the injection
    // channel going away) — this only ever runs for a connection that
    // authenticated (everything above is past the `authenticate` gate),
    // mirroring `session_started`'s placement.
    if let Ok(mut guard) = shared.active_conn.lock() {
        *guard = None;
    }
    shared.record("session_ended", None, None, None, None);
}

/// Field-level validation for the variants that carry free-form strings.
/// Rejecting here (not just in `codrive::exec` on the main thread) means a
/// malformed command never even reaches the channel — the socket thread is
/// the trust boundary, `codrive::exec` is trusted executor.
fn validate(cmd: &InjectCmd) -> Result<(), String> {
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
        InjectCmd::Text { .. } | InjectCmd::Resume | InjectCmd::Status => Ok(()),
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"invalid\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_non_finite_highlight() {
        let cmd = InjectCmd::Highlight { x: f64::NAN, y: 0.0, w: 10.0, h: 10.0, ms: None };
        assert!(validate(&cmd).is_err());
    }

    #[test]
    fn validate_rejects_non_positive_highlight_size() {
        let zero_w = InjectCmd::Highlight { x: 0.0, y: 0.0, w: 0.0, h: 10.0, ms: None };
        assert!(validate(&zero_w).is_err());
        let negative_h = InjectCmd::Highlight { x: 0.0, y: 0.0, w: 10.0, h: -5.0, ms: None };
        assert!(validate(&negative_h).is_err());
    }

    #[test]
    fn validate_accepts_reasonable_highlight() {
        let cmd = InjectCmd::Highlight { x: 10.0, y: 20.0, w: 100.0, h: 50.0, ms: Some(500) };
        assert!(validate(&cmd).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_key_name() {
        let cmd = InjectCmd::KeyName { name: "nonexistent".into(), state: "press".into() };
        assert!(validate(&cmd).is_err());
    }

    #[test]
    fn validate_accepts_known_key_name() {
        let cmd = InjectCmd::KeyName { name: "enter".into(), state: "press".into() };
        assert!(validate(&cmd).is_ok());
    }

    /// The red-line regression test (task brief req 1's "安全順序修正",
    /// same violation class the CD-0 acceptance re-run already found once
    /// for the plain-reconnect case — see BUILD.md's "Acceptance re-run
    /// findings"). Simulates a just-happened emergency stop (`terminated`
    /// = true), then connects and presents a WRONG token: the connection
    /// must be denied without ever clearing `terminated`.
    #[test]
    fn unauthenticated_connection_does_not_clear_terminated() {
        let sock_path =
            std::env::temp_dir().join(format!("duduclaw-codrive-test-badauth-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);

        let shared = Arc::new(CodriveShared::for_test(Some("expected-token".to_string())));
        shared.terminated.store(true, Ordering::SeqCst);

        let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
        spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        writeln!(writer, r#"{{"op":"auth","token":"definitely-wrong"}}"#).unwrap();

        let mut reply = String::new();
        BufReader::new(&conn).read_line(&mut reply).expect("no auth response from listener");
        assert!(reply.contains("auth_failed"), "unexpected auth response: {reply}");

        assert!(
            shared.terminated.load(Ordering::SeqCst),
            "an unauthenticated connection must never clear `terminated` \
             (DESIGN-codrive-desktop §6 red line 2/3)"
        );

        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn correctly_authenticated_connection_is_accepted() {
        let sock_path =
            std::env::temp_dir().join(format!("duduclaw-codrive-test-goodauth-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);

        let shared = Arc::new(CodriveShared::for_test(Some("right-token".to_string())));
        let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
        spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        writeln!(writer, r#"{{"op":"auth","token":"right-token"}}"#).unwrap();

        let mut reply = String::new();
        BufReader::new(&conn).read_line(&mut reply).expect("no auth response from listener");
        assert!(reply.contains(r#""authenticated":true"#), "unexpected auth response: {reply}");

        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn resume_over_socket_is_always_denied_and_never_clears_frozen() {
        let sock_path =
            std::env::temp_dir().join(format!("duduclaw-codrive-test-resume-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);

        let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
        shared.frozen.store(true, Ordering::SeqCst); // simulate an active human freeze
        let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
        spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        let mut br = BufReader::new(&conn);

        writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
        let mut reply = String::new();
        br.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""authenticated":true"#));

        writeln!(writer, r#"{{"op":"resume"}}"#).unwrap();
        reply.clear();
        br.read_line(&mut reply).unwrap();
        assert!(reply.contains("resume_is_human_only"), "unexpected resume response: {reply}");
        assert!(
            shared.frozen.load(Ordering::SeqCst),
            "socket resume must never clear an active freeze — resume is human-side only"
        );

        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn status_answers_even_while_frozen_without_touching_seat_state() {
        let sock_path =
            std::env::temp_dir().join(format!("duduclaw-codrive-test-status-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);

        let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
        shared.frozen.store(true, Ordering::SeqCst);
        let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
        spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        let mut br = BufReader::new(&conn);

        writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
        let mut reply = String::new();
        br.read_line(&mut reply).unwrap();

        writeln!(writer, r#"{{"op":"status"}}"#).unwrap();
        reply.clear();
        br.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""frozen":true"#), "unexpected status response: {reply}");
        assert!(reply.contains(r#""terminated":false"#), "unexpected status response: {reply}");

        let _ = std::fs::remove_file(&sock_path);
    }
}
