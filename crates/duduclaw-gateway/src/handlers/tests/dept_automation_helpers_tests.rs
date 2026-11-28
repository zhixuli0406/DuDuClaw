use super::*;
use crate::dept_grants::{DeptGrantView, GrantGroup};

fn exact_denied(frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => assert_eq!(error, json!("permission denied")),
        other => panic!("expected permission denied: {other:?}"),
    }
}

#[tokio::test]
async fn automation_helper_reads_require_exact_department_scope() {
    let f = fixture().await;
    seed_rows(&f).await;
    for trigger in ["task_created", "task_updated", "task_status_changed"] {
        let params = json!({"trigger_event":trigger});
        let manager = scoped_payload("fields", Box::pin(f.h.handle("autopilot.template_fields", params.clone(), &f.mgr_a)).await);
        let admin = scoped_payload("fields", Box::pin(f.h.handle("autopilot.template_fields", params, &f.admin)).await);
        assert_eq!(manager, admin);
        assert_eq!(manager.as_object().unwrap().len(), 1);
        assert!(manager["fields"].as_array().unwrap().iter().all(Value::is_string));
    }
    for params in [json!({"trigger_event":"channel_message"}), json!({"trigger_event":"tick"}),
        json!({"trigger_event":"task_created","sequence":{}}),
        json!({"trigger_event":"task_created","sequence":null}), json!({}), json!({"trigger_event":42})] {
        exact_denied(Box::pin(f.h.handle("autopilot.template_fields", params, &f.mgr_a)).await);
    }
    for id in ["mfg-rule", "company-rule", "missing-rule", ""] {
        exact_denied(Box::pin(f.h.handle("autopilot.runs.list", json!({"rule_id":id}), &f.mgr_a)).await);
    }
    let result = scoped_payload("runs", Box::pin(f.h.handle("autopilot.runs.list",
        json!({"rule_id":"sales-rule","limit":1}), &f.mgr_a)).await);
    assert_eq!(result, json!({"runs":[]}));
    exact_denied(Box::pin(f.h.handle("autopilot.runs.cancel", json!({"run_id":"missing"}), &f.mgr_a)).await);
    let mut key = f.mgr_a.clone();
    key.origin = ContextOrigin::MemberKey;
    for ctx in [&f.mgr_b, &f.emp, &key] {
        for (method, params) in [
            ("autopilot.template_fields", json!({"trigger_event":"task_created"})),
            ("autopilot.runs.list", json!({"rule_id":"sales-rule"})),
            ("autopilot.notify_destinations", json!({"owner_department":"sales"})),
        ] {
            assert_permission_denied(method, Box::pin(f.h.handle(method, params, ctx)).await);
        }
    }
    f.db.set_dept_grants("admin", "mgrA", "sales", &[GrantGroup::Schedules], true).unwrap();
    exact_denied(Box::pin(f.h.handle("autopilot.template_fields", json!({"trigger_event":"task_created"}), &f.mgr_a)).await);
    assert!(schedule_grant_audits(&f).is_empty());
}

fn set_destination(f: &Fixture, agent: &str, chat: &str) {
    let path = f.home.join("agents").join(agent).join("agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{raw}\n[proactive]\nnotify_channel = 'line'\nnotify_chat_id = '{chat}'\n")).unwrap();
}

#[tokio::test]
async fn automation_notify_destinations_match_save_and_never_list_other_users() {
    let f = fixture().await;
    seed(&f, "a2").await;
    f.db.bind_channel_identity("mgrA", "telegram", "own-dm", true).unwrap();
    f.db.bind_channel_identity("mgrB", "line", "other-dm", true).unwrap();
    f.db.bind_channel_identity("admin", "line", "admin-dm", true).unwrap();
    f.db.bind_channel_identity("mgrA", "line", "unverified", false).unwrap();
    set_destination(&f, "a1", "personal-chat");
    set_destination(&f, "a2", "department-chat");
    set_destination(&f, "b1", "foreign-chat");
    set_destination(&f, "main", "main-chat");
    crate::dept_grants::tests::make_personal(&f, "a1");
    let path = f.home.join("agents/a2/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("display_name = \"a2\"", "display_name = '業務助理'")).unwrap();
    f.h.registry().write().await.scan().await.unwrap();
    for ctx in [&f.mgr_a, &f.admin] {
        let result = scoped_payload("destinations", Box::pin(f.h.handle("autopilot.notify_destinations",
            json!({"owner_department":"sales","created_by_user_id":"mgrB"}), ctx)).await);
        let (channel, chat, label) = if ctx.is_admin() { ("line", "admin-dm", "我的 LINE 私訊") }
            else { ("telegram", "own-dm", "我的 Telegram 私訊") };
        assert_eq!(result, json!({"destinations":[
            {"channel":channel,"chat_id":chat,"kind":"self","agent_id":null,"label":label},
            {"channel":"line","chat_id":"department-chat","kind":"agent","agent_id":"a2","label":"業務助理 的通知對象（LINE）"}
        ]}));
        let view = DeptGrantView::load(&f.h, ctx).await;
        for destination in result["destinations"].as_array().unwrap() {
            let mut rule = row("candidate", Some("sales"));
            rule.created_by_user_id = Some(ctx.user_id.clone());
            rule.action = json!({"type":"steps","steps":[{"type":"notify",
                "channel":destination["channel"],"chat_id":destination["chat_id"],"text":"Report"}]}).to_string();
            assert!(crate::dept_grants::rules::validate_department_rule(&f.h,
                if ctx.is_admin() { None } else { Some(&view) }, &rule).await.is_ok());
            assert_success("save listed destination", Box::pin(f.h.handle("autopilot.create",
                notify_params(destination["channel"].as_str().unwrap(), destination["chat_id"].as_str().unwrap()), ctx)).await);
        }
    }
    for chat in ["other-dm", "admin-dm", "unverified", "personal-chat", "foreign-chat", "main-chat", "department-chat-extra"] {
        exact_denied(Box::pin(f.h.handle("autopilot.create", notify_params("line", chat), &f.mgr_a)).await);
    }
    for params in [json!({}), json!({"owner_department":null}), json!({"owner_department":42}),
        json!({"owner_department":"../sales"}), json!({"owner_department":"mfg"}), json!({"owner_department":"sales-extra"})] {
        exact_denied(Box::pin(f.h.handle("autopilot.notify_destinations", params, &f.mgr_a)).await);
    }
}

#[tokio::test]
async fn automation_notify_destinations_fail_closed_on_candidate_read_errors() {
    for broken in ["notify", "authority", "ownership", "dm"] {
        let f = fixture().await;
        f.db.bind_channel_identity("mgrA", "line", "own-dm", true).unwrap();
        match broken {
            "notify" => set_destination(&f, "a2", "valid-chat"),
            "authority" => std::fs::write(f.home.join("org.toml"), "[broken").unwrap(),
            "ownership" => {
                // Auth only needs agent_name; candidate ownership also needs owner_user_id.
                rusqlite::Connection::open(f.home.join("users.db")).unwrap()
                    .execute_batch("ALTER TABLE isolation_personal_agents RENAME COLUMN owner_user_id TO unavailable_owner_user_id").unwrap();
                crate::personal_visibility::invalidate_owner_cache(&f.home);
            }
            "dm" => { rusqlite::Connection::open(f.home.join("users.db")).unwrap()
                .execute_batch("DROP TABLE channel_identities").unwrap(); }
            _ => unreachable!(),
        }
        if broken == "notify" {
            let path = f.home.join("agents/a2/agent.toml");
            let raw = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, raw.replace("notify_channel = 'line'", "notify_channel = 42")).unwrap();
        }
        assert!(f.db.live_user_context(&f.mgr_a.user_id).unwrap().is_some(), "{broken}");
        let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
        assert!(!view.is_empty(), "{broken}");
        assert_eq!(crate::dept_grants::rules::department_notify_destinations(
            &f.h, Some(&view), "sales", &f.mgr_a.user_id).await.err().as_deref(),
            Some("permission denied"), "{broken}");
        exact_denied(Box::pin(f.h.handle("autopilot.notify_destinations",
            json!({"owner_department":"sales"}), &f.mgr_a)).await);
    }
}

#[tokio::test]
async fn automation_notify_destinations_fail_closed_when_authentication_unavailable() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute_batch("DROP TABLE isolation_personal_agents").unwrap();
    crate::personal_visibility::invalidate_owner_cache(&f.home);
    assert!(f.db.live_user_context(&f.mgr_a.user_id).is_err());
    let frame = Box::pin(f.h.handle("autopilot.notify_destinations",
        json!({"owner_department":"sales"}), &f.mgr_a)).await;
    match frame {
        WsFrame::Response { ok:false, payload:None, error:Some(error), .. } => {
            assert_eq!(error, json!("authentication service unavailable"));
        }
        other => panic!("expected authentication refusal: {other:?}"),
    }
}

#[tokio::test]
async fn automation_helpers_follow_history_personal_edition_gate() {
    let f = fixture().await;
    f.h.set_test_edition(Some(duduclaw_core::EditionProfile::Personal)).await;
    for method in ["autopilot.template_fields", "autopilot.runs.list", "autopilot.notify_destinations"] {
        assert_eq!(crate::handlers::gates::is_enterprise_only_method(method),
            crate::handlers::gates::is_enterprise_only_method("autopilot.history"));
        assert_eq!(crate::handlers::dashboard_scope::minimum_role(method),
            crate::handlers::dashboard_scope::minimum_role("autopilot.history"));
        assert_eq!(crate::handlers::member_contract::member_key_method(method), None);
    }
    assert_success("admin destinations in personal", Box::pin(f.h.handle("autopilot.notify_destinations",
        json!({"owner_department":"sales"}), &f.admin)).await);
    exact_denied(Box::pin(f.h.handle("autopilot.notify_destinations",
        json!({"owner_department":"sales"}), &f.mgr_a)).await);
}

#[tokio::test]
async fn automation_lists_display_creator_access_without_changing_rules() {
    let f = fixture().await;
    let store = seed_rows(&f).await;
    let mut legacy = row("legacy", Some("sales"));
    legacy.created_by_user_id = None;
    store.insert_rule(&legacy).await.unwrap();
    f.db.bind_agent("emp", "a2", duduclaw_auth::AccessLevel::Viewer).unwrap();
    for ctx in [&f.admin, &f.mgr_a, &f.emp] {
        let method = if ctx.user_id == "emp" { "autopilot.affecting_me" } else { "autopilot.list" };
        let result = scoped_payload(method, Box::pin(f.h.handle(method, json!({}), ctx)).await);
        let rules = result["rules"].as_array().unwrap();
        assert_eq!(rules.iter().find(|r| r["id"] == "legacy").unwrap()["creator_access_lost"], false);
        assert_eq!(rules.iter().find(|r| r["id"] == "sales-rule").unwrap()["creator_access_lost"], false);
        if ctx.is_admin() {
            for id in ["company-rule", "mfg-rule"] {
                assert_eq!(rules.iter().find(|r| r["id"] == id).unwrap()["creator_access_lost"], true);
            }
        }
    }
    f.db.set_dept_grants("admin", "mgrA", "sales", &[], true).unwrap();
    for method in ["autopilot.list", "autopilot.affecting_me"] {
        let ctx = if method == "autopilot.list" { &f.admin } else { &f.emp };
        let result = scoped_payload(method, Box::pin(f.h.handle(method, json!({}), ctx)).await);
        assert_eq!(result["rules"].as_array().unwrap().iter().find(|r| r["id"] == "sales-rule").unwrap()["creator_access_lost"], true);
    }
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute_batch("DROP TABLE department_manager_grants").unwrap();
    let result = scoped_payload("admin unknown", f.h.handle_autopilot_list().await);
    let rules = result["rules"].as_array().unwrap();
    assert_eq!(rules.iter().find(|r| r["id"] == "sales-rule").unwrap().get("creator_access_lost"), Some(&Value::Null));
    assert_eq!(rules.iter().find(|r| r["id"] == "legacy").unwrap()["creator_access_lost"], false);
    assert!(store.get_rule("sales-rule").await.unwrap().unwrap().enabled);
}

#[tokio::test]
async fn automation_runs_list_preserves_status_projection_for_managers() {
    use crate::autopilot_store::{AutopilotRunRow, AutopilotRunStepRow, RunInsertOutcome, run_ts};
    let f = fixture().await;
    let store = seed_rows(&f).await;
    let now = Utc::now();
    let run = AutopilotRunRow {
        gateway_origin: true, run_id: "sales-run".into(), rule_id: "sales-rule".into(),
        rule_name: "Department rule".into(), rule_hash: "private-hash".into(),
        event_name: "task_created".into(), event_ref: "private-event".into(),
        dedupe_key: "private-dedupe".into(), fields_json: "{\"private\":true}".into(),
        state: "waiting".into(), step_index: 0, step_count: 1, next_at: Some(run_ts(now)),
        lease_owner: None, lease_until: None, owner_department: Some("sales".into()),
        outcome: None, detail: Some("等待中".into()), notice_pending: false,
        notice_push_state: None, notice_push_attempts: 0, notice_push_next_at: None,
        created_at: run_ts(now), updated_at: run_ts(now),
    };
    let step = AutopilotRunStepRow {
        run_id: run.run_id.clone(), step_index: 0, step_type: "wait".into(), state: "done".into(),
        idempotency_key: "private-key".into(), dispatch_id: None, queue_message_id: None,
        started_at: None, finished_at: None, detail: None,
    };
    assert_eq!(store.insert_run(&run, &[step], now).await.unwrap(), RunInsertOutcome::Inserted);
    let params = json!({"rule_id":"sales-rule","limit":1});
    let manager = scoped_payload("manager runs", Box::pin(f.h.handle("autopilot.runs.list", params.clone(), &f.mgr_a)).await);
    let admin = scoped_payload("admin runs", Box::pin(f.h.handle("autopilot.runs.list", params, &f.admin)).await);
    assert_eq!(manager, admin);
    assert_eq!(manager, json!({"runs":[{
        "run_id":"sales-run","state":"waiting","step_index":0,"step_count":1,
        "next_at":run.next_at,"outcome":null,"detail":"等待中","event_name":"task_created",
        "created_at":run.created_at,"updated_at":run.updated_at,
        "steps":[{"step_index":0,"step_type":"wait","state":"done",
            "started_at":null,"finished_at":null,"detail":null}]
    }]}));
    exact_denied(Box::pin(f.h.handle("autopilot.runs.cancel", json!({"run_id":"sales-run"}), &f.mgr_a)).await);
    assert_eq!(store.get_run("sales-run").await.unwrap().unwrap().state, "waiting");
    // Stale run history alone must not reveal a deleted department rule.
    store.remove_rule("sales-rule").await.unwrap();
    exact_denied(Box::pin(f.h.handle("autopilot.runs.list", json!({"rule_id":"sales-rule"}), &f.mgr_a)).await);
}

#[tokio::test]
async fn automation_creator_unknown_is_null_in_department_and_affecting_lists() {
    let f = fixture().await;
    let store = seed_rows(&f).await;
    let mut unknown = row("unknown-creator", Some("sales"));
    unknown.created_by_user_id = Some("mgrB".into());
    store.insert_rule(&unknown).await.unwrap();
    f.db.bind_agent("emp", "a2", duduclaw_auth::AccessLevel::Viewer).unwrap();
    // Only the creator snapshot is unreadable; caller identity and grants remain valid.
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    conn.execute("INSERT INTO user_agent_bindings(user_id, agent_name, access_level, bound_at) VALUES ('mgrB','a2','unknown-level','2026-11-28T00:00:00Z')", []).unwrap();
    for (method, ctx) in [("autopilot.list", &f.mgr_a), ("autopilot.affecting_me", &f.emp)] {
        let result = scoped_payload(method, Box::pin(f.h.handle(method, json!({}), ctx)).await);
        let rules = result["rules"].as_array().unwrap();
        assert_eq!(rules.iter().find(|r| r["id"] == "unknown-creator").unwrap().get("creator_access_lost"), Some(&Value::Null));
        assert_eq!(rules.iter().find(|r| r["id"] == "sales-rule").unwrap()["creator_access_lost"], false);
    }
}
