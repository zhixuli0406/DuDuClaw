//! SR5 round 3 over a real F6 tombstone (`isolation_employee_rpc_tests`
//! fixture: alice owns the Personal employee `LABEL`, bob is an Admin).
//!
//! - N1: the incarnation rule at `action_grant::verify_and_consume` (the
//!   workflow `bound_human_approval` check is in `workflow_grants/tests.rs`).
//! - N2: NULL-incarnation cards of protected employees are refused at
//!   consumption and invalidated by the open-time migration.
//! - N3: the durable protected-mode latch with the environment flag absent.
//! - N5: member binds to a quarantined label are refused from the lifecycle
//!   index before auth is called.
use super::*;
use crate::approval::action_grant::{verify_and_consume, ActionGrant, GrantRefusal, ACTION_GRANT_KEY};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};

/// Protected mode forced on for this thread while the guard lives.
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

fn approve(home: &Path, id: &ApprovalId) {
    sql(home, "approvals.db", "UPDATE approvals SET status='approved' WHERE id=?1", &[&id.as_str()]);
}

fn bump_incarnation(home: &Path, to: i64) {
    sql(home, "migration-authority/apply.db", "UPDATE confirmed_employees SET incarnation=?1 WHERE label=?2", &[&to, &LABEL]);
}

/// A single-use, payload-bound card (no mail branch).
async fn grant_card(broker: &ApprovalBroker, agent: &str) -> (ActionGrant, ApprovalId) {
    let mut payload = json!({"arguments": {"url": "https://example.invalid/private"}});
    let grant = ActionGrant::build(agent, "web_extract", &payload);
    payload[ACTION_GRANT_KEY] = grant.to_json();
    let id = broker.request(agent, "mcp_call", "egress", payload, 3600).await.unwrap();
    (grant, id)
}

async fn reason(broker: &ApprovalBroker, id: &ApprovalId) -> (ApprovalStatus, Option<String>) {
    let record = broker.get(id).await.unwrap().unwrap();
    (record.status, record.invalidated_reason)
}

// ── N1: verify_and_consume checks the incarnation ───────────────────────

#[tokio::test]
async fn sr5r3_verify_and_consume_refuses_a_card_of_another_incarnation() {
    let f = fixture();
    let home = f.home();
    let _ = applied_personal(&f, 0xab, LABEL, &f.alice);
    let broker = ApprovalBroker::open(home).unwrap();
    let (grant, card) = {
        let _protected = Protected::on();
        grant_card(&broker, LABEL).await
    };
    approve(home, &card);
    // A same-id restore bumps the incarnation: refused and invalidated in
    // the same IMMEDIATE transaction.
    bump_incarnation(home, 1);
    assert_eq!(verify_and_consume(&broker, &card, &grant).await,
        Err(GrantRefusal::Store("employee_incarnation_changed".into())));
    assert_eq!(reason(&broker, &card).await,
        (ApprovalStatus::Invalidated, Some("employee_incarnation_changed".into())));
    assert_eq!(verify_and_consume(&broker, &card, &grant).await, Err(GrantRefusal::NotApproved));
    // A card of the current incarnation is consumed exactly once.
    let (grant, fresh) = {
        let _protected = Protected::on();
        grant_card(&broker, LABEL).await
    };
    approve(home, &fresh);
    assert_eq!(verify_and_consume(&broker, &fresh, &grant).await, Ok(()));
    assert_eq!(verify_and_consume(&broker, &fresh, &grant).await, Err(GrantRefusal::NotApproved));
}

#[tokio::test]
async fn sr5r3_verify_and_consume_refuses_a_tombstoned_employees_card() {
    let f = fixture();
    let home = f.home();
    let _ = applied_personal(&f, 0xab, LABEL, &f.alice);
    let broker = ApprovalBroker::open(home).unwrap();
    let (grant, card) = {
        let _protected = Protected::on();
        grant_card(&broker, LABEL).await
    };
    approve(home, &card);
    // Disable the confirmed row (the tombstone's lifecycle effect) without
    // the tombstone's own card sweep: consumption alone must refuse.
    let encoded: String = Connection::open(home.join("migration-authority/apply.db")).unwrap()
        .query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [LABEL], |r| r.get(0)).unwrap();
    let mut value: Value = serde_json::from_str(&encoded).unwrap();
    value["disabled"] = json!(true);
    sql(home, "migration-authority/apply.db", "UPDATE confirmed_employees SET receipt=?1 WHERE label=?2",
        &[&value.to_string(), &LABEL]);
    assert_eq!(verify_and_consume(&broker, &card, &grant).await,
        Err(GrantRefusal::Store("employee_incarnation_changed".into())));
    assert_eq!(reason(&broker, &card).await.0, ApprovalStatus::Invalidated);
}

// ── N2: NULL incarnation is never fail-open for a protected employee ────

#[tokio::test]
async fn sr5r3_null_incarnation_card_is_refused_for_a_protected_employee() {
    let f = fixture();
    let home = f.home();
    let _ = applied_personal(&f, 0xab, LABEL, &f.alice);
    let broker = ApprovalBroker::open(home).unwrap();
    // Created outside protected mode: no incarnation.
    let (grant, legacy) = grant_card(&broker, LABEL).await;
    let (team_grant, team) = grant_card(&broker, "team").await;
    approve(home, &legacy);
    approve(home, &team);
    let _protected = Protected::on();
    assert_eq!(verify_and_consume(&broker, &legacy, &grant).await,
        Err(GrantRefusal::Store("employee_incarnation_missing".into())));
    assert_eq!(reason(&broker, &legacy).await,
        (ApprovalStatus::Invalidated, Some("employee_incarnation_missing".into())));
    // A label without a confirmed row keeps the previous behaviour.
    assert_eq!(verify_and_consume(&broker, &team, &team_grant).await, Ok(()));
}

#[tokio::test]
async fn sr5r3_open_invalidates_unbound_cards_of_restored_or_tombstoned_employees() {
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    let (pending, approved, team) = {
        let broker = ApprovalBroker::open(home).unwrap();
        // Pre-SR5 cards: created without an incarnation.
        let pending = broker.request(LABEL, "browser_action", "s", json!({}), 3600).await.unwrap();
        let approved = broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap();
        let team = broker.request("team", "mail_send", "s", json!({}), 3600).await.unwrap();
        (pending, approved, team)
    };
    approve(home, &approved);
    approve(home, &team);
    let status = |id: ApprovalId| async move {
        let broker = ApprovalBroker::open(home).unwrap();
        broker.get(&id).await.unwrap().unwrap()
    };
    // Live at incarnation 0: the open leaves them (consumption decides).
    {
        let _protected = Protected::on();
        drop(ApprovalBroker::open(home).unwrap());
    }
    assert_eq!(status(pending.clone()).await.status, ApprovalStatus::Pending);
    assert_eq!(status(approved.clone()).await.status, ApprovalStatus::Approved);
    // Outside protected mode the open never touches them.
    bump_incarnation(home, 1);
    drop(ApprovalBroker::open(home).unwrap());
    assert_eq!(status(approved.clone()).await.status, ApprovalStatus::Approved);
    // Protected, restored as incarnation 1: only that employee's cards go.
    {
        let _protected = Protected::on();
        drop(ApprovalBroker::open(home).unwrap());
    }
    for id in [&pending, &approved] {
        let record = status(id.clone()).await;
        assert_eq!(record.status, ApprovalStatus::Invalidated);
        assert_eq!(record.invalidated_reason.as_deref(), Some("employee_incarnation_missing"));
    }
    assert_eq!(status(team.clone()).await.status, ApprovalStatus::Approved, "other agents untouched");
    // Idempotent; a later tombstone (disabled row) is covered the same way.
    let late = {
        let broker = ApprovalBroker::open(home).unwrap();
        broker.request(LABEL, "mail_send", "s", json!({}), 3600).await.unwrap()
    };
    bump_incarnation(home, 0);
    tombstoned(&f, &receipt);
    {
        let _protected = Protected::on();
        drop(ApprovalBroker::open(home).unwrap());
        drop(ApprovalBroker::open(home).unwrap());
    }
    assert_eq!(status(late).await.status, ApprovalStatus::Invalidated);
    assert_eq!(status(team).await.status, ApprovalStatus::Approved);
}

// ── N3: the durable protected-mode latch ────────────────────────────────

#[test]
fn sr5r3_protected_checks_hold_with_the_env_flag_absent_but_a_durable_fact() {
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    tombstoned(&f, &receipt);
    // The environment flag is not set by these tests; the thread sources below
    // carry no env value either (`env: None`).
    let system = duduclaw_core::protected_mode::active();
    if !system {
        assert!(!crate::runtime_readiness::is_protected_linux());
        assert_eq!(crate::isolation_label_stores::bind_refusal(home, LABEL), None);
    }
    let facts = tempfile::tempdir().unwrap();
    // (a) The root-owned mode marker.
    let marker = facts.path().join("protected-mode");
    std::fs::write(&marker, b"native-systemd\n").unwrap();
    let by_marker = duduclaw_core::protected_mode::Sources {
        env: None, marker, service_home: facts.path().join("no-home"),
    };
    // (b) The service home's confirmed employee index (this fixture's apply.db).
    let by_index = duduclaw_core::protected_mode::Sources {
        env: None, marker: facts.path().join("absent-marker"), service_home: home.to_path_buf(),
    };
    // A root-owned isolation fact (here the spawnd state directory).
    let isolation = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(isolation.path().join("var/lib/duduclaw-spawnd")).unwrap();
    for sources in [by_marker, by_index] {
        duduclaw_core::protected_mode::with_thread_sources(sources, || {
            assert!(crate::runtime_readiness::is_protected_linux());
            // Protected mode alone (a single-binary host): no isolation rules.
            if !duduclaw_core::isolation_install::installed() {
                assert!(!crate::runtime_readiness::isolation_active());
                assert!(!crate::isolation_label_stores::protected_active());
                assert!(!duduclaw_core::execution_gate::isolation_required());
                assert_eq!(crate::isolation_label_stores::bind_refusal(home, LABEL), None);
            }
            duduclaw_core::isolation_install::with_thread_root(isolation.path().to_path_buf(), || {
                assert!(crate::runtime_readiness::isolation_active());
                assert!(crate::isolation_label_stores::protected_active());
                assert!(duduclaw_core::execution_gate::isolation_required());
                // The writer gate and the bind refusal follow the latch.
                assert_eq!(crate::isolation_label_stores::check_label_writable(home, LABEL),
                    Err(crate::isolation_label_stores::GateRefusal::Quarantined));
                assert_eq!(crate::isolation_label_stores::bind_refusal(home, LABEL),
                    Some(crate::isolation_label_stores::LABEL_QUARANTINED));
            });
        });
    }
    assert_eq!(duduclaw_core::protected_mode::active(), system, "thread sources never latch the process");
}

// ── N5: binds refuse a quarantined label from the lifecycle index ───────

fn ok_payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected a result, got {other:?}"),
    }
}

#[tokio::test]
async fn sr5r3_binds_to_a_quarantined_label_are_refused_before_auth() {
    let f = fixture();
    let home = f.home();
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    tombstoned(&f, &receipt);
    assert_eq!(mirror(home, LABEL), None, "the users.db mirror is gone; auth alone cannot see the hold");
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let db = std::sync::Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    handler.set_user_db(db.clone(),
        std::sync::Arc::new(duduclaw_auth::JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    let admin = caller(&f.bob, UserRole::Admin);
    let _protected = Protected::on();
    // users.bind_agent
    let frame = handler.handle_users_bind_agent(
        json!({"user_id": f.alice, "agent_name": LABEL, "access_level": "viewer"}), &admin).await;
    assert_eq!(error_of(&frame), crate::isolation_label_stores::LABEL_QUARANTINED);
    assert_eq!(f.users.check_agent_access(&f.alice, LABEL).unwrap(), None);
    // users.create_members
    let live = db.live_user_context(&f.bob).unwrap().unwrap();
    let result = ok_payload(handler.handle_users_create_members(json!({"members": [{
        "email": "new@example.test", "display_name": "New", "role": "employee", "primary_agent_id": LABEL,
        "bindings": [{"agent_name": LABEL, "access_level": "operator"}], "invite": false}]}), &live).await);
    assert_eq!(result["results"][0]["error"], "member_agent_quarantined");
    assert!(f.users.list_users().unwrap().iter().all(|user| user.email != "new@example.test"));
    // users.offboard with a transfer: a stale binding (the auth guard alone
    // lets it through once the mirror is gone) is never moved.
    f.users.bind_agent(&f.alice, LABEL, AccessLevel::Viewer).unwrap();
    let frame = handler.handle_users_offboard(json!({"user_id": f.alice, "transfer_to": f.bob}), &admin).await;
    assert_eq!(error_of(&frame), crate::isolation_label_stores::LABEL_QUARANTINED);
    assert_eq!(f.users.get_user(&f.alice).unwrap().unwrap().status, UserStatus::Active);
    assert_eq!(f.users.check_agent_access(&f.bob, LABEL).unwrap(), None);
}
