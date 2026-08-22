//! Blocking Unix-socket client for `duduclaw-comp`'s shell-control socket
//! (WP-comp-shell-ipc, 2026-08-22) — the dock's window list/switch IPC.
//!
//! Wire contract mirrored here BY HAND (comp side: `duduclaw-comp/src/
//! shell_control/protocol.rs`) — this crate cannot depend on
//! `duduclaw-comp` (Linux-only, detached workspace, own `Cargo.lock` — see
//! that crate's own `Cargo.toml` header comment), same reasoning
//! `duduclaw-gateway/src/codrive/client.rs`'s own header comment gives for
//! hand-mirroring `codrive`'s wire types instead of sharing a crate. One
//! request per connection (connect → one JSON line out → one JSON line in
//! → close) — see comp's own `shell_control/protocol.rs` module doc for why
//! there is no persistent session here, unlike `codrive`'s own client.
//!
//! Every function here is a PLAIN BLOCKING call — same "callers run it from
//! a `std::thread::spawn` and bridge the result back to gpui via
//! `std::sync::mpsc` + a `cx.spawn` poll loop" contract `gateway_client`'s
//! own module doc establishes (`home/running_windows.rs` is this module's
//! one caller, following that exact pattern).
//!
//! Not `#[cfg(target_os = "linux")]`-gated: `std::os::unix::net::UnixStream`
//! also exists on macOS (this crate's own dev-iteration platform — see
//! `BUILD-LINUX.md`'s header comment on why Docker is needed for the
//! LINUX-specific `gpui_linux` backend, a SEPARATE concern from this plain
//! socket client), so this compiles unconditionally on both. On a dev Mac
//! there is never a real `duduclaw-comp` process — `socket_path()`/`call()`
//! degrade to the ordinary `NotAvailable`/`Io` error paths below, exactly
//! the same "comp isn't running" case a Linux box without the compositor up
//! would also hit; no platform-specific branch is needed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Same value and file name as comp's own `shell_control::protocol::
/// SOCKET_FILE_NAME` — kept as a literal here (not imported — see this
/// file's own module doc on why nothing is shared across the two crates).
const SOCKET_FILE_NAME: &str = "duduclaw-shell.sock";

/// Bounds both the connect-and-request-write phase and the response read —
/// this is a local, same-host round trip (comp's own `MAIN_THREAD_REPLY_
/// TIMEOUT`/`REQUEST_READ_TIMEOUT` are both 3s), so a dock caller waiting
/// this long already means something is badly wrong on the comp side, not
/// ordinary load.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// `$XDG_RUNTIME_DIR/duduclaw-shell.sock`. `None` when `XDG_RUNTIME_DIR`
/// isn't set at all (e.g. a plain `cargo test` invocation, or a dev Mac
/// shell run outside any session manager) — a real kiosk session always
/// has it set (`duduclaw-kiosk.service`'s own `Environment=XDG_RUNTIME_
/// DIR=...`, `appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.
/// service`).
pub fn socket_path() -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime_dir).join(SOCKET_FILE_NAME))
}

/// One `list_windows` row — mirrors comp's own `shell_control::protocol::
/// ShellWindowInfo` field-for-field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompWindow {
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub focused: bool,
}

/// Every failure this client can produce. Deliberately does NOT try to
/// distinguish "comp isn't running" from "comp is running but refused the
/// call" beyond `NotAvailable` vs. everything else — `home/running_
/// windows.rs`'s `RunningWindowsFeed` collapses all of these to the same
/// "offline" presentation either way (same pattern `gateway_client::
/// GatewayError`'s own doc comment documents and justifies for the
/// Notifications feed).
#[derive(Debug, Clone)]
pub enum CompClientError {
    /// No socket to even try connecting to (`XDG_RUNTIME_DIR` unset, or the
    /// socket file doesn't exist yet — comp not running, or running an old
    /// build without this channel).
    NotAvailable(String),
    /// A real I/O failure during connect/write/read.
    Io(String),
    /// Connected and sent a request, but no response line arrived within
    /// `CALL_TIMEOUT`.
    Timeout,
    /// The response line wasn't valid JSON in the expected shape.
    Protocol(String),
    /// comp answered with a structured `{"ok":false,"error":"..."}` — the
    /// call reached comp and comp explicitly declined it (e.g.
    /// `focus_window` found no match: `"not_found"`).
    Comp(String),
}

impl std::fmt::Display for CompClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompClientError::NotAvailable(s) => write!(f, "shell-control socket not available: {s}"),
            CompClientError::Io(s) => write!(f, "shell-control I/O error: {s}"),
            CompClientError::Timeout => write!(f, "shell-control call timed out"),
            CompClientError::Protocol(s) => write!(f, "shell-control protocol error: {s}"),
            CompClientError::Comp(s) => write!(f, "shell-control rejected: {s}"),
        }
    }
}

/// Permissive ack struct — every field but `ok` is `Option`, same
/// "shape varies by op" convention `duduclaw-gateway/src/codrive/
/// client.rs::CodriveAck`'s own doc comment establishes for the sibling
/// codrive client.
#[derive(Debug, Clone, Default, Deserialize)]
struct CompResponse {
    ok: bool,
    #[serde(default)]
    windows: Option<Vec<CompWindow>>,
    #[serde(default)]
    matched_app_id: Option<String>,
    #[serde(default)]
    matched_title_prefix: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// One blocking connect → write one line → read one line → close round
/// trip. `req_line` is a complete, already-serialized JSON request WITHOUT
/// a trailing newline (this fn adds it) — callers build it with
/// `serde_json::json!` rather than a hand-formatted string, so a query
/// value containing `"` or `\` round-trips correctly (unlike comp's own
/// `codrive/mod.rs` debug tooling, which gets away with raw string
/// literals only because none of ITS values are caller-supplied free text).
fn call(req_line: &str) -> Result<CompResponse, CompClientError> {
    let Some(path) = socket_path() else {
        return Err(CompClientError::NotAvailable("XDG_RUNTIME_DIR is not set".to_string()));
    };
    if !path.exists() {
        return Err(CompClientError::NotAvailable(format!("no shell-control socket at {}", path.display())));
    }

    let mut stream = UnixStream::connect(&path).map_err(|e| CompClientError::Io(e.to_string()))?;
    let _ = stream.set_read_timeout(Some(CALL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CALL_TIMEOUT));

    if let Err(e) = writeln!(stream, "{req_line}") {
        return Err(classify_io_error(e));
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return Err(CompClientError::Protocol("connection closed with no response".to_string())),
        Ok(_) => {}
        Err(e) => return Err(classify_io_error(e)),
    }

    serde_json::from_str::<CompResponse>(line.trim()).map_err(|e| CompClientError::Protocol(e.to_string()))
}

fn classify_io_error(e: std::io::Error) -> CompClientError {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => CompClientError::Timeout,
        _ => CompClientError::Io(e.to_string()),
    }
}

/// `{"op":"list_windows"}` — every currently mapped toplevel comp knows
/// about. Blocking; see this file's own module doc for the threading
/// contract.
pub fn list_windows() -> Result<Vec<CompWindow>, CompClientError> {
    let resp = call(r#"{"op":"list_windows"}"#)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.windows.unwrap_or_default())
}

/// Outcome of a successful `focus_window` call — which criterion comp
/// matched on, mirroring `codrive::window_target::WindowMatch`'s own two
/// variants (never both, never neither — see comp's own `protocol.rs`
/// doc). Not currently rendered anywhere (the dock only needs pass/fail),
/// but kept typed rather than discarded so a future caller (a debug
/// overlay, say) doesn't have to re-parse the raw response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusMatch {
    AppId(String),
    TitlePrefix(String),
}

/// `{"op":"focus_window","params":{"query":"..."}}` — raises/focuses the
/// matching toplevel on the compositor's HUMAN seat (comp's own `shell_
/// control` module doc explains why, and why this is a DIFFERENT socket
/// from the agent's `codrive` channel). A query matching nothing comes back
/// as `Err(CompClientError::Comp("not_found"))`, never a silent success.
pub fn focus_window(query: &str) -> Result<FocusMatch, CompClientError> {
    let req = serde_json::json!({ "op": "focus_window", "params": { "query": query } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    match (resp.matched_app_id, resp.matched_title_prefix) {
        (Some(id), _) => Ok(FocusMatch::AppId(id)),
        (None, Some(title)) => Ok(FocusMatch::TitlePrefix(title)),
        (None, None) => Err(CompClientError::Protocol("ok response carried neither matched_app_id nor matched_title_prefix".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_none_when_xdg_runtime_dir_unset() {
        // SAFETY: test-only env mutation, same best-effort caveat this
        // codebase's other env-mutating tests already accept (e.g.
        // `duduclaw-sysd::protocol::tests::resolve_socket_path_prefers_env_
        // override`).
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        assert_eq!(socket_path(), None);
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("XDG_RUNTIME_DIR", v);
            }
        }
    }

    #[test]
    fn socket_path_joins_the_fixed_file_name_under_xdg_runtime_dir() {
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp/some-runtime-dir");
        }
        assert_eq!(socket_path(), Some(PathBuf::from("/tmp/some-runtime-dir/duduclaw-shell.sock")));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn list_windows_against_a_missing_socket_is_not_available_not_a_panic() {
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let dir = std::env::temp_dir().join(format!("duduclaw-shell-comp-client-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
        let err = list_windows().expect_err("no comp process is listening in this test dir");
        assert!(matches!(err, CompClientError::NotAvailable(_)), "unexpected error variant: {err:?}");
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compresponse_deserializes_windows_shape() {
        let json = r#"{"ok":true,"windows":[{"app_id":"foot-A","title":"foot","focused":true}]}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        let windows = resp.windows.unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].app_id.as_deref(), Some("foot-A"));
        assert!(windows[0].focused);
    }

    #[test]
    fn compresponse_deserializes_err_shape() {
        let json = r#"{"ok":false,"error":"not_found"}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("not_found"));
        assert!(resp.windows.is_none());
    }

    #[test]
    fn focus_window_request_is_well_formed_json() {
        // Not a wire round-trip (no live comp) — just confirms the request
        // builder produces valid, correctly-shaped JSON (adjacent tagging,
        // matching comp's own `ShellControlRequest` shape) rather than a
        // hand-formatted string that could break on a query containing `"`.
        let req = serde_json::json!({ "op": "focus_window", "params": { "query": "a \"quoted\" id" } });
        let s = req.to_string();
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["op"], "focus_window");
        assert_eq!(back["params"]["query"], "a \"quoted\" id");
    }

    /// Live-fire, `#[ignore]`d — same "never run by a bare `cargo test`"
    /// contract `oobe::claim`/`gateway_client`'s own live tests establish.
    /// Run against a REAL `duduclaw-comp` (this file's own module doc's
    /// "one-shot" contract means any comp instance works, no special
    /// fixture needed) with `XDG_RUNTIME_DIR` pointed at that comp's own
    /// runtime dir:
    ///   `XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
    ///    live_list_windows_against_real_comp --nocapture`
    #[test]
    #[ignore = "requires a live duduclaw-comp with its shell-control socket up — see doc comment"]
    fn live_list_windows_against_real_comp() {
        match list_windows() {
            Ok(windows) => eprintln!("[live] {} window(s): {windows:?}", windows.len()),
            Err(e) => panic!("list_windows failed against a supposedly-live comp: {e}"),
        }
    }

    /// Same live-fire contract as `live_list_windows_against_real_comp`
    /// above. Expects at least one window whose app_id is `foot-A` to be
    /// mapped in the live comp instance (this round's own container
    /// verification launches exactly that — see `BUILD.md`'s
    /// "WP-comp-shell-ipc" section for the reproducible command):
    ///   `XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
    ///    live_focus_window_against_real_comp --nocapture`
    #[test]
    #[ignore = "requires a live duduclaw-comp with a foot-A window mapped — see doc comment"]
    fn live_focus_window_against_real_comp() {
        match focus_window("foot-A") {
            Ok(m) => eprintln!("[live] focus_window(\"foot-A\") matched: {m:?}"),
            Err(e) => panic!("focus_window(\"foot-A\") failed against a supposedly-live comp: {e}"),
        }
        // A query that should never exist proves the honest not_found path
        // is reachable through THIS client too, not just comp's own side.
        match focus_window("definitely-does-not-exist-xyz") {
            Err(CompClientError::Comp(e)) => {
                assert_eq!(e, "not_found");
                eprintln!("[live] focus_window on a bogus query correctly returned Comp(\"not_found\")");
            }
            other => panic!("expected Comp(\"not_found\") for a bogus query, got {other:?}"),
        }
    }
}
