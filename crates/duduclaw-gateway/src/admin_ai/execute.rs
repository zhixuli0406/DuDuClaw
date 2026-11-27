//! Live-only, single-use execution of the immutable administrator settings card.
use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, sync::Arc, time::Duration};
use duduclaw_auth::UserContext;
use duduclaw_core::admin_ai_settings::{AdminAiField, Item, Schedule};
use serde_json::{json, Value};
use crate::{approval::{action_grant::{self, ActionGrant}, ApprovalBroker, ApprovalRecord,
    ApprovalStatus, TrustedReplyTarget}, handlers::{MethodHandler, agents_update::UpdateOrigin},
    on_behalf_of::admin_turn::AdminTurn, protocol::WsFrame};
use super::{card::ACTION_KIND, proposal::{environment, hash_text, hash_schedule, hash_skills, hash_triggers, trigger_namespace, STALE}, route};

// AppState owns the existing handler. Keeping its Arc avoids constructing a
// second registry/scheduler and does not require MethodHandler to be Clone.
pub(crate) type Executor = Arc<dyn AsRef<MethodHandler> + Send + Sync>;
static BATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const STALE_MESSAGE: &str = "設定在確認後被其他人改過，這項變更沒有儲存，請重新提出。";

#[cfg(test)]
tokio::task_local! {
    static AFTER_REVERIFY: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
}

pub(super) fn order_items(items: &mut Vec<Value>) {
    let original = std::mem::take(items);
    let mut agents = BTreeSet::new();
    for entry in &original {
        if entry["item"]["kind"] == "agent_field" {
            if let Some(target) = entry["target"].as_str().filter(|id| agents.insert((*id).to_owned())) {
                items.extend(original.iter().filter(|v|
                    v["item"]["kind"] == "agent_field" && v["target"] == target).cloned());
            }
        }
    }
    items.extend(original.into_iter().filter(|v| v["item"]["kind"] != "agent_field"));
    for (index, entry) in items.iter_mut().enumerate() { entry["seq"] = json!(index + 1); }
}

pub(super) fn spawn(home: PathBuf, executor: Executor, rec: ApprovalRecord,
    target: Option<TrustedReplyTarget>, webchat: Option<String>) {
    tokio::spawn(async move {
        let result = wait(&home, executor.as_ref().as_ref(), &rec).await;
        if let Some(result) = result {
            let message = result["message"].as_str().unwrap_or("這批變更沒有執行，請重新提出。");
            // Use the original host-captured destination even after the turn ends.
            let delivered = tokio::time::timeout(Duration::from_secs(15), async {
                if let Some(target) = target {
                    crate::notify_push::push_exact_confirmation(&target, message).await.is_some()
                } else if let Some(target) = webchat {
                    crate::scheduled_webchat::deliver(&home, &rec.agent_id, &target, message).await.is_ok()
                } else { false }
            }).await.unwrap_or(false);
            if !delivered {
                tracing::warn!(approval_id = %rec.id, "Admin AI result delivery unavailable; result retained in security audit");
            }
        }
    });
}

async fn wait(home: &Path, handler: &MethodHandler, initial: &ApprovalRecord) -> Option<Value> {
    let broker = match ApprovalBroker::open(home) {
        Ok(broker) => broker,
        Err(_) => return Some(void_audit(home, initial, "stale")),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    loop {
        let rec = match broker.get(&initial.id).await {
            Ok(Some(rec)) => rec,
            _ => return Some(void_audit(home, initial, "stale")),
        };
        if rec.status == ApprovalStatus::Approved {
            return execute_once(home, handler, &broker, &rec).await;
        }
        if rec.status == ApprovalStatus::Pending {
            if expired(&rec) || tokio::time::Instant::now() >= deadline {
                let _ = broker.invalidate_request(&rec.id, "admin_ai:expired").await;
                return Some(void_audit(home, &rec, "expired"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        if consumed(&rec) { return None; }
        let reason = if rec.status == ApprovalStatus::Expired || expired(&rec) { "expired" }
            else if rec.invalidated_reason.as_deref() == Some("restart_requires_reobserve") { "restart" }
            else { "denied" };
        return Some(void_audit(home, &rec, reason));
    }
}
fn consumed(rec: &ApprovalRecord) -> bool {
    rec.invalidated_reason.as_deref().is_some_and(|s| s.starts_with(action_grant::CONSUMED_PREFIX))
}
fn expired(rec: &ApprovalRecord) -> bool {
    rec.expires_at_epoch().is_none_or(|end| chrono::Utc::now().timestamp() >= end)
        || rec.binding.as_ref().is_none_or(|b| chrono::DateTime::parse_from_rfc3339(&b.expires_at)
            .map_or(true, |end| chrono::Utc::now() >= end))
}
fn safe(s: &str) -> String {
    duduclaw_core::truncate_chars(&duduclaw_security::credential_protection::sanitize_output_text(s), 200).to_owned()
}
fn audit_value(v: &Value) -> Value {
    match v {
        Value::String(s) => json!(safe(s)),
        Value::Array(a) => Value::Array(a.iter().map(audit_value).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| {
            // Schedule prompts must never be copied into the security log.
            if k == "task" { ("task_sha256".into(), json!(hash_text(v.as_str().unwrap_or("")))) }
            else { (safe(k), audit_value(v)) }
        }).collect()),
        _ => v.clone(),
    }
}
fn metadata(rec: &ApprovalRecord) -> Value {
    let binding = rec.binding.as_ref();
    audit_value(&json!({"approval_id": rec.id.as_str(), "admin_user_id": rec.payload["arguments"]["admin_user_id"],
        "agent": rec.agent_id, "session_id": rec.payload["arguments"]["session_id"],
        "turn_id": binding.map(|b| &b.run_id), "channel": binding.map(|b| &b.decision_context.channel),
        "audience": binding.map(|b| b.conversation_audience), "item_count": rec.payload["items"].as_array().map(Vec::len),
        "employee_count": rec.payload["employee_count"],
        "decided_via": if rec.decided_by.as_deref().is_some_and(|s| s.starts_with("dashboard:")) { "dashboard" } else { "channel" },
        "decided_at": rec.decided_at, "executed_at": chrono::Utc::now().to_rfc3339()}))
}
fn emit(home: &Path, rec: &ApprovalRecord, event: &str, details: Value) {
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        event, &safe(&rec.agent_id), duduclaw_security::audit::Severity::Info, audit_value(&details)));
}
fn void_audit(home: &Path, rec: &ApprovalRecord, reason: &str) -> Value {
    void_audit_for(home, rec, reason, None)
}
fn void_audit_for(home: &Path, rec: &ApprovalRecord, reason: &str, employee: Option<&str>) -> Value {
    let employee = employee.map(safe).unwrap_or_else(|| "員工".into());
    let message = match reason {
        "stale" => format!("{employee} 的設定在確認前被改過，這批變更都沒有執行。"),
        "admin_changed" => "管理員身分或分身已變更，這批變更都沒有執行。".into(),
        "disabled" => "管理員 AI 設定已關閉，這批變更都沒有執行。".into(),
        "expired" => "確認卡已逾期，這批變更都沒有執行。".into(),
        "restart" => "系統已重新啟動，這批變更都沒有執行，請重新提出。".into(),
        _ => "已取消這批變更，沒有執行任何項目。".into(),
    };
    let mut result = metadata(rec);
    result["outcome"] = json!("voided");
    result["reason"] = json!(reason);
    result["message"] = json!(message);
    result["items"] = json!(item_reports(rec));
    emit(home, rec, "admin_ai_settings_voided", result.clone());
    result
}

struct VoidReason {
    reason: &'static str,
    employee: Option<String>,
}
impl From<&'static str> for VoidReason {
    fn from(reason: &'static str) -> Self { Self { reason, employee: None } }
}

async fn reverify(home: &Path, rec: &ApprovalRecord) -> Result<UserContext, VoidReason> {
    let user = rec.payload["arguments"]["admin_user_id"].as_str().ok_or("admin_changed")?;
    if !super::may_decide(home, rec, user) { return Err("admin_changed".into()); }
    let ctx = route::fresh_user(home, user).map_err(|_| "admin_changed")?;
    if !duduclaw_core::admin_ai_settings::enabled_from_home(home) { return Err("disabled".into()); }
    if duduclaw_core::execution_gate::isolation_required() { return Err("disabled".into()); }
    if expired(rec) { return Err("expired".into()); }
    let binding = rec.binding.as_ref().ok_or("stale")?;
    binding.validate(&rec.payload).map_err(|_| "stale")?;
    for key in ["items", "target_hashes", "admin_user_id", "session_id", "reason", "check_code_hash"] {
        if rec.payload.get(key).is_none() || rec.payload[key] != rec.payload["arguments"][key] { return Err("stale".into()); }
    }
    let turn = AdminTurn { user_id: user.into(), agent: rec.agent_id.clone(),
        session_id: rec.payload["arguments"]["session_id"].as_str().ok_or("stale")?.into(),
        turn_id: binding.run_id.clone(), channel: binding.decision_context.channel.clone(),
        audience: binding.conversation_audience, decision_context: Some(binding.decision_context.clone()) };
    if environment(home, &turn).map_err(|_| "stale")?.0 != binding.environment_hash
        || crate::approval::policy_revision(home, &rec.agent_id).map_err(|_| "stale")? != binding.policy_revision {
        return Err("stale".into());
    }
    let hashes = rec.payload["target_hashes"].as_object().ok_or("stale")?;
    let items = rec.payload["items"].as_array().filter(|v| !v.is_empty()).ok_or("stale")?;
    // A missing target entry must never bypass the snapshot comparison.
    let mut required = BTreeSet::new();
    for (index, entry) in items.iter().enumerate() {
        if entry["seq"].as_u64() != Some((index + 1) as u64) { return Err("stale".into()); }
        let target = entry["target"].as_str().ok_or("stale")?;
        let item: Item = serde_json::from_value(entry["item"].clone()).map_err(|_| "stale")?;
        if item.agent_id().is_some_and(|id| id != target) { return Err("stale".into()); }
        required.insert(format!("agent:{target}"));
        required.insert(format!("agent_raw:{target}"));
        if matches!(item, Item::Skill { .. }) { required.insert(format!("skills:{target}")); }
        if matches!(item, Item::AgentField { field: AdminAiField::Trigger | AdminAiField::DisplayName, .. }) {
            required.insert("triggers".into());
        }
        if let Some(id) = item.schedule_id() { required.insert(format!("schedule:{id}")); }
    }
    if required != hashes.keys().cloned().collect() { return Err("stale".into()); }
    let store = crate::cron_store::CronStore::open(home).map_err(|_| "stale")?;
    for (key, expected) in hashes {
        if key == "triggers" {
            let actual = hash_triggers(&trigger_namespace(home).map_err(|_| "stale")?);
            if expected.as_str() != Some(actual.as_str()) { return Err("stale".into()); }
            continue;
        }
        let (kind, id) = key.split_once(':').ok_or("stale")?;
        let actual = match kind {
            "agent" => {
                let dir = home.join("agents").join(id);
                let text = route::effective_text(&dir).map_err(|_| "stale")?;
                let table = text.parse().map_err(|_| "stale")?;
                if !route::target_allowed(home, &ctx, id, &dir, &table) { return Err("stale".into()); }
                hash_text(&text)
            }
            "agent_raw" => hash_text(&std::fs::read_to_string(home.join("agents").join(id).join("agent.toml"))
                .map_err(|_| "stale")?),
            "skills" => {
                let state = duduclaw_agent::skill_state::read_disabled(home, id);
                if state.error().is_some() { return Err("stale".into()); }
                hash_skills(&state).map_err(|_| "stale")?
            }
            "schedule" => {
                let row = store.get(id).await.map_err(|_| "stale")?.ok_or("stale")?;
                if !route::prompt_only(&row) || store.workflow_binding(id).await.map_err(|_| "stale")?.is_some()
                    || items.iter().any(|v| v["item"]["schedule_id"] == id && v["target"] != row.agent_id) {
                    return Err("stale".into());
                }
                hash_schedule(&row).map_err(|_| "stale")?
            }
            _ => return Err("stale".into()),
        };
        if expected.as_str() != Some(actual.as_str()) {
            let entry = items.iter().find(|v| if kind == "schedule" {
                v["item"]["schedule_id"] == id
            } else { v["target"] == id });
            return Err(VoidReason { reason: "stale", employee: entry
                .and_then(|v| v["employee"].as_str()).map(str::to_owned) });
        }
    }
    Ok(ctx)
}

fn field_param(field: AdminAiField) -> &'static str {
    use AdminAiField::*;
    match field {
        DisplayName => "display_name", Icon => "icon", Trigger => "trigger", Status => "status",
        ReplyLanguage => "reply_language", Model => "preferred", FallbackModel => "fallback",
        Effort => "model.effort", TimeoutSecs => "runtime.timeout_secs", MaxTurns => "runtime.max_turns",
        IdleTimeoutSecs => "runtime.idle_timeout_secs", HeartbeatEnabled => "heartbeat_enabled",
        HeartbeatCron => "heartbeat_cron", HeartbeatInterval => "heartbeat_interval",
        HeartbeatCronTimezone => "heartbeat_cron_timezone",
    }
}
fn frame_result(frame: WsFrame) -> Result<(), &'static str> {
    match frame {
        WsFrame::Response { error: Some(error), .. } if error.as_str() == Some(STALE) => Err(STALE_MESSAGE),
        WsFrame::Response { ok: true, payload, .. } => {
            if payload.as_ref().and_then(|v| v["results"].as_array())
                .is_some_and(|rows| rows.iter().any(|v| v["ok"] != true)) {
                Err("技能設定未能儲存，請到儀表板檢查。")
            } else { Ok(()) }
        }
        WsFrame::Response { error, .. } if error.as_ref().is_some_and(|e|
            e.as_str() == Some(duduclaw_core::admin_ai_settings::DIFF_REFUSED)) =>
            Err("這項修改會連帶改到未核准的設定，已停止。"),
        _ => Err("設定未能完成，請到儀表板檢查；已儲存的部分不會自動還原。"),
    }
}
async fn execute_item(handler: &MethodHandler, ctx: &UserContext, item: Item,
    hashes: &mut BTreeMap<String, String>) -> Result<(), &'static str> {
    let frame = match item {
        Item::Skill { agent_id, skill, enabled } => {
            let key = format!("skills:{agent_id}");
            let expected = hashes.get(&key).ok_or(STALE_MESSAGE)?;
            let frame = handler.handle_admin_ai_skills_set_enabled(
                json!({"items":[{"agent_id":agent_id,"skill_name":skill}],"enabled":enabled}), ctx, expected).await;
            let next_hash = match &frame {
                WsFrame::Response { ok: true, payload: Some(payload), .. } =>
                    payload["state_hash"].as_str().map(str::to_owned),
                _ => None,
            };
            frame_result(frame)?;
            // Advance only to the state returned by our own locked commit.
            hashes.insert(key, next_hash.ok_or(STALE_MESSAGE)?);
            return Ok(());
        },
        Item::Schedule(op) => {
            let expected = item_schedule_hash(&op, hashes)?;
            let (method, params) = match op {
                Schedule::Create { agent_id, name, cron, timezone, task } => ("cron.add",
                    json!({"agent_id":agent_id,"name":name,"cron":cron,"cron_timezone":timezone,"task":task})),
                Schedule::Update { schedule_id, name, cron, timezone, task } => {
                    let mut params = json!({"id":schedule_id});
                    for (key, value) in [("name",name),("cron",cron),("cron_timezone",timezone),("task",task)] {
                        if let Some(value) = value { params[key] = json!(value); }
                    }
                    ("cron.update", params)
                }
                Schedule::Pause { schedule_id } => ("cron.pause", json!({"id":schedule_id})),
                Schedule::Resume { schedule_id } => ("cron.resume", json!({"id":schedule_id})),
                Schedule::Delete { schedule_id } => ("cron.remove", json!({"id":schedule_id})),
            };
            handler.handle_admin_ai_cron(method, params, ctx, expected.as_deref()).await
        }
        Item::AgentField { .. } => return Err("設定項目無法辨識，已停止。"),
    };
    frame_result(frame)
}
fn item_schedule_hash(op: &Schedule, hashes: &BTreeMap<String, String>) -> Result<Option<String>, &'static str> {
    let id = match op {
        Schedule::Create { .. } => return Ok(None),
        Schedule::Update { schedule_id, .. } | Schedule::Pause { schedule_id }
        | Schedule::Resume { schedule_id } | Schedule::Delete { schedule_id } => schedule_id,
    };
    hashes.get(&format!("schedule:{id}")).cloned().map(Some).ok_or(STALE_MESSAGE)
}

fn item_reports(rec: &ApprovalRecord) -> Vec<Value> {
    rec.payload["items"].as_array().into_iter().flatten().map(|v| {
        let item = &v["item"];
        let mut report = json!({"seq":v["seq"],"target":v["target"],"before":audit_value(&v["before"]),
            "after":audit_value(&v["after"]),"result":"not_executed","error":null});
        if item["kind"] == "agent_field" { report["field"] = item["field"].clone(); }
        else { report["op"] = if item["kind"] == "skill" { json!("skills.set_enabled") } else { item["op"].clone() }; }
        if item["kind"] == "skill" { report["skill"] = item["skill"].clone(); }
        audit_value(&report)
    }).collect()
}

/// None means a replay/lost claim: neither execute nor emit a second outcome.
async fn execute_once(home: &Path, handler: &MethodHandler, broker: &ApprovalBroker, requested: &ApprovalRecord) -> Option<Value> {
    let rec = broker.get(&requested.id).await.ok().flatten()?;
    if rec.status != ApprovalStatus::Approved || rec.action_kind != ACTION_KIND { return None; }
    let grant = ActionGrant::build(&rec.agent_id, "admin_settings_propose", &rec.payload);
    if action_grant::verify_and_consume(broker, &rec.id, &grant).await.is_err() {
        let latest = broker.get(&rec.id).await.ok().flatten()?;
        if consumed(&latest) { return None; }
        let reason = if expired(&rec) { "expired" }
            else if !super::may_decide(home, &rec, rec.payload["admin_user_id"].as_str().unwrap_or("")) { "admin_changed" }
            else { "stale" };
        let _ = broker.invalidate_request(&rec.id, reason).await;
        return Some(void_audit(home, &rec, reason));
    }
    let lock = BATCH_LOCK.lock().await;
    let ctx = match reverify(home, &rec).await {
        Ok(ctx) => ctx,
        Err(reason) => {
            drop(lock);
            return Some(void_audit_for(home, &rec, reason.reason, reason.employee.as_deref()));
        }
    };
    #[cfg(test)]
    if let Ok((ready, proceed)) = AFTER_REVERIFY.try_with(Clone::clone) {
        ready.notify_one();
        proceed.notified().await;
    }
    let mut write_hashes: BTreeMap<String, String> = serde_json::from_value(rec.payload["target_hashes"].clone()).ok()?;
    let entries = rec.payload["items"].as_array()?;
    let mut reports = item_reports(&rec);
    let mut index = 0;
    let mut completed = 0;
    let mut failed = None;
    let mut updated_agents = BTreeSet::new();
    while index < entries.len() {
        let mut end = index + 1;
        let item = serde_json::from_value::<Item>(entries[index]["item"].clone()).ok()?;
        let result = if let Item::AgentField { ref agent_id, .. } = item {
            let mut params = json!({"agent_id":agent_id});
            while end < entries.len() && entries[end]["item"]["kind"] == "agent_field"
                && entries[end]["target"] == *agent_id { end += 1; }
            for entry in &entries[index..end] {
                if let Ok(Item::AgentField { field, value, .. }) = serde_json::from_value(entry["item"].clone()) {
                    params[field_param(field)] = value;
                    if field == AdminAiField::DisplayName {
                        if let Some(trigger) = entry.get("trigger_override") { params["trigger"] = trigger.clone(); }
                    }
                }
            }
            if !updated_agents.insert(agent_id.clone()) { Err("同一員工的設定順序不一致，已停止。") }
            else { frame_result(handler.agents_update_inner(params, Some(&ctx), Some(&ctx),
                UpdateOrigin::AdminAi { approval_id: rec.id.to_string(),
                    expected_snapshot: write_hashes.get(&format!("agent:{agent_id}"))
                        .zip(write_hashes.get(&format!("agent_raw:{agent_id}")))
                        .map(|(effective_hash, raw_hash)| crate::agent_files::writer::AdminAiSnapshot {
                            effective_hash: effective_hash.clone(), raw_hash: raw_hash.clone(),
                        }) }).await) }
        } else { execute_item(handler, &ctx, item, &mut write_hashes).await };
        match result {
            Ok(()) => {
                for report in &mut reports[index..end] { report["result"] = json!("complete"); }
                completed += end - index;
            }
            Err(error) => {
                // One grouped employee write is atomic. Remaining fields in
                // that call also failed; no later handler is invoked.
                for report in &mut reports[index..end] {
                    report["result"] = json!("failed"); report["error"] = json!(safe(error));
                }
                failed = Some((index + 1, end - index, error));
                break;
            }
        }
        index = end;
    }
    drop(lock); // Never hold the batch mutex during notification delivery.
    let mut result = metadata(&rec);
    let stale = failed.as_ref().is_some_and(|(_, _, error)| *error == STALE_MESSAGE);
    let voided = stale && completed == 0;
    result["outcome"] = json!(if voided { "voided" } else if failed.is_some() { "partial" } else { "complete" });
    if stale { result["reason"] = json!("stale"); }
    result["items"] = json!(reports);
    result["completed"] = json!(completed);
    let remaining = reports.iter().filter(|v| v["result"] == "not_executed").count();
    result["not_executed"] = json!(remaining);
    result["message"] = json!(match failed {
        Some((seq, count, error)) => format!("已完成 {completed} 項；第 {seq} 項失敗：{error}{}；其餘 {remaining} 項沒有執行。",
            if count > 1 { format!("同位員工合併的 {count} 項均未完成") } else { String::new() }),
        None => format!("已完成 {completed} 項設定變更。"),
    });
    emit(home, &rec, if voided { "admin_ai_settings_voided" } else { "admin_ai_settings_changed" }, result.clone());
    Some(result)
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
