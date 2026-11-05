use super::*;

fn journal_rows(home: &std::path::Path) -> i64 {
    rusqlite::Connection::open(home.join("ag_ui.db")).unwrap().query_row(
        "SELECT (SELECT COUNT(*) FROM ag_ui_runs)+(SELECT COUNT(*) FROM ag_ui_threads)", [], |r| r.get(0),
    ).unwrap()
}

#[tokio::test]
async fn orphan_resume_flood_cannot_allocate_journal_rows() {
    let f = Fixture::new(UserRole::Employee).await;
    f.chat.user_db.bind_agent(&f.user, "worker", AccessLevel::Operator).unwrap();
    let state = f.state();
    let app = Router::new().route("/api/ag-ui/agents/{agent_id}/runs", post(handle)).with_state(state);
    for i in 0..4096 {
        let response = app.clone().oneshot(f.request(Some(&f.token), json!({
            "threadId":format!("unknown-{i}"), "runId":format!("run-{i}"), "messages":[],
            "resume":[{"interruptId":"unknown", "status":"cancelled"}],
        }))).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    assert_eq!(journal_rows(f.home.path()), 0);
    let mut seam = crate::agent_channels::e2e_seam::arm_waiting(f.home.path());
    let response = app.oneshot(f.request(Some(&f.token), serde_json::to_value(f.input()).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let collect = tokio::spawn(events(response));
    let turn = tokio::time::timeout(Duration::from_secs(30), seam.entered.recv()).await.unwrap().unwrap();
    turn.release.send(()).unwrap();
    assert_eq!(collect.await.unwrap().last().unwrap()["outcome"]["type"], "success");
}

#[tokio::test]
async fn matching_receipts_remain_deduplicated_and_rate_limited() {
    let f = Fixture::new(UserRole::Employee).await;
    f.chat.user_db.bind_agent(&f.user, "worker", AccessLevel::Operator).unwrap();
    let state = f.state();
    let sid = session_id(&f.user, "worker", "thread");
    let mut input = f.input();
    input.messages.clear();
    input.resume = serde_json::from_value(json!([{"interruptId":"known", "status":"cancelled"}])).unwrap();
    state.receipts.lock().unwrap().insert(sid.clone(), (std::time::Instant::now(), input.resume.clone()));
    let app = Router::new().route("/api/ag-ui/agents/{agent_id}/runs", post(handle)).with_state(state);
    let response = app.clone().oneshot(f.request(Some(&f.token), serde_json::to_value(input.clone()).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(events(response).await.last().unwrap()["outcome"]["type"], "success");
    let response = app.clone().oneshot(f.request(Some(&f.token), serde_json::to_value(input.clone()).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let conn = rusqlite::Connection::open(f.home.path().join("ag_ui.db")).unwrap();
    let principal = duduclaw_core::HumanPrincipal::user(&f.user).unwrap().to_string();
    conn.execute(
        "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<32)
         INSERT INTO ag_ui_runs(session_id,run_id,started,principal) SELECT ?1,'extra-'||n,?2,?3 FROM ids",
        rusqlite::params![sid, chrono::Utc::now().timestamp(), principal],
    ).unwrap();
    let before = journal_rows(f.home.path());
    input.run_id = "rate-limited".into();
    let response = app.oneshot(f.request(Some(&f.token), serde_json::to_value(input).unwrap())).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(journal_rows(f.home.path()), before);
}
