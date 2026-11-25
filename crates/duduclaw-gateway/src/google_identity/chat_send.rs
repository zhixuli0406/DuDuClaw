//! RM-062: gateway-owned member send. No token or approval capability crosses MCP.
use std::{collections::HashMap, path::Path, sync::{Mutex, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};
use duduclaw_core::{google_identity as gi, authority_coordinator::AuthorityCoordinator};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use super::{Refusal, requester_gate, resolve};
use crate::approval::{ApprovalBroker, ApprovalStatus, ExecutionBinding, RequestKind, action_grant::ActionGrant};

pub(crate) const TOOL: &str = "google_chat_send";
fn refused() -> Refusal { Refusal::hard("requester_refused", "Chat 傳送必須使用提出要求的已驗證成員本人的 Google 帳號，並由本人確認；沒有使用其他帳號。") }
fn changed() -> Refusal { Refusal::hard("identity_changed", "Google 帳號、憑證或權限已變更，請重新確認 Chat 傳送。") }
fn store_error<T>(_: T) -> Refusal { changed() }
fn member(who: &RequesterIdentity) -> Result<&str, Refusal> {
    match who {
        RequesterIdentity::DashboardUser { user_id } => Ok(user_id),
        RequesterIdentity::ChannelUser { user_id: Some(user_id), channel, .. }
            if !duduclaw_knowledge::requester::is_public_visitor_channel(channel) => Ok(user_id),
        _ => Err(refused()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot { pub member: String, pub account: String, version: String }
impl Snapshot {
    pub(crate) fn capture(home: &Path, agent: &str, who: &RequesterIdentity) -> Result<Self, Refusal> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(store_error)?;
        let uid = member(who)?;
        if !crate::google_workspace::integration_enabled(home) || !crate::google_workspace::chat_send_enabled(home) {
            return Err(Refusal::hard("scope_missing", crate::google_workspace::chat_send::GUIDANCE));
        }
        let db = crate::decision_notify::open_user_db_read_only(home).map_err(store_error)?.ok_or_else(refused)?;
        let user = db.get_user(uid).map_err(store_error)?.ok_or_else(refused)?;
        if user.status != duduclaw_auth::UserStatus::Active
            || (crate::machine_employee::requires_admin_for_id(home, agent) && user.role != duduclaw_auth::UserRole::Admin) { return Err(refused()); }
        if user.role != duduclaw_auth::UserRole::Admin
            && !db.check_agent_access(uid, agent).map_err(store_error)?
                .is_some_and(|a| a.at_least(duduclaw_auth::AccessLevel::Operator)) { return Err(refused()); }
        if let RequesterIdentity::ChannelUser { channel, sender, .. } = who {
            if crate::googlechat_identity::verified_user_id(home, &db, channel, sender).map_err(store_error)?.as_deref() != Some(uid) {
                return Err(refused());
            }
        }
        let effective = super::delegate_owner::effective_for(home, Some(agent));
        let (account, credential) = match &effective.effective {
            gi::Effective::Personal { selector } => {
                let owner = crate::google_personal::access::select_user(&db, agent, selector)?;
                if owner.id != uid || db.google_consent(uid, agent).map_err(store_error)? != Some(true) { return Err(refused()); }
                let record = crate::person_credentials::get(home, uid).map_err(store_error)?.ok_or_else(||
                    Refusal::hard("not_connected", "負責人還沒連結 Google，請本人到「我的連線」按「連結我的 Google」。"))?;
                if record.metadata.status != crate::person_credentials::Status::Connected
                    || !crate::google_personal::scopes::permitted_for(TOOL, &record.metadata)
                    || !crate::google_personal::access::domain_allowed(home, &record.metadata).map_err(store_error)? {
                    return Err(Refusal::hard("scope_missing", crate::google_workspace::chat_send::GUIDANCE));
                }
                crate::google_data_consent::require_current(record.metadata.google_data_consent.as_ref()).map_err(store_error)?;
                (record.metadata.account_email.clone(), json!(record.metadata))
            }
            gi::Effective::Delegate { email } => {
                let accounts = super::tenant_source::subject_accounts(&db, agent,
                    super::tenant_source::current_incarnation(home, agent), email).map_err(store_error)?;
                if !matches!(accounts.as_slice(), [account] if account.user_id == uid && account.active && account.attested) { return Err(refused()); }
                let sealed = std::fs::read(home.join("protected/google-service-account.sealed")).map_err(store_error)?;
                let attestation = db.google_delegate_attestation(agent).map_err(store_error)?.ok_or_else(refused)?;
                (email.clone(), json!([gi::google_arguments_digest(&json!(sealed)), format!("{attestation:?}")]))
            }
            _ => return Err(refused()),
        };
        let config = std::fs::read(home.join("config.toml")).map_err(store_error)?;
        let employee = duduclaw_core::agent_toml::read_effective_text(&home.join("agents").join(agent)).map_err(store_error)?;
        let owner = if gi::effective_for(home, Some(agent)).effective == gi::Effective::DelegateOwner {
            Some(super::owner_snapshot::OwnerSnapshot::capture(home, agent)?)
        } else { None };
        let epoch = AuthorityCoordinator::new(home).epoch().map_err(store_error)?;
        Ok(Self { member: uid.into(), account, version: gi::google_arguments_digest(&json!([
            config, employee, credential, owner, epoch, user.email, user.role,
        ])) })
    }
    fn validate(&self, home: &Path, agent: &str, who: &RequesterIdentity) -> Result<(), Refusal> {
        if &Self::capture(home, agent, who)? == self { Ok(()) } else { Err(changed()) }
    }
}

struct Pending { snapshot: Snapshot, args: String, who: RequesterIdentity, turn: String, created: Instant, request_id: String }
#[derive(Default)]
pub(super) struct Store(Mutex<HashMap<(String, String), Pending>>);
impl Pending {
    fn digest(&self, agent: &str, call: &str) -> String {
        gi::google_arguments_digest(&json!([agent, call, self.snapshot, self.args, self.turn, self.request_id]))
    }
}
pub(super) async fn serve(state: &super::execute::ExecuteState, agent: &str, args: &Value,
    who: RequesterIdentity, turn: Option<String>, call: String, approved: Option<String>) -> Result<Value, Refusal> {
    crate::google_workspace::chat_send::parse(args).map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    let turn = turn.ok_or_else(refused)?;
    let coordinator = AuthorityCoordinator::new(&state.home);
    if approved.is_none() { coordinator.initialize().map_err(store_error)?; }
    let _lease = coordinator.shared().await.map_err(store_error)?;
    let snapshot = Snapshot::capture(&state.home, agent, &who)?;
    let pending = {
        let mut entries = state.chats.0.lock().map_err(store_error)?;
        entries.retain(|_, p| p.created.elapsed() < Duration::from_secs(300));
        let key = (agent.to_owned(), call.clone());
        if let Some(digest) = approved {
            let pending = entries.remove(&key).ok_or_else(changed)?;
            if pending.digest(agent, &call) != digest || pending.snapshot != snapshot || pending.who != who
                || pending.turn != turn || pending.args != gi::google_arguments_digest(args) { return Err(changed()); }
            pending
        } else {
            if entries.len() >= 1024 || entries.contains_key(&key) { return Err(changed()); }
            let pending = Pending { snapshot, args:gi::google_arguments_digest(args), who, turn,
                created:Instant::now(), request_id:uuid::Uuid::new_v4().to_string() };
            let result = json!({"approval_binding_digest":pending.digest(agent, &call), "owner_email":pending.snapshot.account});
            entries.insert(key, pending);
            return Ok(result);
        }
    };
    drop(_lease);
    super::employee_execute::verify_policy_approval(&state.home, agent, TOOL, &pending.digest(agent, &call), args).await?;
    execute(&state.home, agent, args, pending.who, &pending.turn, pending.snapshot, pending.request_id).await
}

// The existing private native transport is reused; it revalidates after every
// await and holds a revocation barrier across authenticated I/O.
struct Resolved { access: zeroize::Zeroizing<String>, subject: String }
struct Bearer<'a> { material: &'a Resolved, permit: Permit<'a> }
struct Permit<'a> {
    home: &'a Path, agent: &'a str, who: &'a RequesterIdentity, snapshot: &'a Snapshot,
    turn: &'a str, args: &'a str, progress: &'a AtomicUsize,
}
impl Permit<'_> {
    async fn acquire_native_lease(&self) -> Result<Option<duduclaw_core::authority_coordinator::SharedLease>, Refusal> {
        let lease = AuthorityCoordinator::new(self.home).shared().await.map_err(store_error)?;
        self.snapshot.validate(self.home, self.agent, self.who)?;
        Ok(Some(lease))
    }
    fn native_sent(&self) { self.progress.fetch_add(1, Ordering::SeqCst); }
    #[cfg(test)] fn test_native_boundary(&self, stage: &str) { crate::google_workspace::native_call::testing::boundary(self.home, stage); }
    #[cfg(test)] fn test_endpoint(&self, url: &str) -> Option<reqwest::Url> { crate::google_workspace::native_call::testing::endpoint(self.home, url) }
    #[cfg(test)] fn test_completed(&self, value: &Value) { crate::google_workspace::native_call::testing::completed(self.home, value); }
}
fn validate_native(permit: &Permit<'_>, _: &Resolved, template: &crate::google_workspace::native_call::NativeCallTemplate) -> Result<(), Refusal> {
    if crate::knowledge::turn_requesters::lookup(permit.agent, permit.turn).as_ref() != Some(permit.who) { return Err(changed()); }
    if template.tool() != TOOL || template.digest() != permit.args { return Err(changed()); }
    permit.snapshot.validate(permit.home, permit.agent, permit.who)
}
fn source_notice(material: &Resolved) -> String { format!("已使用你的 Google 帳號：{}。Google 會附上應用程式名稱。", material.subject) }
mod native_io { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/google_identity/assertion_native_io.inc.rs")); }

tokio::task_local! { static PERMIT: (String, String); }
pub(crate) fn has_permit(template: &crate::google_workspace::native_call::NativeCallTemplate) -> bool {
    template.tool() != TOOL || PERMIT.try_with(|p| p.0 == template.digest() && p.1 == template.meet_request_id()).unwrap_or(false)
}

pub(crate) async fn execute(home: &Path, agent: &str, args: &Value, who: RequesterIdentity,
    turn: &str, snapshot: Snapshot, request_id: String) -> Result<Value, Refusal> {
    snapshot.validate(home, agent, &who)?;
    let db = crate::decision_notify::open_user_db(home).map_err(store_error)?.ok_or_else(refused)?;
    let people = requester_gate::UserDbPeople(std::sync::Arc::new(db));
    let resolved = resolve::resolve_person_for_tool(home, agent, &who, &people, TOOL).await?;
    let (token, email) = match resolved {
        resolve::Resolved::Personal { token, email, user_id, .. } if user_id == snapshot.member => (token, email),
        resolve::Resolved::Delegated { token, email } => (token, email),
        _ => return Err(refused()),
    };
    if email != snapshot.account { return Err(refused()); }
    // A refresh that changed credential revision requires a new confirmation.
    snapshot.validate(home, agent, &who)?;
    let mut template = crate::google_workspace::native_call::NativeCallTemplate::prepare(TOOL, args)
        .map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    template.set_chat_request_id(&request_id);
    let mut payload = json!({"name":TOOL, "arguments":args, "chat_member_confirmation":true,
        "chat_snapshot":snapshot, "request_id":request_id});
    let grant = ActionGrant::build(agent, TOOL, &payload);
    payload["action_grant"] = grant.to_json();
    let context = crate::turn_context::decision_context_for(agent, turn).ok_or_else(refused)?;
    if crate::approval::mail_requester::requester_user(home, &context, agent).as_deref() != Some(snapshot.member.as_str()) {
        return Err(refused());
    }
    let audience = crate::turn_context::target_for(agent, turn).map(|t| t.conversation_audience)
        .unwrap_or(crate::approval::ConversationAudience::MultiParty);
    let binding = ExecutionBinding { schema_version:1, run_id:turn.into(), run_origin_kind:"ingress".into(),
        actor_principal:agent.into(), decision_context:context, conversation_audience:audience,
        task_id:None, task_revision:None, task_snapshot_hash:None, payload_hash:crate::approval::payload_hash(&payload),
        policy_revision:crate::approval::policy_revision(home, agent).map_err(store_error)?, cwd:None,
        environment_hash:snapshot.version.clone(), file_hashes:Default::default(),
        expires_at:(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(), resume_handler:"live_only_v1".into(), resume_version:1 };
    let broker = ApprovalBroker::open(home).map_err(store_error)?;
    let principal = duduclaw_core::HumanPrincipal::user(&snapshot.member).ok_or_else(refused)?.to_string();
    let session = crate::turn_context::session_for(agent, turn).ok_or_else(refused)?;
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), crate::approval::scope_turn(&session, turn,
        broker.request_bound(RequestKind::Approval, agent, "以你的 Google 帳號傳送", payload, binding))).await.map_err(store_error)?;
    let rec = broker.get(&id).await.map_err(store_error)?.ok_or_else(changed)?;
    broker.push_new_request(&rec).await;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        snapshot.validate(home, agent, &who)?;
        let rec = broker.get(&id).await.map_err(store_error)?.ok_or_else(changed)?;
        if rec.status == ApprovalStatus::Approved {
            crate::approval::action_grant::verify_and_consume(&broker, &id, &grant).await.map_err(store_error)?;
            break;
        }
        if rec.status != ApprovalStatus::Pending || Instant::now() >= deadline { return Err(Refusal::hard("approval_denied", "本人未確認或已取消，沒有傳送 Chat。")); }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let material = Resolved { access:zeroize::Zeroizing::new(token.access_token), subject:email };
    let progress = AtomicUsize::new(0);
    let digest = gi::google_arguments_digest(args);
    let permit = Permit { home, agent, turn, who:&who, snapshot:&snapshot, args:&digest, progress:&progress };
    let result = PERMIT.scope((digest.clone(), request_id.clone()),
        Bearer { material:&material, permit }.execute_native(&template)).await.map(|r| r.into_value()).map_err(|e| e.into_refusal())
        .and_then(|value| if value["isError"] == true {
            Err(Refusal::hard("result_unconfirmed", "Chat 傳送結果無法確認，請先檢查 Google，勿自動重送。"))
        } else { Ok(value) });
    duduclaw_security::audit::append_tool_call_with_extras(home, agent, "google_execute", TOOL, result.is_ok(), &[
        ("request_id", json!(request_id)), ("approval_id", json!(id.as_str())), ("member", json!(snapshot.member)),
        ("space", args["space"].clone()), ("outcome", json!(if result.is_ok() { "confirmed" } else if progress.load(Ordering::SeqCst) > 0 { "result_unconfirmed" } else { "not_sent" }))]);
    result
}

pub(crate) fn is_member_card(rec: &crate::approval::ApprovalRecord) -> bool {
    rec.payload["name"] == TOOL && rec.payload["chat_member_confirmation"] == true
}
pub(crate) fn eligible(home: &Path, rec: &crate::approval::ApprovalRecord) -> bool {
    let Some(binding) = &rec.binding else { return false; };
    let Ok(snapshot) = serde_json::from_value::<Snapshot>(rec.payload["chat_snapshot"].clone()) else { return false; };
    let Some(who) = crate::knowledge::turn_requesters::lookup(&rec.agent_id, &binding.run_id) else { return false; };
    is_member_card(rec) && rec.request_kind == RequestKind::Approval && rec.action_kind == "bound_action"
        && binding.actor_principal == rec.agent_id
        && duduclaw_core::HumanPrincipal::user(&snapshot.member)
            .is_some_and(|p| rec.on_behalf_of.as_deref() == Some(p.to_string().as_str()))
        && binding.validate(&rec.payload).is_ok()
        && binding.environment_hash == snapshot.version && rec.ttl_seconds <= 300
        && binding.run_origin_kind == "ingress" && binding.resume_handler == "live_only_v1"
        && crate::approval::mail_requester::requester_user(home, &binding.decision_context, &rec.agent_id).as_deref() == Some(snapshot.member.as_str())
        && snapshot.validate(home, &rec.agent_id, &who).is_ok()
        && crate::approval::policy_revision(home, &rec.agent_id).ok().as_deref() == Some(binding.policy_revision.as_str())
        && ActionGrant::build(&rec.agent_id, TOOL, &rec.payload).matches_stored(&rec.payload["action_grant"])
}
pub(crate) fn may_decide(home: &Path, rec: &crate::approval::ApprovalRecord, uid: &str) -> bool {
    rec.payload["chat_snapshot"]["member"].as_str() == Some(uid) && eligible(home, rec)
}
pub(crate) fn decided_by_member(home: &Path, rec: &crate::approval::ApprovalRecord) -> bool {
    if !eligible(home, rec) { return false; }
    let Some(binding) = &rec.binding else { return false; };
    let uid = rec.payload["chat_snapshot"]["member"].as_str().unwrap_or("");
    rec.decided_by.as_deref() == Some(format!("dashboard:{uid}").as_str())
        || (binding.conversation_audience == crate::approval::ConversationAudience::Private
            && rec.decided_by.as_deref() == Some(format!("{}:{}", binding.decision_context.channel, binding.decision_context.principal_id).as_str()))
}
// Display-only projection. Never replace the raw arguments bound by the grant
// and execution binding or used by the native transport with these strings.
pub(crate) fn confirmation_display(rec: &crate::approval::ApprovalRecord) -> Value {
    use duduclaw_security::credential_protection::sanitize_output_text;
    let text = |value: &Value| sanitize_output_text(value.as_str().unwrap_or(""));
    json!({"id":rec.id.as_str(), "employee":sanitize_output_text(&rec.agent_id),
        "account":text(&rec.payload["chat_snapshot"]["account"]),
        "space":text(&rec.payload["arguments"]["space"]),
        "thread":rec.payload["arguments"]["thread"].as_str().map(sanitize_output_text),
        "text_display":text(&rec.payload["arguments"]["text"]),
        "expires_at":rec.binding.as_ref().map(|b| &b.expires_at)})
}
pub(crate) fn card_body(rec: &crate::approval::ApprovalRecord) -> String {
    let display = confirmation_display(rec);
    format!("以你的 Google 帳號傳送\n帳號：{}\n空間：{}\n討論串：{}\n完整文字：\n{}\n\n這段文字可能包含私人資料，確認後會傳到上述空間。Google 會附上應用程式名稱。\n五分鐘內有效，僅限本人決定。\n確認 {}\n取消 {}",
        display["account"].as_str().unwrap_or(""), display["space"].as_str().unwrap_or(""),
        display["thread"].as_str().unwrap_or("新訊息"), display["text_display"].as_str().unwrap_or(""), rec.id, rec.id)
}
