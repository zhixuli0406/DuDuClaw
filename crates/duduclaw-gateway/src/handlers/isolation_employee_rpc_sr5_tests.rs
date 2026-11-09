//! SR5 items 1–3 over a real F6 tombstone (`isolation_employee_rpc_tests`
//! fixture: alice owns the Personal employee `LABEL`, bob is an Admin).
use super::*;
use crate::approval::{ApprovalBroker, ApprovalStatus};
use crate::personal_visibility::{self as visibility, Reader};

/// Protected mode forced on for this thread while the guard lives (the F6h
/// switch: writer gates, approval incarnations).
struct Protected;
impl Protected {
    fn on() -> Self {
        crate::isolation_label_stores::TEST_PROTECTED.with(|cell| cell.set(Some(true)));
        Protected
    }
}
impl Drop for Protected {
    fn drop(&mut self) { crate::isolation_label_stores::TEST_PROTECTED.with(|cell| cell.set(None)); }
}

fn tombstoned(f: &Fixture, receipt: &Receipt) {
    let now = 10_000;
    let input: TombstoneRequest = serde_json::from_value(json!({"label": LABEL,
        "employee_id": hex::encode(receipt.employee.0), "csrf_token": CSRF})).unwrap();
    tombstone(f.home(), Some(&proof(now)), &f.bob, input, now).unwrap();
}

fn set_seal(home: &Path, seal: &str) {
    sql(home, "migration-authority/apply.db", "UPDATE employee_quarantine SET seal=?1 WHERE label=?2", &[&seal, &LABEL]);
    visibility::invalidate_owner_cache(home);
}

fn skill_request(scope: &str, requester: &str) -> crate::install_requests::InstallRequest {
    crate::install_requests::InstallRequest {
        id: "sr5-request".into(), kind: "skill".into(), title: "tidy".into(), description: "d".into(),
        requester_id: requester.into(), requester_email: "x@example.test".into(), requester_role: "employee".into(),
        requester_department: None, risk_level: "Clean".into(), scan: json!([]),
        payload: json!({"scope": scope, "content": CLEAN_SKILL, "url": ""}),
        status: crate::install_requests::RequestStatus::Pending, manager_by: None, manager_at: None, admin_by: None,
        admin_at: None, decided_reason: None, executed: false, execute_error: None, created_at: String::new(),
        ttl_seconds: 3600,
    }
}

const CLEAN_SKILL: &str = "---\nname: tidy-skill\ndescription: A helpful guide\n---\n\n# Tidy Skill\n\nHelpful, harmless steps.\n";

// ── Item 1: skill installs never reach a tombstoned label ───────────────

#[tokio::test]
async fn sr5_skill_install_refuses_a_tombstoned_label_and_never_creates_its_tree() {
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    write_agent_dir(&home.join("agents"), LABEL);
    tombstoned(&f, &receipt);
    assert!(!home.join("agents").join(LABEL).exists(), "the tombstone moved the tree away");
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let _protected = Protected::on();
    // The dashboard path (`skills.install` → run_skill_install).
    let refused = handler.run_skill_install(LABEL, CLEAN_SKILL, "tidy-skill").await.unwrap_err();
    assert!(refused.contains(crate::isolation_label_stores::LABEL_QUARANTINED), "{refused}");
    assert!(!home.join("agents").join(LABEL).exists(), "agents/<label> is never created");
    // The approved-request paths (dashboard decision, channel press).
    let request = skill_request(LABEL, &f.alice);
    assert!(handler.execute_approved_install(&request, &f.bob).await.is_err());
    assert!(crate::install_notify::apply_install_request(home, &request, Some(&f.users), &f.bob).await.is_err());
    assert!(!home.join("agents").join(LABEL).exists());
    // An unknown label is refused the same way (no tree conjured).
    assert!(handler.run_skill_install("ghost-label", CLEAN_SKILL, "tidy-skill").await.is_err());
    assert!(!home.join("agents").join("ghost-label").exists());
    // After purge the label holds nothing, but there is still no live tree.
    set_seal(home, "purged");
    assert!(handler.run_skill_install(LABEL, CLEAN_SKILL, "tidy-skill").await.is_err());
    assert!(!home.join("agents").join(LABEL).exists());
}

// ── Item 2: privacy holds for the whole retention window ────────────────

#[test]
fn sr5_tombstoned_personal_employee_stays_private_until_purge_or_adoption() {
    use crate::handlers::personal_data_gate::{authorize_break_glass, Target, READ_ACTION, REFUSED_ACTION, STEP_UP_REQUIRED};
    use crate::personal_visibility::DataCategory;
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(Some(f.alice.clone())));
    tombstoned(&f, &receipt);
    assert_eq!(mirror(home, LABEL), None, "the users.db owner mirror is gone");
    // ... but the open quarantine row keeps the owner.
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(Some(f.alice.clone())));
    let Ok(Reader::BreakGlass { owner }) = visibility::classify_reader(home, true, &f.bob, true, LABEL) else {
        panic!("a non-owner Admin needs the audited step-up after the tombstone");
    };
    assert_eq!(owner, f.alice);
    assert_eq!(visibility::classify_reader(home, true, &f.alice, false, LABEL), Ok(Reader::Owner));
    // Cross-agent listings still hide it; AI callers are refused.
    assert!(visibility::hidden_from(home, true, &f.bob).unwrap().contains(LABEL));
    assert!(!visibility::hidden_from(home, true, &f.alice).unwrap().contains(LABEL));
    assert!(visibility::acl_denies(home, true, &f.bob, LABEL));
    assert_eq!(visibility::management_refusal(home, true, "team", LABEL), Some(visibility::PERSONAL_TARGET_REFUSED));
    let rows = json!({"rows": [{"agent_id": LABEL, "x": 1}, {"agent_id": "team", "x": 2}]});
    let hidden = visibility::hidden_from(home, true, &f.bob).unwrap();
    let mut filtered = rows.clone();
    assert_eq!(crate::handlers::personal_p1g_gate::drop_hidden_rows(&mut filtered, &hidden), 1);
    assert_eq!(filtered["rows"].as_array().unwrap().len(), 1);
    // Sessions / memory / timeline / mail: no step-up ⇒ refused (audited); with it ⇒ audited read.
    let now = chrono::Utc::now().timestamp();
    let fresh = proof(now - 60);
    for (method, category) in [("chat.sessions.list", DataCategory::Sessions), ("memory.search", DataCategory::Memory),
        ("timeline.list", DataCategory::Work), ("mail.list", DataCategory::Mail)] {
        let target = Target { method, category, agent: LABEL };
        assert_eq!(authorize_break_glass(&f.users, None, &f.bob, Some(&CSRF), now, &target, &owner),
            Err(STEP_UP_REQUIRED), "{method}");
        assert_eq!(authorize_break_glass(&f.users, Some(&fresh), &f.bob, Some(&CSRF), now, &target, &owner), Ok(()),
            "{method}");
    }
    assert_eq!(f.users.query_audit_log(Some(&f.bob), Some(READ_ACTION), 50).unwrap().len(), 4);
    assert_eq!(f.users.query_audit_log(Some(&f.bob), Some(REFUSED_ACTION), 50).unwrap().len(), 4);
    // Adoption (or purge, or restore) closes the row: the owner leaves the map.
    set_seal(home, "adopted");
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(None));
    assert!(!visibility::hidden_from(home, true, &f.bob).unwrap().contains(LABEL));
    set_seal(home, "sealed");
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(Some(f.alice.clone())));
    set_seal(home, "purged");
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(None));
    assert_eq!(visibility::management_refusal(home, true, "team", LABEL), None);
}

#[test]
fn sr5_unreadable_lifecycle_index_refuses_ownership() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    tombstoned(&f, &receipt);
    // The hardened opener refuses a group/other-writable apply.db.
    let apply = home.join("migration-authority").join("apply.db");
    std::fs::set_permissions(&apply, std::fs::Permissions::from_mode(0o666)).unwrap();
    visibility::invalidate_owner_cache(home);
    assert_eq!(visibility::personal_owner(home, LABEL), Err(visibility::Unavailable));
    assert!(visibility::hidden_from(home, true, &f.bob).is_err());
    assert_eq!(visibility::management_refusal(home, true, "team", LABEL), Some(visibility::OWNERSHIP_UNAVAILABLE));
    std::fs::set_permissions(&apply, std::fs::Permissions::from_mode(0o600)).unwrap();
    visibility::invalidate_owner_cache(home);
    assert_eq!(visibility::personal_owner(home, LABEL), Ok(Some(f.alice.clone())));
}

#[test]
fn sr5_approved_request_into_a_tombstoned_personal_employee_needs_its_owner() {
    use crate::handlers::personal_data_gate::{WRITE_ACTION, WRITE_REFUSED_ACTION};
    use crate::handlers::skills_install_target::{authorize_requested_write, RequestedWrite, REQUEST_NOT_OWNER};
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    tombstoned(&f, &receipt);
    // Someone else's request for it is refused even after the mirror is gone.
    let foreign = RequestedWrite { method: "skills.install_request", request_id: "r1", requester: &f.bob,
        decider: &f.bob, employee: LABEL };
    assert_eq!(authorize_requested_write(home, Some(&f.users), true, &foreign), Err(REQUEST_NOT_OWNER));
    assert_eq!(f.users.query_audit_log(Some(&f.bob), Some(WRITE_REFUSED_ACTION), 10).unwrap().len(), 1);
    // The owner's own request passes the ownership check, but the
    // live-employee check refuses the tombstoned label. SR5 round 3 (nit):
    // the config-write row is written only after that check succeeds, so no
    // WRITE_ACTION row precedes this refusal (it is audited as refused).
    let own = RequestedWrite { method: "skills.install_request", request_id: "r2", requester: &f.alice,
        decider: &f.bob, employee: LABEL };
    assert_eq!(authorize_requested_write(home, Some(&f.users), true, &own),
        Err(crate::handlers::skills_install_target::NOT_LIVE_TARGET));
    assert!(f.users.query_audit_log(Some(&f.alice), Some(WRITE_ACTION), 10).unwrap().is_empty());
    assert_eq!(f.users.query_audit_log(Some(&f.bob), Some(WRITE_REFUSED_ACTION), 10).unwrap().len(), 2);
}

// ── Item 3: approval cards ──────────────────────────────────────────────

fn approve(home: &Path, id: &crate::approval::ApprovalId) {
    sql(home, "approvals.db", "UPDATE approvals SET status='approved' WHERE id=?1", &[&id.as_str()]);
}

fn stored_incarnation(home: &Path, id: &crate::approval::ApprovalId) -> Option<i64> {
    Connection::open(home.join("approvals.db")).unwrap()
        .query_row("SELECT employee_incarnation FROM approvals WHERE id=?1", [id.as_str()], |row| row.get(0)).unwrap()
}

#[tokio::test]
async fn sr5_tombstone_invalidates_approved_but_unused_cards_of_every_kind() {
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    let broker = ApprovalBroker::open(home).unwrap();
    let (pending, approved, other) = {
        let _protected = Protected::on();
        let pending = broker.request(LABEL, "browser_action", "s", json!({}), 3600).await.unwrap();
        let approved = broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap();
        let other = broker.request("team", "mail_send", "s", json!({}), 3600).await.unwrap();
        (pending, approved, other)
    };
    approve(home, &approved);
    approve(home, &other);
    // New cards carry the employee incarnation (none for a label without a row).
    assert_eq!(stored_incarnation(home, &approved), Some(0));
    assert_eq!(stored_incarnation(home, &other), None);
    tombstoned(&f, &receipt);
    invalidate_employee_approvals(home, LABEL, receipt.employee).await;
    for id in [&pending, &approved] {
        assert_eq!(broker.get(id).await.unwrap().unwrap().status, ApprovalStatus::Invalidated);
    }
    assert_eq!(broker.get(&other).await.unwrap().unwrap().status, ApprovalStatus::Approved, "other employees untouched");
    // The invalidated approved card can never be consumed.
    assert_eq!(broker.consume_approved(&approved, "apply").await, Ok(false));
}

#[tokio::test]
async fn sr5_consume_refuses_a_card_of_another_incarnation() {
    let f = fixture();
    let home = f.home();
    let _ = applied_personal(&f, 0xab, LABEL, &f.alice);
    let broker = ApprovalBroker::open(home).unwrap();
    let card = {
        let _protected = Protected::on();
        broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap()
    };
    approve(home, &card);
    // A same-id restore bumps the incarnation: the old card is refused and invalidated.
    sql(home, "migration-authority/apply.db", "UPDATE confirmed_employees SET incarnation=1 WHERE label=?1", &[&LABEL]);
    assert_eq!(broker.consume_approved(&card, "apply").await, Err("employee_incarnation_changed".to_owned()));
    let record = broker.get(&card).await.unwrap().unwrap();
    assert_eq!(record.status, ApprovalStatus::Invalidated);
    assert_eq!(record.invalidated_reason.as_deref(), Some("employee_incarnation_changed"));
    // A card of the current incarnation is consumed exactly once.
    let fresh = {
        let _protected = Protected::on();
        broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap()
    };
    assert_eq!(stored_incarnation(home, &fresh), Some(1));
    approve(home, &fresh);
    assert_eq!(broker.consume_approved(&fresh, "apply").await, Ok(true));
    assert_eq!(broker.consume_approved(&fresh, "apply").await, Ok(false));
    // Cards created outside protected mode carry none. SR5 round 3 N2: such
    // a legacy card is refused (and invalidated) for a protected employee
    // once protected mode is on — NULL is never fail-open.
    let legacy = broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap();
    assert_eq!(stored_incarnation(home, &legacy), None);
    approve(home, &legacy);
    {
        let _protected = Protected::on();
        assert_eq!(broker.consume_approved(&legacy, "apply").await,
            Err("employee_incarnation_missing".to_owned()));
    }
    let record = broker.get(&legacy).await.unwrap().unwrap();
    assert_eq!(record.status, ApprovalStatus::Invalidated);
    assert_eq!(record.invalidated_reason.as_deref(), Some("employee_incarnation_missing"));
    // Outside protected mode a NULL card of a label without a confirmed row
    // behaves as before.
    let other = broker.request("team", "mail_send", "s", json!({}), 3600).await.unwrap();
    approve(home, &other);
    assert_eq!(broker.consume_approved(&other, "apply").await, Ok(true));
}
