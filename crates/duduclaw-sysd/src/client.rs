//! UDS client SDK — one request per connection, mirroring the server's
//! transport (see `protocol.rs` module docs). Lives in this crate (not the
//! gateway) so client and server share one source of truth for the wire
//! format, the same reasoning `duduclaw-cli-worker::client` documents for
//! its own `WorkerClient`.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::protocol::{SysdError, SysdOpOutput, SysdRequest, SysdResponse};

#[derive(Debug, Error)]
pub enum SysdClientError {
    #[error("could not connect to sysd socket at {path}: {source}")]
    Connect { path: String, #[source] source: std::io::Error },
    #[error("sysd transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sysd returned malformed response: {0}")]
    Decode(String),
    #[error("sysd call timed out after {0:?}")]
    Timeout(Duration),
    #[error("sysd rejected the call: kind={kind} message={message}")]
    Rejected { kind: String, message: String },
    #[error("sysd indicated success but omitted response data")]
    MissingData,
}

impl From<SysdError> for SysdClientError {
    fn from(e: SysdError) -> Self {
        SysdClientError::Rejected { kind: e.kind, message: e.message }
    }
}

/// Client for the `duduclaw-sysd` UDS. Stateless beyond the socket path +
/// default timeout — a new connection is opened per call (call volume is
/// human-triggered device operations, not a hot path).
#[derive(Debug, Clone)]
pub struct SysdClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl SysdClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path, timeout: Duration::from_secs(30) }
    }

    /// Build from the shared env-aware resolver
    /// ([`crate::protocol::resolve_socket_path`]) so callers don't have to
    /// duplicate the `DUDUCLAW_SYSD_SOCKET` override logic.
    pub fn from_env() -> Self {
        Self::new(crate::protocol::resolve_socket_path())
    }

    /// Override the per-call timeout (default 30s — generous enough for
    /// `systemd-sysupdate update` to at least be accepted and start; the
    /// verbs that tear down the machine, `reboot`/`poweroff`/the reboot
    /// half of `factory_reset`, may never actually send a response at all
    /// if the box goes down mid-write, which callers must treat as a
    /// plausible success, not a hard failure — see `DeviceOps` callers).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    pub async fn call(&self, req: &SysdRequest) -> Result<SysdOpOutput, SysdClientError> {
        let resp = self.call_raw(req).await?;
        if resp.ok {
            resp.data.ok_or(SysdClientError::MissingData)
        } else {
            Err(resp
                .error
                .map(SysdClientError::from)
                .unwrap_or_else(|| SysdClientError::Decode("error envelope missing `error`".into())))
        }
    }

    /// Same as [`Self::call`] but returns the full envelope (including
    /// `audit_id`) even on rejection — useful for callers that want to
    /// surface the audit id alongside a failure, not just on success.
    ///
    /// Non-unix targets have no UDS: every call fails closed with the same
    /// `Connect` variant the "socket missing" path produces on unix, so
    /// callers' error handling stays uniform across platforms (the v1.62.0
    /// Windows release-CI break was this transport compiling ungated).
    #[cfg(not(unix))]
    pub async fn call_raw(&self, _req: &SysdRequest) -> Result<SysdResponse, SysdClientError> {
        Err(SysdClientError::Connect {
            path: self.socket_path.display().to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "duduclaw-sysd requires Unix domain sockets — it only runs on the Linux appliance image",
            ),
        })
    }

    /// See the non-unix twin above for why this is platform-split.
    #[cfg(unix)]
    pub async fn call_raw(&self, req: &SysdRequest) -> Result<SysdResponse, SysdClientError> {
        let connect = tokio::time::timeout(self.timeout, UnixStream::connect(&self.socket_path))
            .await
            .map_err(|_| SysdClientError::Timeout(self.timeout))?;
        let stream = connect.map_err(|e| SysdClientError::Connect {
            path: self.socket_path.display().to_string(),
            source: e,
        })?;

        let (reader, mut writer) = stream.into_split();
        let mut line = serde_json::to_string(req)
            .map_err(|e| SysdClientError::Decode(format!("failed to encode request: {e}")))?;
        line.push('\n');

        tokio::time::timeout(self.timeout, async {
            writer.write_all(line.as_bytes()).await?;
            writer.shutdown().await
        })
        .await
        .map_err(|_| SysdClientError::Timeout(self.timeout))??;

        let mut reader = BufReader::new(reader);
        let mut resp_line = String::new();
        tokio::time::timeout(self.timeout, reader.read_line(&mut resp_line))
            .await
            .map_err(|_| SysdClientError::Timeout(self.timeout))??;

        if resp_line.trim().is_empty() {
            return Err(SysdClientError::Decode("empty response".into()));
        }
        serde_json::from_str(resp_line.trim()).map_err(|e| SysdClientError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_uses_default_when_unset() {
        // Best-effort: only assert when the env var is genuinely unset in
        // this test process (parallel tests in this crate never set
        // `DUDUCLAW_SYSD_SOCKET` outside protocol.rs's own guarded test).
        if std::env::var_os(crate::protocol::SOCKET_PATH_ENV).is_none() {
            let client = SysdClient::from_env();
            assert_eq!(client.socket_path(), std::path::Path::new(crate::protocol::DEFAULT_SOCKET_PATH));
        }
    }

    #[test]
    fn with_timeout_overrides_default() {
        let client = SysdClient::new(PathBuf::from("/tmp/x.sock")).with_timeout(Duration::from_secs(5));
        assert_eq!(client.timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn connect_to_nonexistent_socket_is_a_connect_error() {
        let client = SysdClient::new(PathBuf::from("/tmp/duduclaw-sysd-test-does-not-exist.sock"));
        let result = client.call(&SysdRequest::Reboot).await;
        assert!(matches!(result, Err(SysdClientError::Connect { .. })));
    }
}
