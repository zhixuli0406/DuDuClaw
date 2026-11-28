use super::*;
use crate::autopilot_store::AutopilotHistoryRow;
use crate::dept_grants::tests::{fixture, Fixture};
use duduclaw_auth::acl::ContextOrigin;

const METHOD: &str = "autopilot.affecting_me";

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected success: {other:?}"),
    }
}

fn denied(frame: WsFrame) {
    assert!(matches!(&frame, WsFrame::Response { ok: false, error: Some(error), .. }
        if error.as_str().is_some_and(|s| s.contains(DENIED))), "{frame:?}");
}

fn rule(id: &str, department: Option<&str>, action: Value) -> AutopilotRuleRow {
    AutopilotRuleRow {
        id: id.into(), name: format!("Rule {id}"), enabled: true,
        trigger_event: "agent_idle".into(), conditions: json!({"field":"idle_seconds", "op":"gt", "value":60}).to_string(),
        action: action.to_string(), created_at: "2026-11-27T00:00:00Z".into(),
        last_triggered_at: None, trigger_count: 0, sequence: None, metadata: None,
        active_window: None, run_policy: None, owner_department: department.map(str::to_owned),
        created_by_user_id: Some("private-creator".into()), updated_by_user_id: Some("private-editor".into()),
        updated_at: None,
    }
}

fn delegate(agent: &str) -> Value {
    json!({"type":"delegate", "target_agent":agent, "prompt":"Report {{title}}"})
}

async fn seed(f: &Fixture) -> Arc<AutopilotStore> {
    let store = Arc::new(AutopilotStore::open(&f.home).unwrap());
    store.insert_rule(&rule("company", None, delegate("a1"))).await.unwrap();
    store.insert_rule(&rule("department", Some("sales"), json!({"type":"steps", "steps":[
        delegate("a1"),
        {"type":"notify", "channel":"private-channel", "chat_id":"private-chat", "text":"private-text"},
        {"type":"proactive_notify", "channel":"private-channel", "chat_id":"private-chat", "text":"private-text"},
        {"type":"run_skill", "target_agent":"a1", "skill_name":"report", "require_approval":true},
        {"type":"delegate", "target_agent":"b1", "prompt":"private-other-prompt"}
    ]}))).await.unwrap();
    store.insert_rule(&rule("other", None, delegate("b1"))).await.unwrap();
    store.insert_rule(&rule("prefix", None, delegate("a1-extra"))).await.unwrap();
    f.h.set_autopilot_store(store.clone()).await;
    store
}

#[tokio::test]
async fn affecting_me_projects_company_and_department_rules_and_twenty_history_rows() {
    let f = fixture().await;
    let store = seed(&f).await;
    f.db.bind_agent(&f.emp.user_id, "a1", AccessLevel::Viewer).unwrap();
    for index in 0..25 {
        store.append_history(&AutopilotHistoryRow {
            id: format!("history-{index}"), rule_id: "company".into(), rule_name: "company".into(),
            triggered_at: format!("2026-11-27T00:00:{index:02}Z"), result: "success".into(),
            details: Some(json!({"event_payload":"private-event", "chat_id":"private-chat"}).to_string()),
        }).await.unwrap();
    }
    // The fixture context predates the binding: authorization must re-read it.
    assert!(f.emp.agent_access.is_empty());
    let result = payload(Box::pin(f.h.handle(METHOD, json!({}), &f.emp)).await);
    let rules = result["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    let company = rules.iter().find(|r| r["id"] == "company").unwrap();
    let department = rules.iter().find(|r| r["id"] == "department").unwrap();
    for row in rules {
        let keys: BTreeSet<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "name", "enabled", "trigger_event", "conditions", "action_kinds",
            "target_agents", "actions", "require_approval", "owner_department", "history", "creator_access_lost"].into());
        assert_eq!(row["target_agents"], json!(["a1"]));
        assert_eq!(row["creator_access_lost"], true);
    }
    assert!(company["owner_department"].is_null());
    assert_eq!(company["require_approval"], false);
    assert_eq!(department["owner_department"], "sales");
    assert_eq!(department["require_approval"], true);
    assert_eq!(department["action_kinds"], json!(["delegate", "notify", "proactive_notify", "run_skill", "delegate"]));
    assert_eq!(department["actions"], json!([
        {"type":"delegate", "target_agent":"a1", "prompt":"Report {{title}}", "require_approval":false},
        {"type":"run_skill", "target_agent":"a1", "skill_name":"report", "require_approval":true}
    ]));
    assert_eq!(company["conditions"], json!({"field":"idle_seconds", "op":"gt", "value":60}));
    let history = company["history"].as_array().unwrap();
    assert_eq!(history.len(), 20);
    assert_eq!(history[0]["triggered_at"], "2026-11-27T00:00:24Z");
    assert_eq!(history[19]["triggered_at"], "2026-11-27T00:00:05Z");
    for entry in history {
        assert_eq!(entry.as_object().unwrap().len(), 2);
        assert_eq!(entry["result"], "success");
    }
    let serialized = result.to_string();
    for secret in ["channel", "chat_id", "text", "private-", "details", "created_by", "updated_by", "b1", "a1-extra"] {
        assert!(!serialized.contains(secret), "leaked {secret}: {serialized}");
    }
}

#[tokio::test]
async fn affecting_me_requires_exact_live_direct_bindings_and_dashboard_identity() {
    let f = fixture().await;
    seed(&f).await;
    f.db.bind_agent(&f.emp.user_id, "a1", AccessLevel::Viewer).unwrap();
    for agent in ["b1", "a1-extra", " a1", "a1 ", ""] {
        denied(Box::pin(f.h.handle(METHOD, json!({"agent_id":agent}), &f.emp)).await);
    }
    denied(Box::pin(f.h.handle(METHOD, json!({"agent_id":42}), &f.emp)).await);
    let scoped = payload(Box::pin(f.h.handle(METHOD, json!({"agent_id":"a1"}), &f.emp)).await);
    assert_eq!(scoped["rules"].as_array().unwrap().len(), 2);
    // Neither Admin role, department grants nor inherited team bindings widen this view.
    f.db.set_manager(&f.emp.user_id, Some(&f.mgr_a.user_id)).unwrap();
    for ctx in [&f.admin, &f.mgr_a] {
        let result = payload(Box::pin(f.h.handle(METHOD, json!({}), ctx)).await);
        assert_eq!(result, json!({"rules":[]}));
        denied(Box::pin(f.h.handle(METHOD, json!({"agent_id":"a1"}), ctx)).await);
    }
    for origin in [ContextOrigin::MemberKey, ContextOrigin::AdminToken, ContextOrigin::Other] {
        let mut ctx = f.emp.clone();
        ctx.origin = origin;
        denied(f.h.handle_autopilot_affecting_me(json!({}), &ctx).await);
    }
    assert_eq!(super::super::member_contract::member_key_method(METHOD), None);
    let mut stale = f.emp.clone();
    stale.agent_access.insert("a1".into(), AccessLevel::Owner);
    f.db.unbind_agent(&f.emp.user_id, "a1").unwrap();
    denied(Box::pin(f.h.handle(METHOD, json!({"agent_id":"a1"}), &stale)).await);
    assert_eq!(payload(Box::pin(f.h.handle(METHOD, json!({}), &stale)).await), json!({"rules":[]}));
}

#[tokio::test]
async fn affecting_me_agent_filter_narrows_steps_and_includes_disabled_rules() {
    let f = fixture().await;
    let store = seed(&f).await;
    for (agent, level) in [("a1", AccessLevel::Operator), ("b1", AccessLevel::Owner)] {
        f.db.bind_agent(&f.emp.user_id, agent, level).unwrap();
    }
    let mut disabled = rule("disabled", None, json!({"type":"run_skill", "target_agent":"b1", "skill_name":"nightly"}));
    disabled.enabled = false;
    store.insert_rule(&disabled).await.unwrap();
    let result = payload(Box::pin(f.h.handle(METHOD, json!({"agent_id":"b1"}), &f.emp)).await);
    let rules = result["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3);
    assert!(rules.iter().any(|row| row["id"] == "disabled" && row["enabled"] == false));
    for row in rules {
        assert_eq!(row["target_agents"], json!(["b1"]));
        for action in row["actions"].as_array().unwrap() { assert_eq!(action["target_agent"], "b1"); }
    }
}

#[tokio::test]
async fn affecting_me_read_errors_never_become_empty_success() {
    for broken in ["rules", "history", "bindings", "action", "conditions", "missing_store"] {
        let f = fixture().await;
        f.db.bind_agent(&f.emp.user_id, "a1", AccessLevel::Viewer).unwrap();
        if broken != "missing_store" { seed(&f).await; }
        let (db, sql) = match broken {
            "rules" => ("autopilot.db", "DROP TABLE autopilot_rules"),
            "history" => ("autopilot.db", "DROP TABLE autopilot_history"),
            "bindings" => ("users.db", "DROP TABLE user_agent_bindings"),
            "action" => ("autopilot.db", "UPDATE autopilot_rules SET action='invalid JSON' WHERE id='company'"),
            "conditions" => ("autopilot.db", "UPDATE autopilot_rules SET conditions='invalid JSON' WHERE id='company'"),
            _ => ("", ""),
        };
        if !sql.is_empty() { rusqlite::Connection::open(f.home.join(db)).unwrap().execute_batch(sql).unwrap(); }
        let frame = Box::pin(f.h.handle(METHOD, json!({}), &f.emp)).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{broken}: {frame:?}");
    }
}

#[tokio::test]
async fn affecting_me_protected_rules_are_owner_only_even_with_stale_nonowner_bindings() {
    let f = fixture().await;
    let store = seed(&f).await;
    f.db.bind_agent(&f.emp.user_id, "a1", AccessLevel::Viewer).unwrap();
    let mut conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let tx = conn.transaction().unwrap();
    duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "a2", &f.mgr_a.user_id).unwrap();
    duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, "a2", &f.mgr_a.user_id).unwrap();
    tx.commit().unwrap();
    // Simulate legacy/corrupt non-owner bindings in this test-owned database only.
    conn.execute_batch("DROP TRIGGER isolation_personal_binding_insert").unwrap();
    for ctx in [&f.emp, &f.admin] {
        conn.execute("INSERT INTO user_agent_bindings (user_id, agent_name, access_level, bound_at) VALUES (?1, 'a2', 'viewer', '2026-11-27')",
            [&ctx.user_id]).unwrap();
    }
    store.insert_rule(&rule("personal", None, delegate("a2"))).await.unwrap();
    store.insert_rule(&rule("mixed-personal", None, json!({"type":"steps", "steps":[delegate("a1"), delegate("a2")]}))).await.unwrap();
    for ctx in [&f.emp, &f.admin] {
        let result = f.h.autopilot_affecting_result(&json!({}), ctx, true).await.unwrap();
        assert!(!result.to_string().contains("personal"));
        assert!(!result.to_string().contains("a2"));
        assert_eq!(f.h.autopilot_affecting_result(&json!({"agent_id":"a2"}), ctx, true).await.unwrap_err(), DENIED);
    }
    let result = f.h.autopilot_affecting_result(&json!({"agent_id":"a2"}), &f.mgr_a, true).await.unwrap();
    assert_eq!(result["rules"].as_array().unwrap().len(), 2);
    for row in result["rules"].as_array().unwrap() { assert_eq!(row["target_agents"], json!(["a2"])); }
    conn.execute_batch("DROP TABLE isolation_personal_agents").unwrap();
    assert!(f.h.autopilot_affecting_result(&json!({}), &f.mgr_a, true).await.is_err());
}

#[test]
fn affecting_me_is_registered_without_department_delegation_or_personal_exemption() {
    assert_eq!(super::super::dashboard_scope::minimum_role(METHOD), Some(UserRole::Employee));
    assert_eq!(crate::dept_grants::methods::coverage_for(METHOD), Some((
        crate::dept_grants::GrantGroup::Automation,
        crate::dept_grants::methods::Coverage::NotDelegated("member read-only view of rules acting on own bound employees"),
    )));
    assert!(!super::super::personal_p1g_gate::metadata_allowlisted(METHOD));
}
