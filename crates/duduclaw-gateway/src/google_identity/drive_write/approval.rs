//! Gateway-owned, single-use Drive confirmations. Payload identities are never authority.
use std::{future::Future, path::{Path, PathBuf}, pin::Pin, time::Duration};
use serde_json::{json, Value};
use duduclaw_knowledge::requester::RequesterIdentity;
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus,
    ConversationAudience, ExecutionBinding, RequestKind, action_grant::{self, ActionGrant}};
use crate::google_workspace::drive_write_policy::{self as policy, ApprovalMode};
use super::{credential, serve::{self, ConfirmOutcome, ConfirmRequest, DriveWriteConfirmer}};
use crate::google_identity::member_card;

pub(in crate::google_identity) struct MemberConfirmer { home: PathBuf }
impl MemberConfirmer {
    pub(in crate::google_identity) fn new(home: PathBuf) -> Self { Self { home } }
}
impl DriveWriteConfirmer for MemberConfirmer {
    fn confirm<'a>(&'a self, request: ConfirmRequest)
        -> Pin<Box<dyn Future<Output = ConfirmOutcome> + Send + 'a>> {
        Box::pin(async move {
            // Includes notification time and both stages; cancellation never writes.
            match tokio::time::timeout(Duration::from_secs(600), self.execute(request)).await {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(_)) => ConfirmOutcome::Denied,
                Err(_) => ConfirmOutcome::Expired,
            }
        })
    }
}

fn member(home: &Path, agent: &str, who: &RequesterIdentity) -> Result<String, String> {
    let uid = match who {
        RequesterIdentity::DashboardUser { user_id } => user_id,
        RequesterIdentity::ChannelUser { user_id: Some(user_id), channel, .. }
            if !duduclaw_knowledge::requester::is_public_visitor_channel(channel) => user_id,
        _ => return Err("live member required".into()),
    };
    let route = credential::resolve_route(home, agent).map_err(|_| "identity changed")?;
    credential::requester_check(&route, who, true).map_err(|_| "requester refused")?;
    let db = crate::decision_notify::open_user_db_read_only(home)?.ok_or("user database missing")?;
    let user = db.get_user(uid)?.ok_or("member missing")?;
    if user.status != duduclaw_auth::UserStatus::Active
        || (crate::machine_employee::requires_admin_for_id(home, agent) && user.role != duduclaw_auth::UserRole::Admin)
        || (user.role != duduclaw_auth::UserRole::Admin && !db.check_agent_access(uid, agent)?
            .is_some_and(|a| a.at_least(duduclaw_auth::AccessLevel::Operator))) {
        return Err("member access changed".into());
    }
    if let RequesterIdentity::ChannelUser { channel, sender, .. } = who {
        if crate::googlechat_identity::verified_user_id(home, &db, channel, sender)?.as_deref() != Some(uid.as_str()) {
            return Err("channel identity changed".into());
        }
    }
    Ok(uid.clone())
}

impl MemberConfirmer {
    async fn request_member(&self, request: &ConfirmRequest) -> Result<(ApprovalBroker, ApprovalId, ActionGrant), String> {
        let home = &self.home;
        let turn = request.turn_id.as_deref().ok_or("live turn required")?;
        if crate::knowledge::turn_requesters::lookup(&request.agent, turn).as_ref() != Some(&request.requester) {
            return Err("requester changed".into());
        }
        let uid = member(home, &request.agent, &request.requester)?;
        let version = serve::confirmation_version(home, &request.agent, &request.tool, &request.arguments,
            &request.requester, &request.turn_id, &request.run_id, &request.call_id).map_err(|_| "snapshot changed")?;
        if version != request.version || policy::action_for_tool(&request.tool) != Some(request.action)
            || request.approval_mode == ApprovalMode::None {
            return Err("snapshot changed".into());
        }
        let context = crate::turn_context::decision_context_for(&request.agent, turn).ok_or("decision context missing")?;
        if crate::approval::mail_requester::requester_user(home, &context, &request.agent).as_deref() != Some(uid.as_str()) {
            return Err("decision identity changed".into());
        }
        let audience = crate::turn_context::target_for(&request.agent, turn).map(|t| t.conversation_audience)
            .unwrap_or(ConversationAudience::MultiParty);
        let mut payload = json!({"name":request.tool, "arguments":request.arguments,
            "drive_write_member_confirmation":true, "drive_snapshot":{
                "member":uid, "agent":request.agent, "account":request.account_email,
                "route_kind":request.route_kind, "action":request.action.as_str(),
                "snapshot_digest":request.snapshot_digest, "scope_digest":request.scope_digest,
                "file_id":request.target_file_id, "file_name":request.target_file_name,
                "destination":request.destination, "destination_name":request.destination_name,
                "size":request.size, "preview":request.content_preview.as_deref().map(|s| duduclaw_core::truncate_chars(s, 500)),
                "attachment_name":request.attachment_name, "rename_before":request.rename_before,
                "rename_after":request.rename_after, "parents_before":request.parents_before, "source_names":request.source_names,
                "version":version, "turn_id":request.turn_id, "run_id":request.run_id, "call_id":request.call_id
            }});
        let grant = ActionGrant::build(&request.agent, &request.tool, &payload);
        payload["action_grant"] = grant.to_json();
        let binding = ExecutionBinding { schema_version:1, run_id:turn.into(), run_origin_kind:"ingress".into(),
            actor_principal:request.agent.clone(), decision_context:context, conversation_audience:audience,
            task_id:None, task_revision:None, task_snapshot_hash:None, payload_hash:crate::approval::payload_hash(&payload),
            policy_revision:crate::approval::policy_revision(home, &request.agent)?, cwd:None,
            environment_hash:version, file_hashes:Default::default(),
            expires_at:(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),
            resume_handler:"live_only_v1".into(), resume_version:1 };
        let broker = ApprovalBroker::open(home)?;
        let principal = duduclaw_core::HumanPrincipal::user(&uid).ok_or("member missing")?.to_string();
        let session = crate::turn_context::session_for(&request.agent, turn).ok_or("session missing")?;
        let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), crate::approval::scope_turn(&session, turn,
            broker.request_bound(RequestKind::Approval, &request.agent, "確認雲端硬碟操作", payload, binding))).await?;
        Ok((broker, id, grant))
    }

    async fn execute(&self, request: ConfirmRequest) -> Result<ConfirmOutcome, String> {
        let (broker, id, grant) = self.request_member(&request).await?;
        let rec = broker.get(&id).await?.ok_or("confirmation missing")?;
        broker.push_new_request(&rec).await;
        let member = match wait(&self.home, &broker, &id, None).await? {
            Ok(rec) => rec,
            Err(outcome) => return Ok(outcome),
        };
        if !member_card::decided_by_member(&self.home, &member) { return Ok(ConfirmOutcome::Denied); }
        action_grant::verify_and_consume(&broker, &id, &grant).await.map_err(|_| "member grant invalid")?;
        let mut approval_ids = vec![id.to_string()];
        let mut decided_by = vec![member.decided_by.clone().ok_or("member decision missing")?];
        if request.approval_mode == ApprovalMode::MemberAndAdmin {
            // The existing mcp_call egress path is Admin-only, supports RM-143,
            // and retains the configured self-approval rule via ON_BEHALF_OF.
            let mut payload = json!({"name":request.tool, "native_google_confirmation":true,
                "drive_write_admin_confirmation":true,
                "arguments":{"request":request.arguments, "drive_snapshot":member.payload["drive_snapshot"],
                    "member_approval_id":id.as_str()},
                "egress_guard":{"reason":"drive_write_member_and_admin", "sources":[]}});
            let admin_grant = ActionGrant::build(&request.agent, &request.tool, &payload);
            payload["action_grant"] = admin_grant.to_json();
            let summary = format!("管理員確認雲端硬碟操作\n{}\n五分鐘內有效，請由管理員決定。", card_details(&member));
            crate::google_workspace::native_confirmation::ensure_approval_size(&payload, &summary)?;
            let turn = request.turn_id.as_deref().ok_or("turn missing")?;
            let session = crate::turn_context::session_for(&request.agent, turn).ok_or("session missing")?;
            let admin_id = crate::on_behalf_of::ON_BEHALF_OF.scope(member.on_behalf_of.clone(),
                crate::approval::scope_turn(&session, turn,
                    broker.request(&request.agent, "mcp_call", &summary, payload, 300))).await?;
            let admin = match wait(&self.home, &broker, &admin_id, Some(&member)).await? {
                Ok(rec) => rec,
                Err(outcome) => return Ok(outcome),
            };
            if !snapshot_current(&self.home, &member) { return Ok(ConfirmOutcome::Denied); }
            // The consumer verifies dashboard identity or the durable chat-admin
            // ledger in the same transaction that consumes the one-shot grant.
            action_grant::verify_and_consume(&broker, &admin_id, &admin_grant).await.map_err(|_| "admin grant invalid")?;
            approval_ids.push(admin_id.to_string());
            decided_by.push(admin.decided_by.ok_or("admin decision missing")?);
        }
        Ok(ConfirmOutcome::Approved { approval_ids, decided_by })
    }
}

async fn wait(home: &Path, broker: &ApprovalBroker, id: &ApprovalId, prior: Option<&ApprovalRecord>) -> Result<Result<ApprovalRecord, ConfirmOutcome>, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let rec = broker.get(id).await?.ok_or("confirmation missing")?;
        if tokio::time::Instant::now() >= deadline || rec.status == ApprovalStatus::Expired
            || !rec.expires_at().is_some_and(|until| chrono::Utc::now() < until)
            || rec.binding.as_ref().is_some_and(|b| !chrono::DateTime::parse_from_rfc3339(&b.expires_at)
                .is_ok_and(|until| chrono::Utc::now() < until)) {
            return Ok(Err(ConfirmOutcome::Expired));
        }
        if (member_card::is_member_card(&rec) && !member_card::eligible(home, &rec))
            || prior.is_some_and(|member| !snapshot_current(home, member)) {
            return Ok(Err(ConfirmOutcome::Denied));
        }
        match rec.status {
            ApprovalStatus::Approved => return Ok(Ok(rec)),
            ApprovalStatus::Pending => tokio::time::sleep(Duration::from_millis(250)).await,
            _ => return Ok(Err(ConfirmOutcome::Denied)),
        }
    }
}

fn snapshot_current(home: &Path, rec: &ApprovalRecord) -> bool {
    let Some(binding) = &rec.binding else { return false; };
    let s = &rec.payload["drive_snapshot"];
    let Some(tool) = rec.payload["name"].as_str() else { return false; };
    let Some(who) = crate::knowledge::turn_requesters::lookup(&rec.agent_id, &binding.run_id) else { return false; };
    let Ok(uid) = member(home, &rec.agent_id, &who) else { return false; };
    let Some(call) = s["call_id"].as_str() else { return false; };
    let turn = s["turn_id"].as_str().map(str::to_owned);
    let run = s["run_id"].as_str().map(str::to_owned);
    s["member"] == uid && s["agent"] == rec.agent_id && turn.as_deref() == Some(binding.run_id.as_str())
        && policy::action_for_tool(tool).is_some_and(|a| s["action"] == a.as_str())
        && duduclaw_core::HumanPrincipal::user(&uid).is_some_and(|p| rec.on_behalf_of.as_deref() == Some(p.to_string().as_str()))
        && crate::turn_context::target_for(&rec.agent_id, &binding.run_id)
            .map(|target| target.conversation_audience).unwrap_or(ConversationAudience::MultiParty) == binding.conversation_audience
        && crate::turn_context::decision_context_for(&rec.agent_id, &binding.run_id).as_ref() == Some(&binding.decision_context)
        && crate::approval::mail_requester::requester_user(home, &binding.decision_context, &rec.agent_id).as_deref() == Some(uid.as_str())
        && serve::confirmation_identity_matches(home, rec, &who, &turn, &run, call)
        && serve::confirmation_version(home, &rec.agent_id, tool, &rec.payload["arguments"], &who, &turn, &run, call)
            .is_ok_and(|version| s["version"] == version && binding.environment_hash == version)
        && crate::approval::policy_revision(home, &rec.agent_id).ok().as_deref() == Some(binding.policy_revision.as_str())
}
pub(crate) fn eligible(home: &Path, rec: &ApprovalRecord) -> bool {
    let Some(binding) = &rec.binding else { return false; };
    let Some(tool) = rec.payload["name"].as_str() else { return false; };
    rec.payload["drive_write_member_confirmation"] == true && rec.payload["chat_member_confirmation"] != true
        && rec.request_kind == RequestKind::Approval && rec.action_kind == "bound_action"
        && binding.actor_principal == rec.agent_id && binding.run_origin_kind == "ingress"
        && binding.resume_handler == "live_only_v1" && rec.ttl_seconds > 0 && rec.ttl_seconds <= 300
        && rec.expires_at().is_some_and(|until| chrono::Utc::now() < until)
        && binding.validate(&rec.payload).is_ok() && snapshot_current(home, rec)
        && ActionGrant::build(&rec.agent_id, tool, &rec.payload).matches_stored(&rec.payload["action_grant"])
}
pub(crate) fn may_decide(home: &Path, rec: &ApprovalRecord, uid: &str) -> bool {
    rec.payload["drive_snapshot"]["member"].as_str() == Some(uid) && eligible(home, rec)
}
pub(crate) fn decided_by_member(home: &Path, rec: &ApprovalRecord) -> bool {
    if !eligible(home, rec) { return false; }
    let Some(binding) = &rec.binding else { return false; };
    let Some(uid) = rec.payload["drive_snapshot"]["member"].as_str() else { return false; };
    rec.decided_by.as_deref() == Some(format!("dashboard:{uid}").as_str())
        || (binding.conversation_audience == ConversationAudience::Private
            && rec.decided_by.as_deref() == Some(format!("{}:{}", binding.decision_context.channel, binding.decision_context.principal_id).as_str()))
}

pub(crate) const GROUP_NOTICE: &str = "已傳到你的私訊或儀表板";
/// Display-only values; never use this projection for grants or Google I/O.
pub(crate) fn confirmation_display(rec: &ApprovalRecord) -> Value {
    use duduclaw_security::credential_protection::sanitize_output_text;
    let s = &rec.payload["drive_snapshot"];
    let mut display = json!({"id":rec.id.as_str(), "expires_at":rec.binding.as_ref().map(|b| &b.expires_at)});
    for key in ["agent", "account", "action", "file_id", "file_name", "destination", "destination_name", "size",
        "preview", "attachment_name", "rename_before", "rename_after", "parents_before", "source_names"] {
        let value = if key == "preview" {
            s[key].as_str().map(|v| json!(duduclaw_core::truncate_chars(v, 500))).unwrap_or(Value::Null)
        } else { s[key].clone() };
        display[key] = json!(sanitize_output_text(&crate::google_workspace::native_confirmation::escaped(&value)));
    }
    display["employee"] = json!(sanitize_output_text(&crate::google_workspace::native_confirmation::escaped(&json!(rec.agent_id))));
    display
}
fn card_details(rec: &ApprovalRecord) -> String {
    let display = confirmation_display(rec);
    let action = match rec.payload["drive_snapshot"]["action"].as_str() {
        Some("create") => "建立檔案", Some("modify") => "修改檔案", Some("move") => "搬移檔案",
        Some("trash") => "移到垃圾桶", _ => "無法確認動作",
    };
    let mut body = format!("確認雲端硬碟操作\n動作：{action}\n");
    for (key, label) in [("employee", "AI 員工名稱"), ("account", "Google 帳號"), ("file_name", "檔名"),
        ("destination_name", "目標資料夾"), ("destination", "目標資料夾代碼"), ("size", "大小（位元組）"),
        ("preview", "內容預覽（前 500 字）"), ("attachment_name", "附件"), ("rename_before", "改名前"),
        ("rename_after", "改名後"), ("source_names", "搬移來源資料夾"), ("parents_before", "搬移來源資料夾代碼"), ("destination_name", "搬移目的資料夾")] {
        if matches!(key, "source_names" | "parents_before") && rec.payload["drive_snapshot"]["action"] != "move" { continue; }
        if label == "搬移目的資料夾" && rec.payload["drive_snapshot"]["action"] != "move" { continue; }
        if key == "employee" || !rec.payload["drive_snapshot"][key].is_null() {
            body.push_str(&format!("{label}：{}\n", display[key].as_str().unwrap_or("")));
        }
    }
    body
}
pub(crate) fn card_body(rec: &ApprovalRecord) -> String {
    format!("{}五分鐘內有效，僅限本人決定。\n確認 {}\n取消 {}", card_details(rec), rec.id, rec.id)
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
