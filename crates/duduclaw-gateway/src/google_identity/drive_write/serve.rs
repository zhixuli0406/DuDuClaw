//! The single gateway execution point for RM-131 writes. Approval never grants
//! a bearer to the caller, and no refusal has a CLI or identity fallback.
use std::{collections::HashMap, future::Future, io::Read, path::Path, pin::Pin,
    sync::{Mutex, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};
use duduclaw_core::{authority_coordinator::AuthorityCoordinator, google_identity as gi};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use crate::google_identity::{execute::ExecuteState, Refusal};
use crate::google_workspace::drive_write_policy::{self as policy, ApprovalMode, DriveWriteAction};
use super::{args::{self, ArgError, CreateContent, DriveArgs, ToolArgs}, audit,
    credential::{self, IdentityRoute, Readiness}, program, program_docs, scope,
    transport::{GoogleId, WriteCall, WriteResult, WriteTransport}};

/// P10 supplies the request-bound member/Admin implementation. This interface
/// is gateway-only; tool arguments and host approval tokens cannot implement it.
pub(in crate::google_identity) trait DriveWriteConfirmer: Send + Sync {
    fn confirm<'a>(&'a self, request: ConfirmRequest)
        -> Pin<Box<dyn Future<Output = ConfirmOutcome> + Send + 'a>>;
}
pub(in crate::google_identity) struct ConfirmRequest {
    pub arguments: Value,
    pub version: String,
    pub agent: String,
    pub tool: String,
    pub action: DriveWriteAction,
    pub approval_mode: ApprovalMode,
    pub requester: RequesterIdentity,
    pub route_kind: String,
    pub account_email: String,
    pub target_file_id: Option<String>,
    pub target_file_name: Option<String>,
    pub destination: Option<String>,
    pub destination_name: Option<String>,
    pub size: Option<u64>,
    pub content_preview: Option<String>,
    pub attachment_name: Option<String>,
    pub rename_before: Option<String>,
    pub rename_after: Option<String>,
    pub snapshot_digest: String,
    pub scope_digest: String,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub call_id: String,
    pub parents_before: Vec<String>,
    pub source_names: Vec<String>,
}
pub(in crate::google_identity) enum ConfirmOutcome {
    Approved { approval_ids: Vec<String>, decided_by: Vec<String> },
    Denied,
    Expired,
    Unavailable,
}
pub(in crate::google_identity) struct UnavailableConfirmer;
impl DriveWriteConfirmer for UnavailableConfirmer {
    fn confirm<'a>(&'a self, _: ConfirmRequest)
        -> Pin<Box<dyn Future<Output = ConfirmOutcome> + Send + 'a>> {
        Box::pin(async { ConfirmOutcome::Unavailable })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct Snapshot {
    agent: String, tool: String, args_digest: String,
    turn_id: Option<String>, run_id: Option<String>, call_id: String,
    requester: String, route_kind: String, route_identity: String,
    account_email: String, credential_revision: Option<u64>, credential_digest: String,
    policy_digest: String, approval_mode: ApprovalMode, config_digest: String,
}
#[derive(Clone)]
struct Binding { snapshot: Snapshot, nonce: String }
impl Binding {
    fn digest(&self) -> String { gi::google_arguments_digest(&json!([self.snapshot, self.nonce])) }
}
struct Pending { binding: Binding, created: Instant }
#[derive(Default)]
pub(in crate::google_identity) struct Store(Mutex<HashMap<(String, String), Pending>>);
impl Store {
    fn take(&self, agent: &str, call: &str) -> Result<Binding, Refusal> {
        let mut entries = self.0.lock().map_err(|_| changed())?;
        entries.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
        entries.remove(&(agent.into(), call.into())).map(|p| p.binding).ok_or_else(changed)
    }
    fn insert(&self, binding: Binding) -> Result<(), Refusal> {
        let mut entries = self.0.lock().map_err(|_| changed())?;
        entries.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
        let key = (binding.snapshot.agent.clone(), binding.snapshot.call_id.clone());
        if entries.len() >= 1024 || entries.contains_key(&key) { return Err(changed()); }
        entries.insert(key, Pending { binding, created: Instant::now() });
        Ok(())
    }
}
fn changed() -> Refusal {
    Refusal::hard("changed", "Google 帳號、設定或檔案位置已變更，沒有執行，請重新確認。")
}
fn arg_error(error: ArgError) -> Refusal { Refusal::hard(error.code(), error.user_text()) }
fn unconfirmed() -> Refusal {
    Refusal::hard("result_unconfirmed", "結果無法確認，請先到 Google 檢查，沒有重送。")
}
fn not_sent() -> Refusal { Refusal::hard("not_sent", "這次操作沒有送出。") }
fn mode_text(mode: ApprovalMode) -> &'static str {
    match mode { ApprovalMode::None => "none", ApprovalMode::Member => "member", ApprovalMode::MemberAndAdmin => "member_and_admin" }
}
fn plan(args: &ToolArgs) -> scope::Plan {
    match args { ToolArgs::Drive(args) => args::plan(args), ToolArgs::DocsSheets(args) => program_docs::plan(args) }
}
fn target(plan: &scope::Plan) -> Option<&GoogleId> {
    match plan {
        scope::Plan::Create { parent } => parent.as_ref(),
        scope::Plan::Copy { source, .. } => Some(source),
        scope::Plan::Modify { target, .. } | scope::Plan::Move { target, .. } | scope::Plan::Trash { target } => Some(target),
    }
}
fn destination(plan: &scope::Plan) -> Option<&GoogleId> {
    match plan {
        scope::Plan::Create { parent } => parent.as_ref(),
        scope::Plan::Copy { parent, .. } => Some(parent),
        scope::Plan::Move { destination, .. } => Some(destination),
        _ => None,
    }
}
fn require_scope(verdict: &scope::ScopeVerdict) -> Result<(), Refusal> {
    match verdict {
        scope::ScopeVerdict::InScope { .. } => Ok(()),
        scope::ScopeVerdict::OutOfScope { reason } => Err(Refusal::hard(reason.code(), reason.user_text())),
        scope::ScopeVerdict::LimitExceeded => Err(Refusal::hard("folder_check_limit", "無法在這次操作中確認完整的資料夾範圍，沒有執行。")),
        scope::ScopeVerdict::Failed => Err(Refusal::hard("scope_unconfirmed", "無法確認檔案所在的資料夾，沒有執行。")),
    }
}

fn policy_check(policy: &policy::DriveWritePolicy, action: DriveWriteAction) -> Result<(), Refusal> {
    let (enabled, verb) = match action {
        DriveWriteAction::Create => (policy.create, "建立檔案"),
        DriveWriteAction::Modify => (policy.modify, "修改檔案"),
        DriveWriteAction::Move => (policy.move_, "搬移檔案"),
        DriveWriteAction::Trash => (policy.trash, "將檔案移到垃圾桶"),
    };
    if !enabled { return Err(Refusal::hard("action_off", format!("管理員還沒開放 AI 在雲端硬碟{verb}。"))); }
    if matches!(action, DriveWriteAction::Move | DriveWriteAction::Trash) && policy.folders.is_empty() {
        return Err(Refusal::hard("folder_required", "需要先限定資料夾"));
    }
    Ok(())
}
fn readiness_check(home: &Path, route: &IdentityRoute) -> Result<(), Refusal> {
    match credential::readiness(home, route) {
        Readiness::Ready => Ok(()),
        // RM-147's closed purpose table does not support drive_write yet.
        Readiness::NeedsMemberConsent { .. } => Err(Refusal::hard("drive_write_needs_member_consent", "請到「我的連線」連結雲端硬碟寫入")),
        Readiness::NeedsHomeConsent => Err(Refusal::hard("drive_write_needs_home_consent", "公司 Google 帳號需要重新連結雲端硬碟寫入，請聯絡管理員。")),
        Readiness::NeedsAdminScope => Err(crate::google_identity::resolve::drive_write_admin_scope_missing()),
        Readiness::Unsupported => Err(Refusal::hard("drive_write_unsupported_identity", "這種 Google 連線方式不支援雲端硬碟寫入。")),
    }
}

impl Snapshot {
    fn capture(home: &Path, agent: &str, tool: &str, args: &Value, who: &RequesterIdentity,
        turn: &Option<String>, run: &Option<String>, call: &str,
    ) -> Result<(Self, IdentityRoute, policy::DriveWritePolicy), Refusal> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| changed())?;
        let action = policy::action_for_tool(tool).ok_or_else(changed)?;
        let policy = policy::load(home);
        policy_check(&policy, action)?;
        let route = credential::resolve_route(home, agent)?;
        readiness_check(home, &route)?;
        let mode = policy.approval_for(action);
        credential::requester_check(&route, who, mode != ApprovalMode::None)?;
        if turn.is_none() && run.is_none() { return Err(changed()); }
        for id in [turn.as_deref(), run.as_deref()].into_iter().flatten() {
            if crate::knowledge::turn_requesters::lookup(agent, id).as_ref() != Some(who) { return Err(changed()); }
        }
        let (kind, account, record) = match &route {
            IdentityRoute::Personal { owner_user_id, account_email } => ("personal", account_email.clone(),
                Some(crate::person_credentials::write_store::get(home, "user", owner_user_id)
                    .map_err(|_| changed())?.ok_or_else(changed)?.metadata)),
            IdentityRoute::HomeOAuth => {
                let record = crate::person_credentials::write_store::get(home, "home", "home")
                    .map_err(|_| changed())?.ok_or_else(changed)?.metadata;
                ("home_oauth", record.account_email.clone(), Some(record))
            }
            IdentityRoute::Delegate { email } => ("delegate", email.clone(), None),
            IdentityRoute::DelegateOwner { email, .. } => ("delegate_owner", email.clone(), None),
            IdentityRoute::HomeServiceAccount => ("home_sa", crate::google_identity::home_subject(home).ok_or_else(changed)?, None),
            IdentityRoute::Unsupported => return Err(changed()),
        };
        let config = std::fs::read(home.join("config.toml")).map_err(|_| changed())?;
        let employee = duduclaw_core::agent_toml::read_effective_text(&home.join("agents").join(agent)).map_err(|_| changed())?;
        let material_digest = match &route {
            IdentityRoute::Delegate { .. } | IdentityRoute::DelegateOwner { .. } => {
                let bytes = std::fs::read(crate::google_identity::delegation_key::path(home)).map_err(|_| changed())?;
                hex::encode(Sha256::digest(&bytes))
            }
            IdentityRoute::HomeServiceAccount => {
                let text = std::str::from_utf8(&config).map_err(|_| changed())?;
                let cfg = crate::google_service_account::parse_config(text, home).map_err(|_| changed())?.ok_or_else(changed)?;
                let bytes = zeroize::Zeroizing::new(std::fs::read(cfg.key_file).map_err(|_| changed())?);
                hex::encode(Sha256::digest(bytes.as_slice()))
            }
            _ => gi::google_arguments_digest(&json!(record)),
        };
        let epoch = AuthorityCoordinator::new(home).epoch().map_err(|_| changed())?;
        let snapshot = Self {
            agent: agent.into(), tool: tool.into(), args_digest: gi::google_arguments_digest(args),
            turn_id: turn.clone(), run_id: run.clone(), call_id: call.into(), requester: format!("{who:?}"),
            route_kind: kind.into(), route_identity: format!("{route:?}"), account_email: account,
            credential_revision: record.as_ref().map(|m| m.revision),
            credential_digest: material_digest,
            policy_digest: policy::digest_of(&policy), approval_mode: mode,
            config_digest: gi::google_arguments_digest(&json!([config, employee, epoch])),
        };
        Ok((snapshot, route, policy))
    }
    fn validate(&self, home: &Path, args: &Value, who: &RequesterIdentity) -> Result<(), Refusal> {
        let (now, _, _) = Self::capture(home, &self.agent, &self.tool, args, who,
            &self.turn_id, &self.run_id, &self.call_id).map_err(|_| changed())?;
        if now == *self { Ok(()) } else { Err(changed()) }
    }
}

/// Recompute the executor's local identity/credential/policy snapshot at decision time.
pub(super) fn confirmation_version(home: &Path, agent: &str, tool: &str, args: &Value,
    who: &RequesterIdentity, turn: &Option<String>, run: &Option<String>, call: &str,
) -> Result<String, Refusal> {
    let (snapshot, _, _) = Snapshot::capture(home, agent, tool, args, who, turn, run, call)?;
    Ok(gi::google_arguments_digest(&json!(snapshot)))
}

pub(super) fn confirmation_identity_matches(home: &Path, rec: &crate::approval::ApprovalRecord,
    who: &RequesterIdentity, turn: &Option<String>, run: &Option<String>, call: &str,
) -> bool {
    let Some(tool) = rec.payload["name"].as_str() else { return false; };
    let Ok((snapshot, _, _)) = Snapshot::capture(home, &rec.agent_id, tool,
        &rec.payload["arguments"], who, turn, run, call) else { return false; };
    let displayed = &rec.payload["drive_snapshot"];
    displayed["account"] == snapshot.account_email && displayed["route_kind"] == snapshot.route_kind
        && snapshot.approval_mode != ApprovalMode::None
}

/// Cancellation (including the outer timeout) still records the last known
/// write boundary. Never infer a successful write from cancellation.
struct AuditGuard<'a> {
    home: &'a Path, progress: &'a AtomicUsize, record: audit::DriveWriteAudit, finished: bool,
}
impl Drop for AuditGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let sent = self.progress.load(Ordering::SeqCst) != 0;
            self.record.outcome = if sent { audit::Outcome::MayHaveSent } else { audit::Outcome::NotSent };
            self.record.reason = audit::Reason::from_code(if sent { "result_unconfirmed" } else { "not_sent" });
        }
        audit::append(self.home, &self.record);
    }
}

pub(in crate::google_identity) async fn serve(state: &ExecuteState, agent: &str, tool: &str, args: &Value,
    turn: Option<String>, run: Option<String>, call: String,
    approved: Option<String>, progress: &AtomicUsize,
) -> Result<Value, Refusal> {
    let mut audit = AuditGuard { home: &state.home, progress, finished: false, record: audit::DriveWriteAudit {
        // Missing IDs remain an empty string in the refusal audit, never null
        // or an invented ID; requester() below rejects such requests.
        call_id: call.clone(), google_turn_id: turn.as_ref().or(run.as_ref()).cloned().unwrap_or_default(),
        id_source: if turn.is_some() { "turn" } else { "run" }, run_id: run.clone(), agent_id: agent.into(),
        identity_kind: String::new(), account_email: String::new(), requester: format!("{:?}", RequesterIdentity::Unknown),
        action: policy::action_for_tool(tool).map(|a| a.as_str()).unwrap_or("unknown").into(), tool: tool.into(),
        file_id: None, file_name: None, parents_before: vec![], parents_after: vec![], destination_folder_id: None,
        folder_rule: None, approval: audit::Approval { mode: String::new(), approval_ids: vec![], decided_by: vec![] },
        bytes: None, content_sha256: None, outcome: audit::Outcome::NotSent, reason: None, post_check: audit::PostCheck::None,
    } };
    let result = serve_inner(state, agent, tool, args, turn, run, call, approved, progress, &mut audit.record).await;
    if let Err(error) = &result {
        audit.record.reason = audit::Reason::from_code(error.code).or_else(|| audit::Reason::from_code("failed"));
        audit.record.outcome = if progress.load(Ordering::SeqCst) != 0 { audit::Outcome::MayHaveSent }
            else if error.code == "not_sent" { audit::Outcome::NotSent } else { audit::Outcome::Refused };
    }
    audit.finished = true;
    result
}

async fn serve_inner(state: &ExecuteState, agent: &str, tool: &str, raw: &Value,
    turn: Option<String>, run: Option<String>, call: String, approved: Option<String>,
    progress: &AtomicUsize, record: &mut audit::DriveWriteAudit,
) -> Result<Value, Refusal> {
    // Consume before recomputing anything: a failed execute cannot reuse a grant.
    let pending = if approved.is_some() { Some(state.drive_writes.take(agent, &call)) } else { None };
    if gi::native_google_scope(tool).is_none() || !raw.is_object()
        || !crate::google_identity::employee_execute::valid_id(&call) {
        return Err(changed());
    }
    crate::google_workspace::validate_native_tool_inputs(tool, raw)
        .map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    if !crate::google_workspace::integration_enabled(&state.home) {
        return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
    }
    let who = crate::google_identity::employee_execute::requester(agent, turn.as_deref(), run.as_deref())?;
    record.requester = format!("{who:?}");
    let who = &who;
    let pending = pending.transpose()?;
    let args = args::parse_any(tool, raw).map_err(arg_error)?;
    let coordinator = AuthorityCoordinator::new(&state.home);
    if approved.is_none() { coordinator.initialize().map_err(|_| changed())?; }
    let lease = coordinator.shared().await.map_err(|_| changed())?;
    let captured = Snapshot::capture(&state.home, agent, tool, raw, who, &turn, &run, &call);
    if captured.as_ref().is_err_and(|e| e.code == "approval_unavailable_unattended") {
        drop(lease);
        credential::notify_unattended(&state.home, agent).await;
        return Err(captured.err().ok_or_else(changed)?);
    }
    let (snapshot, route, policy) = if pending.is_some() { captured.map_err(|_| changed())? } else { captured? };
    record.identity_kind = snapshot.route_kind.clone(); record.account_email = snapshot.account_email.clone();
    record.approval.mode = mode_text(snapshot.approval_mode).into();
    let binding = if let Some(pending) = pending {
        if pending.snapshot != snapshot || approved.as_deref() != Some(pending.digest().as_str()) { return Err(changed()); }
        pending
    } else {
        let binding = Binding { snapshot, nonce: uuid::Uuid::new_v4().to_string() };
        let digest = binding.digest();
        state.drive_writes.insert(binding)?;
        return Ok(json!({"approval_binding_digest":digest}));
    };
    drop(lease);
    let digest = binding.digest();
    crate::google_identity::employee_execute::verify_policy_approval(&state.home, agent, tool, &digest, raw).await?;
    let token = credential::write_token(&state.home, &route).await?;
    binding.snapshot.validate(&state.home, raw, who)?;
    if token.meta.account_email != binding.snapshot.account_email
        || token.meta.identity_kind != binding.snapshot.route_kind
        || token.meta.credential_revision != binding.snapshot.credential_revision { return Err(changed()); }
    let token_meta = token.meta;
    let mut transport = ScopedTransport {
        transport: make_transport(token.bearer)?, home: &state.home, route: &route, meta: &token_meta, failure: None,
    };
    let mut budget = scope::Budget::new();
    let attachment = match &args {
        ToolArgs::Drive(DriveArgs::Create { content: CreateContent::Attachment { file_name }, .. }) => {
            let content = read_attachment(&state.home, agent, turn.as_deref(), who, file_name)?;
            record.bytes = Some(content.bytes.len() as u64); record.content_sha256 = Some(content.sha256.clone());
            Some(content)
        }
        _ => None,
    };
    if let Some(text) = content_text(&args) {
        record.bytes = Some(text.len() as u64); record.content_sha256 = Some(hex::encode(Sha256::digest(text.as_bytes())));
    }
    let verdict = scope::check(&mut transport, &mut budget, &policy, plan(&args)).await;
    transport.check_failure()?;
    require_scope(&verdict)?;
    let metadata = checked_metadata(&args, &mut transport, &mut budget).await?;
    let docs_state = if let ToolArgs::DocsSheets(args) = &args {
        let call = program_docs::read_call(args, &verdict).map_err(arg_error)?;
        Some(read_state(&state.home, &route, &token_meta, &mut transport, &call).await?)
    } else { None };
    let docs_plan = match (&args, &docs_state) {
        (ToolArgs::DocsSheets(args), Some(state)) => Some(program_docs::build(args, &verdict, state).map_err(arg_error)?),
        _ => None,
    };
    // Build before asking the human so invalid programs never produce cards.
    let calls = match &args {
        ToolArgs::Drive(args) => program::build(args, &verdict, attachment, metadata.as_ref().map(|m| m.mime_type.as_str())).map_err(arg_error)?,
        ToolArgs::DocsSheets(_) => vec![docs_plan.as_ref().ok_or_else(changed)?.phase1().map_err(arg_error)?],
    };
    let initial_scope = scope::scope_digest(&verdict);
    if let scope::ScopeVerdict::InScope { parents, matched, .. } = &verdict {
        record.parents_before = parents.clone().unwrap_or_default();
        record.folder_rule = Some(if policy.folders.is_empty() { "global".into() } else { matched.join(",") });
    }
    record.file_id = match plan(&args) { scope::Plan::Create { .. } => None, p => target(&p).map(|id| id.as_str().into()) };
    record.file_name = match &args {
        ToolArgs::Drive(DriveArgs::Create { name, .. }) => {
            record.parents_before.clear();
            Some(name.clone())
        }
        _ => metadata.as_ref().map(|m| m.name.clone()),
    };
    record.destination_folder_id = destination(&plan(&args)).map(|id| id.as_str().into());
    let mut confirmation_folders = Vec::new();
    if binding.snapshot.approval_mode != ApprovalMode::None {
        let destination_name = if let Some(id) = destination(&plan(&args)) {
            Some(folder_path(&mut transport, &mut budget, id, &mut confirmation_folders).await?)
        } else if matches!(plan(&args), scope::Plan::Create { .. }) {
            Some("我的雲端硬碟".into())
        } else { None };
        let mut source_names = Vec::new();
        if matches!(policy::action_for_tool(tool), Some(DriveWriteAction::Move)) {
            for parent in &record.parents_before {
                let id = GoogleId::new(parent.clone()).map_err(|_| changed())?;
                source_names.push(folder_path(&mut transport, &mut budget, &id, &mut confirmation_folders).await?);
            }
        }
        let (attachment_name, rename_after) = match &args {
            ToolArgs::Drive(DriveArgs::Create { content: CreateContent::Attachment { file_name }, .. }) => (Some(file_name.clone()), None),
            ToolArgs::Drive(DriveArgs::Update { name, .. }) => (None, name.clone()),
            _ => (None, None),
        };
        let request = ConfirmRequest {
            arguments: raw.clone(), version: gi::google_arguments_digest(&json!(binding.snapshot)),
            agent: agent.into(), tool: tool.into(), action: policy::action_for_tool(tool).ok_or_else(changed)?,
            approval_mode: binding.snapshot.approval_mode, requester: who.clone(), route_kind: binding.snapshot.route_kind.clone(),
            account_email: binding.snapshot.account_email.clone(), target_file_id: record.file_id.clone(),
            target_file_name: match &args {
                ToolArgs::Drive(DriveArgs::Create { name, .. }) => Some(name.clone()),
                _ => record.file_name.clone(),
            },
            destination: record.destination_folder_id.clone(), destination_name, size: record.bytes,
            content_preview: content_text(&args).map(|s| duduclaw_core::truncate_chars(s, 500)), attachment_name,
            rename_before: rename_after.as_ref().and(record.file_name.clone()), rename_after,
            snapshot_digest: digest.clone(), scope_digest: initial_scope.clone(), turn_id: turn.clone(), run_id: run.clone(),
            call_id: call, parents_before: record.parents_before.clone(), source_names,
        };
        match state.drive_write_confirmer.confirm(request).await {
            ConfirmOutcome::Approved { approval_ids, decided_by } if !approval_ids.is_empty() && !decided_by.is_empty() => {
                record.approval.approval_ids = approval_ids; record.approval.decided_by = decided_by;
            }
            ConfirmOutcome::Denied | ConfirmOutcome::Expired => return Err(not_sent()),
            ConfirmOutcome::Approved { .. } | ConfirmOutcome::Unavailable => return Err(Refusal::hard("approval_unavailable", "目前無法送出確認，沒有執行。")),
        }
    }
    // Do not hold authority or a mutex while waiting for a person. Reacquire
    // the revocation barrier for the final checks and the actual Google I/O.
    let _lease = coordinator.shared().await.map_err(|_| changed())?;
    budget.begin_fresh_phase();
    binding.snapshot.validate(&state.home, raw, who)?;
    crate::google_identity::employee_execute::verify_policy_approval(&state.home, agent, tool, &digest, raw).await?;
    let fresh = scope::check(&mut transport, &mut budget, &policy, plan(&args)).await;
    transport.check_failure()?;
    if scope::scope_digest(&fresh) != initial_scope { return Err(changed()); }
    require_scope(&fresh).map_err(|_| changed())?;
    // Folder paths are part of what the member read. Revalidate every displayed
    // ancestor under the same bounded metadata budget before writing.
    for folder in &confirmation_folders {
        let id = GoogleId::new(folder.id.clone()).map_err(|_| changed())?;
        if get_metadata(&mut transport, &mut budget, &id).await? != *folder { return Err(changed()); }
    }
    // MIME/name changes and Docs index changes also invalidate the approved plan.
    if checked_metadata(&args, &mut transport, &mut budget).await? != metadata { return Err(changed()); }
    if let ToolArgs::DocsSheets(args) = &args {
        let call = program_docs::read_call(args, &fresh).map_err(arg_error)?;
        if Some(read_state(&state.home, &route, &token_meta, &mut transport, &call).await?) != docs_state { return Err(changed()); }
    }
    binding.snapshot.validate(&state.home, raw, who)?;
    let mut response = None;
    for call in calls {
        response = Some(send_write(&state.home, &route, &token_meta, &mut transport, &call, progress).await?);
    }
    let (status, body) = response.ok_or_else(not_sent)?;
    let (file_id, file_name, link, post) = match &args {
        ToolArgs::Drive(args) => {
            let outcome = program::interpret(tool, status, &body).map_err(|error| outcome_error(&state.home, &route, &token_meta, error))?;
            if let Some(expected) = record.file_id.as_deref() {
                if !matches!(args, DriveArgs::Copy { .. }) && outcome.file_id != expected { return Err(unconfirmed()); }
            }
            record.parents_after = outcome.parents_after.clone().unwrap_or_default();
            let post = match args {
                DriveArgs::Create { .. } | DriveArgs::Copy { .. } | DriveArgs::Move { .. } =>
                    scope::post_check_parents(&program::expected_parents(args, &fresh), outcome.parents_after.as_deref()),
                DriveArgs::Update { target, .. } | DriveArgs::Trash { target } =>
                    scope::recheck(&mut transport, &mut budget, &policy, target).await,
            };
            (outcome.file_id, outcome.full_file_name, outcome.link, post)
        }
        ToolArgs::DocsSheets(_) => {
            let program = docs_plan.ok_or_else(changed)?;
            let outcome = program.interpret(status, &body).map_err(|e| outcome_error(&state.home, &route, &token_meta, e))?;
            if let Some(call) = program.phase2(status, &body).map_err(|e| outcome_error(&state.home, &route, &token_meta, e))? {
                binding.snapshot.validate(&state.home, raw, who).map_err(|_| unconfirmed())?;
                let (status, body) = send_write(&state.home, &route, &token_meta, &mut transport, &call, progress).await?;
                program.interpret_phase2(status, &body).map_err(|e| outcome_error(&state.home, &route, &token_meta, e))?;
            }
            let id = GoogleId::new(outcome.file_id.clone()).map_err(|_| unconfirmed())?;
            let post = scope::recheck(&mut transport, &mut budget, &policy, &id).await;
            (outcome.file_id, record.file_name.clone().unwrap_or_default(), outcome.link, post)
        }
    };
    if let Err(error) = transport.check_failure() {
        record.post_check = audit::PostCheck::Incomplete;
        return Err(error);
    }
    record.file_id = Some(file_id.clone()); record.file_name = Some(file_name.clone());
    record.outcome = audit::Outcome::Confirmed;
    record.post_check = match post {
        scope::PostCheck::Ok => audit::PostCheck::Ok,
        scope::PostCheck::Violation => audit::PostCheck::Violation,
        scope::PostCheck::Incomplete => audit::PostCheck::Incomplete,
    };
    match post {
        scope::PostCheck::Violation => {
            record.reason = audit::Reason::from_code("post_check_violation");
            audit::notify_admin_post_check_violation(&state.home, agent, &file_id).await;
        }
        scope::PostCheck::Incomplete => record.reason = audit::Reason::from_code("post_check_incomplete"),
        scope::PostCheck::Ok => {}
    }
    Ok(json!({"ok":true, "file_id":file_id, "file_name":file_name, "link":link,
        "message":format!("已使用 {} 寫入。", binding.snapshot.account_email)}))
}

/// Display paths use the same bounded Google metadata source as scope checks.
/// Missing visibility, cycles or ambiguous parents cannot produce a guessed path.
async fn folder_path(transport: &mut ScopedTransport<'_>, budget: &mut scope::Budget,
    id: &GoogleId, displayed: &mut Vec<scope::FileMeta>,
) -> Result<String, Refusal> {
    let mut current = id.clone();
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    loop {
        if !seen.insert(current.as_str().to_owned()) { return Err(changed()); }
        let folder = get_metadata(transport, budget, &current).await?;
        if folder.mime_type != "application/vnd.google-apps.folder" || folder.trashed
            || folder.shortcut_target.is_some() { return Err(changed()); }
        names.push(folder.name.clone());
        let next = match folder.parents.as_deref().unwrap_or_default() {
            [] => None,
            [parent] => Some(GoogleId::new(parent.clone()).map_err(|_| changed())?),
            _ => return Err(changed()),
        };
        if !displayed.iter().any(|existing| existing.id == folder.id) { displayed.push(folder); }
        let Some(next) = next else { break; };
        current = next;
    }
    names.reverse();
    // JSON quoting retains component boundaries even when a folder name contains '/'.
    Ok(names.iter().map(|name| json!(name).to_string()).collect::<Vec<_>>().join(" / "))
}

async fn checked_metadata(args: &ToolArgs, transport: &mut ScopedTransport<'_>, budget: &mut scope::Budget) -> Result<Option<scope::FileMeta>, Refusal> {
    match target(&plan(args)) {
        Some(id) => get_metadata(transport, budget, id).await.map(Some),
        None => Ok(None),
    }
}
async fn get_metadata(transport: &mut ScopedTransport<'_>, budget: &mut scope::Budget, id: &GoogleId) -> Result<scope::FileMeta, Refusal> {
    let result = budget.get(transport, id).await;
    transport.check_failure()?;
    result.map_err(|v| require_scope(&v).err().unwrap_or_else(changed))
}

fn content_text(args: &ToolArgs) -> Option<&str> {
    match args {
        ToolArgs::Drive(DriveArgs::Create { content: CreateContent::Text { text, .. }, .. }) => Some(text),
        ToolArgs::Drive(DriveArgs::Update { text, description, .. }) => text.as_deref().or(description.as_deref()),
        ToolArgs::DocsSheets(super::args_docs::DocsSheetsArgs::Insert { text, .. }) => Some(text),
        ToolArgs::DocsSheets(super::args_docs::DocsSheetsArgs::AddNote { note, .. }) => Some(note),
        ToolArgs::DocsSheets(super::args_docs::DocsSheetsArgs::Format { operations, .. }) => operations.iter().find_map(|op| {
            if let super::args_docs::Operation::Header { text } = op { Some(text.as_str()) } else { None }
        }),
        _ => None,
    }
}

fn read_attachment(home: &Path, agent: &str, turn: Option<&str>, who: &RequesterIdentity, file_name: &str) -> Result<program::AttachmentContent, Refusal> {
    const LIMIT: u64 = 5 * 1024 * 1024;
    let invalid = || Refusal::hard("invalid_attachment", "只能上傳這次對話的附件");
    let too_large = || Refusal::hard("content_too_large", "這個檔案超過 5 MB，這一版不能上傳。");
    args::attachment_name(file_name).map_err(|_| invalid())?;
    if !duduclaw_core::is_valid_agent_id(agent) { return Err(invalid()); }
    let dir = home.join("agents").join(agent).join("attachments");
    for path in [home.join("agents"), home.join("agents").join(agent), dir.clone()] {
        let meta = std::fs::symlink_metadata(path).map_err(|_| invalid())?;
        if meta.file_type().is_symlink() || !meta.is_dir() { return Err(invalid()); }
    }
    let path = dir.join(file_name);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| invalid())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() { return Err(invalid()); }
    if metadata.len() > LIMIT { return Err(too_large()); }
    let mut options = std::fs::OpenOptions::new(); options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path).map_err(|_| invalid())?;
    let opened = file.metadata().map_err(|_| invalid())?;
    if opened.file_type().is_symlink() || !opened.is_file() { return Err(invalid()); }
    if opened.len() > LIMIT { return Err(too_large()); }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).map_err(|_| invalid())?;
    if bytes.len() as u64 > LIMIT { return Err(too_large()); }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let (registered, inbound) = crate::knowledge::turn_requesters::inbound(agent, turn.ok_or_else(invalid)?).ok_or_else(invalid)?;
    if registered != *who || !inbound.attachment_hashes.iter().any(|hash| hash == &sha256) { return Err(invalid()); }
    Ok(program::AttachmentContent { file_name: file_name.into(), bytes: bytes.into(), sha256 })
}

fn outcome_error(home: &Path, route: &IdentityRoute, meta: &credential::TokenMeta, error: program::OutcomeError) -> Refusal {
    if error == program::OutcomeError::InsufficientPermissions {
        if let Err(error) = credential::on_insufficient_permissions(home, route, meta) { return error; }
    }
    if error == program::OutcomeError::Unconfirmed { unconfirmed() }
    else { Refusal::hard(error.code(), error.user_text()) }
}
fn status_check(home: &Path, route: &IdentityRoute, meta: &credential::TokenMeta, status: u16, body: &[u8]) -> Result<(), Refusal> {
    if (200..=299).contains(&status) { return Ok(()); }
    // The common interpreter checks provider error reasons before any success fields.
    let error = program::interpret("drive_update_file", status, body).err().unwrap_or(program::OutcomeError::Unconfirmed);
    Err(outcome_error(home, route, meta, error))
}
async fn read_state(home: &Path, route: &IdentityRoute, meta: &credential::TokenMeta, transport: &mut ScopedTransport<'_>, call: &WriteCall) -> Result<Value, Refusal> {
    match transport.send(call).await {
        WriteResult::Ok((status, body)) => {
            status_check(home, route, meta, status, &body)?;
            serde_json::from_slice(&body).map_err(|_| arg_error(ArgError::InvalidState))
        }
        WriteResult::NotSent(_) | WriteResult::MaybeSent(_) => Err(not_sent()),
    }
}
async fn send_write(home: &Path, route: &IdentityRoute, meta: &credential::TokenMeta, transport: &mut ScopedTransport<'_>, call: &WriteCall, progress: &AtomicUsize) -> Result<(u16, Vec<u8>), Refusal> {
    // Set before awaiting so cancellation cannot misreport a possibly sent write.
    progress.fetch_add(1, Ordering::SeqCst);
    match transport.send(call).await {
        WriteResult::NotSent(_) => {
            let previous = progress.fetch_sub(1, Ordering::SeqCst);
            Err(if previous > 1 { unconfirmed() } else { not_sent() })
        }
        WriteResult::MaybeSent(_) => Err(unconfirmed()),
        WriteResult::Ok((status, body)) => {
            status_check(home, route, meta, status, &body)?;
            Ok((status, body))
        }
    }
}
fn make_transport(bearer: zeroize::Zeroizing<String>) -> Result<WriteTransport, Refusal> {
    #[cfg(test)] {
        // No real Google traffic in tests; the dependency is scoped to one task.
        return TEST_HTTP.try_with(|http| WriteTransport::with_http(bearer, Box::new(http.clone())))
            .map_err(|_| not_sent());
    }
    #[cfg(not(test))]
    WriteTransport::new(bearer).map_err(|_| not_sent())
}
#[cfg(test)]
tokio::task_local! { static TEST_HTTP: std::sync::Arc<tests::Http>; }

#[cfg(test)]
#[path = "serve_tests.rs"]
pub(super) mod tests;

/// Preserve insufficientPermissions while the generic scope layer maps HTTP
/// failures to non-visibility. The token invalidation is always revision-bound.
struct ScopedTransport<'a> {
    transport: WriteTransport,
    home: &'a Path,
    route: &'a IdentityRoute,
    meta: &'a credential::TokenMeta,
    failure: Option<Refusal>,
}
impl ScopedTransport<'_> {
    async fn send(&mut self, call: &WriteCall) -> WriteResult { self.transport.send(call).await }
    fn check_failure(&mut self) -> Result<(), Refusal> {
        match self.failure.take() { Some(error) => Err(error), None => Ok(()) }
    }
}
impl scope::MetadataSource for ScopedTransport<'_> {
    fn get(&mut self, id: &GoogleId) -> scope::BoxFuture<'_, Result<scope::FileMeta, scope::MetaError>> {
        let id = id.clone();
        Box::pin(async move {
            let call = WriteCall::DriveGetMetadata { file_id: id.clone(), fields: super::transport::MetadataFields::Scope };
            let result = self.transport.send(&call).await;
            if let WriteResult::Ok((403, body)) = &result {
                if program::interpret("drive_update_file", 403, body).err() == Some(program::OutcomeError::InsufficientPermissions) {
                    self.failure = Some(outcome_error(self.home, self.route, self.meta, program::OutcomeError::InsufficientPermissions));
                }
            }
            scope::metadata_result(result, &id)
        })
    }
}
