//! Browser-only personal Odoo connections; no administrator can set another key.
use super::*;
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use serde::Deserialize;
use crate::odoo_identity::server::{Server, company_server, member_credentials_enabled, MEMBER_CREDENTIALS_OFF};
use zeroize::Zeroizing;
use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event, try_append_audit_event};

const STORE: &str = "目前無法處理 Odoo 連線，請稍後再試。";
const LOGIN: &str = "請使用瀏覽器重新登入後管理自己的 Odoo 連線。";
const CONFIG: &str = "公司還沒有設定 Odoo 連線，請找管理員。";
const DENIED: &str = "Odoo 登入名稱和金鑰對不上，請確認後再試。";
const REENTER: &str = "你的帳號登入資料已變更，請重新輸入 Odoo 登入名稱和金鑰。";
const CHANGED: &str = "連線資料已變更，請重新整理頁面，確認伺服器位址和資料庫後再試。";
const CONFIRM_SERVER: &str = "請先重新整理連線頁面，確認 Odoo 伺服器位址和資料庫後再連線或重新驗證。";

pub(super) fn owns(method: &str) -> bool {
    method.split('.').take(2).eq(["me", "odoo"])
        || method.split('.').take(2).eq(["odoo", "member_credentials"])
}
/// Explicit permission table, independent of member-key API authorization.
pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "me.odoo.status" | "me.odoo.connect" | "me.odoo.test" | "me.odoo.disconnect" => Some(UserRole::Employee),
        "odoo.member_credentials.list" | "odoo.member_credentials.revoke" | "odoo.member_credentials.set_enabled" => Some(UserRole::Admin),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connect {
    login: String,
    api_key: Zeroizing<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoke { user_id: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabled {
    enabled: bool,
    #[serde(default)]
    acknowledge_risk: bool,
}
fn require_enabled(home: &Path) -> Result<(), String> {
    if member_credentials_enabled(home) { Ok(()) } else { Err(MEMBER_CREDENTIALS_OFF.into()) }
}
fn valid_input(value: &str, max: usize) -> bool {
    !value.is_empty() && value == value.trim() && value.chars().count() <= max
        && !value.chars().any(char::is_control)
}

#[derive(Default)]
struct Limiter { entries: HashMap<(PathBuf, String), VecDeque<Instant>> }
impl Limiter {
    fn allow(&mut self, home: &Path, user: &str, now: Instant) -> bool {
        self.entries.retain(|_, times| {
            while times.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60)) { times.pop_front(); }
            !times.is_empty()
        });
        let key = (home.to_path_buf(), user.to_owned());
        if self.entries.len() >= 4096 && !self.entries.contains_key(&key) { return false; }
        let times = self.entries.entry(key).or_default();
        if times.len() >= 5 { return false; }
        times.push_back(now);
        true
    }
}
static LIMITER: LazyLock<Mutex<Limiter>> = LazyLock::new(|| Mutex::new(Limiter::default()));
// A status read acknowledges the exact destination for this browser, not a
// value supplied in RPC parameters. No key is sent on status reads.
type SeenKey = (PathBuf, String, String);
static SEEN: LazyLock<Mutex<HashMap<SeenKey, (String, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
fn seen_server(home: &Path, user: &str, session: &str, fp: &str, remember: bool) -> Result<bool, String> {
    let mut seen = SEEN.lock().map_err(|_| STORE.to_owned())?;
    let now = Instant::now();
    seen.retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(600));
    let key = (home.to_path_buf(), user.to_owned(), session.to_owned());
    if remember {
        if seen.len() >= 4096 && !seen.contains_key(&key) { return Err(STORE.into()); }
        seen.insert(key, (fp.to_owned(), now));
        Ok(true)
    } else { Ok(seen.get(&key).is_some_and(|(shown, _)| shown == fp)) }
}
fn audit(home: &Path, event: &str, actor: &str, target: &str, reason: &str) {
    // Only local reason codes and database identities, never remote text/login/key.
    append_audit_event(home, &AuditEvent::new(event, "", Severity::Info,
        json!({"actor_user_id":actor,"user_id":target,"reason":reason})));
}
fn fresh(db: &UserDb, ctx: &UserContext, session: &str, role: UserRole) -> Result<UserContext, String> {
    let live = db.live_user_context(&ctx.user_id).map_err(|_| STORE.to_owned())?
        .filter(|u| u.credential_epoch == ctx.credential_epoch && !u.must_change_password)
        .ok_or(LOGIN)?;
    if !db.remember_session_valid(Some(session), &live.user_id, Utc::now().timestamp()).map_err(|_| STORE.to_owned())? {
        return Err(LOGIN.into());
    }
    acl::require_role(&live, role).map_err(|_| "只有管理員可以管理同仁的 Odoo 連線。".to_owned())?;
    Ok(live)
}
fn status(db: &UserDb, home: &Path, user: &str, session: &str) -> Result<Value, String> {
    let enabled = member_credentials_enabled(home);
    // Status remains available even when config cannot be read. No destination
    // acknowledgement is created from an invalid or missing configuration.
    let server = match company_server(home) {
        Ok(server) => server,
        Err(error) if enabled => return Err(error),
        Err(_) => None,
    };
    let mut row = db.odoo_credential_status(user).map_err(|_| STORE.to_owned())?;
    if let Some(row) = row.as_mut() {
        if server.as_ref().is_none_or(|s| s.fp != row.server_fp) {
            db.odoo_credential_mark_state(user, row.rev, "needs_reverify").map_err(|_| STORE.to_owned())?;
            row.state = "needs_reverify".into();
        }
    }
    if let Some(server) = &server { seen_server(home, user, session, &server.fp, true)?; }
    Ok(json!({"member_credentials_enabled":enabled, "connected":row.is_some(), "state":row.as_ref().map(|r| &r.state),
        "login":row.as_ref().map(|r| &r.login), "odoo_name":row.as_ref().map(|r| &r.odoo_name),
        "server":server.as_ref().map(|s| &s.destination), "database":server.as_ref().map(|s| &s.db),
        "verified_at":row.as_ref().map(|r| &r.verified_at),
        "last_used_at":row.as_ref().and_then(|r| r.last_used_at.as_ref())}))
}

#[derive(Clone, Copy)]
enum Failure { AccessDenied, LoginMismatch, Unreachable, InvalidResponse, Changed }
impl Failure {
    fn reason(self) -> &'static str {
        match self { Self::AccessDenied => "access_denied", Self::LoginMismatch => "login_mismatch",
            Self::Unreachable => "unreachable", Self::InvalidResponse => "invalid_response", Self::Changed => "changed" }
    }
    fn message(self) -> &'static str {
        match self { Self::AccessDenied | Self::LoginMismatch => DENIED,
            Self::Changed => CHANGED, _ => "目前無法確認 Odoo 連線，請稍後再試。" }
    }
}
fn rpc_failure(message: String) -> Failure {
    // The existing RPC API exposes local, fixed classification strings. Match
    // them in full; never classify arbitrary remote text by substring.
    match message.as_str() {
        "Authentication failed: invalid credentials or database. Retrying the same credentials will not succeed."
        | "Odoo server authentication gate denied this request. Ask an administrator to check the database, username and API key/password in the saved Odoo connection settings. Retrying the same credentials will not succeed. 使用相同憑證重試不會成功。" => Failure::AccessDenied,
        _ => Failure::Unreachable,
    }
}
async fn verify(server: &Server, login: &str, key: &str, check: impl Fn() -> Result<(), String>) -> Result<(i64, String), Failure> {
    check().map_err(|_| Failure::Changed)?;
    let plan = duduclaw_odoo::url_guard::plan_connection(&server.url, server.allow_private).await.map_err(|_| Failure::Unreachable)?;
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none()).no_proxy();
    if let Some((host, addrs)) = &plan { builder = builder.resolve_to_addrs(host, addrs); }
    let http = builder.build().map_err(|_| Failure::Unreachable)?;
    check().map_err(|_| Failure::Changed)?;
    let url = server.url.trim_end_matches('/');
    let uid = duduclaw_odoo::rpc::authenticate(&http, url, &server.db, login, key).await.map_err(rpc_failure)?;
    check().map_err(|_| Failure::Changed)?;
    let result = duduclaw_odoo::rpc::execute_kw(&http, url, &server.db, uid, key, "res.users", "read",
        vec![json!([uid])], json!({"fields":["login","name"]})).await.map_err(rpc_failure)?;
    let rows = result.as_array().filter(|rows| rows.len() == 1).ok_or(Failure::InvalidResponse)?;
    let row = rows.first().ok_or(Failure::InvalidResponse)?;
    if row.get("id").and_then(Value::as_i64) != Some(uid) { return Err(Failure::InvalidResponse); }
    if row.get("login").and_then(Value::as_str) != Some(login) { return Err(Failure::LoginMismatch); }
    let name = row.get("name").and_then(Value::as_str).filter(|name| valid_input(name, 512))
        .ok_or(Failure::InvalidResponse)?;
    // An untrusted server must not reflect a credential via public metadata.
    if name.contains(key) || login.contains(key) { return Err(Failure::InvalidResponse); }
    Ok((uid, name.to_owned()))
}

impl MethodHandler {
    pub(crate) async fn handle_odoo_member_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.member_key_id.is_some() || ctx.user_id == "system" { return WsFrame::error_response("", LOGIN); }
        let Some(role) = minimum_role(method) else { return WsFrame::error_response("", "沒有這項 Odoo 連線操作。"); };
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session, Err(_) => return WsFrame::error_response("", LOGIN),
        };
        let Some(db) = self.user_db.read().await.clone() else { return WsFrame::error_response("", STORE); };
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let method = method.to_owned();
        let runtime = tokio::runtime::Handle::current();
        // All filesystem/SQLite/audit work runs on a blocking worker. The
        // async RPC client is driven there without holding any DB/Mutex guard.
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let live = fresh(&db, &ctx, &session, role)?;
            let user = &live.user_id;
            match method.as_str() {
                "me.odoo.status" => {
                    serde_json::from_value::<Empty>(params).map_err(|_| "請重新整理連線頁面。".to_owned())?;
                    status(&db, &home, user, &session)
                }
                "me.odoo.disconnect" => {
                    serde_json::from_value::<Empty>(params).map_err(|_| STORE.to_owned())?;
                    db.odoo_credential_delete(user).map_err(|_| STORE.to_owned())?;
                    audit(&home, "odoo_member_credential_removed", user, user, "removed");
                    Ok(json!({"disconnected":true}))
                }
                "odoo.member_credentials.list" => {
                    serde_json::from_value::<Empty>(params).map_err(|_| STORE.to_owned())?;
                    let rows = db.odoo_credentials_admin_list(user).map_err(|_| STORE.to_owned())?;
                    Ok(json!({"credentials":rows,"enabled":member_credentials_enabled(&home)}))
                }
                "odoo.member_credentials.set_enabled" => {
                    let args: SetEnabled = serde_json::from_value(params)
                        .map_err(|_| "請選擇是否開放同仁連接 Odoo。".to_owned())?;
                    if args.enabled && !args.acknowledge_risk {
                        return Err("開放同仁連接 Odoo 前，請先確認你已了解風險。".into());
                    }
                    let path = home.join("config.toml");
                    let raw = super::config_commit::read_text_or_empty(&path).map_err(|_| STORE.to_owned())?;
                    let mut table = raw.parse::<toml::Table>().map_err(|_| CONFIG.to_owned())?;
                    super::config_commit::table_at_mut(&mut table, &["odoo"])
                        .map_err(|_| CONFIG.to_owned())?
                        .insert("member_credentials".into(), toml::Value::Boolean(args.enabled));
                    let guard_db = db.clone();
                    let guard_home = home.clone();
                    let guard_ctx = ctx.clone();
                    let guard_session = session.clone();
                    super::config_commit::commit_odoo_table(
                        &home, &super::config_commit::content_hash(&raw), &table, super::config_commit::OdooWrite::MemberCredentials, move || {
                            let actor = fresh(&guard_db, &guard_ctx, &guard_session, UserRole::Admin)?;
                            // Write-ahead audit under the config lock, after CAS and
                            // live authorization. Failure aborts publication; a later
                            // commit failure may leave an explicitly labelled intent.
                            let event = AuditEvent::new("odoo_member_credentials_toggled", "", Severity::Info,
                                json!({"actor_user_id":actor.user_id,"enabled":args.enabled,"acknowledged":args.acknowledge_risk,"phase":"before_commit"}));
                            try_append_audit_event(&guard_home, &event).map_err(|_| STORE.to_owned())
                        },
                    ).map_err(|_| STORE.to_owned())?;
                    Ok(json!({"enabled":args.enabled}))
                }
                "odoo.member_credentials.revoke" => {
                    let args: Revoke = serde_json::from_value(params).map_err(|_| "請選擇要撤銷連線的同仁。".to_owned())?;
                    if !valid_input(&args.user_id, 128) { return Err("請選擇要撤銷連線的同仁。".into()); }
                    let revoked = db.odoo_credential_admin_revoke(user, &args.user_id).map_err(|_| STORE.to_owned())?;
                    audit(&home, "odoo_member_credential_revoked_by_admin", user, &args.user_id, "revoked");
                    Ok(json!({"revoked":revoked}))
                }
                "me.odoo.connect" | "me.odoo.test" => {
                    require_enabled(&home)?;
                    let is_connect = method == "me.odoo.connect";
                    // The limit is shared, including malformed/failed attempts.
                    let allowed = LIMITER.lock().map_err(|_| STORE.to_owned())?.allow(&home, user, Instant::now());
                    if !allowed {
                        audit(&home, "odoo_member_credential_rejected", user, user, "rate_limited");
                        return Err("嘗試次數太多，請一分鐘後再試。".into());
                    }
                    let server = company_server(&home).and_then(|server| server.ok_or_else(|| CONFIG.to_owned()))
                        .map_err(|message| {
                            audit(&home, "odoo_member_credential_rejected", user, user, "not_configured");
                            message
                        })?;
                    let (login, secret, expected_rev, requires_confirmation) = if is_connect {
                        let args: Connect = serde_json::from_value(params).map_err(|_| {
                            audit(&home, "odoo_member_credential_rejected", user, user, "invalid_input");
                            "請填寫有效的 Odoo 登入名稱和金鑰。".to_owned()
                        })?;
                        if !valid_input(&args.login, 254) || !valid_input(&args.api_key, 512) {
                            audit(&home, "odoo_member_credential_rejected", user, user, "invalid_input");
                            return Err("請填寫有效的 Odoo 登入名稱和金鑰。".into());
                        }
                        if !seen_server(&home, user, &session, &server.fp, false)? {
                            return Err(CONFIRM_SERVER.into());
                        }
                        (args.login, args.api_key, None, true)
                    } else {
                        serde_json::from_value::<Empty>(params).map_err(|_| STORE.to_owned())?;
                        let row = db.odoo_credential_status(user).map_err(|_| STORE.to_owned())?.ok_or("你還沒有連接 Odoo，請先填寫登入名稱和金鑰。")?;
                        if row.server_fp != server.fp {
                            db.odoo_credential_mark_state(user, row.rev, "needs_reverify").map_err(|_| STORE.to_owned())?;
                            if !seen_server(&home, user, &session, &server.fp, false)? {
                                return Err(CONFIRM_SERVER.into());
                            }
                        }
                        let sealer = crate::odoo_credentials::Sealer::load(&home)?;
                        let opened = db.odoo_credential_unseal_for_reverification(user, row.rev, &sealer)
                            .map_err(|_| STORE.to_owned())?.ok_or(REENTER)?;
                        (opened.login, opened.secret, Some(opened.rev), row.server_fp != server.fp)
                    };
                    // Load the existing machine key before sending a newly entered secret.
                    let write_sealer = if is_connect {
                        Some(crate::odoo_credentials::Sealer::for_write(&home)?)
                    } else { None };
                    let check = || -> Result<(), String> {
                        fresh(&db, &ctx, &session, role)?;
                        require_enabled(&home)?;
                        let current = company_server(&home)?.ok_or(CONFIG)?;
                        if current.fp != server.fp { return Err(CHANGED.into()); }
                        if requires_confirmation && !seen_server(&home, user, &session, &current.fp, false)? {
                            return Err(CONFIRM_SERVER.into());
                        }
                        if let Some(rev) = expected_rev {
                            if db.odoo_credential_status(user).map_err(|_| STORE.to_owned())?.is_none_or(|r| r.rev != rev) {
                                return Err(CHANGED.into());
                            }
                        }
                        Ok(())
                    };
                    let verified = runtime.block_on(verify(&server, &login, &secret, &check));
                    let (uid, name) = match verified {
                        Ok(identity) => identity,
                        Err(failure) => {
                            if matches!(failure, Failure::AccessDenied) {
                                if let Some(rev) = expected_rev {
                                    db.odoo_credential_mark_state(user, rev, "invalid").map_err(|_| STORE.to_owned())?;
                                    audit(&home, "odoo_member_credential_invalidated", user, user, failure.reason());
                                }
                            }
                            audit(&home, "odoo_member_credential_rejected", user, user, failure.reason());
                            return Err(failure.message().into());
                        }
                    };
                    check()?;
                    if let Some(rev) = expected_rev {
                        if !db.odoo_credential_reverified(user, rev, uid, &name, &server.fp).map_err(|_| STORE.to_owned())? {
                            return Err(CHANGED.into());
                        }
                        audit(&home, "odoo_member_credential_verified", user, user, "verified");
                    } else {
                        let sealer = write_sealer.as_ref().ok_or(STORE)?;
                        db.odoo_credential_put(user, &login, uid, &name, &secret, &server.fp, sealer).map_err(|_| STORE.to_owned())?;
                        audit(&home, "odoo_member_credential_set", user, user, "verified");
                    }
                    status(&db, &home, user, &session)
                }
                _ => Err("沒有這項 Odoo 連線操作。".into()),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(message)) => WsFrame::error_response("", &message),
            Err(_) => WsFrame::error_response("", STORE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn odoo_member_limiter_is_bounded_and_uses_a_rolling_minute() {
        let mut limiter = Limiter::default();
        let home = Path::new("/test");
        let start = Instant::now();
        for second in [0, 10, 20, 30, 40] { assert!(limiter.allow(home, "a", start + Duration::from_secs(second))); }
        assert!(!limiter.allow(home, "a", start + Duration::from_secs(59)));
        assert!(limiter.allow(home, "a", start + Duration::from_secs(60)));
        assert!(!limiter.allow(home, "a", start + Duration::from_secs(61)));
        for id in 0..4095 { assert!(limiter.allow(home, &format!("user-{id}"), start + Duration::from_secs(61))); }
        assert!(!limiter.allow(home, "overflow", start + Duration::from_secs(61)));
    }
}
