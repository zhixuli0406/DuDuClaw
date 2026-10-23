//! Scheduled assignments over the dashboard RPC: `tasks.goal_create` /
//! `tasks.create` with `schedule`, and `assign_schedules.*`.
use super::team_callable_rpc_tests::{fixture, jwt_ctx, ok_payload};
use super::*;

fn err_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.to_string(),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn weekly(agent: &str) -> Value {
    json!({
        "agent_id": agent,
        "description": "每週一整理上週報價",
        "schedule": { "kind": "weekly", "days": [1], "time": "09:00" },
        "schedule_timezone": "Asia/Taipei",
    })
}

#[tokio::test]
async fn a_team_manager_schedules_a_goal_and_a_peer_cannot() {
    let f = fixture().await;
    let mgr = jwt_ctx(&f.mgr, UserRole::Manager);
    let created = ok_payload(
        &f.handler
            .handle("tasks.goal_create", weekly("sales"), &mgr)
            .await,
    );
    let s = &created["schedule"];
    assert_eq!(s["agent_id"], "sales");
    assert_eq!(s["status"], "active");
    assert_eq!(s["creator_user_id"], f.mgr.as_str());
    assert_eq!(
        s["schedule"],
        json!({"kind": "weekly", "days": [1], "hour": 9, "minute": 0})
    );
    assert!(created.get("task").is_none(), "nothing is assigned now");

    // emp2 (Viewer on hr) and `other`'s finance are not mgr's to assign.
    for agent in ["hr", "finance"] {
        let refused = f
            .handler
            .handle("tasks.goal_create", weekly(agent), &mgr)
            .await;
        assert!(err_text(&refused).contains("permission denied"), "{agent}");
    }
    let lead = jwt_ctx(&f.lead, UserRole::Manager);
    let other = jwt_ctx(&f.other, UserRole::Employee);
    assert!(
        err_text(
            &f.handler
                .handle("tasks.goal_create", weekly("sales"), &other)
                .await
        )
        .contains("permission denied")
    );
    // A board task can be scheduled too (lead reaches sales through emp1).
    let task = ok_payload(
        &f.handler
            .handle(
                "tasks.create",
                json!({ "assigned_to": "sales", "title": "匯出客戶名單", "schedule": "每天 18:00" }),
                &lead,
            )
            .await,
    );
    assert_eq!(task["schedule"]["kind"], "task");
    assert_eq!(task["schedule"]["schedule"]["kind"], "daily");
}

#[tokio::test]
async fn refusals_carry_codes_and_admin_only_shapes_stay_admin_only() {
    let f = fixture().await;
    let mgr = jwt_ctx(&f.mgr, UserRole::Manager);
    let mut cron = weekly("sales");
    cron["schedule"] = json!({ "kind": "cron", "expr": "0 9 * * 1" });
    assert_eq!(
        err_text(
            &f.handler
                .handle("tasks.goal_create", cron.clone(), &mgr)
                .await
        ),
        "\"cron_admin_only\""
    );
    let admin = jwt_ctx(&f.admin, UserRole::Admin);
    ok_payload(&f.handler.handle("tasks.goal_create", cron, &admin).await);

    let mut past = weekly("sales");
    past["schedule"] = json!({ "kind": "once", "at": "2020-01-01T09:00" });
    assert!(
        err_text(&f.handler.handle("tasks.goal_create", past, &mgr).await).contains("time_passed")
    );
    let mut bad = weekly("sales");
    bad["schedule"] = json!("下禮拜找時間");
    assert!(
        err_text(&f.handler.handle("tasks.goal_create", bad, &mgr).await)
            .contains("invalid_schedule")
    );
    let mut outcome = weekly("sales");
    outcome["outcome"] = json!("not a spec");
    assert!(
        err_text(&f.handler.handle("tasks.goal_create", outcome, &mgr).await).contains("outcome")
    );
    // The gateway admin token names nobody that could be re-checked later.
    let system = UserContext::admin_fallback();
    assert!(
        err_text(
            &f.handler
                .handle("tasks.goal_create", weekly("sales"), &system)
                .await
        )
        .contains("account_required")
    );
}

#[tokio::test]
async fn schedules_are_listed_and_controlled_by_their_creator_or_an_admin() {
    let f = fixture().await;
    let mgr = jwt_ctx(&f.mgr, UserRole::Manager);
    let id = ok_payload(
        &f.handler
            .handle("tasks.goal_create", weekly("sales"), &mgr)
            .await,
    )["schedule"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mine = ok_payload(
        &f.handler
            .handle("assign_schedules.list", json!({}), &mgr)
            .await,
    );
    assert_eq!(mine["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(mine["max_active"], 20);
    assert_eq!(mine["min_interval_minutes"], 60);

    let lead = jwt_ctx(&f.lead, UserRole::Manager);
    let theirs = ok_payload(
        &f.handler
            .handle("assign_schedules.list", json!({ "scope": "all" }), &lead)
            .await,
    );
    assert!(
        theirs["schedules"].as_array().unwrap().is_empty(),
        "`all` is for Admins"
    );
    assert_eq!(
        err_text(
            &f.handler
                .handle("assign_schedules.pause", json!({ "id": id }), &lead)
                .await
        ),
        "\"not_found\""
    );

    let paused = ok_payload(
        &f.handler
            .handle("assign_schedules.pause", json!({ "id": id }), &mgr)
            .await,
    );
    assert_eq!(paused["schedule"]["status"], "paused");
    let admin = jwt_ctx(&f.admin, UserRole::Admin);
    let all = ok_payload(
        &f.handler
            .handle("assign_schedules.list", json!({ "scope": "all" }), &admin)
            .await,
    );
    assert_eq!(all["schedules"][0]["creator_name"], "mgr");
    // mgr loses the team grant: resume is refused even for an Admin.
    f.db.set_manager(&f.lead, None).unwrap();
    assert_eq!(
        err_text(
            &f.handler
                .handle("assign_schedules.resume", json!({ "id": id }), &admin)
                .await
        ),
        "\"creator_lost_access\""
    );
    ok_payload(
        &f.handler
            .handle("assign_schedules.remove", json!({ "id": id }), &mgr)
            .await,
    );
    let after = ok_payload(
        &f.handler
            .handle("assign_schedules.list", json!({}), &mgr)
            .await,
    );
    assert!(after["schedules"].as_array().unwrap().is_empty());
}
