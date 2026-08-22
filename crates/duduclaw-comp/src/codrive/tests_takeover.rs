//! CD-3 socket-level integration tests: `take_over`/`watch`'s effect on
//! `listener.rs`'s accept/read loop. Split out of `listener.rs`'s own
//! `#[cfg(test)] mod tests` (which was already at this project's per-file
//! size convention before CD-3 — see that module's closing comment) rather
//! than grown there, same "new scenarios get their own `tests_<topic>.rs`"
//! pattern `duduclaw-gateway/src/codrive/tests_identity.rs` established on
//! the gateway side. Uses `super::listener::{spawn, validate}` and
//! `super::CodriveShared` exactly like `listener.rs`'s own tests do — both
//! are visible here because this module, like `listener`, is a direct child
//! of `codrive` (Rust's "visible within the defining module's whole
//! subtree" privacy rule, not a special-case export).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use smithay::reexports::calloop;

use super::listener::{spawn, validate};
use super::protocol::InjectCmd;
use super::CodriveShared;

fn sock_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("duduclaw-codrive-test-{label}-{}.sock", std::process::id()))
}

/// The load-bearing CD-3 socket-thread invariant (`takeover.rs` module
/// doc): with an active takeover, a shadow-confined `move` must be denied
/// outright — NOT forwarded the way an ordinary freeze forwards a shadow-
/// confined op (see `listener.rs`'s own `frozen_with_shadow_active_
/// forwards_non_shadow_op_to_channel`). Credential windows get zero
/// exceptions.
#[test]
fn frozen_with_takeover_active_denies_even_with_shadow_active() {
    let path = sock_path("fztk");
    let _ = std::fs::remove_file(&path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    shared.shadow_active.store(true, Ordering::SeqCst);
    shared.takeover_active.store(true, Ordering::SeqCst);
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"move","x":1.0,"y":1.0}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains("agent_seat_frozen"), "an active takeover must deny even a shadow-confined op: {reply}");
    assert!(rx.try_recv().is_err(), "a takeover-denied command must never reach the main-thread channel");

    let _ = std::fs::remove_file(&path);
}

/// The complementary invariant: with NO takeover active, a shadow-confined
/// op is still forwarded exactly as WP-CD2-freeze-scope established —
/// proving the new `takeover_active` gate is additive, not a regression on
/// the CD-2 behavior it sits next to.
#[test]
fn frozen_with_shadow_active_and_no_takeover_still_forwards() {
    let path = sock_path("fzsn");
    let _ = std::fs::remove_file(&path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    shared.shadow_active.store(true, Ordering::SeqCst);
    // takeover_active left false (the default) — deliberately not set.
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"move","x":1.0,"y":1.0}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(!reply.contains("agent_seat_frozen"), "no takeover active — the CD-2 shadow-bypass forward must still work: {reply}");
    assert!(rx.try_recv().is_ok(), "expected the move command forwarded to the main-thread channel");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn status_reports_takeover_field() {
    let path = sock_path("sttk");
    let _ = std::fs::remove_file(&path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    shared.frozen.store(true, Ordering::SeqCst);
    shared.takeover_active.store(true, Ordering::SeqCst);
    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"status"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""takeover":true"#), "unexpected status response: {reply}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn status_takeover_field_false_by_default() {
    let path = sock_path("sttf");
    let _ = std::fs::remove_file(&path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    let (tx, _rx) = calloop::channel::channel::<InjectCmd>();
    spawn(path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"status"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""takeover":false"#), "unexpected status response: {reply}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn take_over_can_be_sent_and_acked_when_not_frozen() {
    let path = sock_path("tkok");
    let _ = std::fs::remove_file(&path);

    let shared = Arc::new(CodriveShared::for_test(Some("tok".to_string())));
    let (tx, rx) = calloop::channel::channel::<InjectCmd>();
    spawn(path.clone(), Arc::clone(&shared), tx).expect("test listener failed to bind");

    let conn = UnixStream::connect(&path).expect("test client failed to connect");
    let mut writer = conn.try_clone().unwrap();
    let mut br = BufReader::new(&conn);
    writeln!(writer, r#"{{"op":"auth","token":"tok"}}"#).unwrap();
    let mut reply = String::new();
    br.read_line(&mut reply).unwrap();

    writeln!(writer, r#"{{"op":"take_over","reason":"login page"}}"#).unwrap();
    reply.clear();
    br.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""ok":true"#), "unexpected take_over ack: {reply}");
    match rx.try_recv() {
        Ok(InjectCmd::TakeOver { reason }) => assert_eq!(reason, "login page"),
        other => panic!("expected the take_over command forwarded to the main-thread channel, got {other:?}"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn validate_rejects_oversized_take_over_reason() {
    let cmd = InjectCmd::TakeOver { reason: "x".repeat(super::protocol::MAX_TAKE_OVER_REASON_BYTES + 1) };
    assert!(validate(&cmd).is_err());
}

#[test]
fn validate_accepts_reasonable_take_over_reason() {
    let cmd = InjectCmd::TakeOver { reason: "登入頁需要密碼".to_string() };
    assert!(validate(&cmd).is_ok());
}

#[test]
fn validate_accepts_watch_toggle() {
    assert!(validate(&InjectCmd::Watch { enable: true }).is_ok());
    assert!(validate(&InjectCmd::Watch { enable: false }).is_ok());
}
