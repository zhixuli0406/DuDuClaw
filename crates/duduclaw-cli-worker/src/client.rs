//! Phase 6.3 — reqwest-backed client for the worker IPC.
//!
//! Living in the worker crate (not the gateway) so the client and server
//! share one source of truth for the protocol. The gateway depends on
//! this crate and re-exports [`WorkerClient`].

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

use crate::protocol::{
    HEALTHZ_PATH, InvokeParams, RPC_PATH, Request, Response, RpcError, ShutdownSessionParams,
    StatsResult,
};

/// Client SDK for the worker's HTTP+JSON-RPC surface. Owns a long-lived
/// `reqwest::Client` (connection pool) and the bearer token.
#[derive(Debug, Clone)]
pub struct WorkerClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("worker HTTP transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("worker returned non-success status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("worker error: kind={kind} message={message}")]
    Worker { kind: String, message: String },
    #[error("worker returned malformed JSON: {0}")]
    Decode(String),
    #[error("worker indicated success but omitted `data`")]
    MissingData,
}

impl From<&RpcError> for ClientError {
    fn from(e: &RpcError) -> Self {
        ClientError::Worker {
            kind: e.kind.clone(),
            message: e.message.clone(),
        }
    }
}

impl WorkerClient {
    /// Build a client pointed at `base_url` (e.g. `"http://127.0.0.1:9876"`).
    /// The HTTP client uses a 5-second connect timeout + caller-controlled
    /// per-request timeout via [`Self::invoke`]'s `timeout_ms`.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(4)
            .build()?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Liveness ping. Returns Ok on `200 OK` from `GET /healthz`.
    pub async fn healthz(&self, timeout: Duration) -> Result<(), ClientError> {
        let url = format!("{}{}", self.base_url, HEALTHZ_PATH);
        let resp = self.http.get(&url).timeout(timeout).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }

    /// Invoke a CLI turn through the worker. The outer Duration also acts
    /// as the HTTP request timeout (+ 5s slack to let the worker emit its
    /// own timeout error rather than us cutting off).
    pub async fn invoke(
        &self,
        params: InvokeParams,
        deadline: Duration,
    ) -> Result<String, ClientError> {
        let body: Value = self
            .send(&Request::Invoke(params), deadline + Duration::from_secs(5))
            .await?;
        let text = body
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ClientError::Decode("invoke data missing `text` field".into()))?
            .to_string();
        Ok(text)
    }

    /// Force-evict a pooled session by key.
    pub async fn shutdown_session(
        &self,
        params: ShutdownSessionParams,
    ) -> Result<bool, ClientError> {
        let body: Value = self
            .send(
                &Request::ShutdownSession(params),
                Duration::from_secs(15),
            )
            .await?;
        Ok(body
            .get("shutdown")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }

    pub async fn stats(&self) -> Result<StatsResult, ClientError> {
        self.send_typed(&Request::Stats, Duration::from_secs(5)).await
    }

    pub async fn health(&self) -> Result<Value, ClientError> {
        self.send(&Request::Health, Duration::from_secs(5)).await
    }

    async fn send_typed<T: DeserializeOwned>(
        &self,
        req: &Request,
        timeout: Duration,
    ) -> Result<T, ClientError> {
        let value = self.send(req, timeout).await?;
        serde_json::from_value::<T>(value).map_err(|e| ClientError::Decode(e.to_string()))
    }

    async fn send(&self, req: &Request, timeout: Duration) -> Result<Value, ClientError> {
        let url = format!("{}{}", self.base_url, RPC_PATH);
        let resp = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .json(req)
            .send()
            .await?;
        let status = resp.status();
        let body_bytes = resp.bytes().await?;

        // Parse as our Response envelope. If status is non-2xx but the
        // body is a well-formed error envelope, prefer the envelope's
        // semantic error over a generic status error.
        let parsed: Response<Value> = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                let snippet =
                    String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(400)]).to_string();
                warn!(status = %status, %snippet, "worker returned non-JSON body");
                if !status.is_success() {
                    return Err(ClientError::Status {
                        status: status.as_u16(),
                        body: snippet,
                    });
                }
                return Err(ClientError::Decode(e.to_string()));
            }
        };

        if parsed.ok {
            parsed
                .data
                .ok_or(ClientError::MissingData)
        } else {
            let err = parsed
                .error
                .ok_or_else(|| ClientError::Decode("error envelope missing `error` field".into()))?;
            Err(ClientError::from(&err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_strips_trailing_slash() {
        let c = WorkerClient::new("http://127.0.0.1:9876/", "tok").unwrap();
        assert_eq!(c.base_url(), "http://127.0.0.1:9876");
    }

    #[test]
    fn new_accepts_no_trailing_slash() {
        let c = WorkerClient::new("http://127.0.0.1:9876", "tok").unwrap();
        assert_eq!(c.base_url(), "http://127.0.0.1:9876");
    }

    #[test]
    fn client_error_from_rpc_error_carries_kind_and_message() {
        let rpc = RpcError::invoke_failed("boom");
        let client_err: ClientError = (&rpc).into();
        match client_err {
            ClientError::Worker { kind, message } => {
                assert_eq!(kind, "invoke_failed");
                assert_eq!(message, "boom");
            }
            other => panic!("expected Worker variant, got {other:?}"),
        }
    }
}
