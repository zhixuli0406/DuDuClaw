//! 1Password Connect adapter (read-oriented pull model).
//!
//! Resolves `secret://onepassword/<item>` against a self-hosted 1Password
//! Connect server. `<item>` is matched against item titles in the configured
//! vault; the returned value is the item's concealed credential field
//! (`purpose == "PASSWORD"`, else a field labelled `credential`/`password`,
//! else the first non-empty field value).
//!
//! Only `get`/`exists` are meaningful for the pull model — 1Password Connect
//! tokens are typically read-scoped — so `put`/`delete` return an explicit
//! unsupported error rather than silently succeeding (fail-closed).
//!
//! The URL construction and the field-extraction parser are pure functions with
//! unit tests; the live HTTP round-trip requires a running Connect server and
//! is verified against one at deploy time (PENDING-LIVE).

use super::SecretManager;
use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;

pub struct OnePasswordConnectAdapter {
    client: Client,
    host: String,
    token: String,
    vault: String,
}

impl OnePasswordConnectAdapter {
    pub fn new(
        host: impl Into<String>,
        token: impl Into<String>,
        vault: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            host: host.into().trim_end_matches('/').to_string(),
            token: token.into(),
            vault: vault.into(),
        }
    }

    /// Item-list URL (the title filter is applied as a query param by the
    /// caller via `filter=title eq "<x>"`, which reqwest encodes).
    fn items_url(&self) -> String {
        format!("{}/v1/vaults/{}/items", self.host, self.vault)
    }

    fn item_url(&self, item_id: &str) -> String {
        format!("{}/v1/vaults/{}/items/{}", self.host, self.vault, item_id)
    }

    /// Resolve an item title to its id via the filtered list endpoint.
    async fn resolve_item_id(&self, title: &str) -> Result<Option<String>> {
        let url = self.items_url();
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .query(&[("filter", format!("title eq \"{title}\""))])
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("1password list request failed: {e}")))?;
        match resp.status() {
            StatusCode::OK => {
                let body: Value = resp.json().await.map_err(|e| {
                    DuDuClawError::Security(format!("1password list parse error: {e}"))
                })?;
                Ok(first_item_id(&body))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(DuDuClawError::Security(
                "1password authentication failed listing items".to_string(),
            )),
            status => Err(DuDuClawError::Security(format!(
                "1password returned {status} listing items"
            ))),
        }
    }
}

/// Extract the first item id from a Connect list response (array of items).
fn first_item_id(body: &Value) -> Option<String> {
    body.as_array()
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

/// Pick the credential value out of a full 1Password item document.
///
/// Preference order: a field whose `purpose == "PASSWORD"`, then a field whose
/// `label`/`id` is `credential` or `password`, then the first field with a
/// non-empty `value`. Returns an error if no field carries a value.
fn credential_from_item(body: &Value, name: &str) -> Result<String> {
    let fields = body
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| DuDuClawError::Security(format!("1password item '{name}' has no fields")))?;

    // 1. purpose == PASSWORD
    if let Some(v) = fields
        .iter()
        .find(|f| f.get("purpose").and_then(|p| p.as_str()) == Some("PASSWORD"))
        .and_then(|f| f.get("value"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
    {
        return Ok(v.to_string());
    }

    // 2. labelled credential/password
    if let Some(v) = fields
        .iter()
        .find(|f| {
            let label = f
                .get("label")
                .and_then(|l| l.as_str())
                .or_else(|| f.get("id").and_then(|l| l.as_str()))
                .unwrap_or("")
                .to_ascii_lowercase();
            label == "credential" || label == "password"
        })
        .and_then(|f| f.get("value"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
    {
        return Ok(v.to_string());
    }

    // 3. first non-empty value
    fields
        .iter()
        .filter_map(|f| f.get("value").and_then(|v| v.as_str()))
        .find(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            DuDuClawError::Security(format!("1password item '{name}' has no field value"))
        })
}

#[async_trait]
impl SecretManager for OnePasswordConnectAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        let item_id = self
            .resolve_item_id(name)
            .await?
            .ok_or_else(|| DuDuClawError::Security(format!("1password item not found: {name}")))?;
        let url = self.item_url(&item_id);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| DuDuClawError::Security(format!("1password get request failed: {e}")))?;
        match resp.status() {
            StatusCode::OK => {
                let body: Value = resp.json().await.map_err(|e| {
                    DuDuClawError::Security(format!("1password item parse error: {e}"))
                })?;
                credential_from_item(&body, name)
            }
            StatusCode::NOT_FOUND => Err(DuDuClawError::Security(format!(
                "1password item not found: {name}"
            ))),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(DuDuClawError::Security(
                format!("1password authentication failed for item '{name}'"),
            )),
            status => Err(DuDuClawError::Security(format!(
                "1password returned {status} for item '{name}'"
            ))),
        }
    }

    async fn put(&self, _name: &str, _value: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "1password Connect adapter is read-only (put not supported)".to_string(),
        ))
    }

    async fn delete(&self, _name: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "1password Connect adapter is read-only (delete not supported)".to_string(),
        ))
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.resolve_item_id(name).await?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urls_are_well_formed_and_trim_trailing_slash() {
        let a = OnePasswordConnectAdapter::new("https://op.example.com/", "t", "vaultA");
        assert_eq!(
            a.item_url("abc"),
            "https://op.example.com/v1/vaults/vaultA/items/abc"
        );
        assert_eq!(
            a.items_url(),
            "https://op.example.com/v1/vaults/vaultA/items"
        );
    }

    #[test]
    fn first_item_id_from_list() {
        let body = json!([{ "id": "item1", "title": "GH" }, { "id": "item2" }]);
        assert_eq!(first_item_id(&body).as_deref(), Some("item1"));
        assert_eq!(first_item_id(&json!([])), None);
        assert_eq!(first_item_id(&json!({})), None);
    }

    #[test]
    fn credential_prefers_password_purpose() {
        let body = json!({
            "fields": [
                { "label": "username", "value": "alice" },
                { "purpose": "PASSWORD", "value": "s3cret" }
            ]
        });
        assert_eq!(credential_from_item(&body, "x").unwrap(), "s3cret");
    }

    #[test]
    fn credential_falls_back_to_labelled_then_first_value() {
        let labelled = json!({ "fields": [
            { "label": "note", "value": "" },
            { "label": "credential", "value": "tok_abc" }
        ]});
        assert_eq!(credential_from_item(&labelled, "x").unwrap(), "tok_abc");

        let first = json!({ "fields": [
            { "label": "anything", "value": "firstval" }
        ]});
        assert_eq!(credential_from_item(&first, "x").unwrap(), "firstval");
    }

    #[test]
    fn credential_errors_when_no_value() {
        let empty = json!({ "fields": [ { "label": "x", "value": "" } ] });
        assert!(credential_from_item(&empty, "x").is_err());
        assert!(credential_from_item(&json!({}), "x").is_err());
    }
}
