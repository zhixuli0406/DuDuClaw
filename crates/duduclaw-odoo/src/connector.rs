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
/// Covers system internals, PII-heavy tables, financial records, and communications.
const BLOCKED_MODELS: &[&str] = &[
    // System internals & schema
    "ir.config_parameter",
    "ir.cron",
    "ir.actions.server",
    "ir.rule",
    "ir.model.access",
    "ir.model",
    "ir.model.fields",
    "ir.attachment",
    "base.automation",
    "fetchmail.server",
    "ir.mail_server",
    // Authentication & access
    "res.users",
    "res.groups",
    // PII — contacts & banking
    "res.partner",
    "res.partner.bank",
    // Financial records
    "account.move",
    "account.move.line",
    "account.payment",
    "payment.token",
    // HR — employee PII & payroll
    "hr.employee",
    "hr.employee.private",
    "hr.payslip",
    // Communications
    "mail.message",
    "mail.channel",
];

/// Merge a multi-company scope into an ORM `kwargs` value's `context` map
/// (M18 / RFC-21 §2).
///
/// Injects `allowed_company_ids` (the full company switch board) and
/// `company_id` (the active company — first in the list). A no-op when
/// `company_ids` is empty. Caller-supplied `allowed_company_ids` /
/// `company_id` keys are preserved so an explicit per-call override beats the
/// connector-wide default. A non-object `kwargs` or `context` is coerced into
/// an object first so a malformed value can never silently drop the scope.
fn merge_company_context(company_ids: &[i64], mut kwargs: Value) -> Value {
    if company_ids.is_empty() {
        return kwargs;
    }
    if !kwargs.is_object() {
        kwargs = json!({});
    }
    let obj = kwargs.as_object_mut().expect("kwargs coerced to object");
    let ctx = obj.entry("context").or_insert_with(|| json!({}));
    if !ctx.is_object() {
        *ctx = json!({});
    }
    let ctx_obj = ctx.as_object_mut().expect("context coerced to object");
    ctx_obj
        .entry("allowed_company_ids")
        .or_insert_with(|| json!(company_ids));
    ctx_obj
        .entry("company_id")
        .or_insert_with(|| json!(company_ids[0]));
    kwargs
}

/// Interpret an Odoo `write` RPC result (M54).
///
/// Odoo's `write` returns a JSON boolean. Any other shape (null, number,
/// object) means the call did not behave like a write — treat it as an error
/// instead of silently reporting `false`-as-success, which would hide an
/// unapplied write.
fn interpret_write_result(result: &Value) -> Result<bool, String> {
    result
        .as_bool()
        .ok_or_else(|| format!("Unexpected write response: {result}"))
}

/// Interpret an Odoo `search_count` RPC result (M54).
///
/// `search_count` returns an integer; a missing/unexpected shape would
/// otherwise masquerade as a legitimate count of 0.
fn interpret_count_result(result: &Value) -> Result<i64, String> {
    result
        .as_i64()
        .ok_or_else(|| format!("Unexpected search_count response: {result}"))
}

/// Main Odoo connection handle.
pub struct OdooConnector {
    pub url: String,
    pub db: String,
    pub uid: Option<i64>,
    credential: String,
    pub edition_gate: EditionGate,
    http: reqwest::Client,
    /// Multi-company scope (RFC-21 §2). When non-empty, every RPC call is
    /// scoped to these `res.company` ids via the Odoo ORM context
    /// (`allowed_company_ids` + `company_id`). Empty ⇒ inherit the Odoo
    /// user's default companies (no scoping).
    company_ids: Vec<i64>,
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

        // SSRF hardening (HS9): the SSRF validator only checks the initial
        // URL, so following redirects would let a validated public host 302
        // to cloud-metadata (169.254.169.254) or internal addresses. Odoo
        // JSON-RPC never needs redirects, so disable them entirely.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;

        // Authenticate with exponential backoff retry (MW-H5)
        let mut uid = None;
        let retries = [100, 500, 1000]; // ms
        for (attempt, delay_ms) in retries.iter().enumerate() {
            match rpc::authenticate(&http, &config.url, &config.db, &config.username, credential).await {
                Ok(u) => { uid = Some(u); break; }
                Err(e) if attempt < retries.len() - 1 => {
                    warn!(attempt = attempt + 1, "Odoo auth failed, retrying: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                }
                Err(e) => return Err(format!("Odoo authentication failed after {} attempts: {e}", retries.len())),
            }
        }
        let uid = uid.unwrap();

        info!(url = %config.url, db = %config.db, uid, "Odoo connected");

        let mut conn = Self {
            url: config.url.clone(),
            db: config.db.clone(),
            uid: Some(uid),
            credential: credential.to_string(),
            edition_gate: EditionGate::unknown(),
            http,
            company_ids: Vec::new(),
        };

        // Detect edition
        conn.edition_gate = EditionGate::detect(&conn).await.unwrap_or_else(|e| {
            warn!("Edition detection failed: {e}");
            EditionGate::unknown()
        });

        Ok(conn)
    }

    /// Scope this connector to a set of `res.company` ids (RFC-21 §2 / M18).
    ///
    /// When non-empty, [`execute_kw`](Self::execute_kw) injects
    /// `allowed_company_ids` (the multi-company switch board) and
    /// `company_id` (the active company, first in the list) into every ORM
    /// call's context so cross-company isolation is actually enforced.
    /// Builder-style so callers can write `conn.with_company_ids(ids)`.
    pub fn with_company_ids(mut self, company_ids: Vec<i64>) -> Self {
        self.company_ids = company_ids;
        self
    }

    /// Currently configured multi-company scope (may be empty).
    pub fn company_ids(&self) -> &[i64] {
        &self.company_ids
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
        // M18: scope every ORM call to the agent's allowed companies.
        let kwargs = merge_company_context(&self.company_ids, kwargs);
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
        interpret_write_result(&result)
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
        interpret_count_result(&result)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── M18: company-scope context injection ──────────────────────────────

    #[test]
    fn merge_company_context_noop_when_empty() {
        let kwargs = json!({"fields": ["id"]});
        let out = merge_company_context(&[], kwargs.clone());
        assert_eq!(out, kwargs, "empty scope must not touch kwargs");
    }

    #[test]
    fn merge_company_context_injects_into_empty_kwargs() {
        let out = merge_company_context(&[1, 2], json!({}));
        let ctx = &out["context"];
        assert_eq!(ctx["allowed_company_ids"], json!([1, 2]));
        assert_eq!(ctx["company_id"], json!(1), "active company is first id");
    }

    #[test]
    fn merge_company_context_preserves_existing_context_keys() {
        let kwargs = json!({"context": {"lang": "zh_TW"}, "limit": 5});
        let out = merge_company_context(&[7], kwargs);
        assert_eq!(out["context"]["lang"], "zh_TW", "existing keys kept");
        assert_eq!(out["context"]["allowed_company_ids"], json!([7]));
        assert_eq!(out["context"]["company_id"], json!(7));
        assert_eq!(out["limit"], 5);
    }

    #[test]
    fn merge_company_context_does_not_override_explicit_company() {
        // An explicit per-call company scope must win over the default.
        let kwargs = json!({"context": {"company_id": 99, "allowed_company_ids": [99]}});
        let out = merge_company_context(&[1, 2], kwargs);
        assert_eq!(out["context"]["company_id"], json!(99));
        assert_eq!(out["context"]["allowed_company_ids"], json!([99]));
    }

    #[test]
    fn merge_company_context_coerces_non_object_inputs() {
        // Defensive: a malformed kwargs / context must not drop the scope.
        let out = merge_company_context(&[3], json!("garbage"));
        assert_eq!(out["context"]["company_id"], json!(3));
        let out2 = merge_company_context(&[3], json!({"context": 42}));
        assert_eq!(out2["context"]["company_id"], json!(3));
    }

    // ── M54: result-shape interpretation ─────────────────────────────────

    #[test]
    fn interpret_write_result_accepts_booleans() {
        assert_eq!(interpret_write_result(&json!(true)), Ok(true));
        assert_eq!(interpret_write_result(&json!(false)), Ok(false));
    }

    #[test]
    fn interpret_write_result_rejects_non_bool() {
        // null / number / object are all unexpected and must error, not
        // collapse to a silent `false`.
        assert!(interpret_write_result(&Value::Null).is_err());
        assert!(interpret_write_result(&json!(1)).is_err());
        assert!(interpret_write_result(&json!({"ok": true})).is_err());
    }

    #[test]
    fn interpret_count_result_accepts_integers() {
        assert_eq!(interpret_count_result(&json!(0)), Ok(0));
        assert_eq!(interpret_count_result(&json!(42)), Ok(42));
    }

    #[test]
    fn interpret_count_result_rejects_non_integer() {
        assert!(interpret_count_result(&Value::Null).is_err());
        assert!(interpret_count_result(&json!(false)).is_err());
        assert!(interpret_count_result(&json!("3")).is_err());
    }
}
