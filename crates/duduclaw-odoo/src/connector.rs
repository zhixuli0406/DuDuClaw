//! OdooConnector — connection pool, authentication, and retry logic.
//!
//! [O-1a] Core connector with dual-protocol support and cached uid.

use std::time::Duration;

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::OdooConfig;
use crate::edition::EditionGate;
use crate::rpc;

/// Sensitive models that are never allowed via generic search/execute.
const BLOCKED_MODELS: &[&str] = &[
    "ir.config_parameter",
    "res.users",
    "ir.cron",
    "ir.actions.server",
    "ir.rule",
    "ir.model.access",
    "base.automation",
    "fetchmail.server",
    "ir.mail_server",
];

/// Main Odoo connection handle.
pub struct OdooConnector {
    pub url: String,
    pub db: String,
    pub uid: Option<i64>,
    credential: String,
    pub edition_gate: EditionGate,
    http: reqwest::Client,
}

/// Connection status for monitoring.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OdooStatus {
    pub connected: bool,
    pub url: String,
    pub db: String,
    pub edition: String,
    pub version: String,
    pub uid: Option<i64>,
    pub ee_modules: Vec<String>,
}

impl OdooConnector {
    /// Connect to Odoo, authenticate, and detect edition.
    pub async fn connect(config: &OdooConfig, credential: &str) -> Result<Self, String> {
        if !config.is_configured() {
            return Err("Odoo not configured: url and db are required".to_string());
        }

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;

        // Authenticate
        let uid = rpc::authenticate(&http, &config.url, &config.db, &config.username, credential)
            .await?;

        info!(url = %config.url, db = %config.db, uid, "Odoo connected");

        let mut conn = Self {
            url: config.url.clone(),
            db: config.db.clone(),
            uid: Some(uid),
            credential: credential.to_string(),
            edition_gate: EditionGate::unknown(),
            http,
        };

        // Detect edition
        conn.edition_gate = EditionGate::detect(&conn).await.unwrap_or_else(|e| {
            warn!("Edition detection failed: {e}");
            EditionGate::unknown()
        });

        Ok(conn)
    }

    /// Get Odoo server version.
    pub async fn version(&self) -> Result<Value, String> {
        rpc::version(&self.http, &self.url).await
    }

    /// Execute an ORM method.
    pub async fn execute_kw(
        &self,
        model: &str,
        method: &str,
        args: Vec<Value>,
        kwargs: Value,
    ) -> Result<Value, String> {
        let uid = self.uid.ok_or("Not authenticated")?;
        rpc::execute_kw(
            &self.http,
            &self.url,
            &self.db,
            uid,
            &self.credential,
            model,
            method,
            args,
            kwargs,
        )
        .await
    }

    /// Convenience: search_read with domain, fields, limit.
    pub async fn search_read(
        &self,
        model: &str,
        domain: Vec<Value>,
        fields: &[&str],
        limit: usize,
    ) -> Result<Value, String> {
        self.execute_kw(
            model,
            "search_read",
            vec![json!(domain)],
            json!({
                "fields": fields,
                "limit": limit,
                "context": {"lang": "zh_TW"},
            }),
        )
        .await
    }

    /// Convenience: create a record.
    pub async fn create(
        &self,
        model: &str,
        values: Value,
    ) -> Result<i64, String> {
        let result = self
            .execute_kw(model, "create", vec![json!([values])], json!({}))
            .await?;
        // create returns an array of IDs
        result
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_i64())
            .or_else(|| result.as_i64())
            .ok_or_else(|| format!("Unexpected create response: {result}"))
    }

    /// Convenience: write (update) records.
    pub async fn write(
        &self,
        model: &str,
        ids: &[i64],
        values: Value,
    ) -> Result<bool, String> {
        let result = self
            .execute_kw(model, "write", vec![json!(ids), values], json!({}))
            .await?;
        Ok(result.as_bool().unwrap_or(false))
    }

    /// Convenience: search_count.
    pub async fn count(
        &self,
        model: &str,
        domain: Vec<Value>,
    ) -> Result<i64, String> {
        let result = self
            .execute_kw(model, "search_count", vec![json!(domain)], json!({}))
            .await?;
        Ok(result.as_i64().unwrap_or(0))
    }

    /// Check if a model is in the blocked list.
    pub fn is_model_blocked(model: &str) -> bool {
        BLOCKED_MODELS.contains(&model)
    }

    /// Get connection status for monitoring.
    pub fn status(&self) -> OdooStatus {
        OdooStatus {
            connected: self.uid.is_some(),
            url: self.url.clone(),
            db: self.db.clone(),
            edition: format!("{:?}", self.edition_gate.edition),
            version: self.edition_gate.odoo_version.clone(),
            uid: self.uid,
            ee_modules: self.edition_gate.installed_modules.iter().cloned().collect(),
        }
    }
}
