//! Pause/resume authorization through the RPC dispatcher and live users.db.
use super::agent_files_rpc_tests::{Fx, err, fixture, ok};
use super::*;

fn manager(f: &Fx) -> UserContext {
    let mut ctx = f.operator.clone();
    ctx.role = UserRole::Manager;
    rusqlite::Connection::open(f.home.join("users.db"))
        .unwrap()
        .execute(
            "UPDATE users SET role='manager' WHERE id=?1",
            [&ctx.user_id],
        )
        .unwrap();
    ctx
}

async fn denied_without_status_change(f: &Fx, who: &UserContext, params: Value) {
    for (method, initial) in [("agents.pause", "active"), ("agents.resume", "paused")] {
        f.h.update_agent_status_as("a1", initial, "test", &UserContext::admin_fallback())
            .await
            .unwrap();
        let path = f.home.join("agents/a1/agent.toml");
        let before = std::fs::read(&path).unwrap();
        let registry_before =
            f.h.registry()
                .read()
                .await
                .get("a1")
                .unwrap()
                .config
                .agent
                .status
                .clone();
        let response = f.call(method, params.clone(), who).await;
        assert!(!err(&response).is_empty(), "{method}: {response:?}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{method} changed agent.toml on denial"
        );
        assert_eq!(
            f.h.registry()
                .read()
                .await
                .get("a1")
                .unwrap()
                .config
                .agent
                .status,
            registry_before,
            "{method} changed registry status on denial",
        );
    }
}

async fn allowed(f: &Fx, who: &UserContext) {
    for (method, status) in [("agents.pause", "paused"), ("agents.resume", "active")] {
        let result = ok(&f.call(method, json!({ "agent_id": "a1" }), who).await);
        assert_eq!(result["status"], status);
        let text = std::fs::read_to_string(f.home.join("agents/a1/agent.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["agent"]["status"].as_str(), Some(status));
    }
}

#[tokio::test]
async fn manager_without_binding_is_denied() {
    let f = fixture().await;
    let mut who = manager(&f);
    duduclaw_auth::UserDb::new(&f.home.join("users.db"))
        .unwrap()
        .unbind_agent(&who.user_id, "a1")
        .unwrap();
    who.agent_access.clear();
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn manager_with_operator_or_owner_binding_is_allowed() {
    let f = fixture().await;
    let mut who = manager(&f);
    allowed(&f, &who).await;
    duduclaw_auth::UserDb::new(&f.home.join("users.db"))
        .unwrap()
        .bind_agent(&who.user_id, "a1", AccessLevel::Owner)
        .unwrap();
    who.agent_access.insert("a1".into(), AccessLevel::Owner);
    allowed(&f, &who).await;
}

#[tokio::test]
async fn manager_with_viewer_binding_is_denied() {
    let f = fixture().await;
    let mut who = manager(&f);
    duduclaw_auth::UserDb::new(&f.home.join("users.db"))
        .unwrap()
        .bind_agent(&who.user_id, "a1", AccessLevel::Viewer)
        .unwrap();
    who.agent_access.insert("a1".into(), AccessLevel::Viewer);
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn employee_with_operator_binding_still_needs_manager_role() {
    let f = fixture().await;
    denied_without_status_change(&f, &f.operator, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn admin_without_binding_is_allowed() {
    let f = fixture().await;
    allowed(&f, &f.admin).await;
    allowed(&f, &UserContext::admin_fallback()).await;
}

#[tokio::test]
async fn missing_invalid_or_unknown_agent_is_denied() {
    let f = fixture().await;
    for params in [
        json!({}),
        json!({ "id": "a1" }),
        json!({ "agent_id": null }),
        json!({ "agent_id": 42 }),
        json!({ "agent_id": "" }),
        json!({ "agent_id": "unknown" }),
        json!({ "agent_id": "../a1" }),
    ] {
        denied_without_status_change(&f, &f.admin, params).await;
    }
    assert!(!f.home.join("agents/unknown").exists());
}

#[tokio::test]
async fn live_binding_revocation_or_downgrade_overrides_session() {
    let f = fixture().await;
    let who = manager(&f);
    let db = duduclaw_auth::UserDb::new(&f.home.join("users.db")).unwrap();
    db.unbind_agent(&who.user_id, "a1").unwrap();
    assert!(who.has_agent_access("a1", AccessLevel::Operator));
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
    db.bind_agent(&who.user_id, "a1", AccessLevel::Viewer)
        .unwrap();
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn live_role_demotion_overrides_session() {
    let f = fixture().await;
    let who = manager(&f);
    rusqlite::Connection::open(f.home.join("users.db"))
        .unwrap()
        .execute(
            "UPDATE users SET role='employee' WHERE id=?1",
            [&who.user_id],
        )
        .unwrap();
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn unreadable_users_db_is_denied() {
    let f = fixture().await;
    let who = manager(&f);
    rusqlite::Connection::open(f.home.join("users.db"))
        .unwrap()
        .execute_batch("DROP TABLE user_agent_bindings")
        .unwrap();
    denied_without_status_change(&f, &who, json!({ "agent_id": "a1" })).await;
    denied_without_status_change(&f, &f.admin, json!({ "agent_id": "a1" })).await;
}

#[tokio::test]
async fn permission_revoked_while_waiting_for_file_lock_stops_write() {
    let f = fixture().await;
    let who = manager(&f);
    let db = duduclaw_auth::UserDb::new(&f.home.join("users.db")).unwrap();
    for revoke_binding in [true, false] {
        for (method, initial) in [("agents.pause", "active"), ("agents.resume", "paused")] {
            // Restore authority before dispatch, then revoke it while the writer waits.
            manager(&f);
            db.bind_agent(&who.user_id, "a1", AccessLevel::Operator)
                .unwrap();
            f.h.update_agent_status_as("a1", initial, "test", &UserContext::admin_fallback())
                .await
                .unwrap();
            let path = f.home.join("agents/a1/agent.toml");
            let before = std::fs::read(&path).unwrap();
            let registry_before =
                f.h.registry()
                    .read()
                    .await
                    .get("a1")
                    .unwrap()
                    .config
                    .agent
                    .status
                    .clone();
            let lock = crate::agent_files::file_mutex(
                &f.home,
                "a1",
                &crate::agent_files::FileKey::AgentToml,
            );
            let held = lock.lock().await;
            let mut pending = Box::pin(f.call(method, json!({ "agent_id": "a1" }), &who));
            tokio::select! {
                biased;
                response = &mut pending => panic!("write did not wait for the lock: {response:?}"),
                _ = tokio::task::yield_now() => {},
            }
            if revoke_binding {
                db.unbind_agent(&who.user_id, "a1").unwrap();
            } else {
                rusqlite::Connection::open(f.home.join("users.db"))
                    .unwrap()
                    .execute(
                        "UPDATE users SET role='employee' WHERE id=?1",
                        [&who.user_id],
                    )
                    .unwrap();
            }
            drop(held);
            assert!(err(&pending.await).contains("permission denied"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(
                f.h.registry()
                    .read()
                    .await
                    .get("a1")
                    .unwrap()
                    .config
                    .agent
                    .status,
                registry_before
            );
        }
    }
}
