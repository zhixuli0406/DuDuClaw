//! RM-141 dedicated write-back: caller text never becomes markup or recipients.
use super::*;
use super::odoo_actor::OdooActor;
use super::odoo_access::{ToolPolicy, error_text, follows, identity_suffix};
use super::odoo_identity_client::{self, OdooIdentity};
use duduclaw_odoo::OdooConnector;
use serde_json::json;
use std::sync::Arc;

const TEXT_LIMIT: usize = 10_000;

fn plain_text(params: &Value, key: &str) -> std::result::Result<String, String> {
    let text = params.get(key).and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be plain text"))?;
    // Same Unicode scalar counting as truncate_chars, but never truncate.
    if text.chars().take(TEXT_LIMIT + 1).count() > TEXT_LIMIT {
        return Err(format!("{key} exceeds 10,000 characters"));
    }
    Ok(text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;"))
}

fn positive_id(params: &Value, key: &str) -> std::result::Result<i64, String> {
    params.get(key).and_then(Value::as_i64).filter(|n| *n > 0)
        .ok_or_else(|| format!("{key} must be a positive integer"))
}

struct Writeback {
    model: String,
    res_id: i64,
    method: &'static str,
    kwargs: Value,
}

fn parse(tool: &str, params: &Value, follow: bool) -> std::result::Result<Writeback, String> {
    let activity = tool == "odoo_activity_create";
    if !matches!(tool, "odoo_activity_create" | "odoo_note_post" | "odoo_message_send") {
        return Err("Unknown Odoo write-back tool".into());
    }
    let object = params.as_object().ok_or("Arguments must be an object")?;
    let allowed: &[&str] = if activity {
        &["model", "res_id", "summary", "note", "date_deadline", "activity_type_xmlid", "user_id"]
    } else { &["model", "res_id", "body"] };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Unexpected Odoo write-back argument".into());
    }
    let model = params.get("model").and_then(Value::as_str).ok_or("model is required")?;
    if !duduclaw_odoo::schema::valid_model_name(model) {
        return Err("Invalid Odoo model name".into());
    }
    let res_id = positive_id(params, "res_id")?;
    let (method, mut kwargs) = if activity {
        if !follow && params.get("user_id").is_some() {
            return Err("引擎模式不接受 user_id；活動只能指派給連線帳號本人。".into());
        }
        let xmlid = match params.get("activity_type_xmlid") {
            None => "mail.mail_activity_data_todo",
            Some(value) => value.as_str().filter(|s| !s.is_empty()
                && s.split('.').count() == 2
                && s.split('.').all(|part| !part.is_empty()
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')))
                .ok_or("activity_type_xmlid must be a module-qualified XML ID")?,
        };
        ("activity_schedule", json!({"act_type_xmlid": xmlid, "summary": plain_text(params, "summary")?}))
    } else {
        ("message_post", json!({"message_type":"comment", "body":plain_text(params, "body")?,
            "subtype_xmlid": if tool == "odoo_note_post" { "mail.mt_note" } else { "mail.mt_comment" }}))
    };
    if activity {
        if params.get("note").is_some() { kwargs["note"] = json!(plain_text(params, "note")?); }
        if let Some(value) = params.get("date_deadline") {
            let date = value.as_str().ok_or("date_deadline must be YYYY-MM-DD")?;
            let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| "date_deadline must be YYYY-MM-DD")?;
            if parsed.format("%Y-%m-%d").to_string() != date || date.chars().count() != 10 {
                return Err("date_deadline must be YYYY-MM-DD".into());
            }
            kwargs["date_deadline"] = json!(date);
        }
        if params.get("user_id").is_some() { kwargs["user_id"] = json!(positive_id(params, "user_id")?); }
    } else if tool == "odoo_note_post" {
        kwargs["partner_ids"] = json!([]);
    }
    Ok(Writeback { model: model.into(), res_id, method, kwargs })
}

async fn write(conn: &OdooConnector, call: &Writeback) -> std::result::Result<Value, String> {
    let records = conn.search_read(&call.model, vec![json!(["id", "=", call.res_id])], &["id"], 1).await?;
    if !records.as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["id"].as_i64() == Some(call.res_id)) {
        return Err("找不到目標記錄，或連線帳號無法讀取這筆資料。".into());
    }
    conn.call_kw(&call.model, call.method, vec![json!([call.res_id])], call.kwargs.clone()).await
}

pub(crate) fn odoo_writeback_precheck(home: &Path, agent: &str, tool: &str, params: &Value)
    -> std::result::Result<(), String> {
    ToolPolicy::load(home, agent)?.check_call(tool, params, None)?;
    parse(tool, params, false)?;
    Ok(())
}

pub(super) async fn handle(
    tool: &str, params: &Value, home: &Path, odoo: &OdooState, agent: &str,
    bound: Option<Arc<OdooConnector>>,
) -> Value {
    handle_actor(tool, params, home, odoo, OdooActor::Employee(agent), bound).await
}

pub(super) async fn handle_actor(
    tool: &str, params: &Value, home: &Path, odoo: &OdooState, actor: OdooActor<'_>,
    bound: Option<Arc<OdooConnector>>,
) -> Value {
    let audit_name = actor.audit_name();
    let agent = audit_name.as_ref();
    let policy = match actor.policy(home) {
        Ok(policy) => policy, Err(error) => return mcp_error(&error),
    };
    if let Some(employee) = actor.employee() {
        odoo.register_agent(employee, policy.agent.clone()).await;
        if let Err(error) = odoo.load_global_for_gate(home).await { return mcp_error(&error); }
    }
    // Only the gateway decision chooses a runtime identity/mode. Isolation
    // already binds its connector and retains the conservative Engine policy.
    let (conn, decision): (_, Option<OdooIdentity>) = match bound {
        Some(conn) => (conn, None),
        None => match actor.connect(home, odoo, tool, params).await {
            Ok((conn, identity)) => (conn, Some(identity)),
            Err(error) => return mcp_error(&error),
        },
    };
    let result = async {
        policy.check_call(tool, params, decision.as_ref())?;
        let call = parse(tool, params, follows(decision.as_ref()))?;
        write(&conn, &call).await
    }.await;
    let suffix = identity_suffix(decision.as_ref(), "create");
    odoo_identity_client::audit_call(home, agent, tool, &suffix, result.is_ok(), None);
    match result {
        Ok(value) => tool_text(&format!("Odoo write-back completed: {value}\n{suffix}")),
        Err(error) => mcp_error(&error_text(&error, decision.as_ref())),
    }
}

#[cfg(test)]
#[path = "odoo_writeback_tests.rs"]
mod tests;
