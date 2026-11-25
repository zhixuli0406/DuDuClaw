//! V1 local identity protocol: big-endian u32 length followed by one JSON object.
//! No HTTP, redirects, proxy support, persistent connections or unbounded reads.
use std::{io, time::Duration};
#[cfg(unix)]
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_REQUEST: usize = 4096;
pub const MAX_RESPONSE: usize = 16384;
pub const TIMEOUT: Duration = Duration::from_secs(3);

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
pub async fn request(endpoint: &str, value: &serde_json::Value) -> io::Result<serde_json::Value> {
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
    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocating_body() {
        let (mut sender, mut receiver) = tokio::io::duplex(8);
        sender.write_u32((MAX_REQUEST + 1) as u32).await.unwrap();
        assert!(read_frame(&mut receiver, MAX_REQUEST).await.is_err());
    }
}
