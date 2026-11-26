//! V1 local identity protocol: big-endian u32 length followed by one JSON object.
//! No HTTP, redirects, proxy support, persistent connections or unbounded reads.
use std::{io, time::Duration};
#[cfg(unix)]
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_REQUEST: usize = 4096;
pub const MAX_RESPONSE: usize = 16384;
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Strict versioned server pin supplied by the gateway, never discovery contents.
pub fn server_identity(value: &str) -> Option<super::ProcessIdentity> {
    let (pid, birth) = value.strip_prefix("v1:")?.split_once(':')?;
    if pid.is_empty() || birth.is_empty()
        || !pid.bytes().all(|b| b.is_ascii_digit()) || !birth.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pid = pid.parse().ok()?;
    let birth = birth.parse().ok()?;
    (pid != 0 && birth != 0).then_some(super::ProcessIdentity { pid, birth })
}

pub fn encode_server_identity(identity: super::ProcessIdentity) -> String {
    format!("v1:{}:{}", identity.pid, identity.birth)
}

pub fn endpoint_path(endpoint: &str) -> Option<&str> {
    if endpoint.contains('\0') || endpoint.trim() != endpoint { return None; }
    #[cfg(unix)]
    if let Some(path) = endpoint.strip_prefix("unix:v1:") {
        return Path::new(path).is_absolute().then_some(path);
    }
    #[cfg(windows)]
    if let Some(path) = endpoint.strip_prefix("pipe:v1:") {
        // Restrict to the local namespace and our single-component pipe names.
        let name = path.strip_prefix(r"\\.\pipe\duduclaw-identity-")?;
        return (!name.is_empty() && name.bytes().all(|c| c.is_ascii_hexdigit())).then_some(path);
    }
    None
}

pub async fn read_frame(stream: &mut (impl AsyncRead + Unpin), limit: usize) -> io::Result<serde_json::Value> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > limit { return Err(io::ErrorKind::InvalidData.into()); }
    let mut bytes = zeroize::Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| io::ErrorKind::InvalidData.into())
}

pub async fn write_frame(stream: &mut (impl AsyncWrite + Unpin), value: &serde_json::Value, limit: usize) -> io::Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    if bytes.is_empty() || bytes.len() > limit { return Err(io::ErrorKind::InvalidData.into()); }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// The caller opens a fresh connection itself so the kernel identifies it.
pub async fn request(endpoint: &str, server: &str, value: &serde_json::Value) -> io::Result<serde_json::Value> {
    let expected = server_identity(server).ok_or(io::ErrorKind::PermissionDenied)?;
    let path = endpoint_path(endpoint).ok_or(io::ErrorKind::InvalidInput)?;
    tokio::time::timeout(TIMEOUT, async {
        #[cfg(unix)]
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        #[cfg(windows)]
        let mut stream = loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                Ok(stream) => break stream,
                Err(e) if e.raw_os_error() == Some(231) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(e) => return Err(e),
            }
        };
        #[cfg(unix)]
        let peer = {
            use std::os::fd::{AsRawFd, BorrowedFd};
            // SAFETY: the connected client owns the descriptor throughout capture.
            super::RootProcess::unix_peer(unsafe { BorrowedFd::borrow_raw(stream.as_raw_fd()) })?
        };
        #[cfg(windows)]
        let peer = {
            use std::os::windows::io::{AsRawHandle, BorrowedHandle};
            // SAFETY: the connected client owns the handle throughout capture.
            super::RootProcess::pipe_server(unsafe { BorrowedHandle::borrow_raw(stream.as_raw_handle()) })?
        };
        // Retain the process handle/pidfd until the exchange ends. No frame header
        // or credential bytes are written until both PID and generation match.
        if peer.identity() != expected || !peer.is_current() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        write_frame(&mut stream, value, MAX_REQUEST).await?;
        let response = read_frame(&mut stream, MAX_RESPONSE).await?;
        // Acknowledge receipt before the Windows server closes its pipe handle.
        // Closing a pipe with unread output may discard that output.
        stream.write_u8(1).await?;
        Ok(response)
    }).await.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_http_and_other_protocol_versions_are_refused() {
        for endpoint in ["http://127.0.0.1:42/api/internal/identity/introspect", "unix:/tmp/s", "unix:v2:/tmp/s",
            "pipe:v1:\\\\remote\\pipe\\duduclaw-identity-abc", "unix:v1:relative"] {
            assert!(endpoint_path(endpoint).is_none());
        }
    }
    #[test]
    fn server_pin_requires_a_complete_versioned_process_generation() {
        let identity = super::super::ProcessIdentity { pid: 123, birth: 456 };
        assert_eq!(server_identity(&encode_server_identity(identity)), Some(identity));
        for value in ["", "v1", "v1:123", "v2:123:456", "v1:0:456", "v1:123:0",
            "v1:123:456:789", " v1:123:456", "v1:+123:456", "v1:123:456\0",
            "v1:4294967296:456", "v1:123:18446744073709551616"] {
            assert!(server_identity(value).is_none(), "{value:?}");
        }
    }

    #[tokio::test]
    async fn missing_server_identity_is_refused_before_connecting() {
        #[cfg(unix)]
        let endpoint = "unix:v1:/no-such-identity-socket";
        #[cfg(windows)]
        let endpoint = r"pipe:v1:\\.\pipe\duduclaw-identity-abcdef";
        let error = request(endpoint, "", &serde_json::json!({"credential": "never-send"})).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[tokio::test]
    async fn server_generation_mismatch_sends_no_bytes_and_correct_server_works() {
        let identity = super::super::ProcessIdentity::read(std::process::id()).unwrap();
        for valid in [false, true] {
            #[cfg(unix)]
            let home = tempfile::tempdir_in("/tmp").unwrap();
            #[cfg(unix)]
            let endpoint = format!("unix:v1:{}", home.path().join("s").display());
            #[cfg(unix)]
            let listener = tokio::net::UnixListener::bind(endpoint_path(&endpoint).unwrap()).unwrap();
            #[cfg(windows)]
            let endpoint = format!(r"pipe:v1:\\.\pipe\duduclaw-identity-{}", uuid::Uuid::new_v4().simple());
            #[cfg(windows)]
            let mut stream = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true).reject_remote_clients(true)
                .create(endpoint_path(&endpoint).unwrap()).unwrap();
            let (ready, listening) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                #[cfg(unix)]
                let mut stream = {
                    ready.send(()).unwrap();
                    listener.accept().await.unwrap().0
                };
                #[cfg(windows)]
                {
                    let mut connect = Box::pin(stream.connect());
                    let mut ready = Some(ready);
                    std::future::poll_fn(|cx| {
                        let result = std::future::Future::poll(connect.as_mut(), cx);
                        if let Some(ready) = ready.take() { ready.send(()).unwrap(); }
                        result
                    }).await.unwrap();
                }
                if valid {
                    let request = read_frame(&mut stream, MAX_REQUEST).await.unwrap();
                    assert_eq!(request["credential"], "test-proof");
                    write_frame(&mut stream, &serde_json::json!({"active": true}), MAX_RESPONSE).await.unwrap();
                    assert_eq!(stream.read_u8().await.unwrap(), 1);
                } else {
                    let mut received = Vec::new();
                    if let Err(error) = stream.read_to_end(&mut received).await {
                        assert!(matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset));
                    }
                    assert!(received.is_empty());
                }
            });
            tokio::time::timeout(TIMEOUT, listening).await.unwrap().unwrap();
            let pin = encode_server_identity(super::super::ProcessIdentity {
                birth: if valid { identity.birth } else { identity.birth + 1 }, ..identity
            });
            let result = request(&endpoint, &pin, &serde_json::json!({"credential": "test-proof"})).await;
            if valid { assert_eq!(result.unwrap()["active"], true); }
            else { assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied); }
            tokio::time::timeout(TIMEOUT, server).await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocating_body() {
        let (mut sender, mut receiver) = tokio::io::duplex(8);
        sender.write_u32((MAX_REQUEST + 1) as u32).await.unwrap();
        assert!(read_frame(&mut receiver, MAX_REQUEST).await.is_err());
    }
}
