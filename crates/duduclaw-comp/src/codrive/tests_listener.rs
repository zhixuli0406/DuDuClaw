//! `listener.rs`'s own socket-thread integration tests, split out here in
//! the WP-CD4a-COMP round — `listener.rs` was sitting at 812 lines after
//! this round's `validate()` arm for `activate_window` (protocol.rs's new
//! `InjectCmd::ActivateWindow` variant), over this project's 800-line
//! per-file cap (task brief: "若必須超 800 先拆 listener 的測試到
//! tests_listener.rs"). Every test below moved verbatim from `listener.rs`'s
//! former `#[cfg(test)] mod tests` block — same "new scenarios get their own
//! `tests_<topic>.rs`" pattern `tests_takeover.rs` already established for
//! CD-3 (see that file's own module doc); this file is the same split
//! applied to `listener.rs`'s pre-existing tests instead of new ones. Uses
//! `super::listener::{spawn, validate}` exactly like `tests_takeover.rs`
//! does — both are visible here because this module, like `listener`, is a
//! direct child of `codrive` (Rust's "visible within the defining module's
//! whole subtree" privacy rule, not a special-case export).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use smithay::reexports::calloop;

use super::listener::{spawn, validate};
use super::protocol::InjectCmd;
use super::CodriveShared;

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

/// A2's half of the same red line: `session_active` is what the driving-mode
/// state machine derives `codrive` from (`codrive/mode.rs`), so an
/// unauthenticated connection setting it would let anything that can open the
/// socket claim the compositor is under agent control — an amber
/// "AI 駕駛中" frame around a screen no agent is driving. Same shape as
/// `unauthenticated_connection_does_not_clear_terminated` above.
#[test]
fn unauthenticated_connection_does_not_set_session_active() {
    let sock_path = std::env::temp_dir()
        .join(format!("duduclaw-codrive-test-badauth-session-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("expected-token".to_string())));
    assert!(!shared.session_active.load(Ordering::SeqCst), "precondition");

    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    writeln!(writer, r#"{{"op":"auth","token":"definitely-wrong"}}"#).unwrap();

    let mut reply = String::new();
    BufReader::new(&conn).read_line(&mut reply).expect("no auth response from listener");
    assert!(reply.contains("auth_failed"), "unexpected auth response: {reply}");

    assert!(
        !shared.session_active.load(Ordering::SeqCst),
        "an unauthenticated connection must never publish a live co-drive session \
         (A2 contract §2)"
    );
    // And the derived mode must still read `human` — the property the flag
    // exists to support, asserted directly rather than only via the flag.
    assert_eq!(
        super::mode::status_snapshot(&shared).mode,
        super::mode::DrivingMode::Human
    );

    let _ = std::fs::remove_file(&sock_path);
}

/// A2: the positive half — a connection that DOES authenticate publishes the
/// session, and the status op it can then send reports `codrive`.
#[test]
fn authenticated_connection_publishes_the_session_and_status_reports_codrive() {
    let sock_path = std::env::temp_dir()
        .join(format!("duduclaw-codrive-test-session-active-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("right-token".to_string())));
    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut reader = BufReader::new(&conn);

    writeln!(writer, r#"{{"op":"auth","token":"right-token"}}"#).unwrap();
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("no auth response from listener");
    assert!(reply.contains(r#""authenticated":true"#), "unexpected auth response: {reply}");

    writeln!(writer, r#"{{"op":"status"}}"#).unwrap();
    let mut status = String::new();
    reader.read_line(&mut status).expect("no status response from listener");
    // The pre-A2 prefix, byte-for-byte — every already-shipped caller reads it.
    assert!(
        status.starts_with(r#"{"ok":true,"frozen":false,"terminated":false,"takeover":false,"#),
        "unexpected status: {status}"
    );
    assert!(status.contains(r#""mode":"codrive""#), "unexpected status: {status}");
    assert!(status.contains(r#""handover_reason":null"#), "unexpected status: {status}");
    assert!(status.contains(r#""shadow":false"#), "unexpected status: {status}");
    assert!(status.contains(r#""watch_active":false"#), "unexpected status: {status}");
    assert!(status.contains(r#""watch_paused":false"#), "unexpected status: {status}");
    assert!(shared.session_active.load(Ordering::SeqCst));

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

/// The load-bearing CD-2 regression test (task brief item 1): rotating
/// the token over the socket must (a) make the OLD token immediately
/// unusable for a brand-new connection, (b) leave the connection that
/// requested the rotation completely unaffected — proven here by
/// sending a normal `status` op on it right after — and (c) hand out a
/// token a fresh connection can actually authenticate with.
#[test]
fn rotate_token_over_socket_invalidates_old_token_without_dropping_the_caller() {
    let sock_path =
        std::env::temp_dir().join(format!("duduclaw-codrive-test-rotate-{}.sock", std::process::id()));
    let token_path =
        std::env::temp_dir().join(format!("duduclaw-codrive-test-rotate-{}.token", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&token_path);

    let shared = Arc::new(CodriveShared::for_test_with_token_path(Some("old-token".to_string()), token_path.clone()));
    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    // Connection A authenticates with the original token, then asks
    // for a rotation.
    let conn_a = UnixStream::connect(&sock_path).expect("conn A failed to connect");
    let mut writer_a = conn_a.try_clone().unwrap();
    let mut br_a = BufReader::new(&conn_a);
    writeln!(writer_a, r#"{{"op":"auth","token":"old-token"}}"#).unwrap();
    let mut reply = String::new();
    br_a.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""authenticated":true"#));

    writeln!(writer_a, r#"{{"op":"rotate_token"}}"#).unwrap();
    reply.clear();
    br_a.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""ok":true"#) && reply.contains("rotated"), "unexpected rotate response: {reply}");

    // (b) Connection A is still alive and unauthenticated-gate-free —
    // an ordinary post-auth op still works on it.
    writeln!(writer_a, r#"{{"op":"status"}}"#).unwrap();
    reply.clear();
    br_a.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""ok":true"#), "connection A should survive its own rotation request: {reply}");

    // `spawn`'s listener accepts one connection at a time (module doc)
    // — `accept_loop` only calls `accept()` again once the current
    // `handle_conn` returns, which requires connection A to actually
    // disconnect first. Drop every handle referencing its socket (the
    // borrowed reader, the cloned writer, and the stream itself) so the
    // OS delivers EOF to the server side before conn_b tries to connect.
    drop(br_a);
    drop(writer_a);
    drop(conn_a);

    // (a) A brand-new connection presenting the OLD token is denied.
    let conn_b = UnixStream::connect(&sock_path).expect("conn B failed to connect");
    let mut writer_b = conn_b.try_clone().unwrap();
    let mut br_b = BufReader::new(&conn_b);
    writeln!(writer_b, r#"{{"op":"auth","token":"old-token"}}"#).unwrap();
    reply.clear();
    br_b.read_line(&mut reply).unwrap();
    assert!(reply.contains("auth_failed"), "the pre-rotation token must no longer authenticate: {reply}");
    // Same reasoning as conn_a above: both owned handles (the clone AND
    // the original) must be dropped for the OS to actually close the
    // connection, or conn_c below would hang waiting to be accepted.
    drop(br_b);
    drop(writer_b);
    drop(conn_b);

    // (c) A fresh connection presenting the NEW token (read back from
    // the same file `init` would have pointed the gateway at) succeeds.
    let new_token = std::fs::read_to_string(&token_path).expect("rotated token file must exist");
    assert_ne!(new_token, "old-token", "the token file must actually have changed");
    let conn_c = UnixStream::connect(&sock_path).expect("conn C failed to connect");
    let mut writer_c = conn_c.try_clone().unwrap();
    let mut br_c = BufReader::new(&conn_c);
    writeln!(writer_c, r#"{{"op":"auth","token":"{new_token}"}}"#).unwrap();
    reply.clear();
    br_c.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""authenticated":true"#), "the freshly rotated token must authenticate: {reply}");

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&token_path);
}

// ── WP-CD2-freeze-scope: socket-thread optimistic pre-check ──

/// Invariant (d): with no shadow session active, a frozen seat denies a
/// `move` at the socket thread exactly as it always did — byte-
/// identical to the pre-WP behavior for the non-shadow case.
#[test]
fn frozen_without_shadow_active_denies_move_before_reaching_channel() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-fzns-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    // shadow_active left false (the default) — deliberately not set.
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"move","x":1.0,"y":1.0}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains("agent_seat_frozen"), "unexpected response with no shadow session active: {reply}");
    assert!(rx.try_recv().is_err(), "a denied command must never reach the main-thread channel");

    let _ = std::fs::remove_file(&sock_path);
}

/// Invariant (c)'s socket-thread half: a shadow session being active
/// makes the socket thread FORWARD a non-`shadow` op instead of denying
/// it outright — the precise per-op decision is left to the main
/// thread's `shadow::is_freeze_bypass_eligible` (untestable here
/// without a full `DuduclawComp`; see that function's own unit tests
/// in `shadow.rs`). This test only proves the forwarding half.
#[test]
fn frozen_with_shadow_active_forwards_non_shadow_op_to_channel() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-fzsa-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    shared.shadow_active.store(true, Ordering::SeqCst);
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"move","x":1.0,"y":1.0}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(!reply.contains("agent_seat_frozen"), "a shadow-active session must not be denied at the socket thread: {reply}");
    assert!(reply.contains(r#""frozen":true"#), "the ack must honestly report the seat is still frozen: {reply}");

    match rx.try_recv() {
        Ok(InjectCmd::Move { x, y }) => {
            assert_eq!((x, y), (1.0, 1.0));
        }
        other => panic!("expected the move command forwarded to the main-thread channel, got {other:?}"),
    }

    let _ = std::fs::remove_file(&sock_path);
}

/// Invariant (a)'s socket-thread half: the `shadow` toggle op is
/// ALWAYS denied while frozen — even when a shadow session is already
/// active — never forwarded to let the main thread decide. This is
/// stricter than the non-toggle ops precisely because DESIGN forbids
/// using this one op, in either direction, to change freeze exposure.
#[test]
fn frozen_shadow_toggle_always_denied_even_when_shadow_already_active() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-fzsh-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    shared.shadow_active.store(true, Ordering::SeqCst);
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    for enable in [true, false] {
        writeln!(writer, r#"{{"op":"shadow","enable":{enable}}}"#).unwrap();
        reply.clear();
        br.read_line(&mut reply).unwrap();
        assert!(reply.contains("agent_seat_frozen"), "shadow(enable:{enable}) must be denied while frozen: {reply}");
    }
    assert!(rx.try_recv().is_err(), "a denied shadow toggle must never reach the main-thread channel");

    let _ = std::fs::remove_file(&sock_path);
}

// CD-3 socket-level scenarios (takeover-overrides-shadow-bypass, the
// `status` ack's new `takeover` field, `validate`'s new reason-length
// check) live in `codrive/tests_takeover.rs`, not here — see that file's
// module doc.

// ── WP-CD4a-COMP: `activate_window` wire-level scenarios ──

#[test]
fn validate_rejects_empty_activate_window_app_id() {
    let cmd = InjectCmd::ActivateWindow { app_id: String::new() };
    assert!(validate(&cmd).is_err());
}

#[test]
fn validate_rejects_oversized_activate_window_app_id() {
    let cmd = InjectCmd::ActivateWindow {
        app_id: "x".repeat(super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES + 1),
    };
    assert!(validate(&cmd).is_err());
}

#[test]
fn validate_accepts_reasonable_activate_window_app_id() {
    let cmd = InjectCmd::ActivateWindow { app_id: "foot-A".to_string() };
    assert!(validate(&cmd).is_ok());
}

/// "凍結中拒" — the socket-thread half (end-to-end evidence beyond the pure
/// `shadow::freeze_bypass_decision_activate_window_never_bypasses` unit
/// test): with no shadow session active, a frozen seat denies
/// `activate_window` at the socket thread exactly like `move` does in
/// `frozen_without_shadow_active_denies_move_before_reaching_channel` above
/// — never even reaching the main-thread channel.
#[test]
fn frozen_denies_activate_window_before_reaching_channel() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-fzaw-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"activate_window","app_id":"foot-A"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains("agent_seat_frozen"), "unexpected response while frozen: {reply}");
    assert!(rx.try_recv().is_err(), "a denied activate_window must never reach the main-thread channel");

    let _ = std::fs::remove_file(&sock_path);
}

/// Sanity/wiring check: with the seat NOT frozen, `activate_window` passes
/// `validate` and is forwarded to the main-thread channel exactly like
/// `take_over_can_be_sent_and_acked_when_not_frozen` proves for `TakeOver`
/// in `tests_takeover.rs` — this is what lets `codrive::handle_agent_inject`
/// (main thread, `window_target.rs`) ever actually run.
#[test]
fn activate_window_forwarded_to_channel_when_not_frozen() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-awok-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"activate_window","app_id":"foot-A"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""ok":true"#), "unexpected activate_window ack: {reply}");
    match rx.try_recv() {
        Ok(InjectCmd::ActivateWindow { app_id }) => assert_eq!(app_id, "foot-A"),
        other => panic!("expected the activate_window command forwarded to the main-thread channel, got {other:?}"),
    }

    let _ = std::fs::remove_file(&sock_path);
}

// ── WP-CD4b-fix (B3): the read-only `window_geometry` query ──

/// Field-level validation: an identity-less query is refused outright
/// rather than being "helpfully" matched against whatever single window
/// happens to be mapped (`window_geometry.rs`'s fail-closed policy).
#[test]
fn validate_rejects_window_geometry_without_any_identity() {
    let cmd = InjectCmd::WindowGeometry { app_id: None, pid: None };
    assert!(validate(&cmd).is_err());
}

#[test]
fn validate_rejects_window_geometry_empty_or_oversized_app_id() {
    let empty = InjectCmd::WindowGeometry { app_id: Some(String::new()), pid: None };
    assert!(validate(&empty).is_err());
    let oversized = InjectCmd::WindowGeometry {
        app_id: Some("x".repeat(super::protocol::MAX_ACTIVATE_WINDOW_QUERY_BYTES + 1)),
        pid: None,
    };
    assert!(validate(&oversized).is_err());
}

#[test]
fn validate_rejects_window_geometry_pid_zero() {
    let cmd = InjectCmd::WindowGeometry { app_id: None, pid: Some(0) };
    assert!(validate(&cmd).is_err());
}

#[test]
fn validate_accepts_window_geometry_with_either_identity() {
    assert!(validate(&InjectCmd::WindowGeometry { app_id: None, pid: Some(1234) }).is_ok());
    assert!(validate(&InjectCmd::WindowGeometry { app_id: Some("foot".into()), pid: None }).is_ok());
    assert!(validate(&InjectCmd::WindowGeometry { app_id: Some("foot".into()), pid: Some(1234) }).is_ok());
}

/// With no main-thread query bridge installed (exactly the state
/// `CodriveShared::for_test` leaves it in, and the state a real run is in
/// if `init` ever failed before wiring it), the query answers an explicit
/// `compositor_unavailable` — never a fabricated coordinate, and never a
/// hang. This is the property the whole fix rests on: an unanswerable
/// query must produce a REFUSAL the gateway can fall back from.
#[test]
fn window_geometry_without_a_main_thread_bridge_fails_closed() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-wgnb-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"window_geometry","pid":4321}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""ok":false"#), "unexpected window_geometry ack: {reply}");
    assert!(reply.contains("compositor_unavailable"), "unexpected window_geometry ack: {reply}");
    assert!(
        rx.try_recv().is_err(),
        "a query must never travel down the fire-and-forget InjectCmd channel"
    );

    let _ = std::fs::remove_file(&sock_path);
}

/// The query is answered even while the seat is frozen, exactly like
/// `status` — see `window_geometry.rs`'s "Not an action" section for why
/// denying a read-only query under freeze would push the agent toward the
/// LESS informed click, not away from it. What matters here is that the
/// answer is NOT `agent_seat_frozen`.
#[test]
fn window_geometry_is_answered_while_frozen() {
    let sock_path = std::env::temp_dir().join(format!("duduclaw-codrive-test-wgfz-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    // `terminated` is deliberately NOT set here: `handle_conn` clears it on
    // every successful auth (a fresh connection IS a fresh session), so it
    // could never be observed by a command on this connection anyway.
    shared.frozen.store(true, Ordering::SeqCst);
    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(sock_path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&sock_path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"window_geometry","app_id":"foot-A"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(
        !reply.contains("agent_seat_frozen"),
        "a read-only query must be answered outside the frozen gate, got: {reply}"
    );

    let _ = std::fs::remove_file(&sock_path);
}
