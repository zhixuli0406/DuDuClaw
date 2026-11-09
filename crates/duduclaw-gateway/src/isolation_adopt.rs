//! Adoption of a tombstoned protected employee under a **new** employee id
//! (native Linux isolation v1.81, F6 design r5 §3.10, Q8, task F6g).
//!
//! When: the quarantine row is `sealed` and the root answers an
//! authoritative `NotFound` for the old id (`ROOT_GONE`, S5b). A root that is
//! unavailable, inconsistent or retired never qualifies (R5, S4, S5).
//!
//! - Request ([`file_adoption`]): Personal by the owner (Q2 factor,
//!   `Personal(same owner)` only) or — owner decision 2026-11-09, F6fa2 —
//!   by an Admin for an owner who had no second factor at the tombstone
//!   ([`Filer::AdminForOwner`]; `filed_by_admin`, `owner_unverified`, a third
//!   qualifying Admin approves the card), Shared by an Admin (`Shared(pinned
//!   tenant)` only, member selection ⊆ the tombstone snapshot at ≤ its
//!   level). Journal: `employee_restores` kinds `personal_adopt` /
//!   `shared_adopt` with `target_label` (original label if free).
//! - Proposal ([`prepare_proposal`] + [`bind_proposal`], called from the
//!   handler's `prepare_propose` / `stage_prepared`): an Employee proposal
//!   with the digest-covered field `adopt_quarantine =
//!   <hex>:<incarnation>:<digest>` and `settings` parsed from the sealed
//!   agent.toml after the transform; the row goes `adopting`.
//! - Approval and apply ([`validate_item`], [`apply_item`],
//!   [`compensate_item`], `isolation_adopt_apply.rs`): the F6c2 rule on the
//!   card, the allowlist transform ([`ADOPT_SETTINGS_KEYS`]), the label-store
//!   re-key with a positive zero count (S10), the `adopted` marker with the
//!   `duduclaw-employee-dir-v2` digest (R8), `root_absent` (R12).
//! - Recovery ([`recover_adoptions`], §3.6): finish the re-key, then roll
//!   back or forward from the proposal status.
//!
//! Nothing label-keyed is carried (N1): every non-digest store is re-keyed
//! under `quarantine:<hex>-<inc>` and purged with the row. The old root
//! workspace is never read or copied (the new allocation is empty). Lock
//! order apply.db before users.db (M6). Everything here is blocking.
use std::path::Path;

use duduclaw_auth::admin_approval_rule::{AdminApprovalRule, ApprovalDecision, ApprovalKind, ApprovalRequest};
use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation;
use duduclaw_migration::contract::{ConfirmationItem, ConfirmationStatus, ProposalValue};
use duduclaw_security::audit::Severity;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::fs_safe::sqlite::Access;
use crate::isolation_lifecycle::{self as lifecycle, EmployeeKind, CONFIRMATIONS_DB, USERS_DB};
use crate::isolation_quarantine as quarantine;
use crate::isolation_restore::{self as restore, OwnerProof, Quarantined, Refusal, RequestKind, RestoreEnv, RestoreRequest,
    RouteSource};
use crate::isolation_retire::{self as retire, RetireRoot, RootClass as RootState};

#[path = "isolation_adopt_apply.rs"]
mod apply;
pub(crate) use apply::{apply_item, compensate_item, filter_settings, rename_agent, sealed_inputs, settings_from_toml, Adopted,
    AdoptHook, SealedInputs};
#[cfg(test)]
pub(crate) use apply::ADOPT_HOOK;

/// Proposal field naming the adopted quarantine row (§3.10 step 2).
pub(crate) const ADOPT_FIELD: &str = "adopt_quarantine";
/// The only `.claude/settings.json` keys an adoption keeps (S3): narrowing
/// keys only. `hooks` is regenerated for the new id; every command-running or
/// widening key (`env`, `apiKeyHelper`, `statusLine`, `mcpServers`,
/// `permissions.allow`, …) is dropped by construction.
pub(crate) const ADOPT_SETTINGS_KEYS: [&str; 3] = ["permissions.deny", "permissions.ask", "model"];
/// The only top-level entries the transform carries (same set as the
/// creation marker's allowlist, `employee_dirs::ALLOWED_TOP`).
pub(crate) const ADOPT_ALLOWED_TOP: [&str; 5] = ["SOUL.md", "CONTRACT.toml", "agent.toml", "SKILLS", ".claude"];
const ADOPT_DOMAIN: &[u8] = b"duduclaw-adopt-request-v1\0";
const RATE_WINDOW_SECS: i64 = 30 * 86_400;

/// Why an adoption step refused. [`AdoptError::code`] is the stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdoptError {
    /// A common check of the restore engine refused (§3.4).
    Restore(Refusal),
    /// The root did not answer an authoritative `NotFound` (R5, S4, S5).
    Root(RootState),
    /// The target label is held (R6): live row, marker, proposal, directory
    /// or an open quarantine row.
    LabelNotFree,
    NotSameOwner,
    NotSameTenant,
    MembersInvalid,
    /// A kept file holds an absolute path under the old label's directory.
    PathRejected,
    SettingsInvalid,
    /// The proposal's settings differ from the transformed sealed agent.toml.
    SettingsChanged,
    DigestMismatch,
    /// The post-transform tree fails the v2 digest rules (`FOREIGN_DIR`).
    ForeignDir,
    /// A registered store still holds rows under the label (S10).
    StoresNotEmpty,
    /// The row was already adopted, or the proposal names no open request.
    AlreadyUsed,
    Approval(&'static str),
    Inconsistent,
    Unavailable,
}

impl AdoptError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Restore(refusal) => refusal.code(),
            Self::Root(class) => class.code(),
            Self::LabelNotFree => "LABEL_NOT_FREE",
            Self::NotSameOwner => "ADOPT_NOT_SAME_OWNER",
            Self::NotSameTenant => "ADOPT_NOT_SAME_TENANT",
            Self::MembersInvalid => "ADOPT_MEMBERS_INVALID",
            Self::PathRejected => "ADOPT_PATH_REJECTED",
            Self::SettingsInvalid => "ADOPT_SETTINGS_INVALID",
            Self::SettingsChanged => "ADOPT_SETTINGS_CHANGED",
            Self::DigestMismatch => "ADOPT_DIGEST_MISMATCH",
            Self::ForeignDir => "FOREIGN_DIR",
            Self::StoresNotEmpty => "STORES_NOT_EMPTY",
            Self::AlreadyUsed => "ADOPT_ALREADY_USED",
            Self::Approval(code) => code,
            Self::Inconsistent => "ADOPT_INCONSISTENT",
            Self::Unavailable => "ADOPT_UNAVAILABLE",
        }
    }
    /// zh-TW text for the propose RPC (F6e shows the others by code).
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Root(RootState::Unavailable) => "主機暫時無法連線，請稍後再試",
            Self::Root(RootState::Inconsistent) => "主機資源與紀錄不一致，請聯絡維運",
            Self::Root(_) => "這位員工的主機資源仍存在或已釋放，無法由新員工接手。",
            Self::LabelNotFree => "這個員工代號目前不能使用，請換一個。",
            Self::PathRejected => "隔離區中要帶過去的檔案含有舊員工資料夾的路徑，無法由新員工接手。",
            Self::SettingsInvalid | Self::ForeignDir => "隔離區中的員工設定無法帶過去，無法由新員工接手。",
            Self::SettingsChanged | Self::DigestMismatch | Self::Inconsistent => "申請內容已變更，請重新載入後再試。",
            Self::AlreadyUsed => "這筆隔離區資料已由新員工接手，或申請已處理。",
            Self::NotSameOwner | Self::NotSameTenant | Self::MembersInvalid => "員工的歸屬不正確，無法由新員工接手。",
            _ => "目前無法處理接手申請，已拒絕操作。",
        }
    }
}

impl From<Refusal> for AdoptError {
    fn from(refusal: Refusal) -> Self { Self::Restore(refusal) }
}

fn unavailable<E>(_: E) -> AdoptError { AdoptError::Unavailable }

/// `adopt_quarantine = <hex>:<incarnation>:<digest>` (canonical lowercase
/// hex, decimal incarnation without leading zeros).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdoptRef { pub(crate) employee: EmployeeId, pub(crate) incarnation: u32, pub(crate) digest: String }

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl AdoptRef {
    pub(crate) fn encode(&self) -> String { format!("{}:{}:{}", hex::encode(self.employee.0), self.incarnation, self.digest) }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split(':');
        let (employee, incarnation, digest) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !lower_hex(employee, 32) || !lower_hex(digest, 64) { return None; }
        if incarnation.is_empty() || (incarnation.len() > 1 && incarnation.starts_with('0'))
            || !incarnation.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Self { employee: EmployeeId(hex::decode(employee).ok()?.try_into().ok()?), incarnation: incarnation.parse().ok()?,
            digest: digest.to_owned() })
    }
    pub(crate) fn store_key(&self) -> String { crate::isolation_label_stores::quarantine_key(self.employee, self.incarnation) }
}

/// The adoption field of an Employee proposal: `Ok(None)` for a normal
/// proposal; a malformed or repeated field is `Err`.
pub(crate) fn proposal_ref(item: &ConfirmationItem) -> Result<Option<AdoptRef>, ()> {
    let mut found = item.fields.iter().filter(|field| field.name == ADOPT_FIELD);
    let Some(field) = found.next() else { return Ok(None) };
    if found.next().is_some() { return Err(()); }
    match &field.value {
        ProposalValue::Text { value } => AdoptRef::parse(value).map(Some).ok_or(()),
        _ => Err(()),
    }
}

/// Collaborators of every adoption step.
pub(crate) struct AdoptEnv<'a> {
    /// The schema-2 allocation socket (authoritative `NotFound`, F6l).
    pub(crate) root: &'a dyn RetireRoot,
    pub(crate) routes: &'a dyn RouteSource,
    pub(crate) runs: &'a dyn quarantine::RunStopper,
    /// F6c2 (`EligibilityLogRule` in production).
    pub(crate) rule: &'a dyn AdminApprovalRule,
    /// Regenerates the hooks of the new `agents/<label>` (`agent_hook_installer`).
    pub(crate) hooks: &'a dyn Fn(&Path) -> Result<(), ()>,
    /// Agent-count cap (0 = unlimited), read by the caller (L2).
    pub(crate) agent_cap: usize,
    /// The verified actor (fence retries, audit).
    pub(crate) actor: &'a str,
    pub(crate) now: i64,
}

impl AdoptEnv<'_> {
    /// The restore engine's view (its root probe, store marks and rule are
    /// never consulted by the checks adoption reuses).
    pub(crate) fn restore(&self) -> RestoreEnv<'_> {
        RestoreEnv { root: &restore::SpawndRootProbe, routes: self.routes, stores: &restore::UnregisteredStores, runs: self.runs,
            rule: &restore::ApprovalRuleUnavailable, agent_cap: self.agent_cap, actor: self.actor, now: self.now }
    }
}

/// Hooks for steps that never transform (request, proposal): refuses.
pub(crate) fn no_hooks(_: &Path) -> Result<(), ()> { Err(()) }

/// Production collaborators for the request and proposal steps (the RPC
/// verified the caller). `agent_cap` 0: the license cap is read by F6d.
pub(crate) fn with_production_env<T>(home: &Path, actor: &str, now: i64, run: impl FnOnce(&AdoptEnv<'_>) -> T) -> T {
    let routes = restore::CatalogRoutes(home);
    let env = AdoptEnv { root: &retire::SpawndRetireRoot, routes: &routes, runs: &quarantine::SpawndRunStopper,
        rule: &duduclaw_auth::admin_approval_rule::EligibilityLogRule, hooks: &no_hooks, agent_cap: 0, actor, now };
    run(&env)
}

// ── Marker schema (R8) ──────────────────────────────────────────────────

/// `employee_dir_markers` plus the F6g columns `origin` (`created` for every
/// existing row) and `quarantine_ref`. Idempotent.
pub(crate) fn ensure_marker_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(crate::handlers::employee_dirs::MARKER_SCHEMA)?;
    for (column, ddl) in [
        ("origin", "ALTER TABLE employee_dir_markers ADD COLUMN origin TEXT NOT NULL DEFAULT 'created' CHECK (origin IN ('created','adopted'))"),
        ("quarantine_ref", "ALTER TABLE employee_dir_markers ADD COLUMN quarantine_ref TEXT"),
    ] {
        let present: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('employee_dir_markers') WHERE name=?1)",
            [column], |r| r.get(0))?;
        if !present { db.execute_batch(ddl)?; }
    }
    Ok(())
}

/// `(owner, digest, origin, quarantine_ref)` of the marker on `label`.
pub(crate) fn marker(db: &Connection, label: &str) -> Result<Option<(String, String, String, Option<String>)>, AdoptError> {
    let table: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_dir_markers')",
        [], |r| r.get(0)).map_err(unavailable)?;
    if !table { return Ok(None); }
    let origin: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('employee_dir_markers') WHERE name='origin')",
        [], |r| r.get(0)).map_err(unavailable)?;
    if !origin {
        let row: Option<(String, String)> = db.query_row("SELECT owner_user_id,digest FROM employee_dir_markers WHERE label=?1",
            [label], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(unavailable)?;
        return Ok(row.map(|(owner, digest)| (owner, digest, "created".to_owned(), None)));
    }
    db.query_row("SELECT owner_user_id,digest,origin,quarantine_ref FROM employee_dir_markers WHERE label=?1", [label],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(unavailable)
}

// ── Journal ─────────────────────────────────────────────────────────────

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// The adoption request digest: the §3.4 request digest plus the target label.
pub(crate) fn adopt_request_digest(request: &RestoreRequest, quarantine_digest: &str, target: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(ADOPT_DOMAIN);
    put(&mut hash, "request", restore::request_digest(request, quarantine_digest).as_bytes());
    put(&mut hash, "target", target.as_bytes());
    hex::encode(hash.finalize())
}

/// `(target_label, proposal_id)` of one request.
pub(crate) fn adoption_columns(db: &Connection, request_id: &str) -> Result<(Option<String>, Option<String>), AdoptError> {
    db.query_row("SELECT target_label,proposal_id FROM employee_restores WHERE request_id=?1", [request_id],
        |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(unavailable)?.ok_or(AdoptError::Restore(Refusal::NotFound))
}

fn is_adopt(kind: RequestKind) -> bool { matches!(kind, RequestKind::PersonalAdopt | RequestKind::SharedAdopt) }

/// The adoption request a proposal (hex proposal id) is bound to.
pub(crate) fn request_for_proposal(db: &Connection, employee: EmployeeId, incarnation: u32, proposal: &str)
    -> Result<Option<RestoreRequest>, AdoptError> {
    let id: Option<String> = db.query_row("SELECT request_id FROM employee_restores WHERE employee_hex=?1 AND incarnation=?2
        AND proposal_id=?3 AND kind IN ('personal_adopt','shared_adopt')", params![hex::encode(employee.0), incarnation, proposal],
        |r| r.get(0)).optional().map_err(unavailable)?;
    match id {
        Some(id) => Ok(restore::read_request(db, &id)?),
        None => Ok(None),
    }
}

/// Where the apply.db checks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdoptStage<'a> {
    /// Filing: row `sealed`, no open request.
    Request,
    /// Proposal staging: row `sealed`, only this request open.
    Proposal(&'a str),
    /// Approval and apply: row `adopting`, only this request open.
    Adopting(&'a str),
}

/// The apply.db half of the common checks (§3.4) for an adoption, inside the
/// caller's transaction: as the restore checks, except that the root
/// allocation may already be `root_absent` and the label's confirmed row may
/// be any disabled row (R6).
pub(crate) fn check_apply_db(db: &Connection, q: &Quarantined, stage: AdoptStage<'_>, now: i64) -> Result<(), AdoptError> {
    let (seal, own) = match stage {
        AdoptStage::Request => ("sealed", None),
        AdoptStage::Proposal(id) => ("sealed", Some(id)),
        AdoptStage::Adopting(id) => ("adopting", Some(id)),
    };
    let fresh = restore::read_row(db, q.employee, q.incarnation)?.ok_or(Refusal::NotFound)?;
    if fresh.label != q.label || fresh.digest != q.digest || fresh.root != q.root { return Err(AdoptError::Inconsistent); }
    if fresh.seal == "adopted" { return Err(AdoptError::AlreadyUsed); }
    if matches!(fresh.seal.as_str(), "restoring" | "adopting") && fresh.seal != seal { return Err(Refusal::RestoreInProgress.into()); }
    if fresh.seal != seal || fresh.digest.is_none() { return Err(Refusal::NotSealed.into()); }
    if now < fresh.quarantined_at || now > fresh.quarantined_at.saturating_add(restore::RESTORE_WINDOW_SECS) {
        return Err(Refusal::Expired.into());
    }
    let state: Option<String> = db.query_row("SELECT root_state FROM root_allocations WHERE employee_hex=?1",
        [hex::encode(q.employee.0)], |r| r.get(0)).optional().map_err(unavailable)?;
    match state.as_deref() {
        Some("allocated" | "root_absent") => {}
        Some("retiring") => return Err(Refusal::RetireInProgress.into()),
        Some("retired") => return Err(AdoptError::Root(RootState::Retired)),
        _ => return Err(AdoptError::Unavailable),
    }
    for (employee, incarnation, other) in restore::rows_for_label(db, &q.label)? {
        if (employee, incarnation) != (q.employee, q.incarnation) && matches!(other.as_str(), "restoring" | "adopting") {
            return Err(Refusal::RestoreInProgress.into());
        }
    }
    match (restore::open_request(db, q.employee, q.incarnation)?, own) {
        (None, None) => {}
        (Some(open), Some(id)) if open.request_id == id => {}
        _ => return Err(Refusal::RestoreInProgress.into()),
    }
    if !fresh.pending_fences.is_empty() { return Err(Refusal::FencePending.into()); }
    let row: Option<(String, Option<String>, i64)> = db.query_row(
        "SELECT receipt,tenant_id,incarnation FROM confirmed_employees WHERE label=?1", [&q.label],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(unavailable)?;
    if let Some((encoded, pinned, incarnation)) = row {
        let receipt: allocation::Receipt = serde_json::from_str(&encoded).map_err(unavailable)?;
        if receipt.label != q.label || !receipt.disabled { return Err(AdoptError::Inconsistent); }
        if receipt.employee == q.employee {
            if incarnation != i64::from(q.incarnation) { return Err(AdoptError::Inconsistent); }
            if pinned.as_deref() != Some(q.tenant.as_str()) { return Err(Refusal::TenantMismatch.into()); }
            let owner = match &receipt.principal_binding { Some(EmployeeBinding::Personal(o)) => Some(o.as_str()), _ => None };
            if q.owner.as_deref() != owner { return Err(AdoptError::Inconsistent); }
        }
    }
    Ok(())
}

/// R6 for the target label inside the caller's apply.db transaction: no live
/// confirmed row, no marker (before the transform).
fn target_free_in(db: &Connection, target: &str) -> Result<(), AdoptError> {
    if marker(db, target)?.is_some() { return Err(AdoptError::LabelNotFree); }
    let encoded: Option<String> = db.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [target], |r| r.get(0))
        .optional().map_err(unavailable)?;
    if let Some(encoded) = encoded {
        let receipt: allocation::Receipt = serde_json::from_str(&encoded).map_err(unavailable)?;
        if !receipt.disabled { return Err(AdoptError::LabelNotFree); }
    }
    Ok(())
}

/// The root answer must be an authoritative `NotFound` (or an already
/// recorded `root_absent`). `ROOT_INCONSISTENT` alerts the operator.
pub(crate) fn require_root_gone(home: &Path, env: &AdoptEnv<'_>, q: &Quarantined) -> Result<(), AdoptError> {
    let check = retire::root_precheck(home, q.employee, env.root).map_err(unavailable)?;
    match check.class {
        RootState::Gone => Ok(()),
        RootState::Inconsistent => {
            restore::audit(home, "isolation.employee_root_inconsistent", &q.label, Severity::Critical,
                json!({"employee": hex::encode(q.employee.0), "incarnation": q.incarnation, "stage": "adoption"}));
            Err(AdoptError::Root(RootState::Inconsistent))
        }
        class => Err(AdoptError::Root(class)),
    }
}

/// Every check outside apply.db for one adoption onto `target`; returns the
/// route list the card shows for re-provisioning (never carried).
fn check_outside(home: &Path, env: &AdoptEnv<'_>, q: &Quarantined, target: &str) -> Result<Vec<(String, u64)>, AdoptError> {
    if !lifecycle::valid_label(target) { return Err(AdoptError::LabelNotFree); }
    restore::ensure_tombstone_finished(home, q)?;
    if !env.routes.detail_routes_naming(q.employee).map_err(unavailable)?.is_empty() { return Err(Refusal::FencePending.into()); }
    restore::check_users(home, q)?;
    restore::check_no_open_proposal(home, &q.label)?;
    if target != q.label { restore::check_no_open_proposal(home, target)?; }
    restore::check_target_absent(home, target).map_err(|refusal| match refusal {
        Refusal::LabelTaken => AdoptError::LabelNotFree,
        other => other.into(),
    })?;
    restore::check_quota(home, env.agent_cap)?;
    crate::isolation_label_stores::check_label_not_quarantined(home, target, Some((q.employee, q.incarnation)))
        .map_err(|refusal| match refusal {
            crate::isolation_label_stores::GateRefusal::Unavailable => AdoptError::Unavailable,
            _ => AdoptError::LabelNotFree,
        })?;
    require_root_gone(home, env, q)?;
    env.routes.routes_for(&q.binding()?, &q.tenant).map_err(unavailable)
}

/// Who files an adoption request.
pub(crate) enum Filer<'a> {
    /// The owner with a Q2 factor proof (verified by the RPC, F6d).
    Personal(Option<OwnerProof<'a>>),
    /// An Admin (step-up verified by the RPC) with the member selection
    /// `[{user_id, access_level}]` ⊆ the snapshot at ≤ its level (§3.9, S11).
    Shared { requester: &'a str, members: Value },
    /// Owner decision 2026-11-09 (F6fa2): an Admin files the Personal
    /// adoption on behalf of an owner who had no second factor at the
    /// tombstone (`restore::check_admin_filing`). `Personal(same owner)` only.
    AdminForOwner(restore::AdminFiler<'a>),
}

fn rank(level: &str) -> Option<u8> {
    match level { "owner" => Some(3), "operator" => Some(2), "viewer" => Some(1), _ => None }
}

/// §3.9 for an adoption: every selected member is in the tombstone snapshot,
/// at most at its snapshot level, once.
pub(crate) fn check_members(q: &Quarantined, members: &Value) -> Result<(), AdoptError> {
    let snapshot = q.principal_snapshot["members"].as_array().ok_or(AdoptError::MembersInvalid)?;
    let selection = members.as_array().ok_or(AdoptError::MembersInvalid)?;
    let mut seen = std::collections::BTreeSet::new();
    for member in selection {
        let user = member["user_id"].as_str().ok_or(AdoptError::MembersInvalid)?;
        let wanted = member["access_level"].as_str().and_then(rank).ok_or(AdoptError::MembersInvalid)?;
        let was = snapshot.iter().find(|old| old["user_id"].as_str() == Some(user)).ok_or(AdoptError::MembersInvalid)?;
        let had = was["access_level"].as_str().and_then(rank).ok_or(AdoptError::MembersInvalid)?;
        if wanted > had || !seen.insert(user.to_owned()) || member.as_object().is_none_or(|o| o.len() != 2) {
            return Err(AdoptError::MembersInvalid);
        }
    }
    Ok(())
}

/// `me.isolation.request_adoption` / `isolation.shared_adoption_request`
/// engine (§3.10 step 1). `target`: `None` = the original label.
pub(crate) fn file_adoption(home: &Path, env: &AdoptEnv<'_>, employee: EmployeeId, incarnation: u32, target: Option<&str>,
    filer: Filer<'_>) -> Result<RestoreRequest, AdoptError> {
    let peek = { let db = restore::open_apply(home)?; restore::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)? };
    // F6fa2: the users.db half of an Admin filing for an owner (F6c2 rule).
    let filing = |users: &Connection, admin: &str, owner: &str|
        env.rule.check_admin_filing(users, admin, owner, env.now).map_err(|refusal| refusal.code());
    let (kind, requester, members, factor, filed_by_admin) = match &filer {
        Filer::Personal(proof) => {
            let proof = proof.ok_or(Refusal::FactorRequired)?;
            // Personal(same owner) only; a foreign row answers NOT_FOUND.
            if peek.kind != EmployeeKind::Personal || peek.owner.as_deref() != Some(proof.user_id) { return Err(Refusal::NotFound.into()); }
            if !matches!(proof.factor, "passkey" | "totp" | "email" | "google_reauth") { return Err(Refusal::FactorRequired.into()); }
            (RequestKind::PersonalAdopt, proof.user_id.to_owned(), None, Some(proof.factor), None)
        }
        Filer::Shared { requester, members } => {
            if peek.kind != EmployeeKind::Shared || peek.owner.is_some() { return Err(Refusal::NotFound.into()); }
            check_members(&peek, members)?;
            (RequestKind::SharedAdopt, (*requester).to_owned(), Some(members.clone()), None, None)
        }
        Filer::AdminForOwner(admin) => {
            restore::check_admin_filing(home, &peek, admin.admin_user_id, admin.fresh_step_up, &filing)?;
            (RequestKind::PersonalAdopt, admin.admin_user_id.to_owned(), None, None, Some(admin.admin_user_id.to_owned()))
        }
    };
    let target = target.unwrap_or(&peek.label).to_owned();
    restore::recover_label(home, &env.restore(), &peek.label)?;
    recover_adoptions(home, env, &peek.label)?;
    let q = restore::load_fenced(home, &env.restore(), employee, incarnation)?;
    if let Filer::AdminForOwner(admin) = &filer {
        restore::check_admin_filing(home, &q, admin.admin_user_id, admin.fresh_step_up, &filing)?;
    }
    let routes = check_outside(home, env, &q, &target)?;
    let mut db = restore::open_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    check_apply_db(&tx, &q, AdoptStage::Request, env.now)?;
    if target != q.label { target_free_in(&tx, &target)?; }
    let recent: i64 = tx.query_row("SELECT count(*) FROM employee_restores WHERE employee_hex=?1 AND incarnation=?2 AND created_at>?3",
        params![hex::encode(employee.0), incarnation, env.now - RATE_WINDOW_SECS], |r| r.get(0)).map_err(unavailable)?;
    if recent >= restore::MAX_REQUESTS_PER_ROW { return Err(Refusal::RateLimited.into()); }
    let unsigned = RestoreRequest {
        request_id: hex::encode(uuid::Uuid::new_v4().as_bytes()), kind, employee, incarnation, label: q.label.clone(),
        owner: q.owner.clone(), tenant: q.tenant.clone(), requester, digest: String::new(),
        route_digest: restore::route_digest(&routes), member_selection: members, created_at: env.now,
        expires_at: env.now.saturating_add(restore::REQUEST_TTL_SECS).min(q.quarantined_at.saturating_add(restore::RESTORE_WINDOW_SECS)),
        state: "requested".into(), approver: None, self_approved: None, failure: None, filed_by_admin,
    };
    let digest = adopt_request_digest(&unsigned, q.digest.as_deref().ok_or(Refusal::NotSealed)?, &target);
    let request = RestoreRequest { digest, ..unsigned };
    tx.execute("INSERT INTO employee_restores(request_id,kind,employee_hex,incarnation,label,target_label,owner_user_id,tenant_id,
            requester_user_id,digest,route_digest,member_selection,created_at,expires_at,state,filed_by_admin,owner_unverified)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'requested',?15,?16)",
        params![request.request_id, request.kind.as_str(), hex::encode(employee.0), incarnation, request.label, target,
            request.owner, request.tenant, request.requester, request.digest, request.route_digest,
            request.member_selection.as_ref().map(Value::to_string), request.created_at, request.expires_at,
            request.filed_by_admin, request.owner_unverified()])
        .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    let (event, severity) = if request.owner_unverified() {
        ("isolation.employee_adoption_requested_for_owner", Severity::Warning)
    } else {
        ("isolation.employee_adoption_requested", Severity::Info)
    };
    restore::audit(home, event, &request.label, severity, restore::audit_details(&request, json!({"factor": factor,
        "target_label": target, "routes": routes.len(), "filed_by_admin": request.filed_by_admin,
        "owner_unverified": request.owner_unverified()})));
    Ok(request)
}

/// What the Admin's propose step stages (§3.10 step 2) and the card shows.
#[derive(Debug, Clone)]
pub(crate) struct AdoptionPlan {
    pub(crate) request: RestoreRequest,
    pub(crate) target: String,
    pub(crate) binding: EmployeeBinding,
    pub(crate) adopt: AdoptRef,
    /// `settings` parsed from the transformed sealed agent.toml.
    pub(crate) settings: String,
    /// Card facts (F6e): kept / sealed entry names, tombstoned at, data size.
    pub(crate) card: Value,
}

fn binding_of(q: &Quarantined) -> Result<EmployeeBinding, AdoptError> {
    Ok(match (&q.kind, &q.owner) {
        (EmployeeKind::Personal, Some(owner)) => EmployeeBinding::Personal(Text::new(owner.clone()).map_err(unavailable)?),
        (EmployeeKind::Shared, None) => EmployeeBinding::Shared(Text::new(q.tenant.clone()).map_err(unavailable)?),
        _ => return Err(AdoptError::Inconsistent),
    })
}

/// The request as filed, re-checked for the proposal (Admin + step-up
/// verified by the caller). Nothing is written.
pub(crate) fn prepare_proposal(home: &Path, env: &AdoptEnv<'_>, request_id: &str) -> Result<AdoptionPlan, AdoptError> {
    let request = { let db = restore::open_apply(home)?; restore::read_request(&db, request_id)?.ok_or(Refusal::NotFound)? };
    if !is_adopt(request.kind) { return Err(Refusal::NotSupported.into()); }
    restore::recover_label(home, &env.restore(), &request.label)?;
    recover_adoptions(home, env, &request.label)?;
    let (request, target, proposal) = {
        let db = restore::open_apply(home)?;
        let request = restore::read_request(&db, request_id)?.ok_or(Refusal::NotFound)?;
        let (target, proposal) = adoption_columns(&db, request_id)?;
        (request, target.ok_or(AdoptError::Inconsistent)?, proposal)
    };
    if request.state != "requested" || proposal.is_some() { return Err(AdoptError::AlreadyUsed); }
    if env.now > request.expires_at { return Err(Refusal::Expired.into()); }
    let q = restore::load_fenced(home, &env.restore(), request.employee, request.incarnation)?;
    let sealed_digest = q.digest.clone().ok_or(Refusal::NotSealed)?;
    if adopt_request_digest(&request, &sealed_digest, &target) != request.digest { return Err(AdoptError::DigestMismatch); }
    let routes = check_outside(home, env, &q, &target)?;
    {
        let db = restore::open_apply(home)?;
        check_apply_db(&db, &q, AdoptStage::Proposal(request_id), env.now)?;
        if target != q.label { target_free_in(&db, &target)?; }
    }
    let inputs = sealed_inputs(home, &q, &target)?;
    let settings = apply::settings_from_toml(&inputs.agent_toml)?;
    let card = json!({
        "adopted_from": q.label, "tombstoned_at": q.quarantined_at, "data_bytes": inputs.kept_bytes,
        "kept": inputs.kept, "sealed": inputs.sealed, "routes_to_reprovision": routes.len(),
        "soul_summary": duduclaw_core::truncate_chars(&inputs.soul_summary, 280),
    });
    Ok(AdoptionPlan { binding: binding_of(&q)?, adopt: AdoptRef { employee: q.employee, incarnation: q.incarnation, digest: sealed_digest },
        request, target, settings, card })
}

/// Inside the propose step's apply.db writer transaction (`db`), right after
/// the proposal was staged: bind it to the request, row `sealed → adopting`.
pub(crate) fn bind_proposal(db: &Connection, plan: &AdoptionPlan, proposal: &str, now: i64) -> Result<(), AdoptError> {
    let q = restore::read_row(db, plan.adopt.employee, plan.adopt.incarnation)?.ok_or(Refusal::NotFound)?;
    if q.digest.as_deref() != Some(plan.adopt.digest.as_str()) { return Err(AdoptError::DigestMismatch); }
    check_apply_db(db, &q, AdoptStage::Proposal(&plan.request.request_id), now)?;
    let changed = db.execute("UPDATE employee_restores SET proposal_id=?2 WHERE request_id=?1 AND state='requested' AND proposal_id IS NULL",
        params![plan.request.request_id, proposal]).map_err(unavailable)?;
    if changed != 1 { return Err(AdoptError::AlreadyUsed); }
    restore::set_seal(db, q.employee, q.incarnation, &["sealed"], "adopting")?;
    db.execute("UPDATE employee_quarantine SET closed_by_request=?3 WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(q.employee.0), q.incarnation, plan.request.request_id]).map_err(unavailable)?;
    Ok(())
}

/// Audit after [`bind_proposal`] committed.
pub(crate) fn audit_proposed(home: &Path, plan: &AdoptionPlan, proposal: &str, actor: &str) {
    restore::audit(home, "isolation.employee_adoption_proposed", &plan.request.label, Severity::Info,
        restore::audit_details(&plan.request, json!({"proposal_id": proposal, "target_label": plan.target, "actor_user_id": actor})));
}

// ── Approval card and apply validation ─────────────────────────────────

/// Which side of the transform an adoption item is on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Phase {
    /// Row `adopting`, request open: the card may be approved / applied.
    Pending { request: RestoreRequest, row: Quarantined, target: String, self_approved: bool },
    /// Row `adopted` by this proposal; the tree passes its marker.
    Adopted,
}

pub(crate) fn proposal_key(item: &ConfirmationItem) -> String { hex::encode(item.binding.operation_id.proposal_id.0) }

fn text_field<'a>(item: &'a ConfirmationItem, name: &str) -> Option<&'a str> {
    item.fields.iter().find(|field| field.name == name).and_then(|field| match &field.value {
        ProposalValue::Text { value } => Some(value.as_str()),
        _ => None,
    })
}

/// The F6c2 rule inside a users.db read transaction (M6: after apply.db).
fn decide(home: &Path, env: &AdoptEnv<'_>, request: &RestoreRequest, approver: &str) -> Result<bool, AdoptError> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(unavailable)?;
    let tx = users.transaction().map_err(unavailable)?;
    if let Some(owner) = &request.owner {
        if !lifecycle::owner_eligible(&tx, owner).map_err(unavailable)? { return Err(Refusal::OwnerIneligible.into()); }
    }
    let kind = if request.kind == RequestKind::PersonalAdopt { ApprovalKind::PersonalAdopt } else { ApprovalKind::SharedAdopt };
    let approval = ApprovalRequest { kind, requester_user_id: &request.requester,
        owner_user_id: request.owner.as_deref(), approver_user_id: approver, fresh_step_up: true, now: env.now };
    // F6fa2: an Admin-filed Personal adoption needs a third qualifying Admin
    // (filer and owner are parties; no sole-Admin path).
    let admin_filed = match request.filed_by_admin.as_deref() {
        None => false,
        Some(filer) if filer == request.requester && request.kind == RequestKind::PersonalAdopt
            && request.owner.as_deref().is_some_and(|owner| owner != filer) => true,
        Some(_) => return Err(AdoptError::Inconsistent),
    };
    let decision = if admin_filed { env.rule.decide_admin_filed(&tx, &approval) } else { env.rule.decide(&tx, &approval) };
    tx.finish().map_err(unavailable)?;
    match decision {
        ApprovalDecision::Allowed { self_approved, .. } => {
            let party = approver == request.requester || request.owner.as_deref() == Some(approver);
            if self_approved != party || (admin_filed && (self_approved || party)) {
                return Err(AdoptError::Approval("APPROVAL_RULE_INCONSISTENT"));
            }
            Ok(self_approved)
        }
        ApprovalDecision::Refused(refusal) => Err(AdoptError::Approval(refusal.code())),
    }
}

/// Approval and every apply step of an Employee proposal carrying
/// `adopt_quarantine` (`EmployeeAuthority::validate_current`). `approver`:
/// the live Admin while Pending, else the approval record's actor.
pub(crate) fn validate_item(home: &Path, env: &AdoptEnv<'_>, item: &ConfirmationItem, adopt: &AdoptRef, approver: &str)
    -> Result<Phase, AdoptError> {
    let spec = allocation::specification(item).map_err(|_| AdoptError::Inconsistent)?;
    let binding = spec.binding.clone().ok_or(AdoptError::Inconsistent)?;
    let db = restore::open_apply(home)?;
    let q = restore::read_row(&db, adopt.employee, adopt.incarnation)?.ok_or(Refusal::NotFound)?;
    if q.digest.as_deref() != Some(adopt.digest.as_str()) { return Err(AdoptError::DigestMismatch); }
    // Personal(same owner) / Shared(same tenant) only.
    match (&binding, &q.kind, &q.owner) {
        (EmployeeBinding::Personal(owner), EmployeeKind::Personal, Some(row_owner)) if owner.as_str() == row_owner.as_str() => {}
        (EmployeeBinding::Personal(_), _, _) => return Err(AdoptError::NotSameOwner),
        (EmployeeBinding::Shared(tenant), EmployeeKind::Shared, None) if tenant.as_str() == q.tenant => {}
        (EmployeeBinding::Shared(_), _, _) => return Err(AdoptError::NotSameTenant),
    }
    let key = proposal_key(item);
    let request = request_for_proposal(&db, q.employee, q.incarnation, &key)?.ok_or(AdoptError::AlreadyUsed)?;
    let (target, _) = adoption_columns(&db, &request.request_id)?;
    let target = target.ok_or(AdoptError::Inconsistent)?;
    if target != spec.label { return Err(AdoptError::Inconsistent); }
    let settings = text_field(item, "settings").ok_or(AdoptError::SettingsInvalid)?;
    if q.seal == "adopted" && request.state == "done" {
        let adopted_by: Option<String> = db.query_row("SELECT adopted_by_employee FROM employee_quarantine
            WHERE employee_hex=?1 AND incarnation=?2", params![hex::encode(q.employee.0), q.incarnation], |r| r.get(0))
            .map_err(unavailable)?;
        let (_, digest, origin, reference) = marker(&db, &target)?.ok_or(AdoptError::Inconsistent)?;
        if adopted_by.is_none() || origin != "adopted" || reference.as_deref() != Some(adopt.encode().as_str()) {
            return Err(AdoptError::Inconsistent);
        }
        drop(db);
        let snapshot = crate::handlers::employee_dirs::snapshot(home, &home.join("agents"), &target).map_err(|_| AdoptError::ForeignDir)?;
        if snapshot.digest != digest { return Err(AdoptError::ForeignDir); }
        if snapshot.settings().map_err(|_| AdoptError::SettingsInvalid)? != settings { return Err(AdoptError::SettingsChanged); }
        return Ok(Phase::Adopted);
    }
    if !matches!(request.state.as_str(), "requested" | "applying") { return Err(AdoptError::AlreadyUsed); }
    if env.now > request.expires_at && request.state == "requested" { return Err(Refusal::Expired.into()); }
    check_apply_db(&db, &q, AdoptStage::Adopting(&request.request_id), env.now)?;
    if request.member_selection.is_some() != (request.kind == RequestKind::SharedAdopt) { return Err(AdoptError::Inconsistent); }
    if let Some(members) = &request.member_selection { check_members(&q, members)?; }
    let sealed = q.digest.clone().ok_or(Refusal::NotSealed)?;
    if adopt_request_digest(&request, &sealed, &target) != request.digest { return Err(AdoptError::DigestMismatch); }
    // F6fa2: Admin-filed only for an owner without a second factor at the tombstone.
    if request.owner_unverified() && !restore::admin_may_file_for_owner(&q) { return Err(AdoptError::Inconsistent); }
    drop(db);
    if request.state == "requested" {
        restore::check_users(home, &q)?;
        require_root_gone(home, env, &q)?;
        let inputs = sealed_inputs(home, &q, &target)?;
        if apply::settings_from_toml(&inputs.agent_toml)? != settings { return Err(AdoptError::SettingsChanged); }
    }
    let self_approved = decide(home, env, &request, approver)?;
    Ok(Phase::Pending { request, row: q, target, self_approved })
}

// ── Recovery (§3.6, §3.10 step 6) ──────────────────────────────────────

/// What recovery did for one label: `(request id, action)`.
pub(crate) type RecoveredAdoptions = Vec<(String, &'static str)>;

fn proposal_status(home: &Path, proposal: &str) -> Result<Option<ConfirmationStatus>, AdoptError> {
    let store = duduclaw_migration::confirmation::ConfirmationStore::from_read_only_connection(
        lifecycle::open_store(home, CONFIRMATIONS_DB, Access::ReadOnly).map_err(unavailable)?).map_err(unavailable)?;
    Ok(store.list().map_err(unavailable)?.into_iter()
        .filter(|item| hex::encode(item.binding.operation_id.proposal_id.0) == proposal)
        .max_by_key(|item| item.binding.operation_id.revision).map(|item| item.status))
}

/// Run before any adoption step on `label` and at boot: for every
/// `adopting` row, finish the idempotent re-key first when the apply started
/// (S10), then roll back (proposal rejected, expired, failed or gone) or
/// forward (proposal Applied and the live row's marker names the row);
/// anything else alerts and leaves the row `adopting` (`failed: inconsistent`).
pub(crate) fn recover_adoptions(home: &Path, env: &AdoptEnv<'_>, label: &str) -> Result<RecoveredAdoptions, AdoptError> {
    let rows = { let db = restore::open_apply(home)?; restore::rows_for_label(&db, label)? };
    let mut done = Vec::new();
    for (employee, incarnation, seal) in rows {
        if seal != "adopting" { continue; }
        let (q, request, target, proposal) = {
            let db = restore::open_apply(home)?;
            let q = restore::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)?;
            let request = restore::open_request(&db, employee, incarnation)?.filter(|r| is_adopt(r.kind));
            let (target, proposal) = match &request {
                Some(request) => adoption_columns(&db, &request.request_id)?,
                None => (None, None),
            };
            (q, request, target, proposal)
        };
        let adopt = AdoptRef { employee, incarnation, digest: q.digest.clone().unwrap_or_default() };
        if request.as_ref().is_some_and(|r| r.state == "applying") {
            apply::rekey_row(home, &q, &adopt)?;
        }
        let status = match &proposal { Some(id) => proposal_status(home, id)?, None => None };
        let id = request.as_ref().map(|r| r.request_id.clone()).unwrap_or_default();
        match status {
            Some(ConfirmationStatus::Pending | ConfirmationStatus::Approved | ConfirmationStatus::Applying) => {}
            Some(ConfirmationStatus::Applied) => {
                let target = target.clone().ok_or(AdoptError::Inconsistent)?;
                match apply::roll_forward(home, env, &q, &adopt, request.as_ref(), &target) {
                    Ok(()) => done.push((id, "rolled_forward")),
                    Err(error) => {
                        restore::audit(home, "isolation.employee_adoption_inconsistent", &q.label, Severity::Critical,
                            json!({"request_id": id, "employee": hex::encode(employee.0), "incarnation": incarnation,
                                "reason": error.code()}));
                        if let Some(request) = &request {
                            let mut db = restore::open_apply(home)?;
                            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
                            restore::set_request_state(&tx, &request.request_id, &["requested", "applying"], "failed",
                                Some("inconsistent"), env.now)?;
                            tx.commit().map_err(unavailable)?;
                        }
                        return Err(AdoptError::Inconsistent);
                    }
                }
            }
            other => {
                let state = match other {
                    Some(ConfirmationStatus::Rejected | ConfirmationStatus::Rejecting) => ("denied", None),
                    Some(ConfirmationStatus::ExpiredRejected) => ("expired", None),
                    _ => ("failed", Some("interrupted")),
                };
                apply::roll_back(home, env, &q, request.as_ref(), target.as_deref(), state)?;
                done.push((id, "rolled_back"));
            }
        }
    }
    Ok(done)
}
