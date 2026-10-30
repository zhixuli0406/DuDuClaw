//! Server-owned recipient identities and scheduled delivery admission.

use std::path::Path;
use duduclaw_auth::{AccessLevel, UserStatus};
use duduclaw_core::recipient_policy::{Decision, Mode, channel_recipient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientContext {
    pub source: Option<String>,
    pub owners: Vec<String>,
}

/// Only active users bound as owners contribute verified channel identities.
/// Proactive destinations are employee-editable and never establish ownership.
pub fn owners(home: &Path, agent: &str) -> Result<Vec<String>, String> {
    let Some(db) = crate::decision_notify::open_user_db(home)? else {
        return Ok(Vec::new());
    };
    let mut recipients = Vec::new();
    for binding in db.get_agent_users(agent)? {
        if binding.access_level != AccessLevel::Owner { continue; }
        if !db.get_user(&binding.user_id)?.is_some_and(|user| user.status == UserStatus::Active) {
            continue;
        }
        for identity in db.verified_channels_for_user(&binding.user_id)? {
            if let Some(recipient) = channel_recipient(&identity.channel, &identity.channel_user_id) {
                recipients.push(recipient);
            }
        }
    }
    recipients.sort();
    recipients.dedup();
    Ok(recipients)
}

pub fn context(home: &Path, agent: &str, turn: &str) -> Result<RecipientContext, String> {
    Ok(RecipientContext {
        source: crate::turn_context::source_conversation_for(agent, turn),
        owners: owners(home, agent)?,
    })
}

/// Resolve legacy scheduled aliases against the same live registry as runtime dispatch.
pub(crate) async fn scheduled_agent(
    registry: &std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>,
    agent: &str,
) -> Result<String, String> {
    if agent != "default" { return Ok(agent.to_owned()); }
    registry.read().await.main_agent().map(|agent| agent.config.agent.name.clone())
        .ok_or_else(|| "排程的預設員工不存在，無法確認收件人政策。".into())
}

fn scheduled_decision(home: &Path, agent: &str, targets: &[String]) -> Result<Decision, String> {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let policy = duduclaw_core::recipient_policy::load(home, Some(&dir));
    if policy.mode == Mode::Off && policy.valid { return Ok(Decision::Allow); }
    let owners = match owners(home, agent) {
        Ok(owners) => owners,
        Err(_) if policy.mode == Mode::Shadow && policy.valid => return Ok(Decision::WouldAsk),
        Err(error) => return Err(error),
    };
    if !policy.valid { return Err("無法讀取收件人防護設定，已暫停排程通知。".into()); }
    Ok(policy.evaluate(targets, None, &owners))
}

/// Re-read policy at fire time. Scheduled jobs never inherit a source chat.
/// A fresh approval covers this exact occurrence, destination and message only.
pub(crate) async fn admit_scheduled(
    home: &Path, agent: &str, targets: &[String], args: Value,
) -> Result<(), String> {
    let decision = scheduled_decision(home, agent, targets)?;
    if decision == Decision::Allow { return Ok(()); }
    if decision == Decision::WouldAsk {
        duduclaw_security::audit::append_tool_call_with_extras(home, agent, "scheduled_notification",
            "scheduled recipient is neither source conversation nor allowed recipient", true,
            &[("recipient_guard", json!("would_ask"))]);
        return Ok(());
    }
    use crate::approval::{ApprovalBroker, ApprovalStatus, action_grant::verify_and_consume};
    let broker = ApprovalBroker::open(home)?;
    let (payload, grant) = scheduled_request(agent, targets, args);
    let id = broker.request(agent, "mcp_call", "排程通知的對象不是來源對話也不在允許清單；請確認完整收件人與內容。",
        payload, 300).await?;
    if broker.await_decision(&id, std::time::Duration::from_secs(2)).await? != ApprovalStatus::Approved {
        return Err(format!("排程通知未獲管理員核准（審批編號 {id}）。"));
    }
    // A malformed or unreadable policy cannot be overridden by an approval.
    scheduled_decision(home, agent, targets)?;
    verify_and_consume(&broker, &id, &grant).await
        .map_err(|_| "排程通知核准已失效或使用，已停止傳送。".into())
}

fn scheduled_request(agent: &str, targets: &[String], args: Value) -> (Value, crate::approval::action_grant::ActionGrant) {
    use crate::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY};
    let mut payload = json!({"name": "scheduled_notification", "arguments": args});
    let grant = ActionGrant::build(agent, "scheduled_notification", &payload);
    payload[ACTION_GRANT_KEY] = grant.to_json();
    payload["recipient_guard"] = json!({"reason": "outside_allowed_recipients", "targets": targets});
    payload["egress_guard"] = json!({"reason": "recipient_guard", "sources": [],
        "recipient_guard": payload["recipient_guard"].clone()});
    (payload, grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{UserDb, UserRole};

    #[test]
    fn owners_require_owner_binding_and_verified_identity_and_ignore_proactive() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[proactive]\nnotify_channel='line'\nnotify_chat_id='editable'\n").unwrap();
        assert!(owners(home.path(), "bot").unwrap().is_empty());
        assert!(!home.path().join("users.db").exists());
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let owner = db.create_user("owner@example.com", "Owner", "test-password", UserRole::Admin).unwrap();
        let operator = db.create_user("operator@example.com", "Operator", "test-password", UserRole::Admin).unwrap();
        db.bind_agent(&owner.id, "bot", AccessLevel::Owner).unwrap();
        db.bind_agent(&operator.id, "bot", AccessLevel::Operator).unwrap();
        db.bind_channel_identity(&owner.id, "line", "verified", true).unwrap();
        db.bind_channel_identity(&owner.id, "line", "unverified", false).unwrap();
        db.bind_channel_identity(&operator.id, "telegram", "123", true).unwrap();
        assert_eq!(owners(home.path(), "bot").unwrap(), ["line:verified"]);
        db.set_user_status(&owner.id, UserStatus::Suspended).unwrap();
        assert!(owners(home.path(), "bot").unwrap().is_empty());
    }

    #[test]
    fn scheduled_fire_rechecks_policy_with_an_empty_source_set() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipients=['telegram:123']\n").unwrap();
        let targets = vec!["telegram:123".into()];
        assert_eq!(scheduled_decision(home.path(), "bot", &targets).unwrap(), Decision::Allow);
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipients=[]\n").unwrap();
        assert_eq!(scheduled_decision(home.path(), "bot", &targets).unwrap(), Decision::Ask);
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        assert_eq!(scheduled_decision(home.path(), "bot", &targets).unwrap(), Decision::WouldAsk);
    }

    #[tokio::test]
    async fn shadow_scheduled_notification_never_blocks_and_records_would_ask() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "[capabilities]\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        admit_scheduled(home.path(), "bot", &["line:outside".into()], json!({"text":"hello"})).await.unwrap();
        let text = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(row["recipient_guard"], "would_ask");
    }
    #[tokio::test]
    async fn scheduled_approval_is_admin_only_and_binds_one_destination_and_body() {
        use crate::approval::{ApprovalBroker, action_grant::{ActionGrant, verify_and_consume}};
        let home = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let args = json!({"task_id":"task-1", "channel":"line", "chat_id":"outside", "text":"private"});
        let (payload, grant) = scheduled_request("bot", &["line:outside".into()], args.clone());
        assert_eq!(payload["recipient_guard"]["reason"], "outside_allowed_recipients");
        let id = broker.request("bot", "mcp_call", "recipient approval", payload, 300).await.unwrap();
        assert!(broker.decide(&id, true, "channel:admin").await.is_err());
        let mut manager = duduclaw_auth::UserContext::admin_fallback();
        manager.role = UserRole::Manager;
        assert!(broker.decide_egress_dashboard(&id, &manager, true).await.is_err());
        broker.decide_egress_dashboard(&id, &duduclaw_auth::UserContext::admin_fallback(), true).await.unwrap();
        for (key, value) in [("chat_id", "other"), ("text", "changed"), ("task_id", "next-occurrence")] {
            let mut changed = args.clone();
            changed[key] = json!(value);
            let changed = ActionGrant::build("bot", "scheduled_notification", &json!({"arguments":changed}));
            assert!(verify_and_consume(&broker, &id, &changed).await.is_err());
        }
        verify_and_consume(&broker, &id, &grant).await.unwrap();
        assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    }

    #[tokio::test]
    async fn legacy_default_schedule_uses_the_real_main_employee_policy() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/real-main");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        std::fs::write(dir.join("agent.toml"), r#"
[agent]
name = "real-main"
display_name = "Main"
role = "main"
status = "active"
trigger = "@main"
reports_to = ""
icon = "x"
[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""
[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false
[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = true
[capabilities]
recipient_guard = "ask"
"#).unwrap();
        let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.path().join("agents"));
        registry.scan().await.unwrap();
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(registry));
        let agent = scheduled_agent(&registry, "default").await.unwrap();
        assert_eq!(agent, "real-main");
        assert_eq!(scheduled_decision(home.path(), &agent, &["line:outside".into()]).unwrap(), Decision::Ask);
        let empty = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::registry::AgentRegistry::new(home.path().join("empty"))));
        assert!(scheduled_agent(&empty, "default").await.is_err());
    }

}
