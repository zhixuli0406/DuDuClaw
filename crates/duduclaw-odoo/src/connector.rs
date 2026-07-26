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

/// Non-sensitive `res.partner` columns exposed by the safe customer-search
/// tool. Deliberately excludes bank (`res.partner.bank`), tax
/// (`vat`/`property_*`), and credit fields — only enough to identify a partner
/// and reach them. Any change here is a data-exposure decision.
pub const PARTNER_SEARCH_FIELDS: &[&str] = &[
    "id",
    "name",
    "email",
    "phone",
    "mobile",
    "city",
    "country_id",
    "is_company",
    "parent_id",
    "ref",
];

/// System models that stay **read-only** even after an operator opts them out
/// of the block list via `unblock_models`. Schema introspection needs to *read*
/// `ir.model` / `ir.model.fields`; attachments may be *read* for context — but a
/// generic `write`/`unlink`/`create` against these is refused unconditionally
/// (fail closed). Unblocking never grants mutation of Odoo's own metadata tables.
const READ_ONLY_WHEN_UNBLOCKED: &[&str] = &[
    "ir.model",
    "ir.model.fields",
    "ir.attachment",
];

/// Coarse access class used by the block-list gate. `Read` covers
/// `search` / `read` / `fields_get` and friends; `Mutate` covers everything
/// that can change Odoo state (`create` / `write` / `unlink` / workflow buttons)
/// and any method we cannot positively classify as read-only (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Mutate,
}

/// Why a generic-tool call against a default-blocked model was refused.
/// Lets the MCP layer render a message that points the operator at the right
/// knob without leaking internal paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockDenial {
    /// Model is in the built-in security block list and the agent has not
    /// listed it under `unblock_models`. Resolvable by an admin.
    SecurityDefault,
    /// Model was unblocked but is a system metadata table — mutating verbs are
    /// never allowed regardless of `unblock_models` (fail closed).
    SystemReadOnly,
}

/// Decide whether a generic `odoo_search` / `odoo_execute` call against `model`
/// clears the built-in block list, honouring the agent's `unblock_models`
/// opt-out. Pure + fail-closed so it is unit-testable in isolation.
///
/// - Not default-blocked ⇒ `Ok(())` (the per-agent `allowed_models` whitelist,
///   checked separately upstream, is the remaining gate).
/// - Default-blocked & not in `unblock_models` ⇒ `Err(SecurityDefault)`.
/// - Default-blocked & unblocked:
///     - system metadata model + `Mutate` ⇒ `Err(SystemReadOnly)`.
///     - otherwise ⇒ `Ok(())`.
pub fn check_blocklist(
    model: &str,
    kind: AccessKind,
    unblock_models: &[String],
) -> Result<(), BlockDenial> {
    if !BLOCKED_MODELS.contains(&model) {
        return Ok(());
    }
    let unblocked = unblock_models.iter().any(|m| m == model);
    if !unblocked {
        return Err(BlockDenial::SecurityDefault);
    }
    if READ_ONLY_WHEN_UNBLOCKED.contains(&model) && kind == AccessKind::Mutate {
        return Err(BlockDenial::SystemReadOnly);
    }
    Ok(())
}

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

/// Metadata for one Odoo field (from `ir.model.fields`). Metadata only — no
/// row data is ever carried here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaField {
    pub name: String,
    /// Odoo field type (`char`, `many2one`, `date`, …).
    pub ttype: String,
    /// Human label.
    pub label: String,
    pub required: bool,
    /// Target model for relational fields (`many2one`/`one2many`/`many2many`).
    pub relation: Option<String>,
}

/// Metadata for one Odoo model (from `ir.model`) plus its fields.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaModel {
    /// Technical name (`res.partner`, `x_custom_model`).
    pub model: String,
    /// Human label.
    pub name: String,
    /// Whether this is a Studio/custom model (`x_`-prefixed).
    pub custom: bool,
    pub field_count: usize,
    pub fields: Vec<SchemaField>,
}

/// Result of a schema introspection sweep — metadata only.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaReport {
    pub models: Vec<SchemaModel>,
    /// Models discovered before the `max_models` cap was applied.
    pub total_models: usize,
    /// True when the model list was truncated by `max_models`.
    pub truncated: bool,
}

/// Whether an `ir.model` model name is pure system noise we skip during
/// introspection (transient/wizard tables and framework internals). Custom
/// models (`x_`) are always kept. Pure function for unit tests.
pub fn is_introspection_noise(model: &str) -> bool {
    if model.starts_with("x_") {
        return false;
    }
    const NOISE_PREFIXES: &[&str] = &["ir.", "base.", "bus.", "base_import.", "web_editor.", "report."];
    NOISE_PREFIXES.iter().any(|p| model.starts_with(p))
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

    /// Check if a model is in the built-in security block list (static — no
    /// per-agent `unblock_models` override). Retained for callers that only
    /// need the default decision; the override-aware gate is
    /// [`check_blocklist`].
    pub fn is_model_blocked(model: &str) -> bool {
        BLOCKED_MODELS.contains(&model)
    }

    /// Introspect the Odoo schema — models plus their field metadata.
    ///
    /// Privileged management operation: reads `ir.model` / `ir.model.fields`
    /// directly. The generic-tool block list is an MCP-layer concern and does
    /// not apply to this admin sweep. Returns **metadata only** — never a
    /// business row. Framework/transient noise is filtered out, the model count
    /// is capped at `max_models`, and fields-per-model is capped so a large
    /// database can't produce an unbounded payload.
    pub async fn introspect_schema(&self, max_models: usize) -> Result<SchemaReport, String> {
        const MAX_FIELDS_PER_MODEL: usize = 80;

        // 1. Enumerate non-transient models.
        let model_domain = vec![json!(["transient", "=", false])];
        let raw_models = self
            .search_read("ir.model", model_domain, &["model", "name", "transient"], 2000)
            .await?;
        let arr = raw_models.as_array().cloned().unwrap_or_default();

        // 2. Drop framework noise; keep custom + business models.
        let mut kept: Vec<(String, String)> = Vec::new();
        for m in &arr {
            let model = m.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() || is_introspection_noise(model) {
                continue;
            }
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(model)
                .to_string();
            kept.push((model.to_string(), name));
        }
        kept.sort_by(|a, b| a.0.cmp(&b.0));
        let total_models = kept.len();
        let truncated = total_models > max_models;
        kept.truncate(max_models);

        if kept.is_empty() {
            return Ok(SchemaReport { models: Vec::new(), total_models, truncated });
        }

        // 3. One batched field query for all kept models.
        let model_names: Vec<&str> = kept.iter().map(|(m, _)| m.as_str()).collect();
        let field_domain = vec![json!(["model", "in", model_names])];
        let field_cap = kept
            .len()
            .saturating_mul(MAX_FIELDS_PER_MODEL)
            .min(20_000);
        let raw_fields = self
            .search_read(
                "ir.model.fields",
                field_domain,
                &["model", "name", "ttype", "field_description", "required", "relation"],
                field_cap,
            )
            .await?;

        // 4. Group fields by model (respecting the per-model cap).
        let mut by_model: std::collections::HashMap<String, Vec<SchemaField>> =
            std::collections::HashMap::new();
        let empty = Vec::new();
        for f in raw_fields.as_array().unwrap_or(&empty) {
            let model = f.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if model.is_empty() {
                continue;
            }
            let entry = by_model.entry(model).or_default();
            if entry.len() >= MAX_FIELDS_PER_MODEL {
                continue;
            }
            let relation = f
                .get("relation")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            entry.push(SchemaField {
                name: f.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                ttype: f.get("ttype").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                label: f
                    .get("field_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                required: f.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                relation,
            });
        }

        // 5. Assemble.
        let models: Vec<SchemaModel> = kept
            .into_iter()
            .map(|(model, name)| {
                let fields = by_model.remove(&model).unwrap_or_default();
                let custom = model.starts_with("x_");
                SchemaModel {
                    field_count: fields.len(),
                    custom,
                    model,
                    name,
                    fields,
                }
            })
            .collect();

        Ok(SchemaReport { models, total_models, truncated })
    }

    /// Field metadata for a single known model via `fields_get`. Read-only,
    /// metadata only. Caps the number of returned fields.
    pub async fn schema_fields(&self, model: &str, max_fields: usize) -> Result<Vec<SchemaField>, String> {
        let raw = self
            .execute_kw(
                model,
                "fields_get",
                vec![json!([])],
                json!({"attributes": ["string", "type", "required", "relation"]}),
            )
            .await?;
        let obj = raw
            .as_object()
            .ok_or_else(|| format!("Unexpected fields_get response for '{model}'"))?;
        let mut out: Vec<SchemaField> = Vec::new();
        for (fname, meta) in obj {
            if out.len() >= max_fields {
                break;
            }
            out.push(SchemaField {
                name: fname.clone(),
                ttype: meta.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                label: meta.get("string").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                required: meta.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                relation: meta
                    .get("relation")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Safe customer search over `res.partner`. Field set is fixed to
    /// non-sensitive columns (no bank/tax data) and the query matches
    /// name/email/ref. Privileged (bypasses the generic block list) because the
    /// field projection is hard-coded — callers still enforce `OdooRead` scope
    /// and per-agent `read` permission upstream.
    pub async fn partner_search(&self, query: &str, limit: usize) -> Result<Value, String> {
        let mut domain: Vec<Value> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            // OR over name / email / ref.
            domain.push(json!("|"));
            domain.push(json!("|"));
            domain.push(json!(["name", "ilike", q]));
            domain.push(json!(["email", "ilike", q]));
            domain.push(json!(["ref", "ilike", q]));
        }
        self.search_read("res.partner", domain, PARTNER_SEARCH_FIELDS, limit)
            .await
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

    // ── A: configurable block list (unblock_models) ───────────────────────

    #[test]
    fn check_blocklist_allows_non_blocked_model() {
        // A model not in the default block list always clears this gate.
        assert!(check_blocklist("sale.order", AccessKind::Read, &[]).is_ok());
        assert!(check_blocklist("crm.lead", AccessKind::Mutate, &[]).is_ok());
    }

    #[test]
    fn check_blocklist_denies_blocked_model_without_unblock() {
        // res.partner is default-blocked; no opt-out ⇒ security default denial.
        assert_eq!(
            check_blocklist("res.partner", AccessKind::Read, &[]),
            Err(BlockDenial::SecurityDefault)
        );
    }

    #[test]
    fn check_blocklist_allows_unblocked_business_model() {
        // Opting res.partner out lets both read and mutate through — it's a
        // business (not system) table.
        let unblock = vec!["res.partner".to_string()];
        assert!(check_blocklist("res.partner", AccessKind::Read, &unblock).is_ok());
        assert!(check_blocklist("res.partner", AccessKind::Mutate, &unblock).is_ok());
    }

    #[test]
    fn check_blocklist_system_model_read_only_even_when_unblocked() {
        // ir.model / ir.model.fields may be READ once unblocked (schema
        // introspection), but a mutating verb is refused fail-closed.
        let unblock = vec!["ir.model".to_string(), "ir.model.fields".to_string()];
        assert!(check_blocklist("ir.model", AccessKind::Read, &unblock).is_ok());
        assert_eq!(
            check_blocklist("ir.model", AccessKind::Mutate, &unblock),
            Err(BlockDenial::SystemReadOnly)
        );
        assert!(check_blocklist("ir.model.fields", AccessKind::Read, &unblock).is_ok());
        assert_eq!(
            check_blocklist("ir.model.fields", AccessKind::Mutate, &unblock),
            Err(BlockDenial::SystemReadOnly)
        );
    }

    #[test]
    fn check_blocklist_attachment_read_only_when_unblocked() {
        let unblock = vec!["ir.attachment".to_string()];
        assert!(check_blocklist("ir.attachment", AccessKind::Read, &unblock).is_ok());
        assert_eq!(
            check_blocklist("ir.attachment", AccessKind::Mutate, &unblock),
            Err(BlockDenial::SystemReadOnly)
        );
    }

    // ── B: introspection noise filter ─────────────────────────────────────

    #[test]
    fn introspection_keeps_business_and_custom_models() {
        assert!(!is_introspection_noise("res.partner"));
        assert!(!is_introspection_noise("sale.order"));
        assert!(!is_introspection_noise("account.move"));
        // Custom (Studio) models are always kept even if oddly prefixed.
        assert!(!is_introspection_noise("x_custom_model"));
        assert!(!is_introspection_noise("x_bus_route"));
    }

    #[test]
    fn introspection_drops_framework_noise() {
        assert!(is_introspection_noise("ir.model"));
        assert!(is_introspection_noise("ir.ui.view"));
        assert!(is_introspection_noise("base.language.install"));
        assert!(is_introspection_noise("bus.bus"));
        assert!(is_introspection_noise("report.layout"));
    }

    // ── C: partner search field whitelist ─────────────────────────────────

    #[test]
    fn partner_search_fields_exclude_sensitive_columns() {
        // The whitelist must never leak bank / tax / credit columns.
        for banned in ["vat", "bank_ids", "credit", "debit", "property_account_receivable_id"] {
            assert!(
                !PARTNER_SEARCH_FIELDS.contains(&banned),
                "partner search must not expose '{banned}'"
            );
        }
        // And it must carry the identifying columns callers rely on.
        for needed in ["id", "name", "email", "ref"] {
            assert!(
                PARTNER_SEARCH_FIELDS.contains(&needed),
                "partner search must expose '{needed}'"
            );
        }
    }
}
