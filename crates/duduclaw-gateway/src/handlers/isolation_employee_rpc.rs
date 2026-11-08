//! Protected employee proposals (native Linux isolation v1.81, task A4,
//! owner decision D-1). Gateway-owned: dispatch routes `isolation.employee_*`
//! and `me.isolation.*` here before any extension.
//!
//! A protected employee exists only through an Employee proposal in the
//! migration confirmation store that an Admin approves on the existing
//! approval card (`migration.confirmations.approve`, password + TOTP
//! step-up); the root allocator then records the immutable principal and the
//! apply path writes the Personal mirror and binds the owner. Nothing in this
//! module approves or applies a proposal.
//!
//! - `me.isolation.request_personal_employee` (any Active, logged-in user
//!   without a forced password change): stages a Pending Employee proposal
//!   for `Personal(caller)`. The owner is always the verified caller; no
//!   request field can name one. The request carries no product
//!   configuration (`settings`), so the approval validator refuses it until
//!   an Admin prepares it with `isolation.employee_propose`.
//! - `me.isolation.list_personal_employee_requests`: the caller's own only.
//! - `isolation.employee_propose` (Admin + step-up): writes the employee's
//!   product configuration through the normal create path (inert until
//!   allocated) and stages the approvable proposal, either as the next
//!   revision of a staff request or as a new Personal/Shared proposal.
//! - `isolation.employee_tombstone` (Admin + step-up): tombstone of a
//!   Personal or Shared employee (`isolation_lifecycle::tombstone_employee`, F6d).
//! - F6d restore RPCs (`isolation_employee_restore.rs`): `me.isolation.list_restorable`,
//!   `restore_challenge`, `request_restore`, `cancel_restore`; `isolation.employee_restore_decide`,
//!   `employee_restore_cancel`, `employee_restore_regrant`, `employee_retire_propose`.
//! - `isolation.employee_list` (Admin): confirmed employees and requests,
//!   plus the step-up state for the dashboard.
//! - `isolation.employee_purge_requests` (Admin + step-up, SR2): removes
//!   rejected/expired staff requests that never reached an apply step.
//! - `me.isolation.list_personal_delegations` /
//!   `me.isolation.decide_personal_delegation` (SR2, owner decision
//!   2026-11-08): the Personal owner's approval cards for delegations from
//!   the employee's live run to a Shared employee.
//!
//! SR2 hardening: only open Employee proposals count toward the store quota;
//! staff requests are rate limited per user ([`MAX_REQUESTS_PER_DAY`]); a
//! Personal proposal adopts an existing `agents/<label>` only with this
//! flow's creation marker (`isolation_employee_dirs.rs`); the propose steps
//! hold the same apply.db writer lock as the request path, so two open
//! proposals for one label cannot coexist.
//!
//! Step-up is the migration dashboard proof (`UserDb::migration_step_up`,
//! written by `migration.confirmations.step_up` or
//! `isolation.connector_step_up`) with its session-bound CSRF token.
use super::*;
use duduclaw_auth::mfa::MigrationConfirmationSession;
use duduclaw_isolation::{bounded::Text, capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::confirmation::{ConfirmationError, ConfirmationStore, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
    ProposalOperationId, ProposalValue, MAX_PROPOSAL_VALUE_BYTES};
use duduclaw_migration::ids::RequestId;
use rusqlite::{Connection, TransactionBehavior};
use crate::fs_safe::sqlite::Access;
use super::isolation_connectors_rpc::authorize_step_up;
use crate::isolation_lifecycle::{self as lifecycle, LifecycleError};

const REQUEST: &str = "me.isolation.request_personal_employee";
const MY_REQUESTS: &str = "me.isolation.list_personal_employee_requests";
const LIST: &str = "isolation.employee_list";
const PROPOSE: &str = "isolation.employee_propose";
const TOMBSTONE: &str = "isolation.employee_tombstone";
const PURGE: &str = "isolation.employee_purge_requests";
pub(super) const METHODS: [&str; 8] = [REQUEST, MY_REQUESTS, LIST, PROPOSE, TOMBSTONE, PURGE,
    delegation::DELEGATIONS, delegation::DECIDE_DELEGATION];

#[path = "isolation_employee_dirs.rs"]
mod employee_dirs;
#[path = "isolation_employee_delegation.rs"]
mod delegation;
/// F6d: restore, decide, cancel, regrant and retire RPCs (`restore::METHODS`).
#[path = "isolation_employee_restore.rs"]
mod restore;

/// `agents.create` in protected mode points here.
pub(super) const DIRECT_CREATE_REFUSED: &str = "受保護模式下不能直接建立 AI 員工。請改用員工配置提案：\
    管理員提出提案並通過密碼與雙因素驗證後，在核准卡片上確認；同仁可在自己的帳號頁申請個人專屬員工，由管理員核准。";

/// SR2 finding 1c: every other writer of a new `agents/<id>` (role and
/// company templates, pack installs) refuses in native Linux protected mode
/// and points to the proposal path. `None` outside protected mode.
pub(super) fn protected_create_refusal(protected: bool) -> Option<WsFrame> {
    protected.then(|| WsFrame::error_response("", DIRECT_CREATE_REFUSED))
}

const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
const APPLY_DB: &str = "migration-authority/apply.db";
/// Root quota of a product-proposed employee; the root derives inodes from it.
const DISK_BYTES: u64 = 1024 * 1024 * 1024;
/// Open Personal requests one user may have at a time.
const MAX_OPEN_REQUESTS: usize = 3;
/// Personal requests one user may file in any 24 hours, whatever happened
/// to them (SR2): a reject/re-request loop cannot flood the store.
const MAX_REQUESTS_PER_DAY: u32 = 10;
const RATE_LIMITED: &str = "你在 24 小時內已送出太多個人專屬員工申請，請稍後再申請。";
const MAX_DISPLAY_CHARS: usize = 64;
const MAX_LISTED: u32 = 500;

const NOT_ALLOWED: &str = "不允許此員工配置操作。";
const ADMIN_REQUIRED: &str = "請以管理員帳號重新登入後再操作。";
const NOT_PROTECTED: &str = "這個環境未啟用受保護模式，不需要申請受保護的 AI 員工。";
const USERS_UNAVAILABLE: &str = "無法讀取帳號資料。";
const SERVICE_UNAVAILABLE: &str = "員工配置服務無法使用，請重新載入。";
const STORE_UNAVAILABLE: &str = "員工配置提案目前無法讀取或寫入，已拒絕操作。";
const SESSION_EXPIRED: &str = "登入憑證已失效，請重新登入。";
const CLOCK_UNAVAILABLE: &str = "無法確認目前時間。";
const REQUEST_INVALID: &str = "申請內容不正確：請填寫員工代號與顯示名稱。";
const PROPOSAL_INVALID: &str = "員工配置提案內容不正確。";
const LABEL_INVALID: &str = "員工代號只能使用小寫英文字母、數字與連字號（最多 64 字元），且不能使用系統保留名稱。";
const DISPLAY_INVALID: &str = "顯示名稱需為 1 到 64 個字，且不能包含控制字元。";
const REQUESTER_INELIGIBLE: &str = "目前帳號無法申請個人專屬員工：帳號必須已啟用、完成登入，且沒有待變更的密碼。";
const OWNER_INELIGIBLE: &str = "這位成員目前無法擁有個人專屬員工，請確認帳號已啟用、完成登入且沒有待變更的密碼。";
const LABEL_TAKEN: &str = "這個員工代號已被使用，請換一個。";
const LABEL_PENDING: &str = "這個員工代號已有待處理的員工配置提案，請換一個。";
const DUPLICATE: &str = "你已經申請過這個員工代號，申請仍在等待管理員處理。";
const TOO_MANY: &str = "你目前有太多待處理的申請，請等管理員處理後再申請。";
const LIMIT: &str = "待處理的員工配置提案已達上限，請等管理員處理後再申請。";
const LABEL_UNAVAILABLE: &str = "這個員工代號目前不能用於這種員工歸屬，請換一個或聯絡管理員。";
const STALE: &str = "申請內容已變更，請重新載入後再試。";
const BINDING_INVALID: &str = "員工的歸屬不正確：個人專屬員工需指定擁有者，共用員工不可指定擁有者。";
const TENANT_UNAVAILABLE: &str = "無法確認此安裝的租戶。";
const REQUEST_NOT_FOUND: &str = "找不到這筆申請。";
const REQUEST_MISMATCH: &str = "這筆申請已處理，或內容與提案不一致，請重新載入。";
const CREATE_FAILED: &str = "無法建立員工設定，請確認名稱後重試。";
const SETTINGS_INVALID: &str = "員工設定無法放入提案，已拒絕操作。";
const EMPLOYEE_NOT_FOUND: &str = "找不到這位員工，請重新載入。";

/// Namespace owned by this handler; unknown members are refused here.
pub(super) fn owns(method: &str) -> bool {
    method.strip_prefix("isolation.").is_some_and(|rest| rest.starts_with("employee_"))
        || method.starts_with("me.isolation.")
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalRequest { label: String, display_name: String }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeRequest {
    label: String,
    #[serde(default)]
    display_name: Option<String>,
    /// `personal` (with `owner_id`, or the owner of `request_id`) or `shared`.
    /// A shared employee always binds this installation's tenant.
    binding_kind: String,
    #[serde(default)]
    owner_id: Option<String>,
    /// A staff request to prepare instead of a new proposal.
    #[serde(default)]
    request_id: Option<String>,
    csrf_token: [u8; 32],
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TombstoneRequest { label: String, employee_id: String, csrf_token: [u8; 32] }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeRequest { csrf_token: [u8; 32] }

/// A checked admin proposal, carried from the first blocking step through the
/// product create to the staging step (which re-checks the step-up).
struct Prepared {
    label: String,
    display_name: String,
    binding: EmployeeBinding,
    /// The staff request this proposal supersedes.
    request: Option<ProposalOperationId>,
    csrf: [u8; 32],
    /// Personal proposals (SR2 1b): the agents directory whose `<label>`
    /// entry must still carry this flow's creation marker at staging.
    adopt: Option<std::path::PathBuf>,
}

impl MethodHandler {
    pub(crate) async fn handle_isolation_employee(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if restore::METHODS.contains(&method) { return self.handle_isolation_restore(method, params, ctx).await; }
        match self.isolation_employee(method, params, ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(message) => WsFrame::error_response("", message),
        }
    }

    async fn isolation_employee(&self, method: &str, params: Value, ctx: &UserContext) -> Result<Value, &'static str> {
        if !METHODS.contains(&method) || ctx.user_id == "system" { return Err(NOT_ALLOWED); }
        // The dispatcher refuses these callers first; fail closed if reached directly.
        if ctx.requires_password_change() { return Err(REQUESTER_INELIGIBLE); }
        let admin = method.starts_with("isolation.");
        if admin && (acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err()) {
            return Err(ADMIN_REQUIRED);
        }
        let session = if admin {
            Some(crate::google_personal::session_for(&ctx.user_id).map_err(|_| ADMIN_REQUIRED)?)
        } else { None };
        if !crate::runtime_readiness::is_protected_linux() { return Err(NOT_PROTECTED); }
        let db = self.user_db.read().await.clone().ok_or(USERS_UNAVAILABLE)?;
        let params = if params.is_null() { json!({}) } else { params };
        let (home, user, epoch) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch);
        match method {
            REQUEST => {
                let input: PersonalRequest = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
                if !valid_label(&input.label) { return Err(LABEL_INVALID); }
                let display = checked_display(&input.display_name)?;
                let taken = self.employee_label_taken(&input.label).await;
                blocking(move || request_personal(&home, &db, &user, &input.label, &display, taken)).await
            }
            MY_REQUESTS => {
                let _: NoParams = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
                blocking(move || list_own_requests(&home, &user)).await
            }
            LIST => {
                let _: NoParams = serde_json::from_value(params).map_err(|_| PROPOSAL_INVALID)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                blocking(move || {
                    let now = now()?;
                    let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
                    let mut value = admin_list(&home)?;
                    // F6d: tombstoned rows, open requests, root state (已註銷).
                    value["restores"] = admin_restores(&home, now);
                    value["step_up_valid"] = json!(proof.is_some());
                    value["csrf_token"] = json!(proof.as_ref().map(|proof| proof.csrf));
                    value["mfa_enrolled"] = json!(db.mfa_active(&user).map_err(|_| USERS_UNAVAILABLE)?);
                    Ok(value)
                }).await
            }
            TOMBSTONE => {
                let input: TombstoneRequest = serde_json::from_value(params).map_err(|_| PROPOSAL_INVALID)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                let (approvals_home, label) = (home.clone(), input.label.clone());
                let employee = parse_hex16(&input.employee_id).map(EmployeeId);
                let done = blocking(move || {
                    let now = now()?;
                    let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
                    tombstone(&home, proof.as_ref(), &user, input, now)
                }).await?;
                // The directory is now in quarantine (SR2 finding 4).
                if let Err(error) = self.registry.write().await.scan().await {
                    warn!(error = %error, "employee tombstone: registry rescan failed");
                }
                // F6h (H5): the tombstone aborted the employee's running bots;
                // drop their handles and status here (exact `<channel>:<label>`).
                for channel in crate::isolation_label_bots::BOT_CHANNELS {
                    self.hot_stop_channel(&format!("{channel}:{label}")).await;
                }
                // F6a (A8): approvals naming the employee die with it.
                if let Some(employee) = employee {
                    invalidate_employee_approvals(&approvals_home, &label, employee).await;
                }
                Ok(done)
            }
            PROPOSE => {
                let input: ProposeRequest = serde_json::from_value(params).map_err(|_| PROPOSAL_INVALID)?;
                self.employee_propose(input, ctx, db, session.ok_or(ADMIN_REQUIRED)?).await
            }
            PURGE => {
                let input: PurgeRequest = serde_json::from_value(params).map_err(|_| PROPOSAL_INVALID)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                blocking(move || {
                    let now = now()?;
                    let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
                    purge_requests(&home, proof.as_ref(), &user, input, now)
                }).await
            }
            delegation::DELEGATIONS => {
                let _: NoParams = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
                delegation::list(&home, &user).await
            }
            delegation::DECIDE_DELEGATION => {
                let input = delegation::parse_decision(params)?;
                delegation::decide(&home, &user, input).await
            }
            _ => Err(NOT_ALLOWED),
        }
    }

    /// Same collision rule as `agents.create`, plus any existing directory.
    /// Any metadata error other than absence counts as taken (fail closed).
    async fn employee_label_taken(&self, label: &str) -> bool {
        let registry = self.registry.read().await;
        registry.list().iter().any(|agent| agent.config.agent.name == label
            || agent.dir.file_name().and_then(|name| name.to_str()) == Some(label))
            || !matches!(std::fs::symlink_metadata(registry.agents_dir().join(label)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }

    async fn employee_propose(&self, input: ProposeRequest, ctx: &UserContext, db: Arc<UserDb>, session: String)
        -> Result<Value, &'static str> {
        let (home, user, epoch) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch);
        let mut prepared = {
            let (home, db, user, session) = (home.clone(), db.clone(), user.clone(), session.clone());
            blocking(move || {
                let now = now()?;
                let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
                prepare_propose(&home, &db, proof.as_ref(), &user, input, now)
            }).await?
        };
        // The product configuration comes from the normal writer and stays
        // inert until the approved proposal is allocated by the root. An
        // existing, unallocated Shared configuration (a retry, or an imported
        // one) is proposed as it is. A Personal proposal (SR2 1b) adopts an
        // existing directory only with this flow's marker for the same
        // owner and an unchanged digest; it never adopts a foreign one.
        let agents_dir = self.registry.read().await.agents_dir().to_path_buf();
        let owner = match &prepared.binding {
            EmployeeBinding::Personal(owner) => Some(owner.as_str().to_owned()),
            EmployeeBinding::Shared(_) => None,
        };
        let exists = self.registry.read().await.get(&prepared.label).is_some();
        let present = exists || !matches!(std::fs::symlink_metadata(agents_dir.join(&prepared.label)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        let created = match (&owner, present) {
            (Some(owner), true) => {
                let (home, agents_dir, label, owner) = (home.clone(), agents_dir.clone(), prepared.label.clone(), owner.clone());
                blocking(move || employee_dirs::adopt_existing(&home, &agents_dir, &label, &owner).map(|_| ())).await?;
                false
            }
            (None, true) if exists => false,
            (None, true) => return Err(LABEL_TAKEN),
            (_, false) => {
                if self.employee_label_taken(&prepared.label).await { return Err(LABEL_TAKEN); }
                let frame = self.create_agent_files(
                    json!({ "name": prepared.label, "display_name": prepared.display_name }), Some(ctx)).await;
                if !matches!(frame, WsFrame::Response { ok: true, .. }) {
                    warn!(agent = %prepared.label, "employee proposal: product create refused");
                    return Err(CREATE_FAILED);
                }
                if let Some(owner) = &owner {
                    let (home, dir, label, owner) = (home.clone(), agents_dir.clone(), prepared.label.clone(), owner.clone());
                    if let Err(error) = blocking(move || employee_dirs::record_marker(&home, &dir, &label, &owner)).await {
                        // Without a marker the directory could never be
                        // proposed; remove what this call just created.
                        warn!(agent = %prepared.label, "employee proposal: creation marker not recorded");
                        let _ = tokio::fs::remove_dir_all(agents_dir.join(&prepared.label)).await;
                        let _ = self.registry.write().await.scan().await;
                        return Err(error);
                    }
                }
                true
            }
        };
        let settings = if owner.is_some() {
            // SR2 1b (round 3): a Personal proposal's settings are parsed from
            // exactly the agent.toml bytes inside the adopted digest, never
            // from the registry entry looked up by name. The live registry
            // entry (what the approval validator compares) must be this very
            // directory with the same settings, or nothing is staged.
            let (home, dir, label) = (home.clone(), agents_dir.clone(), prepared.label.clone());
            let settings = blocking(move || employee_dirs::snapshot(&home, &dir, &label)?.settings()).await?;
            let registry = self.registry.read().await;
            let live = registry.get(&prepared.label).ok_or(CREATE_FAILED)?;
            if live.dir.file_name().and_then(|name| name.to_str()) != Some(prepared.label.as_str())
                || settings_for(&live.config)? != settings {
                return Err(employee_dirs::FOREIGN_DIR);
            }
            settings
        } else {
            let registry = self.registry.read().await;
            settings_for(&registry.get(&prepared.label).ok_or(CREATE_FAILED)?.config)?
        };
        if owner.is_some() { prepared.adopt = Some(agents_dir); }
        let mut staged = blocking(move || {
            let now = now()?;
            let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
            stage_prepared(&home, proof.as_ref(), &user, &prepared, &settings, now)
        }).await?;
        staged["agent_created"] = json!(created);
        Ok(staged)
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, &'static str> + Send + 'static)
    -> Result<T, &'static str> {
    tokio::task::spawn_blocking(work).await.map_err(|_| SERVICE_UNAVAILABLE)?
}

fn now() -> Result<i64, &'static str> {
    let now = chrono::Utc::now().timestamp();
    if now < 0 { Err(CLOCK_UNAVAILABLE) } else { Ok(now) }
}

fn valid_label(label: &str) -> bool {
    duduclaw_core::is_valid_new_agent_id(label) && !duduclaw_core::is_reserved_agent_id(label)
        && label != duduclaw_core::UNTRUSTED_AGENT_ID
}

fn valid_existing_label(label: &str) -> bool {
    duduclaw_core::is_valid_agent_id(label) && !duduclaw_core::is_reserved_agent_id(label)
        && label != duduclaw_core::UNTRUSTED_AGENT_ID
}

fn checked_display(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_DISPLAY_CHARS || name.chars().any(char::is_control) {
        return Err(DISPLAY_INVALID);
    }
    Ok(name.to_owned())
}

/// Same rule as the Personal mirror apply path and C3 connector owners:
/// Active, logged in, no forced password change.
fn eligible_owner(db: &UserDb, id: &str) -> Result<(), &'static str> {
    let user = db.get_user(id).map_err(|_| USERS_UNAVAILABLE)?.ok_or(OWNER_INELIGIBLE)?;
    if user.id != id || user.status != duduclaw_auth::UserStatus::Active || user.must_change_password
        || user.last_login.as_deref().is_none_or(str::is_empty) {
        return Err(OWNER_INELIGIBLE);
    }
    Ok(())
}

/// Personal: an eligible owner. Shared: this installation's tenant from
/// users.db; no request can name a tenant.
fn resolve_binding(home: &Path, db: &UserDb, kind: &str, owner: Option<&str>) -> Result<EmployeeBinding, &'static str> {
    match (kind, owner) {
        ("personal", Some(owner)) => {
            eligible_owner(db, owner)?;
            Text::new(owner.to_owned()).map(EmployeeBinding::Personal).map_err(|_| BINDING_INVALID)
        }
        ("shared", None) => {
            // SR2 7: the no-follow, never-create authority-store open.
            let users = crate::fs_safe::sqlite::open_existing(home, "users.db", Access::ReadOnly)
                .map_err(|_| USERS_UNAVAILABLE)?;
            let tenant = duduclaw_auth::isolation::read_isolation_tenant(&users).map_err(|_| TENANT_UNAVAILABLE)?;
            Text::new(tenant).map(EmployeeBinding::Shared).map_err(|_| BINDING_INVALID)
        }
        _ => Err(BINDING_INVALID),
    }
}

fn binding_error(error: LifecycleError) -> &'static str {
    match error {
        LifecycleError::Unavailable => STORE_UNAVAILABLE,
        LifecycleError::Denied | LifecycleError::Stale | LifecycleError::Tombstoned => LABEL_UNAVAILABLE,
    }
}

fn stage_error(error: ConfirmationError) -> &'static str {
    match error {
        ConfirmationError::Limit => LIMIT,
        ConfirmationError::Denied => LABEL_UNAVAILABLE,
        ConfirmationError::Stale => STALE,
        ConfirmationError::Storage => STORE_UNAVAILABLE,
        _ => PROPOSAL_INVALID,
    }
}

/// Staging-time validation of every product Employee proposal. Approval and
/// apply re-validate (`EmployeeAuthority`), including the live configuration.
struct StagingCheck<'a> { home: &'a Path }
impl ProposalValidator for StagingCheck<'_> {
    fn validate_fresh(&self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
        item.validate().map_err(|_| ConfirmationError::Invalid)?;
        let spec = allocation::specification(item).map_err(|_| ConfirmationError::Invalid)?;
        if item.fields.iter().any(|f| !matches!(f.name.as_str(),
            "employee_id" | "disk_bytes" | "settings" | "binding_kind" | "owner_id" | "tenant_id")) {
            return Err(ConfirmationError::Denied);
        }
        // A fresh product proposal always names its principal.
        let binding = spec.binding.ok_or(ConfirmationError::Denied)?;
        lifecycle::validate_proposed_binding(self.home, &spec.label, Some(&binding)).map_err(|error| match error {
            LifecycleError::Unavailable => ConfirmationError::Storage,
            _ => ConfirmationError::Denied,
        })
    }
}

fn text_field(name: &str, value: &str) -> ProposalField {
    ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }
}

fn principal_fields(binding: &EmployeeBinding) -> [ProposalField; 2] {
    match binding {
        EmployeeBinding::Personal(owner) => [text_field("binding_kind", "personal"), text_field("owner_id", owner.as_str())],
        EmployeeBinding::Shared(tenant) => [text_field("binding_kind", "shared"), text_field("tenant_id", tenant.as_str())],
    }
}

/// Exact-name lookup of a text field; never a substring match.
fn field<'a>(item: &'a ConfirmationItem, name: &str) -> Option<&'a str> {
    match item.fields.iter().find(|f| f.name == name).map(|f| &f.value) {
        Some(ProposalValue::Text { value }) => Some(value.as_str()),
        _ => None,
    }
}

/// An Employee proposal that still holds its label: waiting for a decision
/// or being applied/compensated. A Failed employee proposal is never retried
/// (a new proposal is required), so it does not hold the label.
fn is_open_employee(item: &ConfirmationItem) -> bool {
    item.category == ConfirmationCategory::Employee && matches!(item.status,
        ConfirmationStatus::Pending | ConfirmationStatus::Approved | ConfirmationStatus::Applying | ConfirmationStatus::Rejecting)
}

fn latest(items: &[ConfirmationItem], id: RequestId) -> Option<&ConfirmationItem> {
    items.iter().filter(|item| item.binding.operation_id.proposal_id == id)
        .max_by_key(|item| item.binding.operation_id.revision)
}

fn status_name(status: ConfirmationStatus) -> &'static str {
    match status {
        ConfirmationStatus::Pending => "pending",
        ConfirmationStatus::Approved | ConfirmationStatus::Applying => "applying",
        ConfirmationStatus::Applied => "applied",
        ConfirmationStatus::Failed => "failed",
        ConfirmationStatus::Rejecting | ConfirmationStatus::Rejected => "rejected",
        ConfirmationStatus::ExpiredRejected => "expired",
    }
}

fn new_proposal_id() -> RequestId { RequestId(*uuid::Uuid::new_v4().as_bytes()) }

/// Lowercase hex of exactly 16 bytes; anything else is not an id.
fn parse_hex16(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    hex::decode(value).ok()?.try_into().ok()
}

/// The gateway-owned confirmation store. On a protected home that never ran a
/// migration, the first Employee proposal creates it exactly as import
/// staging does (private file, schema, no proposals).
fn open_confirmations(home: &Path) -> Result<ConfirmationStore, &'static str> {
    let path = home.join(CONFIRMATIONS_DB);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            super::stage_import_proposals(home, &[]).map_err(|_| STORE_UNAVAILABLE)?,
        // Same acceptance as the approval RPC (`migration_confirmations_rpc`).
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 64 * 1024 * 1024 => {}
        _ => return Err(STORE_UNAVAILABLE),
    }
    ConfirmationStore::open_existing(&path).map_err(|_| STORE_UNAVAILABLE)
}

/// Read-only view; an absent store has no proposals.
fn read_items(home: &Path) -> Result<Vec<ConfirmationItem>, &'static str> {
    let path = home.join(CONFIRMATIONS_DB);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 64 * 1024 * 1024 => {}
        _ => return Err(STORE_UNAVAILABLE),
    }
    ConfirmationStore::open_read_only(&path).and_then(|store| store.list()).map_err(|_| STORE_UNAVAILABLE)
}

/// The request index (display names) lives in the private apply.db next to
/// the confirmed-employee index. It is presentation only: status and owner
/// always come from the digest-bound proposal.
fn open_requests(home: &Path) -> Result<Connection, &'static str> {
    // Creates or validates the private migration-authority directory and apply.db.
    super::migration_authority::GatewayAuthority::new(home.to_path_buf()).map_err(|_| STORE_UNAVAILABLE)?;
    // SR2 7: then opened like every other authority store (no-follow walk,
    // owner/mode/link checks, never created by SQLite).
    let db = open_apply(home, Access::ReadWrite)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| STORE_UNAVAILABLE)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS personal_employee_requests (
            request_id TEXT PRIMARY KEY, owner_user_id TEXT NOT NULL, label TEXT NOT NULL,
            display_name TEXT NOT NULL, created_at TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_personal_employee_requests_owner ON personal_employee_requests(owner_user_id);
        CREATE TABLE IF NOT EXISTS personal_employee_request_log (
            owner_user_id TEXT NOT NULL, created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_personal_employee_request_log_owner
            ON personal_employee_request_log(owner_user_id, created_at);")
        .map_err(|_| STORE_UNAVAILABLE)?;
    db.execute_batch(employee_dirs::MARKER_SCHEMA).map_err(|_| STORE_UNAVAILABLE)?;
    Ok(db)
}

/// `migration-authority/apply.db` through
/// [`crate::fs_safe::sqlite::open_existing`] (SR2 7): no-follow descriptor
/// walk to the directory, owner/mode/single-link and dev+inode checks on the
/// store and its sidecars, `SQLITE_OPEN_NOFOLLOW`, never created.
fn open_apply(home: &Path, access: Access) -> Result<Connection, &'static str> {
    let (directory, name) = APPLY_DB.rsplit_once('/').ok_or(STORE_UNAVAILABLE)?;
    let db = crate::fs_safe::sqlite::open_existing(&home.join(directory), name, access).map_err(|_| STORE_UNAVAILABLE)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| STORE_UNAVAILABLE)?;
    Ok(db)
}

struct RequestRow { request_id: String, owner: String, label: String, display_name: String, created_at: String }

/// Index rows, newest first; `owner` filters by exact user id.
fn request_rows(home: &Path, owner: Option<&str>) -> Result<Vec<RequestRow>, &'static str> {
    let path = home.join(APPLY_DB);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(metadata) if metadata.file_type().is_file() => {}
        _ => return Err(STORE_UNAVAILABLE),
    }
    let db = open_apply(home, Access::ReadOnly)?;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='personal_employee_requests')",
        [], |r| r.get(0)).map_err(|_| STORE_UNAVAILABLE)?;
    if !table { return Ok(Vec::new()); }
    let mut query = db.prepare("SELECT request_id,owner_user_id,label,display_name,created_at FROM personal_employee_requests
        WHERE ?1 IS NULL OR owner_user_id=?1 ORDER BY created_at DESC, request_id LIMIT ?2").map_err(|_| STORE_UNAVAILABLE)?;
    let rows = query.query_map(params![owner, MAX_LISTED], |r| Ok(RequestRow {
        request_id: r.get(0)?, owner: r.get(1)?, label: r.get(2)?, display_name: r.get(3)?, created_at: r.get(4)?,
    })).map_err(|_| STORE_UNAVAILABLE)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|_| STORE_UNAVAILABLE)
}

fn request_display_name(home: &Path, id: RequestId) -> Result<Option<String>, &'static str> {
    let id = hex::encode(id.0);
    Ok(request_rows(home, None)?.into_iter().find(|row| row.request_id == id).map(|row| row.display_name))
}

/// One request joined with its proposal. `None` when the index row does not
/// match the digest-bound proposal (a forged or stale row is never shown).
fn request_value(row: &RequestRow, items: &[ConfirmationItem]) -> Option<Value> {
    let item = latest(items, RequestId(parse_hex16(&row.request_id)?))?;
    if item.category != ConfirmationCategory::Employee || field(item, "binding_kind") != Some("personal")
        || field(item, "owner_id") != Some(row.owner.as_str()) || field(item, "employee_id") != Some(row.label.as_str()) {
        return None;
    }
    Some(json!({
        "request_id": row.request_id, "label": row.label, "display_name": row.display_name,
        "created_at": row.created_at, "status": status_name(item.status),
        // `requested`: waiting for an Admin to prepare it; `proposed`: on the approval card.
        "stage": if field(item, "settings").is_some() { "proposed" } else { "requested" },
    }))
}

/// `me.isolation.request_personal_employee`. `caller` is the verified
/// dashboard user, the only possible owner.
fn request_personal(home: &Path, db: &UserDb, caller: &str, label: &str, display: &str, label_taken: bool)
    -> Result<Value, &'static str> {
    eligible_owner(db, caller).map_err(|error| if error == USERS_UNAVAILABLE { error } else { REQUESTER_INELIGIBLE })?;
    if !valid_label(label) { return Err(LABEL_INVALID); }
    let display = checked_display(display)?;
    if label_taken { return Err(LABEL_TAKEN); }
    let owner = EmployeeBinding::Personal(Text::new(caller.to_owned()).map_err(|_| REQUESTER_INELIGIBLE)?);
    // Also creates the private apply.db the lifecycle check reads.
    let mut requests = open_requests(home)?;
    lifecycle::validate_proposed_binding(home, label, Some(&owner)).map_err(binding_error)?;
    // The index writer lock serializes concurrent requests across the
    // duplicate check, the staging and the index row.
    let tx = requests.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORE_UNAVAILABLE)?;
    let mut store = open_confirmations(home)?;
    let items = store.list().map_err(|_| STORE_UNAVAILABLE)?;
    let mine: Vec<&ConfirmationItem> = items.iter().filter(|item| is_open_employee(item)
        && field(item, "binding_kind") == Some("personal") && field(item, "owner_id") == Some(caller)).collect();
    if mine.iter().any(|item| field(item, "employee_id") == Some(label)) { return Err(DUPLICATE); }
    if items.iter().any(|item| is_open_employee(item) && field(item, "employee_id") == Some(label)) { return Err(LABEL_PENDING); }
    if mine.len() >= MAX_OPEN_REQUESTS { return Err(TOO_MANY); }
    // SR2: at most MAX_REQUESTS_PER_DAY filed in any 24 hours. The log is
    // separate from the request index, so a purge never resets it.
    tx.execute("DELETE FROM personal_employee_request_log WHERE created_at <= CAST(strftime('%s','now') AS INTEGER) - 86400",
        []).map_err(|_| STORE_UNAVAILABLE)?;
    let recent: u32 = tx.query_row("SELECT COUNT(*) FROM personal_employee_request_log WHERE owner_user_id=?1
        AND created_at > CAST(strftime('%s','now') AS INTEGER) - 86400", [caller], |r| r.get(0))
        .map_err(|_| STORE_UNAVAILABLE)?;
    if recent >= MAX_REQUESTS_PER_DAY { return Err(RATE_LIMITED); }
    let operation = ProposalOperationId { proposal_id: new_proposal_id(), revision: 1 };
    let mut fields = vec![text_field("employee_id", label), text_field("disk_bytes", &DISK_BYTES.to_string())];
    fields.extend(principal_fields(&owner));
    let item = store.stage_proposal(operation, ConfirmationCategory::Employee, fields, &StagingCheck { home })
        .map_err(stage_error)?;
    let request_id = hex::encode(item.binding.operation_id.proposal_id.0);
    let created_at = chrono::Utc::now().to_rfc3339();
    tx.execute("INSERT INTO personal_employee_requests(request_id,owner_user_id,label,display_name,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![request_id, caller, label, display, created_at]).map_err(|_| STORE_UNAVAILABLE)?;
    tx.execute("INSERT INTO personal_employee_request_log(owner_user_id,created_at) VALUES(?1,CAST(strftime('%s','now') AS INTEGER))",
        [caller]).map_err(|_| STORE_UNAVAILABLE)?;
    tx.commit().map_err(|_| STORE_UNAVAILABLE)?;
    Ok(json!({ "request_id": request_id, "label": label, "display_name": display, "created_at": created_at,
        "status": "pending", "stage": "requested", "approval_required": true }))
}

/// `me.isolation.list_personal_employee_requests`: the caller's own requests.
fn list_own_requests(home: &Path, caller: &str) -> Result<Value, &'static str> {
    let items = read_items(home)?;
    let requests: Vec<Value> = request_rows(home, Some(caller))?.iter()
        .filter(|row| row.owner == caller)
        .filter_map(|row| request_value(row, &items)).collect();
    Ok(json!({ "requests": requests }))
}

/// Admin read: confirmed employees (no tenant id) and all requests.
fn admin_list(home: &Path) -> Result<Value, &'static str> {
    let items = read_items(home)?;
    let requests: Vec<Value> = request_rows(home, None)?.iter().filter_map(|row| {
        let mut value = request_value(row, &items)?;
        value["owner_user_id"] = json!(row.owner);
        Some(value)
    }).collect();
    Ok(json!({ "employees": confirmed_employees(home)?, "requests": requests }))
}

fn confirmed_employees(home: &Path) -> Result<Vec<Value>, &'static str> {
    let path = home.join(APPLY_DB);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(metadata) if metadata.file_type().is_file() => {}
        _ => return Err(STORE_UNAVAILABLE),
    }
    let db = open_apply(home, Access::ReadOnly)?;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| STORE_UNAVAILABLE)?;
    if !table { return Ok(Vec::new()); }
    let mut query = db.prepare("SELECT label,receipt FROM confirmed_employees ORDER BY label LIMIT ?1").map_err(|_| STORE_UNAVAILABLE)?;
    let rows = query.query_map([MAX_LISTED], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|_| STORE_UNAVAILABLE)?.collect::<Result<Vec<_>, _>>().map_err(|_| STORE_UNAVAILABLE)?;
    rows.into_iter().map(|(label, encoded)| {
        // A corrupt row refuses the whole listing rather than hiding an employee.
        let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| STORE_UNAVAILABLE)?;
        if receipt.label != label { return Err(STORE_UNAVAILABLE); }
        let binding = match &receipt.principal_binding {
            Some(EmployeeBinding::Personal(owner)) => json!({ "type": "personal", "owner_user_id": owner.as_str() }),
            Some(EmployeeBinding::Shared(_)) | None => json!({ "type": "shared" }),
        };
        Ok(json!({ "label": label, "employee_id": hex::encode(receipt.employee.0), "binding": binding,
            "disabled": receipt.disabled }))
    }).collect()
}

/// First admin step: step-up, principal, request match, label checks.
fn prepare_propose(home: &Path, db: &UserDb, proof: Option<&MigrationConfirmationSession>, user: &str,
    input: ProposeRequest, now: i64) -> Result<Prepared, &'static str> {
    authorize_step_up(proof, user, &input.csrf_token, now)?;
    if !valid_label(&input.label) { return Err(LABEL_INVALID); }
    // SR2 finding 6: the request path's apply.db writer lock, held across
    // the open-label check (and taken again by `stage_prepared`).
    let mut requests = open_requests(home)?;
    let lock = requests.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORE_UNAVAILABLE)?;
    let items = read_items(home)?;
    let (binding, request) = match input.request_id.as_deref() {
        Some(id) => {
            let id = RequestId(parse_hex16(id).ok_or(REQUEST_NOT_FOUND)?);
            let item = latest(&items, id).ok_or(REQUEST_NOT_FOUND)?;
            let owner = field(item, "owner_id").ok_or(REQUEST_MISMATCH)?;
            if input.binding_kind != "personal" || input.owner_id.as_deref().is_some_and(|named| named != owner)
                || item.category != ConfirmationCategory::Employee || item.status != ConfirmationStatus::Pending
                || field(item, "settings").is_some() || field(item, "binding_kind") != Some("personal")
                || field(item, "employee_id") != Some(input.label.as_str()) {
                return Err(REQUEST_MISMATCH);
            }
            (resolve_binding(home, db, "personal", Some(owner))?, Some(item.binding.operation_id))
        }
        None => (resolve_binding(home, db, &input.binding_kind, input.owner_id.as_deref())?, None),
    };
    // One open Employee proposal per label; the request itself is superseded.
    if label_pending(&items, &input.label, request) { return Err(LABEL_PENDING); }
    crate::isolation_label_stores::proposal_refusal(home, &input.label)?; // F6h R7: LABEL_QUARANTINED
    lifecycle::validate_proposed_binding(home, &input.label, Some(&binding)).map_err(binding_error)?;
    crate::isolation_restore_rpc::refuse_while_restoring(&lock, &input.label)?; // F6d §3.5: RESTORE_IN_PROGRESS
    let display_name = match (input.display_name.as_deref(), request) {
        (Some(name), _) => checked_display(name)?,
        (None, Some(op)) => request_display_name(home, op.proposal_id)?.unwrap_or_else(|| input.label.clone()),
        (None, None) => input.label.clone(),
    };
    lock.finish().map_err(|_| STORE_UNAVAILABLE)?;
    Ok(Prepared { label: input.label, display_name, binding, request, csrf: input.csrf_token, adopt: None })
}

/// Another open Employee proposal holds `label` (the staff request that
/// `request` supersedes does not count).
fn label_pending(items: &[ConfirmationItem], label: &str, request: Option<ProposalOperationId>) -> bool {
    items.iter().any(|item| is_open_employee(item) && field(item, "employee_id") == Some(label)
        && request.is_none_or(|op| item.binding.operation_id.proposal_id != op.proposal_id))
}

/// The live configuration as the approval validator compares it: the text
/// must re-parse to exactly the serialized value, or the proposal could never
/// be approved (or worse, approve something else).
fn settings_for(config: &impl serde::Serialize) -> Result<String, &'static str> {
    let value = serde_json::to_value(config).map_err(|_| SETTINGS_INVALID)?;
    let encoded = serde_json::to_string(&value).map_err(|_| SETTINGS_INVALID)?;
    if encoded.len() > MAX_PROPOSAL_VALUE_BYTES || serde_json::from_str::<Value>(&encoded).ok().as_ref() != Some(&value) {
        return Err(SETTINGS_INVALID);
    }
    Ok(encoded)
}

/// Last admin step: the step-up is checked again, then the proposal is
/// staged Pending. It is never approved here.
fn stage_prepared(home: &Path, proof: Option<&MigrationConfirmationSession>, user: &str, prepared: &Prepared,
    settings: &str, now: i64) -> Result<Value, &'static str> {
    authorize_step_up(proof, user, &prepared.csrf, now)?;
    let operation = match prepared.request {
        Some(op) => ProposalOperationId { proposal_id: op.proposal_id, revision: op.revision.checked_add(1).ok_or(STALE)? },
        None => ProposalOperationId { proposal_id: new_proposal_id(), revision: 1 },
    };
    let mut fields = vec![text_field("employee_id", &prepared.label), text_field("disk_bytes", &DISK_BYTES.to_string()),
        text_field("settings", settings)];
    fields.extend(principal_fields(&prepared.binding));
    // SR2 finding 6: the same apply.db writer lock as the request path,
    // across the open-label recheck and the staging, so a concurrent request
    // or proposal for this label cannot slip in between.
    let mut requests = open_requests(home)?;
    let lock = requests.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORE_UNAVAILABLE)?;
    let mut store = open_confirmations(home)?;
    let items = store.list().map_err(|_| STORE_UNAVAILABLE)?;
    if label_pending(&items, &prepared.label, prepared.request) { return Err(LABEL_PENDING); }
    crate::isolation_label_stores::proposal_refusal(home, &prepared.label)?; // F6h R7: LABEL_QUARANTINED
    crate::isolation_restore_rpc::refuse_while_restoring(&lock, &prepared.label)?; // F6d §3.5: RESTORE_IN_PROGRESS
    // SR2 finding 1b: the adopted directory still carries this flow's marker,
    // holds nothing outside the allowlist, and the staged settings are
    // exactly those parsed from the agent.toml bytes inside that digest.
    if let (Some(agents_dir), EmployeeBinding::Personal(owner)) = (&prepared.adopt, &prepared.binding) {
        let adopted = employee_dirs::check_marker(&lock, home, agents_dir, &prepared.label, owner.as_str())?;
        if adopted.settings()? != settings { return Err(STALE); }
    }
    let item = store.stage_proposal(operation, ConfirmationCategory::Employee, fields, &StagingCheck { home })
        .map_err(stage_error)?;
    lock.commit().map_err(|_| STORE_UNAVAILABLE)?;
    Ok(json!({
        "request_id": hex::encode(item.binding.operation_id.proposal_id.0),
        "revision": item.binding.operation_id.revision,
        "label": prepared.label,
        "status": status_name(item.status),
        "approval_required": true,
    }))
}

/// `isolation.employee_purge_requests` (Admin + step-up, SR2 finding 3):
/// staff requests whose proposal is closed (latest revision Rejected or
/// ExpiredRejected) and never reached an apply step are removed from the
/// confirmation store and the request index. Open, applied or compensated
/// proposals and import-staged proposals are never touched; the per-user
/// rate-limit log is kept.
fn purge_requests(home: &Path, proof: Option<&MigrationConfirmationSession>, user: &str, input: PurgeRequest, now: i64)
    -> Result<Value, &'static str> {
    authorize_step_up(proof, user, &input.csrf_token, now)?;
    let proof = proof.ok_or(ADMIN_REQUIRED)?;
    let actor = uuid::Uuid::parse_str(user).map_err(|_| ADMIN_REQUIRED)?;
    let session = duduclaw_migration::confirmation::ApprovalSession::from_verified_session(true, true,
        Some(proof.verified_at), proof.csrf, RequestId(*actor.as_bytes()));
    let mut requests = open_requests(home)?;
    let tx = requests.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORE_UNAVAILABLE)?;
    let indexed: Vec<String> = {
        let mut query = tx.prepare("SELECT request_id FROM personal_employee_requests LIMIT 10000").map_err(|_| STORE_UNAVAILABLE)?;
        let rows = query.query_map([], |r| r.get::<_, String>(0)).map_err(|_| STORE_UNAVAILABLE)?;
        rows.collect::<Result<_, _>>().map_err(|_| STORE_UNAVAILABLE)?
    };
    let mut store = open_confirmations(home)?;
    let items = store.list().map_err(|_| STORE_UNAVAILABLE)?;
    let closed: Vec<RequestId> = indexed.iter().filter_map(|id| parse_hex16(id).map(RequestId))
        .filter(|id| latest(&items, *id).is_some_and(|item| item.category == ConfirmationCategory::Employee
            && matches!(item.status, ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected)))
        .collect();
    let mut purged = 0;
    for batch in closed.chunks(200) {
        purged += store.purge_closed_employee(batch, &session, input.csrf_token, u64::try_from(now).map_err(|_| CLOCK_UNAVAILABLE)?)
            .map_err(stage_error)?;
    }
    // Drop index rows whose proposal is gone (purged now, or never staged).
    let remaining = store.list().map_err(|_| STORE_UNAVAILABLE)?;
    let mut removed = 0;
    for id in &indexed {
        let present = parse_hex16(id).is_some_and(|bytes| latest(&remaining, RequestId(bytes)).is_some());
        if !present {
            removed += tx.execute("DELETE FROM personal_employee_requests WHERE request_id=?1", [id])
                .map_err(|_| STORE_UNAVAILABLE)?;
        }
    }
    tx.commit().map_err(|_| STORE_UNAVAILABLE)?;
    Ok(json!({ "purged": purged, "index_rows_removed": removed }))
}

/// F6d: the admin `已註銷` view inside `isolation.employee_list`; an
/// unreadable or absent store is reported, never an empty "nothing tombstoned".
fn admin_restores(home: &Path, now: i64) -> Value {
    if matches!(std::fs::symlink_metadata(home.join(APPLY_DB)), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        return json!({"quarantine": [], "requests": []});
    }
    crate::isolation_restore_rpc::admin_view(home, now)
        .unwrap_or_else(|error| json!({"unavailable": true, "code": error.code()}))
}

/// `isolation.employee_tombstone`: tombstone of a Personal or Shared employee.
///
/// F6a: runs the sealing tombstone (`lifecycle::tombstone_employee`, §3.2)
/// and returns the seal state (Q6), the revoked detail routes and the
/// pending fences. F6d (§3.9): Shared rows are accepted too (the lifecycle
/// function snapshots the tenant pin and members and removes the bindings);
/// refused `RESTORE_IN_PROGRESS` while a restore of the label is applying.
fn tombstone(home: &Path, proof: Option<&MigrationConfirmationSession>, user: &str, input: TombstoneRequest, now: i64)
    -> Result<Value, &'static str> {
    authorize_step_up(proof, user, &input.csrf_token, now)?;
    if !valid_existing_label(&input.label) { return Err(LABEL_INVALID); }
    let employee = EmployeeId(parse_hex16(&input.employee_id).ok_or(EMPLOYEE_NOT_FOUND)?);
    let refusal = |error| match error {
        LifecycleError::Unavailable => STORE_UNAVAILABLE,
        LifecycleError::Denied | LifecycleError::Tombstoned => EMPLOYEE_NOT_FOUND,
        LifecycleError::Stale => STALE,
    };
    // Personal or Shared; the principal is read from the stored receipt,
    // never from the request. An unknown label is not found.
    if lifecycle::confirmed_receipt(home, &input.label).map_err(refusal)?.is_none() { return Err(EMPLOYEE_NOT_FOUND); }
    // §3.5 mutual exclusion, read on the private apply.db (exists: a receipt was read).
    crate::isolation_restore_rpc::refuse_while_restoring(&open_apply(home, Access::ReadOnly)?, &input.label)?;
    let env = lifecycle::TombstoneEnv { actor: user, runs: &crate::isolation_quarantine::SpawndRunStopper, now };
    let outcome = lifecycle::tombstone_employee(home, &input.label, employee, &env).map_err(refusal)?;
    Ok(json!({ "label": input.label, "tombstoned": true, "readback_required": true,
        "incarnation": outcome.incarnation, "seal": outcome.seal, "digest_prefix": outcome.digest_prefix,
        "routes_revoked": outcome.routes_revoked.len(), "pending_fences": outcome.pending_fences,
        "runs_stopped": outcome.runs_stopped.is_some() }))
}

/// F6a (A8): after a tombstone, invalidate every open or approved approval
/// card naming the employee's label (its own cards, and Personal-delegation
/// cards from or to it). A failure is recorded as the `approvals:` pending
/// fence (H2), which blocks a restore until a retry succeeds.
pub(crate) async fn invalidate_employee_approvals(home: &Path, label: &str, employee: EmployeeId) {
    use crate::approval::{ApprovalBroker, ApprovalStatus};
    let result = async {
        let broker = ApprovalBroker::open(home).map_err(|_| ())?;
        let mut ids = Vec::new();
        for card in broker.list_pending(Some(label)).await.map_err(|_| ())? { ids.push(card.id); }
        for card in broker.list_by_kind(crate::isolation_background::PERSONAL_DELEGATION_KIND).await.map_err(|_| ())? {
            let names = card.agent_id == label
                || card.payload.get("target_agent").and_then(Value::as_str) == Some(label)
                || card.payload.get("source_agent").and_then(Value::as_str) == Some(label);
            if names && matches!(card.status, ApprovalStatus::Pending | ApprovalStatus::Approved) { ids.push(card.id); }
        }
        let mut invalidated = 0usize;
        for id in ids {
            broker.invalidate_request(&id, "employee_tombstoned").await.map_err(|_| ())?;
            invalidated += 1;
        }
        Ok::<usize, ()>(invalidated)
    }.await;
    let fences: Vec<String> = match result {
        Ok(_) => Vec::new(),
        Err(()) => vec!["approvals:unavailable".to_owned()],
    };
    let (home, label) = (home.to_path_buf(), label.to_owned());
    let recorded = tokio::task::spawn_blocking(move || lifecycle::record_fence_group(&home, &label, employee, "approvals:", &fences))
        .await;
    if !matches!(recorded, Ok(Ok(()))) {
        warn!("employee tombstone: approval fence could not be recorded");
    }
}

#[cfg(test)]
#[path = "isolation_employee_rpc_tests.rs"]
mod tests;
