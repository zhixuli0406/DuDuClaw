//! PR-B (team callable): `agents.callable` lists exactly what the caller may
//! call (direct bindings, a Manager's team, the default and public
//! employees), and the task board lets a Manager hand work to a report's
//! employee (`can_assign_to`) without widening anything else.
use super::*;
use std::collections::HashMap;

fn seed_agent(home: &std::path::Path, name: &str, display: &str, role: &str, extra: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{display}"
role = "{role}"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-haiku-4-5"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
additional_mounts = []

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
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = true
{extra}
"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

struct Fixture {
    handler: MethodHandler,
    db: Arc<UserDb>,
    _home: tempfile::TempDir,
    admin: String,
    mgr: String,
    lead: String,
    emp1: String,
    emp2: String,
    other: String,
}

/// The design's live-test org: mgr (Manager) → lead (Manager) → emp1
/// (sales Owner); emp2 (under mgr, hr Viewer); other (finance Owner, no
/// manager). Employees: main (default), finance, sales, hr, faq (public).
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "main", "嘟嘟總機", "main", "");
    seed_agent(home.path(), "finance", "財務小幫手", "specialist", "");
    seed_agent(home.path(), "sales", "業務專員", "specialist", "");
    seed_agent(home.path(), "hr", "人資助理", "specialist", "");
    seed_agent(
        home.path(),
        "faq",
        "常見問題",
        "specialist",
        "\n[channels]\npublic_trigger = true\n",
    );
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let mk = |name: &str, role| {
        db.create_user(&format!("{name}@test.invalid"), name, "isolated-test-password", role)
            .unwrap()
            .id
    };
    let admin = mk("admin", UserRole::Admin);
    let mgr = mk("mgr", UserRole::Manager);
    let lead = mk("lead", UserRole::Manager);
    let emp1 = mk("emp1", UserRole::Employee);
    let emp2 = mk("emp2", UserRole::Employee);
    let other = mk("other", UserRole::Employee);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.set_manager(&emp2, Some(&mgr)).unwrap();
    db.bind_agent(&emp1, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&emp2, "hr", AccessLevel::Viewer).unwrap();
    db.bind_agent(&other, "finance", AccessLevel::Owner).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let jwt = Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"));
    handler.set_user_db(db.clone(), jwt).await;
    handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    handler
        .set_task_store(Arc::new(TaskStore::open(home.path()).unwrap()))
        .await;
    Fixture { handler, db, _home: home, admin, mgr, lead, emp1, emp2, other }
}

/// What the WS handshake builds: claims only, no team (the live context
/// replaces it per RPC).
fn jwt_ctx(user_id: &str, role: UserRole) -> UserContext {
    UserContext {
        user_id: user_id.to_string(),
        email: format!("{user_id}@test.invalid"),
        role,
        agent_access: HashMap::new(),
        must_change_password: false,
        team_access: HashMap::new(),
        acl_generation: 0,
    }
}

fn ok_payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn is_denied(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: false, error: Some(e), .. }
        if e.to_string().contains("permission denied"))
}

/// `name → entry` of an `agents.callable` reply.
async fn callable(f: &Fixture, ctx: &UserContext) -> HashMap<String, Value> {
    let p = ok_payload(&f.handler.handle("agents.callable", json!({}), ctx).await);
    p["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["name"].as_str().unwrap().to_string(), a.clone()))
        .collect()
}

fn names(map: &HashMap<String, Value>) -> Vec<String> {
    let mut v: Vec<String> = map.keys().cloned().collect();
    v.sort();
    v
}

#[tokio::test]
async fn the_list_differs_per_user_and_says_where_each_right_comes_from() {
    let f = fixture().await;
    let mgr = callable(&f, &jwt_ctx(&f.mgr, UserRole::Manager)).await;
    assert_eq!(names(&mgr), vec!["faq", "hr", "main", "sales"]);
    assert_eq!(mgr["sales"]["via"], "team");
    assert_eq!(mgr["sales"]["via_user"], json!(f.emp1));
    assert_eq!(mgr["sales"]["via_user_name"], "emp1");
    assert_eq!(mgr["sales"]["depth"], 2);
    assert_eq!(mgr["sales"]["level"], "operator", "Owner below derives Operator");
    assert_eq!(mgr["sales"]["assignable"], true);
    assert_eq!(mgr["sales"]["selectable"], true);
    assert_eq!(mgr["hr"]["level"], "viewer");
    assert_eq!(mgr["hr"]["assignable"], false, "a Viewer-derived grant only talks");
    assert_eq!(mgr["main"]["via"], "default");
    assert_eq!(mgr["main"]["selectable"], false);
    assert_eq!(mgr["faq"]["via"], "public");
    assert_eq!(mgr["faq"]["assignable"], false);
    assert_eq!(mgr["sales"]["call_text"], "@業務專員");

    let other = callable(&f, &jwt_ctx(&f.other, UserRole::Employee)).await;
    assert_eq!(names(&other), vec!["faq", "finance", "main"]);
    assert_eq!(other["finance"]["via"], "binding");
    assert_eq!(other["finance"]["level"], "owner");
    assert_eq!(other["finance"]["assignable"], true);

    let lead = callable(&f, &jwt_ctx(&f.lead, UserRole::Manager)).await;
    assert_eq!(names(&lead), vec!["faq", "main", "sales"]);
    assert_eq!(lead["sales"]["depth"], 1);

    // An Employee never gets a team, even with someone below them on file.
    let emp2 = callable(&f, &jwt_ctx(&f.emp2, UserRole::Employee)).await;
    assert_eq!(names(&emp2), vec!["faq", "hr", "main"]);

    let admin = callable(&f, &jwt_ctx(&f.admin, UserRole::Admin)).await;
    assert_eq!(names(&admin), vec!["faq", "finance", "hr", "main", "sales"]);
    assert!(admin.values().all(|a| a["via"] == "admin" && a["assignable"] == true));
}

#[tokio::test]
async fn a_manager_change_reaches_the_next_rpc_without_a_new_login() {
    let f = fixture().await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    assert!(callable(&f, &ctx).await.contains_key("sales"));
    f.db.set_manager(&f.lead, None).unwrap();
    assert!(!callable(&f, &ctx).await.contains_key("sales"));
    let create = f
        .handler
        .handle("tasks.create", json!({ "title": "報價", "assigned_to": "sales" }), &ctx)
        .await;
    assert!(is_denied(&create), "{create:?}");
}

#[tokio::test]
async fn a_manager_assigns_to_a_reports_employee_but_not_a_peers() {
    let f = fixture().await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    let created = f
        .handler
        .handle("tasks.create", json!({ "title": "整理報價單", "assigned_to": "sales" }), &ctx)
        .await;
    let task = ok_payload(&created)["task"].clone();
    assert_eq!(task["assigned_to"], "sales");
    let task_id = task["id"].as_str().unwrap().to_string();

    let refused = f
        .handler
        .handle("tasks.create", json!({ "title": "x", "assigned_to": "finance" }), &ctx)
        .await;
    assert!(is_denied(&refused), "{refused:?}");
    // A Viewer-derived grant can talk but not assign.
    let refused = f
        .handler
        .handle("tasks.create", json!({ "title": "x", "assigned_to": "hr" }), &ctx)
        .await;
    assert!(is_denied(&refused), "{refused:?}");

    // The board lists the employee the manager may assign to; content the
    // manager has no direct binding for comes back as a board card.
    let listed = ok_payload(
        &f.handler
            .handle("tasks.list", json!({ "agent_id": "sales" }), &ctx)
            .await,
    );
    let rows = listed["tasks"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!(task_id));
    assert_eq!(rows[0]["title"], "整理報價單");
    let page = f
        .handler
        .handle("tasks.list_page", json!({ "agent_id": "sales" }), &ctx)
        .await;
    assert!(!is_denied(&page), "{page:?}");
    for agent in ["finance", "hr"] {
        let refused = f
            .handler
            .handle("tasks.list", json!({ "agent_id": agent }), &ctx)
            .await;
        assert!(is_denied(&refused), "{agent}: {refused:?}");
    }

    // Moving a task onto a team employee needs Operator on its current
    // owner (direct), and assign rights on the destination.
    let other = jwt_ctx(&f.other, UserRole::Employee);
    let own = ok_payload(
        &f.handler
            .handle("tasks.create", json!({ "title": "帳", "assigned_to": "finance" }), &other)
            .await,
    )["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = f
        .handler
        .handle("tasks.assign", json!({ "task_id": own, "agent_id": "sales" }), &other)
        .await;
    assert!(is_denied(&refused), "a peer cannot hand work to mgr's team");
    let refused = f
        .handler
        .handle("tasks.assign", json!({ "task_id": own, "agent_id": "sales" }), &ctx)
        .await;
    assert!(is_denied(&refused), "mgr has no right on finance's task");
}

#[tokio::test]
async fn team_grants_do_not_open_memory_history_or_settings() {
    let f = fixture().await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    for (method, params) in [
        ("agents.status", json!({ "agent_id": "sales" })),
        ("memory.browse", json!({ "agent_id": "sales" })),
        ("agents.inspect", json!({ "agent_id": "sales" })),
    ] {
        let frame = f.handler.handle(method, params, &ctx).await;
        assert!(is_denied(&frame), "{method}: {frame:?}");
    }
    // agents.list stays direct bindings only.
    let listed = ok_payload(&f.handler.handle("agents.list", json!({}), &ctx).await);
    assert!(listed["agents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn agents_callable_is_not_enterprise_gated() {
    let f = fixture().await;
    f.handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Personal))
        .await;
    let system = UserContext::admin_fallback();
    let p = ok_payload(&f.handler.handle("agents.callable", json!({}), &system).await);
    assert_eq!(p["agents"].as_array().unwrap().len(), 5);
    assert_eq!(p["default_agent"], "main");
    assert!(!is_enterprise_only_method("agents.callable"));
}

// ── review round (#182) ─────────────────────────────────────────────

#[tokio::test]
async fn personal_edition_ignores_leftover_reporting_lines() {
    let f = fixture().await;
    // Enterprise → Personal downgrade: the users.db rows stay behind.
    f.handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Personal))
        .await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    let mgr = callable(&f, &ctx).await;
    assert_eq!(names(&mgr), vec!["faq", "main"], "no team grants in personal");
    let create = f
        .handler
        .handle("tasks.create", json!({ "title": "報價", "assigned_to": "sales" }), &ctx)
        .await;
    assert!(is_denied(&create), "{create:?}");
    let listed = f
        .handler
        .handle("tasks.list", json!({ "agent_id": "sales" }), &ctx)
        .await;
    assert!(is_denied(&listed), "{listed:?}");
    // The read-only task identity follows the recorded edition too.
    let live = crate::review_evidence::audience::trusted_dashboard_principal(f._home.path(), &f.mgr).unwrap();
    assert!(live.team_access.is_empty());
}

#[tokio::test]
async fn a_board_card_carries_no_conversation_link() {
    let f = fixture().await;
    let store = f.handler.task_store().await.unwrap();
    let mut row = TaskRow::new(
        "t-line".into(),
        "追款".into(),
        "客戶 A 的帳款".into(),
        "medium".into(),
        "sales".into(),
        f.emp1.clone(),
    );
    row.source_channel = Some("line".into());
    row.source_chat_id = Some("line:Ucustomer".into());
    store.insert_task(&row).await.unwrap();
    for method in ["tasks.list", "tasks.list_page"] {
        let mgr = ok_payload(
            &f.handler
                .handle(method, json!({ "agent_id": "sales" }), &jwt_ctx(&f.mgr, UserRole::Manager))
                .await,
        );
        let card = &mgr["tasks"][0];
        assert_eq!(card["restricted"], true, "{method}");
        assert_eq!(card["channel"], Value::Null, "{method}");
        assert_eq!(card["channel_link"], Value::Null, "{method}");
        assert_eq!(card["description"], "", "{method}");
        // The employee's own user still gets the channel.
        let own = ok_payload(
            &f.handler
                .handle(method, json!({ "agent_id": "sales" }), &jwt_ctx(&f.emp1, UserRole::Employee))
                .await,
        );
        assert_eq!(own["tasks"][0]["channel"], "line", "{method}");
    }
}

#[tokio::test]
async fn the_default_employee_follows_the_callers_conversation() {
    let f = fixture().await;
    seed_agent(
        f._home.path(),
        "desk",
        "櫃台",
        "specialist",
        "\n[channels.discord]\n[[channels.discord.bindings]]\nkind = \"thread\"\nid = \"T1\"\n",
    );
    f.handler.registry.write().await.scan().await.unwrap();
    let ctx = jwt_ctx(&f.other, UserRole::Employee);
    let fresh = ok_payload(&f.handler.handle("agents.callable", json!({}), &ctx).await);
    assert_eq!(fresh["default_agent"], "main");
    assert!(fresh["agents"].as_array().unwrap().iter().all(|a| a["name"] != "desk"));
    let bound = ok_payload(
        &f.handler
            .handle("agents.callable", json!({ "session_id": "webchat:thread:T1" }), &ctx)
            .await,
    );
    assert_eq!(bound["default_agent"], "desk");
    let desk = bound["agents"].as_array().unwrap().iter().find(|a| a["name"] == "desk").unwrap();
    assert_eq!(desk["via"], "default");
    // A non-WebChat session id is ignored.
    let other = ok_payload(
        &f.handler
            .handle("agents.callable", json!({ "session_id": "discord:thread:T1" }), &ctx)
            .await,
    );
    assert_eq!(other["default_agent"], "main");
}

#[tokio::test]
async fn a_manager_manages_tasks_they_created_for_the_team() {
    let f = fixture().await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    let id = ok_payload(
        &f.handler
            .handle("tasks.create", json!({ "title": "整理報價單", "assigned_to": "sales" }), &ctx)
            .await,
    )["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let renamed = f
        .handler
        .handle("tasks.update", json!({ "task_id": id, "title": "整理本月報價單" }), &ctx)
        .await;
    assert_eq!(ok_payload(&renamed)["task"]["title"], "整理本月報價單");
    // `cancelled` is a terminal task status (dashboard `TaskStatus`, the
    // goal loop and the stop path treat it as finished).
    let cancelled = ok_payload(
        &f.handler
            .handle("tasks.update", json!({ "task_id": id, "status": "cancelled" }), &ctx)
            .await,
    );
    assert_eq!(cancelled["task"]["status"], "cancelled");
    // Someone else's task on the same employee still needs direct Operator.
    let theirs = ok_payload(
        &f.handler
            .handle("tasks.create", json!({ "title": "x", "assigned_to": "sales" }), &jwt_ctx(&f.emp1, UserRole::Employee))
            .await,
    )["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = f
        .handler
        .handle("tasks.update", json!({ "task_id": theirs, "title": "y" }), &ctx)
        .await;
    assert!(is_denied(&refused), "{refused:?}");
    let refused = f.handler.handle("tasks.remove", json!({ "task_id": theirs }), &ctx).await;
    assert!(is_denied(&refused), "{refused:?}");
    // Once the line is cut the manager loses the task too.
    f.db.set_manager(&f.lead, None).unwrap();
    let refused = f
        .handler
        .handle("tasks.update", json!({ "task_id": id, "title": "z" }), &ctx)
        .await;
    assert!(is_denied(&refused), "{refused:?}");
    f.db.set_manager(&f.lead, Some(&f.mgr)).unwrap();
    let removed = f.handler.handle("tasks.remove", json!({ "task_id": id }), &ctx).await;
    assert!(!is_denied(&removed), "{removed:?}");
}
