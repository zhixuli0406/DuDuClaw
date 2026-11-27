//! Bound confirmation creation and display-only projections; never executes settings.
use std::path::Path;
use duduclaw_security::credential_protection::sanitize_output_text;
use rand::{distributions::Alphanumeric, rngs::OsRng, Rng};
use serde_json::{json, Value};
use crate::{approval::{action_grant::ActionGrant, ApprovalBroker, ApprovalId, ApprovalRecord,
    ConversationAudience, ExecutionBinding, payload_hash, policy_revision}, on_behalf_of::admin_turn::AdminTurn};
use super::proposal::{Prepared, Result, refused, hash_text};

pub(crate) const ACTION_KIND: &str = "admin_ai_settings";
const NOTICE: &str = "已送出確認卡，請到儀表板收件匣確認；十分鐘內有效。";
const RESULT_MESSAGE: &str = "已送出確認卡，請到儀表板收件匣確認；十分鐘內有效";

pub(super) fn tool_result(count: usize) -> Value { json!({"message": RESULT_MESSAGE, "item_count": count}) }
pub(crate) fn check_code_hash(id: &str, code: &str) -> String {
    hash_text(&format!("admin-ai-check-code:{id}:{code}"))
}
fn display(value: &Value) -> Value {
    match value {
        Value::String(s) => json!(sanitize_output_text(s)),
        Value::Array(a) => Value::Array(a.iter().map(display).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (sanitize_output_text(k), display(v))).collect()),
        _ => value.clone(),
    }
}

/// Dashboard consumers must use this projection, never the raw binding arguments.
/// The ephemeral check code is deliberately absent from the stored payload.
pub(crate) fn confirmation_display(rec: &ApprovalRecord) -> Value {
    let items: Vec<Value> = rec.payload["items"].as_array().into_iter().flatten().map(|item|
        display(&json!({"seq": item["seq"], "target": item["target"], "employee": item["employee"],
            "label": item["label"], "before": item["before"], "after": item["after"]}))).collect();
    display(&json!({"id": rec.id.as_str(), "admin": rec.payload["admin_name"], "avatar": rec.payload["avatar_name"],
        "employee_count": rec.payload["employee_count"], "item_count": items.len(), "items": items,
        "reason": rec.payload["reason"], "expires_at": rec.binding.as_ref().map(|b| &b.expires_at)}))
}
fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "未設定".into(),
        Value::Bool(v) => if *v { "開啟" } else { "關閉" }.into(),
        Value::String(s) => sanitize_output_text(s),
        Value::Object(m) => {
            // Schedule prompts are shown in full, including their line breaks.
            let labels = [("name", "名稱"), ("cron", "時間"), ("timezone", "時區"), ("enabled", "啟用"), ("task", "排程提示全文")];
            labels.into_iter().filter_map(|(key, label)| m.get(key).map(|v| format!("{label}：{}", value_text(v))))
                .collect::<Vec<_>>().join("\n")
        }
        _ => sanitize_output_text(&value.to_string()),
    }
}
pub(crate) fn card_body(rec: &ApprovalRecord, code: Option<&str>) -> String {
    let d = confirmation_display(rec);
    let text = |key: &str| d[key].as_str().unwrap_or("");
    let mut body = format!("管理員 AI 設定變更確認\n提出：{}，透過「{}」\n共 {} 位 AI 員工、{} 項變更：\n",
        text("admin"), text("avatar"), d["employee_count"], d["item_count"]);
    for item in d["items"].as_array().into_iter().flatten() {
        body.push_str(&format!("{}. {}：{}　{} → {}\n", item["seq"],
            item["employee"].as_str().unwrap_or(""), item["label"].as_str().unwrap_or(""),
            value_text(&item["before"]), value_text(&item["after"])));
    }
    body.push_str(&format!("說明：{}\n十分鐘內有效，只有你本人能確認。確認前任何一位員工的設定被改過，整批不執行。", text("reason")));
    if let Some(code) = code {
        let code = sanitize_output_text(code);
        body.push_str(&format!("\n檢查碼：{code}\n確認 {} {code}\n取消 {}", text("id"), text("id")));
    }
    body
}
fn channel_body(rec: &ApprovalRecord, code: &str) -> String {
    let _ = code; // Reserved for the immutable proposal contract; never authorize free text.
    let body = format!("{}\n請到儀表板收件匣確認或取消。", card_body(rec, None));
    if body.chars().count() <= 1800 { body } else { NOTICE.into() }
}

pub(super) async fn create(home: &Path, turn: &AdminTurn, mut prepared: Prepared, executor: Option<super::execute::Executor>) -> Result<Value> {
    #[cfg(not(test))]
    if executor.is_none() { return Err(refused("admin_ai_execution_unavailable")); }
    if turn.audience != ConversationAudience::Private { return Err(refused("admin_ai_private_audience_required")); }
    let context = turn.decision_context.clone().ok_or_else(|| refused("admin_ai_invalid_context"))?;
    let target = if turn.channel == "webchat" { None } else {
        let target = crate::turn_context::target_for(&turn.agent, &turn.turn_id)
            .filter(|t| t.context == context && t.conversation_audience == ConversationAudience::Private)
            .ok_or_else(|| refused("admin_ai_invalid_context"))?;
        Some(target)
    };
    // Show the exact execution order: one atomic field update per employee,
    // followed by schedule/skill operations. Never execute a later displayed item early.
    super::execute::order_items(&mut prepared.items);
    let id = ApprovalId::new();
    let code: String = OsRng.sample_iter(&Alphanumeric).take(4).map(char::from).collect();
    let count = prepared.items.len();
    // ActionGrant hashes `arguments`, not arbitrary top-level payload fields.
    // Bind a complete copy of the immutable proposal contract there as well.
    let arguments = json!({"items": prepared.items, "target_hashes": prepared.target_hashes,
        "admin_user_id": turn.user_id, "session_id": turn.session_id, "reason": prepared.reason,
        "check_code_hash": check_code_hash(id.as_str(), &code)});
    let mut payload = arguments.clone();
    payload["kind"] = json!(ACTION_KIND);
    payload["name"] = json!("admin_settings_propose");
    payload["arguments"] = arguments;
    payload["admin_name"] = json!(prepared.admin_name);
    payload["avatar_name"] = json!(prepared.avatar_name);
    payload["employee_count"] = json!(prepared.employee_count);
    payload["action_grant"] = ActionGrant::build(&turn.agent, "admin_settings_propose", &payload).to_json();
    let binding = ExecutionBinding { schema_version: 1, run_id: turn.turn_id.clone(), run_origin_kind: "ingress".into(),
        actor_principal: turn.agent.clone(), decision_context: context, conversation_audience: turn.audience,
        task_id: None, task_revision: None, task_snapshot_hash: None, payload_hash: payload_hash(&payload),
        policy_revision: policy_revision(home, &turn.agent).map_err(|_| refused("admin_ai_policy_unavailable"))?,
        cwd: None, environment_hash: prepared.environment_hash, file_hashes: Default::default(),
        expires_at: (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339(),
        resume_handler: "live_only_v1".into(), resume_version: 1 };
    let broker = ApprovalBroker::open(home).map_err(|_| refused("admin_ai_approval_unavailable"))?;
    let principal = duduclaw_core::HumanPrincipal::user(&turn.user_id)
        .ok_or_else(|| refused("admin_ai_invalid_context"))?.to_string();
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), crate::approval::scope_turn(
        &turn.session_id, &turn.turn_id, broker.request_admin_ai(id, &turn.agent, payload, binding))).await
        .map_err(|e| refused(match e.as_str() {
            "admin_ai_pending_limit" => "admin_ai_pending_limit",
            "admin_ai_hourly_limit" => "admin_ai_hourly_limit",
            _ => "admin_ai_approval_unavailable",
        }))?;
    let rec = broker.get(&id).await.map_err(|_| refused("admin_ai_approval_unavailable"))?
        .ok_or_else(|| refused("admin_ai_approval_unavailable"))?;
    let audit_text = |s: &str| duduclaw_core::truncate_chars(&sanitize_output_text(s), 200).to_owned();
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        "admin_ai_settings_proposed", &audit_text(&turn.agent), duduclaw_security::audit::Severity::Info,
        json!({"approval_id": audit_text(id.as_str()), "admin_user_id": audit_text(&turn.user_id),
            "agent": audit_text(&turn.agent), "session_id": audit_text(&turn.session_id),
            "turn_id": audit_text(&turn.turn_id), "channel": audit_text(&turn.channel),
            "audience": turn.audience, "item_count": count, "employee_count": prepared.employee_count})));
    if let Some(executor) = executor {
        let webchat = crate::turn_context::webchat_origin_for(&turn.agent, &turn.turn_id)
            .and_then(|origin| crate::scheduled_webchat::bind_origin(home, Some(&origin), &turn.agent));
        super::execute::spawn(home.to_owned(), executor, rec.clone(), target.clone(), webchat);
    }
    if let Some(target) = target {
        // Failure leaves the already-persisted inbox card available. Never fan out.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15),
            crate::notify_push::push_exact_confirmation(&target, &channel_body(&rec, &code))).await;
    } else if let Some(origin) = crate::turn_context::webchat_origin_for(&turn.agent, &turn.turn_id) {
        // Use only the host-captured WebChat origin and its checked owner binding.
        // No approval id, digest or check code enters the conversation history.
        if let Some(target) = crate::scheduled_webchat::bind_origin(home, Some(&origin), &turn.agent) {
            let _ = crate::scheduled_webchat::deliver(home, &turn.agent, &target, NOTICE).await;
        }
    }
    // The same one-line result remains available if the transport is unavailable.
    Ok(tool_result(count))
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
