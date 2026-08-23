//! UDS server: bind, peer-credential check, one-line-request dispatch.
//!
//! Every accepted connection is handled independently (one spawned task
//! per connection) so a slow/stuck peer cannot block other callers. There
//! is no expectation of concurrent load — this is a root daemon serving
//! human-triggered device operations — so no connection-limiting beyond
//! the OS backlog is implemented this round.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};
use uuid::Uuid;

use crate::dispatch::dispatch;
use crate::protocol::{MAX_REQUEST_LINE_BYTES, SysdError, SysdRequest, SysdResponse};

/// Server configuration. `allowed_uid: None` is a legal, intentional
/// state (see [`crate::protocol::ALLOWED_UID_ENV`]'s doc comment) — it
/// means "deny every connection", not "accept every connection".
#[derive(Debug, Clone)]
pub struct SysdServerConfig {
    pub socket_path: std::path::PathBuf,
    pub allowed_uid: Option<u32>,
}

/// Bind the UDS.
///
/// Ordering matters for the "never world-accessible" guarantee: create
/// the parent directory (or confirm it exists) and IMMEDIATELY set its
/// mode before doing anything else; same for the socket file after
/// `bind()`. No connection can be accepted before `serve()` is called on
/// the returned listener, so there is no window in which a connection
/// could race the permission tightening.
///
/// The file-permission layer must ADMIT the same audience the
/// SO_PEERCRED gate in `handle_connection` admits, or that gate is dead
/// logic: the client uid is never root (that is the whole point of the
/// privilege separation), and the original `0700` parent + `0600 root`
/// socket stopped exactly the one uid `allowed_uid` exists to let in —
/// connect() failed with EACCES before peer credentials were ever read.
/// Found live on the appliance VM (2026-08-23): the lock-screen reboot
/// was audited by the gateway and then silently did nothing. So, when an
/// allowed uid is configured: parent `0711` (traverse-only for others —
/// path-walking to the socket is possible, listing the directory is
/// not), socket chown'd to that uid with mode `0600` (owner + root can
/// connect, everyone else is refused at the filesystem). The peer-cred
/// check stays authoritative; the two layers now agree instead of
/// contradicting each other. With no allowed uid the old fully-closed
/// modes are kept — sysd rejects every connection in that state anyway.
///
/// A stale socket file left behind by an unclean shutdown is removed
/// first (bind() would otherwise fail with `AddrInUse`) — this only ever
/// removes a file at the exact configured socket path, never anything
/// else under the directory.
pub fn bind(socket_path: &Path, allowed_uid: Option<u32>) -> io::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
        let parent_mode = if allowed_uid.is_some() { 0o711 } else { 0o700 };
        fs::set_permissions(parent, fs::Permissions::from_mode(parent_mode))?;
    }
    match fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(socket_path)?;
    if let Some(uid) = allowed_uid {
        // Non-fatal: an unprivileged process (dev/test — production sysd is
        // root) cannot chown to a DIFFERENT uid (EPERM). Failing open here
        // only leaves the socket LESS accessible (still owner-only), never
        // more — this is an admission fix, not a security gate; the
        // SO_PEERCRED check in `handle_connection` stays authoritative.
        if let Err(e) = std::os::unix::fs::chown(socket_path, Some(uid), None) {
            warn!(uid, error = %e, "sysd: could not chown socket to allowed uid; socket stays owner-only");
        }
    }
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accept loop. Runs until `shutdown` resolves (e.g. a SIGTERM future in
/// `main.rs`), then stops accepting NEW connections and returns —
/// in-flight connections that were already spawned are not forcibly
/// cut off, they simply finish their one request/response cycle.
pub async fn serve(
    listener: UnixListener,
    config: SysdServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    let allowed_uid = config.allowed_uid;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                tokio::spawn(handle_connection(stream, allowed_uid));
            }
            _ = &mut shutdown => {
                info!("sysd: shutdown signal received, no longer accepting connections");
                return Ok(());
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, allowed_uid: Option<u32>) {
    let audit_id = Uuid::new_v4().to_string();

    // Peer credentials are read from the socket itself (SO_PEERCRED on
    // Linux, via tokio's `peer_cred()`) — not anything the client sends,
    // so there is nothing for a malicious peer to spoof here. Checked
    // BEFORE any data is read from the connection: an unauthorized peer's
    // bytes are never even parsed.
    let peer_uid = match stream.peer_cred() {
        Ok(cred) => Some(cred.uid()),
        Err(e) => {
            warn!(audit_id = %audit_id, error = %e, "sysd: could not read peer credentials; denying");
            None
        }
    };
    let authorized = matches!((allowed_uid, peer_uid), (Some(a), Some(p)) if a == p);

    let (reader, writer) = stream.into_split();

    if !authorized {
        warn!(
            audit_id = %audit_id,
            peer_uid = ?peer_uid,
            allowed_uid = ?allowed_uid,
            "sysd: rejected connection — caller uid not authorized"
        );
        let resp = SysdResponse::err(audit_id, SysdError::unauthorized());
        let _ = write_response(writer, &resp).await;
        // Drain (bounded) whatever the peer already sent before closing:
        // on Linux, closing a socket with unread inbound data turns the
        // close into an RST that can destroy the just-written rejection
        // response in flight — the client then sees ECONNRESET instead of
        // the structured `unauthorized` error (surfaced as the CI-only
        // `mismatched_uid_is_rejected` failure; macOS delivers the
        // response first, which is why it never reproduced locally). The
        // drain is size-capped AND time-capped, so a hostile unauthorized
        // peer cannot hold the connection open past ~500ms.
        let mut sink = Vec::new();
        let mut bounded =
            tokio::io::AsyncReadExt::take(reader, MAX_REQUEST_LINE_BYTES as u64);
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::io::AsyncReadExt::read_to_end(&mut bounded, &mut sink),
        )
        .await;
        return;
    }

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let read_outcome =
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
    let n = match read_outcome {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            warn!(audit_id = %audit_id, error = %e, "sysd: connection read error");
            let resp =
                SysdResponse::err(audit_id, SysdError::bad_request(format!("read error: {e}")));
            let _ = write_response(writer, &resp).await;
            return;
        }
        Err(_) => {
            warn!(audit_id = %audit_id, "sysd: request read timed out (no newline within 5s)");
            let resp = SysdResponse::err(audit_id, SysdError::bad_request("request timed out"));
            let _ = write_response(writer, &resp).await;
            return;
        }
    };
    if n == 0 || line.len() > MAX_REQUEST_LINE_BYTES {
        warn!(audit_id = %audit_id, bytes = n, "sysd: empty or oversized request line rejected");
        let resp =
            SysdResponse::err(audit_id, SysdError::bad_request("empty or oversized request"));
        let _ = write_response(writer, &resp).await;
        return;
    }

    let req: SysdRequest = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                audit_id = %audit_id,
                error = %e,
                peer_uid = ?peer_uid,
                "sysd: malformed request rejected (unknown verb or bad JSON)"
            );
            let resp = SysdResponse::err(
                audit_id,
                SysdError::bad_request(format!("malformed request: {e}")),
            );
            let _ = write_response(writer, &resp).await;
            return;
        }
    };

    info!(audit_id = %audit_id, verb = req.verb_name(), peer_uid = ?peer_uid, "sysd: dispatching");
    let result = dispatch(&req).await;
    match &result {
        Ok(out) => {
            info!(audit_id = %audit_id, verb = req.verb_name(), success = out.success, "sysd: completed")
        }
        Err(e) => {
            warn!(audit_id = %audit_id, verb = req.verb_name(), kind = %e.kind, message = %e.message, "sysd: op failed")
        }
    }
    let resp = match result {
        Ok(out) => SysdResponse::ok(audit_id, out),
        Err(e) => SysdResponse::err(audit_id, e),
    };
    let _ = write_response(writer, &resp).await;
}

async fn write_response(mut writer: OwnedWriteHalf, resp: &SysdResponse) -> io::Result<()> {
    let mut payload = serde_json::to_string(resp).unwrap_or_else(|_| {
        format!(
            r#"{{"audit_id":{:?},"ok":false,"error":{{"kind":"internal","message":"failed to serialize response"}}}}"#,
            resp.audit_id
        )
    });
    payload.push('\n');
    writer.write_all(payload.as_bytes()).await?;
    writer.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixStream as StdUnixStream;

    fn temp_socket_path() -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        // Keep the tempdir alive for the socket's lifetime by leaking its
        // handle into the returned path's parent — simplest way to avoid
        // the socket file outliving its directory within one test.
        let path = dir.path().join("sysd-test.sock");
        std::mem::forget(dir);
        path
    }

    fn current_uid() -> u32 {
        // SAFETY: `getuid()` has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// End-to-end: bind, serve in the background, connect as ourselves
    /// (peer uid == our own uid, since the test process both listens and
    /// connects), send a request, read the response — exercises the real
    /// SO_PEERCRED path, not a mock.
    #[tokio::test]
    async fn authorized_peer_gets_a_dispatched_response() {
        let socket_path = temp_socket_path();
        let listener = bind(&socket_path, None).unwrap();
        let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid: Some(current_uid()) };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = serve(listener, config, async {
                let _ = rx.await;
            })
            .await;
        });

        let resp = send_request(&socket_path, r#"{"verb":"sysupdate_status"}"#).await;
        // `systemd-sysupdate` doesn't exist on every test host (this crate
        // must build/test cross-platform), so the assertion here is on the
        // AUTHORIZATION outcome, not the shell-out's own success: an
        // authorized, well-formed request must reach `dispatch()` and come
        // back either `ok: true` (command ran) or a `kind: "unsupported"`
        // spawn failure — never `unauthorized` or `bad_request`.
        match &resp.error {
            None => assert!(resp.ok, "ok response must carry data: {resp:?}"),
            Some(e) => assert_eq!(e.kind, "unsupported", "unexpected error kind: {resp:?}"),
        }

        let _ = tx.send(());
        let _ = server.await;
    }

    /// The core security property: a peer whose real uid does not match
    /// the configured `allowed_uid` is rejected — even though the
    /// connection itself succeeds at the OS level. `current_uid() + 1`
    /// stands in for "some other uid" (we can't actually connect as a
    /// different real uid without root in a test).
    #[tokio::test]
    async fn mismatched_uid_is_rejected() {
        let socket_path = temp_socket_path();
        let listener = bind(&socket_path, None).unwrap();
        let wrong_uid = current_uid().wrapping_add(1);
        let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid: Some(wrong_uid) };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = serve(listener, config, async {
                let _ = rx.await;
            })
            .await;
        });

        let resp = send_request(&socket_path, r#"{"verb":"reboot"}"#).await;
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().kind, "unauthorized");
        assert!(resp.data.is_none());

        let _ = tx.send(());
        let _ = server.await;
    }

    /// `allowed_uid: None` (no env/arg configured) must deny even a
    /// same-uid connection — fail-closed default, not "open by omission".
    #[tokio::test]
    async fn unconfigured_allowed_uid_denies_everyone() {
        let socket_path = temp_socket_path();
        let listener = bind(&socket_path, None).unwrap();
        let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid: None };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = serve(listener, config, async {
                let _ = rx.await;
            })
            .await;
        });

        let resp = send_request(&socket_path, r#"{"verb":"reboot"}"#).await;
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().kind, "unauthorized");

        let _ = tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn unknown_verb_is_a_structured_bad_request() {
        let socket_path = temp_socket_path();
        let listener = bind(&socket_path, None).unwrap();
        let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid: Some(current_uid()) };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = serve(listener, config, async {
                let _ = rx.await;
            })
            .await;
        });

        let resp = send_request(&socket_path, r#"{"verb":"launch_missiles"}"#).await;
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().kind, "bad_request");

        let _ = tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn malformed_json_is_a_structured_bad_request_not_a_crash() {
        let socket_path = temp_socket_path();
        let listener = bind(&socket_path, None).unwrap();
        let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid: Some(current_uid()) };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = serve(listener, config, async {
                let _ = rx.await;
            })
            .await;
        });

        let resp = send_request(&socket_path, "{not valid json at all").await;
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().kind, "bad_request");

        // The server task must still be alive and able to serve a second,
        // well-formed request — one bad line must not have wedged anything.
        // `systemd-sysupdate` doesn't exist on every test host, so — same
        // caveat as `authorized_peer_gets_a_dispatched_response` — the
        // assertion is "reached real dispatch" (ok, or an `unsupported`
        // spawn failure), not "the shell-out itself succeeded".
        let resp2 = send_request(&socket_path, r#"{"verb":"sysupdate_status"}"#).await;
        match &resp2.error {
            None => assert!(resp2.ok, "ok response must carry data: {resp2:?}"),
            Some(e) => assert_eq!(
                e.kind, "unsupported",
                "server should still be healthy after a bad request: {resp2:?}"
            ),
        }

        let _ = tx.send(());
        let _ = server.await;
    }

    // `#[tokio::test]`, not `#[test]`: `UnixListener::bind` registers the
    // socket with tokio's reactor at creation time and panics with "there
    // is no reactor running" outside a Tokio runtime context, even though
    // `bind()` itself is a plain sync function.
    #[tokio::test]
    async fn bind_sets_directory_and_socket_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("nested").join("sysd.sock");
        let _listener = bind(&socket_path, None).unwrap();

        let dir_mode = fs::metadata(socket_path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "parent dir must be 0700");

        let sock_mode = fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "socket file must be 0600");
    }

    /// The 2026-08-23 appliance regression: with an allowed uid configured,
    /// the parent dir must be traversable (0711) and the socket owned by
    /// that uid, or connect() dies with EACCES before the SO_PEERCRED gate
    /// ever runs. chown-to-own-uid is a no-op that succeeds unprivileged,
    /// so this pins the whole code path without needing root.
    #[tokio::test]
    async fn bind_with_allowed_uid_makes_the_socket_reachable_by_that_uid() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("nested").join("sysd.sock");
        let _listener = bind(&socket_path, Some(current_uid())).unwrap();

        let dir_mode = fs::metadata(socket_path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o711, "parent dir must be traverse-only for non-owners");

        let meta = fs::metadata(&socket_path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "socket file must stay 0600");
        assert_eq!(
            std::os::unix::fs::MetadataExt::uid(&meta),
            current_uid(),
            "socket must be owned by the allowed uid so its 0600 admits that uid"
        );
    }

    #[tokio::test]
    async fn bind_removes_a_stale_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("sysd.sock");
        // First bind + drop leaves the socket file on disk (no cleanup on Drop).
        {
            let _listener = bind(&socket_path, None).unwrap();
        }
        assert!(socket_path.exists());
        // Second bind must succeed despite the stale file, not fail with AddrInUse.
        let _listener2 = bind(&socket_path, None).unwrap();
    }

    /// Blocking helper: connect over the real UDS, write one line, read
    /// one line back. Uses `std::os::unix::net::UnixStream` inside
    /// `spawn_blocking` so it exercises a genuinely separate OS-level
    /// connection against the tokio server (not an in-process shortcut).
    async fn send_request(socket_path: &Path, request_line: &str) -> SysdResponse {
        let socket_path = socket_path.to_path_buf();
        let request_line = request_line.to_string();
        tokio::task::spawn_blocking(move || {
            let mut stream = StdUnixStream::connect(&socket_path).unwrap();
            writeln!(stream, "{request_line}").unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut resp_line = String::new();
            std::io::Read::read_to_string(&mut stream, &mut resp_line).unwrap();
            serde_json::from_str::<SysdResponse>(resp_line.trim()).unwrap_or_else(|e| {
                panic!("response did not parse as SysdResponse: {e}; raw={resp_line:?}")
            })
        })
        .await
        .unwrap()
    }
}
