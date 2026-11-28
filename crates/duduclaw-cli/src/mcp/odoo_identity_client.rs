//! Per-call gateway decisions. No local setting, argument or environment value
//! selects an Odoo account; environment values only authenticate the employee.
use std::{fmt, path::Path, time::Duration};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use serde_json::{Value, json};
use zeroize::Zeroizing;

const ROUTE: &str = "/api/internal/odoo-identity";
const UNAVAILABLE: &str = "Odoo 身分暫時無法確認，請確認 DuDuClaw 主程式正在執行後再試。";
const MALFORMED: &str = "DuDuClaw 主程式的 Odoo 身分回應格式不正確，這次操作尚未執行。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdentityKind { Personal, Agent, Company }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdentityPermission { Follow, Engine, ReadOnly }

pub(crate) struct OdooIdentity {
    pub kind: IdentityKind,
    pub permission: IdentityPermission,
    pub user_id: Option<String>,
    pub odoo_uid: Option<i64>,
    pub login: Option<String>,
    pub rev: Option<i64>,
    pub url: Option<String>,
    pub db: Option<String>,
    pub server_fp: Option<String>,
    pub secret: Option<Zeroizing<String>>,
}

impl fmt::Debug for OdooIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OdooIdentity").field("kind", &self.kind)
            .field("permission", &self.permission).field("user_id", &self.user_id)
            .field("rev", &self.rev).field("secret", &"[REDACTED]").finish()
    }
}

impl OdooIdentity {
    /// Validate the exact gateway snapshot again at the connection boundary.
    pub fn personal_endpoint(&self) -> Result<(&str, &str, &str), String> {
        let url = self.url.as_deref().ok_or(MALFORMED)?;
        let db = self.db.as_deref().ok_or(MALFORMED)?;
        let fp = self.server_fp.as_deref().ok_or(MALFORMED)?;
        let parsed = reqwest::Url::parse(url).map_err(|_| MALFORMED)?;
        if self.kind != IdentityKind::Personal || url.trim() != url || url.len() > 512
            || url.chars().any(char::is_control)
            || !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none()
            || !parsed.username().is_empty() || parsed.password().is_some()
            || parsed.query().is_some() || parsed.fragment().is_some()
            || !duduclaw_odoo::url_guard::is_valid_odoo_db(db)
            || fp != format!("{:x}", Sha256::digest(format!("{url}|{db}").as_bytes()))
        { return Err(MALFORMED.into()); }
        Ok((url, db, fp))
    }

    pub fn personal_profile(&self) -> Result<String, String> {
        let (_, _, fp) = self.personal_endpoint()?;
        Ok(format!("person:{}:{}:{fp}", self.user_id.as_deref().ok_or(MALFORMED)?,
            self.rev.ok_or(MALFORMED)?))
    }

    pub fn audit_value(&self) -> Value {
        json!({"kind": match self.kind {
            IdentityKind::Personal => "personal", IdentityKind::Agent => "agent", IdentityKind::Company => "company",
        }, "user_id": self.user_id})
    }
}

#[derive(Debug)]
pub(crate) struct IdentityRefusal { pub message: String }
impl fmt::Display for IdentityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for IdentityRefusal {}
fn refuse(message: &str) -> IdentityRefusal { IdentityRefusal { message: message.to_owned() } }

// A present null secret is malformed too. Deserialize straight into zeroizing
// storage, without keeping a second copy in a serde_json::Value.
fn secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Zeroizing<String>>, D::Error> {
    String::deserialize(d).map(|s| Some(Zeroizing::new(s)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    ok: bool,
    identity: Option<IdentityKind>,
    permission: Option<IdentityPermission>,
    user_id: Option<String>,
    odoo_uid: Option<i64>,
    login: Option<String>,
    rev: Option<i64>,
    url: Option<String>,
    db: Option<String>,
    server_fp: Option<String>,
    #[serde(default, deserialize_with = "secret")]
    secret: Option<Zeroizing<String>>,
    code: Option<String>,
    message: Option<String>,
}

fn interpret(status: u16, body: &[u8], have_rev: Option<i64>) -> Result<OdooIdentity, IdentityRefusal> {
    let r: Response = serde_json::from_slice(body).map_err(|_| refuse(MALFORMED))?;
    if status != 200 || !r.ok {
        let mut message = r.message.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| UNAVAILABLE.into());
        if let Some(secret) = &r.secret {
            if !secret.is_empty() { message = message.replace(secret.as_str(), "[REDACTED]"); }
        }
        return Err(refuse(&duduclaw_core::truncate_chars(&message, 500)));
    }
    let kind = r.identity.ok_or_else(|| refuse(MALFORMED))?;
    let permission = r.permission.ok_or_else(|| refuse(MALFORMED))?;
    if r.code.is_some() || r.message.is_some() { return Err(refuse(MALFORMED)); }
    match kind {
        IdentityKind::Personal => {
            let valid_text = |s: &Option<String>| s.as_ref().is_some_and(|s|
                !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control));
            if !valid_text(&r.user_id) || !valid_text(&r.login)
                || !r.odoo_uid.is_some_and(|n| n > 0) || !r.rev.is_some_and(|n| n > 0)
                || r.secret.as_ref().is_some_and(|s| s.is_empty())
                || (r.secret.is_none() && have_rev != r.rev)
            { return Err(refuse(MALFORMED)); }
        }
        IdentityKind::Agent | IdentityKind::Company => {
            if r.secret.is_some() || r.user_id.is_some() || r.odoo_uid.is_some()
                || r.login.is_some() || r.rev.is_some() || r.url.is_some() || r.db.is_some() || r.server_fp.is_some()
            { return Err(refuse(MALFORMED)); }
        }
    }
    let identity = OdooIdentity { kind, permission, user_id: r.user_id, odoo_uid: r.odoo_uid,
        login: r.login, rev: r.rev, secret: r.secret, url: r.url, db: r.db, server_fp: r.server_fp };
    if kind == IdentityKind::Personal { identity.personal_endpoint().map_err(|_| refuse(MALFORMED))?; }
    Ok(identity)
}

fn env_trimmed(name: &str) -> Option<Zeroizing<String>> {
    crate::mcp_isolation_identity::var(name).ok().map(|s| Zeroizing::new(s.trim().to_owned()))
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
}

pub(crate) async fn request_identity(home: &Path, agent: &str, tool: &str, have_rev: Option<i64>)
    -> Result<OdooIdentity, IdentityRefusal> {
    let body = json!({"tool":tool, "have_rev":have_rev,
        "turn_id":env_trimmed(duduclaw_core::ENV_TRUST_TURN_ID).as_deref().map(String::as_str),
        "run_id":env_trimmed(duduclaw_core::ENV_DISPATCH_RUN_ID).as_deref().map(String::as_str)});
    post(home, Some(agent), None, body, have_rev).await
}

/// P10 obtains this bearer from current_call_matches(principal).raw_person_key().
/// It is never a tool argument and never an employee authentication fallback.
pub(crate) async fn request_identity_for_person_key(home: &Path, raw_member_key: &str,
    tool: &str, have_rev: Option<i64>) -> Result<OdooIdentity, IdentityRefusal> {
    let identity = post(home, None, Some(raw_member_key), json!({"tool":tool,"have_rev":have_rev}), have_rev).await?;
    if identity.kind != IdentityKind::Personal || identity.permission == IdentityPermission::Engine { return Err(refuse(MALFORMED)); }
    Ok(identity)
}

async fn post(home: &Path, agent: Option<&str>, bearer: Option<&str>, body: Value,
    have_rev: Option<i64>) -> Result<OdooIdentity, IdentityRefusal> {
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never()).timeout(Duration::from_secs(30)).build()
        .map_err(|_| refuse(UNAVAILABLE))?;
    let bytes = serde_json::to_vec(&body).map_err(|_| refuse(MALFORMED))?;
    if bytes.len() > 4096 { return Err(refuse(MALFORMED)); }
    let mut request = client.post(format!("http://127.0.0.1:{port}{ROUTE}"))
        .header("Content-Type", "application/json");
    if let Some(raw) = bearer {
        if !duduclaw_auth::member_keys::is_member_key(raw) { return Err(refuse("本人金鑰格式不正確。")); }
        let value = Zeroizing::new(format!("Bearer {raw}"));
        let mut header = reqwest::header::HeaderValue::from_bytes(value.as_bytes()).map_err(|_| refuse(MALFORMED))?;
        header.set_sensitive(true);
        request = request.header(reqwest::header::AUTHORIZATION, header);
    } else {
        let key = employee_auth(duduclaw_core::ENV_MCP_API_KEY)?;
        let token = employee_auth(duduclaw_core::ENV_AGENT_TOKEN)?;
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| refuse(UNAVAILABLE))?.as_secs();
        let nonce: [u8; 16] = rand::random();
        for (name, value) in super::computer_use_client::signed_headers(
            &key, agent.ok_or_else(|| refuse(MALFORMED))?, &token, timestamp, &nonce, &bytes) {
            request = request.header(name, value);
        }
    }
    let mut response = request.body(bytes).send().await.map_err(|_| refuse(UNAVAILABLE))?;
    let status = response.status().as_u16();
    let mut out = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| refuse(UNAVAILABLE))? {
        if out.len() + chunk.len() > 64 * 1024 { return Err(refuse(MALFORMED)); }
        out.extend_from_slice(&chunk);
    }
    interpret(status, &out, have_rev)
}

fn employee_auth(name: &str) -> Result<Zeroizing<String>, IdentityRefusal> {
    #[cfg(test)]
    if let Ok(value) = tests::AUTH.try_with(|_| Zeroizing::new("test-identity-signing-key".to_owned())) { return Ok(value); }
    env_trimmed(name).ok_or_else(|| refuse("無法驗證 AI 員工的 Odoo 請求，請由 DuDuClaw 重新啟動這位員工。"))
}

/// Bounded process-memory cache. Callers hold the pool's async lock through
/// decision and connect, preventing an older response resurrecting a revoked rev.
#[derive(Default)]
pub(crate) struct IdentityCache {
    pub entries: Vec<(String, String, i64, String, Zeroizing<String>)>,
}

// The call context holds only the audit stamp; connectors return the complete
// decision (including permission) to their caller for P4b's mode gate.
tokio::task_local! { static CALL_IDENTITY: std::cell::RefCell<Option<Value>>; }
pub(super) async fn with_call<F: std::future::Future>(future: F) -> F::Output {
    if CALL_IDENTITY.try_with(|_| ()).is_ok() { return future.await; }
    CALL_IDENTITY.scope(std::cell::RefCell::new(None), future).await
}
pub(super) async fn with_new_call<F: std::future::Future>(future: F) -> F::Output {
    CALL_IDENTITY.scope(std::cell::RefCell::new(None), future).await
}
pub(super) fn record(identity: &OdooIdentity) {
    let _ = CALL_IDENTITY.try_with(|cell| *cell.borrow_mut() = Some(identity.audit_value()));
}
// Person calls emit one metadata-only row at their boundary, including early
// refusals. Handler summaries/results can contain business text or credentials.
tokio::task_local! { static PERSON_CALL: (); }
pub(super) async fn with_person_call<F: std::future::Future>(future: F) -> F::Output {
    PERSON_CALL.scope((), future).await
}

/// Explicit sensitive models plus exact dot/underscore-delimited credential
/// segments. No result content (including upstream errors) is retained here.
pub(super) fn sensitive_model(model: &str) -> bool {
    matches!(model, "ir.config_parameter" | "ir.mail_server" | "fetchmail.server"
        | "payment.provider" | "res.users" | "ir.attachment")
        || model == "res.users.apikeys" || model.strip_prefix("res.users.apikeys.").is_some()
        || model.split(['.', '_']).any(|segment| matches!(segment,
            "token" | "tokens" | "secret" | "secrets" | "key" | "keys"
            | "password" | "passwords" | "credential" | "credentials" | "apikey" | "apikeys"))
}

pub(super) fn audit_result(home: &Path, agent: &str, tool: &str, params: &Value,
    success: bool, raw: &str, metadata_only: bool) {
    if PERSON_CALL.try_with(|_| ()).is_ok() { return; }
    let (_, model, method) = super::odoo_access::operation(tool, Some(params))
        .unwrap_or((duduclaw_odoo::access_policy::ToolClass::Read, None, None));
    if !metadata_only && !model.is_some_and(sensitive_model) {
        audit_call(home, agent, tool, "Odoo call", success, Some(raw));
        return;
    }
    let model = model.filter(|m| duduclaw_odoo::schema::valid_model_name(m));
    let method = method.filter(|m| !m.is_empty() && m.len() <= 128
        && m.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let value = serde_json::from_str::<Value>(raw).ok().or_else(||
        raw.rsplit_once('\n').and_then(|(body, _)| serde_json::from_str::<Value>(body).ok()));
    let ids = params.get("ids").and_then(|v| v.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).or_else(|| Some(v.clone())));
    let stamp = CALL_IDENTITY.try_with(|cell| cell.borrow().clone()).ok().flatten().unwrap_or(Value::Null);
    duduclaw_security::audit::append_tool_call_with_extras(home, agent, tool, "Odoo result metadata", success,
        &[("odoo_identity", stamp), ("odoo_result", json!({
            "model": model, "method": method,
            "record_count": value.as_ref().and_then(|v| v.as_array().map(Vec::len)
                .or_else(|| v.get("id").filter(|id| id.is_i64()).map(|_| 1))),
            "ids_count": ids.as_ref().and_then(Value::as_array).map(Vec::len),
            "byte_length": raw.len(),
        }))]);
}

pub(super) fn audit_call(home: &Path, agent: &str, tool: &str, summary: &str,
    success: bool, result: Option<&str>) {
    if PERSON_CALL.try_with(|_| ()).is_ok() { return; }
    let stamp = CALL_IDENTITY.try_with(|cell| cell.borrow().clone()).ok().flatten().unwrap_or(Value::Null);
    let mut extras = vec![("odoo_identity", stamp)];
    if let Some(raw) = result {
        let masked = duduclaw_security::audit::mask_sensitive_text(raw);
        if !masked.trim().is_empty() {
            let cap = duduclaw_security::audit::AUDIT_RESULT_TEXT_MAX_CHARS;
            let truncated = masked.chars().count() > cap;
            extras.push(("result_text", json!(duduclaw_core::truncate_chars(&masked, cap))));
            if truncated { extras.push(("result_text_truncated", json!(true))); }
        }
    }
    duduclaw_security::audit::append_tool_call_with_extras(home, agent, tool, summary, success, &extras);
}

#[cfg(test)]
#[path = "odoo_identity_client_tests.rs"]
pub(super) mod tests;
