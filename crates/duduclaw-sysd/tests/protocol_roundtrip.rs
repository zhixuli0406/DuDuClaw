//! Integration test: exercises the crate's PUBLIC API end to end
//! (`bind` + `serve` + `SysdClient`) over a real Unix domain socket at a
//! `tempfile`-backed path (the `DUDUCLAW_SYSD_SOCKET` env-override path,
//! same mechanism the appliance dev/test story relies on) — as opposed to
//! `src/server.rs`'s unit tests, which reach into server internals from
//! within the crate.

// Real-UDS test — meaningless on non-unix targets, where `bind` is the
// fail-closed stub (see `lib.rs`).
#![cfg(unix)]

use std::time::Duration;

use duduclaw_sysd::{SysdClient, SysdServerConfig, SysdRequest, bind, serve};

fn current_uid() -> u32 {
    // SAFETY: `getuid()` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

async fn spawn_test_server(
    allowed_uid: Option<u32>,
) -> (std::path::PathBuf, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("sysd.sock");
    std::mem::forget(dir); // keep the directory alive for the socket's lifetime

    let listener = bind(&socket_path, None).unwrap();
    let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid };
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, config, async {
            let _ = rx.await;
        })
        .await;
    });
    (socket_path, tx, handle)
}

#[tokio::test]
async fn client_round_trips_a_real_request_through_a_real_socket() {
    let (socket_path, tx, handle) = spawn_test_server(Some(current_uid())).await;
    let client = SysdClient::new(socket_path).with_timeout(Duration::from_secs(5));

    // `sysupdate_status` doesn't require any params, and this test is not
    // about whether `systemd-sysupdate` exists on the CI/dev host — it is
    // about whether an authorized, well-formed request reaches dispatch at
    // all. Any outcome except "sysd rejected the call as unauthorized" or
    // a malformed-request decode error proves that.
    let result = client.call(&SysdRequest::SysupdateStatus).await;
    match result {
        Ok(_) => {}
        Err(duduclaw_sysd::SysdClientError::Rejected { kind, .. }) => {
            assert_eq!(kind, "unsupported", "must not be rejected as unauthorized/bad_request");
        }
        Err(e) => panic!("unexpected transport-level error: {e}"),
    }

    let _ = tx.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn unknown_verb_over_the_wire_is_rejected_as_bad_request() {
    // Exercises the "malformed request never reaches an unwrap/panic"
    // property from the client side: hand-craft a raw line with a verb
    // that isn't in the closed enum and confirm the SERVER's response
    // (not the client's local serde, which would refuse to even construct
    // this — we go around it deliberately) comes back as a structured
    // rejection.
    let (socket_path, tx, handle) = spawn_test_server(Some(current_uid())).await;

    use std::io::Write as _;
    let raw_response: String = tokio::task::spawn_blocking({
        let socket_path = socket_path.clone();
        move || {
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
            writeln!(stream, r#"{{"verb":"self_destruct"}}"#).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut out = String::new();
            std::io::Read::read_to_string(&mut stream, &mut out).unwrap();
            out
        }
    })
    .await
    .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(raw_response.trim()).unwrap();
    assert_eq!(parsed["ok"], serde_json::Value::Bool(false));
    assert_eq!(parsed["error"]["kind"], "bad_request");
    assert!(parsed["audit_id"].as_str().is_some_and(|s| !s.is_empty()));

    let _ = tx.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn uid_mismatch_is_rejected_even_over_a_successful_connection() {
    // `current_uid() + 1` stands in for "some other uid" — we cannot
    // actually connect as a different real uid without root in a test
    // environment, so the mismatch is constructed by configuring the
    // server's `allowed_uid` to something that is NOT our real uid while
    // connecting as ourselves (the only uid we're actually capable of
    // connecting as).
    let wrong_uid = current_uid().wrapping_add(1);
    let (socket_path, tx, handle) = spawn_test_server(Some(wrong_uid)).await;
    let client = SysdClient::new(socket_path).with_timeout(Duration::from_secs(5));

    let result = client.call(&SysdRequest::Reboot).await;
    match result {
        Err(duduclaw_sysd::SysdClientError::Rejected { kind, .. }) => assert_eq!(kind, "unauthorized"),
        other => panic!("expected an unauthorized rejection, got {other:?}"),
    }

    let _ = tx.send(());
    let _ = handle.await;
}
