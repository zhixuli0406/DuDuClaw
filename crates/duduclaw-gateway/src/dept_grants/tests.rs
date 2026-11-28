use super::*;
use std::sync::Arc;

use duduclaw_auth::{JwtConfig, UserDb};
use duduclaw_core::EditionProfile;

/// Shared with later RPC matrix tests. Personal-only p1 needs an isolated
/// process: the production ACL restriction is a home-bound global OnceLock,
/// with no scoped test override. Do not install it in this parallel fixture.
pub(crate) struct Fixture {
    pub(crate) h: MethodHandler,
    pub(crate) db: Arc<UserDb>,
    pub(crate) home: PathBuf,
    pub(crate) admin: UserContext,
    pub(crate) mgr_a: UserContext,
    pub(crate) mgr_b: UserContext,
    pub(crate) emp: UserContext,
    _dir: tempfile::TempDir,
}

pub(crate) async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    std::fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    let mut org = String::from("schema = 1\n");
    for (id, department, role) in [
        ("a1", "sales", "specialist"), ("a2", "sales", "specialist"),
        ("b1", "mfg", "specialist"), ("main", "sales", "main"),
    ] {
        crate::trigger_route::tests::write_agent(&home.join("agents"), id, id, "", role, "");
        let path = home.join("agents").join(id).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("[agent]", &format!("[agent]\ndepartment = '{department}'"))).unwrap();
        org.push_str(&format!("\n[agents.{id}]\ndepartment = '{department}'\nreports_to = ''\n"));
    }
    std::fs::write(home.join("org.toml"), org).unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let add = |id: &str, role: UserRole| {
        let user = db.create_user(&format!("{id}@example.invalid"), id, "isolated-test-password", role).unwrap();
        rusqlite::Connection::open(home.join("users.db")).unwrap()
            .execute("UPDATE users SET id=?1 WHERE id=?2", rusqlite::params![id, user.id]).unwrap();
        let mut ctx = db.live_user_context(id).unwrap().unwrap();
        ctx.origin = ContextOrigin::DashboardSession;
        ctx
    };
    let admin = add("admin", UserRole::Admin);
    let mgr_a = add("mgrA", UserRole::Manager);
    let mgr_b = add("mgrB", UserRole::Manager);
    let emp = add("emp", UserRole::Employee);
    db.set_dept_grants(&admin.user_id, &mgr_a.user_id, "sales", &GrantGroup::ALL, true).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    h.set_test_edition(Some(EditionProfile::Enterprise)).await;
    h.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
    h.registry().write().await.scan().await.unwrap();
    Fixture { h, db, home, admin, mgr_a, mgr_b, emp, _dir: dir }
}

fn allows_create(view: &DeptGrantView, agent: &str) -> bool {
    department_grant_allows(view, agent, GrantGroup::Schedules, GrantAction::Create)
}

pub(crate) fn make_personal(f: &Fixture, agent: &str) -> UserContext {
    rusqlite::Connection::open(f.home.join("users.db")).unwrap().execute(
        "INSERT INTO isolation_personal_agents(agent_name, owner_user_id, created_at) VALUES (?1, ?2, 'now')",
        rusqlite::params![agent, f.mgr_a.user_id],
    ).unwrap();
    f.db.bind_agent(&f.mgr_a.user_id, agent, duduclaw_auth::AccessLevel::Owner).unwrap();
    crate::personal_visibility::invalidate_owner_cache(&f.home);
    let mut owner = f.db.live_user_context(&f.mgr_a.user_id).unwrap().unwrap();
    owner.origin = ContextOrigin::DashboardSession;
    owner
}

#[tokio::test]
async fn personal_owner_never_inherits_any_department_grant() {
    let f = fixture().await;
    let owner = make_personal(&f, "a1");
    let restriction = crate::personal_visibility::AclRestriction::with_flag(&f.home, true);
    assert!(owner.has_agent_access_under("a1", duduclaw_auth::AccessLevel::Owner, Some(&restriction)));
    let view = DeptGrantView::load(&f.h, &owner).await;
    assert_eq!(grant_target_department(&view, "a1"), Ok(None));
    for group in GrantGroup::ALL {
        for action in [GrantAction::Read, GrantAction::Create, GrantAction::Edit,
            GrantAction::Toggle, GrantAction::Remove, GrantAction::Restore,
            GrantAction::RunNow, GrantAction::RequestInstall, GrantAction::Decide] {
            assert!(!department_grant_allows(&view, "a1", group, action));
        }
        assert!(department_grant_allows(&view, "a2", group, GrantAction::Read));
    }
}

#[tokio::test]
async fn ownership_read_error_denies_all_department_grants() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a1"));
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute_batch("DROP TABLE isolation_personal_agents").unwrap();
    crate::personal_visibility::invalidate_owner_cache(&f.home);
    assert_eq!(grant_target_department(&view, "a1"), Err(()));
    for group in GrantGroup::ALL {
        assert!(!department_grant_allows(&view, "a1", group, GrantAction::Read));
    }
    assert!(!DeptGrantView::notification_allows(&f.home, &f.mgr_a.user_id, "a1"));
}

#[tokio::test]
async fn grants_exclude_other_departments_and_main_agents() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(!view.is_empty());
    assert!(allows_create(&view, "a1"));
    assert!(allows_create(&view, "a2"));
    for id in ["b1", "main", "../a1", "missing"] { assert!(!allows_create(&view, id), "{id}"); }
    assert_eq!(view.departments_for(GrantGroup::Schedules), vec!["sales".to_string()]);
    for ctx in [&f.admin, &f.mgr_b, &f.emp] {
        assert!(DeptGrantView::load(&f.h, ctx).await.is_empty());
    }
    assert!(!allows_create(&DeptGrantView::empty(), "a1"));
    assert!(DeptGrantView::empty().departments_for(GrantGroup::Cost).is_empty());
}

#[tokio::test]
async fn registry_missing_target_denied() {
    let f = fixture().await;
    // Create a valid target after the registry scan, with matching authority/mirror.
    crate::trigger_route::tests::write_agent(&f.home.join("agents"), "a3", "a3", "", "specialist", "");
    let path = f.home.join("agents/a3/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("[agent]", "[agent]\ndepartment = 'sales'")).unwrap();
    let org_path = f.home.join("org.toml");
    let org = std::fs::read_to_string(&org_path).unwrap();
    std::fs::write(org_path, format!("{org}\n[agents.a3]\ndepartment = 'sales'\nreports_to = ''\n")).unwrap();
    assert_eq!(duduclaw_agent::skill_loader::authoritative_department(&f.home, "a3"), Some("sales".into()));
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a1"));
    assert!(!allows_create(&view, "a3"));
}

#[tokio::test]
async fn role_changed_to_main_after_scan_denied() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a2"));
    let path = f.home.join("agents/a2/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("role = \"specialist\"", "role = \"main\"")).unwrap();
    assert!(!allows_create(&view, "a2"));
    assert!(!allows_create(&DeptGrantView::load(&f.h, &f.mgr_a).await, "a2"));
}

#[tokio::test]
async fn unreadable_agent_toml_denied() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a1"));
    std::fs::write(f.home.join("agents/a1/agent.toml"), "invalid TOML !").unwrap();
    assert!(!allows_create(&view, "a1"));
}

#[tokio::test]
async fn config_name_alias_not_accepted() {
    let f = fixture().await;
    let path = f.home.join("agents/a1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    let aliased = raw.replace("name = \"a1\"", "name = \"alias\"");
    std::fs::write(&path, &aliased).unwrap();
    f.h.registry().write().await.scan().await.unwrap();
    // Give the alias valid on-disk authority without adding its directory to
    // the registry, so rejection cannot depend on a missing file or mirror.
    std::fs::create_dir(f.home.join("agents/alias")).unwrap();
    std::fs::write(f.home.join("agents/alias/agent.toml"), aliased).unwrap();
    let org_path = f.home.join("org.toml");
    let org = std::fs::read_to_string(&org_path).unwrap();
    std::fs::write(org_path, format!("{org}\n[agents.alias]\ndepartment = 'sales'\nreports_to = ''\n")).unwrap();
    assert_eq!(duduclaw_agent::skill_loader::authoritative_department(&f.home, "alias"), Some("sales".into()));
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert_eq!(view.loaded_agents.get("a1").map(String::as_str), Some("alias"));
    assert!(allows_create(&view, "a1"));
    assert!(!allows_create(&view, "alias"));
}

#[tokio::test]
async fn actions_outside_constant_table_are_denied() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(!department_grant_allows(&view, "a1", GrantGroup::Cost, GrantAction::Edit));
    assert!(!department_grant_allows(&view, "a1", GrantGroup::Skills, GrantAction::Create));
}

#[tokio::test]
async fn user_department_change_grants_nothing() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET department='sales' WHERE id=?1", [&f.mgr_b.user_id]).unwrap();
    let view = DeptGrantView::load(&f.h, &f.mgr_b).await;
    assert!(view.is_empty());
    assert!(!allows_create(&view, "a1"));
}

#[tokio::test]
async fn agent_department_mirror_mismatch_denies() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a1"));
    let path = f.home.join("agents/a1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
    assert!(!allows_create(&view, "a1"));
}

#[tokio::test]
async fn revoked_grant_denies_next_rpc() {
    let f = fixture().await;
    assert!(allows_create(&DeptGrantView::load(&f.h, &f.mgr_a).await, "a1"));
    f.db.set_dept_grants(&f.admin.user_id, &f.mgr_a.user_id, "sales", &[], true).unwrap();
    assert!(!allows_create(&DeptGrantView::load(&f.h, &f.mgr_a).await, "a1"));
}

#[tokio::test]
async fn member_keys_chat_mcp_and_admin_tokens_have_no_grants() {
    let f = fixture().await;
    for origin in [ContextOrigin::MemberKey, ContextOrigin::Other, ContextOrigin::AdminToken] {
        let mut ctx = f.mgr_a.clone();
        ctx.origin = origin;
        assert!(DeptGrantView::load(&f.h, &ctx).await.is_empty(), "{origin:?}");
    }
    let mut ctx = f.mgr_a.clone();
    ctx.member_key_id = Some("key-id".into());
    assert!(DeptGrantView::load(&f.h, &ctx).await.is_empty());
    ctx.member_key_id = None;
    ctx.user_id = "system".into();
    assert!(DeptGrantView::load(&f.h, &ctx).await.is_empty());
}

#[tokio::test]
async fn personal_edition_view_is_empty() {
    let f = fixture().await;
    assert!(!DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
    f.h.set_edition_override(Some(EditionProfile::Personal)).await;
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn unreadable_org_toml_denies() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(allows_create(&view, "a1"));
    std::fs::write(f.home.join("org.toml"), "invalid TOML !").unwrap();
    assert!(!allows_create(&view, "a1"));
    std::fs::remove_file(f.home.join("org.toml")).unwrap();
    assert!(!allows_create(&DeptGrantView::load(&f.h, &f.mgr_a).await, "a1"));
}

#[tokio::test]
async fn manager_demoted_to_employee_has_empty_view() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET role='employee' WHERE id=?1", [&f.mgr_a.user_id]).unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn suspended_manager_has_empty_view() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.mgr_a.user_id]).unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn must_change_password_has_empty_view() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&f.mgr_a.user_id]).unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn credential_epoch_change_denies_stale_session() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.mgr_a.user_id]).unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn department_names_match_exact_case() {
    let f = fixture().await;
    f.db.set_dept_grants(&f.admin.user_id, &f.mgr_a.user_id, "sales", &[], true).unwrap();
    f.db.set_dept_grants(&f.admin.user_id, &f.mgr_a.user_id, "Sales", &GrantGroup::ALL, true).unwrap();
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert_eq!(view.departments_for(GrantGroup::Schedules), vec!["Sales".to_string()]);
    assert!(!allows_create(&view, "a1"));
}

#[tokio::test]
async fn unavailable_account_or_grant_store_denies() {
    let f = fixture().await;
    let mut missing = f.mgr_a.clone();
    missing.user_id = "missing".into();
    assert!(DeptGrantView::load(&f.h, &missing).await.is_empty());
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("DROP TABLE department_manager_grants", []).unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn default_directory_alias_and_main_role_are_excluded() {
    let f = fixture().await;
    // The default is a directory id; its config name differs. Both are excluded.
    let path = f.home.join("agents/a1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("name = \"a1\"", "name = \"alias\"")).unwrap();
    std::fs::write(f.home.join("config.toml"), "[general]\ndefault_agent = 'a1'\n").unwrap();
    f.h.registry().write().await.scan().await.unwrap();
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(view.main_agents.contains("alias"));
    for id in ["a1", "alias", "main"] { assert!(!allows_create(&view, id), "{id}"); }
    std::fs::write(f.home.join("config.toml"), "invalid TOML !").unwrap();
    assert!(DeptGrantView::load(&f.h, &f.mgr_a).await.is_empty());
}

#[tokio::test]
async fn user_principal_cannot_use_system_machine_shortcut() {
    let f = fixture().await;
    let view = DeptGrantView::load(&f.h, &f.mgr_a).await;
    let principal = format!("user:{}", f.mgr_a.user_id);
    assert_eq!(crate::on_behalf_of::PrincipalState::from_wire(Some(&principal)),
        crate::on_behalf_of::PrincipalState::Human(duduclaw_core::HumanPrincipal::User(f.mgr_a.user_id.clone())));
    assert!(!crate::on_behalf_of::access::system_operation_permitted(&f.home, Some(&principal)));
    assert!(allows_create(&view, "a1"));
    std::fs::write(duduclaw_core::preset::bindings_path(&f.home),
        "schema = 1\n[agents.a1]\npreset_id = 'system-operator'\n").unwrap();
    assert!(!allows_create(&view, "a1"));
}

#[tokio::test]
async fn grant_use_audits_mutations_but_not_reads() {
    let f = fixture().await;
    record_grant_use(&f.h, &f.mgr_a, "a1", GrantGroup::Skills, GrantAction::Read, "skills.list").await;
    record_grant_use(&f.h, &f.mgr_a, "a1", GrantGroup::Skills, GrantAction::Toggle, "skills.set_enabled").await;
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM auth_audit_log WHERE action='dept_grant_used'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    let (actor, target, detail): (String, String, String) = conn.query_row(
        "SELECT user_id, target, detail FROM auth_audit_log WHERE action='dept_grant_used'", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(actor, f.mgr_a.user_id);
    assert_eq!(target, "a1");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&detail).unwrap(),
        serde_json::json!({"group":"skills", "action":"toggle", "method":"skills.set_enabled"}));
}

#[test]
fn action_allowed_full_table() {
    use GrantAction::*;
    let actions = [Read, Create, Edit, Toggle, Remove, Restore, RunNow, RequestInstall, Decide];
    let expected = [
        (GrantGroup::Schedules,  [true, true, true, true, true, false, true, false, false]),
        (GrantGroup::Automation, [true, true, true, true, true, false, false, false, false]),
        (GrantGroup::Skills,     [true, false, false, true, true, true, false, true, false]),
        (GrantGroup::Cost,       [true, false, false, false, false, false, false, false, false]),
        (GrantGroup::EgressReview, [true, false, false, false, false, false, false, false, true]),
    ];
    for (group, row) in expected {
        for (action, allowed) in actions.into_iter().zip(row) {
            assert_eq!(action_allowed(group, action), allowed, "{group:?} {action:?}");
        }
    }
    for (action, name) in actions.into_iter().zip([
        "read", "create", "edit", "toggle", "remove", "restore", "run_now", "request_install", "decide",
    ]) { assert_eq!(action.as_str(), name); }
}

#[tokio::test]
async fn display_creator_access_lost_checks_accounts_bindings_and_exact_grant_targets() {
    let f = fixture().await;
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(None, "a1"), Some(false));
    assert_eq!(display.display_creator_access_lost(Some("missing"), "a1"), Some(true));
    assert_eq!(display.display_creator_access_lost(Some("admin"), "a1"), Some(false));
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), "a1"), Some(false));
    for agent in ["b1", "main", "../a1", "A1", "missing"] {
        assert_eq!(display.display_creator_access_lost(Some("mgrA"), agent), Some(true), "{agent}");
    }
    // A cache is request-local; the next request reads fresh account/binding facts.
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    conn.execute("INSERT INTO user_agent_bindings(user_id, agent_name, access_level, bound_at) VALUES ('emp','a1','operator','2026-11-27T00:00:00Z')", []).unwrap();
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("emp"), "a1"), Some(false));
    assert_eq!(display.display_creator_access_lost(Some("emp"), "a2"), Some(true));
    conn.execute("UPDATE users SET status='suspended' WHERE id='emp'", []).unwrap();
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("emp"), "a1"), Some(true));
    assert_eq!(display.creators.len(), 1);
}

#[tokio::test]
async fn display_creator_access_lost_preserves_unknown_reads_and_mirror_drift() {
    let f = fixture().await;
    let path = f.home.join("agents/a1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), "a1"), Some(true));
    std::fs::write(&path, &raw).unwrap();
    std::fs::write(f.home.join("org.toml"), "invalid TOML !").unwrap();
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), "a1"), None);
    assert_eq!(display.display_creator_access_lost(Some("admin"), "a1"), Some(false));
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("DROP TABLE department_manager_grants", []).unwrap();
    let mut display = DisplayCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), "a1"), None);
    assert_eq!(display.display_creator_access_lost(None, "a1"), Some(false));
}

#[tokio::test]
async fn display_autopilot_creator_access_checks_exact_automation_grants_and_accounts() {
    let f = fixture().await;
    let mut display = DisplayAutopilotCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(None, None), Some(false));
    assert_eq!(display.display_creator_access_lost(Some("missing"), Some("sales")), Some(true));
    for department in [None, Some("sales"), Some("mfg")] {
        assert_eq!(display.display_creator_access_lost(Some("admin"), department), Some(false));
    }
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), Some("sales")), Some(false));
    for department in [None, Some("mfg"), Some("Sales"), Some("sale"), Some("sales-extra")] {
        assert_eq!(display.display_creator_access_lost(Some("mgrA"), department), Some(true));
    }
    for user in ["mgrB", "emp", ""] {
        assert_eq!(display.display_creator_access_lost(Some(user), Some("sales")), Some(true));
    }
    // A direct binding never substitutes for Automation authority.
    f.db.bind_agent("mgrA", "a2", duduclaw_auth::AccessLevel::Owner).unwrap();
    f.db.set_dept_grants("admin", "mgrA", "sales", &[GrantGroup::Schedules], true).unwrap();
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), Some("sales")), Some(false));
    let mut fresh = DisplayAutopilotCreatorAccessCache::load(&f.h).await;
    assert_eq!(fresh.display_creator_access_lost(Some("mgrA"), Some("sales")), Some(true));
    assert_eq!(fresh.creators.len(), 1);
    f.db.set_dept_grants("admin", "mgrA", "sales", &[GrantGroup::Automation], true).unwrap();
    f.h.set_test_edition(Some(EditionProfile::Personal)).await;
    let mut personal = DisplayAutopilotCreatorAccessCache::load(&f.h).await;
    assert_eq!(personal.display_creator_access_lost(Some("mgrA"), Some("sales")), Some(true));
    assert_eq!(personal.display_creator_access_lost(Some("admin"), Some("sales")), Some(false));
    f.db.set_user_status("admin", UserStatus::Suspended).unwrap();
    f.db.set_user_status("mgrA", UserStatus::Suspended).unwrap();
    let mut suspended = DisplayAutopilotCreatorAccessCache::load(&f.h).await;
    for user in ["admin", "mgrA"] {
        assert_eq!(suspended.display_creator_access_lost(Some(user), Some("sales")), Some(true));
    }
}

#[tokio::test]
async fn display_autopilot_creator_access_preserves_unknown_and_legacy() {
    let f = fixture().await;
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute_batch("DROP TABLE department_manager_grants").unwrap();
    let mut display = DisplayAutopilotCreatorAccessCache::load(&f.h).await;
    assert_eq!(display.display_creator_access_lost(Some("mgrA"), Some("sales")), None);
    assert_eq!(display.display_creator_access_lost(None, Some("sales")), Some(false));
    assert_eq!(display.display_creator_access_lost(Some("admin"), None), Some(false));
    display.db = None;
    assert_eq!(display.display_creator_access_lost(Some("admin"), None), None);
    assert_eq!(display.display_creator_access_lost(None, None), Some(false));
}
