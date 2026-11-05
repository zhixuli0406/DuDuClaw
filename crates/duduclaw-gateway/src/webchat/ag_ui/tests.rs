use super::*;
use axum::{body::{Body, to_bytes}, http::Request};
use duduclaw_auth::{AccessLevel, UserRole};
use tower::ServiceExt;

struct Fixture { home: tempfile::TempDir, chat: Arc<WebChatState>, token: String, user: String }
impl Fixture {
    async fn new(role: UserRole) -> Self {
        let home = tempfile::tempdir().unwrap();
        crate::trigger_route::tests::write_agent(&home.path().join("agents"), "worker", "Worker", "@worker", "specialist", "");
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        let ctx = crate::agent_channels::e2e_tests::reply_ctx(home.path()).await;
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = db.create_user("agui@example.test", "User", "AGUI-test-password-123", role).unwrap();
        let jwt = Arc::new(JwtConfig::new(b"ag-ui-fixture-key-long-enough-for-jwt"));
        let token = jwt.issue_access_token(&user, &[]).unwrap();
        crate::mfa::reload_policy(home.path());
        let chat = Arc::new(WebChatState::new(ctx, jwt, db));
        Self { home, chat, token, user: user.id }
    }
    fn input(&self) -> RunInput {
        serde_json::from_value(json!({"threadId":"thread", "runId":"run", "messages":[{"id":"u", "role":"user", "content":"hello"}]})).unwrap()
    }
    fn request(&self, token: Option<&str>, input: Value) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/api/ag-ui/agents/worker/runs").header("content-type", "application/json");
        if let Some(token) = token { builder = builder.header("authorization", format!("Bearer {token}")); }
        builder.body(Body::from(input.to_string())).unwrap()
    }
    fn state(&self) -> Arc<AgUiState> {
        Arc::new(AgUiState { chat: self.chat.clone(), journal: Some(store::Store::open(self.home.path()).unwrap()),
            admission: tokio::sync::Mutex::new(()), connections: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()), receipts: Mutex::new(HashMap::new()) })
    }
    fn connection(&self, input: RunInput) -> (Connection, mpsc::Receiver<Frame>) {
        let (output, rx) = mpsc::channel(8);
        (Connection { input, claims: self.chat.jwt_config.verify_access_token(&self.token).unwrap(), output }, rx)
    }
}
async fn frame(rx: &mut mpsc::Receiver<Frame>) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
    let _ = frame.ack.send(());
    frame.value
}
async fn events(response: Response) -> Vec<Value> {
    let bytes = tokio::time::timeout(Duration::from_secs(30), to_bytes(response.into_body(), MAX_BODY * 4)).await.unwrap().unwrap();
    std::str::from_utf8(&bytes).unwrap().lines().filter_map(|line| line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")))
        .map(|data| serde_json::from_str(data).unwrap()).collect()
}

#[tokio::test]
async fn auth_refused_without_valid_bearer_or_for_suspended_user() {
    let f = Fixture::new(UserRole::Admin).await;
    let app = router(f.chat.clone());
    for token in [None, Some("invalid"), Some("widget:fake")] {
        assert_eq!(app.clone().oneshot(f.request(token, json!({}))).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }
    f.chat.user_db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert_eq!(app.oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unbound_employee_refused_before_stream() {
    let f = Fixture::new(UserRole::Employee).await;
    let response = router(f.chat.clone()).oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn normal_run_event_order_uses_guarded_pipeline() {
    let f = Fixture::new(UserRole::Employee).await;
    f.chat.user_db.bind_agent(&f.user, "worker", AccessLevel::Viewer).unwrap();
    let _seam = crate::agent_channels::e2e_seam::arm_waiting(f.home.path());
    // arm_waiting permits explicit control without ever invoking a provider.
    let mut seam = _seam;
    let app = router(f.chat.clone());
    let response = app.clone().oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let collect = tokio::spawn(events(response));
    let turn = tokio::time::timeout(Duration::from_secs(30), seam.entered.recv()).await.unwrap().unwrap();
    assert_eq!(turn.session_id, session_id(&f.user, "worker", "thread"));
    turn.release.send(()).unwrap();
    let output = collect.await.unwrap();
    assert_eq!(output.iter().map(|e| e["type"].as_str().unwrap()).collect::<Vec<_>>(),
        ["RUN_STARTED", "TEXT_MESSAGE_START", "TEXT_MESSAGE_CONTENT", "TEXT_MESSAGE_END", "RUN_FINISHED"]);
    assert_eq!(output[0]["protocolVersion"], "1.0");
    assert!(output[2]["delta"].as_str().unwrap().contains("OWN_BOT_REPLY"));
    assert_eq!(output[4]["outcome"]["type"], "success");
    let duplicate = app.oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn invalid_body_and_body_limit_refused_before_stream() {
    let f = Fixture::new(UserRole::Admin).await;
    let app = router(f.chat.clone());
    assert_eq!(app.clone().oneshot(f.request(Some(&f.token), json!({}))).await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(app.oneshot(f.request(Some(&f.token), json!({"padding":"x".repeat(MAX_BODY)}))).await.unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn conversation_namespace_is_per_user_agent_and_exact_thread() {
    let sid = session_id("u", "worker", "你好:thread");
    assert_ne!(sid, session_id("v", "worker", "你好:thread"));
    assert_ne!(sid, session_id("u", "other", "你好:thread"));
    assert_ne!(sid, session_id("u", "worker", "你好thread"));
    assert!(owns_webchat_session(&sid, &webchat_owner_tag("u")));
    assert!(is_session(&sid));
    assert!(!is_session("webchat:webchat:tag:ag-ui-evil:thread"));
}

#[tokio::test]
async fn approval_interrupt_and_authorized_resume_preserve_original_turn() {
    approval_resume_case(UserRole::Admin, "mcp_call", false, true, false).await;
}

#[tokio::test]
async fn dashboard_only_external_approval_resumes_original_pipeline() {
    approval_resume_case(UserRole::Admin, "memory_forget_source", true, true, false).await;
}

#[tokio::test]
async fn non_admin_can_confirm_dashboard_approval_and_resume() {
    approval_resume_case(UserRole::Employee, "mcp_call", true, true, false).await;
}

#[tokio::test]
async fn non_admin_can_confirm_dashboard_only_denial_and_resume() {
    approval_resume_case(UserRole::Employee, "memory_forget_source", true, false, false).await;
}

#[tokio::test]
async fn externally_decided_interrupt_survives_original_card_deadline() {
    approval_resume_case(UserRole::Employee, "memory_forget_source", true, true, true).await;
}

async fn approval_resume_case(role: UserRole, kind: &str, external: bool, approve: bool, after_expiry: bool) {
    let f = Fixture::new(role).await;
    f.chat.user_db.bind_agent(&f.user, "worker", AccessLevel::Viewer).unwrap();
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    let sid = session_id(&f.user, "worker", "thread");
    let dashboard = if external {
        let admin = f.chat.user_db.create_user("decider@example.test", "Decider", "AGUI-admin-password-123", UserRole::Admin).unwrap();
        let mut context = duduclaw_auth::UserContext::admin_fallback();
        context.user_id = admin.id.clone();
        context.email = admin.email;
        let handler = crate::handlers::MethodHandler::new(f.home.path().to_path_buf()).await;
        Some((admin.id, context, handler))
    } else { None };
    let approval = crate::approval::scope_turn(&sid, "host-turn", broker.request("worker", kind, "private args", json!({"secret":"not-in-stream"}), if after_expiry { 5 } else { 120 })).await.unwrap();
    let mut seam = crate::agent_channels::e2e_seam::arm_waiting(f.home.path());
    let (connection, mut rx) = f.connection(f.input());
    let (incoming_tx, mut incoming_rx) = mpsc::channel(1);
    let waiting = Arc::new(AtomicBool::new(false));
    let waiting_worker = waiting.clone();
    let user = f.user.clone();
    let sid_worker = sid.clone();
    let broker_worker = broker.clone();
    let worker = tokio::spawn(async move {
        run::execute(&state, &broker_worker, &user, "worker", &sid_worker, "host-turn", "hello", connection, &mut incoming_rx, &waiting_worker).await
    });
    assert_eq!(frame(&mut rx).await["type"], "RUN_STARTED");
    assert_eq!(frame(&mut rx).await["type"], "STATE_SNAPSHOT");
    assert_eq!(frame(&mut rx).await["type"], "MESSAGES_SNAPSHOT");
    let terminal = frame(&mut rx).await;
    assert_eq!(terminal["outcome"]["type"], "interrupt");
    assert_eq!(terminal["outcome"]["interrupts"][0]["id"], approval.as_str());
    assert!(!terminal.to_string().contains("private args"));
    assert!(!terminal.to_string().contains("not-in-stream"));
    if let Some((admin, context, handler)) = dashboard {
        let decision = handler.handle_approvals_decide(json!({"id":approval.as_str(), "approve":approve}), &context).await;
        assert!(matches!(decision, crate::protocol::WsFrame::Response { ok: true, .. }), "{decision:?}");
        assert_eq!(broker.get(&approval).await.unwrap().unwrap().decided_by,
            Some(format!("dashboard:{admin}")));
    }
    if after_expiry {
        tokio::time::sleep(Duration::from_millis(5100)).await;
        assert!(!worker.is_finished(), "a timely external decision must survive the old pending deadline");
    }
    let mut resume = f.input();
    resume.run_id = "resumed".into();
    resume.resume = serde_json::from_value(json!([{"interruptId":approval.as_str(),"status":"resolved","payload":{"approved":approve}}])).unwrap();
    let (next, mut resumed) = f.connection(resume);
    incoming_tx.send(next).await.unwrap();
    assert_eq!(frame(&mut resumed).await["type"], "RUN_STARTED");
    let turn = tokio::time::timeout(Duration::from_secs(30), seam.entered.recv()).await.unwrap().unwrap();
    turn.release.send(()).unwrap();
    assert_eq!(frame(&mut resumed).await["type"], "TEXT_MESSAGE_START");
    assert_eq!(frame(&mut resumed).await["type"], "TEXT_MESSAGE_CONTENT");
    assert_eq!(frame(&mut resumed).await["type"], "TEXT_MESSAGE_END");
    assert_eq!(frame(&mut resumed).await["outcome"]["type"], "success");
    assert!(worker.await.unwrap());
    assert_eq!(broker.get(&approval).await.unwrap().unwrap().status,
        if approve { crate::approval::ApprovalStatus::Approved } else { crate::approval::ApprovalStatus::Denied });
    if external {
        assert!(broker.get(&approval).await.unwrap().unwrap().decided_by.unwrap().strip_prefix("dashboard:").is_some());
    }
}

#[tokio::test]
async fn resume_authorization_denies_dashboard_only_other_kinds_and_non_admin() {
    let f = Fixture::new(UserRole::Admin).await;
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    for kind in ["memory_forget_source", "responsibility_operator_change", "unknown_kind"] {
        let id = crate::approval::scope_turn("resume-session", "resume-turn",
            broker.request("worker", kind, "test", json!({}), 120)).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        assert!(!run::may_decide(&record, true));
        let mut input = f.input();
        input.resume = serde_json::from_value(json!([{"interruptId":id.as_str(), "status":"cancelled"}])).unwrap();
        let (connection, _rx) = f.connection(input);
        assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn", &connection, &[record], &mut HashMap::new()).await, Err("approval_forbidden"));
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
    }
    let id = crate::approval::scope_turn("resume-session", "resume-turn",
        broker.request("worker", "mcp_call", "test", json!({}), 120)).await.unwrap();
    assert!(!run::may_decide(&broker.get(&id).await.unwrap().unwrap(), false));
}

#[tokio::test]
async fn resumed_decision_cannot_change_after_atomic_success() {
    let f = Fixture::new(UserRole::Admin).await;
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    let id = crate::approval::scope_turn("resume-session", "resume-turn",
        broker.request("worker", "mcp_call", "test", json!({}), 120)).await.unwrap();
    let record = broker.get(&id).await.unwrap().unwrap();
    let mut input = f.input();
    input.resume = serde_json::from_value(json!([{"interruptId":id.as_str(),"status":"resolved","payload":{"approved":true}}])).unwrap();
    let mut accepted = HashMap::new();
    let (connection, _rx) = f.connection(input.clone());
    run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn", &connection, &[record.clone()], &mut accepted).await.unwrap();
    input.resume[0].payload = Some(json!({"approved":false}));
    let (changed, _rx) = f.connection(input);
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn", &changed, &[record], &mut accepted).await, Err("resume_decision_changed"));
}

#[tokio::test]
async fn error_path_has_no_success_terminal() {
    let f = Fixture::new(UserRole::Admin).await;
    let (connection, mut rx) = f.connection(f.input());
    let task = tokio::spawn(run::reject(connection, "stale_or_unknown_interrupt"));
    assert_eq!(frame(&mut rx).await["type"], "RUN_STARTED");
    let failure = frame(&mut rx).await;
    assert_eq!(failure["type"], "RUN_ERROR");
    assert_eq!(failure["code"], "stale_or_unknown_interrupt");
    task.await.unwrap();
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn disconnected_stream_releases_run_slot_and_withdraws_approvals() {
    let f = Fixture::new(UserRole::Admin).await;
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    let sid = session_id(&f.user, "worker", "thread");
    let id = crate::approval::scope_turn(&sid, "host-turn", broker.request("worker", "mcp_call", "test", json!({}), 120)).await.unwrap();
    let (connection, rx) = f.connection(f.input());
    drop(rx);
    let (_tx, incoming) = mpsc::channel(1);
    let lease = RunLease { state: state.clone(), sid: sid.clone() };
    run::start(state, broker.clone(), lease, f.user.clone(), "worker".into(), sid, "host-turn".into(), format!("user:{}", f.user), "hello".into(), connection, incoming, Arc::new(AtomicBool::new(false))).await;
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Denied);
}

#[tokio::test]
async fn every_sse_connection_including_errors_counts_toward_limit() {
    let f = Fixture::new(UserRole::Admin).await;
    let app = router(f.chat.clone());
    let first = app.clone().oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let mut input = f.input();
    input.run_id = "second".into();
    let second = app.clone().oneshot(f.request(Some(&f.token), serde_json::to_value(input.clone()).unwrap())).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    input.run_id = "third".into();
    let third = app.oneshot(f.request(Some(&f.token), serde_json::to_value(input).unwrap())).await.unwrap();
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    drop(second);
}

#[tokio::test]
async fn self_approval_policy_blocks_approve_but_allows_deny() {
    let f = Fixture::new(UserRole::Admin).await;
    std::fs::write(f.home.path().join("config.toml"), "[approval]\nforbid_self_approval = true\n").unwrap();
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    let id = crate::on_behalf_of::scope_first(Some(format!("user:{}", f.user)),
        crate::approval::scope_turn("resume-session", "resume-turn",
            broker.request("worker", "mcp_call", "test", json!({}), 120))).await.unwrap();
    let record = broker.get(&id).await.unwrap().unwrap();
    let mut input = f.input();
    input.resume = serde_json::from_value(json!([{"interruptId":id.as_str(),"status":"resolved","payload":{"approved":true}}])).unwrap();
    let (connection, _rx) = f.connection(input.clone());
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn", &connection, &[record.clone()], &mut HashMap::new()).await, Err("self_approval_forbidden"));
    input.resume[0].payload = Some(json!({"approved":false}));
    let (connection, _rx) = f.connection(input);
    run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn", &connection, &[record], &mut HashMap::new()).await.unwrap();
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Denied);
}

#[path = "admission_tests.rs"]
mod admission;

#[tokio::test]
async fn external_confirmation_requires_matching_result_origin_and_live_access() {
    let f = Fixture::new(UserRole::Employee).await;
    f.chat.user_db.bind_agent(&f.user, "worker", AccessLevel::Viewer).unwrap();
    let state = f.state();
    let broker = state.chat.approvals.clone().unwrap();
    let id = crate::approval::scope_turn("resume-session", "resume-turn",
        broker.request("worker", "mcp_call", "test", json!({}), 120)).await.unwrap();
    let card = broker.get(&id).await.unwrap().unwrap();
    let mut input = f.input();
    input.resume = serde_json::from_value(json!([{"interruptId":id.as_str(), "status":"resolved", "payload":{"approved":true}}])).unwrap();
    let (connection, _rx) = f.connection(input.clone());
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn",
        &connection, &[card.clone()], &mut HashMap::new()).await, Err("approval_forbidden"));
    broker.decide(&id, false, "dashboard:admin").await.unwrap();
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn",
        &connection, &[card.clone()], &mut HashMap::new()).await, Err("resume_decision_changed"));
    input.resume[0].payload = Some(json!({"approved":false}));
    let (connection, _rx) = f.connection(input);
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "other-session", "resume-turn",
        &connection, &[card.clone()], &mut HashMap::new()).await, Err("stale_interrupt"));
    f.chat.user_db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert_eq!(run::decide_resume(&state, &broker, &f.user, "worker", "resume-session", "resume-turn",
        &connection, &[card], &mut HashMap::new()).await, Err("authorization_changed"));
    assert_eq!(broker.get(&id).await.unwrap().unwrap().decided_by.as_deref(), Some("dashboard:admin"));
}
