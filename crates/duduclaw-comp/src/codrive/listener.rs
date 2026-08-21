// CD-0 codrive spike — the agent injection socket's accept/read loop.
// DESIGN-codrive-desktop-2026-08.md §3.3.1 + §6 (safety redlines): a
// private Unix socket at `$XDG_RUNTIME_DIR/duduclaw-codrive.sock`, JSON
// lines in, one JSON-line ack out per command.
//
// **Spike simplification, explicitly flagged (task brief + DESIGN §6.2):
// this channel is unauthenticated.** Whoever can connect to the socket has
// full agent-seat control for as long as the connection lives. Production
// (CD-1+) needs to bind this to a caller identity the same way the gateway
// authenticates its other privileged channels (bus queue / MCP) — tracked
// as a known gap, not silently forgotten. The mitigations that DO exist at
// CD-0: the socket file is chmod 0600 and lives inside `$XDG_RUNTIME_DIR`
// (which the OS/session-manager already keeps 0700 per-user), and only one
// connection is accepted at a time (a second connect attempt just queues in
// the kernel backlog until the first disconnects) — not real authentication,
// but it does mean "any local process" rather than "any network peer", and
// it rules out two agents fighting over the seat concurrently.

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

use super::{protocol::InjectCmd, CodriveShared};

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
        "codrive: agent injection socket listening (single connection, unauthenticated \
         — CD-1 adds caller-identity auth, see module doc)"
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

        // A brand-new connection clears a prior emergency-stop's `terminated`
        // flag (deliberate CD-0 choice — see the state-machine note in
        // `codrive/mod.rs` — rather than requiring a full process restart
        // after every emergency stop). It must NOT clear `frozen`: the freeze
        // is set by HUMAN input and DESIGN-codrive-desktop §6 red line 3 says
        // the agent cannot circumvent it — an earlier revision reset `frozen`
        // here, which let a simple reconnect bypass an active human freeze
        // (caught by the acceptance re-run's cross-connection probe). Only an
        // explicit `resume` op clears it; CD-1 moves resume issuance to the
        // human-side channel entirely.
        shared.terminated.store(false, Ordering::SeqCst);
        shared.record("session_started", None, None, None, None);

        if let Ok(clone) = stream.try_clone() {
            if let Ok(mut guard) = shared.active_conn.lock() {
                *guard = Some(clone);
            }
        }

        handle_conn(stream, &shared, &tx);

        if let Ok(mut guard) = shared.active_conn.lock() {
            *guard = None;
        }
        shared.record("session_ended", None, None, None, None);
    }
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
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                tracing::info!("codrive: agent connection closed (EOF)");
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "codrive: injection socket read error — closing connection");
                return;
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

        // Every field the wire protocol accepts (btn/state/keycode/text) is
        // DATA from an untrusted local caller — parsed and range-checked
        // here, never trusted to already be well-formed by the time it
        // would reach seat calls on the main thread.
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

        if shared.terminated.load(Ordering::SeqCst) {
            let _ = writeln!(writer, r#"{{"ok":false,"error":"session_terminated"}}"#);
            continue;
        }

        match cmd {
            InjectCmd::Resume => {
                shared.frozen.store(false, Ordering::SeqCst);
                shared.record("resume", Some("resume"), None, None, None);
                let _ = writeln!(writer, r#"{{"ok":true,"frozen":false}}"#);
            }
            other => {
                let frozen = shared.frozen.load(Ordering::SeqCst);
                if frozen {
                    // Freeze policy (DESIGN §3.1, "作用域" note + task brief):
                    // dropped, not buffered. A buffered command executes at
                    // an unpredictable later moment the human never agreed
                    // to — after a takeover, the desktop may look completely
                    // different, so replaying a stale click/keystroke is a
                    // worse surprise than simply losing that one intent. The
                    // agent finds out via this ack (`"frozen":true`) and can
                    // re-issue the command after `resume`.
                    let (op, x, y) = other.describe();
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

                if tx.send(other).is_err() {
                    tracing::error!("codrive: injection channel closed — compositor event loop gone");
                    let _ = writeln!(writer, r#"{{"ok":false,"error":"compositor_unavailable"}}"#);
                    return;
                }
                let _ = writeln!(writer, r#"{{"ok":true,"frozen":false}}"#);
            }
        }
    }
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
        InjectCmd::Move { x, y } => {
            if !x.is_finite() || !y.is_finite() {
                return Err("x/y must be finite".into());
            }
            Ok(())
        }
        InjectCmd::Text { .. } | InjectCmd::Resume => Ok(()),
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"invalid\"".to_string())
}
