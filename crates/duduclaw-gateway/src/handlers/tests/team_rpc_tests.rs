//! PR-E (people org UI): `users.my_team` returns the caller's reporting tree
//! and, per person, the employees the caller may call through them, live
//! from users.db, and nothing for an Employee or a Personal install.
use super::*;
use std::collections::HashMap;

fn seed_agent(home: &std::path::Path, name: &str, display: &str, status: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{display}"
role = "specialist"
status = "{status}"
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
"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

struct Fixture {
    handler: MethodHandler,
    db: Arc<UserDb>,
    store: Arc<TaskStore>,
    _home: tempfile::TempDir,
    admin: String,
    mgr: String,
    lead: String,
    emp1: String,
    emp2: String,
}

/// mgr (Manager) → lead (Manager) → emp1 (sales Owner, paused Owner);
/// mgr → emp2 (hr Viewer, archived Owner); other (finance Owner, no manager).
async fn fixture(edition: duduclaw_core::EditionProfile) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "finance", "財務小幫手", "active");
    seed_agent(home.path(), "sales", "業務專員", "active");
    seed_agent(home.path(), "hr", "人資助理", "active");
    seed_agent(home.path(), "paused-bot", "暫停中", "paused");
    seed_agent(home.path(), "old-bot", "已封存", "archived");
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let mk = |name: &str, role| {
        db.create_user(
            &format!("{name}@test.invalid"),
            name,
            "isolated-test-password",
            role,
        )
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
    db.bind_agent(&emp1, "paused-bot", AccessLevel::Owner)
        .unwrap();
    db.bind_agent(&emp2, "hr", AccessLevel::Viewer).unwrap();
    db.bind_agent(&emp2, "old-bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&other, "finance", AccessLevel::Owner)
        .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let jwt = Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"));
    handler.set_user_db(db.clone(), jwt).await;
    handler.set_edition_override(Some(edition)).await;
    let store = Arc::new(TaskStore::open(home.path()).unwrap());
    handler.set_task_store(store.clone()).await;
    Fixture {
        handler,
        db,
        store,
        _home: home,
        admin,
        mgr,
        lead,
        emp1,
        emp2,
    }
}

/// What the WS handshake builds: claims only (the live context replaces the
/// role, bindings and team per RPC).
fn jwt_ctx(user_id: &str, role: UserRole) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: user_id.to_string(),
        email: format!("{user_id}@test.invalid"),
        role,
        agent_access: HashMap::new(),
        must_change_password: false,
        team_access: HashMap::new(),
        acl_generation: 0,
    }
}

async fn my_team(f: &Fixture, ctx: &UserContext) -> WsFrame {
    f.handler.handle("users.my_team", json!({}), ctx).await
}

fn ok_payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok response, got {other:?}"),
    }
}

/// `user id → member` of a reply.
fn members(p: &Value) -> HashMap<String, Value> {
    p["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["id"].as_str().unwrap().to_string(), m.clone()))
        .collect()
}

fn agent_names(member: &Value) -> Vec<String> {
    member["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_string())
        .collect()
}

fn agent<'a>(member: &'a Value, name: &str) -> &'a Value {
    member["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("{name} not listed in {member}"))
}

async fn add_task(store: &TaskStore, id: &str, agent: &str, status: &str) {
    let mut row = TaskRow::new(
        id.to_string(),
        format!("t-{id}"),
        String::new(),
        "medium".to_string(),
        agent.to_string(),
        "someone".to_string(),
    );
    row.status = status.to_string();
    store.insert_task(&row).await.unwrap();
}

#[tokio::test]
async fn a_manager_sees_the_whole_tree_with_derived_levels_and_open_work() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    add_task(&f.store, "1", "sales", "todo").await;
    add_task(&f.store, "2", "sales", "in_progress").await;
    add_task(&f.store, "3", "sales", "done").await;
    let p = ok_payload(&my_team(&f, &jwt_ctx(&f.mgr, UserRole::Manager)).await);
    assert_eq!(p["manager"]["id"], json!(f.mgr));
    assert_eq!(p["max_depth"], 8);
    let m = members(&p);
    let mut ids: Vec<&String> = m.keys().collect();
    ids.sort();
    let mut want = vec![&f.lead, &f.emp1, &f.emp2];
    want.sort();
    assert_eq!(ids, want, "everyone below mgr, nobody else");
    assert_eq!(m[&f.lead]["depth"], 1);
    assert_eq!(m[&f.emp2]["depth"], 1);
    assert_eq!(m[&f.emp1]["depth"], 2);
    assert_eq!(m[&f.emp1]["manager_user_id"], json!(f.lead));

    let sales = agent(&m[&f.emp1], "sales");
    assert_eq!(sales["display_name"], "業務專員");
    assert_eq!(sales["call_text"], "@業務專員");
    assert_eq!(
        sales["level"], "operator",
        "an Owner below derives Operator"
    );
    assert_eq!(sales["assignable"], true);
    assert_eq!(sales["operational"], true);
    assert_eq!(sales["active_tasks"], 2, "done work is not counted");

    let paused = agent(&m[&f.emp1], "paused-bot");
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["operational"], false);
    assert_eq!(
        paused["assignable"], false,
        "a paused employee takes no new work"
    );

    let hr = agent(&m[&f.emp2], "hr");
    assert_eq!(hr["level"], "viewer");
    assert_eq!(hr["assignable"], false, "a Viewer-derived grant only talks");
    assert_eq!(hr["active_tasks"], 0);
    assert_eq!(
        agent_names(&m[&f.emp2]),
        vec!["hr".to_string()],
        "archived employees are not listed"
    );
    assert!(agent_names(&m[&f.lead]).is_empty());
    assert!(
        !p.to_string().contains("finance"),
        "a peer's employee never appears"
    );
}

#[tokio::test]
async fn each_manager_sees_only_their_own_subtree() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    let p = ok_payload(&my_team(&f, &jwt_ctx(&f.lead, UserRole::Manager)).await);
    let m = members(&p);
    assert_eq!(m.len(), 1);
    assert_eq!(m[&f.emp1]["depth"], 1);
    assert_eq!(agent(&m[&f.emp1], "sales")["assignable"], true);
}

#[tokio::test]
async fn an_employee_is_refused() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    let frame = my_team(&f, &jwt_ctx(&f.emp1, UserRole::Employee)).await;
    assert!(
        matches!(&frame, WsFrame::Response { ok: false, error: Some(e), .. }
            if e.to_string().contains("permission denied")),
        "{frame:?}"
    );
    // A stale JWT claiming Manager does not help: the role is re-read live.
    let frame = my_team(&f, &jwt_ctx(&f.emp1, UserRole::Manager)).await;
    assert!(
        matches!(&frame, WsFrame::Response { ok: false, .. }),
        "{frame:?}"
    );
}

#[tokio::test]
async fn an_admin_sees_their_own_reports() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    let p = ok_payload(&my_team(&f, &jwt_ctx(&f.admin, UserRole::Admin)).await);
    assert!(
        p["members"].as_array().unwrap().is_empty(),
        "nobody reports to admin yet"
    );
    f.db.set_manager(&f.mgr, Some(&f.admin)).unwrap();
    let p = ok_payload(&my_team(&f, &jwt_ctx(&f.admin, UserRole::Admin)).await);
    let m = members(&p);
    assert_eq!(m.len(), 4);
    assert_eq!(m[&f.emp1]["depth"], 3);
    assert_eq!(
        agent(&m[&f.emp2], "hr")["assignable"],
        true,
        "admin may assign anywhere"
    );
}

#[tokio::test]
async fn changes_reach_the_next_call_and_offboarded_people_drop_out() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);

    // Suspended people stay (their manager covers for them, Q3).
    f.db.set_user_status(&f.emp1, duduclaw_auth::UserStatus::Suspended)
        .unwrap();
    let m = members(&ok_payload(&my_team(&f, &ctx).await));
    assert_eq!(m[&f.emp1]["status"], "suspended");
    assert_eq!(agent(&m[&f.emp1], "sales")["assignable"], true);

    // Offboarded people are not part of the tree.
    f.db.set_user_status(&f.emp2, duduclaw_auth::UserStatus::Offboarded)
        .unwrap();
    let m = members(&ok_payload(&my_team(&f, &ctx).await));
    assert!(!m.contains_key(&f.emp2));

    // Clearing a manager removes the whole branch at once.
    f.db.set_manager(&f.lead, None).unwrap();
    let m = members(&ok_payload(&my_team(&f, &ctx).await));
    assert!(m.is_empty(), "{m:?}");
}

#[tokio::test]
async fn history_buttons_follow_the_live_oversight_gate_not_derived_call_access() {
    let f = fixture(duduclaw_core::EditionProfile::Enterprise).await;
    let config = f._home.path().join("config.toml");
    std::fs::write(&config, "[people]\nmanager_team_history = true\n").unwrap();
    let ctx = jwt_ctx(&f.mgr, UserRole::Manager);
    let p = ok_payload(&my_team(&f, &ctx).await);
    assert_eq!(p["history_enabled"], true);
    let m = members(&p);
    assert_eq!(agent(&m[&f.emp1], "sales")["history_readable"], true);
    assert_eq!(agent(&m[&f.emp2], "hr")["history_readable"], false);

    f.db.set_user_status(&f.emp1, duduclaw_auth::UserStatus::Suspended).unwrap();
    let m = members(&ok_payload(&my_team(&f, &ctx).await));
    assert_eq!(agent(&m[&f.emp1], "sales")["assignable"], true);
    assert_eq!(agent(&m[&f.emp1], "sales")["history_readable"], false);

    std::fs::write(&config, "[people]\nmanager_team_history = false\n").unwrap();
    let p = ok_payload(&my_team(&f, &ctx).await);
    assert_eq!(p["history_enabled"], false);
    for member in p["members"].as_array().unwrap() {
        for a in member["agents"].as_array().unwrap() {
            assert_eq!(a["history_readable"], false);
        }
    }
}

#[tokio::test]
async fn personal_edition_refuses_the_team_page() {
    let f = fixture(duduclaw_core::EditionProfile::Personal).await;
    for ctx in [
        jwt_ctx(&f.mgr, UserRole::Manager),
        jwt_ctx(&f.admin, UserRole::Admin),
    ] {
        let frame = my_team(&f, &ctx).await;
        match &frame {
            WsFrame::Response {
                ok: false,
                error: Some(e),
                ..
            } => {
                assert_eq!(e["code"], "enterprise_edition_required", "{frame:?}");
            }
            other => panic!("expected the edition refusal, got {other:?}"),
        }
    }
}
