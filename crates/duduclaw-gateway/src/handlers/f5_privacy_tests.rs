//! F5-D: the remaining paths to a private task's content or decision, the
//! per-connection push table, and a removed source task under an activated
//! workflow.
use super::task_privacy_tests::{Fixture, OPEN, PRIVATE, admin, denied, fixture, ok, payload};
use super::*;

#[tokio::test]
async fn writes_to_a_private_task_follow_the_goal_decide_bar() {
    let f = fixture().await;
    let h = &f.handler;
    let store = TaskStore::open(&f.home).unwrap();
    store
        .update_task(PRIVATE, &json!({"status": "needs_human"}))
        .await
        .unwrap();
    let decide = json!({"task_id": PRIVATE, "status": "done"});
    assert!(denied(&h.handle_tasks_update(decide.clone(), &f.bob).await));
    let id = json!({"task_id": PRIVATE});
    assert!(denied(&h.handle_tasks_archive(id.clone(), &f.bob).await));
    assert!(denied(&h.handle_tasks_pin(id.clone(), &f.bob).await));
    assert!(denied(
        &h.handle_tasks_rename(json!({"task_id": PRIVATE, "title": "x"}), &f.bob)
            .await
    ));
    assert!(denied(
        &h.handle_tasks_assign(json!({"task_id": PRIVATE, "agent_id": "sales"}), &f.bob)
            .await
    ));
    assert!(denied(&h.handle_tasks_remove(id.clone(), &f.bob).await));
    assert_eq!(
        store.get_task(PRIVATE).await.unwrap().unwrap().status,
        "needs_human"
    );
    // The same bar for an open task is unchanged.
    assert!(ok(&h
        .handle_tasks_pin(json!({"task_id": OPEN}), &f.bob)
        .await));
    // In the audience: allowed; then unbound on the same connection: refused.
    assert!(ok(&h.handle_tasks_archive(id.clone(), &f.alice).await));
    f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
    assert!(denied(&h.handle_tasks_update(decide, &f.alice).await));
    assert!(denied(&h.handle_tasks_remove(id.clone(), &f.alice).await));
    // An admin outside the audience decides and removes.
    let root = admin(&f);
    assert!(ok(&h.handle_tasks_remove(id, &root).await));
}

#[tokio::test]
async fn a_tool_card_raised_in_a_private_round_follows_the_task() {
    let f = fixture().await;
    let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
    let card = duduclaw_core::with_host_task_id(
        json!({"name": "send_message", "arguments": {"text": "secret deliverable"}}),
        Some(PRIVATE),
    );
    assert_eq!(card["task_id"], PRIVATE);
    let id = broker
        .request("sales", "mcp_call", "send a message", card, 300)
        .await
        .unwrap();
    let listed = |v: &Value| v.to_string().contains(id.as_str());
    let bob = payload(f.handler.handle_approvals_list(json!({}), &f.bob).await);
    assert!(!listed(&bob), "bob is outside the task's audience");
    let alice = payload(f.handler.handle_approvals_list(json!({}), &f.alice).await);
    assert!(listed(&alice));
    let decide = json!({"id": id.as_str(), "approve": true});
    assert!(denied(
        &f.handler.handle_approvals_decide(decide, &f.bob).await
    ));
    // A card from outside a goal round is untouched.
    assert_eq!(
        duduclaw_core::with_host_task_id(json!({"name": "x"}), None),
        json!({"name": "x"})
    );
}

#[tokio::test]
async fn search_drops_deliverables_of_tasks_the_reader_may_not_see() {
    let f = fixture().await;
    let line = |task: &str, name: &str| {
        json!({
            "produced_at": Utc::now().to_rfc3339(), "agent_id": "sales",
            "archived_name": format!("1_{name}"), "display_name": name,
            "size": 1, "origin": "declared", "task_id": task,
        })
        .to_string()
    };
    std::fs::create_dir_all(f.home.join("agents/sales/attachments")).unwrap();
    std::fs::write(
        f.home.join("artifacts.jsonl"),
        format!(
            "{}\n{}\n",
            line(PRIVATE, "secret-quote.md"),
            line(OPEN, "secret-open.md")
        ),
    )
    .unwrap();
    let q = json!({"q": "secret", "agent_id": "sales", "sources": ["artifacts"]});
    let bob = payload(f.handler.handle_search_query(q.clone(), &f.bob).await).to_string();
    assert!(!bob.contains("secret-quote.md"), "{bob}");
    assert!(bob.contains("secret-open.md"));
    let alice = payload(f.handler.handle_search_query(q, &f.alice).await).to_string();
    assert!(alice.contains("secret-quote.md"));
}

#[tokio::test]
async fn forward_recent_keeps_only_rows_of_readable_tasks() {
    let f = fixture().await;
    let db = f.home.join("prediction.db");
    let _model = crate::prediction::task_forward_store::TaskForwardModel::new(db.clone());
    let conn = rusqlite::Connection::open(&db).unwrap();
    for (id, task, agent) in [
        ("p1", PRIVATE, "sales"),
        ("p2", OPEN, "sales"),
        ("p3", "gone", "other"),
    ] {
        conn.execute(
            "INSERT INTO task_prediction_log (prediction_id, task_id, agent_id, round, state_key, \
             prediction_json, prediction_source, created_at) VALUES (?1, ?2, ?3, 1, 'k', '{}', 'prior', ?4)",
            rusqlite::params![id, task, agent, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
    let rows = |v: Value| -> Vec<String> {
        v["predictions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["task_id"].as_str().unwrap().to_string())
            .collect()
    };
    let bob = rows(payload(
        f.handler.handle_forward_recent(json!({}), &f.bob).await,
    ));
    assert_eq!(bob, vec![OPEN.to_string()]);
    let root = admin(&f);
    let all = rows(payload(
        f.handler.handle_forward_recent(json!({}), &root).await,
    ));
    assert_eq!(all.len(), 3, "admins keep removed and private tasks");
}

#[tokio::test]
async fn fork_views_need_a_manager_bound_to_the_employee() {
    let f = fixture().await;
    let store = duduclaw_fork::ForkStore::open(&f.home.join("fork_store.db")).unwrap();
    for (id, agent) in [("f-sales", "sales"), ("f-other", "other")] {
        store
            .insert_fork(
                &duduclaw_fork::ForkRow {
                    fork_id: id.into(),
                    agent_id: agent.into(),
                    prompt: "branch work".into(),
                    merge_mode: "judge".into(),
                    resolved: false,
                    winner: None,
                    promoted: false,
                    aggregate_spent_usd: 0.0,
                    created_at: Utc::now().to_rfc3339(),
                },
                &[],
            )
            .unwrap();
    }
    let listed = payload(f.handler.handle_fork_list(json!({}), &f.bob)).to_string();
    assert!(listed.contains("f-sales") && !listed.contains("f-other"));
    assert!(ok(&f
        .handler
        .handle_fork_inspect(json!({"fork_id": "f-sales"}), &f.bob)));
    assert!(denied(
        &f.handler
            .handle_fork_inspect(json!({"fork_id": "f-other"}), &f.bob)
    ));
    f.db.unbind_agent(&f.bob.user_id, "sales").unwrap();
    assert!(denied(
        &f.handler
            .handle_fork_inspect(json!({"fork_id": "f-sales"}), &f.bob)
    ));
    let employee =
        f.db.create_user(
            "e@test.invalid",
            "E",
            "isolated-test-password",
            UserRole::Employee,
        )
        .unwrap();
    f.db.bind_agent(&employee.id, "sales", AccessLevel::Operator)
        .unwrap();
    let mut e = UserContext::admin_fallback();
    e.user_id = employee.id;
    e.role = UserRole::Employee;
    assert!(denied(&f.handler.handle_fork_list(json!({}), &e)));
}

fn event(name: &str, payload: Value) -> String {
    serde_json::to_string(&WsFrame::event(name, payload)).unwrap()
}

#[tokio::test]
async fn push_table_by_event_kind() {
    let f: Fixture = fixture().await;
    let root = admin(&f);
    let bob = &f.bob;
    let check = |ctx: &UserContext, pre_auth: bool, line: &str| {
        PushGate::new(&f.home, ctx, pre_auth).filter(line).is_some()
    };
    let other_reply = event(
        "activity.new",
        json!({"agent_id": "other", "summary": "回覆 X 對話「secret」"}),
    );
    let own_reply = event(
        "activity.new",
        json!({"agent_id": "sales", "summary": "ok"}),
    );
    let session = event(
        "chat.sessions.updated",
        json!({"agent_id": "other", "session_id": "telegram:1"}),
    );
    let failed = event(
        "channels.send_failed",
        json!({"channel": "discord", "error": "x"}),
    );
    let login = event(
        "auth.cli_login.output",
        json!({"session_id": "s", "data": "code"}),
    );
    let status = event("system.status_changed", json!({}));
    let unknown = event("brand.new.event", json!({}));
    let raw_queue = json!({"type": "channel_queue_rejected", "channel": "telegram"}).to_string();
    assert!(!check(bob, false, &other_reply), "another employee's reply");
    assert!(check(bob, false, &own_reply));
    assert!(!check(bob, false, &session));
    assert!(!check(bob, false, &failed));
    assert!(!check(bob, false, &login));
    assert!(!check(bob, false, &unknown), "unknown kinds are admin-only");
    assert!(check(bob, false, &status));
    assert!(check(bob, false, &raw_queue));
    for line in [&other_reply, &session, &failed, &login, &unknown] {
        assert!(check(&root, false, line), "admin: {line}");
    }
    // Lock screen: system status only.
    let lock = UserContext {
        user_id: "lockscreen".into(),
        email: "lockscreen@local".into(),
        role: UserRole::Employee,
        agent_access: HashMap::new(),
        must_change_password: false,
    };
    assert!(check(&lock, true, &status));
    for line in [&own_reply, &raw_queue, &other_reply] {
        assert!(!check(&lock, true, line));
    }
    // Revocation reaches a connection's cached identity after the cache window.
    let mut gate = PushGate::new(&f.home, bob, false);
    assert!(gate.filter(&own_reply).is_some());
    f.db.unbind_agent(&bob.user_id, "sales").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(PUSH_CACHE_SECS + 1));
    assert!(gate.filter(&own_reply).is_none());
}

/// P-M7: once the source task of an activated workflow is removed, an admin
/// can still read and act on the draft; a non-admin cannot.
#[tokio::test]
async fn a_removed_source_task_leaves_admins_in_charge() {
    let f = fixture().await;
    let root = admin(&f);
    let audience = crate::review_evidence::audience::authorize_workflow_audience;
    assert!(audience(&f.home, &root, "no-such-task", &[]).await.is_ok());
    assert!(
        audience(&f.home, &f.alice, "no-such-task", &[])
            .await
            .is_err()
    );
    assert!(audience(&f.home, &f.alice, OPEN, &[]).await.is_ok());
}

/// F5-D2: identities a real dashboard connection carries pass the live check
/// with their actual role; only the lock-screen identity is refused.
#[tokio::test]
async fn production_identities_pass_the_live_check() {
    let home = tempfile::tempdir().unwrap();
    // (a) no users.db: the no-login connection is `system` + Admin.
    let fallback = UserContext::admin_fallback();
    let live = live_reader_context(home.path(), &fallback).unwrap();
    assert!(live.is_admin());
    // (c) users exist, gateway admin token: still `system` + Admin.
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let boot = db
        .create_user(
            "boot@test.invalid",
            "Boot",
            "isolated-test-password",
            UserRole::Admin,
        )
        .unwrap();
    assert!(
        live_reader_context(home.path(), &fallback)
            .unwrap()
            .is_admin()
    );
    // (b) bootstrap admin: a users.db row, read back with its role.
    let mut jwt = UserContext::admin_fallback();
    jwt.user_id = boot.id;
    assert!(live_reader_context(home.path(), &jwt).unwrap().is_admin());
    // (d) lock screen: not an account, refused.
    let lock = UserContext {
        user_id: "lockscreen".into(),
        email: "lockscreen@local".into(),
        role: UserRole::Employee,
        agent_access: HashMap::new(),
        must_change_password: false,
    };
    assert!(live_reader_context(home.path(), &lock).is_err());
}

/// P-L9: round transcripts and activity outlive their task; once it is
/// removed, admins keep reading them and nobody else does.
#[tokio::test]
async fn content_of_a_removed_task_stays_with_admins() {
    let f = fixture().await;
    let store = TaskStore::open(&f.home).unwrap();
    let root = admin(&f);
    let h = &f.handler;
    let level = AccessLevel::Viewer;
    assert!(
        h.authorize_task_content_read(&store, &root, "gone", level)
            .await
            .is_ok()
    );
    assert!(
        h.authorize_task_content_read(&store, &f.alice, "gone", level)
            .await
            .is_err()
    );
    assert!(
        h.authorize_task_content_read(&store, &f.bob, PRIVATE, level)
            .await
            .is_err()
    );
    assert!(
        TaskReader::new(&f.home, &root)
            .unwrap()
            .can_read_owned("gone", None)
    );
    assert!(
        !TaskReader::new(&f.home, &f.alice)
            .unwrap()
            .can_read_owned("gone", None)
    );
}

/// The gateway log tail is Manager+ on the live role; a lock screen and an
/// employee never get it, and a downgrade stops it.
#[tokio::test]
async fn log_tail_follows_the_live_role() {
    let f = fixture().await;
    assert!(PushGate::new(&f.home, &f.bob, false).allows_log_tail());
    assert!(!PushGate::new(&f.home, &f.bob, true).allows_log_tail());
    f.db.update_user(&f.bob.user_id, None, Some(UserRole::Employee), None)
        .unwrap();
    assert!(!PushGate::new(&f.home, &f.bob, false).allows_log_tail());
}
