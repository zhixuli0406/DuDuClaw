//! HashiCorp Vault KV v2 HTTP adapter (ADR-004).
//!
//! Communicates with a Vault server over its HTTP API v1.
//!
//! ## KV v2 API paths used
//!
//! | Operation | Method | Path                                    |
//! |-----------|--------|-----------------------------------------|
//! | Read      | GET    | `/v1/<mount>/data/<name>`               |
//! | Write     | POST   | `/v1/<mount>/data/<name>`               |
//! | Delete    | DELETE | `/v1/<mount>/data/<name>`               |
//! | Exists    | GET    | `/v1/<mount>/metadata/<name>`           |
//!
//! Authentication is via `X-Vault-Token` header.

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::SecretManager;

/// HashiCorp Vault KV v2 adapter.
pub struct VaultHttpAdapter {
    client: Client,
    addr: String,
    token: String,
    mount: String,
}

impl VaultHttpAdapter {
    /// Create a new adapter.
    ///
    /// - `addr`  — Vault server base URL, e.g. `http://127.0.0.1:8200`
    /// - `token` — Vault token with read/write/delete permissions on `<mount>/data/*`
    /// - `mount` — KV v2 mount point (typically `"secret"`)
    pub fn new(addr: impl Into<String>, token: impl Into<String>, mount: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            addr: addr.into().trim_end_matches('/').to_string(),
            token: token.into(),
            mount: mount.into().trim_matches('/').to_string(),
        }
    }

    fn data_url(&self, name: &str) -> String {
        format!("{}/v1/{}/data/{}", self.addr, self.mount, name)
    }

    fn metadata_url(&self, name: &str) -> String {
        format!("{}/v1/{}/metadata/{}", self.addr, self.mount, name)
    }
}

#[async_trait]
impl SecretManager for VaultHttpAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        let url = self.data_url(name);
        debug!(url = %url, "vault get secret");

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("vault request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK => {
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|e| DuDuClawError::Security(format!("vault response parse error: {e}")))?;

                // KV v2 response: {"data": {"data": {"<key>": "<value>"}}}
                // For simplicity we store secrets as a single key `"value"`.
                body.pointer("/data/data/value")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        DuDuClawError::Security(format!(
                            "vault secret '{name}' missing 'value' field"
                        ))
                    })
            }
            StatusCode::NOT_FOUND => Err(DuDuClawError::Security(format!(
                "vault secret not found: {name}"
            ))),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(DuDuClawError::Security(
                format!("vault authentication failed for secret '{name}'"),
            )),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(DuDuClawError::Security(format!(
                    "vault returned {status} for secret '{name}': {body}"
                )))
            }
        }
    }

    async fn put(&self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() {
            return Err(DuDuClawError::Security(
                "secret name must not be empty".to_string(),
            ));
        }
        if value.is_empty() {
            return Err(DuDuClawError::Security(
                "secret value must not be empty".to_string(),
            ));
        }

        let url = self.data_url(name);
        debug!(url = %url, "vault put secret");

        // KV v2 write: POST with {"data": {"value": "<secret>"}}
        let payload = json!({ "data": { "value": value } });

        let resp = self
            .client
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("vault put request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::CREATED => {
                debug!(secret_name = name, "vault secret written");
                Ok(())
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(DuDuClawError::Security(
                format!("vault authentication failed writing secret '{name}'"),
            )),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(DuDuClawError::Security(format!(
                    "vault returned {status} writing secret '{name}': {body}"
                )))
            }
        }
    }

    async fn delete(&self, name: &str) -> Result<()> {
        // Check existence first so we can return a meaningful error.
        if !self.exists(name).await? {
            return Err(DuDuClawError::Security(format!(
                "vault secret not found for deletion: {name}"
            )));
        }

        let url = self.data_url(name);
        debug!(url = %url, "vault delete secret");

        let resp = self
            .client
            .delete(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("vault delete request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                debug!(secret_name = name, "vault secret deleted");
                Ok(())
            }
            StatusCode::NOT_FOUND => Err(DuDuClawError::Security(format!(
                "vault secret not found: {name}"
            ))),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(DuDuClawError::Security(
                format!("vault authentication failed deleting secret '{name}'"),
            )),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(DuDuClawError::Security(format!(
                    "vault returned {status} deleting secret '{name}': {body}"
                )))
            }
        }
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        let url = self.metadata_url(name);

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("vault metadata request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                warn!(
                    secret_name = name,
                    "vault authentication failed checking existence"
                );
                Ok(false)
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(DuDuClawError::Security(format!(
                    "vault returned {status} checking existence of '{name}': {body}"
                )))
            }
        }
    }
}
