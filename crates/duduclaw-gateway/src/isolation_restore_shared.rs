//! Shared tombstone restore (native Linux isolation v1.81, F6 design r5
//! §3.9 "Shared tombstone and restore (Q7)", §3.4, §3.7; task F6k).
//!
//! The F6c engine (`isolation_restore`) already applies and recovers a
//! `shared_restore` request (selected bindings re-created at ≤ the snapshot
//! level, S11). This module adds what it left to F6k:
//!
//! - **Filing** ([`file_shared_request`], RPC [`request`]) by an Admin with a
//!   fresh step-up: the common checks of §3.4 (tenant == apply.db pin ==
//!   users.db tenant, and the tombstone snapshot's `tenant_id` == the pin);
//!   `member_selection` ⊆ the tombstone-time snapshot members, each at no
//!   higher access level (S11), once; members no longer active (suspended,
//!   offboarded, deleted) are **dropped** and reported, never bound; at least
//!   one member remains. Nothing is re-bound automatically: snapshot members
//!   not selected stay unbound (new members use the normal binding flow).
//!   Tenant routes: only the active `SharedTenant`/`SharedHome` routes of
//!   `Shared(tenant)` that are not per-employee detail routes are offered
//!   ([`TenantRouteSource`]), each ticked explicitly with its generation; the
//!   request's route digest covers exactly the ticked list and the request
//!   digest the member selection **with levels** (both re-checked at
//!   approval, M5).
//! - **Approval** ([`approve`]) through `isolation.employee_restore_decide`:
//!   the selection is re-validated against the snapshot and every selected
//!   member must still be active (else `MEMBERS_CHANGED`, terminal); the
//!   engine's route check sees only the ticked accounts ([`Ticked`]), so a
//!   ticked route revoked or re-generated since filing refuses
//!   `ROUTES_CHANGED`. The approval rule is the F6c2 rule unchanged (R1/S2:
//!   another qualifying Admin; sole-Admin self-approval with step-up, cooling
//!   and the `self_approved` audit, §2 SA — Shared keeps that path).
//! - **After restore** ([`after_restore`], §3.7): the reply lists the re-bound
//!   members, the re-approved tenant routes and that detail routes need
//!   `isolation.connector_provision` again; every re-bound member is notified
//!   (never capped). Work stays paused (the engine never resumes it).
//! - **Use-time gate** ([`tenant_route_gate`], called by the connector
//!   authority on every connector call): an incarnation of a Shared employee
//!   that came back through a Shared restore may use only the tenant routes
//!   that restore's approval ticked ("re-approved, not reactivated"); a
//!   missing or unreadable record refuses. Detail routes are gated by their
//!   own pins (fresh provisioning).
//!
//! Lock order: users.db reads happen outside every apply.db transaction; the
//! filing writes apply.db only (M6). Everything here is blocking.
use std::collections::BTreeSet;
use std::path::Path;

use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::VerificationError;
use duduclaw_security::audit::Severity;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::fs_safe::sqlite::Access;
use crate::isolation_connectors::ConnectorKind;
use crate::isolation_lifecycle::{self as lifecycle, EmployeeKind, APPLY_DB, USERS_DB};
use crate::isolation_restore::{self as engine, Quarantined, Refusal, RequestKind, RestoreEnv, RestoreRequest, RouteSource, Stage};
use crate::isolation_restore_wiring::{self as wiring, NoticeScope};

/// L1 window of the per-row request cap (same as the engine's).
const RATE_WINDOW_SECS: i64 = 30 * 86_400;
/// Most tenant routes one request may tick.
const MAX_TICKED: usize = 64;
/// Most members one selection may name (a snapshot is bounded the same way).
const MAX_MEMBERS: usize = 1_000;
const MAX_USER_ID_CHARS: usize = 128;
const MAX_ACCOUNT_CHARS: usize = 256;

/// apply.db: the ticked tenant routes and dropped members of one Shared
/// request, written in the filing transaction. `routes` is digest-bound
/// through the request's `route_digest`; the use-time gate reads it by the
/// request id the restored row was closed by.
const SELECTION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS shared_restore_selections (
        request_id TEXT PRIMARY KEY CHECK (length(request_id) = 32 AND request_id NOT GLOB '*[^0-9a-f]*'),
        employee_hex TEXT NOT NULL CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
        incarnation INTEGER NOT NULL CHECK (incarnation >= 0 AND incarnation <= 4294967295),
        routes TEXT NOT NULL,
        dropped TEXT NOT NULL,
        created_at INTEGER NOT NULL)";

/// Why a Shared restore step refused. Engine refusals keep their codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedError {
    Restore(Refusal),
    /// Malformed selection, a member outside the snapshot, or a duplicate.
    MembersInvalid,
    /// A selected level above the member's tombstone-time level (S11).
    LevelRaised,
    /// No active member left to bind.
    NoMembers,
    /// A selected member is no longer active, or the stored selection no
    /// longer matches the snapshot (M5).
    MembersChanged,
    /// A ticked route is not an offered tenant route at that generation.
    RouteNotOffered,
    Invalid,
}

impl From<Refusal> for SharedError { fn from(r: Refusal) -> Self { Self::Restore(r) } }

impl SharedError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Restore(r) => r.code(),
            Self::MembersInvalid => "MEMBERS_INVALID",
            Self::LevelRaised => "ACCESS_LEVEL_RAISED",
            Self::NoMembers => "MEMBERS_REQUIRED",
            Self::MembersChanged => "MEMBERS_CHANGED",
            Self::RouteNotOffered => "ROUTE_NOT_OFFERED",
            Self::Invalid => "INVALID_REQUEST",
        }
    }
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Restore(r) => crate::isolation_restore_rpc::RpcError::Restore(r).message(),
            Self::MembersInvalid => "成員只能從註銷當時的成員名單中選擇，且每人只能出現一次。",
            Self::LevelRaised => "還原時不能提高成員的權限，只能維持或降低註銷當時的權限。",
            Self::NoMembers => "至少要選一位目前仍在使用帳號的成員，才能還原共用員工。",
            Self::MembersChanged => "成員名單或成員帳號狀態已變更，請重新申請。",
            Self::RouteNotOffered => "勾選的連線已不在可用清單中，請重新載入後再申請。",
            Self::Invalid => "申請內容不正確，請重新載入後再試。",
        }
    }
}

fn unavailable<E>(_: E) -> Refusal { Refusal::Unavailable }

/// Canonical lowercase 32-hex employee id (never matched loosely).
fn employee_id(value: &str) -> Result<EmployeeId, SharedError> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(SharedError::Invalid);
    }
    let bytes = hex::decode(value).map_err(|_| SharedError::Invalid)?;
    Ok(EmployeeId(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| SharedError::Invalid)?))
}

fn bounded_text(value: &Value, max_chars: usize) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty() && text.chars().count() <= max_chars && !text.chars().any(char::is_control))
}

/// `(canonical name, rank)` of an access level.
fn level(value: &str) -> Option<(&'static str, u8)> {
    match value { "owner" => Some(("owner", 3)), "operator" => Some(("operator", 2)), "viewer" => Some(("viewer", 1)), _ => None }
}

/// One selected member binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) user_id: String,
    pub(crate) access_level: &'static str,
}

fn selection_value(members: &[Selected]) -> Value {
    Value::Array(members.iter().map(|m| json!({"user_id": m.user_id, "access_level": m.access_level})).collect())
}

/// The row is Shared, pinned, and its snapshot names the same tenant.
fn check_shared_row(q: &Quarantined) -> Result<(), SharedError> {
    if q.kind != EmployeeKind::Shared || q.owner.is_some() { return Err(Refusal::NotFound.into()); }
    if q.tenant.is_empty() || q.principal_snapshot["tenant_id"].as_str() != Some(q.tenant.as_str()) {
        return Err(Refusal::TenantMismatch.into());
    }
    Ok(())
}

/// "Tenant must equal the pin" (§3.9), stated before the root precheck (which
/// would only answer `NOT_FOUND` for a moved pin): the label's apply.db pin is
/// the row's tenant, and so is users.db's installation tenant.
fn check_pin(home: &Path, q: &Quarantined) -> Result<(), SharedError> {
    let pinned: Option<Option<String>> = {
        let db = engine::open_apply(home)?;
        db.query_row("SELECT tenant_id FROM confirmed_employees WHERE label=?1", [&q.label], |r| r.get(0))
            .optional().map_err(unavailable)?
    };
    if pinned.ok_or(Refusal::NotFound)?.as_deref() != Some(q.tenant.as_str()) { return Err(Refusal::TenantMismatch.into()); }
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(|_| Refusal::Unavailable)?;
    let tx = users.transaction().map_err(unavailable)?;
    let tenant = duduclaw_auth::isolation::read_isolation_tenant(&tx).map_err(unavailable)?;
    tx.finish().map_err(unavailable)?;
    if tenant != q.tenant { return Err(Refusal::TenantMismatch.into()); }
    Ok(())
}

/// §3.9 / S11: every selected member is in the tombstone snapshot, at most
/// at its snapshot level, once; entries carry exactly `user_id` and
/// `access_level`. Sorted by user id (canonical, digest-stable).
pub(crate) fn parse_selection(q: &Quarantined, members: &Value) -> Result<Vec<Selected>, SharedError> {
    let snapshot = q.principal_snapshot["members"].as_array().ok_or(Refusal::Inconsistent)?;
    let selection = members.as_array().ok_or(SharedError::MembersInvalid)?;
    if selection.len() > MAX_MEMBERS { return Err(SharedError::MembersInvalid); }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(selection.len());
    for member in selection {
        if member.as_object().is_none_or(|fields| fields.len() != 2) { return Err(SharedError::MembersInvalid); }
        let user = bounded_text(&member["user_id"], MAX_USER_ID_CHARS).ok_or(SharedError::MembersInvalid)?;
        let (name, wanted) = member["access_level"].as_str().and_then(level).ok_or(SharedError::MembersInvalid)?;
        // Exact id equality with the snapshot entry.
        let was = snapshot.iter().find(|old| old["user_id"].as_str() == Some(user)).ok_or(SharedError::MembersInvalid)?;
        let (_, had) = was["access_level"].as_str().and_then(level).ok_or(Refusal::Inconsistent)?;
        if wanted > had { return Err(SharedError::LevelRaised); }
        if !seen.insert(user.to_owned()) { return Err(SharedError::MembersInvalid); }
        out.push(Selected { user_id: user.to_owned(), access_level: name });
    }
    out.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    Ok(out)
}

/// Account status now, in one users.db read: `Ok(None)` = active, else the
/// reason it is dropped (`inactive` / `deleted`).
fn member_states(home: &Path, users: &[&str]) -> Result<Vec<Option<&'static str>>, Refusal> {
    let mut db = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(|_| Refusal::Unavailable)?;
    let tx = db.transaction().map_err(unavailable)?;
    let states = users.iter().map(|user| {
        let status: Option<String> = tx.query_row("SELECT status FROM users WHERE id=?1", [*user], |r| r.get(0))
            .optional().map_err(unavailable)?;
        Ok(match status.as_deref() { Some("active") => None, Some(_) => Some("inactive"), None => Some("deleted") })
    }).collect::<Result<Vec<_>, Refusal>>()?;
    tx.finish().map_err(unavailable)?;
    Ok(states)
}

/// The selection split into the members still active and those dropped
/// (`{user_id, access_level, reason}`).
fn keep_active(home: &Path, selected: Vec<Selected>) -> Result<(Vec<Selected>, Vec<Value>), Refusal> {
    let ids: Vec<&str> = selected.iter().map(|m| m.user_id.as_str()).collect();
    let states = member_states(home, &ids)?;
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for (member, state) in selected.into_iter().zip(states) {
        match state {
            None => kept.push(member),
            Some(reason) => dropped.push(json!({"user_id": member.user_id, "access_level": member.access_level, "reason": reason})),
        }
    }
    Ok((kept, dropped))
}

// ── Tenant routes ───────────────────────────────────────────────────────

/// One tenant route an Admin may tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantRoute {
    pub(crate) account: String,
    pub(crate) generation: u64,
    pub(crate) provider: String,
    pub(crate) kind: ConnectorKind,
}

impl TenantRoute {
    fn to_value(&self) -> Value {
        json!({"account": self.account, "generation": self.generation, "provider": self.provider, "kind": self.kind.as_str()})
    }
}

/// The tenant routes a restored Shared employee may use again (§3.9):
/// active `SharedTenant`/`SharedHome` routes of exactly `Shared(tenant)`,
/// never a per-employee detail route (those are re-provisioned), sorted by
/// account. `Err` = unreadable (never "none").
pub(crate) trait TenantRouteSource {
    fn tickable(&self, tenant: &str) -> Result<Vec<TenantRoute>, ()>;
}

/// Production: the connector catalog. No catalog file means nothing was ever
/// provisioned (no routes).
pub(crate) struct CatalogTenantRoutes<'a>(pub(crate) &'a Path);

impl TenantRouteSource for CatalogTenantRoutes<'_> {
    fn tickable(&self, tenant: &str) -> Result<Vec<TenantRoute>, ()> {
        use crate::isolation_connectors::{ConnectorCatalog, CATALOG_FILE};
        match std::fs::symlink_metadata(self.0.join(CATALOG_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(()),
            Ok(_) => {}
        }
        let catalog = ConnectorCatalog::open(self.0).map_err(|_| ())?;
        let binding = EmployeeBinding::Shared(Text::new(tenant.to_owned()).map_err(|_| ())?);
        let mut out = Vec::new();
        for route in catalog.active_routes_for(&binding).map_err(|_| ())? {
            if !route.active || route.binding != binding || !matches!(route.kind, ConnectorKind::SharedTenant | ConnectorKind::SharedHome)
                || crate::isolation_connector_details::needs_details(route.provider) {
                continue;
            }
            // A route carrying a detail record is per-employee, whatever its provider.
            if matches!(catalog.route_details(&route.account).map_err(|_| ())?, Some((_, Some(_)))) { continue; }
            out.push(TenantRoute { account: route.account.as_str().to_owned(), generation: route.generation,
                provider: route.provider.to_owned(), kind: route.kind });
        }
        out.sort_by(|a, b| a.account.cmp(&b.account));
        Ok(out)
    }
}

/// The ticked `[{account, generation}]` list: each exactly one offered route
/// (same account and generation), once. Sorted by account.
fn parse_ticks(ticks: &Value, offered: &[TenantRoute]) -> Result<Vec<TenantRoute>, SharedError> {
    let list = ticks.as_array().ok_or(SharedError::Invalid)?;
    if list.len() > MAX_TICKED { return Err(SharedError::Invalid); }
    let mut seen = BTreeSet::new();
    let mut chosen = Vec::with_capacity(list.len());
    for tick in list {
        if tick.as_object().is_none_or(|fields| fields.len() != 2) { return Err(SharedError::Invalid); }
        let account = bounded_text(&tick["account"], MAX_ACCOUNT_CHARS).ok_or(SharedError::Invalid)?;
        let generation = tick["generation"].as_u64().ok_or(SharedError::Invalid)?;
        if !seen.insert(account.to_owned()) { return Err(SharedError::Invalid); }
        let route = offered.iter().find(|route| route.account == account && route.generation == generation)
            .ok_or(SharedError::RouteNotOffered)?;
        chosen.push(route.clone());
    }
    chosen.sort_by(|a, b| a.account.cmp(&b.account));
    Ok(chosen)
}

fn pairs(routes: &[TenantRoute]) -> Vec<(String, u64)> {
    routes.iter().map(|route| (route.account.clone(), route.generation)).collect()
}

/// The engine's route source restricted to the ticked accounts: at approval
/// and in the apply transaction the route digest is recomputed over exactly
/// these, so a ticked route revoked or re-generated refuses `ROUTES_CHANGED`.
pub(crate) struct Ticked<'a> {
    pub(crate) inner: &'a dyn RouteSource,
    pub(crate) accounts: BTreeSet<String>,
}

impl RouteSource for Ticked<'_> {
    fn routes_for(&self, binding: &EmployeeBinding, tenant: &str) -> Result<Vec<(String, u64)>, ()> {
        Ok(self.inner.routes_for(binding, tenant)?.into_iter().filter(|(account, _)| self.accounts.contains(account)).collect())
    }
    fn detail_routes_naming(&self, employee: EmployeeId) -> Result<Vec<String>, ()> { self.inner.detail_routes_naming(employee) }
}

// ── Journal ─────────────────────────────────────────────────────────────

fn open(home: &Path) -> Result<Connection, Refusal> {
    let db = engine::open_apply(home)?;
    db.execute_batch(SELECTION_SCHEMA).map_err(unavailable)?;
    Ok(db)
}

/// The stored ticks and dropped members of one Shared request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoredSelection {
    pub(crate) routes: Vec<TenantRoute>,
    pub(crate) dropped: Value,
}

fn decode_routes(text: &str) -> Result<Vec<TenantRoute>, Refusal> {
    let value: Value = serde_json::from_str(text).map_err(unavailable)?;
    value.as_array().ok_or(Refusal::Unavailable)?.iter().map(|route| {
        // Exactly the names `ConnectorKind::as_str` writes, tenant-wide kinds only.
        let kind = route["kind"].as_str().and_then(|name|
            [ConnectorKind::SharedTenant, ConnectorKind::SharedHome].into_iter().find(|kind| kind.as_str() == name))
            .ok_or(Refusal::Unavailable)?;
        Ok(TenantRoute {
            account: route["account"].as_str().ok_or(Refusal::Unavailable)?.to_owned(),
            generation: route["generation"].as_u64().ok_or(Refusal::Unavailable)?,
            provider: route["provider"].as_str().ok_or(Refusal::Unavailable)?.to_owned(), kind,
        })
    }).collect()
}

pub(crate) fn read_selection(db: &Connection, request: &RestoreRequest) -> Result<Option<StoredSelection>, Refusal> {
    let row: Option<(String, String)> = db.query_row(
        "SELECT routes,dropped FROM shared_restore_selections WHERE request_id=?1 AND employee_hex=?2 AND incarnation=?3",
        params![request.request_id, hex::encode(request.employee.0), request.incarnation], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional().map_err(unavailable)?;
    row.map(|(routes, dropped)| Ok(StoredSelection {
        routes: decode_routes(&routes)?, dropped: serde_json::from_str(&dropped).map_err(unavailable)?,
    })).transpose()
}

// ── Filing ──────────────────────────────────────────────────────────────

/// The filing Admin: the RPC verified Admin + current MFA and the migration
/// step-up of this call; `fresh_step_up` is that proof's freshness.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SharedFiler<'a> {
    pub(crate) admin_user_id: &'a str,
    pub(crate) fresh_step_up: bool,
}

/// A filed Shared restore request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SharedFiled {
    pub(crate) request: RestoreRequest,
    pub(crate) members: Vec<Selected>,
    pub(crate) dropped: Vec<Value>,
    pub(crate) routes: Vec<TenantRoute>,
}

/// `isolation.shared_restore_request` engine (§3.9). `members`:
/// `[{user_id, access_level}]`; `ticks`: `[{account, generation}]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn file_shared_request(home: &Path, env: &RestoreEnv<'_>, tenant_routes: &dyn TenantRouteSource,
    employee: EmployeeId, incarnation: u32, filer: SharedFiler<'_>, members: &Value, ticks: &Value)
    -> Result<SharedFiled, SharedError> {
    if !filer.fresh_step_up { return Err(Refusal::Approval("STEP_UP_REQUIRED").into()); }
    if filer.admin_user_id.is_empty() || filer.admin_user_id == "system" { return Err(SharedError::Invalid); }
    let peek = { let db = engine::open_apply(home)?; engine::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)? };
    check_shared_row(&peek)?;
    parse_selection(&peek, members)?;
    engine::recover_label(home, env, &peek.label)?;
    let q = engine::load_fenced(home, env, employee, incarnation)?;
    check_shared_row(&q)?;
    check_pin(home, &q)?;
    let (kept, dropped) = keep_active(home, parse_selection(&q, members)?)?;
    if kept.is_empty() { return Err(SharedError::NoMembers); }
    // Every common check outside apply.db (tenant == users.db, tombstone
    // finished, fences, proposal, target, quota, store marks, root).
    let all_routes = engine::check_outside(home, env, &q)?;
    let offered = tenant_routes.tickable(&q.tenant).map_err(unavailable)?;
    let chosen = parse_ticks(ticks, &offered)?;
    // The two catalog reads must agree on every ticked route.
    if chosen.iter().any(|route| !all_routes.contains(&(route.account.clone(), route.generation))) {
        return Err(SharedError::RouteNotOffered);
    }
    let request = insert_request(home, env, &q, filer.admin_user_id, &kept, &dropped, &chosen)?;
    engine::audit(home, "isolation.employee_restore_requested", &request.label, Severity::Info,
        engine::audit_details(&request, json!({"members": kept.len(), "members_dropped": dropped.len(),
            "routes_ticked": chosen.iter().map(|route| route.account.as_str()).collect::<Vec<_>>()})));
    Ok(SharedFiled { request, members: kept, dropped, routes: chosen })
}

/// The apply.db half: the common checks in the writer transaction, the L1
/// cap, the digest-bound request row and its ticks, in one transaction.
fn insert_request(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined, requester: &str, kept: &[Selected], dropped: &[Value],
    chosen: &[TenantRoute]) -> Result<RestoreRequest, Refusal> {
    let mut db = open(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    engine::check_apply_db(&tx, q, Stage::Request, env.now)?;
    let recent: i64 = tx.query_row("SELECT count(*) FROM employee_restores WHERE employee_hex=?1 AND incarnation=?2 AND created_at>?3",
        params![hex::encode(q.employee.0), q.incarnation, env.now - RATE_WINDOW_SECS], |r| r.get(0)).map_err(unavailable)?;
    if recent >= engine::MAX_REQUESTS_PER_ROW { return Err(Refusal::RateLimited); }
    let unsigned = RestoreRequest {
        request_id: hex::encode(uuid::Uuid::new_v4().as_bytes()), kind: RequestKind::SharedRestore, employee: q.employee,
        incarnation: q.incarnation, label: q.label.clone(), owner: None, tenant: q.tenant.clone(), requester: requester.to_owned(),
        digest: String::new(), route_digest: engine::route_digest(&pairs(chosen)), member_selection: Some(selection_value(kept)),
        created_at: env.now,
        expires_at: env.now.saturating_add(engine::REQUEST_TTL_SECS).min(q.quarantined_at.saturating_add(engine::RESTORE_WINDOW_SECS)),
        state: "requested".into(), approver: None, self_approved: None, failure: None, filed_by_admin: None,
    };
    let digest = engine::request_digest(&unsigned, q.digest.as_deref().ok_or(Refusal::NotSealed)?);
    let request = RestoreRequest { digest, ..unsigned };
    tx.execute("INSERT INTO employee_restores(request_id,kind,employee_hex,incarnation,label,owner_user_id,tenant_id,
            requester_user_id,digest,route_digest,member_selection,created_at,expires_at,state)
            VALUES(?1,?2,?3,?4,?5,NULL,?6,?7,?8,?9,?10,?11,?12,'requested')",
        params![request.request_id, request.kind.as_str(), hex::encode(q.employee.0), q.incarnation, request.label, request.tenant,
            request.requester, request.digest, request.route_digest, request.member_selection.as_ref().map(Value::to_string),
            request.created_at, request.expires_at]).map_err(unavailable)?;
    let routes = Value::Array(chosen.iter().map(TenantRoute::to_value).collect());
    tx.execute("INSERT INTO shared_restore_selections(request_id,employee_hex,incarnation,routes,dropped,created_at)
            VALUES(?1,?2,?3,?4,?5,?6)",
        params![request.request_id, hex::encode(q.employee.0), q.incarnation, routes.to_string(),
            Value::Array(dropped.to_vec()).to_string(), env.now]).map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    Ok(request)
}

// ── Approval ────────────────────────────────────────────────────────────

/// Record a terminal refusal of a still-`requested` request and audit it.
fn fail_request(home: &Path, request: &RestoreRequest, error: SharedError, now: i64) -> SharedError {
    let written = (|| -> Result<(), Refusal> {
        let mut db = engine::open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        engine::set_request_state(&tx, &request.request_id, &["requested"], "failed", Some(error.code()), now)?;
        tx.commit().map_err(unavailable)
    })();
    engine::audit(home, "isolation.employee_restore_failed", &request.label, Severity::Warning,
        engine::audit_details(request, json!({"failure": error.code(), "recorded": written.is_ok()})));
    error
}

/// The approval call of `isolation.employee_restore_decide` (approve). Any
/// kind other than `shared_restore` goes to the engine unchanged. For a
/// Shared request (M5, S11): the stored ticks must still give the request's
/// route digest, the stored selection must still be a subset of the
/// snapshot at ≤ its levels and every selected member must still be active
/// (else the request fails `MEMBERS_CHANGED`); the engine then approves with
/// the route source restricted to the ticked accounts ([`Ticked`]).
pub(crate) fn approve(home: &Path, env: &RestoreEnv<'_>, request: &RestoreRequest, approver: &str)
    -> Result<engine::Restored, SharedError> {
    if request.kind != RequestKind::SharedRestore {
        return engine::approve(home, env, &request.request_id, approver).map_err(SharedError::from);
    }
    let (q, stored) = {
        let db = open(home)?;
        (engine::read_row(&db, request.employee, request.incarnation)?.ok_or(Refusal::NotFound)?, read_selection(&db, request)?)
    };
    if request.state == "requested" {
        let checked = (|| -> Result<(), SharedError> {
            check_shared_row(&q)?;
            check_pin(home, &q)?;
            let stored = stored.as_ref().ok_or(SharedError::Restore(Refusal::Inconsistent))?;
            if engine::route_digest(&pairs(&stored.routes)) != request.route_digest { return Err(Refusal::Inconsistent.into()); }
            let selection = request.member_selection.as_ref().ok_or(SharedError::MembersChanged)?;
            let parsed = parse_selection(&q, selection).map_err(|_| SharedError::MembersChanged)?;
            if parsed.is_empty() || &selection_value(&parsed) != selection { return Err(SharedError::MembersChanged); }
            let (_, dropped) = keep_active(home, parsed)?;
            if !dropped.is_empty() { return Err(SharedError::MembersChanged); }
            Ok(())
        })();
        match checked {
            Ok(()) => {}
            // Transient: the request stays open for a retry.
            Err(error @ SharedError::Restore(Refusal::Unavailable)) => return Err(error),
            Err(error) => return Err(fail_request(home, request, error, env.now)),
        }
    }
    let accounts = stored.map(|stored| stored.routes.into_iter().map(|route| route.account).collect()).unwrap_or_default();
    let ticked = Ticked { inner: env.routes, accounts };
    let restricted = RestoreEnv { root: env.root, routes: &ticked, stores: env.stores, runs: env.runs, rule: env.rule,
        agent_cap: env.agent_cap, actor: env.actor, now: env.now };
    engine::approve(home, &restricted, &request.request_id, approver).map_err(SharedError::from)
}

/// §3.7 after a Shared restore committed: extend the decide reply with the
/// re-bound members, the re-approved tenant routes and the provisioning
/// note; notify every re-bound member (never capped). No-op for other kinds.
pub(crate) fn after_restore(home: &Path, request: &RestoreRequest, incarnation: u32, reply: &mut Value, now: i64) {
    if request.kind != RequestKind::SharedRestore { return; }
    let stored = open(home).ok().and_then(|db| read_selection(&db, request).ok().flatten());
    let members = request.member_selection.clone().unwrap_or(Value::Array(Vec::new()));
    let routes: Vec<Value> = stored.as_ref().map(|s| s.routes.iter().map(TenantRoute::to_value).collect()).unwrap_or_default();
    reply["members"] = members.clone();
    reply["members_dropped"] = stored.as_ref().map(|s| s.dropped.clone()).unwrap_or(Value::Array(Vec::new()));
    reply["routes_reapproved"] = Value::Array(routes.clone());
    // Detail routes were revoked at tombstone and never come back (§3.7).
    reply["routes_need_provisioning"] = json!(true);
    reply["work_paused"] = json!(true);
    reply["members_notified"] = json!(notify_members(home, request, &members, incarnation, now));
    engine::audit(home, "isolation.shared_restore_routes_reapproved", &request.label, Severity::Info,
        engine::audit_details(request, json!({"new_incarnation": incarnation, "routes": routes,
            "members": members.as_array().map_or(0, Vec::len)})));
}

/// One never-dropped notice per re-bound member, to their current address.
fn notify_members(home: &Path, request: &RestoreRequest, members: &Value, incarnation: u32, now: i64) -> usize {
    let addresses = (|| -> Result<Vec<(String, String, String)>, Refusal> {
        let mut db = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(|_| Refusal::Unavailable)?;
        let tx = db.transaction().map_err(unavailable)?;
        let mut out = Vec::new();
        for member in members.as_array().into_iter().flatten() {
            let (Some(user), Some(level)) = (member["user_id"].as_str(), member["access_level"].as_str()) else { continue };
            let email: Option<String> = tx.query_row("SELECT email FROM users WHERE id=?1", [user], |r| r.get(0))
                .optional().map_err(unavailable)?;
            if let Some(email) = email { out.push((user.to_owned(), level.to_owned(), email)); }
        }
        tx.finish().map_err(unavailable)?;
        Ok(out)
    })();
    let Ok(addresses) = addresses else {
        engine::audit(home, "isolation.shared_member_notice_unavailable", &request.label, Severity::Critical,
            engine::audit_details(request, json!({})));
        return 0;
    };
    let scope = NoticeScope::Tenant(request.tenant.clone());
    addresses.iter().filter(|(user, level, email)| {
        let details = json!({"request_id": request.request_id, "label": request.label, "user_id": user,
            "access_level": level, "incarnation": incarnation});
        wiring::queue_or_alert(home, &scope, "isolation.shared_member_rebound", std::slice::from_ref(email), &details, false, now)
    }).count()
}

// ── Use-time gate (§3.7 "re-approved, not reactivated") ────────────────

/// Called by the connector authority for every tenant-wide (non-detail)
/// route use. A Shared employee at incarnation `N > 0` came back through a
/// Shared restore of its row `N − 1`; it may use only the tenant routes that
/// restore's approval ticked. Personal employees, incarnation 0 and other
/// route kinds pass (their own checks apply). Every unreadable or missing
/// record refuses.
pub(crate) fn tenant_route_gate(home: &Path, employee: EmployeeId, incarnation: u32, binding: &EmployeeBinding, account: &str,
    kind: ConnectorKind) -> Result<(), VerificationError> {
    if !matches!(binding, EmployeeBinding::Shared(_)) || incarnation == 0 { return Ok(()); }
    if !matches!(kind, ConnectorKind::SharedTenant | ConnectorKind::SharedHome) { return Ok(()); }
    fn fail<E>(_: E) -> VerificationError { VerificationError::Unavailable }
    let db = lifecycle::open_store(home, APPLY_DB, Access::ReadOnly).map_err(fail)?;
    let hex = hex::encode(employee.0);
    let prior = incarnation - 1;
    let row: Option<(String, String, Option<String>)> = db.query_row(
        "SELECT kind,seal,closed_by_request FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex, prior], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(fail)?;
    let (row_kind, seal, closed) = row.ok_or(VerificationError::Unavailable)?;
    if row_kind != "shared" { return Err(VerificationError::Unavailable); }
    let request = match (seal.as_str(), closed) { ("restored", Some(request)) => request, _ => return Err(VerificationError::Revoked) };
    let routes: Option<String> = db.query_row(
        "SELECT s.routes FROM shared_restore_selections s JOIN employee_restores r ON r.request_id=s.request_id
         WHERE s.request_id=?1 AND s.employee_hex=?2 AND s.incarnation=?3 AND r.kind='shared_restore' AND r.state='done'",
        params![request, hex, prior], |r| r.get(0)).optional().map_err(fail)?;
    let routes = decode_routes(&routes.ok_or(VerificationError::Revoked)?).map_err(|_| VerificationError::Unavailable)?;
    if routes.iter().any(|route| route.account == account) { Ok(()) } else { Err(VerificationError::Revoked) }
}

// ── RPC bodies (`isolation.shared_restore_*`, Admin; gates in the handler) ─

/// Notice to the log-eligible Admins (S16), L1-capped per tenant.
fn notify_admins(home: &Path, q: &Quarantined, event: &str, mut details: Value, capped: bool, now: i64) -> bool {
    let recipients = wiring::admin_notice_addresses(home, &q.label, event, now);
    details["label"] = json!(q.label);
    wiring::queue_or_alert(home, &NoticeScope::Tenant(q.tenant.clone()), event, &recipients, &details, capped, now)
}

/// `isolation.shared_restore_request` (Admin + MFA + migration step-up,
/// checked by the handler; `fresh_step_up` = that proof's freshness).
#[allow(clippy::too_many_arguments)]
pub(crate) fn request(home: &Path, env: &RestoreEnv<'_>, tenant_routes: &dyn TenantRouteSource, actor: &str, employee: &str,
    incarnation: u32, members: &Value, ticks: &Value, fresh_step_up: bool) -> Result<Value, SharedError> {
    let employee = employee_id(employee)?;
    if !members.is_array() || !ticks.is_array() { return Err(SharedError::Invalid); }
    let filed = file_shared_request(home, env, tenant_routes, employee, incarnation,
        SharedFiler { admin_user_id: actor, fresh_step_up }, members, ticks)?;
    let request = &filed.request;
    let q = { let db = engine::open_apply(home)?; engine::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)? };
    let notified = notify_admins(home, &q, "isolation.shared_restore_requested", json!({"request_id": request.request_id,
        "requester_user_id": request.requester, "expires_at": request.expires_at, "members": filed.members.len()}), true, env.now);
    Ok(json!({"request_id": request.request_id, "kind": request.kind.as_str(), "state": request.state, "label": request.label,
        "employee_id": hex::encode(employee.0), "incarnation": incarnation, "expires_at": request.expires_at,
        "approval_required": true, "members": selection_value(&filed.members), "members_dropped": filed.dropped,
        "routes": filed.routes.iter().map(TenantRoute::to_value).collect::<Vec<_>>(), "routes_need_provisioning": true,
        "notified": notified}))
}

/// `isolation.shared_restore_options` (Admin): what the request and approval
/// cards show for one Shared row — the tombstone-time members with levels and
/// whether each account is still active, the tickable tenant routes, the
/// stripped grants, and the open Shared request with its selection and ticks.
/// Read-only.
pub(crate) fn options(home: &Path, tenant_routes: &dyn TenantRouteSource, employee: &str, incarnation: u32, now: i64)
    -> Result<Value, SharedError> {
    let employee = employee_id(employee)?;
    engine::expire_requests(home, now)?;
    let db = open(home)?;
    let q = engine::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)?;
    check_shared_row(&q)?;
    let snapshot = q.principal_snapshot["members"].as_array().cloned().ok_or(Refusal::Inconsistent)?;
    let ids: Vec<&str> = snapshot.iter().map(|m| m["user_id"].as_str().ok_or(Refusal::Inconsistent)).collect::<Result<_, _>>()?;
    let states = member_states(home, &ids)?;
    let members: Vec<Value> = snapshot.iter().zip(states).map(|(member, state)| json!({
        "user_id": member["user_id"], "access_level": member["access_level"], "is_primary": member["is_primary"],
        "active": state.is_none(), "dropped_reason": state,
    })).collect();
    let routes = tenant_routes.tickable(&q.tenant).map_err(unavailable)?;
    let stripped = crate::isolation_quarantine::read_quarantine(&db, employee, incarnation).map_err(unavailable)?
        .map(|row| row.stripped).unwrap_or(Value::Null);
    let open_request = match engine::open_request(&db, employee, incarnation)? {
        Some(request) if request.kind == RequestKind::SharedRestore => {
            let stored = read_selection(&db, &request)?;
            Some(json!({"request_id": request.request_id, "state": request.state, "requester_user_id": request.requester,
                "created_at": request.created_at, "expires_at": request.expires_at, "members": request.member_selection,
                "members_dropped": stored.as_ref().map(|s| s.dropped.clone()),
                "routes": stored.map(|s| s.routes.iter().map(TenantRoute::to_value).collect::<Vec<_>>())}))
        }
        Some(request) => Some(json!({"request_id": request.request_id, "kind": request.kind.as_str(), "state": request.state})),
        None => None,
    };
    Ok(json!({"employee_id": hex::encode(employee.0), "incarnation": incarnation, "label": q.label, "tenant_id": q.tenant,
        "seal": q.seal, "tombstoned_at": q.quarantined_at,
        "restore_deadline": q.quarantined_at.saturating_add(engine::RESTORE_WINDOW_SECS),
        "members": members, "routes": routes.iter().map(TenantRoute::to_value).collect::<Vec<_>>(),
        "stripped": stripped, "work_paused": true, "routes_need_provisioning": true, "open_request": open_request}))
}

#[cfg(test)]
#[path = "isolation_restore_shared_unit_tests.rs"]
mod unit_tests;
