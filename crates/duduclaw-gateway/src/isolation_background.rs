//! Durable, record-bound authority for protected background work.
//! A stored owner label is a claim awaiting approval, never authentication.
use std::{future::Future, path::{Path, PathBuf}, sync::Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use duduclaw_isolation::{capability::Lane, execution::{RunHandle, WaitOrigin}, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;

pub mod material;
mod store;
pub const APPROVAL_KIND: &str = "isolation_background_execution";
pub const APPROVAL_SPEC: crate::approval::operator_cli_gate::KindSpec = crate::approval::operator_cli_gate::KindSpec {
    kind: APPROVAL_KIND,
    validity: crate::approval::operator_cli_gate::Validity::Fixed(30 * 24 * 60),
    consume: crate::approval::operator_cli_gate::Consume::Never,
    reminders: false, max_pending_per_target: 1, max_pending_per_kind: 20,
    push_cap_per_hour: Some(2), legacy_scope_key: "agent_id",
    admin_refusal: "背景執行授權只能由管理員在儀表板核准。",
    expired_text: "這筆背景執行授權已逾期；請重新設定工作後送審。",
    channel_refusal: Some("請到儀表板確認交辦人與工作內容，再決定這筆背景執行授權。"),
    notice: |_, _, deadline| format!("有一筆背景工作需要確認交辦人與執行內容，請由管理員到儀表板審核。期限：{deadline}"),
    on_push_suppressed: |_, _, _| {},
};
const MAX_AUTHORITY_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Host-private receipt. No RPC or employee environment accepts this value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordAuthority {
    home: PathBuf,
    kind: String,
    id: String,
    agent: String,
    employee: EmployeeId,
    owner: String,
    epoch: u64,
    acl_generation: u64,
    policy_epoch: u64,
    policy: String,
    material: String,
    approval: Option<String>,
    expires_at: u64,
    lane: Lane,
    caller: Option<EmployeeId>,
    parent_run: Option<RunHandle>,
    parent_agent: Option<String>,
    parent_policy: Option<String>,
    parent: Option<Box<RecordAuthority>>,
    detached_bus: bool,
}

tokio::task_local! { static RECORD: Mutex<Option<RecordAuthority>>; static PENDING: bool; }
static REQUEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unavailable() -> VerificationError { VerificationError::Unavailable }
fn now() -> Result<u64, VerificationError> { duduclaw_mcp_boundary::authority::now_seconds() }
fn user_id(owner: &str) -> Result<String, VerificationError> {
    duduclaw_core::HumanPrincipal::parse(owner).ok()
        .and_then(|p| p.user_id().map(str::to_owned)).ok_or(VerificationError::Invalid)
}
fn snapshot(home: &Path, owner: &str, agent: &str)
    -> Result<duduclaw_auth::IsolationAuthoritySnapshot, VerificationError> {
    let user = user_id(owner)?;
    crate::decision_notify::open_user_db(home).map_err(|_| unavailable())?
        .ok_or_else(unavailable)?.isolation_authority_snapshot(Some(&user), agent)
        .map_err(|_| VerificationError::Revoked)
}

/// File one bounded approval for the exact record and captured owner epoch.
/// Repeated ticks reuse the same receipt, including a denied/stale receipt;
/// they never refresh its credential epoch or silently renew authorization.
pub async fn request_record(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value) -> Result<(), VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    if kind == "delegation" { return Err(VerificationError::Invalid); }
    request_record_inner(home, kind, id, agent, owner, material, false).await
}

/// Explicit authenticated settings saves may request a new approval after a
/// previous receipt expires or is revoked. Scheduler ticks cannot renew it.
pub async fn request_record_from_settings(home: &Path, kind: &str, id: &str, agent: &str,
    owner: &str, material: &Value) -> Result<(), VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    if kind != "heartbeat" { return Err(VerificationError::Invalid); }
    request_record_inner(home, kind, id, agent, Some(owner), material, true).await
}

async fn request_record_inner(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value, renew: bool) -> Result<(), VerificationError> {
    let owner = owner.ok_or(VerificationError::Invalid)?.to_owned();
    let digest = store::material_digest(material)?;
    let _lock = REQUEST_LOCK.try_lock().map_err(|_| unavailable())?;
    let (capture_home, capture_kind, capture_id, capture_agent) =
        (home.to_owned(), kind.to_owned(), id.to_owned(), agent.to_owned());
    let captured = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let (home, kind, id, agent) = (&capture_home, capture_kind.as_str(), capture_id.as_str(), capture_agent.as_str());
        if let Some(existing) = store::load(home, kind, id)? {
            if existing.material == digest && existing.agent == agent && existing.owner == owner {
                if !renew { return Ok(None); }
                // A still-pending card can already be stale after credential or
                // policy changes. Only an explicitly authenticated settings save
                // can replace it; polling never refreshes captured authority.
                if recheck_receipt(&existing, false, false).is_ok()
                    && (store::approval_pending(&existing)? || recheck(&existing).is_ok()) { return Ok(None); }
            }
        }
        let current = snapshot(home, &owner, agent)?;
        let employee = confirmed_employee(home, agent)?;
        let epoch = current.human.as_ref().ok_or(VerificationError::Invalid)?.credential_epoch;
        let captured = RecordAuthority { home: home.to_owned(), kind: kind.into(), id: id.into(),
            agent: agent.into(), employee, owner, epoch, acl_generation: current.acl_generation,
            policy_epoch: current.policy_epoch,
            policy: crate::approval::strict_policy_revision(home, agent).map_err(|_| unavailable())?,
            material: digest, approval: None, expires_at: now()?.checked_add(MAX_AUTHORITY_SECONDS)
                .ok_or(VerificationError::InvalidTime)?, lane: Lane::Standard,
            caller: None, parent_run: None, parent_agent: None, parent_policy: None, parent: None, detached_bus: false };
        store::validate(&captured)?;
        Ok(Some(captured))
    }).await?;
    let Some(captured) = captured else { return Ok(()); };
    let broker = crate::approval::ApprovalBroker::open(home).map_err(|_| unavailable())?;
    // Cap pending cards independently of polling cadence and record churn.
    let pending = broker.list_pending(None).await.map_err(|_| unavailable())?;
    if pending.iter().filter(|r| r.action_kind == APPROVAL_KIND).count() >= 20 { return Err(unavailable()); }
    let payload = json!({"authority": captured, "record": material});
    let approval = broker.request(agent, APPROVAL_KIND,
        "請於 24 小時內決定；核准後最多執行 30 天。內容、權限或交辦人憑證變更、取消工作後立即失效。", payload, 24 * 60 * 60)
        .await.map_err(|_| unavailable())?;
    let mut captured = captured;
    captured.approval = Some(approval.to_string());
    duduclaw_mcp_boundary::authority::verify_blocking(move || store::save(&captured)).await
}

/// The owning scheduler reads the record again on each fire and supplies its
/// execution material. This scope crosses only the explicit future boundary.
pub async fn scope_record<F: Future>(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value, future: F) -> F::Output {
    let future = Box::pin(future);
    if !duduclaw_core::execution_gate::isolation_required() { return future.await; }
    if RECORD.try_with(|_| ()).is_ok() { return future.await; }
    if kind != "delegation" { let _ = request_record(home, kind, id, agent, owner, material).await; }
    let (home, kind, id, agent, owner, material) = (home.to_owned(), kind.to_owned(), id.to_owned(),
        agent.to_owned(), owner.map(str::to_owned), material.clone());
    let (captured, pending) = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let mut pending = false;
        let captured = (|| {
            let receipt = store::load(&home, &kind, &id)?.ok_or_else(unavailable)?;
            check_material(&receipt, &agent, owner.as_deref(), &store::material_digest(&material)?)?;
            pending = store::approval_pending(&receipt).unwrap_or(false);
            recheck(&receipt)?;
            Ok::<_, VerificationError>(receipt)
        })().ok();
        Ok((captured, pending))
    }).await.unwrap_or((None, false));
    PENDING.scope(pending, RECORD.scope(Mutex::new(captured), future)).await
}

fn check_material(receipt: &RecordAuthority, agent: &str, owner: Option<&str>, digest: &str)
    -> Result<(), VerificationError> {
    if receipt.agent != agent || Some(receipt.owner.as_str()) != owner || receipt.material != digest {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(())
}

fn check_identity(receipt: &RecordAuthority, epoch: Option<u64>, acl: u64, policy_epoch: u64, policy: &str)
    -> Result<(), VerificationError> {
    if epoch != Some(receipt.epoch) || acl != receipt.acl_generation
        || policy_epoch != receipt.policy_epoch || policy != receipt.policy {
        return Err(VerificationError::Revoked);
    }
    Ok(())
}

fn confirmed_employee(home: &Path, agent: &str) -> Result<EmployeeId, VerificationError> {
    let current = crate::handlers::resolve_confirmed_employee(home, agent).map_err(|_| unavailable())?;
    if current.disabled || current.label != agent { return Err(VerificationError::Revoked); }
    Ok(current.employee)
}

fn take_record() -> Result<RecordAuthority, VerificationError> {
    RECORD.try_with(|slot| slot.lock().ok().and_then(|mut s| s.take()))
        .ok().flatten().ok_or_else(unavailable)
}

/// Used only to recover the owner of a previously submitted heartbeat approval.
/// The returned label still requires the full record/approval/epoch recheck.
pub fn record_owner(home: &Path, kind: &str, id: &str) -> Option<String> {
    store::load(home, kind, id).ok().flatten().map(|r| r.owner)
}

pub(crate) fn recheck(receipt: &RecordAuthority) -> Result<(), VerificationError> {
    recheck_receipt(receipt, false, true)
}
fn recheck_receipt(receipt: &RecordAuthority, ancestor: bool, check_decision: bool) -> Result<(), VerificationError> {
    store::validate(receipt)?;
    if now()? >= receipt.expires_at { return Err(VerificationError::InvalidTime); }
    if confirmed_employee(&receipt.home, &receipt.agent)? != receipt.employee { return Err(VerificationError::Revoked); }
    if store::load(&receipt.home, &receipt.kind, &receipt.id)?.as_ref() != Some(receipt) {
        return Err(VerificationError::Revoked);
    }
    if !receipt.detached_bus {
        let material = if ancestor { material::read_ancestor(&receipt.home, &receipt.kind, &receipt.id) }
            else { material::read_current(&receipt.home, &receipt.kind, &receipt.id) }
            .map_err(|_| VerificationError::Revoked)?;
        if store::material_digest(&material)? != receipt.material { return Err(VerificationError::Revoked); }
    }
    let current = snapshot(&receipt.home, &receipt.owner, &receipt.agent)?;
    let policy = crate::approval::strict_policy_revision(&receipt.home, &receipt.agent).map_err(|_| unavailable())?;
    check_identity(receipt, current.human.as_ref().map(|h| h.credential_epoch),
        current.acl_generation, current.policy_epoch, &policy)?;
    if let Some(approval) = &receipt.approval { if check_decision { store::check_approval(receipt, approval)?; } }
    else if receipt.kind != "delegation" { return Err(VerificationError::Invalid); }
    if let Some(parent) = &receipt.parent { recheck_receipt(parent, true, true)?; }
    if let (Some(agent), Some(policy)) = (&receipt.parent_agent, &receipt.parent_policy) {
        if Some(confirmed_employee(&receipt.home, agent)?) != receipt.caller { return Err(VerificationError::Revoked); }
        let origin = snapshot(&receipt.home, &receipt.owner, agent)?;
        if origin.human.as_ref().map(|h| h.credential_epoch) != Some(receipt.epoch)
            || origin.acl_generation != receipt.acl_generation || origin.policy_epoch != receipt.policy_epoch
            || crate::approval::strict_policy_revision(&receipt.home, agent).ok().as_deref() != Some(policy) {
            return Err(VerificationError::Revoked);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) async fn authorize_current(agent_dir: &Path, principal_context: Option<String>)
    -> Result<crate::isolation_mcp::RuntimeAuthorization, VerificationError> {
    let receipt = take_record()?;
    if agent_dir != receipt.home.join("agents").join(&receipt.agent)
        || crate::on_behalf_of::current().as_deref() != Some(&receipt.owner) {
        return Err(VerificationError::BindingMismatch);
    }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten()
        .unwrap_or_else(|| format!("background:{}", uuid::Uuid::new_v4()));
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone).ok().flatten();
    let egress_turn = crate::runtime_turn::current();
    let metadata = crate::isolation_egress::RunMetadata {
        dispatch_run: crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten(),
        user_message: crate::memory_provenance::TURN_USER_MESSAGE.try_with(Clone::clone).ok().flatten(),
        reply_channel: crate::claude_runner::REPLY_CHANNEL.try_with(Clone::clone).ok(),
        reply_sender: crate::claude_runner::REPLY_SENDER.try_with(Clone::clone).ok(),
        upstream_unknown: crate::memory_provenance::UPSTREAM_UNKNOWN.try_with(|v| *v).unwrap_or(false),
        ..delegation_metadata(&receipt)?
    };
    let task = crate::runtime::round_task_env().map(|(_, task)| task)
        .or_else(|| (receipt.kind == "task").then(|| receipt.id.clone()));
    let lane = if crate::explore_lane::in_explore() || receipt.lane == Lane::Explore { Lane::Explore } else { Lane::Standard };
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        recheck(&receipt)?;
        let mut auth = crate::isolation_mcp::record_authorization(receipt.home.clone(), receipt.agent.clone(),
            user_id(&receipt.owner)?, receipt.epoch, lane,
            crate::isolation_mcp::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
                turn, approval_session: session }, metadata: Some(metadata), principal_context,
                egress_turn, background: Some(receipt.clone()) }, Some(receipt.expires_at))?;
        auth.grant.task = task.map(duduclaw_isolation::bounded::Text::new).transpose()
            .map_err(|_| VerificationError::Invalid)?;
        if let Some(task) = &auth.grant.task {
            let mut resources = auth.grant.resources.as_slice().to_vec();
            resources.push(duduclaw_isolation::capability::ResourceGrant::Task { id: task.clone() });
            auth.grant.resources = duduclaw_isolation::bounded::List::new(resources)
                .map_err(|_| VerificationError::Invalid)?;
        }
        // Queue consumers are detached host waiters. The initiating employee
        // and run remain in the receipt; treating them as a synchronous waiter
        // would deadlock/refuse a valid self-delegation after its parent exits.
        auth.origin = WaitOrigin::Gateway;
        auth.caller = None;
        Ok(auth)
    }).await
}

#[cfg(target_os = "linux")]
fn delegation_metadata(receipt: &RecordAuthority) -> Result<crate::isolation_egress::RunMetadata, VerificationError> {
    if receipt.kind != "delegation" { return Ok(crate::isolation_egress::RunMetadata::root()); }
    crate::claude_runner::DELEGATION_ENV.try_with(|env| {
        let number = |key| env.get(key).and_then(|v| v.parse::<u8>().ok()).ok_or(VerificationError::Invalid);
        Ok(crate::isolation_egress::RunMetadata {
            delegation_depth: number(duduclaw_core::ENV_DELEGATION_DEPTH)?,
            hop_depth: env.get(duduclaw_core::ENV_HOP_DEPTH).and_then(|v| v.parse().ok()).unwrap_or(0),
            delegation_origin: env.get(duduclaw_core::ENV_DELEGATION_ORIGIN).cloned(),
            ..crate::isolation_egress::RunMetadata::root()
        })
    }).map_err(|_| unavailable())?
}

pub(crate) fn has_scope() -> bool { RECORD.try_with(|_| ()).is_ok() }
pub fn approval_pending() -> bool { PENDING.try_with(|pending| *pending).unwrap_or(false) }

#[cfg(target_os = "linux")]
pub(crate) struct DelegationSource {
    pub owner: String, pub epoch: u64, pub acl_generation: u64, pub policy_epoch: u64,
    pub policy: String, pub agent: String, pub employee: EmployeeId, pub run: RunHandle,
    pub expires_at: u64, pub lane: Lane, pub parent: Option<RecordAuthority>,
}

/// Called by the original product send handler after its delegation/action
/// gates, before inserting the exact queue row. Authority is read back from D's
/// live run association, never from queue fields or a freshly read user epoch.
pub async fn record_delegation(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value)
    -> Result<(), VerificationError> {
    record_delegation_inner(home, sender, context, target, id, material, false).await
}

/// The bus consumer removes its input under a file lock. Its original material
/// is therefore sealed by the immutable host receipt at enqueue, and compared
/// again by scope_record at dequeue. Cancellation remains tied to the original
/// turn and parent approval; a drained pathname is never an authority source.
pub async fn record_bus_delegation(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value)
    -> Result<(), VerificationError> {
    record_delegation_inner(home, sender, context, target, id, material, true).await
}

async fn record_delegation_inner(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value, detached_bus: bool)
    -> Result<(), VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    #[cfg(target_os = "linux")]
    {
        let (home, sender, context, target, id, material) =
            (home.to_owned(), sender.to_owned(), context.clone(), target.to_owned(), id.to_owned(), material.clone());
        return duduclaw_mcp_boundary::authority::verify_blocking(move || {
            let source = crate::isolation_mcp::delegation_source(&home, &sender, &context)?;
            let current = snapshot(&home, &source.owner, &target)?;
            let employee = confirmed_employee(&home, &target)?;
            if current.human.as_ref().map(|h| h.credential_epoch) != Some(source.epoch)
                || current.acl_generation != source.acl_generation || current.policy_epoch != source.policy_epoch {
                return Err(VerificationError::Revoked);
            }
            let policy = crate::approval::strict_policy_revision(&home, &target).map_err(|_| unavailable())?;
            let value = RecordAuthority { home, kind: "delegation".into(), id, agent: target.clone(), employee,
                owner: source.owner, epoch: source.epoch, acl_generation: source.acl_generation,
                policy_epoch: source.policy_epoch,
                policy,
                material: store::material_digest(&material)?, approval: None, expires_at: source.expires_at,
                lane: source.lane, caller: Some(source.employee), parent_run: Some(source.run),
                parent_agent: Some(source.agent), parent_policy: Some(source.policy), parent: source.parent.map(Box::new), detached_bus };
            if store::load(&value.home, "delegation", &value.id)?.is_some() { return Err(VerificationError::Revoked); }
            store::save(&value)
        }).await;
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (home, sender, context, target, id, material, detached_bus); Err(unavailable()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn receipt(home: &Path) -> RecordAuthority {
        RecordAuthority { home: home.to_owned(), kind: "cron".into(), id: "original-record".into(),
            agent: "alice".into(), employee: EmployeeId([1;16]), owner: "user:original-user".into(), epoch: 7,
            acl_generation: 11, policy_epoch: 13, policy: "original-policy".into(),
            material: store::material_digest(&json!({"task":"original"})).unwrap(),
            approval: Some("approval-id".into()), expires_at: now().unwrap() + 3600,
            lane: Lane::Standard, caller: None, parent_run: None, parent_agent: None,
            parent_policy: None, parent: None, detached_bus: false }
    }

    #[test]
    fn background_authority_binds_owner_record_and_original_epochs() {
        let receipt = receipt(Path::new("/trusted-home"));
        assert!(check_material(&receipt, "alice", Some("user:original-user"), &receipt.material).is_ok());
        for (agent, owner, digest) in [("bob", Some("user:original-user"), receipt.material.as_str()),
            ("alice", None, receipt.material.as_str()), ("alice", Some("user:other"), receipt.material.as_str()),
            ("alice", Some("user:original-user"), "modified")] {
            assert!(check_material(&receipt, agent, owner, digest).is_err());
        }
        assert!(check_identity(&receipt, Some(7), 11, 13, "original-policy").is_ok());
        for (epoch, acl, policy_epoch, policy) in [(None,11,13,"original-policy"),
            (Some(8),11,13,"original-policy"), (Some(7),12,13,"original-policy"),
            (Some(7),11,14,"original-policy"), (Some(7),11,13,"replacement-policy")] {
            assert_eq!(check_identity(&receipt, epoch, acl, policy_epoch, policy), Err(VerificationError::Revoked));
        }
        for owner in ["", "dashboard", "unknown:owner", "unmapped:telegram:123"] { assert!(user_id(owner).is_err()); }
    }

    #[tokio::test]
    async fn background_authority_is_one_use_and_does_not_cross_spawn() {
        assert!(take_record().is_err());
        RECORD.scope(Mutex::new(Some(receipt(Path::new("/trusted-home")))), async {
            assert!(tokio::spawn(async { take_record().is_err() }).await.unwrap());
            assert_eq!(take_record().unwrap().epoch, 7);
            assert!(take_record().is_err());
        }).await;
        assert!(take_record().is_err());
    }

    #[tokio::test]
    async fn background_pending_signal_is_scoped_and_not_a_grant() {
        assert!(!approval_pending());
        PENDING.scope(true, RECORD.scope(Mutex::new(None), async {
            assert!(approval_pending());
            assert!(take_record().is_err());
            assert!(!tokio::spawn(async { approval_pending() }).await.unwrap());
        })).await;
        assert!(!approval_pending());
    }
}
