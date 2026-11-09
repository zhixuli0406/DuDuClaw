//! Admin connector-route lifecycle (C3). Gateway-owned: dispatch routes the
//! whole `isolation.connector_` namespace here before any extension.
//!
//! Every method needs a current Admin re-read from users.db and the caller's
//! dashboard session. Provision and revoke additionally need the dashboard
//! step-up already used for sensitive migration decisions (password + TOTP,
//! five minutes, bound to this session and credential epoch; see
//! `UserDb::migration_step_up`) and that proof's CSRF token. The step-up
//! method here is the same proof, so a session verified on either page is
//! valid on both. No request field can claim Admin, MFA or step-up state.
//!
//! F5 UI-RPC: `isolation.connector_list` rows also carry the employees that
//! may use each route (`employees`, plus `employee_id`/`employee_label`/
//! `employee_kind` for a detail route pinned to one employee) and a derived
//! `status` (`active` / `needs_setup` / `revoked` / `unavailable`, from F3
//! readiness). P1: rows of another person's Personal routes are hidden
//! (`hidden_personal` counts them) unless the request carries the step-up
//! CSRF token (`step_up_csrf`), which reveals them and is audited.
use super::*;
use duduclaw_auth::mfa::MigrationConfirmationSession;
use duduclaw_isolation::{bounded::Text, capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_migration::confirmation::ApprovalSession;
use crate::isolation_connector_details::{needs_details, AccountDetailRecord, ChannelTokenRef, DetailEmployee, DetailError,
    DetailReference};
use crate::isolation_connector_lifecycle::{self as lifecycle, AdminActor, LifecycleError};
use crate::isolation_connectors::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, Route, MAX_LIST_PAGE};
use crate::isolation_lifecycle::ConfirmedPrincipal;
use crate::isolation_readiness::Reason;
use std::collections::{BTreeMap, BTreeSet};

const LIST: &str = "isolation.connector_list";
const STEP_UP: &str = "isolation.connector_step_up";
const PROVISION: &str = "isolation.connector_provision";
const REVOKE: &str = "isolation.connector_revoke";
/// v1.81 C5: stage migration Connector proposals from this home's legacy
/// connector accounts. Creates no route: each proposal still needs approval
/// on the migration card (`migration.confirmations.approve`).
const IMPORT: &str = "isolation.connector_import_legacy";
pub(super) const METHODS: [&str; 5] = [LIST, STEP_UP, PROVISION, REVOKE, IMPORT];

/// Namespace owned by this handler; unknown members are refused here.
pub(super) fn owns(method: &str) -> bool {
    method.strip_prefix("isolation.").is_some_and(|rest| rest.starts_with("connector_"))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
    #[serde(default)] after: Option<String>,
    /// F5 UI-RPC, P1 break-glass: the step-up CSRF token. With a fresh
    /// step-up, rows of other people's Personal routes are shown too (and the
    /// reveal is audited); without it they are hidden and only counted.
    #[serde(default)] step_up_csrf: Option<[u8; 32]>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StepUpRequest { password: String, code: String }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionRequest {
    provider: String,
    kind: String,
    /// `personal` (with `owner_user_id`) or `shared`. A shared route always
    /// binds this installation's tenant; no request can name a tenant.
    binding: String,
    #[serde(default)]
    owner_user_id: Option<String>,
    label: String,
    /// C6: required for `database`, `computer` and `channel`, refused for
    /// every other provider.
    #[serde(default)]
    details: Option<DetailParams>,
    csrf_token: [u8; 32],
}
/// Per-provider account details. Exactly the fields of the provider are
/// accepted; there is no field for a raw token, password or URL.
/// - database: `source` (a registered `[db_sources.<name>]` the employee is granted)
/// - computer: `credential` (a website credential id whose allowed agents include the employee)
/// - channel: `channel`, `scope` (exact chat / settings scope / pairing subject),
///   `token_ref` (`none` | `home` | `agent`), optional `status_label`, `extra_id`
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailParams {
    /// Agent id of the confirmed protected employee that will use the
    /// account. Resolved through the employee lifecycle, never trusted as-is.
    employee: String,
    #[serde(default)] source: Option<String>,
    #[serde(default)] credential: Option<String>,
    #[serde(default)] channel: Option<String>,
    #[serde(default)] scope: Option<String>,
    #[serde(default)] status_label: Option<String>,
    #[serde(default)] token_ref: Option<String>,
    #[serde(default)] extra_id: Option<String>,
}
impl DetailParams {
    /// Exactly the provider's fields; anything else (including an unknown
    /// token reference form) is refused.
    fn reference(&self, provider: &str) -> Option<DetailReference> {
        let Self { employee: _, source, credential, channel, scope, status_label, token_ref, extra_id } = self;
        let channel_fields = channel.is_some() || scope.is_some() || status_label.is_some() || token_ref.is_some() || extra_id.is_some();
        match provider {
            "database" if credential.is_none() && !channel_fields =>
                source.clone().map(|source| DetailReference::Database { source }),
            "computer" if source.is_none() && !channel_fields =>
                credential.clone().map(|credential| DetailReference::Computer { credential }),
            "channel" if source.is_none() && credential.is_none() => {
                let (channel, scope) = (channel.clone()?, scope.clone()?);
                let token = ChannelTokenRef::parse(token_ref.as_deref()?)?;
                Some(DetailReference::Channel { status_label: status_label.clone().unwrap_or_else(|| channel.clone()),
                    channel, scope, token, extra_id: extra_id.clone() })
            }
            _ => None,
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest { account: String, csrf_token: [u8; 32] }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest { csrf_token: [u8; 32] }

impl MethodHandler {
    pub(crate) async fn handle_isolation_connectors(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if !METHODS.contains(&method) || ctx.user_id == "system" {
            return WsFrame::error_response("", "不允許此連線帳號操作。");
        }
        if acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err() {
            return WsFrame::error_response("", "請以管理員帳號重新登入後再操作。");
        }
        let Ok(session) = crate::google_personal::session_for(&ctx.user_id) else {
            return WsFrame::error_response("", "請以管理員帳號重新登入後再操作。");
        };
        if !crate::runtime_readiness::is_protected_linux() {
            return WsFrame::error_response("", "這個環境未啟用受保護模式，沒有可管理的隔離連線帳號。");
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "無法讀取帳號資料。");
        };
        if method == STEP_UP && !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), &ctx.email) {
            return WsFrame::error_response("", "嘗試次數過多，請稍後再試。");
        }
        let (home, user, epoch, method) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch, method.to_owned());
        match tokio::task::spawn_blocking(move || serve(&home, &db, &user, &session, epoch, &method, params)).await {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(message)) => WsFrame::error_response("", message),
            Err(_) => WsFrame::error_response("", "連線帳號服務無法使用，請重新載入。"),
        }
    }
}

fn serve(home: &Path, db: &UserDb, user: &str, session: &str, epoch: u64, method: &str, params: Value)
    -> Result<Value, &'static str> {
    let now = chrono::Utc::now().timestamp();
    if now < 0 { return Err("無法確認目前時間。"); }
    if method == STEP_UP {
        let input: StepUpRequest = serde_json::from_value(params).map_err(|_| "請輸入密碼與驗證碼。")?;
        db.migration_step_up(user, session, epoch, &input.password, &input.code, &crate::mfa::Cipher(home), now)
            .map_err(|_| "驗證失敗，請確認密碼與新的驗證碼後重試。")?;
        return Ok(json!({ "verified": true }));
    }
    let proof = db.migration_confirmation_session(user, session, epoch, now)
        .map_err(|_| "登入憑證已失效，請重新登入。")?;
    match method {
        LIST => {
            let input: ListRequest = serde_json::from_value(params).map_err(|_| "查詢條件不正確。")?;
            let after = input.after.as_deref().map(AccountId::parse).transpose().map_err(|_| "查詢條件不正確。")?;
            // P1 break-glass is opt-in and needs the same fresh step-up as a mutation.
            let reveal = match input.step_up_csrf.as_ref() {
                None => false,
                Some(csrf) => { authorize_step_up(proof.as_ref(), user, csrf, now)?; true }
            };
            let catalog = ConnectorCatalog::open(home).map_err(catalog_message)?;
            let view = EmployeeView::read(home, user, reveal)?;
            let (mut value, revealed) = list_value(&catalog, after.as_ref(), &view).map_err(catalog_message)?;
            if revealed > 0 {
                // Audited before anything is returned; a failed write refuses (P1).
                db.log_action(Some(user), PERSONAL_REVEAL_ACTION, None,
                    Some(&json!({ "rows": revealed, "after": input.after }).to_string()), None)
                    .map_err(|_| "無法寫入稽核紀錄，已拒絕列出。")?;
            }
            value["step_up_valid"] = json!(proof.is_some());
            value["csrf_token"] = json!(proof.as_ref().map(|proof| proof.csrf));
            value["mfa_enrolled"] = json!(db.mfa_active(user).map_err(|_| "無法讀取帳號資料。")?);
            Ok(value)
        }
        PROVISION => {
            let input: ProvisionRequest = serde_json::from_value(params).map_err(|_| "連線帳號資料不正確。")?;
            authorize_step_up(proof.as_ref(), user, &input.csrf_token, now)?;
            let kind = parse_kind(&input.kind).ok_or("連線帳號資料不正確。")?;
            let binding = resolve_binding(home, db, &input.binding, input.owner_user_id.as_deref())?;
            let actor = AdminActor::verified(user).map_err(|_| "無法確認管理員帳號。")?;
            let target = match &input.details {
                Some(params) => {
                    let reference = params.reference(&input.provider).ok_or("連線帳號的設定資料不正確。")?;
                    Some((resolve_employee(home, &params.employee)?, reference))
                }
                None => None,
            };
            let account = lifecycle::provision(home, &input.provider, kind, &binding, &input.label,
                target.as_ref().map(|(employee, reference)| (employee, reference)), &actor)
                .map_err(lifecycle_message)?;
            Ok(json!({ "account": account.as_str(), "details": target.is_some(), "readback_required": true }))
        }
        REVOKE => {
            let input: RevokeRequest = serde_json::from_value(params).map_err(|_| "連線帳號識別碼無效。")?;
            authorize_step_up(proof.as_ref(), user, &input.csrf_token, now)?;
            let account = AccountId::parse(&input.account).map_err(|_| "連線帳號識別碼無效。")?;
            let actor = AdminActor::verified(user).map_err(|_| "無法確認管理員帳號。")?;
            let route = lifecycle::revoke(home, &account, &actor).map_err(lifecycle_message)?;
            Ok(json!({ "account": route.account.as_str(), "active": route.active,
                "generation": route.generation, "readback_required": true }))
        }
        IMPORT => {
            let input: ImportRequest = serde_json::from_value(params).map_err(|_| "匯入要求不正確。")?;
            authorize_step_up(proof.as_ref(), user, &input.csrf_token, now)?;
            // Every legacy store is read before anything is staged; any
            // unreadable store refuses the whole import.
            let summary = super::migration_authority::connectors::import_legacy(home)
                .map_err(|_| "無法完整讀取舊的連線帳號資料，已停止匯入；未建立任何連線帳號路由。")?;
            Ok(json!({ "staged": summary.staged, "unchanged": summary.unchanged, "closed": summary.closed,
                "unproven_personal": summary.unproven_personal, "approval_required": true }))
        }
        _ => Err("不允許此連線帳號操作。"),
    }
}

/// The proof is read by the caller from users.db for this exact user, session
/// and credential epoch (already proving current Admin + enrolled MFA); this
/// adds the five-minute freshness window and the session-bound CSRF token.
/// Shared with the employee proposal RPCs (A4), which reuse the same proof.
pub(super) fn authorize_step_up(proof: Option<&MigrationConfirmationSession>, user: &str, csrf: &[u8; 32], now: i64)
    -> Result<(), &'static str> {
    const REQUIRED: &str = "請先以密碼與雙因素驗證確認身分。";
    let proof = proof.ok_or(REQUIRED)?;
    let actor = uuid::Uuid::parse_str(user).map_err(|_| "無法確認帳號。")?;
    let now = u64::try_from(now).map_err(|_| REQUIRED)?;
    ApprovalSession::from_verified_session(true, true, Some(proof.verified_at), proof.csrf,
        duduclaw_migration::ids::RequestId(*actor.as_bytes()))
        .authorize(csrf, now).map_err(|_| "身分驗證已失效，請重新驗證後再操作。")
}

fn parse_kind(kind: &str) -> Option<ConnectorKind> {
    Some(match kind {
        "personal" => ConnectorKind::Personal,
        "shared_tenant" => ConnectorKind::SharedTenant,
        "shared_home" => ConnectorKind::SharedHome,
        _ => return None,
    })
}

/// Personal: the owner must be a live, active, logged-in user without a
/// forced password change. Shared: this installation's tenant from users.db.
fn resolve_binding(home: &Path, db: &UserDb, binding: &str, owner: Option<&str>) -> Result<EmployeeBinding, &'static str> {
    const INVALID: &str = "連線帳號的歸屬不正確。";
    match (binding, owner) {
        ("personal", Some(owner)) => {
            let user = db.get_user(owner).map_err(|_| "無法讀取帳號資料。")?.ok_or(INVALID)?;
            if user.id != owner || user.status != duduclaw_auth::UserStatus::Active || user.must_change_password
                || user.last_login.as_deref().is_none_or(str::is_empty) {
                return Err("這位成員目前無法擁有個人連線帳號，請確認帳號已啟用並完成登入。");
            }
            Text::new(user.id).map(EmployeeBinding::Personal).map_err(|_| INVALID)
        }
        ("shared", None) => {
            let users = rusqlite::Connection::open_with_flags(home.join("users.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .map_err(|_| "無法讀取帳號資料。")?;
            let tenant = duduclaw_auth::isolation::read_isolation_tenant(&users).map_err(|_| "無法確認此安裝的租戶。")?;
            Text::new(tenant).map(EmployeeBinding::Shared).map_err(|_| INVALID)
        }
        _ => Err(INVALID),
    }
}

/// The confirmed protected employee currently holding `agent`, from the
/// employee lifecycle (apply.db + confirmation + root readback + users.db).
/// Its immutable `EmployeeId` and binding are what the details are pinned to.
fn resolve_employee(home: &Path, agent: &str) -> Result<DetailEmployee, &'static str> {
    let lifecycle = crate::isolation_lifecycle::read_lifecycle(home, agent)
        .map_err(|_| "找不到這位受保護員工，或其狀態目前無法使用。")?;
    if lifecycle.label != agent { return Err("找不到這位受保護員工，或其狀態目前無法使用。"); }
    Ok(DetailEmployee { employee: lifecycle.employee, agent: lifecycle.label, binding: lifecycle.binding })
}

/// `auth_audit_log` action of an Admin's step-up reveal of other people's
/// Personal connector rows (F5 UI-RPC, P1 break-glass).
const PERSONAL_REVEAL_ACTION: &str = "isolation.connector_list_personal_revealed";
/// Upper bound of confirmed employees read for the route → employee map.
const MAX_MAPPED_EMPLOYEES: usize = 4096;

/// F5 UI-RPC: who may use the routes of one list page and how ready each
/// employee is, so the dashboard can group rows by employee. Display only:
/// every selection is still decided by the lifecycle and the D1 plan.
pub(super) struct EmployeeView {
    /// Live confirmed employees whose principal is known (apply.db);
    /// `None` = unreadable (rows then read `unavailable`, never `active`).
    employees: Option<Vec<ConfirmedPrincipal>>,
    /// F3 readiness reasons by label (`isolation_readiness`); `None` = unreadable.
    readiness: Option<BTreeMap<String, Vec<Reason>>>,
    /// P1 (`personal_visibility::hidden_from`): Personal employees the caller
    /// does not own.
    hidden: BTreeSet<String>,
    caller: String,
    /// P1 break-glass: an Admin with a fresh step-up sees every row, audited.
    reveal: bool,
}

impl EmployeeView {
    /// Blocking (SQLite + the readiness assessment's root ReadBack).
    fn read(home: &Path, caller: &str, reveal: bool) -> Result<Self, &'static str> {
        // An unreadable Personal mirror refuses the listing (P1, fail closed).
        let hidden = crate::personal_visibility::hidden_from(home, true, caller)
            .map_err(|_| "無法確認員工歸屬，已拒絕列出。")?;
        let employees = crate::isolation_lifecycle::confirmed_principals(home, MAX_MAPPED_EMPLOYEES).ok()
            .map(|rows| rows.into_iter().filter(|row| !row.disabled && row.binding.is_some()).collect());
        let assessment = crate::isolation_readiness::reasons::assess_installed(home);
        let unreadable = assessment.employees.is_empty() && assessment.reasons.contains(&Reason::ServiceUnavailable);
        let readiness = (!unreadable).then(|| assessment.employees.into_iter()
            .map(|employee| (employee.label, employee.reasons)).collect());
        Ok(Self { employees, readiness, hidden, caller: caller.to_owned(), reveal })
    }

    fn visible(&self, label: &str) -> bool { self.reveal || !self.hidden.contains(label) }

    /// Connector status of one employee for a route of `provider`, from its
    /// F3 reasons: `unavailable` (not assessed / service unavailable),
    /// `needs_setup` (`connector_setup_required` for this provider, or without
    /// provider = no connector build at all), else `active`.
    fn status(&self, label: &str, provider: &str) -> &'static str {
        let Some(reasons) = self.readiness.as_ref().and_then(|readiness| readiness.get(label)) else { return "unavailable" };
        if reasons.contains(&Reason::ServiceUnavailable) { return "unavailable"; }
        let setup = reasons.iter().any(|reason| match reason {
            Reason::ConnectorSetupRequired { provider: None } => true,
            Reason::ConnectorSetupRequired { provider: Some(named) } => *named == provider,
            _ => false,
        });
        if setup { "needs_setup" } else { "active" }
    }
}

fn binding_kind(binding: &EmployeeBinding) -> &'static str {
    match binding { EmployeeBinding::Personal(_) => "personal", EmployeeBinding::Shared(_) => "shared" }
}

/// The employees that may use `route`, by the D1 plan's admission
/// (`isolation_connector_boot::plan_connector_accounts`): a detail route
/// (database / computer / channel) only the employee its C6 row pins; a
/// Personal route the owner's Personal employees; a SharedTenant route the
/// tenant's Shared employees; a SharedHome route the tenant's Shared
/// employees and, through the home binding, Personal employees
/// (`via_home: true`; the product-home check still decides per call).
/// Personal employees the caller does not own are left out (P1) unless
/// revealed. Each entry: `{employee_id, label, kind, status, via_home}`.
fn route_employees(route: &Route, details: Option<&AccountDetailRecord>, view: &EmployeeView) -> Vec<Value> {
    let status = |label: &str| if route.active { view.status(label, route.provider) } else { "revoked" };
    let entry = |employee: &EmployeeId, label: &str, kind: &str, via_home: bool| json!({
        "employee_id": hex::encode(employee.0), "label": label, "kind": kind, "status": status(label), "via_home": via_home });
    let employees = view.employees.as_deref().unwrap_or_default();
    if needs_details(route.provider) {
        let Some(record) = details else { return Vec::new() };
        if !view.visible(&record.agent) { return Vec::new(); }
        let live = employees.iter().find(|row| row.employee == record.employee && row.label == record.agent);
        return vec![match live {
            Some(row) => entry(&row.employee, &row.label, row.binding.as_ref().map_or("shared", binding_kind), false),
            // Pinned to an employee that is no longer live: never `active`.
            None => {
                let mut value = entry(&record.employee, &record.agent, binding_kind(&route.binding), false);
                if route.active { value["status"] = json!("unavailable"); }
                value
            }
        }];
    }
    employees.iter().filter(|row| view.visible(&row.label)).filter_map(|row| {
        let binding = row.binding.as_ref()?;
        let via_home = match (route.kind, binding) {
            (ConnectorKind::Personal, EmployeeBinding::Personal(_)) | (ConnectorKind::SharedTenant, EmployeeBinding::Shared(_))
                | (ConnectorKind::SharedHome, EmployeeBinding::Shared(_)) if *binding == route.binding => false,
            (ConnectorKind::SharedHome, EmployeeBinding::Personal(_)) => true,
            _ => return None,
        };
        Some(entry(&row.employee, &row.label, binding_kind(binding), via_home))
    }).collect()
}

/// Row status: `revoked` (route revoked), `unavailable` (the employee map or
/// readiness could not be read, or a mapped employee's readiness), `needs_setup`
/// (a detail route without its details, or a mapped employee's
/// `connector_setup_required` for this provider), else `active`.
fn route_status(route: &Route, details: Option<&AccountDetailRecord>, employees: &[Value], view: &EmployeeView) -> &'static str {
    let any = |status: &str| employees.iter().any(|employee| employee["status"] == status);
    if !route.active { "revoked" }
    else if view.employees.is_none() || view.readiness.is_none() || any("unavailable") { "unavailable" }
    else if (needs_details(route.provider) && details.is_none()) || any("needs_setup") { "needs_setup" }
    else { "active" }
}

/// One page for the admin list, plus the number of other people's Personal
/// rows revealed (P1 break-glass; the caller audits a non-zero count).
/// Contains no secret (the catalog holds none), no tenant id and no email: a
/// Personal route names only its owner's user id; employees appear by label
/// and id. Rows of another person's Personal route are hidden (counted in
/// `hidden_personal`) unless `view.reveal`.
fn list_value(catalog: &ConnectorCatalog, after: Option<&AccountId>, view: &EmployeeView)
    -> Result<(Value, usize), CatalogError> {
    let routes = catalog.list(after, MAX_LIST_PAGE)?;
    let next = (routes.len() == MAX_LIST_PAGE).then(|| routes.last().map(|route| route.account.to_string())).flatten();
    let (mut rows, mut hidden, mut revealed) = (Vec::new(), 0usize, 0usize);
    for route in &routes {
        let foreign = matches!(&route.binding, EmployeeBinding::Personal(owner) if owner.as_str() != view.caller);
        if foreign && !view.reveal { hidden += 1; continue; }
        if foreign { revealed += 1; }
        // Details resolve only for an active route at its current generation.
        let details = if needs_details(route.provider) {
            catalog.route_details(&route.account)?.and_then(|(_, details)| details)
        } else { None };
        rows.push(route_summary(route, details.as_ref(), view));
    }
    Ok((json!({ "routes": rows, "next": next, "hidden_personal": hidden, "personal_revealed": view.reveal,
        "employees_available": view.employees.is_some(), "readiness_available": view.readiness.is_some() }), revealed))
}
fn route_summary(route: &Route, details: Option<&AccountDetailRecord>, view: &EmployeeView) -> Value {
    let binding = match &route.binding {
        EmployeeBinding::Personal(owner) => json!({ "type": "personal", "owner_user_id": owner.as_str() }),
        EmployeeBinding::Shared(_) => json!({ "type": "shared" }),
    };
    let employees = route_employees(route, details, view);
    let status = route_status(route, details, &employees, view);
    let mut row = json!({ "account": route.account.as_str(), "label": route.label, "provider": route.provider,
        "kind": route.kind.as_str(), "binding": binding, "generation": route.generation, "active": route.active,
        "status": status });
    // A detail route is bound to exactly one employee.
    if let Some(pinned) = employees.first().filter(|_| details.is_some()) {
        row["employee_id"] = pinned["employee_id"].clone();
        row["employee_label"] = pinned["label"].clone();
        row["employee_kind"] = pinned["kind"].clone();
    }
    row["employees"] = Value::Array(employees);
    row
}

fn catalog_message(error: CatalogError) -> &'static str {
    match error {
        CatalogError::Invalid => "連線帳號資料不正確。",
        CatalogError::NotFound => "找不到這個連線帳號。",
        CatalogError::Revoked => "這個連線帳號已撤銷。",
        CatalogError::Unavailable => "連線帳號目錄無法使用，已拒絕操作。",
    }
}
fn lifecycle_message(error: LifecycleError) -> &'static str {
    match error {
        LifecycleError::Catalog(error) => catalog_message(error),
        LifecycleError::Detail(error) => match error {
            DetailError::Required => "這類連線帳號需要填寫設定資料（資料來源、網站登入資料或頻道）。",
            DetailError::NotApplicable => "這類連線帳號不需要額外設定資料。",
            DetailError::Invalid => "連線帳號的設定資料不正確。",
            DetailError::Employee => "這位員工的歸屬不能使用這個連線帳號。",
            DetailError::Unknown => "找不到指定的資料來源、網站登入資料或頻道憑證。",
            DetailError::Foreign => "這位員工沒有使用指定資料來源、網站登入資料或頻道憑證的權限。",
            DetailError::Unavailable => "無法讀取設定資料的來源，已拒絕操作。",
        },
        LifecycleError::MaterialPending => "已停用這個連線帳號，但憑證尚未清除完成，請再執行一次撤銷。",
        LifecycleError::Conflict => "這個帳號已有連線帳號路由，未建立新的路由。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(verified_at: u64) -> MigrationConfirmationSession {
        MigrationConfirmationSession { verified_at, csrf: [7; 32] }
    }
    const ADMIN: &str = "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10";

    #[tokio::test]
    async fn rpc_requires_admin_step_up() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let mut employee = UserContext::admin_fallback();
        employee.user_id = ADMIN.into();
        employee.role = UserRole::Employee;
        let bootstrap = UserContext::admin_fallback();
        let csrf = vec![7u8; 32];
        let provision = json!({"provider":"google","kind":"personal","binding":"personal","owner_user_id":ADMIN,
            "label":"x","csrf_token":csrf.clone()});
        let revoke = json!({"account":"a".repeat(32),"csrf_token":csrf.clone()});
        for caller in [&employee, &bootstrap] {
            for (method, params) in [(LIST, json!({})), (STEP_UP, json!({"password":"p","code":"000000"})),
                (PROVISION, provision.clone()), (REVOKE, revoke.clone()), ("isolation.connector_purge", json!({}))] {
                let result = handler.handle(method, params, caller).await;
                assert!(!matches!(result, WsFrame::Response { ok: true, .. }), "{method}");
            }
        }
        assert!(!home.path().join(crate::isolation_connectors::CATALOG_FILE).exists());

        // An Admin's mutation needs a fresh proof and its CSRF token.
        let now = 10_000;
        assert!(authorize_step_up(None, ADMIN, &[7; 32], now).is_err());
        assert!(authorize_step_up(Some(&proof(now as u64 - 301)), ADMIN, &[7; 32], now).is_err());
        assert!(authorize_step_up(Some(&proof(now as u64 + 1)), ADMIN, &[7; 32], now).is_err());
        assert!(authorize_step_up(Some(&proof(now as u64)), ADMIN, &[8; 32], now).is_err());
        assert!(authorize_step_up(Some(&proof(now as u64)), "not-a-uuid", &[7; 32], now).is_err());
        assert!(authorize_step_up(Some(&proof(now as u64 - 60)), ADMIN, &[7; 32], now).is_ok());
        // Requests cannot carry authority claims.
        assert!(serde_json::from_value::<ProvisionRequest>(provision.clone()).is_ok());
        for extra in ["admin", "step_up_valid", "mfa_enrolled", "tenant_id", "account"] {
            let mut forged = provision.clone();
            forged[extra] = json!(true);
            assert!(serde_json::from_value::<ProvisionRequest>(forged).is_err(), "{extra}");
        }
        assert!(serde_json::from_value::<RevokeRequest>(json!({"account":"a".repeat(32)})).is_err());
        assert!(serde_json::from_value::<RevokeRequest>(json!({"account":"a".repeat(32),"csrf_token":csrf,"force":true})).is_err());
        assert!(serde_json::from_value::<RevokeRequest>(revoke).is_ok());
    }

    #[test]
    fn c6_detail_params_are_exact_and_reference_only() {
        let parse = |value: Value| serde_json::from_value::<DetailParams>(value);
        let channel = || json!({"employee":"employee-a","channel":"telegram","scope":"1","token_ref":"agent"});
        // There is no field that could carry a raw token, password or URL,
        // nor one that could name an employee id or binding directly.
        for forbidden in ["token", "bot_token", "password", "url", "employee_id", "binding", "tenant_id"] {
            let mut value = channel();
            value[forbidden] = json!("123456:AAHraw");
            assert!(parse(value).is_err(), "{forbidden}");
        }
        let parsed = parse(channel()).unwrap();
        assert!(matches!(parsed.reference("channel"), Some(DetailReference::Channel { token: ChannelTokenRef::Agent,
            status_label, .. }) if status_label == "telegram"));
        assert!(parsed.reference("database").is_none(), "channel fields never describe another provider");
        let mut raw = channel();
        raw["token_ref"] = json!("123456:AAHraw");
        assert!(parse(raw).unwrap().reference("channel").is_none(), "a token value is not a reference form");
        let db = parse(json!({"employee":"employee-a","source":"demo"})).unwrap();
        assert!(matches!(db.reference("database"), Some(DetailReference::Database { source }) if source == "demo"));
        assert!(db.reference("computer").is_none() && db.reference("google").is_none());
        let mixed = parse(json!({"employee":"employee-a","source":"demo","credential":"x"})).unwrap();
        assert!(mixed.reference("database").is_none() && mixed.reference("computer").is_none());

        let mut request = json!({"provider":"database","kind":"personal","binding":"personal","owner_user_id":ADMIN,
            "label":"x","csrf_token":vec![7u8; 32],"details":{"employee":"employee-a","source":"demo"}});
        assert!(serde_json::from_value::<ProvisionRequest>(request.clone()).is_ok());
        request["details"]["employee_binding"] = json!("personal");
        assert!(serde_json::from_value::<ProvisionRequest>(request).is_err());
    }

    #[test]
    fn rpc_namespace_and_binding_shape_are_exact() {
        for method in METHODS { assert!(owns(method), "{method}"); }
        for method in ["isolation.connector_purge", "isolation.connector_list.extra"] { assert!(owns(method), "{method}"); }
        for method in ["isolation.recording_status", "isolation", "connector_list", "evil.isolation.connector_list"] {
            assert!(!owns(method), "{method}");
        }
        assert_eq!(parse_kind("shared_home"), Some(ConnectorKind::SharedHome));
        assert_eq!(parse_kind("Personal"), None);
        let home = crate::fs_safe::private_test_home().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        // A caller cannot pick a tenant, omit a Personal owner or name an unknown one.
        assert!(resolve_binding(home.path(), &db, "shared", Some("tenant")).is_err());
        assert!(resolve_binding(home.path(), &db, "personal", None).is_err());
        assert!(resolve_binding(home.path(), &db, "personal", Some(ADMIN)).is_err());
        assert!(resolve_binding(home.path(), &db, "tenant", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn list_shows_no_secrets() {
        use crate::isolation_connector_clients::{AccountCredential, AccountCredentialVault};
        let home = crate::fs_safe::private_test_home().unwrap();
        let tenant = "6f1c2f4e-3b7a-4d0e-9a8b-1c2d3e4f5a6b";
        let owner = EmployeeBinding::Personal(Text::new(ADMIN.into()).unwrap());
        let shared = EmployeeBinding::Shared(Text::new(tenant.into()).unwrap());
        let actor = AdminActor::verified(ADMIN).unwrap();
        let personal = lifecycle::provision(home.path(), "google", ConnectorKind::Personal, &owner, "Alice mail", None, &actor).unwrap();
        let team = lifecycle::provision(home.path(), "notion", ConnectorKind::SharedHome, &shared, "Team wiki", None, &actor).unwrap();
        // Material exists in the process vault; the listing never consults it.
        let vault = AccountCredentialVault::default();
        vault.install(AccountCredential::new(personal.to_string(), "google".into(), "ya29.secret-material".into(), u64::MAX)
            .unwrap(), 1).unwrap();
        let catalog = ConnectorCatalog::open(home.path()).unwrap();
        catalog.revoke(&team).unwrap();

        let (value, revealed) = list_value(&catalog, None, &view(ADMIN, vec![], false)).unwrap();
        assert_eq!(revealed, 0);
        let encoded = value.to_string();
        for forbidden in [tenant, "secret-material", "token", "tenant", "email", "@", "shared:", "personal:"] {
            assert!(!encoded.contains(forbidden), "{forbidden}");
        }
        let routes = value["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 2);
        let row = |id: &AccountId| routes.iter().find(|row| row["account"] == id.as_str()).unwrap().clone();
        let alice = row(&personal);
        assert_eq!((alice["provider"].as_str(), alice["kind"].as_str(), alice["active"].as_bool()),
            (Some("google"), Some("personal"), Some(true)));
        assert_eq!(alice["binding"], json!({"type":"personal","owner_user_id":ADMIN}));
        assert!(alice["generation"].as_u64().is_some_and(|generation| generation > 0));
        let team_row = row(&team);
        assert_eq!(team_row["binding"], json!({"type":"shared"}));
        assert_eq!(team_row["active"], json!(false));
        let mut keys: Vec<_> = alice.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["account", "active", "binding", "employees", "generation", "kind", "label", "provider", "status"]);
        assert_eq!((alice["status"].as_str(), team_row["status"].as_str()), (Some("active"), Some("revoked")));
        assert_eq!(value["next"], Value::Null);
    }

    const BOB: &str = "5a0c1e2d-3b4f-4a6e-8d7c-9b1a2c3d4e5f";
    const TENANT: &str = "6f1c2f4e-3b7a-4d0e-9a8b-1c2d3e4f5a6b";

    fn principal(label: &str, id: u8, binding: EmployeeBinding) -> ConfirmedPrincipal {
        ConfirmedPrincipal { label: label.into(), employee: EmployeeId([id; 16]), binding: Some(binding), disabled: false }
    }
    fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(owner.into()).unwrap()) }
    fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new(TENANT.into()).unwrap()) }

    /// A view with every listed employee ready, `alice-helper` owned by
    /// ADMIN and `bob-helper` by BOB (hidden from ADMIN unless revealed).
    fn view(caller: &str, employees: Vec<ConfirmedPrincipal>, reveal: bool) -> EmployeeView {
        let readiness = employees.iter().map(|row| (row.label.clone(), Vec::new())).collect();
        let hidden = employees.iter().filter_map(|row| match &row.binding {
            Some(EmployeeBinding::Personal(owner)) if owner.as_str() != caller => Some(row.label.clone()),
            _ => None,
        }).collect();
        EmployeeView { employees: Some(employees), readiness: Some(readiness), hidden, caller: caller.into(), reveal }
    }

    fn staff() -> Vec<ConfirmedPrincipal> {
        vec![principal("alice-helper", 1, personal(ADMIN)), principal("bob-helper", 2, personal(BOB)),
            principal("team-bot", 3, shared())]
    }

    fn labels(row: &Value) -> Vec<String> {
        row["employees"].as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap().to_owned()).collect()
    }

    /// F5 UI-RPC: rows name the employees that may use them, hide another
    /// person's Personal rows and employees (P1) unless revealed, and derive
    /// a status from readiness.
    #[cfg(unix)]
    #[test]
    fn f5_connector_list_maps_routes_to_employees() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let actor = AdminActor::verified(ADMIN).unwrap();
        let mine = lifecycle::provision(home.path(), "google", ConnectorKind::Personal, &personal(ADMIN), "Mine", None, &actor).unwrap();
        let bobs = lifecycle::provision(home.path(), "google", ConnectorKind::Personal, &personal(BOB), "Bob mail", None, &actor).unwrap();
        let tenant = lifecycle::provision(home.path(), "odoo", ConnectorKind::SharedTenant, &shared(), "Team ERP", None, &actor).unwrap();
        let home_wide = lifecycle::provision(home.path(), "notion", ConnectorKind::SharedHome, &shared(), "Wiki", None, &actor).unwrap();
        let catalog = ConnectorCatalog::open(home.path()).unwrap();
        let row = |value: &Value, id: &AccountId| value["routes"].as_array().unwrap().iter()
            .find(|row| row["account"] == id.as_str()).cloned();

        let (value, revealed) = list_value(&catalog, None, &view(ADMIN, staff(), false)).unwrap();
        assert_eq!((revealed, value["hidden_personal"].as_u64()), (0, Some(1)));
        assert!(row(&value, &bobs).is_none(), "another person's Personal route is hidden");
        assert!(!value.to_string().contains("bob-helper") && !value.to_string().contains(BOB));
        let mine_row = row(&value, &mine).unwrap();
        assert_eq!(labels(&mine_row), ["alice-helper"]);
        assert_eq!(mine_row["employees"][0], json!({"employee_id": hex::encode([1u8; 16]), "label": "alice-helper",
            "kind": "personal", "status": "active", "via_home": false}));
        assert!(mine_row.get("employee_id").is_none(), "only a detail route is bound to one employee");
        assert_eq!(labels(&row(&value, &tenant).unwrap()), ["team-bot"]);
        let wiki = row(&value, &home_wide).unwrap();
        assert_eq!(labels(&wiki), ["alice-helper", "team-bot"], "SharedHome: Shared + (via home) own Personal");
        assert_eq!(wiki["employees"][0]["via_home"], true);
        assert_eq!(wiki["status"], "active");

        // Break-glass reveal: every row and employee, counted for the audit.
        let (value, revealed) = list_value(&catalog, None, &view(ADMIN, staff(), true)).unwrap();
        assert_eq!((revealed, value["hidden_personal"].as_u64(), value["personal_revealed"].as_bool()), (1, Some(0), Some(true)));
        assert_eq!(labels(&row(&value, &bobs).unwrap()), ["bob-helper"]);
        assert_eq!(labels(&row(&value, &home_wide).unwrap()), ["alice-helper", "bob-helper", "team-bot"]);

        // Status from readiness: setup required for this provider (or no build).
        let mut needs = view(ADMIN, staff(), false);
        needs.readiness.as_mut().unwrap().insert("team-bot".into(),
            vec![Reason::ConnectorSetupRequired { provider: Some("odoo") }]);
        let (value, _) = list_value(&catalog, None, &needs).unwrap();
        assert_eq!(row(&value, &tenant).unwrap()["status"], "needs_setup");
        assert_eq!(row(&value, &home_wide).unwrap()["status"], "active", "another provider's setup does not apply");
        needs.readiness.as_mut().unwrap().insert("team-bot".into(), vec![Reason::ConnectorSetupRequired { provider: None }]);
        let (value, _) = list_value(&catalog, None, &needs).unwrap();
        assert_eq!(row(&value, &home_wide).unwrap()["status"], "needs_setup");
        // Fail closed: unreadable readiness or employee map is never `active`.
        for broken in [EmployeeView { readiness: None, ..view(ADMIN, staff(), false) },
            EmployeeView { employees: None, ..view(ADMIN, staff(), false) }] {
            let (value, _) = list_value(&catalog, None, &broken).unwrap();
            assert!(value["routes"].as_array().unwrap().iter().all(|row| row["status"] == "unavailable"), "{value}");
        }
        let mut missing = view(ADMIN, staff(), false);
        missing.readiness.as_mut().unwrap().remove("team-bot");
        let (value, _) = list_value(&catalog, None, &missing).unwrap();
        assert_eq!(row(&value, &tenant).unwrap()["status"], "unavailable");
        // Revoked routes read `revoked`, their employees too.
        catalog.revoke(&tenant).unwrap();
        let (value, _) = list_value(&catalog, None, &view(ADMIN, staff(), false)).unwrap();
        let revoked = row(&value, &tenant).unwrap();
        assert_eq!((revoked["status"].as_str(), revoked["employees"][0]["status"].as_str()), (Some("revoked"), Some("revoked")));
    }

    /// A detail route is bound to the one employee its C6 row pins; one with
    /// no resolvable details needs setup.
    #[test]
    fn f5_detail_route_is_bound_to_its_pinned_employee() {
        let route = |active: bool| Route { account: AccountId::parse(&"a".repeat(32)).unwrap(), label: "DB".into(),
            provider: "database", kind: ConnectorKind::SharedTenant, binding: shared(), generation: 7, active };
        let record = AccountDetailRecord { employee: EmployeeId([3; 16]), agent: "team-bot".into(),
            reference: DetailReference::Database { source: "demo".into() } };
        let all = view(ADMIN, staff(), false);
        let row = route_summary(&route(true), Some(&record), &all);
        assert_eq!((row["employee_id"].as_str(), row["employee_label"].as_str(), row["employee_kind"].as_str()),
            (Some(hex::encode([3u8; 16]).as_str()), Some("team-bot"), Some("shared")));
        assert_eq!((labels(&row), row["status"].as_str()), (vec!["team-bot".to_owned()], Some("active")));
        // Other Shared employees of the tenant never see a pinned route.
        assert_eq!(labels(&row).len(), 1);
        // Pinned to an employee that is no longer live: not `active`.
        let gone = AccountDetailRecord { employee: EmployeeId([9; 16]), ..record.clone() };
        assert_eq!(route_summary(&route(true), Some(&gone), &all)["status"], "unavailable");
        // No details (pre-C6 or older generation): needs setup, no employee.
        let bare = route_summary(&route(true), None, &all);
        assert_eq!((bare["status"].as_str(), bare["employees"].as_array().map(Vec::len)), (Some("needs_setup"), Some(0)));
        assert!(bare.get("employee_id").is_none());
        assert_eq!(route_summary(&route(false), None, &all)["status"], "revoked");
    }

    #[test]
    fn f5_list_request_reveal_is_only_a_step_up_token() {
        assert!(serde_json::from_value::<ListRequest>(json!({"step_up_csrf": vec![7u8; 32]})).is_ok());
        for forged in [json!({"reveal": true}), json!({"step_up_csrf": true}), json!({"owner_user_id": ADMIN})] {
            assert!(serde_json::from_value::<ListRequest>(forged).is_err());
        }
    }
}
