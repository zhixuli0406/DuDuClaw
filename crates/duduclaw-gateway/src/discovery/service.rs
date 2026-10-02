//! Public discovery entry points. Authority is constructed from authenticated
//! transport context and never deserialized from a task or model request.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use duduclaw_auth::{UserContext, models::{AccessLevel, UserRole}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::task_store::{TaskKind, TaskRow, TaskStore};
use super::{config::{DiscoveryConfig, AttemptSandbox, EvaluatorSandbox}, contracts::RunBudget,
    night::{DefaultsNamespace, VersionedDefaults}, online::RunSpec, store::DiscoveryStore,
    tree::Direction, workspace};

#[path = "service_lifecycle.rs"]
mod lifecycle;
#[path = "service_queries.rs"]
mod queries;
pub use lifecycle::poll;
pub use queries::{catalog, list, tree, artifact, cancel};

// Exact `blocked_reason` values written on cancellation; public views map them
// with string equality to `cancel_code` / `approval_status`.
const CANCEL_APPROVAL_DENIED: &str = "approval denied";
const CANCEL_APPROVAL_EXPIRED: &str = "approval expired";
/// Written before denial and expiry were distinguished; read as a denial.
const CANCEL_APPROVAL_LEGACY: &str = "approval denied or expired";
const CANCEL_WITHDRAWN: &str = "approval withdrawn by authorized caller";
const CANCEL_BY_CALLER: &str = "cancelled by authorized caller";
/// System decider for a withdrawn request. A DENY with no decision receipt:
/// `apply_receipted_decision` can never turn it into authority.
const WITHDRAWN_DECIDER: &str = "system:discovery-withdrawn";

/// Resolve a still-pending approval whose discovery task is already terminal,
/// so it leaves the manager inbox. The task CAS stays the authority: a broker
/// failure is logged and never reverts the cancellation.
/// The record must also be bound to `task_id`; a mismatch only logs.
async fn withdraw_approval(broker: &ApprovalBroker, approval_id: &str, task_id: &str) {
    let id = ApprovalId::from(approval_id.to_owned());
    match broker.get(&id).await {
        Ok(Some(record)) if record.payload.get("task_id").and_then(Value::as_str) != Some(task_id) => {
            tracing::warn!(approval_id, task_id, "discovery approval withdrawal refused: task binding mismatch");
        }
        Ok(Some(record)) if record.status == ApprovalStatus::Pending && record.action_kind == "discovery" => {
            if let Err(error) = broker.decide(&id, false, WITHDRAWN_DECIDER).await {
                tracing::warn!(approval_id, error, "discovery approval withdrawal failed");
            }
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(approval_id, error, "discovery approval withdrawal lookup failed"),
    }
}

#[derive(Debug, Clone)]
pub struct TrustedCaller { principal: Principal }
#[derive(Debug, Clone)]
enum Principal { User(UserContext), Agent(String) }
impl TrustedCaller {
    pub fn from_user(context: &UserContext) -> Result<Self, String> {
        if context.user_id.trim().is_empty() || context.requires_password_change() {
            return Err("permission denied".into());
        }
        Ok(Self { principal: Principal::User(context.clone()) })
    }
    pub fn from_signed_agent(home: &Path, id: &str, token: Option<&str>) -> Result<Self, String> {
        valid_id(id)?;
        if !matches!(duduclaw_core::verify_identity_claim(home, id, token.unwrap_or(""), true),
            duduclaw_core::IdentityVerdict::Verified) {
            return Err("discovery requires a verified agent identity".into());
        }
        Ok(Self { principal: Principal::Agent(id.into()) })
    }
    fn id(&self) -> String {
        match &self.principal { Principal::User(user) => format!("user:{}", user.user_id),
            Principal::Agent(agent) => format!("agent:{agent}") }
    }
    fn origin(&self) -> &'static str {
        match &self.principal { Principal::User(user) if user.has_role(UserRole::Manager) => "manager",
            Principal::User(_) => "rpc", Principal::Agent(_) => "mcp" }
    }
    fn manager_for(&self, agent: &str) -> bool {
        matches!(&self.principal, Principal::User(user) if user.has_role(UserRole::Manager)
            && user.has_agent_access(agent, AccessLevel::Operator))
    }
    fn authorize_create(&self, home: &Path, agent: &str) -> Result<(), String> {
        valid_id(agent)?;
        if !home.join("agents").join(agent).join("agent.toml").is_file() {
            return Err("unknown assigned agent".into());
        }
        match &self.principal {
            Principal::User(user) if user.has_agent_access(agent, AccessLevel::Operator) => Ok(()),
            Principal::User(_) => Err("permission denied".into()),
            Principal::Agent(id) if id == agent => Ok(()),
            Principal::Agent(id) => {
                let org = crate::delegation_gate::DispatchOrgView::new(home, None);
                duduclaw_core::delegation_policy::can_delegate_rules(
                    &duduclaw_core::delegation_policy::delegation_rules_from_home(home),
                    &org, id, agent).map_err(|_| "delegation denied".into())
            }
        }
    }
    fn authorize_task(&self, home: &Path, task: &TaskRow) -> Result<(), String> {
        if task.kind != TaskKind::Discovery { return Err("discovery not found".into()); }
        let visible = self.manager_for(&task.assigned_to) || match &self.principal {
            Principal::User(user) => task.created_by == self.id()
                && user.has_agent_access(&task.assigned_to, AccessLevel::Operator),
            Principal::Agent(_) => task.created_by == self.id()
                && self.authorize_create(home, &task.assigned_to).is_ok(),
        };
        if visible { Ok(()) } else { Err("permission denied".into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicDiscoverySpec {
    pub approved_root_id: String,
    pub evaluator: String,
    pub runtime: String,
    pub model: String,
    pub branch_count: u32,
    pub refine_count: u32,
    pub max_parallelism: u32,
    pub budget: RunBudget,
    #[serde(default = "default_direction")]
    pub direction: Direction,
}
fn default_direction() -> Direction { Direction::Max }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenCreation {
    spec: PublicDiscoverySpec,
    scorer_hash: String,
    creator_origin: String,
    defaults: VersionedDefaults,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PublicFrozenRequest {
    spec: Value, scorer_hash: String, creator_origin: String,
    defaults_version: u64, policy_id: String, source_sha256: Option<String>,
    beta: f64, origin_task_ids: Vec<String>, frozen_sha256: String,
}
fn public_request(frozen: &FrozenCreation, digest: &str) -> PublicFrozenRequest {
    PublicFrozenRequest {spec:json!(frozen.spec),scorer_hash:frozen.scorer_hash.clone(),
        creator_origin:frozen.creator_origin.clone(),defaults_version:frozen.defaults.version,
        policy_id:frozen.defaults.bundle.policy_id.clone(),source_sha256:frozen.defaults.bundle.source_sha256.clone(),
        beta:frozen.defaults.bundle.beta,origin_task_ids:frozen.defaults.bundle.origin_task_ids.clone(),frozen_sha256:digest.into()}
}
fn public_frozen(task: &TaskRow) -> Result<PublicFrozenRequest, String> {
    serde_json::from_str(task.discovery_spec_json.as_deref().ok_or("missing public discovery request")?)
        .map_err(|_| "invalid public discovery request".into())
}
fn frozen_request(home: &Path, task: &TaskRow) -> Result<FrozenCreation, String> {
    let public=public_frozen(task)?;
    let run=task.discovery_run_id.as_deref().ok_or("missing run identity")?;
    let raw=DiscoveryStore::open(home).map_err(|_| "private discovery request unavailable")?
        .load_frozen_request(&task.id,run,&public.frozen_sha256).map_err(|_| "private discovery request binding failed")?;
    let frozen:FrozenCreation=serde_json::from_str(&raw).map_err(|_| "invalid private discovery request")?;
    frozen.defaults.bundle.validate()?;
    if public_request(&frozen,&public.frozen_sha256) != public { return Err("public/private discovery request mismatch".into()); }
    Ok(frozen)
}
fn approval_payload(task: &TaskRow) -> Result<Value,String> {
    Ok(json!({"task_id":task.id,"run_id":task.discovery_run_id,"creator_id":task.created_by,
        "request":public_frozen(task)?}))
}
fn payload_digest(payload: &Value) -> String { format!("{:x}",Sha256::digest(payload.to_string().as_bytes())) }
#[derive(Debug, Clone, Serialize)]
pub struct CreatedDiscovery {
    pub task_id: String, pub run_id: String, pub status: String, pub approval_id: Option<String>,
}

pub fn load_config(home: &Path) -> Result<DiscoveryConfig, String> {
    let content = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|_| "discovery is not configured".to_string())?;
    let table: toml::Value = toml::from_str(&content).map_err(|_| "invalid discovery configuration")?;
    table.get("discovery").cloned().ok_or("discovery is not configured")?
        .try_into().map_err(|_| "invalid discovery configuration".into())
}
fn valid_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        Err("invalid discovery identifier".into())
    } else { Ok(()) }
}
fn root_id(path: &Path) -> String {
    format!("root-{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()))
}
fn approved_root(config: &DiscoveryConfig, id: &str) -> Result<PathBuf, String> {
    for root in &config.approved_workspace_roots {
        let path = workspace::canonical_real_directory(root).map_err(|_| "approved root unavailable")?;
        if root_id(&path) == id { return Ok(path); }
    }
    Err("unknown approved root".into())
}
fn validate_spec(home: &Path, config: &DiscoveryConfig, agent: &str,
    goal: &str, spec: &PublicDiscoverySpec, beta: f64) -> Result<(RunSpec, String), String> {
    if config.attempt.sandbox != AttemptSandbox::Container || config.account_pool.is_empty() {
        return Err("public discovery requires a confined dedicated account pool".into());
    }
    valid_id(&spec.evaluator)?;
    super::agent_spawn::AttemptRunnerFactory::check_runtime_capability(&config.attempt,&spec.runtime)
        .map_err(|_| "runtime does not support the discovery confinement and hard generation limits")?;
    let evaluator = config.evaluators.get(&spec.evaluator).ok_or("unknown evaluator")?;
    if evaluator.sandbox != EvaluatorSandbox::Container {
        return Err("public discovery requires a confined evaluator".into());
    }
    let scorer_root = workspace::canonical_real_directory(&home.join("discovery/evaluators").join(&spec.evaluator))
        .map_err(|_| "registered evaluator unavailable")?;
    super::evaluator::validate_argv(&scorer_root, &evaluator.command)
        .map_err(|_| "invalid registered evaluator")?;
    let hash = workspace::directory_sha256(&scorer_root).map_err(|_| "registered evaluator unavailable")?;
    if hash != evaluator.sha256 { return Err("registered evaluator integrity changed".into()); }
    let run = RunSpec { goal: goal.into(), agent_id: agent.into(), runtime: spec.runtime.clone(),
        model: spec.model.clone(), evaluator: spec.evaluator.clone(),
        starting_workspace: approved_root(config, &spec.approved_root_id)?, direction: spec.direction,
        budget: spec.budget, branch_count: spec.branch_count, refine_count: spec.refine_count,
        max_parallelism: spec.max_parallelism, attempt_timeout_secs: 300, max_turns: 20,
        beta, dream_versions: 3, policy_model: None };
    run.validate()?;
    Ok((run, hash))
}

/// Shared by authenticated RPC, signed MCP, and other trusted transports.
pub async fn create(home: &Path, store: &TaskStore, broker: &ApprovalBroker,
    caller: &TrustedCaller, agent: &str, title: &str, description: &str,
    spec: PublicDiscoverySpec) -> Result<CreatedDiscovery, String> {
    caller.authorize_create(home, agent).inspect_err(|_| {
        crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
            "discovery_create_denied", &caller.id(), duduclaw_security::audit::Severity::Warning,
            json!({"assigned_to":agent,"origin":caller.origin()})));
    })?;
    if title.trim().is_empty() || title.chars().count() > 200 || description.trim().is_empty()
        || description.len() > 16_000 { return Err("invalid discovery description".into()); }
    if duduclaw_security::input_guard::scan_input(description, 70).blocked {
        return Err("discovery description failed the input security scan".into());
    }
    let config = load_config(home)?;
    // Store the canonical family name so an alias (`agy`) never reaches the
    // run row, nodes or catalog as a second spelling.
    let mut spec = spec;
    if let Ok(family) = super::attempt_adapter::RuntimeFamily::parse(&spec.runtime) { spec.runtime = family.name().into(); }
    let (_, scorer_hash) = validate_spec(home, &config, agent, description, &spec, 0.6)?;
    let namespace = DefaultsNamespace { agent_id: agent.into(), scorer_name: spec.evaluator.clone(),
        scorer_hash: scorer_hash.clone(), direction: spec.direction,
        approved_root_id: spec.approved_root_id.clone(), runtime: spec.runtime.clone(),
        configured_model: spec.model.clone() };
    let defaults = DiscoveryStore::open(home).map_err(|e| e.to_string())?
        .load_discovery_default(&namespace).map_err(|e| e.to_string())?;
    let frozen = FrozenCreation { spec, scorer_hash, creator_origin: caller.origin().into(), defaults };
    let mut task = TaskRow::new(uuid::Uuid::new_v4().to_string(), title.into(), description.into(),
        "medium".into(), agent.into(), caller.id());
    task.kind = TaskKind::Discovery;
    task.discovery_run_id = Some(uuid::Uuid::new_v4().to_string());
    let raw=serde_json::to_string(&frozen).map_err(|e|e.to_string())?;
    let digest=DiscoveryStore::open(home).map_err(|e|e.to_string())?
        .save_frozen_request(&task.id,task.discovery_run_id.as_deref().unwrap(),&raw).map_err(|e|e.to_string())?;
    task.discovery_spec_json = Some(serde_json::to_string(&public_request(&frozen,&digest)).map_err(|e|e.to_string())?);
    task.status = if caller.manager_for(agent) { "queued" } else { "pending_approval" }.into();
    if task.status == "pending_approval" {
        let id = broker.request(agent, "discovery", title, approval_payload(&task)?, 86_400).await?;
        task.discovery_approval_id = Some(id.as_str().into());
    }
    store.insert_task(&task).await?;
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        "discovery_task_created", &task.assigned_to, duduclaw_security::audit::Severity::Info,
        json!({"task_id":task.id,"run_id":task.discovery_run_id,"creator_id":task.created_by,
            "creator_origin":caller.origin(),"approval_id":task.discovery_approval_id,"status":task.status})));
    Ok(CreatedDiscovery { task_id: task.id, run_id: task.discovery_run_id.unwrap(),
        status: task.status, approval_id: task.discovery_approval_id })
}

/// Bind the public approval to its private source and the exact task/run IDs.
fn validate_record_request(store: &TaskStore, task: &TaskRow,
    record: &crate::approval::ApprovalRecord, id: &ApprovalId) -> Result<(),String> {
    if task.kind != TaskKind::Discovery || record.action_kind != "discovery"
        || task.discovery_approval_id.as_deref() != Some(id.as_str())
        || record.agent_id != task.assigned_to || record.payload != approval_payload(task)? {
        return Err("approval does not match the frozen discovery request".into());
    }
    frozen_request(store.discovery_home()?,task)?;
    Ok(())
}
async fn approval_task(store: &TaskStore, record: &crate::approval::ApprovalRecord,
    id: &ApprovalId) -> Result<TaskRow,String> {
    let task_id=record.payload.get("task_id").and_then(Value::as_str).ok_or("invalid approval")?;
    let task=store.get_task(task_id).await?.ok_or("discovery not found")?;
    validate_record_request(store,&task,record,id)?;
    Ok(task)
}
/// Read-only preflight retained for trusted transports that only inspect.
pub async fn validate_approval_decision(store: &TaskStore, broker: &ApprovalBroker,
    caller: &TrustedCaller, id: &ApprovalId) -> Result<(), String> {
    let record=broker.get(id).await?.ok_or("approval not found")?;
    let task=approval_task(store,&record,id).await?;
    if !caller.manager_for(&task.assigned_to) { return Err("permission denied".into()); }
    if record.status != ApprovalStatus::Pending || record.expires_at_epoch()
        .is_none_or(|deadline|chrono::Utc::now().timestamp() >= deadline) {
        return Err("discovery approval is expired or already decided".into());
    }
    if task.status != "pending_approval" { return Err("discovery is no longer awaiting this approval".into()); }
    Ok(())
}
/// A host-created manager intent is persisted BEFORE the broker commit. A
/// terminal broker row, including a self-filled decided_by, is never authority.
pub async fn prepare_approval_decision(store: &TaskStore, broker: &ApprovalBroker,
    caller: &TrustedCaller, id: &ApprovalId, approve: bool) -> Result<(),String> {
    validate_approval_decision(store,broker,caller,id).await?;
    let record=broker.get(id).await?.ok_or("approval not found")?;
    let task=approval_task(store,&record,id).await?;
    let decider=match &caller.principal { Principal::User(user)=>format!("dashboard:{}",user.user_id),
        Principal::Agent(_)=>return Err("permission denied".into()) };
    store.save_discovery_decision_receipt(&crate::task_store::DiscoveryDecisionReceipt {
        approval_id:id.as_str().into(),task_id:task.id.clone(),decider,approve,
        payload_sha256:payload_digest(&record.payload),frozen_sha256:public_frozen(&task)?.frozen_sha256,
    }).await
}
async fn apply_receipted_decision(store: &TaskStore, record: &crate::approval::ApprovalRecord,
    id: &ApprovalId) -> Result<bool,String> {
    let task=approval_task(store,record,id).await?;
    let receipt=store.discovery_decision_receipt(id.as_str()).await?.ok_or("discovery requires a trusted manager decision receipt")?;
    if receipt.task_id != task.id || record.decided_by.as_deref() != Some(receipt.decider.as_str())
        || receipt.payload_sha256 != payload_digest(&record.payload)
        || receipt.frozen_sha256 != public_frozen(&task)?.frozen_sha256 {
        return Err("discovery decision receipt does not match the broker and private request".into());
    }
    if record.status == ApprovalStatus::Approved {
        if !receipt.approve { return Err("discovery decision differs from the human intent".into()); }
        let decided=record.decided_at.as_deref().and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value|value.timestamp()).ok_or("invalid discovery decision time")?;
        if record.expires_at_epoch().is_none_or(|deadline|decided >= deadline) {
            return Err("discovery decision exceeded the approval deadline".into());
        }
        return store.authorize_discovery(&task.id,id.as_str()).await;
    }
    if record.status == ApprovalStatus::Denied && !receipt.approve {
        return store.cancel_discovery(&task.id,CANCEL_APPROVAL_DENIED).await;
    }
    Err("discovery approval does not match the terminal human decision".into())
}
/// Normal transport and restart reconciliation use the same receipt gate.
pub async fn authorize_approved_request(store: &TaskStore, broker: &ApprovalBroker,
    caller: &TrustedCaller, id: &ApprovalId) -> Result<(),String> {
    let record=broker.get(id).await?.ok_or("approval not found")?;
    let task=approval_task(store,&record,id).await?;
    if !caller.manager_for(&task.assigned_to) { return Err("permission denied".into()); }
    let decider=match &caller.principal {Principal::User(user)=>format!("dashboard:{}",user.user_id),
        Principal::Agent(_)=>return Err("permission denied".into())};
    if record.decided_by.as_deref() != Some(decider.as_str()) {return Err("discovery requires the authenticated manager decision".into());}
    if !apply_receipted_decision(store,&record,id).await? {return Err("discovery is no longer awaiting approval".into());}
    Ok(())
}

#[cfg(test)]
#[path = "tests_service.rs"]
mod tests;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnvelope {
    kind: String, assigned_to: String, title: String, description: String,
    discovery: PublicDiscoverySpec,
}
pub async fn create_from_value(home: &Path, store: &TaskStore, broker: &ApprovalBroker,
    caller: &TrustedCaller, value: Value) -> Result<CreatedDiscovery, String> {
    let request: CreateEnvelope = serde_json::from_value(value).map_err(|_| "invalid discovery create request")?;
    if !request.kind.trim().eq_ignore_ascii_case("discovery") { return Err("wrong task kind".into()); }
    create(home, store, broker, caller, &request.assigned_to, &request.title,
        &request.description, request.discovery).await
}
