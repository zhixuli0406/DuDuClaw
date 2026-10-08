//! P2-A C9 / ET5.3 (dashboard side): every responsibility, steering and stop
//! RPC checks the caller at its entry — a non-admin account reaches only the
//! employees it is bound to, at Viewer for reads and Operator for changes.

use super::*;

/// A dashboard account bound to `agent` at `level`, stored in `users.db`:
/// since the S9 rebase every responsibility / steering / stop RPC re-reads
/// the caller from the account store (`task_privacy::live_reader_context`),
/// so a context that exists only in memory is refused.
fn user_ctx(r: &Rig, agent: &str, level: AccessLevel, role: UserRole) -> UserContext {
    let db = UserDb::new(&r._dir.path().join("users.db")).unwrap();
    let email = format!("{}@test.invalid", uuid::Uuid::new_v4());
    let u = db
        .create_user(&email, &email, "isolated-test-password", role)
        .unwrap();
    db.bind_agent(&u.id, agent, level).unwrap();
    let mut agent_access = std::collections::HashMap::new();
    agent_access.insert(agent.to_string(), level);
    UserContext {
        user_id: u.id,
        email,
        role,
        agent_access,
        must_change_password: false,
    }
}

fn bound_ctx(r: &Rig, agent: &str, level: AccessLevel) -> UserContext {
    user_ctx(r, agent, level, UserRole::Employee)
}

fn is_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

struct Rig {
    _dir: tempfile::TempDir,
    handler: MethodHandler,
    store: Arc<TaskStore>,
    resp_id: String,
    epoch: i64,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[dispatch]\nenabled = true\n\n[responsibilities]\nenabled = true\n\n\
         [goal_loop]\nsteering_enabled = true\n",
    )
    .unwrap();
    let agent = dir.path().join("agents").join("alice");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"), "[agent]\nname = \"alice\"\n").unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    handler.set_task_store(Arc::clone(&store)).await;
    handler
        .set_message_queue(Arc::new(
            crate::message_queue::MessageQueue::open(dir.path()).unwrap(),
        ))
        .await;
    let now = Utc::now();
    let input = crate::responsibility::service::ResponsibilityInput {
        owner_agent_id: "alice".into(),
        objective: "每天整理信件".into(),
        acceptance_template: "摘要".into(),
        source_refs: vec![],
        notification_policy: None,
        schedule: Some(crate::responsibility::service::ScheduleSpec {
            cron: "0 0 9 * * *".into(),
            timezone: "Asia/Taipei".into(),
        }),
        event_subscriptions: vec![],
        occurrence_hours: 4,
        occurrence_cost_cap_cents: 100,
        budget_period: "day".into(),
        budget_timezone: "Asia/Taipei".into(),
        period_cost_limit_cents: 1000,
        period_occurrence_limit: 5,
        min_wake_interval_secs: 300,
        max_consecutive_failures: 3,
        stop_at: now + chrono::Duration::days(5),
        lane: None,
    };
    let r = crate::responsibility::service::create(&store, dir.path(), &input, "op", now)
        .await
        .unwrap();
    let mut t = TaskRow::new(
        "alice-goal".into(),
        "t".into(),
        "w".into(),
        "medium".into(),
        "alice".into(),
        "system".into(),
    );
    t.status = "in_progress".into();
    t.goal_mode = true;
    store.insert_task(&t).await.unwrap();
    Rig {
        _dir: dir,
        handler,
        store,
        resp_id: r.responsibility_id,
        epoch: r.control_epoch,
    }
}

impl Rig {
    async fn call(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        self.handler
            .handle_responsibilities_rpc(method, params, ctx)
            .await
    }
}

#[tokio::test]
async fn another_employees_responsibility_is_unreachable() {
    let r = rig().await;
    let bob = bound_ctx(&r, "bob", AccessLevel::Operator);
    for method in [
        "responsibilities.get",
        "responsibilities.occurrences",
        "responsibilities.fires",
    ] {
        let f = r
            .call(method, json!({"responsibility_id": r.resp_id}), &bob)
            .await;
        assert!(!is_ok(&f), "{method}: {f:?}");
    }
    let f = r
        .call(
            "responsibilities.pause",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &bob,
        )
        .await;
    assert!(!is_ok(&f));
    let f = r
        .call("responsibilities.list", json!({"agent_id": "alice"}), &bob)
        .await;
    assert!(!is_ok(&f));
    let f = r.call("responsibilities.list", json!({}), &bob).await;
    assert!(!is_ok(&f), "non-admin must name an employee");
    assert_eq!(
        r.store
            .get_responsibility(&r.resp_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "active"
    );
}

#[tokio::test]
async fn viewer_reads_but_cannot_change() {
    let r = rig().await;
    let viewer = bound_ctx(&r, "alice", AccessLevel::Viewer);
    let f = r
        .call(
            "responsibilities.get",
            json!({"responsibility_id": r.resp_id}),
            &viewer,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let f = r
        .call(
            "responsibilities.pause",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &viewer,
        )
        .await;
    assert!(!is_ok(&f));
    let f = r
        .call(
            "tasks.steer",
            json!({"task_id": "alice-goal", "body": "先做 A", "client_request_id": "c1"}),
            &viewer,
        )
        .await;
    assert!(!is_ok(&f));
    let f = r
        .call("tasks.steering", json!({"task_id": "alice-goal"}), &viewer)
        .await;
    assert!(is_ok(&f), "{f:?}");
}

#[tokio::test]
async fn operator_of_the_owner_can_pause_and_steer() {
    let r = rig().await;
    let op = bound_ctx(&r, "alice", AccessLevel::Operator);
    let f = r
        .call(
            "responsibilities.pause",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &op,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    assert_eq!(
        r.store
            .get_responsibility(&r.resp_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "paused"
    );
    let f = r
        .call(
            "tasks.steer",
            json!({"task_id": "alice-goal", "body": "先做 A", "client_request_id": "c1"}),
            &op,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let bob = bound_ctx(&r, "bob", AccessLevel::Operator);
    let f = r
        .call("tasks.steering", json!({"task_id": "alice-goal"}), &bob)
        .await;
    assert!(!is_ok(&f));
    let f = r
        .call(
            "tasks.stop",
            json!({"task_id": "alice-goal", "expected_authority_revision": 0}),
            &bob,
        )
        .await;
    assert!(!is_ok(&f));
    assert_eq!(
        r.store
            .get_task("alice-goal")
            .await
            .unwrap()
            .unwrap()
            .status,
        "in_progress"
    );
}

#[tokio::test]
async fn feature_off_allows_only_narrowing_changes() {
    let r = rig().await;
    std::fs::write(
        r._dir.path().join("config.toml"),
        "[dispatch]\nenabled = true\n",
    )
    .unwrap();
    let op = bound_ctx(&r, "alice", AccessLevel::Operator);
    let f = r
        .call(
            "responsibilities.resume",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &op,
        )
        .await;
    assert!(!is_ok(&f), "{f:?}");
    let f = r
        .call(
            "responsibilities.pause",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &op,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
}

/// S-H1: a stop by an account below manager level counts as unsuccessful.
#[tokio::test]
async fn operator_stop_is_recorded_as_counting_a_failure() {
    let r = rig().await;
    let op = bound_ctx(&r, "alice", AccessLevel::Operator);
    let rev = r
        .store
        .get_task("alice-goal")
        .await
        .unwrap()
        .unwrap()
        .authority_revision;
    let f = r
        .call(
            "tasks.stop",
            json!({"task_id": "alice-goal", "expected_authority_revision": rev}),
            &op,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let req = r
        .store
        .get_stop_request("alice-goal")
        .await
        .unwrap()
        .unwrap();
    assert!(req.counts_as_failure);
}

/// S-L4: a Viewer's `tasks.stop_status` reads the stored state only.
#[tokio::test]
async fn viewer_stop_status_is_read_only() {
    let r = rig().await;
    let viewer = bound_ctx(&r, "alice", AccessLevel::Viewer);
    let f = r
        .call(
            "tasks.stop_status",
            json!({"task_id": "alice-goal"}),
            &viewer,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let admin = UserContext::admin_fallback();
    let rev = r
        .store
        .get_task("alice-goal")
        .await
        .unwrap()
        .unwrap()
        .authority_revision;
    let f = r
        .call(
            "tasks.stop",
            json!({"task_id": "alice-goal", "expected_authority_revision": rev}),
            &admin,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let before = r
        .store
        .get_stop_request("alice-goal")
        .await
        .unwrap()
        .unwrap();
    let f = r
        .call(
            "tasks.stop_status",
            json!({"task_id": "alice-goal"}),
            &viewer,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
    let after = r
        .store
        .get_stop_request("alice-goal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.updated_at, after.updated_at,
        "viewer read wrote nothing"
    );
    assert!(
        !before.counts_as_failure,
        "an admin stop is a manager decision"
    );
}

/// S-§0 end to end: a terminal-filed change is decided only through the
/// real `approvals.decide` RPC by an Admin; a manager is refused, and the
/// gate then honours exactly the dashboard decision.
#[tokio::test]
async fn cli_change_is_decided_only_by_an_admin_through_approvals_decide() {
    use crate::responsibility::operator_gate::{self as gate, Gate, GateRequest, GatedAction};
    let r = rig().await;
    let home = r._dir.path();
    let broker = crate::approval::ApprovalBroker::open(home).unwrap();
    let row = r
        .store
        .get_responsibility(&r.resp_id)
        .await
        .unwrap()
        .unwrap();
    let state = gate::state_fingerprint(Some(&row));
    let args = json!({"reason": "x"});
    let card = gate::card_for_row(GatedAction::Pause, &row, &[]);
    let req = GateRequest {
        action: GatedAction::Pause,
        target: &r.resp_id,
        owner: "alice",
        args: &args,
        state: &state,
        card: &card,
        valid_minutes: 30,
    };
    let Gate::Requested(id) = gate::gate(&broker, &req).await.unwrap() else {
        panic!("expected a new request");
    };
    let manager = user_ctx(&r, "alice", AccessLevel::Owner, UserRole::Manager);
    let f = r
        .handler
        .handle_approvals_decide(json!({"id": id.as_str(), "approve": true}), &manager)
        .await;
    assert!(!is_ok(&f), "{f:?}");
    assert_eq!(
        broker.get(&id).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Pending
    );
    // A current Admin account. (Once `users.db` holds accounts, S9's
    // `require_current_dashboard_role_in_home` no longer accepts the
    // `system` admin-token context for this kind.)
    let admin = user_ctx(&r, "alice", AccessLevel::Owner, UserRole::Admin);
    let f = r
        .handler
        .handle_approvals_decide(json!({"id": id.as_str(), "approve": true}), &admin)
        .await;
    assert!(is_ok(&f), "{f:?}");
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert!(
        rec.decided_by
            .as_deref()
            .unwrap_or("")
            .starts_with("dashboard:"),
        "{rec:?}"
    );
    assert!(matches!(
        gate::gate(&broker, &req).await.unwrap(),
        Gate::Proceed(_)
    ));
}

/// L-1: clearing the failure streak needs a manager (it undoes what a
/// non-manager stop counted).
#[tokio::test]
async fn clearing_failures_needs_a_manager() {
    let r = rig().await;
    let op = bound_ctx(&r, "alice", AccessLevel::Owner);
    let f = r
        .call(
            "responsibilities.clear_failures",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &op,
        )
        .await;
    assert!(!is_ok(&f), "{f:?}");
    let manager = user_ctx(&r, "alice", AccessLevel::Operator, UserRole::Manager);
    let f = r
        .call(
            "responsibilities.clear_failures",
            json!({"responsibility_id": r.resp_id, "expected_control_epoch": r.epoch}),
            &manager,
        )
        .await;
    assert!(is_ok(&f), "{f:?}");
}

/// P5: the page's feature state is readable by any signed-in account and
/// reports the configuration, never rows.
#[tokio::test]
async fn status_reports_the_switches() {
    let r = rig().await;
    let viewer = bound_ctx(&r, "alice", AccessLevel::Viewer);
    let f = r.call("responsibilities.status", json!({}), &viewer).await;
    let WsFrame::Response { ok: true, payload: Some(p), .. } = &f else {
        panic!("{f:?}");
    };
    assert_eq!(p["enabled"], json!(true));
    assert_eq!(p["dispatch_enabled"], json!(true));
    assert_eq!(p["lanes"], json!(["explore"]));
}

/// P5: a read-only preset stores the explore lane in the contract scope; an
/// unknown lane is refused.
#[tokio::test]
async fn create_with_explore_lane_records_it() {
    let r = rig().await;
    let op = bound_ctx(&r, "alice", AccessLevel::Operator);
    let mut input = json!({
        "owner_agent_id": "alice",
        "objective": "每日簡報",
        "acceptance_template": "三點摘要",
        "schedule": {"cron": "0 0 8 * * *", "timezone": "Asia/Taipei"},
        "occurrence_hours": 1,
        "occurrence_cost_cap_cents": 50,
        "budget_period": "day",
        "budget_timezone": "Asia/Taipei",
        "period_cost_limit_cents": 50,
        "period_occurrence_limit": 1,
        "min_wake_interval_secs": 3600,
        "stop_at": (Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
        "lane": "explore",
    });
    let f = r.call("responsibilities.create", input.clone(), &op).await;
    let WsFrame::Response { ok: true, payload: Some(p), .. } = &f else {
        panic!("{f:?}");
    };
    let scope: Value =
        serde_json::from_str(p["responsibility"]["scope_json"].as_str().unwrap()).unwrap();
    assert_eq!(scope["lane"], json!("explore"));
    input["lane"] = json!("write");
    let f = r.call("responsibilities.create", input, &op).await;
    assert!(!is_ok(&f), "{f:?}");
}
