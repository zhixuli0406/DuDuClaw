//! R8r-3a: dashboard RPCs for an isolated employee's model connection
//! document (`/home/employee/.duduclaw-api/providers.json`, read only by that
//! employee's sandboxed OpenAI-compatible runtime). Gateway-owned: dispatch
//! routes the whole `isolation.provider_` namespace here before any gate,
//! plugin or extension sees the request; unknown members are refused.
//!
//! - `isolation.provider_status` — Admin. What the gateway recorded after
//!   confirmed dashboard changes (names, protocols, endpoint origins, time);
//!   never a key, path, query or hash. Documents written by the host root
//!   command (`duduclaw service provider set`) bypass the gateway and are not
//!   visible here. Another person's Personal employees are hidden unless the
//!   request carries the step-up CSRF token, which reveals them and is
//!   audited (the connector list's P1 rule). Hidden means absent: no count
//!   (`hidden_personal: null`; `0` when revealed), a filtered request for one
//!   answers as an unknown id, and paging runs over the visible rows only.
//! - `isolation.provider_set` / `isolation.provider_clear` — exactly the
//!   connector PROVISION gate: a current Admin re-read from users.db, the
//!   caller's dashboard session, the fresh dashboard step-up (password +
//!   second factor, five minutes, bound to this session and credential epoch:
//!   `UserDb::migration_step_up`, obtained through `isolation.connector_step_up`
//!   or `migration.confirmations.step_up`, the same proof) and that proof's
//!   CSRF token. Root writes the document through spawnd's allocation channel
//!   (schema 5 `provision_providers`, gateway peer: the employee's Applied
//!   confirmation item). Audited fail-closed in two phases: a strict
//!   `admitted` entry before anything is sent to root (a failed write answers
//!   `audit_unavailable`, nothing sent), then a `completed` entry with the same
//!   attempt id and the result (a failed write still returns the real outcome,
//!   marked `"audit":"incomplete"`). A request refused before root gets one
//!   best-effort `refused` entry.
//!
//! THE GATEWAY NEVER STORES A KEY. [`ProviderCall::take_from`] runs at the top
//! of dispatch: every `api_key` string is MOVED out of the parsed params into
//! a `Zeroizing<String>` (no copy) and everything left in the params is
//! overwritten. The document the worker reads is written by hand
//! ([`document`]) into one zeroizing buffer allocated at its exact final size
//! (no reallocation, no `serde_json::to_vec` of plain strings) and moved into
//! the schema-5 payload; the transport serialises it into a zeroizing frame.
//! No struct holding a key derives `Debug` or `Clone`. A name, key or URL that
//! would need a JSON escape (`"`, `\`, a control character) is refused, so the
//! document needs no escaping at all and every accepted dashboard request was
//! parsed without serde_json's unescape scratch buffer.
use super::*;
use super::isolation_connectors_rpc::authorize_step_up;
use crate::isolation_lifecycle::ConfirmedPrincipal;
use crate::isolation_provider_records::{self as records, ProviderSummary, Record};
use duduclaw_api_worker::vendor::Protocol;
use duduclaw_auth::mfa::MigrationConfirmationSession;
use duduclaw_isolation::{capability::EmployeeBinding, ids::{EmployeeId, RequestId}};
use duduclaw_migration::allocation::providers::{self as wire, Payload, ProviderAction, ProviderErrorCode, ProviderRequest,
    Provisioned, Zeroizing};
use duduclaw_migration::contract::ConfirmationItem;
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroize;

const STATUS: &str = "isolation.provider_status";
const SET: &str = "isolation.provider_set";
const CLEAR: &str = "isolation.provider_clear";
pub(super) const METHODS: [&str; 3] = [STATUS, SET, CLEAR];

/// Namespace owned by this handler; unknown members are refused here.
pub(super) fn owns(method: &str) -> bool {
    method.strip_prefix("isolation.").is_some_and(|rest| rest.starts_with("provider_"))
}

/// The worker's limit (`Credentials::select`).
const MAX_PROVIDERS: usize = 32;
/// The root side's payload limit (65,536 bytes).
const MAX_DOCUMENT: usize = wire::MAX_PAYLOAD;
/// Upper bound of confirmed employees read for status.
const MAX_EMPLOYEES: usize = 4096;
/// Rows per status page.
const PAGE: usize = 200;
/// `auth_audit_log` action of an Admin's step-up reveal of other people's
/// Personal employees in status (P1 break-glass, as the connector list).
const PERSONAL_REVEAL_ACTION: &str = "isolation.provider_status_personal_revealed";
/// `agent_id` of the audit events (as `isolation-connectors` for connectors).
const AUDIT_AGENT: &str = "isolation-providers";

/// A fixed refusal: a stable machine `code` (the dashboard keys its copy off
/// it) and a fixed zh-TW line. Never contains anything from the request.
/// `audit_incomplete`: the outcome is real (root was asked) but its completion
/// audit entry could not be written; the frame then carries `"audit":"incomplete"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refusal { pub(crate) code: &'static str, pub(crate) message: &'static str, pub(crate) audit_incomplete: bool }
impl Refusal {
    const fn new(code: &'static str, message: &'static str) -> Self { Self { code, message, audit_incomplete: false } }
    const fn with_incomplete_audit(self) -> Self { Self { audit_incomplete: true, ..self } }
    fn error_value(self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if self.audit_incomplete { error["audit"] = json!("incomplete"); }
        error
    }
    fn frame(self) -> WsFrame {
        WsFrame::Response { id: String::new(), ok: false, payload: None, error: Some(self.error_value()) }
    }
}

const NOT_ALLOWED: Refusal = Refusal::new("not_allowed", "不允許此模型連線資料操作。");
const ADMIN_REQUIRED: Refusal = Refusal::new("admin_required", "請以管理員帳號重新登入後再操作。");
/// The connector RPCs' fixed "not available" answer, word for word.
const NOT_AVAILABLE: Refusal = Refusal::new("not_available", "這個環境未啟用受保護模式，沒有可管理的隔離連線帳號。");
const USERS_UNAVAILABLE: Refusal = Refusal::new("users_unavailable", "無法讀取帳號資料。");
const SESSION_EXPIRED: Refusal = Refusal::new("session_expired", "登入憑證已失效，請重新登入。");
const STEP_UP_REQUIRED: Refusal = Refusal::new("step_up_required",
    "請先以密碼與雙因素驗證確認身分（5 分鐘內有效），再操作。");
const REQUEST_INVALID: Refusal = Refusal::new("request_invalid", "要求格式不正確。");
const CLOCK_UNAVAILABLE: Refusal = Refusal::new("clock_unavailable", "無法確認目前時間。");
const INVALID_PROVIDER: Refusal = Refusal::new("invalid_provider",
    "模型連線資料不正確（需要 1 至 32 個名稱不重複的提供者；端點必須是 https，且不含帳號密碼、查詢字串或片段；\
     名稱、金鑰與端點不可含引號、反斜線或控制字元；整份資料不可超過 65,536 位元組），未送出。");
const NOT_ALLOCATED: Refusal = Refusal::new("not_allocated",
    "找不到可用的員工：此員工未配置、尚未就緒、已停用或已退役，未變更。");
const BUSY: Refusal = Refusal::new("busy", "此員工目前有工作正在執行，未變更；請待員工閒置後再試。");
const HOME_ENTRY_UNSAFE: Refusal = Refusal::new("home_entry_unsafe",
    "員工家目錄中的 .duduclaw-api 或 providers.json 不安全（符號連結、非目錄、擁有者或權限不符），未寫入任何資料。");
const STORAGE_UNAVAILABLE: Refusal = Refusal::new("storage_unavailable",
    "員工儲存空間無法使用或已滿，未完成變更；請確認空間後再試。");
const UNCONFIRMED: Refusal = Refusal::new("unconfirmed",
    "結果未確認：請求已送出，但沒有收到隔離服務的確認，檔案可能已變更也可能沒有；再次送出同一設定是安全的（每次都會重新寫入或移除該檔案）。");
const UNREACHABLE: Refusal = Refusal::new("unreachable", "無法連線到隔離服務（spawnd 未執行或通道無法使用），未變更。");
const EMPLOYEES_UNAVAILABLE: Refusal = Refusal::new("employees_unavailable", "無法讀取受保護員工清單，已拒絕列出。");
const RECORDS_UNAVAILABLE: Refusal = Refusal::new("records_unavailable", "無法讀取模型連線紀錄，已拒絕列出。");
const AUDIT_UNAVAILABLE: Refusal = Refusal::new("audit_unavailable", "無法寫入稽核紀錄，已拒絕列出。");
/// Set/clear: the admission entry could not be written; nothing was sent to root.
const AUDIT_UNAVAILABLE_CHANGE: Refusal = Refusal::new("audit_unavailable", "無法寫入稽核紀錄，未送出任何變更。");
const SERVICE_UNAVAILABLE: Refusal = Refusal::new("service_unavailable", "模型連線資料服務無法使用，請重新載入。");

/// The isolation gate (same predicate as the connector RPCs).
fn availability(isolation_active: bool) -> Result<(), Refusal> {
    if isolation_active { Ok(()) } else { Err(NOT_AVAILABLE) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action { Status, Set, Clear, Unknown }
impl Action {
    fn of(method: &str) -> Self {
        match method { STATUS => Self::Status, SET => Self::Set, CLEAR => Self::Clear, _ => Self::Unknown }
    }
    /// The audited mutations.
    fn audited(self) -> Option<&'static str> {
        match self { Self::Set => Some("set"), Self::Clear => Some("clear"), Self::Status | Self::Unknown => None }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    /// One employee (32 lowercase hex); absent = every allocated employee, paged.
    #[serde(default)] employee_id: Option<String>,
    /// Paging cursor: the `next` of the previous page (an employee label).
    #[serde(default)] after: Option<String>,
    /// P1 break-glass: the step-up CSRF token reveals other people's Personal employees (audited).
    #[serde(default)] step_up_csrf: Option<[u8; 32]>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearRequest { employee_id: String, step_up_csrf: [u8; 32] }
/// Deliberately neither `Debug` nor `Clone`: it holds keys.
struct SetRequest { employee_id: String, providers: Vec<Entry>, step_up_csrf: [u8; 32] }
/// One provider as taken from the request. Every string lives only in its
/// zeroizing owner (moved out of the parsed params, never copied).
/// Deliberately neither `Debug` nor `Clone`.
struct Entry { name: Zeroizing<String>, api_key: Zeroizing<String>, base_url: Zeroizing<String>, protocol: Protocol }

enum Input { Status(StatusRequest), Set(SetRequest), Clear(ClearRequest) }

/// One `isolation.provider_*` request, taken out of the params at the top of
/// dispatch. Deliberately neither `Debug` nor `Clone`.
pub(crate) struct ProviderCall {
    action: Action,
    input: Result<Input, Refusal>,
    /// The request's employee id when it is a valid one (audit only).
    employee: Option<EmployeeId>,
}

/// Overwrite every string left in `value`, object KEYS included (a secret may
/// sit in an unknown property's name): containers are taken out whole
/// (`std::mem::take`), each owned key is zeroized and each value erased
/// recursively, so `value` is left as `""`, `[]` or `{}`.
///
/// Residuals this layer cannot reach: serde_json's parser drops the earlier
/// value of a DUPLICATE key (last wins) without zeroizing it, and the
/// WebSocket text frame / buffer the params were parsed from is owned and
/// dropped by the transport, not by this handler.
fn erase(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => {
            for mut item in std::mem::take(items) { erase(&mut item); }
        }
        Value::Object(fields) => {
            for (mut key, mut item) in std::mem::take(fields) {
                key.zeroize();
                erase(&mut item);
            }
        }
        _ => {}
    }
}

fn take_text(object: &mut serde_json::Map<String, Value>, key: &str) -> Option<Zeroizing<String>> {
    match object.remove(key)? {
        // Moves the parsed String (its heap buffer) into the zeroizing owner.
        Value::String(text) => Some(Zeroizing::new(text)),
        mut other => { erase(&mut other); None }
    }
}

fn protocol_named(name: &str) -> Option<Protocol> {
    Some(match name {
        "chat_completions" => Protocol::ChatCompletions,
        "responses" => Protocol::Responses,
        "anthropic" => Protocol::Anthropic,
        "gemini" => Protocol::Gemini,
        _ => return None,
    })
}
/// The worker's serde name of `protocol` (`vendor::Protocol`, snake_case).
fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::ChatCompletions => "chat_completions",
        Protocol::Responses => "responses",
        Protocol::Anthropic => "anthropic",
        Protocol::Gemini => "gemini",
    }
}

/// Exactly `name`, `api_key`, `base_url` and an optional `protocol`.
fn take_entry(item: &mut Value) -> Option<Entry> {
    let object = item.as_object_mut()?;
    // The key first: it leaves the params whatever else is wrong.
    let api_key = take_text(object, "api_key");
    let name = take_text(object, "name");
    let base_url = take_text(object, "base_url");
    let protocol = match object.remove("protocol") {
        None => Some(Protocol::default()),
        Some(Value::String(mut text)) => { let protocol = protocol_named(&text); text.zeroize(); protocol }
        Some(mut other) => { erase(&mut other); None }
    };
    if !object.is_empty() { return None; }
    Some(Entry { name: name?, api_key: api_key?, base_url: base_url?, protocol: protocol? })
}

/// 1..=32 entries. Every item is emptied on every path (a refused list
/// drops, and so wipes, the entries already taken).
fn take_entries(value: Option<Value>) -> Result<Vec<Entry>, Refusal> {
    let items = match value {
        Some(Value::Array(items)) => items,
        Some(mut other) => { erase(&mut other); return Err(INVALID_PROVIDER); }
        None => return Err(REQUEST_INVALID),
    };
    let mut valid = !items.is_empty() && items.len() <= MAX_PROVIDERS;
    let mut entries = Vec::with_capacity(if valid { items.len() } else { 0 });
    for mut item in items {
        if valid {
            match take_entry(&mut item) { Some(entry) => entries.push(entry), None => valid = false }
        }
        erase(&mut item);
    }
    if valid { Ok(entries) } else { Err(INVALID_PROVIDER) }
}

/// `{employee_id, providers, step_up_csrf}` and nothing else.
fn take_set(params: &mut Value) -> Result<SetRequest, Refusal> {
    let object = params.as_object_mut().ok_or(REQUEST_INVALID)?;
    let providers = take_entries(object.remove("providers"));
    let employee_id = match object.remove("employee_id") {
        Some(Value::String(id)) => Some(id),
        Some(mut other) => { erase(&mut other); None }
        None => None,
    };
    let csrf = object.remove("step_up_csrf").and_then(|mut value| {
        let csrf = <[u8; 32] as serde::Deserialize>::deserialize(&value).ok();
        erase(&mut value);
        csrf
    });
    if !object.is_empty() { return Err(REQUEST_INVALID); }
    let (Some(employee_id), Some(step_up_csrf)) = (employee_id, csrf) else { return Err(REQUEST_INVALID) };
    Ok(SetRequest { employee_id, providers: providers?, step_up_csrf })
}

impl ProviderCall {
    /// Take the request out of `params` and overwrite everything left in it
    /// (every string, on every path). After this the params hold no key.
    pub(crate) fn take_from(method: &str, params: &mut Value) -> Self {
        let action = Action::of(method);
        let employee = params.get("employee_id").and_then(Value::as_str).and_then(wire::parse_employee_id);
        let input = match action {
            Action::Set => take_set(params).map(Input::Set),
            // Omitted params (`null`) are an unfiltered first page.
            Action::Status if params.is_null() =>
                Ok(Input::Status(StatusRequest { employee_id: None, after: None, step_up_csrf: None })),
            Action::Status => <StatusRequest as serde::Deserialize>::deserialize(&*params).map(Input::Status)
                .map_err(|_| REQUEST_INVALID),
            Action::Clear => <ClearRequest as serde::Deserialize>::deserialize(&*params).map(Input::Clear)
                .map_err(|_| REQUEST_INVALID),
            Action::Unknown => Err(NOT_ALLOWED),
        };
        erase(params);
        Self { action, input, employee }
    }

    /// Audit a set/clear refused before [`serve`] (status is not audited);
    /// nothing was sent, so the single entry is best effort.
    fn audit_refusal(&self, home: &Path, actor: &str, refusal: Refusal) {
        Attempt::new(self.action, self.employee).refused(home, &SECURITY_AUDIT, actor, refusal.code);
    }
}

/// The set/clear audit writer. Production: `security_audit.jsonl` through the
/// STRICT `duduclaw_security::audit::try_append_audit_event` (lock,
/// serialisation, write and sync errors are returned, never swallowed), then
/// the autopilot bus mirror. Tests inject a fake.
pub(crate) trait AuditLog {
    fn append(&self, home: &Path, event: &duduclaw_security::audit::AuditEvent) -> Result<(), ()>;
}
pub(crate) struct SecurityAudit;
impl AuditLog for SecurityAudit {
    fn append(&self, home: &Path, event: &duduclaw_security::audit::AuditEvent) -> Result<(), ()> {
        duduclaw_security::audit::try_append_audit_event(home, event).map_err(|_| ())?;
        crate::security_autopilot::emit_security_autopilot_event(&event.severity, &event.event_type,
            Some(event.agent_id.as_str()), crate::security_autopilot::SOURCE_AUDIT);
        Ok(())
    }
}
static SECURITY_AUDIT: SecurityAudit = SecurityAudit;

/// What one set/clear attempt is audited with: actor, employee id / label,
/// action, a random attempt id, the phase, result code, provider NAMES and
/// ORIGINS only (and the time, from the audit event). Never a key, hash, path
/// or query.
///
/// Phases: `refused` (one best-effort entry; nothing was sent to root),
/// `admitted` (written strictly BEFORE root is asked; a failed write refuses
/// with `audit_unavailable` and sends nothing) and `completed` (same attempt
/// id, the result code; Warning unless `ok`).
struct Attempt {
    action: Action,
    attempt_id: String,
    employee: Option<EmployeeId>,
    label: Option<String>,
    kind: Option<&'static str>,
    foreign_personal: Option<bool>,
    providers: Vec<(String, String)>,
    recorded: Option<bool>,
}
impl Attempt {
    fn new(action: Action, employee: Option<EmployeeId>) -> Self {
        Self { action, attempt_id: hex::encode(uuid::Uuid::new_v4().as_bytes()), employee, label: None, kind: None,
            foreign_personal: None, providers: Vec::new(), recorded: None }
    }
    /// `None` for an unaudited action (status, unknown members).
    fn event(&self, actor: &str, phase: &str, result: Option<&str>) -> Option<duduclaw_security::audit::AuditEvent> {
        use duduclaw_security::audit::{AuditEvent, Severity};
        let action = self.action.audited()?;
        let providers: Vec<Value> = self.providers.iter()
            .map(|(name, origin)| json!({ "name": name, "origin": origin })).collect();
        let mut details = json!({
            "actor_user_id": actor, "action": action, "attempt_id": self.attempt_id, "phase": phase,
            "employee_id": self.employee.map(|employee| hex::encode(employee.0)), "label": self.label,
            "kind": self.kind, "foreign_personal": self.foreign_personal, "providers": providers,
        });
        if let Some(result) = result {
            details["result"] = json!(result);
            details["recorded"] = json!(self.recorded);
        }
        let severity = if result.is_none_or(|result| result == "ok") { Severity::Info } else { Severity::Warning };
        Some(AuditEvent::new(format!("isolation.provider_{action}"), AUDIT_AGENT, severity, details))
    }
    /// Refused before anything was admitted or sent: one best-effort entry.
    fn refused(&self, home: &Path, log: &dyn AuditLog, actor: &str, code: &str) {
        let Some(event) = self.event(actor, "refused", Some(code)) else { return };
        if log.append(home, &event).is_err() {
            tracing::warn!("model connection request refused; its audit entry could not be written");
        }
    }
    /// The durable admission entry, strict.
    fn admit(&self, home: &Path, log: &dyn AuditLog, actor: &str) -> Result<(), ()> {
        let event = self.event(actor, "admitted", None).ok_or(())?;
        log.append(home, &event)
    }
    /// The completion entry (same attempt id), strict.
    fn complete(&self, home: &Path, log: &dyn AuditLog, actor: &str, result: &str) -> Result<(), ()> {
        let event = self.event(actor, "completed", Some(result)).ok_or(())?;
        log.append(home, &event)
    }
}

/// The step-up proof source (production: users.db). Tests inject a fake.
pub(crate) trait AdminSession {
    /// The fresh-step-up proof of this exact user, session and credential
    /// epoch; `Err` = not a current Admin at that epoch.
    fn proof(&self, user: &str, session: &str, epoch: u64, now: i64) -> Result<Option<MigrationConfirmationSession>, ()>;
    fn mfa_enrolled(&self, user: &str) -> Result<bool, ()>;
    /// `auth_audit_log` row of a P1 reveal; a failed write refuses the reveal.
    fn audit_reveal(&self, user: &str, detail: &str) -> Result<(), ()>;
}
impl AdminSession for UserDb {
    fn proof(&self, user: &str, session: &str, epoch: u64, now: i64) -> Result<Option<MigrationConfirmationSession>, ()> {
        self.migration_confirmation_session(user, session, epoch, now).map_err(|_| ())
    }
    fn mfa_enrolled(&self, user: &str) -> Result<bool, ()> { self.mfa_active(user).map_err(|_| ()) }
    fn audit_reveal(&self, user: &str, detail: &str) -> Result<(), ()> {
        self.log_action(Some(user), PERSONAL_REVEAL_ACTION, None, Some(detail), None).map_err(|_| ())
    }
}

/// The employee a set/clear acts on, resolved through the live lifecycle.
pub(crate) struct Target {
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) label: String,
    pub(crate) binding: EmployeeBinding,
    /// The Applied allocation item: the gateway peer's identification.
    pub(crate) item: ConfirmationItem,
}

/// The outcome of one schema-5 exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Done(Provisioned),
    Refused(ProviderErrorCode),
    /// Nothing reached root (`ProvideFailure::NotSent`): nothing changed.
    NotSent,
    /// Sent, no confirmed answer (`ProvideFailure::Unconfirmed`).
    Unconfirmed,
}

/// Root's side: employee resolution and the socket call. Tests inject a fake.
pub(crate) trait ProviderRoot: Send + Sync {
    fn resolve(&self, home: &Path, employee: EmployeeId) -> Result<Target, Refusal>;
    /// Takes the request by value (its payload is wiped when it drops).
    fn provide(&self, request: ProviderRequest) -> Delivery;
}

/// Production: the employee lifecycle and spawnd's allocation socket.
pub(crate) struct SpawndProviders;
impl ProviderRoot for SpawndProviders {
    /// A live confirmed employee (apply.db row + Applied proposal + root
    /// ReadBack + users.db, `isolation_lifecycle::read_lifecycle`) whose
    /// immutable id is `employee`, and its Applied allocation item — the same
    /// binding the gateway's Allocate / Entitle / ReadBack requests carry.
    fn resolve(&self, home: &Path, employee: EmployeeId) -> Result<Target, Refusal> {
        use crate::isolation_lifecycle::{self as lifecycle, LifecycleError};
        let refusal = |error: LifecycleError| match error {
            LifecycleError::Unavailable => UNREACHABLE,
            LifecycleError::Denied | LifecycleError::Stale | LifecycleError::Tombstoned => NOT_ALLOCATED,
        };
        let row = lifecycle::confirmed_by_employee(home, employee, MAX_EMPLOYEES).map_err(refusal)?.ok_or(NOT_ALLOCATED)?;
        if row.disabled || row.binding.is_none() { return Err(NOT_ALLOCATED); }
        let live = lifecycle::read_lifecycle(home, &row.label).map_err(refusal)?;
        if live.employee != employee { return Err(NOT_ALLOCATED); }
        let item = lifecycle::applied_item(home, &live.allocation).map_err(refusal)?;
        Ok(Target { employee, incarnation: live.incarnation, label: live.label, binding: live.binding, item })
    }

    fn provide(&self, request: ProviderRequest) -> Delivery {
        #[cfg(target_os = "linux")]
        {
            use duduclaw_migration::allocation::transport::{self, ProvideFailure};
            match transport::provide(request) {
                Ok(Ok(done)) => Delivery::Done(done),
                Ok(Err(code)) => Delivery::Refused(code),
                Err(ProvideFailure::NotSent(_)) => Delivery::NotSent,
                Err(ProvideFailure::Unconfirmed(_)) => Delivery::Unconfirmed,
            }
        }
        #[cfg(not(target_os = "linux"))]
        { drop(request); Delivery::NotSent }
    }
}
static SPAWND: SpawndProviders = SpawndProviders;

/// Root's schema-5 code → the fixed RPC refusal.
fn refusal_for(code: ProviderErrorCode) -> Refusal {
    match code {
        ProviderErrorCode::NotAllocated | ProviderErrorCode::InvalidRequest => NOT_ALLOCATED,
        ProviderErrorCode::NotReady => BUSY,
        ProviderErrorCode::InvalidPayload => INVALID_PROVIDER,
        ProviderErrorCode::HomeEntryUnsafe => HOME_ENTRY_UNSAFE,
        ProviderErrorCode::StorageUnavailable => STORAGE_UNAVAILABLE,
        // Root's 4 s bound: the write may still complete.
        ProviderErrorCode::IsolationUnavailable => UNCONFIRMED,
    }
}

/// The worker's `Credential`, built only to call its own `validate`; its
/// strings (copies of the entry) are wiped when this guard drops — on every
/// path, unwind included (the CLI's `Checked` approach).
struct Checked(duduclaw_api_worker::Credential);
impl Checked {
    fn new(entry: &Entry) -> Self {
        Self(duduclaw_api_worker::Credential { name: entry.name.as_str().to_owned(), api_key: entry.api_key.as_str().to_owned(),
            base_url: entry.base_url.as_str().to_owned(), protocol: entry.protocol })
    }
}
impl Drop for Checked {
    fn drop(&mut self) {
        self.0.api_key.zeroize();
        self.0.base_url.zeroize();
        self.0.name.zeroize();
    }
}

/// Characters a JSON string would have to escape (and `char::is_control`
/// beyond them): refused, so [`document`] copies every accepted string verbatim.
fn needs_escape(text: &str) -> bool {
    text.chars().any(|c| c == '"' || c == '\\' || c.is_control())
}

/// `scheme://host[:port]` of an endpoint (never the path or query).
fn origin(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// The worker's rules (`Credentials::select`): 1..=32 providers (checked when
/// taken), each passing `Credential::validate`, unique names; plus no JSON
/// escape in any string. Returns the non-secret summary.
fn validate(entries: &[Entry]) -> Result<Vec<ProviderSummary>, Refusal> {
    if entries.is_empty() || entries.len() > MAX_PROVIDERS { return Err(INVALID_PROVIDER); }
    let mut seen = BTreeSet::new();
    let mut summary = Vec::with_capacity(entries.len());
    for entry in entries {
        if needs_escape(&entry.name) || needs_escape(&entry.api_key) || needs_escape(&entry.base_url) {
            return Err(INVALID_PROVIDER);
        }
        let checked = Checked::new(entry);
        let valid = checked.0.validate().is_ok();
        drop(checked);
        if !valid || !seen.insert(entry.name.as_str()) { return Err(INVALID_PROVIDER); }
        let origin = origin(&entry.base_url).ok_or(INVALID_PROVIDER)?;
        summary.push(ProviderSummary { name: entry.name.as_str().to_owned(), protocol: protocol_name(entry.protocol).to_owned(), origin });
    }
    Ok(summary)
}

/// The pieces of one provider object, in order.
fn parts(entry: &Entry) -> [&[u8]; 9] {
    [&br#"{"name":""#[..], entry.name.as_bytes(), &br#"","api_key":""#[..], entry.api_key.as_bytes(),
        &br#"","base_url":""#[..], entry.base_url.as_bytes(), &br#"","protocol":""#[..],
        protocol_name(entry.protocol).as_bytes(), &br#""}"#[..]]
}

/// `{"providers":[{"name","api_key","base_url","protocol"},…]}` — the exact
/// shape `duduclaw_api_worker::Credentials` reads — written into ONE
/// zeroizing buffer allocated at its exact final length (measured first; no
/// reallocation leaves a copy). The strings were checked by [`validate`] to
/// contain no `"`, `\` or control character, so they are valid JSON string
/// contents verbatim (Rust strings are valid UTF-8). Over 65,536 bytes is
/// refused before anything is allocated.
fn document(entries: &[Entry]) -> Result<Zeroizing<Vec<u8>>, Refusal> {
    const OPEN: &[u8] = br#"{"providers":["#;
    const CLOSE: &[u8] = b"]}";
    let mut length = OPEN.len() + CLOSE.len() + entries.len().saturating_sub(1);
    for entry in entries {
        for part in parts(entry) { length = length.checked_add(part.len()).ok_or(INVALID_PROVIDER)?; }
    }
    if entries.is_empty() || length > MAX_DOCUMENT { return Err(INVALID_PROVIDER); }
    let mut bytes = Zeroizing::new(Vec::with_capacity(length));
    bytes.extend_from_slice(OPEN);
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 { bytes.push(b','); }
        for part in parts(entry) { bytes.extend_from_slice(part); }
    }
    bytes.extend_from_slice(CLOSE);
    if bytes.len() != length { return Err(INVALID_PROVIDER); }
    Ok(bytes)
}

/// Serialises dashboard sets/clears in this process, so the recorded summary
/// follows the order in which root wrote the documents.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The authenticated caller.
pub(crate) struct Caller<'a> { pub(crate) user: &'a str, pub(crate) session: &'a str, pub(crate) epoch: u64 }

impl MethodHandler {
    /// Dispatch entry: the namespace's Admin floor (as the connector
    /// namespace), audited for set/clear, then the handler.
    pub(super) async fn route_isolation_providers(&self, call: ProviderCall, ctx: &UserContext) -> WsFrame {
        if acl::require_role(ctx, UserRole::Admin).is_err() {
            call.audit_refusal(&self.home_dir, &ctx.user_id, ADMIN_REQUIRED);
            return ADMIN_REQUIRED.frame();
        }
        self.handle_isolation_providers(call, ctx).await
    }

    async fn handle_isolation_providers(&self, call: ProviderCall, ctx: &UserContext) -> WsFrame {
        let home = self.home_dir.clone();
        if call.action == Action::Unknown || ctx.user_id == "system" {
            call.audit_refusal(&home, &ctx.user_id, NOT_ALLOWED);
            return NOT_ALLOWED.frame();
        }
        if acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err() {
            call.audit_refusal(&home, &ctx.user_id, ADMIN_REQUIRED);
            return ADMIN_REQUIRED.frame();
        }
        let Ok(session) = crate::google_personal::session_for(&ctx.user_id) else {
            call.audit_refusal(&home, &ctx.user_id, ADMIN_REQUIRED);
            return ADMIN_REQUIRED.frame();
        };
        if let Err(refusal) = availability(crate::runtime_readiness::isolation_active()) {
            call.audit_refusal(&home, &ctx.user_id, refusal);
            return refusal.frame();
        }
        let Some(db) = self.user_db.read().await.clone() else {
            call.audit_refusal(&home, &ctx.user_id, USERS_UNAVAILABLE);
            return USERS_UNAVAILABLE.frame();
        };
        let (user, epoch) = (ctx.user_id.clone(), ctx.credential_epoch);
        let joined = tokio::task::spawn_blocking(move || {
            let now = chrono::Utc::now().timestamp();
            let admin: &UserDb = &db;
            serve(&home, admin, &Caller { user: &user, session: &session, epoch }, call, &SPAWND, &SECURITY_AUDIT, now)
        }).await;
        match joined {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(refusal)) => refusal.frame(),
            Err(_) => SERVICE_UNAVAILABLE.frame(),
        }
    }
}

/// Blocking (SQLite, the lifecycle's root ReadBack, the allocation socket).
/// Every set/clear that reaches this is audited: one best-effort `refused`
/// entry when it stops before root, otherwise a strict `admitted` entry
/// before root is asked and a `completed` entry with the outcome.
#[allow(clippy::too_many_arguments)]
fn serve(home: &Path, admin: &dyn AdminSession, caller: &Caller<'_>, call: ProviderCall, root: &dyn ProviderRoot,
    audit: &dyn AuditLog, now: i64) -> Result<Value, Refusal> {
    let ProviderCall { action, input, employee } = call;
    match action {
        Action::Status => match input {
            Ok(Input::Status(request)) => status(home, admin, caller, request, now),
            Ok(_) => Err(REQUEST_INVALID),
            Err(refusal) => Err(refusal),
        },
        Action::Set | Action::Clear => {
            let mut attempt = Attempt::new(action, employee);
            match prepare(home, admin, caller, action, input, root, now, &mut attempt) {
                Ok(prepared) => deliver(home, root, audit, caller.user, &mut attempt, prepared),
                // Nothing was admitted or sent.
                Err(refusal) => {
                    attempt.refused(home, audit, caller.user, refusal.code);
                    Err(refusal)
                }
            }
        }
        Action::Unknown => Err(NOT_ALLOWED),
    }
}

/// A validated, resolved set/clear, ready for root. Deliberately neither
/// `Debug` nor `Clone`: the payload holds keys (wiped when it drops).
struct Prepared {
    employee: EmployeeId,
    incarnation: u32,
    item: ConfirmationItem,
    clear: bool,
    summary: Vec<ProviderSummary>,
    payload: Option<Payload>,
}

/// Two-phase audited delivery: the admission entry is durable BEFORE root is
/// asked (a failed write answers `audit_unavailable` and sends nothing); the
/// completion entry carries the same attempt id and the result. A failed
/// completion write cannot undo what root did: the real outcome is returned
/// with `"audit":"incomplete"` and the admission entry is the durable trace.
fn deliver(home: &Path, root: &dyn ProviderRoot, audit: &dyn AuditLog, actor: &str, attempt: &mut Attempt,
    prepared: Prepared) -> Result<Value, Refusal> {
    let Prepared { employee, incarnation, item, clear, summary, payload } = prepared;
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if attempt.admit(home, audit, actor).is_err() {
        // The payload (the document) is wiped as it drops here.
        drop(payload);
        return Err(AUDIT_UNAVAILABLE_CHANGE);
    }
    let request = ProviderRequest { schema: wire::SCHEMA_V5, nonce: RequestId(*uuid::Uuid::new_v4().as_bytes()),
        action: ProviderAction::ProvisionProviders, item: Some(item), employee_id: None, label: None, payload, clear };
    let outcome = match root.provide(request) {
        // A success must match what was asked (the transport checks it too).
        Delivery::Done(done) if done.provisioned != clear => Ok(done),
        Delivery::Done(_) | Delivery::Unconfirmed => Err(UNCONFIRMED),
        Delivery::Refused(code) => Err(refusal_for(code)),
        Delivery::NotSent => Err(UNREACHABLE),
    };
    let (result, code) = match outcome {
        Ok(done) => {
            // Confirmed: record (set) or forget (clear) the non-secret summary.
            let recorded = if clear {
                records::remove(home, employee).is_ok()
            } else {
                records::put(home, employee, &Record { incarnation, providers: summary, updated_unix: done.updated_unix })
                    .is_ok()
            };
            if !recorded {
                tracing::warn!(employee = %hex::encode(employee.0),
                    "model connection change confirmed by root but not recorded; status may be stale");
            }
            attempt.recorded = Some(recorded);
            (Ok(json!({ "provisioned": done.provisioned, "updated_unix": done.updated_unix,
                "result": if recorded { "recorded" } else { "not_recorded" } })),
                if recorded { "ok" } else { "not_recorded" })
        }
        Err(refusal) => (Err(refusal), refusal.code),
    };
    if attempt.complete(home, audit, actor, code).is_err() {
        tracing::warn!("model connection change finished but its completion audit entry could not be written; \
            the admission entry is the durable trace");
        return match result {
            Ok(mut value) => { value["audit"] = json!("incomplete"); Ok(value) }
            Err(refusal) => Err(refusal.with_incomplete_audit()),
        };
    }
    result
}

/// Every check before root: session, input, step-up, provider validation,
/// the document, employee resolution. Fills in what the audit entries carry.
#[allow(clippy::too_many_arguments)]
fn prepare(home: &Path, admin: &dyn AdminSession, caller: &Caller<'_>, action: Action, input: Result<Input, Refusal>,
    root: &dyn ProviderRoot, now: i64, attempt: &mut Attempt) -> Result<Prepared, Refusal> {
    if now < 0 { return Err(CLOCK_UNAVAILABLE); }
    let proof = admin.proof(caller.user, caller.session, caller.epoch, now).map_err(|()| SESSION_EXPIRED)?;
    let (employee_id, csrf, entries) = match (action, input?) {
        (Action::Set, Input::Set(request)) => (request.employee_id, request.step_up_csrf, Some(request.providers)),
        (Action::Clear, Input::Clear(request)) => (request.employee_id, request.step_up_csrf, None),
        _ => return Err(REQUEST_INVALID),
    };
    authorize_step_up(proof.as_ref(), caller.user, &csrf, now).map_err(|_| STEP_UP_REQUIRED)?;
    let employee = wire::parse_employee_id(&employee_id).ok_or(REQUEST_INVALID)?;
    // Validate and build the document before any I/O; the entries (and with
    // them every key copy but the document) are wiped right after.
    let prepared = match entries {
        Some(entries) => {
            let summary = validate(&entries)?;
            let bytes = document(&entries)?;
            drop(entries);
            Some((summary, bytes))
        }
        None => None,
    };
    if let Some((summary, _)) = &prepared {
        attempt.providers = summary.iter().map(|provider| (provider.name.clone(), provider.origin.clone())).collect();
    }
    let target = root.resolve(home, employee)?;
    if target.employee != employee { return Err(NOT_ALLOCATED); }
    attempt.label = Some(target.label.clone());
    let (kind, foreign) = match &target.binding {
        EmployeeBinding::Personal(owner) => ("personal", owner.as_str() != caller.user),
        EmployeeBinding::Shared(_) => ("shared", false),
    };
    attempt.kind = Some(kind);
    attempt.foreign_personal = Some(foreign);
    let clear = prepared.is_none();
    let (summary, payload) = match prepared {
        // Moved into the payload: no copy of the document is made.
        Some((summary, bytes)) => (summary, Some(Payload::new(bytes).map_err(|_| INVALID_PROVIDER)?)),
        None => (Vec::new(), None),
    };
    Ok(Prepared { employee, incarnation: target.incarnation, item: target.item, clear, summary, payload })
}

fn status(home: &Path, admin: &dyn AdminSession, caller: &Caller<'_>, request: StatusRequest, now: i64) -> Result<Value, Refusal> {
    if now < 0 { return Err(CLOCK_UNAVAILABLE); }
    let proof = admin.proof(caller.user, caller.session, caller.epoch, now).map_err(|()| SESSION_EXPIRED)?;
    let filter = match request.employee_id.as_deref() {
        Some(id) => Some(wire::parse_employee_id(id).ok_or(REQUEST_INVALID)?),
        None => None,
    };
    if request.after.as_deref().is_some_and(|after| filter.is_some() || !crate::isolation_lifecycle::valid_label(after)) {
        return Err(REQUEST_INVALID);
    }
    // P1 break-glass is opt-in and needs the same fresh step-up as a mutation.
    let reveal = match request.step_up_csrf.as_ref() {
        None => false,
        Some(csrf) => { authorize_step_up(proof.as_ref(), caller.user, csrf, now).map_err(|_| STEP_UP_REQUIRED)?; true }
    };
    let rows: Vec<ConfirmedPrincipal> = crate::isolation_lifecycle::confirmed_principals(home, MAX_EMPLOYEES)
        .map_err(|_| EMPLOYEES_UNAVAILABLE)?.into_iter().filter(|row| !row.disabled && row.binding.is_some()).collect();
    let recorded = records::read_all(home).map_err(|()| RECORDS_UNAVAILABLE)?;
    let incarnation = |row: &ConfirmedPrincipal| -> Result<Option<u32>, ()> {
        match crate::isolation_lifecycle::confirmed_row(home, &row.label).map_err(|_| ())? {
            Some((receipt, _, incarnation)) if receipt.employee == row.employee => Ok(Some(incarnation)),
            _ => Ok(None),
        }
    };
    let (mut value, revealed) = status_value(&rows, &recorded, filter, request.after.as_deref(), caller.user, reveal, &incarnation)?;
    if revealed > 0 {
        // Audited before anything is returned; a failed write refuses (P1).
        admin.audit_reveal(caller.user, &json!({ "rows": revealed, "after": request.after }).to_string())
            .map_err(|()| AUDIT_UNAVAILABLE)?;
    }
    value["step_up_valid"] = json!(proof.is_some());
    value["csrf_token"] = json!(proof.as_ref().map(|proof| proof.csrf));
    value["mfa_enrolled"] = json!(admin.mfa_enrolled(caller.user).map_err(|()| USERS_UNAVAILABLE)?);
    Ok(value)
}

/// Another person's Personal employee (P1).
fn foreign_personal(row: &ConfirmedPrincipal, caller: &str) -> bool {
    matches!(&row.binding, Some(EmployeeBinding::Personal(owner)) if owner.as_str() != caller)
}

/// One status page (pure). Without `reveal`, rows of another person's
/// Personal employees do not exist for this reply: no count
/// (`hidden_personal: null`), a filter naming one answers exactly as an
/// unknown id (`not_allocated`, same lookups), and the `after` cursor and
/// `next` are computed over the VISIBLE rows only (an `after` that is a hidden
/// label behaves as any other string: "rows after it"). With `reveal` every
/// row is listed (`hidden_personal: 0`) and the count of revealed rows is
/// returned for the audit. A record counts only for the incarnation it was
/// made for.
#[allow(clippy::too_many_arguments)]
fn status_value(rows: &[ConfirmedPrincipal], recorded: &BTreeMap<EmployeeId, Record>, filter: Option<EmployeeId>,
    after: Option<&str>, caller: &str, reveal: bool, incarnation: &dyn Fn(&ConfirmedPrincipal) -> Result<Option<u32>, ()>)
    -> Result<(Value, usize), Refusal> {
    let visible = |row: &&ConfirmedPrincipal| reveal || !foreign_personal(row, caller);
    let candidates: Vec<&ConfirmedPrincipal> = match filter {
        Some(id) => vec![select_visible(rows, id, |row| visible(&row)).ok_or(NOT_ALLOCATED)?],
        None => rows.iter().filter(visible).filter(|row| after.is_none_or(|after| row.label.as_str() > after)).collect(),
    };
    let (mut listed, mut revealed, mut next) = (Vec::new(), 0usize, None);
    for row in candidates {
        if listed.len() == PAGE {
            next = listed.last().and_then(|value: &Value| value["label"].as_str().map(str::to_owned));
            break;
        }
        if foreign_personal(row, caller) { revealed += 1; }
        let record = match recorded.get(&row.employee) {
            Some(record) => (incarnation(row).map_err(|()| RECORDS_UNAVAILABLE)? == Some(record.incarnation)).then_some(record),
            None => None,
        };
        listed.push(row_value(row, record));
    }
    // Kept as a key for a stable reply shape; a number only when revealed.
    let hidden = if reveal { json!(0) } else { Value::Null };
    Ok((json!({ "employees": listed, "next": next, "hidden_personal": hidden, "personal_revealed": reveal,
        // Constant: only changes made through the dashboard are visible here.
        "visibility": "dashboard_changes_only" }), revealed))
}

/// The row named by a status filter (pure). Visibility is applied BEFORE the
/// match and EVERY row is visited, with no early exit: `visible` is called
/// once per row whether or not, and wherever, the id matches, so a hidden
/// foreign Personal id and an unknown id cost the same scan (no timing oracle).
fn select_visible<'a>(rows: &'a [ConfirmedPrincipal], id: EmployeeId,
    mut visible: impl FnMut(&ConfirmedPrincipal) -> bool) -> Option<&'a ConfirmedPrincipal> {
    rows.iter().fold(None, |found, row| {
        // Both sides evaluated (no `&&` short-circuit on the visibility call).
        let shown = visible(row);
        if shown & (row.employee == id) { Some(row) } else { found }
    })
}

fn row_value(row: &ConfirmedPrincipal, record: Option<&Record>) -> Value {
    let kind = match &row.binding { Some(EmployeeBinding::Personal(_)) => "personal", _ => "shared" };
    let providers: Vec<Value> = record.map(|record| record.providers.iter()
        .map(|provider| json!({ "name": provider.name, "protocol": provider.protocol, "origin": provider.origin })).collect())
        .unwrap_or_default();
    let mut value = json!({ "employee_id": hex::encode(row.employee.0), "label": row.label, "kind": kind,
        "provisioned": record.is_some(), "providers": providers });
    if let Some(record) = record {
        value["source"] = json!("dashboard");
        value["updated_unix"] = json!(record.updated_unix);
    }
    value
}

#[cfg(test)]
#[path = "isolation_providers_rpc_tests.rs"]
mod tests;
