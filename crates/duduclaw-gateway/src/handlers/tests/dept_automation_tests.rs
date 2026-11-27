use super::*;
use crate::autopilot_store::{AutopilotHistoryRow, AutopilotRuleRow, AutopilotStore};

fn assert_validation_error(frame: WsFrame, expected: &str) {
    assert!(matches!(&frame, WsFrame::Response { ok: false, error: Some(error), .. }
        if error.as_str().is_some_and(|message| message.contains(expected))), "{frame:?}");
}

fn create_params(agent: &str) -> Value {
    json!({"name": "Department rule", "owner_department": "sales", "trigger_event": "agent_idle",
        "action": {"type": "delegate", "target_agent": agent, "prompt": "Prepare report"}})
}

fn seed<'a>(f: &'a Fixture, _agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        f.h.set_autopilot_store(Arc::new(AutopilotStore::open(&f.home).unwrap())).await;
    })
}

fn row(id: &str, department: Option<&str>) -> AutopilotRuleRow {
    let action = json!({"type": "delegate", "target_agent": if department == Some("mfg") { "b1" } else { "a2" },
        "prompt": "Report"});
    AutopilotRuleRow {
        id: id.into(), name: id.into(), enabled: true, trigger_event: "agent_idle".into(),
        conditions: "{}".into(), action: if department.is_some() {
            json!({"type": "steps", "steps": [action]}).to_string()
        } else { action.to_string() },
        created_at: Utc::now().to_rfc3339(), last_triggered_at: None, trigger_count: 0,
        sequence: None, metadata: None, active_window: None, run_policy: None,
        owner_department: department.map(str::to_owned), created_by_user_id: Some("mgrA".into()),
        updated_by_user_id: None, updated_at: None,
    }
}

async fn seed_rows(f: &Fixture) -> Arc<AutopilotStore> {
    let store = Arc::new(AutopilotStore::open(&f.home).unwrap());
    for (id, department) in [("sales-rule", Some("sales")), ("mfg-rule", Some("mfg")), ("company-rule", None)] {
        store.insert_rule(&row(id, department)).await.unwrap();
        store.append_history(&AutopilotHistoryRow {
            id: format!("history-{id}"), rule_id: id.into(), rule_name: id.into(),
            triggered_at: Utc::now().to_rfc3339(), result: "success".into(), details: Some(id.into()),
        }).await.unwrap();
    }
    f.h.set_autopilot_store(store.clone()).await;
    store
}

#[tokio::test]
async fn automation_create_target_scoped_matrix() {
    assert_scoped("autopilot.create", create_params, seed, assert_scoped_rpc_outcome).await;
    let f = fixture().await;
    seed(&f, "a2").await;
    let mut params = create_params("a2");
    params["created_by_user_id"] = json!("admin");
    params["updated_by_user_id"] = json!("admin");
    let payload = scoped_payload("create a2", Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await);
    assert_eq!(payload["rule"]["created_by_user_id"], "mgrA");
    assert_eq!(payload["rule"]["updated_by_user_id"], "mgrA");
    assert!(payload["rule"]["updated_at"].is_string());
    assert_eq!(payload["rule"]["action"]["type"], "steps");
    assert_eq!(payload["rule"]["action"]["steps"][0]["target_agent"], "a2");
    let id = payload["rule"]["id"].as_str().unwrap();
    let stored = f.h.ap_store().await.unwrap().get_rule(id).await.unwrap().unwrap();
    assert_eq!(stored.created_by_user_id.as_deref(), Some("mgrA"));
    assert_eq!(serde_json::from_str::<Value>(&stored.action).unwrap(), payload["rule"]["action"]);
    assert_eq!(schedule_grant_audits(&f), vec![("mgrA".into(), "sales".into(),
        json!({"group": "automation", "action": "create", "method": "autopilot.create"}))]);
}

#[tokio::test]
async fn automation_department_record_scoped_matrix() {
    for method in ["autopilot.list", "autopilot.update", "autopilot.remove", "autopilot.history"] {
        assert!(matches!(coverage_for(method), Some((_, Coverage::Scoped(_)))));
        let f = fixture().await;
        seed_rows(&f).await;
        let mut key = f.mgr_a.clone();
        key.origin = ContextOrigin::MemberKey;
        for ctx in [&f.mgr_b, &f.emp, &key] {
            assert_permission_denied(method, Box::pin(f.h.handle(method,
                json!({"rule_id": "sales-rule", "enabled": false}), ctx)).await);
        }
        if method == "autopilot.list" {
            let payload = scoped_payload(method, Box::pin(f.h.handle(method, json!({}), &f.mgr_a)).await);
            assert_eq!(payload["rules"].as_array().unwrap().len(), 1);
            assert_eq!(payload["rules"][0]["id"], "sales-rule");
            let admin = scoped_payload(method, Box::pin(f.h.handle(method, json!({}), &f.admin)).await);
            assert_eq!(admin["rules"].as_array().unwrap().len(), 3);
        } else {
            for id in ["mfg-rule", "company-rule", "missing-rule"] {
                assert_permission_denied(method, Box::pin(f.h.handle(method,
                    json!({"rule_id": id, "enabled": false}), &f.mgr_a)).await);
            }
            let payload = scoped_payload(method, Box::pin(f.h.handle(method,
                json!({"rule_id": "sales-rule", "enabled": false}), &f.mgr_a)).await);
            if method == "autopilot.history" {
                assert_eq!(payload["entries"][0]["details"], "sales-rule");
                assert_permission_denied(method, Box::pin(f.h.handle(method, json!({}), &f.mgr_a)).await);
            }
        }
    }
}

#[tokio::test]
async fn automation_update_validates_result_and_audits_remove() {
    let f = fixture().await;
    let store = seed_rows(&f).await;
    for patch in [json!({"owner_department": "mfg"}), json!({"owner_department": null}),
        json!({"owner_department": "sales-extra"}), json!({"trigger_event": "tick"}),
        json!({"metadata": {"induced": true}}), json!({"sequence": {}}),
        json!({"action": {"type": "delegate", "target_agent": "b1", "prompt": "x"}})] {
        let mut params = patch;
        params["rule_id"] = json!("sales-rule");
        let invalid_sequence = params.get("sequence").is_some();
        let frame = Box::pin(f.h.handle("autopilot.update", params, &f.mgr_a)).await;
        if invalid_sequence { assert_validation_error(frame, "sequence"); }
        else { assert_permission_denied("update", frame); }
    }
    assert!(schedule_grant_audits(&f).is_empty());
    assert_success("update", Box::pin(f.h.handle("autopilot.update",
        json!({"rule_id": "sales-rule", "enabled": false, "owner_department": "sales",
            "created_by_user_id": "admin", "updated_by_user_id": "admin"}), &f.mgr_a)).await);
    let updated = store.get_rule("sales-rule").await.unwrap().unwrap();
    assert!(!updated.enabled);
    assert_eq!(updated.created_by_user_id.as_deref(), Some("mgrA"));
    assert_eq!(updated.updated_by_user_id.as_deref(), Some("mgrA"));
    assert!(updated.updated_at.is_some());
    // Omitted action still needs validation after an employee moves.
    let path = f.home.join("agents/a2/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, original.replace("department = 'sales'", "department = 'mfg'")).unwrap();
    assert_permission_denied("update moved target", Box::pin(f.h.handle("autopilot.update",
        json!({"rule_id": "sales-rule", "enabled": true}), &f.mgr_a)).await);
    // Removal needs the rule's department grant, not a still-live action target.
    assert_success("remove", Box::pin(f.h.handle("autopilot.remove", json!({"rule_id": "sales-rule"}), &f.mgr_a)).await);
    // Simulate late history appended by an old dispatch after deletion.
    store.append_history(&AutopilotHistoryRow { id: "late".into(), rule_id: "sales-rule".into(),
        rule_name: "sales-rule".into(), triggered_at: Utc::now().to_rfc3339(), result: "success".into(), details: None,
    }).await.unwrap();
    assert_permission_denied("deleted history", Box::pin(f.h.handle("autopilot.history",
        json!({"rule_id": "sales-rule"}), &f.mgr_a)).await);
    assert_eq!(schedule_grant_audits(&f), vec![
        ("mgrA".into(), "sales".into(), json!({"group":"automation", "action":"edit", "method":"autopilot.update"})),
        ("mgrA".into(), "sales".into(), json!({"group":"automation", "action":"remove", "method":"autopilot.remove"})),
    ]);
}

#[tokio::test]
async fn automation_department_triggers_and_provenance_are_closed() {
    let f = fixture().await;
    seed(&f, "a2").await;
    for trigger in ["tick", "odoo_event", "mcp_event", "os_file", "os_frontmost", "security_event", "cron_tick", "unknown"] {
        let mut params = create_params("a2");
        params["trigger_event"] = json!(trigger);
        let frame = Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await;
        if matches!(trigger, "cron_tick" | "unknown") { assert_validation_error(frame, "trigger_event"); }
        else { assert_permission_denied(trigger, frame); }
    }
    for (field, value) in [("sequence", json!({"first":{"event":"task_created"}, "then":{"event":"task_updated"}, "within_secs":60})),
        ("metadata", json!({"induced":true})), ("owner_department", json!("sales-extra")),
        ("owner_department", json!("../sales")), ("owner_department", Value::Null)] {
        let mut params = create_params("a2");
        params[field] = value;
        assert_permission_denied(field, Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await);
    }
    let mut absent = create_params("a2");
    absent.as_object_mut().unwrap().remove("owner_department");
    assert_permission_denied("missing department", Box::pin(f.h.handle("autopilot.create", absent, &f.mgr_a)).await);
    for trigger in ["task_created", "task_updated", "task_status_changed", "activity_new", "channel_message", "agent_idle", "run_at_risk"] {
        let mut params = create_params("a2");
        params["trigger_event"] = json!(trigger);
        assert_success(trigger, Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await);
    }
}

fn notify_params(channel: &str, chat: &str) -> Value {
    let mut params = create_params("a2");
    params["action"] = json!({"type":"notify", "channel":channel, "chat_id":chat, "text":"Report"});
    params
}

#[tokio::test]
async fn automation_notify_requires_exact_verified_owner_or_department_destination() {
    let f = fixture().await;
    seed(&f, "a2").await;
    f.db.bind_channel_identity("mgrA", "line", "owner-dm", true).unwrap();
    f.db.bind_channel_identity("mgrB", "line", "other-dm", true).unwrap();
    f.db.bind_channel_identity("mgrA", "telegram", "pending-dm", false).unwrap();
    f.db.bind_channel_identity("mgrA", "googlechat", "users/123", true).unwrap();
    let path = f.home.join("agents/a2/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{original}\n[proactive]\nnotify_channel = 'line'\nnotify_chat_id = 'department-chat'\n")).unwrap();
    for (channel, chat) in [("line", "arbitrary"), ("line", "owner-dm-extra"), ("telegram", "owner-dm"),
        ("line", "other-dm"), ("telegram", "pending-dm"), ("googlechat", "users/123")] {
        assert_permission_denied(chat, Box::pin(f.h.handle("autopilot.create", notify_params(channel, chat), &f.mgr_a)).await);
    }
    for chat in ["owner-dm", "department-chat"] {
        let mut params = notify_params("line", chat);
        params["action"]["type"] = json!("proactive_notify");
        assert_success(chat, Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await);
    }
    let created = scoped_payload("owner DM", Box::pin(f.h.handle("autopilot.create",
        notify_params("line", "owner-dm"), &f.mgr_a)).await);
    let id = created["rule"]["id"].as_str().unwrap();
    f.db.set_dept_grants("admin", "mgrB", "sales", &[crate::dept_grants::GrantGroup::Automation], true).unwrap();
    // The editor's DM must not replace the original rule owner's DM.
    let mut patch = notify_params("line", "other-dm");
    patch["rule_id"] = json!(id);
    patch["created_by_user_id"] = json!("mgrB");
    assert_permission_denied("editor DM", Box::pin(f.h.handle("autopilot.update", patch, &f.mgr_b)).await);
    assert_success("original owner DM", Box::pin(f.h.handle("autopilot.update",
        json!({"rule_id": id, "enabled": false}), &f.mgr_b)).await);
    std::fs::write(&path, "invalid TOML !").unwrap();
    assert_permission_denied("unreadable notification config", Box::pin(f.h.handle("autopilot.create",
        notify_params("line", "owner-dm"), &f.mgr_a)).await);
}

#[tokio::test]
async fn automation_admin_legacy_and_department_paths() {
    let f = fixture().await;
    seed(&f, "a2").await;
    let mut legacy = notify_params("line", "arbitrary");
    legacy.as_object_mut().unwrap().remove("owner_department");
    legacy["trigger_event"] = json!("tick");
    let created = scoped_payload("admin legacy", Box::pin(f.h.handle("autopilot.create", legacy, &f.admin)).await);
    assert!(created["rule"]["owner_department"].is_null());
    assert_eq!(created["rule"]["action"]["type"], "notify");
    let id = created["rule"]["id"].as_str().unwrap();
    assert_success("admin update", Box::pin(f.h.handle("autopilot.update",
        json!({"rule_id":id, "enabled":false}), &f.admin)).await);
    assert_success("admin history without id", Box::pin(f.h.handle("autopilot.history", json!({}), &f.admin)).await);
    assert_success("admin remove", Box::pin(f.h.handle("autopilot.remove", json!({"rule_id":id}), &f.admin)).await);
    assert_success("admin department", Box::pin(f.h.handle("autopilot.create", create_params("a2"), &f.admin)).await);
    for target in ["b1", "main"] {
        assert_permission_denied(target, Box::pin(f.h.handle("autopilot.create", create_params(target), &f.admin)).await);
    }
    assert_permission_denied("admin arbitrary destination", Box::pin(f.h.handle("autopilot.create",
        notify_params("line", "arbitrary"), &f.admin)).await);
    assert!(schedule_grant_audits(&f).is_empty());
}

#[tokio::test]
async fn automation_steps_validate_every_target_and_preserve_approval() {
    let f = fixture().await;
    seed(&f, "a2").await;
    let mut params = create_params("a2");
    params["action"]["require_approval"] = json!(true);
    let created = scoped_payload("approval", Box::pin(f.h.handle("autopilot.create", params.clone(), &f.mgr_a)).await);
    assert_eq!(created["rule"]["action"]["steps"][0]["require_approval"], true);
    assert!(created["rule"]["action"].get("require_approval").is_none());
    params["action"] = json!({"type":"steps", "steps":[
        {"type":"delegate", "target_agent":"a1", "prompt":"Prepare"},
        {"type":"wait", "minutes":1},
        {"type":"run_skill", "target_agent":"a2", "skill_name":"report"},
    ]});
    assert_success("steps", Box::pin(f.h.handle("autopilot.create", params.clone(), &f.mgr_a)).await);
    // Even managers granted both departments cannot cross the rule's department.
    f.db.set_dept_grants("admin", "mgrA", "mfg", &[crate::dept_grants::GrantGroup::Automation], true).unwrap();
    params["action"]["steps"][2]["target_agent"] = json!("b1");
    assert_permission_denied("later step", Box::pin(f.h.handle("autopilot.create", params.clone(), &f.mgr_a)).await);
    params["action"] = json!({"type":"steps", "require_approval":false, "steps":[
        {"type":"delegate", "target_agent":"a2", "prompt":"x"}]});
    assert_validation_error(Box::pin(f.h.handle("autopilot.create", params, &f.mgr_a)).await, "require_approval");
    let store = seed_rows(&f).await;
    // A legacy RPC cannot modify a department row even if scope changed since routing.
    assert!(store.update_company_rule("sales-rule", &json!({"metadata":{"induced":true}})).await.is_err());
    assert!(store.get_rule("sales-rule").await.unwrap().unwrap().metadata.is_none());
}

#[tokio::test]
async fn automation_personal_and_unreadable_authority_are_denied() {
    let f = fixture().await;
    seed(&f, "a2").await;
    crate::trigger_route::tests::write_agent(&f.home.join("agents"), "p1", "p1", "", "specialist", "");
    let path = f.home.join("agents/p1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("[agent]", "[agent]\ndepartment = 'sales'")).unwrap();
    let org_path = f.home.join("org.toml");
    let org = std::fs::read_to_string(&org_path).unwrap();
    std::fs::write(org_path, format!("{org}\n[agents.p1]\ndepartment = 'sales'\nreports_to = ''\n")).unwrap();
    f.h.registry().write().await.scan().await.unwrap();
    // Use the persisted ownership source; no process-global ACL override is needed.
    let mut conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let tx = conn.transaction().unwrap();
    duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "p1", "mgrA").unwrap();
    duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, "p1", "mgrA").unwrap();
    tx.commit().unwrap();
    for ctx in [&f.mgr_a, &f.admin] {
        assert_permission_denied("personal target", Box::pin(f.h.handle("autopilot.create", create_params("p1"), ctx)).await);
    }
    std::fs::write(f.home.join("org.toml"), "invalid TOML !").unwrap();
    assert_permission_denied("unreadable authority", Box::pin(f.h.handle("autopilot.create", create_params("a1"), &f.mgr_a)).await);
}

#[tokio::test]
async fn automation_dm_read_failure_never_falls_back_to_department_destination() {
    let f = fixture().await;
    seed(&f, "a2").await;
    let path = f.home.join("agents/a2/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{original}\n[proactive]\nnotify_channel = 'line'\nnotify_chat_id = 'department-chat'\n")).unwrap();
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("DROP TABLE channel_identities", []).unwrap();
    assert_permission_denied("unreadable DM source", Box::pin(f.h.handle("autopilot.create",
        notify_params("line", "department-chat"), &f.mgr_a)).await);
}

#[tokio::test]
async fn department_create_and_update_share_normalized_save_validation() {
    let f = fixture().await;
    seed(&f, "a2").await;
    let mut params = create_params("a2");
    params["trigger_event"] = json!("channel_message");
    params["action"] = json!({"type":"run_skill", "target_agent":"a2", "skill_name":"report",
        "params":{"note":"{{text}}"}, "require_approval":true,
        "screen":{"mode":"local", "prompt":"Check relevance"}});
    params["active_window"] = json!({"days":[1], "start":"09:00", "end":"18:00", "timezone":"UTC"});
    params["run_policy"] = json!({"outside_window":"skip"});
    let created = scoped_payload("department params", Box::pin(f.h.handle("autopilot.create", params.clone(), &f.mgr_a)).await);
    let id = created["rule"]["id"].as_str().unwrap();
    let store = f.h.ap_store().await.unwrap();
    let before = store.get_rule(id).await.unwrap().unwrap();
    let action: Value = serde_json::from_str(&before.action).unwrap();
    assert_eq!(action["screen"]["mode"], "local");
    assert_eq!(action["steps"][0]["params"]["note"], "{{text}}");
    assert_eq!(action["steps"][0]["require_approval"], true);
    assert!(action["steps"][0].get("screen").is_none());
    for (key, value, message) in [
        ("trigger_event", json!(42), "trigger_event"),
        ("active_window", json!("invalid"), "active_window"),
        ("run_policy", json!("invalid"), "run_policy"),
        ("action", json!({"type":"run_skill", "target_agent":"a2", "skill_name":"report",
            "params":{"note":"{{unavailable_field}}"}}), "placeholder"),
        ("action", json!({"type":"steps", "steps":[{"type":"run_skill", "target_agent":"a2",
            "skill_name":"report", "screen":{"mode":"local", "prompt":"invalid step screen"}}]}), "screen"),
    ] {
        let mut invalid_create = params.clone();
        invalid_create[key] = value.clone();
        assert_validation_error(Box::pin(f.h.handle("autopilot.create", invalid_create, &f.mgr_a)).await, message);
        let mut patch = json!({"rule_id":id});
        patch[key] = value;
        assert_validation_error(Box::pin(f.h.handle("autopilot.update", patch, &f.mgr_a)).await, message);
    }
    let after = store.get_rule(id).await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&after).unwrap(), serde_json::to_value(&before).unwrap());
    assert_eq!(store.list_rules().await.unwrap().len(), 1);
    // Admin department edits use the same path; a valid legacy action is wrapped.
    assert_success("admin department update", Box::pin(f.h.handle("autopilot.update",
        json!({"rule_id":id, "action":create_params("a2")["action"]}), &f.admin)).await);
    let updated = store.get_rule(id).await.unwrap().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&updated.action).unwrap()["type"], "steps");
}
