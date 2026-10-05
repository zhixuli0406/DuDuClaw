//! F3 (V-H-1): every task-content RPC and push event refuses a reader
//! outside the task's packet audience, and stops answering a connection
//! whose binding or role was revoked after it authenticated.
use super::*;
use duduclaw_auth::UserStatus;

pub(super) const PRIVATE: &str = "private-task";
pub(super) const OPEN: &str = "open-task";

pub(super) struct Fixture {
    pub(super) _home: tempfile::TempDir,
    pub(super) home: PathBuf,
    pub(super) handler: MethodHandler,
    pub(super) db: UserDb,
    pub(super) alice: UserContext,
    pub(super) bob: UserContext,
}

fn cached(user: &duduclaw_auth::User, level: AccessLevel) -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = user.id.clone();
    ctx.email = user.email.clone();
    ctx.role = user.role;
    ctx.agent_access.insert("sales".into(), level);
    ctx
}

pub(super) async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::create_dir_all(home.join("agents/sales")).unwrap();
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let mk = |email: &str| {
        let u = db
            .create_user(email, email, "isolated-test-password", UserRole::Manager)
            .unwrap();
        db.bind_agent(&u.id, "sales", AccessLevel::Operator)
            .unwrap();
        u
    };
    let alice = mk("alice@test.invalid");
    let bob = mk("bob@test.invalid");
    let handler = MethodHandler::new(home.clone()).await;
    let store = Arc::new(TaskStore::open(&home).unwrap());
    handler.set_task_store(store.clone()).await;
    for id in [PRIVATE, OPEN] {
        let mut row = TaskRow::new(
            id.into(),
            "Quarterly report".into(),
            "secret description".into(),
            "normal".into(),
            "sales".into(),
            "system".into(),
        );
        row.result_summary = Some("secret result".into());
        store.insert_task(&row).await.unwrap();
        store
            .append_activity(&ActivityRow {
                id: format!("act-{id}"),
                event_type: "task_completed".into(),
                agent_id: "sales".into(),
                task_id: Some(id.into()),
                summary: format!("secret activity {id}"),
                timestamp: Utc::now().to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();
    }
    let packets = home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(PRIVATE)
        .join("1");
    std::fs::create_dir_all(&packets).unwrap();
    std::fs::write(
        packets.join("p.json"),
        json!({
            "packet_id": "p", "goal_id": PRIVATE, "round": 1,
            "from_role": "executor", "to_role": "verifier",
            "objective": "Review", "output_format": "files",
            "audience": [format!("user:{}", alice.id)]
        })
        .to_string(),
    )
    .unwrap();
    Fixture {
        _home: dir,
        home,
        handler,
        alice: cached(&alice, AccessLevel::Operator),
        bob: cached(&bob, AccessLevel::Operator),
        db,
    }
}

pub(super) fn denied(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: false, payload: None, error: Some(e), .. }
        if e == PERMISSION_DENIED)
}
pub(super) fn ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}
pub(super) fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p,
        other => panic!("unexpected frame {other:?}"),
    }
}

/// Call one single-task RPC by name.
async fn call(f: &Fixture, method: &str, task: &str, ctx: &UserContext) -> WsFrame {
    let p = json!({ "task_id": task, "body": "a note", "action": "retry" });
    let h = &f.handler;
    match method {
        "tasks.changes" => h.handle_tasks_changes(p, ctx).await,
        "tasks.iterations" => h.handle_tasks_iterations(p, ctx).await,
        "tasks.timeline" => h.handle_tasks_timeline(p, ctx).await,
        "tasks.comments" => h.handle_tasks_comments(p, ctx).await,
        "tasks.comment" => h.handle_tasks_comment(p, ctx).await,
        "tasks.role_turns" => h.handle_tasks_role_turns(p, ctx).await,
        "tasks.artifacts" => h.handle_tasks_artifacts(p, ctx).await,
        "tasks.goal_decide" => h.handle_tasks_goal_decide(p, ctx).await,
        "activity.list" => h.handle_activity_list(p, ctx).await,
        other => panic!("unknown method {other}"),
    }
}

const SINGLE_TASK_RPCS: &[&str] = &[
    "tasks.changes",
    "tasks.iterations",
    "tasks.timeline",
    "tasks.comments",
    "tasks.comment",
    "tasks.role_turns",
    "tasks.artifacts",
    "activity.list",
];

#[tokio::test]
async fn single_task_rpcs_refuse_reader_outside_audience() {
    let f = fixture().await;
    for method in SINGLE_TASK_RPCS {
        assert!(
            denied(&call(&f, method, PRIVATE, &f.bob).await),
            "{method}: bob is not in the audience"
        );
        assert!(
            ok(&call(&f, method, PRIVATE, &f.alice).await),
            "{method}: alice"
        );
        assert!(
            ok(&call(&f, method, OPEN, &f.bob).await),
            "{method}: open task"
        );
    }
    // A decision on a private needs_human card is content too.
    assert!(denied(
        &call(&f, "tasks.goal_decide", PRIVATE, &f.bob).await
    ));
}

#[tokio::test]
async fn single_task_rpcs_stop_on_the_same_connection_after_revocation() {
    let f = fixture().await;
    let alice_id = f.alice.user_id.clone();
    // Binding removed: the cached context still claims Operator on sales.
    f.db.unbind_agent(&alice_id, "sales").unwrap();
    for method in SINGLE_TASK_RPCS {
        assert!(
            denied(&call(&f, method, PRIVATE, &f.alice).await),
            "{method} unbound"
        );
        assert!(
            denied(&call(&f, method, OPEN, &f.alice).await),
            "{method} unbound open"
        );
    }
    // Rebound but suspended.
    f.db.bind_agent(&alice_id, "sales", AccessLevel::Viewer)
        .unwrap();
    f.db.set_user_status(&alice_id, UserStatus::Suspended)
        .unwrap();
    for method in SINGLE_TASK_RPCS {
        assert!(
            denied(&call(&f, method, PRIVATE, &f.alice).await),
            "{method} suspended"
        );
    }
    // Active again but a Viewer binding cannot decide (Operator needed).
    f.db.set_user_status(&alice_id, UserStatus::Active).unwrap();
    assert!(ok(&call(&f, "tasks.changes", PRIVATE, &f.alice).await));
    assert!(denied(
        &call(&f, "tasks.goal_decide", PRIVATE, &f.alice).await
    ));
}

#[tokio::test]
async fn lists_show_only_the_board_card_outside_the_audience() {
    let f = fixture().await;
    let list = |ctx: UserContext| {
        let h = &f.handler;
        async move {
            let p = json!({ "agent_id": "sales" });
            (
                payload(h.handle_tasks_list(p.clone(), &ctx).await),
                payload(h.handle_tasks_list_page(p.clone(), &ctx).await),
                payload(h.handle_activity_list(p, &ctx).await),
            )
        }
    };
    let (tasks, page, activity) = list(f.bob.clone()).await;
    for rows in [&tasks["tasks"], &page["tasks"]] {
        let private = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == PRIVATE)
            .expect("the card stays listed");
        assert_eq!(private["restricted"], true);
        assert_eq!(private["title"], "Quarterly report");
        assert_eq!(private["description"], "");
        assert!(private["result_summary"].is_null());
        assert_eq!(private["tags"], json!([]));
        let open = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == OPEN)
            .unwrap();
        assert_eq!(open["result_summary"], "secret result");
    }
    assert_eq!(page["total"], 2, "counts stay consistent");
    let row = activity["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["task_id"] == PRIVATE)
        .unwrap();
    assert_eq!(row["summary"], "");
    assert_eq!(row["restricted"], true);
    let (tasks, _, activity) = list(f.alice.clone()).await;
    assert!(tasks.to_string().contains("secret description"));
    assert!(!activity.to_string().contains("\"restricted\""));
    // Live revocation on the listing itself.
    f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
    let h = &f.handler;
    let p = json!({ "agent_id": "sales" });
    assert!(denied(&h.handle_tasks_list(p.clone(), &f.alice).await));
    assert!(denied(&h.handle_tasks_list_page(p.clone(), &f.alice).await));
    assert!(denied(&h.handle_activity_list(p, &f.alice).await));
}

#[tokio::test]
async fn push_events_follow_the_same_gate_per_connection() {
    let f = fixture().await;
    let store = TaskStore::open(&f.home).unwrap();
    let row = store.get_task(PRIVATE).await.unwrap().unwrap();
    let frame = |event: &str, payload: Value| {
        serde_json::to_string(&WsFrame::event(event, payload)).unwrap()
    };
    let updated = frame("task.updated", task_row_to_json(&row));
    let comment = frame(
        "task.comment",
        json!({"task_id": PRIVATE, "body": "secret note"}),
    );
    let activity = frame(
        "activity.new",
        json!({"task_id": PRIVATE, "agent_id": "sales", "summary": "secret activity"}),
    );
    let unrelated = frame("system.status_changed", json!({"x": 1}));

    let bob_update = filter_push_event(&f.home, &f.bob, &updated).expect("card stays");
    assert!(bob_update.contains("\"restricted\":true"));
    assert!(!bob_update.contains("secret"));
    assert!(filter_push_event(&f.home, &f.bob, &comment).is_none());
    let bob_activity = filter_push_event(&f.home, &f.bob, &activity).unwrap();
    assert!(!bob_activity.contains("secret"));
    assert_eq!(
        filter_push_event(&f.home, &f.bob, &unrelated).unwrap(),
        unrelated
    );

    assert_eq!(
        filter_push_event(&f.home, &f.alice, &updated).unwrap(),
        updated
    );
    assert_eq!(
        filter_push_event(&f.home, &f.alice, &comment).unwrap(),
        comment
    );
    // Same connection after the binding is removed: nothing about the task.
    f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
    for line in [&updated, &comment, &activity] {
        assert!(filter_push_event(&f.home, &f.alice, line).is_none());
    }
}

#[tokio::test]
async fn timeline_forward_and_approval_cards_follow_the_task_gate() {
    let f = fixture().await;
    let h = &f.handler;
    let window = json!({
        "agent_id": "sales",
        "from": (Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
        "to": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    });
    let bob = payload(h.handle_timeline_list(window.clone(), &f.bob).await).to_string();
    assert!(
        !bob.contains("secret activity private-task"),
        "activity text of the private task"
    );
    assert!(bob.contains("secret activity open-task"));
    let alice = payload(h.handle_timeline_list(window.clone(), &f.alice).await).to_string();
    assert!(alice.contains("secret activity private-task"));

    let chain = json!({ "task_id": PRIVATE });
    assert!(denied(&h.handle_forward_chain(chain.clone(), &f.bob).await));
    assert!(ok(&h.handle_forward_chain(chain, &f.alice).await));

    assert_eq!(
        approval_task_ref(&json!({"fields": {"task": {"id": PRIVATE}}})).as_deref(),
        Some(PRIVATE)
    );
    assert_eq!(
        approval_task_ref(&json!({"task_id": OPEN})).as_deref(),
        Some(OPEN)
    );
    assert!(approval_task_ref(&json!({"fields": {"text": "x"}})).is_none());
    let owner = task_owner_readonly(&f.home, PRIVATE).unwrap();
    let live_bob = live_reader_context(&f.home, &f.bob).unwrap();
    assert!(!task_content_visible(&f.home, &live_bob, PRIVATE, &owner));
    assert!(task_content_visible(&f.home, &live_bob, OPEN, &owner));

    f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
    assert!(denied(&h.handle_timeline_list(window, &f.alice).await));
}

// ── F3 follow-up: AI-written audiences cannot lock out admins or people ──

pub(super) fn set_packet(f: &Fixture, raw: &str) {
    let dir = f
        .home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(PRIVATE)
        .join("1");
    std::fs::write(dir.join("p.json"), raw).unwrap();
}
pub(super) fn packet_with(audience: &[&str]) -> String {
    json!({
        "packet_id": "p", "goal_id": PRIVATE, "round": 1,
        "from_role": "executor", "to_role": "verifier",
        "objective": "Review", "output_format": "files",
        "audience": audience
    })
    .to_string()
}
pub(super) fn admin(f: &Fixture) -> UserContext {
    let u =
        f.db.create_user(
            "root@test.invalid",
            "Root",
            "isolated-test-password",
            UserRole::Admin,
        )
        .unwrap();
    let mut c = UserContext::admin_fallback();
    c.user_id = u.id;
    c
}

#[tokio::test]
async fn role_names_in_a_packet_restrict_nobody() {
    let f = fixture().await;
    set_packet(&f, &packet_with(&["verifier"]));
    let root = admin(&f);
    for method in SINGLE_TASK_RPCS {
        assert!(
            ok(&call(&f, method, PRIVATE, &f.bob).await),
            "{method}: bound non-admin"
        );
        assert!(
            ok(&call(&f, method, PRIVATE, &root).await),
            "{method}: admin"
        );
    }
    let list = payload(
        f.handler
            .handle_tasks_list(json!({"agent_id": "sales"}), &f.bob)
            .await,
    );
    assert!(!list.to_string().contains("\"restricted\""));
    assert!(list.to_string().contains("secret description"));
}

#[tokio::test]
async fn admins_read_through_any_packet_audience() {
    let f = fixture().await;
    let root = admin(&f);
    let token = UserContext::admin_fallback();
    for raw in [
        packet_with(&["user:alice"]),
        packet_with(&["channel:telegram"]),
        packet_with(&["user:alice", "verifier"]),
    ] {
        set_packet(&f, &raw);
        for method in SINGLE_TASK_RPCS {
            assert!(
                ok(&call(&f, method, PRIVATE, &root).await),
                "{method}: admin {raw}"
            );
            assert!(
                ok(&call(&f, method, PRIVATE, &token).await),
                "{method}: admin token"
            );
        }
    }
    // A channel-only audience has no `channel:dashboard`: dashboard
    // non-admins are refused, including alice.
    set_packet(&f, &packet_with(&["channel:telegram"]));
    assert!(denied(&call(&f, "tasks.changes", PRIVATE, &f.alice).await));
    assert!(denied(&call(&f, "tasks.changes", PRIVATE, &f.bob).await));
}

#[tokio::test]
async fn corrupt_packets_lock_out_non_admins_only_and_are_marked() {
    let f = fixture().await;
    set_packet(&f, "{");
    let root = admin(&f);
    assert!(denied(&call(&f, "tasks.changes", PRIVATE, &f.alice).await));
    let view = payload(call(&f, "tasks.timeline", PRIVATE, &root).await);
    assert_eq!(view["audience_restriction"]["state"], "unreadable");
    let store = TaskStore::open(&f.home).unwrap();
    let marks = store
        .list_activity_for_task(PRIVATE, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.event_type == crate::review_evidence::audience::AUDIENCE_UNREADABLE_EVENT)
        .count();
    assert_eq!(marks, 1, "marked once");
    let _ = call(&f, "tasks.changes", PRIVATE, &root).await;
    let again = store
        .list_activity_for_task(PRIVATE, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.event_type == crate::review_evidence::audience::AUDIENCE_UNREADABLE_EVENT)
        .count();
    assert_eq!(again, 1);
}

#[tokio::test]
async fn a_packet_that_limits_people_is_recorded_with_its_author() {
    use crate::review_evidence::audience::{
        AUDIENCE_RESTRICTED_EVENT, TaskAudience, record_audience_restriction, task_audience,
    };
    let f = fixture().await;
    set_packet(&f, &packet_with(&["verifier"]));
    let before = task_audience(&f.home, PRIVATE);
    assert_eq!(before, TaskAudience::Open);
    assert!(!record_audience_restriction(&f.home, PRIVATE, &before, 1, "executor", "eph-1").await);
    set_packet(&f, &packet_with(&["user:alice", "verifier"]));
    assert!(record_audience_restriction(&f.home, PRIVATE, &before, 1, "executor", "eph-1").await);
    let after = task_audience(&f.home, PRIVATE);
    assert!(!record_audience_restriction(&f.home, PRIVATE, &after, 2, "executor", "eph-1").await);
    let store = TaskStore::open(&f.home).unwrap();
    let row = store
        .list_activity_for_task(PRIVATE, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.event_type == AUDIENCE_RESTRICTED_EVENT)
        .expect("activity row");
    let meta: Value = serde_json::from_str(row.metadata.as_deref().unwrap()).unwrap();
    assert_eq!(meta["keys"], json!(["user:alice"]));
    assert_eq!(meta["author"], "eph-1");
    assert_eq!(meta["from_role"], "executor");
    let audit = std::fs::read_to_string(f.home.join("audit.jsonl")).unwrap_or_default();
    let audit_dir = walk_text(&f.home);
    assert!(
        audit.contains(AUDIENCE_RESTRICTED_EVENT) || audit_dir.contains(AUDIENCE_RESTRICTED_EVENT),
        "security audit event"
    );
    let root = admin(&f);
    let view = payload(call(&f, "tasks.timeline", PRIVATE, &root).await);
    assert_eq!(view["audience_restriction"]["state"], "limited");
    assert_eq!(
        view["audience_restriction"]["sources"][0]["from_role"],
        "executor"
    );
}

fn walk_text(dir: &Path) -> String {
    let mut out = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push_str(&walk_text(&p));
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    out
}

/// The case that used to dead-lock: a private task waiting for a person.
/// An admin decides it on the dashboard; a channel press is refused.
#[tokio::test]
async fn admin_decides_a_private_needs_human_task_on_the_dashboard() {
    let f = fixture().await;
    set_packet(&f, &packet_with(&["user:somebody-else"]));
    let store = TaskStore::open(&f.home).unwrap();
    store
        .update_task(PRIVATE, &json!({"status": "needs_human"}))
        .await
        .unwrap();
    let root = admin(&f);
    assert!(denied(
        &call(&f, "tasks.goal_decide", PRIVATE, &f.alice).await
    ));
    let decided = f
        .handler
        .handle_tasks_goal_decide(json!({"task_id": PRIVATE, "action": "done"}), &root)
        .await;
    assert!(ok(&decided), "{decided:?}");
    assert_ne!(
        store.get_task(PRIVATE).await.unwrap().unwrap().status,
        "needs_human"
    );
}
