//! Infisical adapter (read-oriented pull model).
//!
//! Resolves `secret://infisical/<name>` against an Infisical instance via the
//! raw-secret endpoint `GET /api/v3/secrets/raw/<name>` (scoped by workspace id
//! + environment), returning `secret.secretValue`.
//!
//! Like the 1Password adapter this is read-focused: `put`/`delete` return an
//! explicit unsupported error (fail-closed) since the pull model only needs
//! `get`. URL construction and the response parser are pure + unit-tested; the
//! live round-trip needs a running Infisical instance (PENDING-LIVE).

use super::SecretManager;
use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;

pub struct InfisicalAdapter {
    client: Client,
    addr: String,
    token: String,
    project_id: String,
    environment: String,
}

impl InfisicalAdapter {
    pub fn new(
        addr: impl Into<String>,
        token: impl Into<String>,
        project_id: impl Into<String>,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            addr: addr.into().trim_end_matches('/').to_string(),
            token: token.into(),
            project_id: project_id.into(),
            environment: environment.into(),
        }
    }

    fn raw_secret_url(&self, name: &str) -> String {
        format!("{}/api/v3/secrets/raw/{}", self.addr, name)
    }
}

/// Extract `secret.secretValue` from an Infisical raw-secret response.
fn secret_value_from_json(body: &Value, name: &str) -> Result<String> {
    body.pointer("/secret/secretValue")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            DuDuClawError::Security(format!(
                "infisical secret '{name}' missing secret.secretValue field"
            ))
        })
}

#[async_trait]
impl SecretManager for InfisicalAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        let url = self.raw_secret_url(name);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .query(&[
                ("workspaceId", self.project_id.as_str()),
                ("environment", self.environment.as_str()),
            ])
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("infisical request failed: {e}")))?;
        match resp.status() {
            StatusCode::OK => {
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|e| DuDuClawError::Security(format!("infisical parse error: {e}")))?;
                secret_value_from_json(&body, name)
            }
            StatusCode::NOT_FOUND => Err(DuDuClawError::Security(format!(
                "infisical secret not found: {name}"
            ))),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(DuDuClawError::Security(
                format!("infisical authentication failed for secret '{name}'"),
            )),
            status => Err(DuDuClawError::Security(format!(
                "infisical returned {status} for secret '{name}'"
            ))),
        }
    }

    async fn put(&self, _name: &str, _value: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "infisical adapter is read-only (put not supported)".to_string(),
        ))
    }

    async fn delete(&self, _name: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "infisical adapter is read-only (delete not supported)".to_string(),
        ))
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        // Classify the HTTP status directly rather than folding every `get`
        // error into `Ok(false)`: a genuine 404 ⇒ `false`, but auth/network
        // failures must PROPAGATE so callers can distinguish "absent" from
        // "misconfigured" (matches the fail-closed 1Password `exists`).
        let url = self.raw_secret_url(name);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .query(&[
                ("workspaceId", self.project_id.as_str()),
                ("environment", self.environment.as_str()),
            ])
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("infisical request failed: {e}")))?;
        match resp.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(DuDuClawError::Security(
                format!("infisical authentication failed checking '{name}'"),
            )),
            status => Err(DuDuClawError::Security(format!(
                "infisical returned {status} checking '{name}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn url_is_well_formed() {
        let a = InfisicalAdapter::new("https://infisical.example.com/", "t", "proj", "prod");
        assert_eq!(
            a.raw_secret_url("DB_PASSWORD"),
            "https://infisical.example.com/api/v3/secrets/raw/DB_PASSWORD"
        );
    }

    #[test]
    fn parses_secret_value() {
        let body = json!({ "secret": { "secretKey": "DB", "secretValue": "hunter2" } });
        assert_eq!(secret_value_from_json(&body, "DB").unwrap(), "hunter2");
    }

    #[test]
    fn missing_value_errors() {
        assert!(secret_value_from_json(&json!({ "secret": {} }), "x").is_err());
        assert!(secret_value_from_json(&json!({}), "x").is_err());
    }
}
