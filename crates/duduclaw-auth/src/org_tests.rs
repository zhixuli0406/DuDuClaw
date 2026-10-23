//! Reporting lines: `set_manager` refusals, the demotion guard, offboarding
//! reparent, effective access (recursion, `min(level, Operator)`, role and
//! status rules, hand-written cycles) and the ACL generation bump on every
//! authorization-relevant write.

use super::*;
use crate::models::{AccessLevel, UserRole, UserStatus};
use crate::org_write::Reparent;
use tempfile::NamedTempFile;

fn test_db() -> (UserDb, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let db = UserDb::new(tmp.path()).unwrap();
    (db, tmp)
}

fn person(db: &UserDb, name: &str, role: UserRole) -> String {
    db.create_user(&format!("{name}@test.invalid"), name, "isolated-test-password", role)
        .unwrap()
        .id
}

fn generation(db: &UserDb) -> u64 {
    db.acl_generation().unwrap()
}

/// admin; mgr (Manager); lead (Manager, under mgr); emp1 (under lead, sales
/// Owner); emp2 (under mgr, hr Viewer); other (finance Owner, no manager).
struct Org {
    mgr: String,
    lead: String,
    emp1: String,
    emp2: String,
    other: String,
}

fn org(db: &UserDb) -> Org {
    person(db, "admin", UserRole::Admin);
    let mgr = person(db, "mgr", UserRole::Manager);
    let lead = person(db, "lead", UserRole::Manager);
    let emp1 = person(db, "emp1", UserRole::Employee);
    let emp2 = person(db, "emp2", UserRole::Employee);
    let other = person(db, "other", UserRole::Employee);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.set_manager(&emp2, Some(&mgr)).unwrap();
    db.bind_agent(&emp1, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&emp2, "hr", AccessLevel::Viewer).unwrap();
    db.bind_agent(&other, "finance", AccessLevel::Owner).unwrap();
    Org { mgr, lead, emp1, emp2, other }
}

// ── set_manager refusals ─────────────────────────────────────────────

#[test]
fn set_manager_refuses_self_and_cycles() {
    let (db, _t) = test_db();
    let a = person(&db, "a", UserRole::Manager);
    let b = person(&db, "b", UserRole::Manager);
    let c = person(&db, "c", UserRole::Manager);
    assert_eq!(db.set_manager(&a, Some(&a)), Err(OrgError::SelfManager));
    assert_eq!(OrgError::SelfManager.code(), MANAGER_CYCLE);

    db.set_manager(&a, Some(&b)).unwrap();
    assert_eq!(db.set_manager(&b, Some(&a)), Err(OrgError::Cycle), "mutual");

    db.set_manager(&b, Some(&c)).unwrap();
    // a → b → c; c → a closes the loop.
    assert_eq!(db.set_manager(&c, Some(&a)), Err(OrgError::Cycle));
    assert_eq!(db.get_user(&c).unwrap().unwrap().manager_user_id, None);
}

#[test]
fn set_manager_refuses_missing_inactive_and_low_role_managers() {
    let (db, _t) = test_db();
    let u = person(&db, "u", UserRole::Employee);
    let emp = person(&db, "emp", UserRole::Employee);
    let gone = person(&db, "gone", UserRole::Manager);
    let away = person(&db, "away", UserRole::Manager);
    db.set_user_status(&gone, UserStatus::Offboarded).unwrap();
    db.set_user_status(&away, UserStatus::Suspended).unwrap();

    assert_eq!(db.set_manager(&u, Some("no-such-id")), Err(OrgError::ManagerNotFound));
    assert_eq!(db.set_manager(&u, Some(&gone)), Err(OrgError::ManagerInactive));
    assert_eq!(db.set_manager(&u, Some(&away)), Err(OrgError::ManagerInactive));
    assert_eq!(db.set_manager(&u, Some(&emp)), Err(OrgError::ManagerRoleTooLow));
    assert_eq!(OrgError::ManagerRoleTooLow.code(), MANAGER_ROLE);
    assert_eq!(db.set_manager("no-such-user", None), Err(OrgError::UserNotFound));
    // Admins can manage.
    let admin = person(&db, "boss", UserRole::Admin);
    db.set_manager(&u, Some(&admin)).unwrap();
    assert_eq!(db.get_user(&u).unwrap().unwrap().manager_user_id, Some(admin));
}

#[test]
fn set_manager_refuses_chains_deeper_than_eight() {
    let (db, _t) = test_db();
    // A chain of eight managers: m0 (top) … m7.
    let chain: Vec<String> = (0..8)
        .map(|i| person(&db, &format!("m{i}"), UserRole::Manager))
        .collect();
    for pair in chain.windows(2) {
        db.set_manager(&pair[1], Some(&pair[0])).unwrap();
    }
    let ninth = person(&db, "ninth", UserRole::Employee);
    assert_eq!(db.set_manager(&ninth, Some(&chain[7])), Err(OrgError::TooDeep));
    assert_eq!(OrgError::TooDeep.code(), MANAGER_TOO_DEEP);
    assert_eq!(db.check_manager_for_new_user(&chain[7]), Err(OrgError::TooDeep));
    assert!(db.check_manager_for_new_user(&chain[6]).is_ok());
    // Under m6 it fits (level 8).
    db.set_manager(&ninth, Some(&chain[6])).unwrap();

    // Moving a subtree: a two-level team under `sub` cannot go below m6.
    let sub = person(&db, "sub", UserRole::Manager);
    let leaf = person(&db, "leaf", UserRole::Employee);
    db.set_manager(&leaf, Some(&sub)).unwrap();
    assert_eq!(db.set_manager(&sub, Some(&chain[6])), Err(OrgError::TooDeep));
    db.set_manager(&sub, Some(&chain[5])).unwrap();
}

#[test]
fn clearing_a_manager_works_and_refused_writes_do_not_bump() {
    let (db, _t) = test_db();
    let o = org(&db);
    let before = generation(&db);
    assert!(db.set_manager(&o.mgr, Some(&o.emp1)).is_err());
    assert_eq!(generation(&db), before, "a refused write changes nothing");
    db.set_manager(&o.lead, None).unwrap();
    assert_eq!(db.get_user(&o.lead).unwrap().unwrap().manager_user_id, None);
    assert!(generation(&db) > before);
}

// ── demotion guard ───────────────────────────────────────────────────

#[test]
fn demoting_a_manager_with_reports_is_refused() {
    let (db, _t) = test_db();
    let o = org(&db);
    assert!(db.manager_has_reports(&o.lead).unwrap());
    let before = generation(&db);
    assert_eq!(
        db.update_user(&o.lead, Some("renamed"), Some(UserRole::Employee), None),
        Err(MANAGER_HAS_REPORTS.to_string())
    );
    let lead = db.get_user(&o.lead).unwrap().unwrap();
    assert_eq!(lead.role, UserRole::Manager);
    assert_eq!(lead.display_name, "lead", "nothing in the refused update applies");
    assert_eq!(generation(&db), before);

    // Promoting is fine; Manager → Admin keeps the reports.
    db.update_user(&o.lead, None, Some(UserRole::Admin), None).unwrap();
    db.update_user(&o.lead, None, Some(UserRole::Manager), None).unwrap();

    // Move emp1 away, then the demotion goes through.
    db.set_manager(&o.emp1, Some(&o.mgr)).unwrap();
    assert!(!db.manager_has_reports(&o.lead).unwrap());
    db.update_user(&o.lead, None, Some(UserRole::Employee), None).unwrap();
}

#[test]
fn an_offboarded_report_does_not_block_demotion() {
    let (db, _t) = test_db();
    let m = person(&db, "m", UserRole::Manager);
    let e = person(&db, "e", UserRole::Employee);
    db.set_manager(&e, Some(&m)).unwrap();
    db.set_user_status(&e, UserStatus::Offboarded).unwrap();
    assert!(!db.manager_has_reports(&m).unwrap());
    db.update_user(&m, None, Some(UserRole::Employee), None).unwrap();
}

// ── offboarding reparent ─────────────────────────────────────────────

#[test]
fn offboard_without_transfer_moves_reports_to_the_leavers_manager() {
    let (db, _t) = test_db();
    let o = org(&db);
    let out = db.offboard_user(&o.lead, None, Some("actor-1")).unwrap();
    assert_eq!(
        out.reparented,
        vec![Reparent { user_id: o.emp1.clone(), to: Some(o.mgr.clone()) }]
    );
    assert!(out.transferred_agents.is_empty());
    let lead = db.get_user(&o.lead).unwrap().unwrap();
    assert_eq!(lead.status, UserStatus::Offboarded);
    assert_eq!(lead.manager_user_id, None, "the leaver leaves the tree");
    assert_eq!(db.get_user(&o.emp1).unwrap().unwrap().manager_user_id, Some(o.mgr.clone()));

    let audit = db.query_audit_log(Some("actor-1"), Some("user.reparent"), 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].target.as_deref(), Some(o.emp1.as_str()));
    assert_eq!(
        audit[0].detail.as_deref(),
        Some(format!("from={} to={}", o.lead, o.mgr).as_str())
    );
    // mgr still reaches sales, now at depth 1.
    let access = db.effective_access(&o.mgr).unwrap().unwrap();
    assert_eq!(access.team["sales"].depth, 1);
}

#[test]
fn offboard_with_transfer_moves_reports_and_bindings_to_the_target() {
    let (db, _t) = test_db();
    let o = org(&db);
    let heir = person(&db, "heir", UserRole::Manager);
    db.bind_agent(&o.lead, "ops", AccessLevel::Operator).unwrap();
    let out = db.offboard_user(&o.lead, Some(&heir), None).unwrap();
    assert_eq!(out.transferred_agents, vec!["ops".to_string()]);
    assert_eq!(
        out.reparented,
        vec![Reparent { user_id: o.emp1.clone(), to: Some(heir.clone()) }]
    );
    assert_eq!(db.check_agent_access(&heir, "ops").unwrap(), Some(AccessLevel::Operator));
    assert_eq!(db.check_agent_access(&o.lead, "ops").unwrap(), None);
}

#[test]
fn offboard_refuses_an_unfit_explicit_target_and_changes_nothing() {
    let (db, _t) = test_db();
    let o = org(&db);
    let before = generation(&db);
    // `other` is an Employee: cannot take emp1 as a report.
    assert_eq!(
        db.offboard_user(&o.lead, Some(&o.other), None),
        Err(OrgError::ManagerRoleTooLow)
    );
    assert_eq!(db.get_user(&o.lead).unwrap().unwrap().status, UserStatus::Active);
    assert_eq!(generation(&db), before);
    assert_eq!(
        db.offboard_user(&o.lead, Some(&o.lead), None),
        Err(OrgError::TransferTargetInvalid)
    );
    assert_eq!(
        db.offboard_user(&o.lead, Some("nobody"), None),
        Err(OrgError::TransferTargetInvalid)
    );
    // Someone with no reports can hand bindings to an Employee, as before.
    db.offboard_user(&o.emp2, Some(&o.other), None).unwrap();
    assert_eq!(db.check_agent_access(&o.other, "hr").unwrap(), Some(AccessLevel::Viewer));
}

#[test]
fn offboard_puts_reports_on_top_when_the_fallback_manager_is_unfit() {
    let (db, _t) = test_db();
    let o = org(&db);
    db.set_user_status(&o.mgr, UserStatus::Suspended).unwrap();
    let out = db.offboard_user(&o.lead, None, None).unwrap();
    assert_eq!(out.reparented, vec![Reparent { user_id: o.emp1.clone(), to: None }]);
    assert_eq!(db.get_user(&o.emp1).unwrap().unwrap().manager_user_id, None);
}

#[test]
fn offboard_promotes_a_report_who_is_the_transfer_target() {
    let (db, _t) = test_db();
    let top = person(&db, "top", UserRole::Manager);
    let leaver = person(&db, "leaver", UserRole::Manager);
    let deputy = person(&db, "deputy", UserRole::Manager);
    let peer = person(&db, "peer", UserRole::Employee);
    db.set_manager(&leaver, Some(&top)).unwrap();
    db.set_manager(&deputy, Some(&leaver)).unwrap();
    db.set_manager(&peer, Some(&leaver)).unwrap();
    db.offboard_user(&leaver, Some(&deputy), None).unwrap();
    assert_eq!(db.get_user(&deputy).unwrap().unwrap().manager_user_id, Some(top));
    assert_eq!(db.get_user(&peer).unwrap().unwrap().manager_user_id, Some(deputy));
}

// ── effective access ─────────────────────────────────────────────────

#[test]
fn manager_reaches_two_levels_down_at_most_operator() {
    let (db, _t) = test_db();
    let o = org(&db);
    let a = db.effective_access(&o.mgr).unwrap().unwrap();
    assert!(a.direct.is_empty());
    assert_eq!(
        a.team["sales"],
        TeamGrant { level: AccessLevel::Operator, via_user: o.emp1.clone(), depth: 2 },
        "Owner below derives Operator, never Owner"
    );
    assert_eq!(a.team["hr"].level, AccessLevel::Viewer, "Viewer stays Viewer");
    assert!(!a.team.contains_key("finance"), "a peer's agents are not reachable");
    assert!(a.may_call("sales") && a.may_assign("sales"));
    assert!(a.may_call("hr") && !a.may_assign("hr"));
    assert!(!a.may_call("finance"));
    assert_eq!(
        a.level("sales").unwrap().1.label(),
        format!("team:{}:2", o.emp1)
    );

    let lead = db.effective_access(&o.lead).unwrap().unwrap();
    assert_eq!(lead.team.keys().collect::<Vec<_>>(), vec!["sales"]);
    assert_eq!(lead.team["sales"].depth, 1);
}

#[test]
fn team_grants_on_a_read_only_connection_match_effective_access() {
    let (db, t) = test_db();
    let o = org(&db);
    let ro = rusqlite::Connection::open_with_flags(
        t.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let grants = team_grants_on(&ro, &o.mgr).unwrap();
    let live = db.effective_access(&o.mgr).unwrap().unwrap();
    assert_eq!(grants, live.team);
    assert_eq!(grants["sales"].level, AccessLevel::Operator);
    assert_eq!(grants["sales"].via_user, o.emp1);
    assert_eq!(grants["sales"].depth, 2);
    assert_eq!(grants["hr"].level, AccessLevel::Viewer);
    assert!(!grants.contains_key("finance"));
    // Someone outside the tree reaches nothing.
    assert!(team_grants_on(&ro, &o.other).unwrap().is_empty());
}

#[test]
fn employees_get_no_team_and_direct_bindings_win_ties() {
    let (db, _t) = test_db();
    let o = org(&db);
    // emp1 is an Employee: someone reporting to them (by hand) relays nothing.
    let stray = person(&db, "stray", UserRole::Employee);
    db.bind_agent(&stray, "stray-agent", AccessLevel::Owner).unwrap();
    db.conn()
        .execute("UPDATE users SET manager_user_id = ?1 WHERE id = ?2", [&o.emp1, &stray])
        .unwrap();
    let e = db.effective_access(&o.emp1).unwrap().unwrap();
    assert!(e.team.is_empty());
    assert_eq!(e.level("sales"), Some((AccessLevel::Owner, GrantSource::Direct(AccessLevel::Owner))));
    // …and the stray link is not walked through emp1 for lead either.
    let lead = db.effective_access(&o.lead).unwrap().unwrap();
    assert!(!lead.team.contains_key("stray-agent"));

    // A direct Viewer binding is upgraded by an Operator team grant.
    db.bind_agent(&o.mgr, "sales", AccessLevel::Viewer).unwrap();
    let m = db.effective_access(&o.mgr).unwrap().unwrap();
    assert_eq!(m.level("sales").unwrap().0, AccessLevel::Operator);
    db.bind_agent(&o.mgr, "sales", AccessLevel::Operator).unwrap();
    let m = db.effective_access(&o.mgr).unwrap().unwrap();
    assert_eq!(m.level("sales").unwrap().1, GrantSource::Direct(AccessLevel::Operator));
}

#[test]
fn suspended_people_are_walked_through_and_their_bindings_count() {
    let (db, _t) = test_db();
    let o = org(&db);
    db.set_user_status(&o.lead, UserStatus::Suspended).unwrap();
    db.set_user_status(&o.emp2, UserStatus::Suspended).unwrap();
    let a = db.effective_access(&o.mgr).unwrap().unwrap();
    assert!(a.team.contains_key("sales"), "through the suspended middle manager");
    assert!(a.team.contains_key("hr"), "a suspended report's agents can be looked after");
    // The suspended lead gets nothing at all.
    assert!(db.effective_access(&o.lead).unwrap().is_none());
    assert!(db.live_user_context(&o.lead).unwrap().is_none());
}

#[test]
fn offboarded_people_are_excluded() {
    let (db, _t) = test_db();
    let o = org(&db);
    // Mark offboarded by hand (no reparent) to prove the read side excludes
    // them even when the tree still points at them.
    db.set_user_status(&o.emp2, UserStatus::Offboarded).unwrap();
    db.set_user_status(&o.lead, UserStatus::Offboarded).unwrap();
    let a = db.effective_access(&o.mgr).unwrap().unwrap();
    assert!(a.team.is_empty(), "{:?}", a.team);
    assert!(db.team_members(&o.mgr, MAX_ORG_DEPTH).unwrap().is_empty());
}

#[test]
fn hand_written_cycle_terminates() {
    let (db, _t) = test_db();
    let a = person(&db, "a", UserRole::Manager);
    let b = person(&db, "b", UserRole::Manager);
    let c = person(&db, "c", UserRole::Manager);
    db.bind_agent(&c, "c-agent", AccessLevel::Owner).unwrap();
    {
        let conn = db.conn();
        for (who, boss) in [(&a, &c), (&b, &a), (&c, &b)] {
            conn.execute("UPDATE users SET manager_user_id = ?1 WHERE id = ?2", [boss, who])
                .unwrap();
        }
    }
    let access = db.effective_access(&a).unwrap().unwrap();
    assert_eq!(access.team["c-agent"].depth, 2);
    let members = db.team_members(&a, MAX_ORG_DEPTH).unwrap();
    let ids: Vec<(&str, u8)> = members.iter().map(|(u, d)| (u.id.as_str(), *d)).collect();
    assert_eq!(ids, vec![(b.as_str(), 1), (c.as_str(), 2)]);
    // Writes refuse to extend it.
    assert!(db.set_manager(&a, Some(&b)).is_err());
}

#[test]
fn team_members_lists_the_tree_by_depth() {
    let (db, _t) = test_db();
    let o = org(&db);
    let members = db.team_members(&o.mgr, MAX_ORG_DEPTH).unwrap();
    let names: Vec<(&str, u8)> = members.iter().map(|(u, d)| (u.display_name.as_str(), *d)).collect();
    assert_eq!(names, vec![("emp2", 1), ("lead", 1), ("emp1", 2)]);
    assert_eq!(db.team_members(&o.mgr, 1).unwrap().len(), 2);
}

#[test]
fn live_user_context_reads_role_bindings_team_and_flag() {
    let (db, _t) = test_db();
    let o = org(&db);
    let ctx = db.live_user_context(&o.mgr).unwrap().unwrap();
    assert_eq!(ctx.role, UserRole::Manager);
    assert!(ctx.agent_access.is_empty());
    assert!(ctx.can_assign_to("sales"));
    assert!(!ctx.has_agent_access("sales", AccessLevel::Viewer));
    assert_eq!(ctx.acl_generation, generation(&db));
    assert!(db.live_user_context("nobody").unwrap().is_none());
    let other = db.live_user_context(&o.other).unwrap().unwrap();
    assert_eq!(other.agent_access.get("finance"), Some(&AccessLevel::Owner));
}

// ── generation bumps ─────────────────────────────────────────────────

#[test]
fn every_authorization_write_bumps_the_generation() {
    let (db, _t) = test_db();
    let o = org(&db);
    let heir = person(&db, "heir", UserRole::Manager);
    let mut last = generation(&db);
    let mut expect_bump = |label: &str, db: &UserDb| {
        let now = generation(db);
        assert!(now > last, "{label} must bump the ACL generation ({last} → {now})");
        last = now;
    };

    db.bind_agent(&o.other, "x", AccessLevel::Viewer).unwrap();
    expect_bump("bind_agent", &db);
    db.unbind_agent(&o.other, "x").unwrap();
    expect_bump("unbind_agent", &db);
    db.update_user(&o.other, None, Some(UserRole::Manager), None).unwrap();
    expect_bump("update_user(role)", &db);
    db.update_user(&o.other, None, None, Some("another-password")).unwrap();
    expect_bump("update_user(password)", &db);
    db.set_user_status(&o.other, UserStatus::Suspended).unwrap();
    expect_bump("set_user_status", &db);
    db.set_manager(&o.emp2, Some(&o.lead)).unwrap();
    expect_bump("set_manager", &db);
    db.bind_channel_identity(&o.emp1, "line", "U_emp1", true).unwrap();
    expect_bump("bind_channel_identity", &db);
    db.unbind_channel_identity("line", "U_emp1").unwrap();
    expect_bump("unbind_channel_identity", &db);
    db.change_email(&o.emp1, "emp1-new@test.invalid").unwrap();
    expect_bump("change_email", &db);
    db.offboard_user(&o.lead, Some(&heir), None).unwrap();
    expect_bump("offboard_user", &db);

    // A display-name-only edit is not authorization input.
    let before = generation(&db);
    db.update_user(&o.emp1, Some("Emp One"), None, None).unwrap();
    assert_eq!(generation(&db), before);
}

#[test]
fn claim_bumps_the_generation() {
    let (db, _t) = test_db();
    db.ensure_default_admin().unwrap().expect("admin created");
    let before = generation(&db);
    assert!(db.claim_default_admin_random().unwrap());
    assert!(generation(&db) > before);
    let after = generation(&db);
    assert!(!db.claim_default_admin_random().unwrap());
    assert_eq!(generation(&db), after, "a no-op claim does not bump");
}

#[test]
fn legacy_database_gains_manager_column_and_generation() {
    let tmp = NamedTempFile::new().unwrap();
    {
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, display_name TEXT NOT NULL,
                password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'employee',
                status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL, last_login TEXT);
             INSERT INTO users VALUES ('legacy', 'l@test.invalid', 'L', 'x', 'manager', 'active', 'a', 'a', NULL);",
        )
        .unwrap();
    }
    let db = UserDb::new(tmp.path()).unwrap();
    assert_eq!(db.acl_generation().unwrap(), 1);
    assert_eq!(db.get_user("legacy").unwrap().unwrap().manager_user_id, None);
    drop(db);
    // Reopening is idempotent and keeps the counter.
    let db = UserDb::new(tmp.path()).unwrap();
    assert_eq!(db.acl_generation().unwrap(), 1);
}

#[test]
fn a_missing_generation_row_is_an_error_and_a_write_recreates_it() {
    let (db, _t) = test_db();
    let u = person(&db, "u", UserRole::Employee);
    db.conn().execute("DELETE FROM auth_meta", []).unwrap();
    assert!(db.acl_generation().is_err());
    assert!(db.effective_access(&u).is_err(), "callers must narrow");
    db.bind_agent(&u, "a", AccessLevel::Viewer).unwrap();
    assert!(db.acl_generation().unwrap() >= 2);
}

// ── review fixes: atomic combined writes, binding merge, races ───────

#[test]
fn a_refused_manager_leaves_the_rest_of_the_update_untouched() {
    let (db, _t) = test_db();
    let o = org(&db);
    let before = generation(&db);
    let refused = db.apply_user_update(
        &o.mgr,
        &crate::UserUpdate {
            display_name: Some("renamed"),
            password: Some("a-brand-new-password"),
            department: Some(Some("sales")),
            manager: Some(Some(&o.emp1)),
            ..Default::default()
        },
    );
    assert_eq!(refused, Err(crate::UserWriteError::Org(OrgError::ManagerRoleTooLow)));
    let mgr = db.get_user(&o.mgr).unwrap().unwrap();
    assert_eq!((mgr.display_name.as_str(), mgr.department.as_deref()), ("mgr", None));
    assert!(db.verify_password("mgr@test.invalid", "isolated-test-password").is_ok());
    assert_eq!(generation(&db), before);

    // The same request with an acceptable manager applies every field.
    let heir = person(&db, "heir", UserRole::Manager);
    db.apply_user_update(
        &o.mgr,
        &crate::UserUpdate {
            display_name: Some("renamed"),
            department: Some(Some("sales")),
            manager: Some(Some(&heir)),
            ..Default::default()
        },
    )
    .unwrap();
    let mgr = db.get_user(&o.mgr).unwrap().unwrap();
    assert_eq!(mgr.display_name, "renamed");
    assert_eq!(mgr.department.as_deref(), Some("sales"));
    assert_eq!(mgr.manager_user_id, Some(heir));
    assert!(generation(&db) > before);
}

#[test]
fn create_with_a_refused_manager_creates_nothing() {
    let (db, _t) = test_db();
    let o = org(&db);
    let new = |manager| crate::NewUser {
        email: "new@test.invalid",
        display_name: "new",
        password: "new-password-1",
        role: UserRole::Employee,
        department: Some("sales"),
        manager,
    };
    assert_eq!(
        db.create_user_with(&new(Some(&o.emp1))).unwrap_err(),
        crate::UserWriteError::Org(OrgError::ManagerRoleTooLow)
    );
    assert!(db.get_user_by_email("new@test.invalid").unwrap().is_none());
    let created = db.create_user_with(&new(Some(&o.lead))).unwrap();
    let stored = db.get_user(&created.id).unwrap().unwrap();
    assert_eq!(stored, created);
    assert_eq!(stored.manager_user_id, Some(o.lead.clone()));
    assert!(matches!(
        db.create_user_with(&new(None)),
        Err(crate::UserWriteError::Other(_))
    ), "duplicate email is not an org error");
}

#[test]
fn offboard_transfer_keeps_the_higher_binding_level() {
    let (db, _t) = test_db();
    let leaver = person(&db, "leaver", UserRole::Employee);
    let heir = person(&db, "heir", UserRole::Employee);
    // Recipient already higher: kept.
    db.bind_agent(&leaver, "a", AccessLevel::Operator).unwrap();
    db.bind_agent(&heir, "a", AccessLevel::Owner).unwrap();
    // Recipient lower: raised.
    db.bind_agent(&leaver, "b", AccessLevel::Owner).unwrap();
    db.bind_agent(&heir, "b", AccessLevel::Viewer).unwrap();
    // Recipient had none: moved as is.
    db.bind_agent(&leaver, "c", AccessLevel::Viewer).unwrap();
    let out = db.offboard_user(&leaver, Some(&heir), None).unwrap();
    assert_eq!(out.transferred_agents, vec!["a", "b", "c"]);
    assert_eq!(db.check_agent_access(&heir, "a").unwrap(), Some(AccessLevel::Owner));
    assert_eq!(db.check_agent_access(&heir, "b").unwrap(), Some(AccessLevel::Owner));
    assert_eq!(db.check_agent_access(&heir, "c").unwrap(), Some(AccessLevel::Viewer));
    assert!(db.get_user_agents(&leaver).unwrap().is_empty());
}

#[test]
fn concurrent_opposite_assignments_cannot_both_win() {
    let tmp = NamedTempFile::new().unwrap();
    let setup = UserDb::new(tmp.path()).unwrap();
    for round in 0..20 {
        let a = person(&setup, &format!("a{round}"), UserRole::Manager);
        let b = person(&setup, &format!("b{round}"), UserRole::Manager);
        // Two independent pools on the same file, released together.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let spawn = |who: String, boss: String| {
            let path = tmp.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = UserDb::new(&path).unwrap();
                barrier.wait();
                db.set_manager(&who, Some(&boss))
            })
        };
        let t1 = spawn(a.clone(), b.clone());
        let t2 = spawn(b.clone(), a.clone());
        let results = [t1.join().unwrap(), t2.join().unwrap()];
        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "round {round}: {results:?}");
        assert!(results.contains(&Err(OrgError::Cycle)), "round {round}: {results:?}");
        let ma = setup.get_user(&a).unwrap().unwrap().manager_user_id;
        let mb = setup.get_user(&b).unwrap().unwrap().manager_user_id;
        assert!(ma.is_none() || mb.is_none(), "round {round}: no two-person loop");
    }
}

#[test]
fn oversight_keeps_every_active_operator_or_owner_report() {
    let (db, _t) = test_db();
    let o = org(&db);
    db.bind_agent(&o.emp2, "sales", AccessLevel::Operator).unwrap();
    db.bind_agent(&o.other, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&o.mgr, "sales", AccessLevel::Owner).unwrap();
    let reports = db.overseen_reports(&o.mgr, "sales").unwrap();
    assert_eq!(reports.len(), 2, "self and unrelated people are excluded");
    assert!(reports.iter().any(|r| r.user_id == o.emp1 && r.depth == 2 && r.level == AccessLevel::Owner));
    assert!(reports.iter().any(|r| r.user_id == o.emp2 && r.depth == 1 && r.level == AccessLevel::Operator));
    assert!(db.overseen_reports(&o.mgr, "hr").unwrap().is_empty(), "Viewer is not oversight authority");
    db.set_user_status(&o.emp2, UserStatus::Suspended).unwrap();
    assert_eq!(db.overseen_reports(&o.mgr, "sales").unwrap().len(), 1);
    db.set_manager(&o.emp1, None).unwrap();
    assert!(db.overseen_reports(&o.mgr, "sales").unwrap().is_empty(), "reassignment is immediate");
}

#[test]
fn oversight_walks_suspended_managers_but_excludes_inactive_reports_and_corrupt_bindings() {
    let (db, _t) = test_db();
    let o = org(&db);
    db.bind_agent(&o.lead, "sales", AccessLevel::Owner).unwrap();
    db.set_user_status(&o.lead, UserStatus::Suspended).unwrap();
    let reports = db.overseen_reports(&o.mgr, "sales").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].user_id, o.emp1);
    db.conn().execute("UPDATE user_agent_bindings SET access_level = 'unexpected' WHERE user_id = ?1", [&o.emp1]).unwrap();
    assert!(db.overseen_reports(&o.mgr, "sales").unwrap().is_empty());
}

#[test]
fn oversight_bounds_hand_written_cycles_and_denies_unreadable_storage() {
    let (db, _t) = test_db();
    let o = org(&db);
    db.conn().execute("UPDATE users SET manager_user_id = ?1 WHERE id = ?2", rusqlite::params![o.lead, o.mgr]).unwrap();
    let reports = db.overseen_reports(&o.mgr, "sales").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].user_id, o.emp1);
    db.conn().execute("DROP TABLE user_agent_bindings", []).unwrap();
    assert!(db.overseen_reports(&o.mgr, "sales").is_err());
}
