// WP-comp-shell-ipc — shell-control socket accept/read loop.
//
// One thread accepts connections sequentially and handles each one to
// completion before calling `accept()` again — same "single dedicated
// listener thread" shape `codrive::listener` uses, but for a DIFFERENT
// reason: `codrive` serializes because it only ever wants ONE live agent
// session at a time (DESIGN §3.3.1). Here it's simply "this channel's
// call volume is human-triggered (a dock, polling every few seconds) with
// each call being a fast connect/request/response/close round trip (this
// file's own module doc) — a second concurrent caller just waits its turn
// in the kernel's listen backlog, and a per-connection thread would be
// unjustified complexity for that load. A stalled peer cannot wedge this
// thread indefinitely: `set_read_timeout` bounds the request-line read, and
// `recv_timeout` bounds the wait for the main thread's answer (below).
//
// Auth (task brief: "鑑別可比 agent 通道輕...但絕不可讓 agent 拿到這條
// socket"): checked BEFORE a single byte of the request is read, mirroring
// `duduclaw-sysd`'s own `handle_connection` ordering
// (`duduclaw-sysd/src/server.rs`) — SO_PEERCRED credentials come from the
// kernel, read off the socket itself, so there is nothing for a peer to
// spoof. `own_uid` is this process's OWN uid (`libc::getuid()`, computed
// once in `mod.rs::init` — see that module's doc for why this is simpler
// than `duduclaw-sysd`'s externally-configured `allowed_uid`: `duduclaw-
// comp`/`duduclaw-shell` always run as the SAME kiosk-session user, so
// "same uid as me" is the entire policy, no env var needed). A peer whose
// real uid differs — which, on the appliance, is exactly what an agent CLI
// subprocess looks like (`duduclaw-gateway.service` runs `User=duduclaw`;
// the kiosk session runs `User=duduclaw-kiosk` — two DIFFERENT system
// users, `appliance/postinst.d/20-users-and-units.sh`) — is denied without
// its bytes ever being parsed or reaching `self.space`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};

use smithay::reexports::calloop;

use super::protocol::{
    ShellControlRequest, ShellControlResponse, MAX_CURSOR_SOURCE_BYTES, MAX_OUTPUT_NAME_BYTES,
    MAX_QUERY_BYTES, MAX_REQUEST_LINE_BYTES, MAX_THEME_BYTES, OUTPUT_SCALE_STEPS,
};
use super::codrive_ops::{CodriveDriveAction, MAX_CODRIVE_ACTION_BYTES};
use super::{ShellControlMsg, ShellControlShared};
use crate::cursor::source::{cursor_size_from_wire, CursorSource};
use crate::decor::Theme;

/// Bounds how long `handle_connection` will block waiting for a request
/// line from an already-accepted (and already peer-cred-authorized) peer.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Bounds how long the socket thread will wait for the calloop main thread
/// to compute and send back an answer, so a stalled/overloaded main loop
/// degrades this connection to a timeout error instead of hanging the
/// listener thread forever (which would also starve every later caller,
/// since connections are handled sequentially — see this file's module
/// doc).
const MAIN_THREAD_REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub fn spawn(
    sock_path: PathBuf,
    own_uid: u32,
    shared: Arc<ShellControlShared>,
    tx: calloop::channel::Sender<ShellControlMsg>,
) -> std::io::Result<()> {
    // Stale socket file from a previous crashed run — same handling as
    // `codrive::listener::spawn`.
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path)?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;

    tracing::info!(
        path = %sock_path.display(),
        own_uid,
        "shell_control: shell-control socket listening (one-shot request/response, \
         same-uid SO_PEERCRED auth — see this module's doc comment)"
    );

    std::thread::Builder::new()
        .name("shell-control".into())
        .spawn(move || accept_loop(listener, own_uid, shared, tx))
        .map(|_handle| ())
}

fn accept_loop(listener: UnixListener, own_uid: u32, shared: Arc<ShellControlShared>, tx: calloop::channel::Sender<ShellControlMsg>) {
    loop {
        let (stream, _addr) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(error = %e, "shell_control: accept() failed — listener thread exiting");
                return;
            }
        };
        handle_connection(stream, own_uid, &shared, &tx);
    }
}

/// Reads the connecting peer's real uid straight from the kernel via
/// `getsockopt(SOL_SOCKET, SO_PEERCRED)` (Linux-specific — matches this
/// crate's existing "already Linux-only, `libc` already resolved
/// transitively" reasoning `codrive/mod.rs::generate_token_bytes` and
/// `codrive/rotation.rs`'s SIGHUP handling both give for their own raw
/// `libc` calls).
///
/// `std::os::unix::net::UnixStream::peer_cred()` looks like the obvious
/// std API for this and is what `duduclaw-sysd` calls — but that's tokio's
/// OWN `peer_cred()` (tokio implements `SO_PEERCRED` itself, independent of
/// std), not this one: as of this crate's pinned toolchain (`rustc
/// 1.97.1`), std's equivalent is still gated behind the unstable
/// `peer_credentials_unix_socket` feature (rust-lang/rust#42839) — confirmed
/// by actually trying it first and reading the resulting `E0658`, not
/// assumed. Hand-rolling the raw `getsockopt` call, exactly what tokio's
/// own implementation does under the hood, needs no nightly toolchain and
/// no new dependency.
fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    let fd = stream.as_raw_fd();
    // SAFETY: `fd` is a valid, open socket owned by `stream` for the
    // duration of this call (borrowed, not consumed); `cred` is a
    // correctly-sized, zero-initialized `libc::ucred` and `len` is set to
    // its exact size before the call, matching `getsockopt(2)`'s documented
    // contract. `SO_PEERCRED` is read-only and has no side effects on the
    // socket itself.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// True iff `peer_uid` (as reported by the kernel via `SO_PEERCRED`) equals
/// `own_uid` (this compositor process's own uid). Pure function — see
/// `mod.rs`'s tests for the "agent cannot reach this socket" proof: since a
/// real different-uid peer can't be spawned without root in a test, the
/// established convention in this codebase (`duduclaw-sysd::server`'s own
/// `mismatched_uid_is_rejected` test) is to vary the CONFIGURED side
/// instead — exactly what this function's own signature makes easy to test
/// directly, no socket needed.
pub(crate) fn is_authorized_peer(peer_uid: Option<u32>, own_uid: u32) -> bool {
    matches!(peer_uid, Some(p) if p == own_uid)
}

fn handle_connection(stream: UnixStream, own_uid: u32, shared: &Arc<ShellControlShared>, tx: &calloop::channel::Sender<ShellControlMsg>) {
    let peer_uid = match peer_uid(&stream) {
        Ok(uid) => Some(uid),
        Err(e) => {
            tracing::warn!(error = %e, "shell_control: could not read peer credentials — denying");
            None
        }
    };

    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "shell_control: could not clone the connection for writing — closing");
            return;
        }
    };

    if !is_authorized_peer(peer_uid, own_uid) {
        tracing::warn!(peer_uid = ?peer_uid, own_uid, "shell_control: rejected connection — caller uid is not this process's own uid");
        shared.record("auth_denied", Some(format!("peer_uid={peer_uid:?} own_uid={own_uid}")));
        let _ = write_response(&mut writer, &ShellControlResponse::err("unauthorized"));
        // Same "read error before auth" non-issue `codrive::listener`
        // accepts implicitly: an unauthorized peer's connection is simply
        // dropped here — its request bytes, if any were ever sent, are
        // never read at all (fail-closed: deny happens purely from kernel-
        // verified peer credentials, before this thread looks at a single
        // byte the peer wrote).
        return;
    }

    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return, // peer connected then closed without sending anything
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "shell_control: request read error/timeout — closing connection");
            let _ = write_response(&mut writer, &ShellControlResponse::err("read_error"));
            return;
        }
    }

    let trimmed = line.trim();
    if trimmed.len() > MAX_REQUEST_LINE_BYTES {
        let _ = write_response(&mut writer, &ShellControlResponse::err("line_too_long"));
        return;
    }

    let req: ShellControlRequest = match serde_json::from_str(trimmed) {
        Ok(r) => r,
        Err(_) => {
            let _ = write_response(&mut writer, &ShellControlResponse::err("parse_error"));
            return;
        }
    };

    if let Err(reason) = validate(&req) {
        let _ = write_response(&mut writer, &ShellControlResponse::err(reason));
        return;
    }

    tracing::debug!(op = req.op_name(), "shell_control: dispatching to the main thread");

    // Bridge to the calloop main thread (the only thread allowed to touch
    // `self.space`/`self.seat`) via a oneshot `std::sync::mpsc` reply
    // channel — see `mod.rs`'s module doc for why this round trip exists
    // (unlike `codrive`'s fire-and-forget `InjectCmd` channel, this
    // caller genuinely needs the real computed answer, not an
    // immediately-knowable ack).
    let (reply_tx, reply_rx) = mpsc::channel::<ShellControlResponse>();
    if tx.send(ShellControlMsg { req, reply_tx }).is_err() {
        tracing::error!("shell_control: request channel closed — compositor event loop gone");
        let _ = write_response(&mut writer, &ShellControlResponse::err("compositor_unavailable"));
        return;
    }

    match reply_rx.recv_timeout(MAIN_THREAD_REPLY_TIMEOUT) {
        Ok(resp) => {
            let _ = write_response(&mut writer, &resp);
        }
        Err(_) => {
            tracing::warn!("shell_control: main thread did not answer within the timeout");
            let _ = write_response(&mut writer, &ShellControlResponse::err("timeout"));
        }
    }
}

fn write_response(writer: &mut UnixStream, resp: &ShellControlResponse) -> std::io::Result<()> {
    let line = serde_json::to_string(resp).unwrap_or_else(|_| r#"{"ok":false,"error":"internal_serialize_error"}"#.to_string());
    writeln!(writer, "{line}")
}

/// Field-level validation, same "reject here so a malformed request never
/// even reaches the main-thread channel" reasoning as
/// `codrive::listener::validate`.
pub(super) fn validate(req: &ShellControlRequest) -> Result<(), String> {
    match req {
        ShellControlRequest::ListWindows => Ok(()),
        ShellControlRequest::FocusWindow { query } => {
            if query.is_empty() {
                return Err("focus_window query must not be empty".into());
            }
            if query.len() > MAX_QUERY_BYTES {
                return Err(format!("focus_window query exceeds {MAX_QUERY_BYTES} bytes"));
            }
            Ok(())
        }
        ShellControlRequest::GetCursorSource => Ok(()),
        ShellControlRequest::SetCursorSource { source } => {
            // Length first, so a pathological string is refused before it is
            // parsed at all — same ordering as `focus_window` above.
            if source.len() > MAX_CURSOR_SOURCE_BYTES {
                return Err(format!(
                    "set_cursor_source source exceeds {MAX_CURSOR_SOURCE_BYTES} bytes"
                ));
            }
            if CursorSource::parse_strict(source).is_none() {
                // The error string is a fixed token, NOT the caller's own
                // value echoed back: this line reaches a log and, through
                // the shell, potentially a UI, and reflecting arbitrary
                // caller-controlled text into either is a habit worth not
                // having. `op_name()` already tells a reader which op failed.
                return Err("invalid_cursor_source".into());
            }
            Ok(())
        }
        // CUR-3. No length pre-check to mirror `set_cursor_source`'s: the
        // field is already a number by the time serde is done, so there is no
        // pathological-string case to refuse before parsing — the
        // `MAX_REQUEST_LINE_BYTES` cap upstream already bounds the whole line.
        ShellControlRequest::SetCursorSize { size } => {
            if cursor_size_from_wire(*size).is_none() {
                // Fixed token, same no-echo reasoning as above. Note this is
                // a REFUSAL, not a clamp — see `cursor_size_from_wire`'s doc.
                return Err("invalid_cursor_size".into());
            }
            Ok(())
        }
        // WP-comp-shell-display: read-only, never touches `self.space`.
        ShellControlRequest::GetOutputs => Ok(()),
        // WP-comp-shell-display: this socket thread has no access to
        // `self.space`, so it cannot know which modes a real output
        // actually has — that full check (`protocol::mode_request_matches`)
        // runs on the main thread, in `mod.rs`'s handler. What CAN be
        // checked here without any state is the `output` field's shape
        // (same length discipline as `focus_window`'s `query`) and that
        // width/height/refresh_mhz are even *positive* — no real mode has
        // ever had a non-positive dimension or refresh rate, so this is a
        // real, unconditional rejection, not a partial guess.
        ShellControlRequest::SetOutputMode { output, width, height, refresh_mhz } => {
            if output.is_empty() {
                return Err("set_output_mode output must not be empty".into());
            }
            if output.len() > MAX_OUTPUT_NAME_BYTES {
                return Err(format!("set_output_mode output exceeds {MAX_OUTPUT_NAME_BYTES} bytes"));
            }
            if *width <= 0 || *height <= 0 || *refresh_mhz <= 0 {
                // Fixed token, same no-echo reasoning as `set_cursor_size`
                // above — a caller-controlled number never reaches a log
                // line or a UI string verbatim.
                return Err("invalid_mode".into());
            }
            Ok(())
        }
        ShellControlRequest::SetOutputScale { output, scale_pct } => {
            if output.is_empty() {
                return Err("set_output_scale output must not be empty".into());
            }
            if output.len() > MAX_OUTPUT_NAME_BYTES {
                return Err(format!("set_output_scale output exceeds {MAX_OUTPUT_NAME_BYTES} bytes"));
            }
            if !OUTPUT_SCALE_STEPS.contains(scale_pct) {
                // Fixed token, no echo — same discipline as `set_cursor_size`.
                // This IS the full check (the set is static, unlike a mode's
                // per-output set), so nothing further runs on the main thread
                // except a defensive re-check (see `mod.rs`'s handler).
                return Err("invalid_scale".into());
            }
            Ok(())
        }
        // D2. Length first (same ordering as `set_cursor_source`), then the
        // strict, closed-set parse — a value the socket thread can already
        // refuse without touching `self.space`.
        ShellControlRequest::SetTheme { theme } => {
            if theme.len() > MAX_THEME_BYTES {
                return Err(format!("set_theme theme exceeds {MAX_THEME_BYTES} bytes"));
            }
            if Theme::parse_strict(theme).is_none() {
                // Fixed token, same no-echo reasoning as every other op here.
                return Err("invalid_theme".into());
            }
            Ok(())
        }
        // A1: no params at all — same shape as `list_windows`/
        // `get_cursor_source`.
        ShellControlRequest::TakeShellIntents => Ok(()),
        // A2: read-only, no params — same shape as `codrive`'s own `status`
        // op, which is likewise answered without touching the seat.
        ShellControlRequest::CodriveStatus => Ok(()),
        // A2. Length first (so a pathological string is refused before it is
        // parsed at all), then the strict, closed-set parse — exactly
        // `set_cursor_source`'s ordering. `parse_strict` here is intentionally
        // stricter than the appearance ops': it does not trim or case-fold,
        // because this value is only ever sent by the shell's own button
        // handler from a fixed string, so a near-miss is a caller bug rather
        // than a person's typing.
        ShellControlRequest::CodriveDrive { action } => {
            if action.len() > MAX_CODRIVE_ACTION_BYTES {
                return Err(format!("codrive_drive action exceeds {MAX_CODRIVE_ACTION_BYTES} bytes"));
            }
            if CodriveDriveAction::parse_strict(action).is_none() {
                // Fixed token, no echo — same discipline as every other op
                // here: a caller-controlled string never reaches a log line
                // or a shell UI verbatim.
                return Err("invalid_codrive_action".into());
            }
            Ok(())
        }
        // D9-bug3/D9-bug4: the one field is a `bool`, so serde has already
        // done the entire validation this op admits of — anything that is not
        // `true`/`false` fails to parse and never reaches here. There is
        // deliberately no "is the caller allowed to unlock" check on this
        // socket: the boundary is `SO_PEERCRED` same-uid (see this module's
        // own doc), which is the same boundary that would let a caller kill
        // and restart the shell anyway.
        ShellControlRequest::SetSessionLocked { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn current_uid() -> u32 {
        // SAFETY: `getuid()` has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    fn temp_socket_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("duduclaw-shell-control-test-{tag}-{}.sock", std::process::id()))
    }

    // ── Pure validation ─────────────────────────────────────────────────

    #[test]
    fn validate_accepts_list_windows() {
        assert!(validate(&ShellControlRequest::ListWindows).is_ok());
    }

    #[test]
    fn validate_rejects_empty_query() {
        let req = ShellControlRequest::FocusWindow { query: String::new() };
        assert!(validate(&req).is_err());
    }

    #[test]
    fn validate_rejects_oversized_query() {
        let req = ShellControlRequest::FocusWindow { query: "x".repeat(MAX_QUERY_BYTES + 1) };
        assert!(validate(&req).is_err());
    }

    #[test]
    fn validate_accepts_reasonable_query() {
        let req = ShellControlRequest::FocusWindow { query: "foot-A".to_string() };
        assert!(validate(&req).is_ok());
    }

    // ── CUR-2 cursor ops ─────────────────────────────────────────────────

    #[test]
    fn validate_accepts_get_cursor_source() {
        assert!(validate(&ShellControlRequest::GetCursorSource).is_ok());
    }

    #[test]
    fn validate_accepts_the_three_legal_cursor_sources() {
        for v in ["system", "brand", "duduclaw", "  Brand  ", "SYSTEM"] {
            let req = ShellControlRequest::SetCursorSource { source: v.to_string() };
            assert!(validate(&req).is_ok(), "{v:?} should be accepted");
        }
    }

    #[test]
    fn validate_rejects_an_unknown_cursor_source_instead_of_coercing_it() {
        // The env var folds a typo into `system`; a control-channel op must
        // not — see `CursorSource::parse_strict`'s doc comment.
        for v in ["", "   ", "brnad", "claw", "1", "🐾"] {
            let req = ShellControlRequest::SetCursorSource { source: v.to_string() };
            assert_eq!(
                validate(&req).unwrap_err(),
                "invalid_cursor_source",
                "{v:?} should be refused"
            );
        }
    }

    #[test]
    fn validate_rejects_an_oversized_cursor_source() {
        let req = ShellControlRequest::SetCursorSource {
            source: "b".repeat(MAX_CURSOR_SOURCE_BYTES + 1),
        };
        let err = validate(&req).unwrap_err();
        assert!(err.contains(&MAX_CURSOR_SOURCE_BYTES.to_string()), "{err}");
    }

    // ── CUR-3 cursor size ────────────────────────────────────────────────

    #[test]
    fn validate_accepts_exactly_the_five_offered_sizes() {
        for n in crate::cursor::source::CURSOR_SIZE_STEPS {
            let req = ShellControlRequest::SetCursorSize { size: n as i64 };
            assert!(validate(&req).is_ok(), "{n} should be accepted");
        }
    }

    #[test]
    fn validate_refuses_an_off_step_size_instead_of_clamping_it() {
        // The op is the UI's channel, so an unrepresentable value is an error
        // the caller can show — never a silently substituted neighbour. The
        // operator escape hatch for an arbitrary size is XCURSOR_SIZE, which
        // this op deliberately does not duplicate.
        for bad in [-1_i64, 0, 1, 8, 25, 40, 63, 100, 512, 100_000, i64::MAX, i64::MIN] {
            let req = ShellControlRequest::SetCursorSize { size: bad };
            assert_eq!(
                validate(&req).unwrap_err(),
                "invalid_cursor_size",
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn an_invalid_cursor_size_error_does_not_echo_the_callers_value() {
        // Same no-echo discipline as the source op: the token is fixed, so a
        // caller-controlled number never reaches a log line or a UI string.
        let err = validate(&ShellControlRequest::SetCursorSize { size: 133_713_371 }).unwrap_err();
        assert_eq!(err, "invalid_cursor_size");
        assert!(!err.contains("1337"));
    }

    #[test]
    fn an_invalid_cursor_source_error_does_not_echo_the_callers_value() {
        let req = ShellControlRequest::SetCursorSource {
            source: "<script>alert(1)</script>".to_string(),
        };
        let err = validate(&req).unwrap_err();
        assert_eq!(err, "invalid_cursor_source");
        assert!(!err.contains("script"));
    }

    // ── WP-comp-shell-display: get_outputs / set_output_mode / set_output_scale ──

    #[test]
    fn validate_accepts_get_outputs() {
        assert!(validate(&ShellControlRequest::GetOutputs).is_ok());
    }

    #[test]
    fn validate_accepts_a_reasonable_set_output_mode_request() {
        let req = ShellControlRequest::SetOutputMode {
            output: "Virtual-1".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
        };
        assert!(validate(&req).is_ok());
    }

    #[test]
    fn validate_rejects_an_empty_output_name() {
        let req = ShellControlRequest::SetOutputMode {
            output: String::new(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
        };
        assert!(validate(&req).is_err());
        let req = ShellControlRequest::SetOutputScale { output: String::new(), scale_pct: 100 };
        assert!(validate(&req).is_err());
    }

    #[test]
    fn validate_rejects_an_oversized_output_name() {
        let req = ShellControlRequest::SetOutputMode {
            output: "x".repeat(MAX_OUTPUT_NAME_BYTES + 1),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
        };
        let err = validate(&req).unwrap_err();
        assert!(err.contains(&MAX_OUTPUT_NAME_BYTES.to_string()), "{err}");
        let req = ShellControlRequest::SetOutputScale {
            output: "x".repeat(MAX_OUTPUT_NAME_BYTES + 1),
            scale_pct: 100,
        };
        let err = validate(&req).unwrap_err();
        assert!(err.contains(&MAX_OUTPUT_NAME_BYTES.to_string()), "{err}");
    }

    #[test]
    fn validate_rejects_non_positive_mode_dimensions_or_refresh() {
        for (w, h, r) in [(0, 1080, 60000), (-1, 1080, 60000), (1920, 0, 60000), (1920, -1, 60000), (1920, 1080, 0), (1920, 1080, -1)] {
            let req = ShellControlRequest::SetOutputMode {
                output: "Virtual-1".to_string(),
                width: w,
                height: h,
                refresh_mhz: r,
            };
            assert_eq!(validate(&req).unwrap_err(), "invalid_mode", "(w={w}, h={h}, r={r}) should be refused");
        }
    }

    #[test]
    fn validate_accepts_an_implausibly_large_but_positive_mode_without_knowing_if_it_is_real() {
        // The socket thread has no access to `self.space`, so it cannot know
        // whether a specific (width, height, refresh_mhz) triple is a REAL
        // mode of a REAL output — only the main thread's
        // `protocol::mode_request_matches` can answer that. This is the
        // deliberate split, not an oversight: a huge-but-positive value must
        // still reach the main thread to be told `invalid_mode` there.
        let req = ShellControlRequest::SetOutputMode {
            output: "Virtual-1".to_string(),
            width: 999_999_999,
            height: 999_999_999,
            refresh_mhz: 999_999_999,
        };
        assert!(validate(&req).is_ok());
    }

    #[test]
    fn an_invalid_mode_error_does_not_echo_the_callers_values() {
        let req = ShellControlRequest::SetOutputMode {
            output: "Virtual-1".to_string(),
            width: -133713,
            height: 1080,
            refresh_mhz: 60000,
        };
        let err = validate(&req).unwrap_err();
        assert_eq!(err, "invalid_mode");
        assert!(!err.contains("1337"));
    }

    #[test]
    fn validate_accepts_exactly_the_five_offered_output_scales() {
        for pct in OUTPUT_SCALE_STEPS {
            let req = ShellControlRequest::SetOutputScale { output: "Virtual-1".to_string(), scale_pct: pct };
            assert!(validate(&req).is_ok(), "{pct} should be accepted");
        }
    }

    #[test]
    fn validate_refuses_an_off_step_output_scale_instead_of_clamping_it() {
        for bad in [-1_i64, 0, 1, 50, 99, 101, 110, 199, 201, 300, 1000, i64::MAX, i64::MIN] {
            let req = ShellControlRequest::SetOutputScale { output: "Virtual-1".to_string(), scale_pct: bad };
            assert_eq!(validate(&req).unwrap_err(), "invalid_scale", "{bad} should be refused");
        }
    }

    #[test]
    fn an_invalid_output_scale_error_does_not_echo_the_callers_value() {
        let req = ShellControlRequest::SetOutputScale { output: "Virtual-1".to_string(), scale_pct: 133_713_371 };
        let err = validate(&req).unwrap_err();
        assert_eq!(err, "invalid_scale");
        assert!(!err.contains("1337"));
    }

    // ── D2 set_theme ─────────────────────────────────────────────────────

    #[test]
    fn validate_accepts_the_two_legal_themes() {
        for v in ["light", "dark", "LIGHT", "Dark", "  dark  "] {
            let req = ShellControlRequest::SetTheme { theme: v.to_string() };
            assert!(validate(&req).is_ok(), "{v:?} should be accepted");
        }
    }

    #[test]
    fn validate_rejects_an_unknown_theme_instead_of_defaulting_to_either_side() {
        for v in ["", "   ", "blue", "1", "auto", "system", "🐾", "lightdark"] {
            let req = ShellControlRequest::SetTheme { theme: v.to_string() };
            assert_eq!(validate(&req).unwrap_err(), "invalid_theme", "{v:?} should be refused");
        }
    }

    #[test]
    fn validate_rejects_an_oversized_theme() {
        let req = ShellControlRequest::SetTheme { theme: "d".repeat(MAX_THEME_BYTES + 1) };
        let err = validate(&req).unwrap_err();
        assert!(err.contains(&MAX_THEME_BYTES.to_string()), "{err}");
    }

    #[test]
    fn an_invalid_theme_error_does_not_echo_the_callers_value() {
        // Short enough to clear MAX_THEME_BYTES (16) and reach the parser —
        // this is testing the no-echo discipline of `parse_strict`'s
        // rejection, not the separate oversized-length rejection above.
        let req = ShellControlRequest::SetTheme { theme: "<script>".to_string() };
        let err = validate(&req).unwrap_err();
        assert_eq!(err, "invalid_theme");
        assert!(!err.contains("script"));
    }

    // ── A1 take_shell_intents ───────────────────────────────────────────

    #[test]
    fn validate_accepts_take_shell_intents() {
        assert!(validate(&ShellControlRequest::TakeShellIntents).is_ok());
    }

    // A2's `codrive_status`/`codrive_drive` validate tests live in
    // `shell_control/codrive_ops.rs` alongside that round's action parser and
    // wire-shape tests — one file per A2 concern, and this one was already
    // past the 800-line cap before A2 touched it.

    // ── Pure auth predicate — the "agent cannot reach this socket" proof ──
    // (see this file's own `is_authorized_peer` doc comment for why the
    // CONFIGURED side, not the real peer, is varied here — same strategy
    // `duduclaw-sysd::server::tests::mismatched_uid_is_rejected` already
    // establishes as this codebase's accepted way to test this property
    // without root.)

    #[test]
    fn is_authorized_peer_accepts_matching_uid() {
        assert!(is_authorized_peer(Some(1000), 1000));
    }

    #[test]
    fn is_authorized_peer_rejects_a_different_uid() {
        // Stands in for an agent CLI subprocess: on the appliance, agents
        // run as `duduclaw` (uid 1000, `duduclaw-gateway.service`) while
        // this compositor and its shell client both run as `duduclaw-kiosk`
        // (a DIFFERENT system user — `appliance/postinst.d/
        // 20-users-and-units.sh`) — so a same-uid check is a REAL boundary
        // between the two, not just a naming convention.
        assert!(!is_authorized_peer(Some(1000), 1001));
    }

    #[test]
    fn is_authorized_peer_rejects_unreadable_peer_credentials() {
        // `peer_cred()` failing (e.g. an exotic platform) must fail closed,
        // never fail open.
        assert!(!is_authorized_peer(None, 1000));
    }

    // ── Socket-level round trip (transport only — business logic is
    // proven separately: `codrive::window_target`'s pure `match_window_query`
    // tests cover the matching POLICY this reuses, and BUILD.md's live
    // container run covers a REAL `self.space`/`self.seat`, which this
    // crate has never constructed in a unit test — see `window_target.rs`'s
    // own module doc for that established precedent). These tests stand a
    // fake "main thread" in for the real calloop-driven `DuduclawComp` main
    // loop, proving the listener correctly bridges request -> channel ->
    // reply -> response for an AUTHORIZED peer, and that an UNAUTHORIZED
    // peer's bytes never reach that bridge at all. ──

    /// Drains one `ShellControlMsg` off `rx` on a background thread and
    /// replies with `canned`, counting how many messages it actually saw —
    /// the counter is what proves an unauthorized connection's request
    /// never reached "the main thread" at all, not just that its response
    /// was an error.
    fn spawn_fake_main_thread(
        rx: calloop::channel::Channel<ShellControlMsg>,
        canned: ShellControlResponse,
    ) -> (std::sync::Arc<AtomicUsize>, std::sync::mpsc::Sender<()>) {
        let seen = std::sync::Arc::new(AtomicUsize::new(0));
        let seen_clone = std::sync::Arc::clone(&seen);
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

        std::thread::spawn(move || {
            let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(rx, move |event, _, _| {
                    if let calloop::channel::Event::Msg(msg) = event {
                        seen_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = msg.reply_tx.send(canned.clone());
                    }
                })
                .unwrap();
            loop {
                if stop_rx.try_recv().is_ok() {
                    return;
                }
                let _ = event_loop.dispatch(Some(Duration::from_millis(50)), &mut ());
            }
        });

        (seen, stop_tx)
    }

    #[test]
    fn authorized_same_uid_connection_round_trips_to_the_main_thread_and_back() {
        let sock_path = temp_socket_path("authok");
        let _ = std::fs::remove_file(&sock_path);

        let audit_path = std::env::temp_dir().join(format!("duduclaw-shell-control-test-authok-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&audit_path);
        let shared = Arc::new(ShellControlShared::for_test());

        let (tx, rx) = calloop::channel::channel::<ShellControlMsg>();
        let canned = ShellControlResponse::focused_by_app_id("foot-A".to_string());
        let (seen, stop_tx) = spawn_fake_main_thread(rx, canned);

        spawn(sock_path.clone(), current_uid(), Arc::clone(&shared), tx).expect("listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        writeln!(writer, r#"{{"op":"focus_window","params":{{"query":"foot-A"}}}}"#).unwrap();

        let mut reply = String::new();
        BufReader::new(&conn).read_line(&mut reply).expect("no response from listener");
        assert!(reply.contains(r#""matched_app_id":"foot-A""#), "unexpected response: {reply}");
        assert_eq!(seen.load(Ordering::SeqCst), 1, "the request must have reached the main-thread stand-in exactly once");

        let _ = stop_tx.send(());
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn mismatched_configured_uid_denies_connection_before_it_ever_reaches_the_main_thread() {
        let sock_path = temp_socket_path("authbad");
        let _ = std::fs::remove_file(&sock_path);
        let shared = Arc::new(ShellControlShared::for_test());

        let (tx, rx) = calloop::channel::channel::<ShellControlMsg>();
        // A canned reply the stand-in would send IF it ever saw a message —
        // the assertion below is that it never does.
        let canned = ShellControlResponse::windows(vec![]);
        let (seen, stop_tx) = spawn_fake_main_thread(rx, canned);

        // The listener believes its OWN uid is `current_uid() + 1` — i.e.
        // this test process (whatever its real uid is) is treated exactly
        // like a different-uid caller would be, same test strategy this
        // file's own `is_authorized_peer_rejects_a_different_uid` and
        // `duduclaw-sysd::server::tests::mismatched_uid_is_rejected` use.
        let wrong_own_uid = current_uid().wrapping_add(1);
        spawn(sock_path.clone(), wrong_own_uid, Arc::clone(&shared), tx).expect("listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        // Deliberately NOT `.unwrap()`: this is the REJECTION path, so the
        // listener is racing us to close the connection the moment it reads
        // the peer's credentials — it owes an unauthorized caller nothing and
        // does not wait for the request line. When it wins that race our write
        // lands on a closed socket and returns EPIPE, which made this test
        // flake at roughly 1-in-15 (measured over a 15-run loop, 2026-08-22).
        // Whether the write itself succeeds is not what is under test; the
        // assertions below are, and both hold either way — the listener still
        // answers `ok:false`/`unauthorized` and still never reaches the main
        // thread. Swallowing the error here removes the flake without weakening
        // a single assertion.
        let _ = writeln!(writer, r#"{{"op":"list_windows"}}"#);

        let mut reply = String::new();
        BufReader::new(&conn).read_line(&mut reply).expect("no response from listener");
        assert!(reply.contains(r#""ok":false"#), "unexpected response: {reply}");
        assert!(reply.contains("unauthorized"), "unexpected response: {reply}");
        assert_eq!(seen.load(Ordering::SeqCst), 0, "an unauthorized peer's request must never reach the main thread");

        let _ = stop_tx.send(());
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn oversized_request_line_is_rejected_without_reaching_the_main_thread() {
        let sock_path = temp_socket_path("oversized");
        let _ = std::fs::remove_file(&sock_path);
        let shared = Arc::new(ShellControlShared::for_test());

        let (tx, rx) = calloop::channel::channel::<ShellControlMsg>();
        let canned = ShellControlResponse::windows(vec![]);
        let (seen, stop_tx) = spawn_fake_main_thread(rx, canned);

        spawn(sock_path.clone(), current_uid(), Arc::clone(&shared), tx).expect("listener failed to bind");

        let conn = UnixStream::connect(&sock_path).expect("client failed to connect");
        let mut writer = conn.try_clone().unwrap();
        let oversized_query = "x".repeat(MAX_QUERY_BYTES + 500);
        writeln!(writer, r#"{{"op":"focus_window","params":{{"query":"{oversized_query}"}}}}"#).unwrap();

        let mut reply = String::new();
        BufReader::new(&conn).read_line(&mut reply).expect("no response from listener");
        assert!(reply.contains(r#""ok":false"#), "unexpected response: {reply}");
        assert_eq!(seen.load(Ordering::SeqCst), 0);

        let _ = stop_tx.send(());
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn socket_file_is_created_mode_0600() {
        let sock_path = temp_socket_path("perm");
        let _ = std::fs::remove_file(&sock_path);
        let shared = Arc::new(ShellControlShared::for_test());
        let (tx, _rx) = calloop::channel::channel::<ShellControlMsg>();
        spawn(sock_path.clone(), current_uid(), shared, tx).expect("listener failed to bind");
        // Give the listener thread a moment to bind before checking.
        std::thread::sleep(Duration::from_millis(100));
        let mode = std::fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&sock_path);
    }
}
