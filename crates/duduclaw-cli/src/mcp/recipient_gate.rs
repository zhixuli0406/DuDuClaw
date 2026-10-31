//! Recipient policy uses only gateway-attested source and owner identities.
use std::path::Path;
use duduclaw_core::recipient_policy::{self, Decision, Mode};
use serde_json::{json, Value};

fn channel_target(args: &Value, channel: &str, chat: &str) -> String {
    recipient_policy::channel_recipient(args[channel].as_str().unwrap_or_default(), args[chat].as_str().unwrap_or_default())
        .unwrap_or_default()
}

/// None means no recipient-bearing operation. Invalid targets remain in the
/// set: the policy must ask rather than silently dropping an unparsed address.
pub(crate) fn targets(tool: &str, args: &Value) -> Option<Vec<String>> {
    match tool {
        "send_message" | "send_photo" | "send_sticker" | "create_reminder" => {
            Some(vec![channel_target(args, "channel", "chat_id")])
        }
        "mail_send" => Some(vec![recipient_policy::channel_recipient("mail", args["to"].as_str().unwrap_or_default()).unwrap_or_default()]),
        "tasks_create" | "update_cron_task" => {
            if !["notify_channel", "notify_chat_id", "notify_thread_id"].iter()
                .any(|key| args.get(key).is_some_and(|v| !v.is_null()))
            {
                return None;
            }
            let empty = |key: &str| args.get(key).is_none_or(|v| v.is_null() || v.as_str() == Some(""));
            if empty("notify_channel") && empty("notify_chat_id") && empty("notify_thread_id") {
                return None;
            }
            let mut out = vec![channel_target(args, "notify_channel", "notify_chat_id")];
            if let Some(thread) = args["notify_thread_id"].as_str().filter(|s| !s.is_empty()) {
                // Discord sends directly to this id; it is not proof of membership
                // in the supplied parent channel.
                out.push(recipient_policy::channel_recipient(args["notify_channel"].as_str().unwrap_or_default(), thread).unwrap_or_default());
            }
            Some(out)
        }
        "calendar_create_event" => match args.get("attendees") {
            None | Some(Value::Null) => None,
            Some(Value::String(list)) => {
                let recipients: Vec<_> = list.split(',').map(str::trim).filter(|s| !s.is_empty())
                    .map(|email| recipient_policy::channel_recipient("mail", email).unwrap_or_default()).collect();
                if recipients.is_empty() { None } else { Some(recipients) }
            }
            Some(_) => Some(vec![String::new()]),
        },
        // GitHub targets a publication resource, not a person. Responsibility
        // questions are server-routed; OS notifications have no remote address.
        // Delegation retains its own policy. Unknown tools have no trusted
        // recipient adapter, so egress effects carry an unresolvable target.
        _ if duduclaw_core::tool_effect::effect_of_builtin(tool).is_none()
            && duduclaw_core::tool_effect::egress_capable(tool).unwrap_or(true) => Some(vec![String::new()]),
        _ => None,
    }
}

pub(crate) async fn check(
    home: &Path, agent: &str, tool: &str, args: &Value, employee: bool, use_turn: bool,
) -> Option<Value> {
    check_with_call(home, agent, tool, args, employee, use_turn, &uuid::Uuid::new_v4().to_string()).await
}

pub(crate) async fn check_with_call(
    home: &Path, agent: &str, tool: &str, args: &Value, employee: bool, use_turn: bool, call_id: &str,
) -> Option<Value> {
    check_inner(home, agent, tool, args, employee, use_turn, Some(call_id)).await
}

async fn check_inner(
    home: &Path, agent: &str, tool: &str, args: &Value, employee: bool, use_turn: bool, call_id: Option<&str>,
) -> Option<Value> {
    let targets = targets(tool, args)?;
    let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let policy = recipient_policy::load(home, employee.then_some(dir.as_path()));
    if policy.valid && policy.mode == Mode::Off { return None; }
    let context = if use_turn {
        duduclaw_gateway::egress_client::recipients(home, agent).await.ok()
    } else { None };
    let decision = if !policy.valid {
        Decision::Ask
    } else {
        recipient_policy::evaluate(policy.mode, &targets,
            context.as_ref().and_then(|c| c.source.as_deref()),
            context.as_ref().map(|c| c.owners.as_slice()).unwrap_or(&[]), &policy.recipients)
    };
    if let Some(call_id) = call_id { record_decision(home, agent, tool, decision, call_id) }
    else if decision == Decision::Ask { Some(json!({"reason":"outside_allowed_recipients"})) }
    else { None }
}

pub(crate) async fn confirm_dispatch(
    home: &Path, agent: &str, tool: &str, payload: &Value, employee: bool, use_turn: bool,
) -> Result<(), String> {
    if check_inner(home, agent, tool, payload.get("arguments").unwrap_or(&Value::Null), employee, use_turn, None).await.is_some()
        && payload["egress_guard"].get("recipient_guard").is_none()
    {
        return Err("等待核准期間收件對象的允許範圍已改變，請重試並取得收件人核准。".into());
    }
    Ok(())
}

fn record_decision(home: &Path, agent: &str, tool: &str, decision: Decision, call_id: &str) -> Option<Value> {
    match decision {
        Decision::Allow => None,
        Decision::WouldAsk => {
            duduclaw_gateway::egress_shadow::record(home, agent, tool, "recipient_would_ask", &[], call_id,
                duduclaw_gateway::egress_shadow::Transport::Mcp);
            duduclaw_security::audit::append_tool_call_with_extras(
                home, agent, tool, "recipient_guard: would ask before sending to an unlisted recipient",
                true, &[("recipient_guard", json!("would_ask")), ("call_id", json!(call_id))],
            );
            None
        }
        Decision::Ask => Some(json!({"reason":"outside_allowed_recipients"})),
    }
}

/// Use the existing Admin-only egress approval envelope so both gates share
/// one request and the same single-use binding over the final arguments.
pub(crate) fn merge_reason(payload: &mut Value, reason: Value) {
    if payload.get("egress_guard").is_none() {
        payload["egress_guard"] = json!({"reason":"recipient_guard", "sources":[]});
    }
    payload["egress_guard"]["recipient_guard"] = reason.clone();
    payload["recipient_guard"] = reason;
}

pub(crate) fn approval_summary(tool: &str, payload: &Value) -> String {
    let private_data = payload["egress_guard"]["sources"].as_array().is_some_and(|s| !s.is_empty());
    let recipient = payload["egress_guard"].get("recipient_guard").is_some();
    let mut summary = format!("工具「{tool}」需確認完整內容與目的地。");
    if private_data { summary.push_str("\n本回合已讀取私人資料，這次動作將向外傳送資料（egress_guard）。"); }
    if recipient { summary.push_str("\n目標不是來源對話也不在允許清單（含已驗證擁有者），需要管理員核准（recipient_guard）。"); }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not run: builds and tests are prohibited on the memory-constrained host.
    #[tokio::test]
    async fn unknown_tools_require_recipient_approval_unless_off() {
        let home = tempfile::tempdir().unwrap();
        for mode in ["off", "shadow", "ask"] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\nrecipient_guard='{mode}'\n")).unwrap();
            let reason = check(home.path(), "alice", "mcp__third_party__send",
                &json!({"to":"line:allowed"}), false, false).await;
            assert_eq!(reason.is_some(), mode == "ask");
        }
        assert_eq!(targets("mcp__third_party__send", &json!({"to":"line:allowed"})), Some(vec![String::new()]));
    }

    #[tokio::test]
    async fn builtin_recipient_shadow_shares_call_id_with_egress() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='shadow'\nrecipient_guard='shadow'\n").unwrap();
        let taint = duduclaw_gateway::egress_guard::Taint {
            tainted: true, sources: vec!["memory_search".into()],
            ..duduclaw_gateway::egress_guard::Taint::unknown()
        };
        duduclaw_gateway::egress_gate::evaluate_with_call(home.path(), "alice", "send_message", true, taint, "shared-call").unwrap();
        assert!(check_with_call(home.path(), "alice", "send_message",
            &json!({"channel":"telegram","chat_id":"123","text":"hello"}), false, false, "shared-call").await.is_none());
        let report = duduclaw_gateway::egress_shadow::report(home.path(), 7).unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.ticket_total, 1);
        assert_eq!(report.by_kind.len(), 2);
        assert!(report.by_kind.iter().all(|row| row.count == 1));
        let conn = rusqlite::Connection::open(home.path().join("egress_shadow.db")).unwrap();
        let calls: u64 = conn.query_row("SELECT COUNT(*) FROM shadow_events WHERE call_id='shared-call'", [], |row| row.get(0)).unwrap();
        assert_eq!(calls, 2);
    }

    #[test]
    fn covered_tools_extract_every_destination() {
        for tool in ["send_message", "send_photo", "send_sticker", "create_reminder"] {
            assert_eq!(targets(tool, &json!({"channel":"line","chat_id":"U123"})), Some(vec!["line:U123".into()]));
        }
        for tool in ["tasks_create", "update_cron_task"] {
            assert_eq!(targets(tool, &json!({"notify_channel":"discord","notify_chat_id":"123","notify_thread_id":"456"})),
                Some(vec!["discord:123".into(), "discord:456".into()]));
            assert_eq!(targets(tool, &json!({})), None);
            assert_eq!(targets(tool, &json!({"notify_thread_id":"456"})), Some(vec![String::new(), String::new()]));
        }
        assert_eq!(targets("mail_send", &json!({"to":"ops@example.com"})), Some(vec!["mail:ops@example.com".into()]));
        assert_eq!(targets("calendar_create_event", &json!({"attendees":"a@example.com, b@example.com"})),
            Some(vec!["mail:a@example.com".into(), "mail:b@example.com".into()]));
        assert_eq!(targets("calendar_create_event", &json!({})), None);
        assert_eq!(targets("calendar_create_event", &json!({"attendees":42})), Some(vec![String::new()]));
        for tool in ["github_issue_comment", "responsibility_ask", "os_notify", "send_to_agent", "spawn_agent", "team_handoff"] {
            assert_eq!(targets(tool, &json!({"to":"other@example.com"})), None);
        }
    }

    #[test]
    fn raw_mail_and_channel_targets_cannot_use_source_aliases() {
        for target in ["ops@example.com#agent:forged", "ops@example.com:thread"] {
            assert_eq!(targets("mail_send", &json!({"to":target})), Some(vec![String::new()]));
            assert_eq!(targets("calendar_create_event", &json!({"attendees":target})), Some(vec![String::new()]));
        }
        for target in ["U123#agent:forged", "U123:thread"] {
            assert_eq!(targets("send_message", &json!({"channel":"line","chat_id":target})), Some(vec![String::new()]));
        }
    }

    #[tokio::test]
    async fn removing_a_recipient_during_another_approval_requires_a_new_card() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/alice");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        std::fs::write(agent.join("agent.toml"), "[capabilities]\nrecipients=['line:U123']\n").unwrap();
        let mut payload = json!({"name":"send_message","arguments":{"channel":"line","chat_id":"U123","text":"hi"}});
        assert!(check(home.path(), "alice", "send_message", &payload["arguments"], true, false).await.is_none());
        std::fs::write(agent.join("agent.toml"), "[capabilities]\nrecipients=[]\n").unwrap();
        assert!(confirm_dispatch(home.path(), "alice", "send_message", &payload, true, false).await.is_err());
        merge_reason(&mut payload, json!({"reason":"outside_allowed_recipients"}));
        assert!(confirm_dispatch(home.path(), "alice", "send_message", &payload, true, false).await.is_ok());
    }

    #[tokio::test]
    async fn recipient_grant_is_admin_only_payload_bound_and_single_use() {
        use duduclaw_gateway::approval::{ApprovalBroker, action_grant::{ActionGrant, verify_and_consume}};
        let home = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let mut payload = json!({"name":"send_message","arguments":{"channel":"line","chat_id":"U123","text":"hi"}});
        merge_reason(&mut payload, json!({"reason":"outside_allowed_recipients"}));
        let grant = ActionGrant::build("alice", "send_message", &payload);
        payload["action_grant"] = grant.to_json();
        let id = broker.request("alice", "mcp_call", "recipient", payload.clone(), 300).await.unwrap();
        assert!(broker.decide(&id, true, "channel:operator").await.is_err());
        broker.decide_egress_dashboard(&id, &duduclaw_auth::UserContext::admin_fallback(), true).await.unwrap();
        payload["arguments"]["chat_id"] = json!("U456");
        assert!(verify_and_consume(&broker, &id, &ActionGrant::build("alice", "send_message", &payload)).await.is_err());
        assert!(verify_and_consume(&broker, &id, &grant).await.is_ok());
        assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    }

    #[test]
    fn shadow_records_without_creating_an_approval_reason() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(record_decision(home.path(), "alice", "send_message", Decision::WouldAsk, "builtin-call"), None);
        let row: Value = serde_json::from_str(std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(row["recipient_guard"], "would_ask");
        assert!(!home.path().join("approvals.db").exists());
        let report = duduclaw_gateway::egress_shadow::report(home.path(), 7).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.by_kind[0].key, "recipient_would_ask");
    }

    #[test]
    fn two_reasons_share_the_admin_envelope_and_argument_binding() {
        let mut payload = json!({"name":"send_message", "arguments":{"channel":"line","chat_id":"U123","text":"hello"},
            "egress_guard":{"reason":"egress_guard","sources":["memory_read"]}});
        merge_reason(&mut payload, json!({"reason":"outside_allowed_recipients"}));
        assert_eq!(payload["egress_guard"]["sources"], json!(["memory_read"]));
        assert_eq!(payload["egress_guard"]["recipient_guard"], payload["recipient_guard"]);
        let summary = approval_summary("send_message", &payload);
        assert!(summary.lines().any(|line| line.ends_with("（egress_guard）。")));
        assert!(summary.lines().any(|line| line.ends_with("（recipient_guard）。")));
        let grant = duduclaw_gateway::approval::action_grant::ActionGrant::build("alice", "send_message", &payload);
        payload["arguments"]["chat_id"] = json!("U456");
        assert_ne!(grant.args_digest, duduclaw_gateway::approval::action_grant::ActionGrant::build("alice", "send_message", &payload).args_digest);
        assert_ne!(grant.args_digest, duduclaw_gateway::approval::action_grant::ActionGrant::build("bob", "send_message", &payload).args_digest);
    }
}
