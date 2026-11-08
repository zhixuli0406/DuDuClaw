//! v1.81 P1: Admin break-glass reads of a Personal employee's private data.
use super::*;
use crate::personal_visibility::tests::{PERSONAL, SHARED, home};

fn proof(verified_at: i64) -> MigrationConfirmationSession {
    MigrationConfirmationSession { verified_at: verified_at as u64, csrf: [7; 32] }
}

#[test]
fn p1_admin_read_requires_step_up_and_audits() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    let Ok(Reader::BreakGlass { owner }) =
        visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL) else {
        panic!("an Admin who is not the owner needs break-glass");
    };
    assert_eq!(owner, h.alice);
    let target = Target { method: "memory.search", category: DataCategory::Memory, agent: PERSONAL };

    // Refused without a fresh, session-bound step-up and its CSRF token.
    let refusals: [(Option<MigrationConfirmationSession>, Option<[u8; 32]>); 5] = [
        (None, Some([7; 32])),            // no step-up proof
        (Some(proof(now)), None),         // proof but no token in the request
        (Some(proof(now)), Some([8; 32])), // wrong token
        (Some(proof(now - 301)), Some([7; 32])), // older than five minutes
        (Some(proof(now + 1)), Some([7; 32])),   // from the future
    ];
    for (proof, csrf) in &refusals {
        assert_eq!(
            authorize_break_glass(&h.db, proof.as_ref(), &h.admin, csrf.as_ref(), now, &target, &owner),
            Err(STEP_UP_REQUIRED)
        );
    }
    assert!(h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap().is_empty(),
        "nothing was read, so no read row");
    assert_eq!(h.db.query_audit_log(Some(&h.admin), Some(REFUSED_ACTION), 50).unwrap().len(), refusals.len());

    // Fresh step-up + token: allowed, and the audit row names the Admin, the
    // employee, the owner and the data category.
    assert_eq!(
        authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, Some(&[7; 32]), now, &target, &owner),
        Ok(())
    );
    let rows = h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target.as_deref(), Some(PERSONAL));
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["admin_user_id"], json!(h.admin));
    assert_eq!(detail["employee"], json!(PERSONAL));
    assert_eq!(detail["owner_user_id"], json!(h.alice));
    assert_eq!(detail["category"], json!("memory"));
    assert_eq!(detail["method"], json!("memory.search"));

    // The owner and Shared employees never need a step-up; the owner's
    // manager is refused outright.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    assert_eq!(visibility::classify_reader(h.path(), true, &h.admin, true, SHARED), Ok(Reader::Unrestricted));
    assert_eq!(visibility::classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
}

#[test]
fn p1_gated_methods_and_step_up_field() {
    use DataCategory::*;
    for (method, category) in [
        ("memory.search", Memory), ("memory.browse", Memory), ("memory.graph", Memory), ("memory.forget", Memory),
        ("wiki.pages", Wiki), ("wiki.read", Wiki), ("wiki.search", Wiki),
        ("agent_files.list", AgentFiles), ("agent_files.get", AgentFiles), ("agent_files.history", AgentFiles),
        ("agents.inspect", AgentFiles), ("contract.get", AgentFiles),
        ("chat.sessions.list", Sessions), ("search.query", Sessions), ("runs.list", Sessions),
    ] {
        assert_eq!(agent_scoped(method), Some(category), "{method}");
    }
    for method in ["agents.list", "shared_wiki.read", "system.status", "chat.sessions.history"] {
        assert_eq!(agent_scoped(method), None, "{method}");
    }
    // The token is taken out of the params; a handler never sees it.
    assert_eq!(STEP_UP_FIELD, "step_up_csrf");
    let mut params = json!({"agent_id": PERSONAL, "step_up_csrf": serde_json::to_value([7u8; 32]).unwrap()});
    assert_eq!(take_step_up(&mut params), Some([7; 32]));
    assert!(params.get(STEP_UP_FIELD).is_none());
    assert_eq!(params["agent_id"], json!(PERSONAL));
    let mut malformed = json!({"step_up_csrf": "not-a-token"});
    assert_eq!(take_step_up(&mut malformed), None);
    assert!(malformed.get(STEP_UP_FIELD).is_none());
}

// ── v1.81 P1b: the work record (tasks, activity, timeline, decisions,
// canvas, approvals) ─────────────────────────────────────────────────────

fn row(id: &str, agent: &str) -> TaskRow {
    let mut row = TaskRow::new(id.into(), "Quarterly report".into(), "secret description".into(),
        "normal".into(), agent.into(), "system".into());
    row.result_summary = Some("secret result".into());
    row
}

fn activity(agent: &str, task: Option<&str>) -> ActivityRow {
    ActivityRow {
        id: format!("act-{agent}"),
        event_type: "task_completed".into(),
        agent_id: agent.into(),
        task_id: task.map(str::to_owned),
        summary: "secret activity".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    }
}

#[test]
fn p1b_work_methods_gated_by_agent_or_task() {
    // Lists and agent-scoped reads: gated on `agent_id` (scoped) and hidden
    // through TaskReader (unscoped).
    for method in ["activity.list", "timeline.list", "decisions.list", "decisions.dismiss", "canvas.get",
        "tasks.list", "tasks.list_page", "tasks.flow_metrics", "tasks.assign"] {
        assert_eq!(agent_scoped(method), Some(DataCategory::Work), "{method}");
    }
    // Single-task gets and writes: gated on the task's employee.
    for method in ["tasks.update", "tasks.remove", "tasks.assign", "tasks.archive", "tasks.unarchive", "tasks.pin",
        "tasks.unpin", "tasks.rename", "tasks.comment", "tasks.comments", "tasks.iterations", "tasks.timeline",
        "tasks.changes", "tasks.artifacts", "tasks.role_turns", "tasks.goal_decide", "activity.list"] {
        assert!(task_scoped(method), "{method}");
    }
    // Approvals are decided per card in the handler; other methods untouched.
    for method in ["approvals.list", "approvals.get", "tasks.create", "agents.list", "chat.sessions.history"] {
        assert!(!task_scoped(method), "{method}");
    }
    assert_eq!(agent_scoped("approvals.list"), None);
}

#[tokio::test]
async fn p1b_task_employee_read_before_the_handler() {
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    store.insert_task(&row("personal-task", PERSONAL)).await.unwrap();
    store.insert_task(&row("shared-task", SHARED)).await.unwrap();
    assert_eq!(task_employee(h.path(), "personal-task"), Ok(Some(PERSONAL.to_string())));
    assert_eq!(task_employee(h.path(), "shared-task"), Ok(Some(SHARED.to_string())));
    // Unknown task: the handler answers (not found / permission denied).
    assert_eq!(task_employee(h.path(), "missing"), Ok(None));
    // No readable store: the gate refuses (never "not Personal").
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(task_employee(empty.path(), "personal-task"), Err(()));
    // The gate then classifies the task's employee exactly like any other
    // Personal read: owner passes, Admin needs break-glass, manager refused.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    assert!(matches!(visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL),
        Ok(Reader::BreakGlass { .. })));
    assert_eq!(visibility::classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
}

#[test]
fn p1b_admin_work_read_is_audited_as_work() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    for method in ["tasks.comments", "activity.list", "timeline.list", "decisions.list", "canvas.get", "approvals.get"] {
        let target = Target { method, category: DataCategory::Work, agent: PERSONAL };
        assert_eq!(authorize_break_glass(&h.db, None, &h.admin, Some(&[7; 32]), now, &target, &h.alice),
            Err(STEP_UP_REQUIRED), "{method}: no step-up, no read");
        assert_eq!(authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, Some(&[7; 32]), now, &target,
            &h.alice), Ok(()), "{method}");
    }
    let rows = h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 6);
    for row in rows {
        let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["category"], json!("work"));
        assert_eq!(detail["employee"], json!(PERSONAL));
    }
}

#[test]
fn p1b_task_reader_hides_personal_rows_from_non_owner() {
    let h = home();
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let alice = h.db.live_user_context(&h.alice).unwrap().unwrap();
    let personal = row("personal-task", PERSONAL);

    // Admin (not the owner), unscoped list: hidden, and any row that still
    // reaches a reader is a board card without content.
    let reader = TaskReader::build(h.path(), &admin, true).unwrap();
    assert!(reader.hides(PERSONAL) && !reader.hides(SHARED));
    assert!(!reader.can_read("personal-task", PERSONAL) && !reader.can_read_row(&personal));
    let card = reader.task_json(&personal);
    assert_eq!(card["restricted"], json!(true));
    assert!(!card.to_string().contains("secret"), "{card}");
    let event = reader.activity_json(&activity(PERSONAL, None), None);
    assert_eq!(event["restricted"], json!(true));
    assert!(!event.to_string().contains("secret"), "{event}");
    // A listing scoped to it passed the dispatch gate (audited step-up).
    let scoped = TaskReader::build(h.path(), &admin, true).unwrap().admit_scope(Some(PERSONAL));
    assert!(!scoped.hides(PERSONAL));
    let other_scope = TaskReader::build(h.path(), &admin, true).unwrap().admit_scope(Some(SHARED));
    assert!(other_scope.hides(PERSONAL), "admitting one agent never admits another");

    // The owner: not hidden.
    let owner = TaskReader::build(h.path(), &alice, true).unwrap();
    assert!(!owner.hides(PERSONAL));

    // Unreadable ownership store: no reader at all (refused).
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert!(TaskReader::build(empty.path(), &UserContext::admin_fallback(), true).is_err());
}

#[test]
fn p1b_unprotected_task_reader_unchanged() {
    let h = home();
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let reader = TaskReader::build(h.path(), &admin, false).unwrap();
    assert!(!reader.hides(PERSONAL) && !reader.hides(SHARED));
    let event = reader.activity_json(&activity(PERSONAL, None), None);
    assert!(event.get("restricted").is_none(), "{event}");
    // The legacy admin context needs no users.db outside protected mode.
    let missing = crate::fs_safe::private_test_home().unwrap();
    assert!(TaskReader::build(missing.path(), &UserContext::admin_fallback(), false).is_ok());
}

#[tokio::test]
async fn p1b_approval_card_of_personal_employee_is_metadata_only() {
    let h = home();
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let id = broker.request(PERSONAL, "dynamic_review_kind", "secret summary of the owner's work",
        json!({"secret": "owner payload"}), 300).await.unwrap();
    let record = broker.get(&id).await.unwrap().unwrap();
    let card = personal_metadata_card(&record);
    // Label, status and timing only.
    assert_eq!(card["id"], json!(id.as_str()));
    assert_eq!(card["agent_id"], json!(PERSONAL));
    assert_eq!(card["status"], json!(record.status));
    assert_eq!(card["restricted"], json!(true));
    assert_eq!(card["may_decide"], json!(false));
    for field in ["digest", "binding", "answer", "simulation", "channel_link"] {
        assert!(card[field].is_null(), "{field}");
    }
    assert!(!card.to_string().contains("secret"), "{card}");
}
