//! v1.81 P1c: decisions on, delegation to, counts of and push events about
//! another person's Personal employee, over real users.db / tasks.db /
//! approvals.db stores (hardened opener, private test home). As in P1/P1b,
//! `protected` is passed in: no test sets the process-wide protected flag.
use super::*;
use super::super::personal_data_gate::{READ_ACTION, authorize_break_glass};
use super::super::personal_delegation_gate::{delegation_refusal, delegation_target_key};
use super::super::personal_push::withheld;
use super::super::push_filter::PreparedPush;
use crate::personal_decision::{card_employee, channel_card_refusal_with, decider_refusal};
use crate::personal_visibility::tests::{PERSONAL, SHARED, home, record_personal};
use crate::personal_visibility::{
    OWNERSHIP_UNAVAILABLE, PERSONAL_DECISION_REFUSED, PERSONAL_DELEGATION_REFUSED, PERSONAL_TARGET_REFUSED, Reader,
    Unavailable,
};
use duduclaw_auth::mfa::MigrationConfirmationSession;
use std::collections::BTreeSet;

/// The hardened opener refuses group/other-writable stores and sidecars.
fn private_store(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
        let path = home.join(name);
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

fn proof(verified_at: i64) -> MigrationConfirmationSession {
    MigrationConfirmationSession { verified_at: verified_at as u64, csrf: [7; 32] }
}

fn task(id: &str, agent: &str) -> TaskRow {
    let mut row = TaskRow::new(id.into(), "Quarterly report".into(), "secret description".into(),
        "normal".into(), agent.into(), "system".into());
    row.goal_mode = true;
    row.status = "review".into();
    row
}

fn activity(id: &str, agent: &str, task: Option<&str>) -> ActivityRow {
    ActivityRow {
        id: id.into(),
        event_type: "task_completed".into(),
        agent_id: agent.into(),
        task_id: task.map(str::to_owned),
        summary: "secret activity".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    }
}

/// A decision closure that must not run (Shared / unprotected cards never
/// look up the decider).
fn no_lookup() -> Option<String> {
    panic!("the decider is looked up only for a Personal employee's card")
}

// ── 1. approval decisions ───────────────────────────────────────────────

#[tokio::test]
async fn p1c_approval_decide_owner_or_audited_step_up() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let id = broker.request(PERSONAL, "dynamic_review_kind", "the owner's card", json!({}), 300).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(card_employee(h.path(), &rec), Ok(Some(PERSONAL.to_string())));

    // Dashboard. The owner decides as before.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    // The owner's manager (and any other non-Admin) is refused with the
    // decision text.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
    assert_eq!(decide_refusal_text(NOT_ALLOWED), PERSONAL_DECISION_REFUSED);
    // An Admin who is not the owner: without a step-up the refusal says one
    // is needed, and nothing is decided or audited as a read.
    let Ok(Reader::BreakGlass { owner }) =
        visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL) else {
        panic!("an Admin who is not the owner needs break-glass");
    };
    let target = Target { method: "approvals.decide", category: DataCategory::Approval, agent: PERSONAL };
    for (proof, csrf) in [(None, Some([7u8; 32])), (Some(proof(now)), None), (Some(proof(now - 301)), Some([7; 32]))] {
        let refused = authorize_break_glass(&h.db, proof.as_ref(), &h.admin, csrf.as_ref(), now, &target, &owner);
        assert_eq!(refused, Err(STEP_UP_REQUIRED));
        assert_eq!(decide_refusal_text(refused.unwrap_err()), DECIDE_STEP_UP_REQUIRED);
    }
    assert!(DECIDE_STEP_UP_REQUIRED.contains("step_up_csrf"));
    assert!(h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap().is_empty());
    // With a fresh step-up: allowed, audited as an `approval` decision.
    assert_eq!(authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, Some(&[7; 32]), now, &target, &owner),
        Ok(()));
    let rows = h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 1);
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["category"], json!("approval"));
    assert_eq!(detail["method"], json!("approvals.decide"));
    assert_eq!(detail["employee"], json!(PERSONAL));
    assert_eq!(detail["owner_user_id"], json!(h.alice));

    // Channels (presses, text replies, Mini App, bound requests): no
    // step-up exists, so the owner's verified account only.
    assert_eq!(decider_refusal(h.path(), true, PERSONAL, || Some(h.alice.clone())), None);
    for decider in [Some(h.admin.clone()), Some(h.bob.clone()), Some("system".to_string()), Some(String::new()), None] {
        assert_eq!(decider_refusal(h.path(), true, PERSONAL, || decider.clone()), Some(PERSONAL_DECISION_REFUSED));
    }
    // A sender with no verified dashboard account is not the owner (P1d:
    // told what an unknown card says, never whose card it is).
    assert_eq!(channel_card_refusal_with(h.path(), true, &rec, "telegram", "424242"),
        Some(crate::personal_decision::CHANNEL_CARD_NOT_FOUND));
    // Shared cards: unchanged, the decider is never looked up.
    assert_eq!(decider_refusal(h.path(), true, SHARED, no_lookup), None);
    let shared = broker.request(SHARED, "dynamic_review_kind", "shared", json!({}), 300).await.unwrap();
    let shared = broker.get(&shared).await.unwrap().unwrap();
    assert_eq!(channel_card_refusal_with(h.path(), true, &shared, "telegram", "424242"), None);

    // A legacy card with a blank employee resolves through its task.
    let store = TaskStore::open(h.path()).unwrap();
    store.insert_task(&task("personal-task", PERSONAL)).await.unwrap();
    let mut legacy = rec.clone();
    legacy.agent_id = String::new();
    legacy.payload = json!({"task_id": "personal-task"});
    assert_eq!(card_employee(h.path(), &legacy), Ok(Some(PERSONAL.to_string())));
    assert_eq!(channel_card_refusal_with(h.path(), true, &legacy, "telegram", "424242"),
        Some(crate::personal_decision::CHANNEL_CARD_NOT_FOUND));

    // Fail closed: no readable ownership mirror / task store.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(decider_refusal(empty.path(), true, PERSONAL, no_lookup), Some(OWNERSHIP_UNAVAILABLE));
    assert_eq!(card_employee(empty.path(), &legacy), Err(Unavailable));
    assert_eq!(channel_card_refusal_with(empty.path(), true, &legacy, "telegram", "424242"), Some(OWNERSHIP_UNAVAILABLE));
}

// ── 2. delegation ────────────────────────────────────────────────────────

#[test]
fn p1c_delegation_to_personal_employee_owner_only() {
    let h = home();
    // P1d: `tasks.assign` (destination `agent_id`) is delegation too.
    for (method, key) in [("tasks.create", "assigned_to"), ("tasks.update", "assigned_to"),
        ("tasks.goal_create", "agent_id"), ("plans.create", "agent_id"), ("tasks.assign", "agent_id")] {
        assert_eq!(delegation_target_key(method), Some(key), "{method}");
    }
    for method in ["tasks.list", "approvals.decide"] {
        assert_eq!(delegation_target_key(method), None, "{method}");
    }
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, bob, admin) = (ctx(h.alice.as_str()), ctx(h.bob.as_str()), ctx(h.admin.as_str()));
    // The owner hands work to its own Personal employee.
    assert_eq!(delegation_refusal(h.path(), true, &alice, PERSONAL), None);
    // Anyone else is refused — an Admin too (no break-glass for delegation),
    // and the legacy `system` context.
    assert_eq!(delegation_refusal(h.path(), true, &bob, PERSONAL), Some(PERSONAL_DELEGATION_REFUSED));
    assert_eq!(delegation_refusal(h.path(), true, &admin, PERSONAL), Some(PERSONAL_DELEGATION_REFUSED));
    assert_eq!(delegation_refusal(h.path(), true, &UserContext::admin_fallback(), PERSONAL),
        Some(PERSONAL_DELEGATION_REFUSED));
    // Shared employees keep the existing checks.
    for user in [&alice, &bob, &admin] {
        assert_eq!(delegation_refusal(h.path(), true, user, SHARED), None);
    }

    // MCP delegation: the employee itself, or another Personal employee of
    // the same owner (the owner's own work); never another person's.
    record_personal(h.path(), "alice-second", &h.alice);
    record_personal(h.path(), "bob-personal", &h.bob);
    private_store(h.path());
    assert_eq!(visibility::delegation_refusal(h.path(), true, PERSONAL, PERSONAL), None);
    assert_eq!(visibility::delegation_refusal(h.path(), true, "alice-second", PERSONAL), None);
    assert_eq!(visibility::delegation_refusal(h.path(), true, "bob-personal", PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    assert_eq!(visibility::delegation_refusal(h.path(), true, SHARED, PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    // Reads and management stay "the employee itself only".
    assert_eq!(visibility::management_refusal(h.path(), true, "alice-second", PERSONAL), Some(PERSONAL_TARGET_REFUSED));

    // Fail closed: no readable ownership mirror.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(delegation_refusal(empty.path(), true, &alice, PERSONAL), Some(OWNERSHIP_UNAVAILABLE));
    assert_eq!(visibility::delegation_refusal(empty.path(), true, SHARED, PERSONAL), Some(OWNERSHIP_UNAVAILABLE));
    // A revoked owner is re-read from users.db and refused.
    h.db.set_user_status(&h.alice, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert_eq!(delegation_refusal(h.path(), true, &alice, PERSONAL), Some(super::super::task_privacy::PERMISSION_DENIED));
}

// ── 3. counts ────────────────────────────────────────────────────────────

#[tokio::test]
async fn p1c_counts_exclude_hidden_personal_rows() {
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    let bob_principal = format!("user:{}", h.bob);
    for (id, agent) in [("p1", PERSONAL), ("p2", PERSONAL), ("s1", SHARED)] {
        let mut row = task(id, agent);
        if id != "p2" {
            row.on_behalf_of = Some(bob_principal.clone());
        }
        store.insert_task(&row).await.unwrap();
    }
    // A Personal row, a Shared row, and a Shared row about a Personal task.
    for (id, agent, task) in [("a-p", PERSONAL, None), ("a-s", SHARED, None), ("a-st", SHARED, Some("p1"))] {
        store.append_activity(&activity(id, agent, task)).await.unwrap();
    }
    let now = chrono::Utc::now().to_rfc3339();
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let alice = h.db.live_user_context(&h.alice).unwrap().unwrap();
    let principal = format!("user:{}", h.admin);

    // Admin who is not the owner, unscoped: totals and breakdowns cover only
    // what the Admin can list (a full page, not a short one).
    let reader = TaskReader::build(h.path(), &admin, true).unwrap();
    let hidden = reader.hidden().clone();
    assert_eq!(hidden, BTreeSet::from([PERSONAL.to_string()]));
    let (rows, total) = store.list_tasks_page_hiding(None, None, None, None, None, 1, 0, Some(principal.as_str()), &hidden)
        .await.unwrap();
    assert_eq!((total, rows.len(), rows[0].id.as_str()), (1, 1, "s1"));
    let (events, total) = store.list_activity_hiding(None, None, 50, 0, &principal, &hidden).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a-s"]);
    let metrics = store.flow_metrics_hiding(&now, &principal, &hidden).await.unwrap();
    assert!(metrics.agents.iter().all(|a| a.agent_id != PERSONAL));
    assert_eq!(metrics.review_queue_depth, 1);
    let (rows, total) = store.list_principal_page_hiding(&bob_principal, None, None, None, None, 50, 0, &hidden)
        .await.unwrap();
    assert_eq!((total, rows.len()), (1, 1));

    // The owner: nothing hidden, everything counted.
    let owner = TaskReader::build(h.path(), &alice, true).unwrap();
    assert!(owner.hidden().is_empty());
    let alice_principal = format!("user:{}", h.alice);
    let (_, total) = store.list_tasks_page_hiding(None, None, None, None, None, 50, 0, Some(alice_principal.as_str()),
        owner.hidden()).await.unwrap();
    assert_eq!(total, 3);
    let (_, total) = store.list_activity_hiding(None, None, 50, 0, &alice_principal, owner.hidden()).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(store.flow_metrics_hiding(&now, &alice_principal, owner.hidden()).await.unwrap().review_queue_depth, 3);

    // An Admin scope admitted by the audited step-up gate counts that employee.
    let scoped = TaskReader::build(h.path(), &admin, true).unwrap().admit_scope(Some(PERSONAL));
    let (_, total) = store.list_tasks_page_hiding(None, Some(PERSONAL), None, None, None, 50, 0, Some(principal.as_str()),
        scoped.hidden()).await.unwrap();
    assert_eq!(total, 2);

    // Unprotected: nothing hidden; the plain queries are unchanged.
    assert!(TaskReader::build(h.path(), &admin, false).unwrap().hidden().is_empty());
    assert_eq!(store.list_tasks_paginated(None, None, None, None, None, 50, 0).await.unwrap().1, 3);
    assert_eq!(store.list_activity_for_principal(None, None, 50, 0, Some(principal.as_str())).await.unwrap().1, 3);
    assert_eq!(store.flow_metrics(&now, &principal).await.unwrap().review_queue_depth, 3);
    assert_eq!(store.list_principal_page(&bob_principal, None, None, None, None, 50, 0).await.unwrap().0.len(), 2);
}

// ── 4. push events ───────────────────────────────────────────────────────

#[tokio::test]
async fn p1c_push_events_withheld_from_non_owner() {
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    store.insert_task(&task("personal-task", PERSONAL)).await.unwrap();
    store.insert_task(&task("shared-task", SHARED)).await.unwrap();
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let card = broker.request(PERSONAL, "dynamic_review_kind", "secret", json!({}), 300).await.unwrap();
    let empty = crate::fs_safe::private_test_home().unwrap();
    let prepare = |frame: WsFrame| {
        let home = h.path().to_path_buf();
        async move { PreparedPush::prepare(home, serde_json::to_string(&frame).unwrap()).await.expect("prepared") }
    };
    for frame in [
        WsFrame::event("task.updated", json!({"id": "personal-task", "assigned_to": PERSONAL})),
        WsFrame::event("goal.done", json!({"task_id": "personal-task"})),
        WsFrame::event("activity.new", json!({"agent_id": SHARED, "task_id": "personal-task"})),
        WsFrame::event("activity.new", json!({"agent_id": PERSONAL})),
        WsFrame::event("approval.created", json!({"id": card.as_str()})),
        WsFrame::event("agent.turn_started", json!({"agent_id": PERSONAL})),
    ] {
        let prepared = prepare(frame).await;
        // Not sent to anyone but the owner: Admin, manager, legacy context.
        for user in [h.admin.as_str(), h.bob.as_str(), "system"] {
            assert!(withheld(h.path(), true, user, &prepared), "{user}: {}", prepared.line);
        }
        assert!(!withheld(h.path(), true, &h.alice, &prepared), "{}", prepared.line);
        // Unreadable ownership mirror: withheld (fail closed).
        assert!(withheld(empty.path(), true, &h.alice, &prepared));
        // Unprotected: unchanged.
        assert!(!withheld(h.path(), false, &h.admin, &prepared));
    }
    // Shared employees' and system events are unchanged and read nothing.
    for frame in [
        WsFrame::event("task.updated", json!({"id": "shared-task", "assigned_to": SHARED})),
        WsFrame::event("system.status_changed", json!({"agent_id": PERSONAL})),
    ] {
        let prepared = prepare(frame).await;
        assert!(!withheld(h.path(), true, &h.admin, &prepared), "{}", prepared.line);
    }
    let system = prepare(WsFrame::event("system.status_changed", json!({}))).await;
    assert!(!withheld(empty.path(), true, &h.admin, &system));
}

// ── 5. unprotected installs ──────────────────────────────────────────────

#[tokio::test]
async fn p1c_unprotected_install_unchanged() {
    let h = home();
    let missing = Path::new("/nonexistent/duduclaw-home");
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let id = broker.request(PERSONAL, "dynamic_review_kind", "card", json!({}), 300).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    for home in [h.path(), missing] {
        assert_eq!(decider_refusal(home, false, PERSONAL, no_lookup), None);
        assert_eq!(channel_card_refusal_with(home, false, &rec, "telegram", "424242"), None);
        assert_eq!(delegation_refusal(home, false, &admin, PERSONAL), None);
        assert_eq!(visibility::delegation_refusal_for_user(home, false, &h.admin, PERSONAL), None);
        assert_eq!(visibility::delegation_refusal(home, false, SHARED, PERSONAL), None);
    }
    let frame = PreparedPush::prepare(h.path().to_path_buf(),
        serde_json::to_string(&WsFrame::event("approval.created", json!({"id": id.as_str()}))).unwrap())
        .await.unwrap();
    assert!(!withheld(missing, false, &h.admin, &frame));
    assert_eq!(DataCategory::Approval.as_str(), "approval");
}
