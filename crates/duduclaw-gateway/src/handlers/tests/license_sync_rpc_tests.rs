//! RM-175 — `license.sync` (Admin only) and the seat fields of
//! `license.status` / `license.sync`.
use super::*;

fn ctx(role: UserRole) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn payload(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response { ok: true, payload, .. } => payload.clone().unwrap_or(Value::Null),
        other => panic!("expected success, got {other:?}"),
    }
}

fn error_text(f: &WsFrame) -> String {
    match f {
        WsFrame::Response { ok: false, error: Some(e), .. } => {
            e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())
        }
        other => panic!("expected refusal, got {other:?}"),
    }
}

fn add_agent(home: &std::path::Path, id: &str, status: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{id}\"\ndisplay_name = \"{id}\"\nstatus = \"{status}\"\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn license_sync_refuses_non_admins_before_any_work() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for role in [UserRole::Manager, UserRole::Employee] {
        let frame = handler.handle("license.sync", json!({}), &ctx(role)).await;
        let text = error_text(&frame);
        // Refused by a role gate (`require_admin!` answers "permission
        // denied"), never by reaching the sync handler itself.
        assert!(!text.contains("授權服務"), "{role:?}: {text}");
        assert!(!text.contains("Unknown method"), "{role:?}: {text}");
    }
    // Nothing was written for the refused callers.
    assert!(!home.path().join(crate::license_rev::REV_FILENAME).exists());
    assert!(!home.path().join("license.json").exists());
}

#[tokio::test]
async fn license_status_reports_seat_limit_and_use_to_managers() {
    let home = tempfile::tempdir().unwrap();
    add_agent(home.path(), "seat-a", "active");
    add_agent(home.path(), "seat-b", "archived");
    add_agent(home.path(), "seat-c", "deleted");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_agent_cap_fixture(Some(5)).await;
    let data = payload(&handler.handle("license.status", json!({}), &ctx(UserRole::Manager)).await);
    assert_eq!(data["seat_limit"], 5);
    assert_eq!(data["seat_unlimited"], false);
    // Archived employees hold a seat; deleted ones do not.
    assert_eq!(data["seats_used"], 2);
    assert!(data.get("tier").is_some(), "the snapshot fields are still there");
    // Managers never see the admin-only validation reason.
    assert!(data["validation_warning"].is_null());

    // Uncapped: no made-up number, an explicit "unlimited".
    handler.set_agent_cap_fixture(Some(0)).await;
    let data = payload(&handler.handle("license.status", json!({}), &ctx(UserRole::Manager)).await);
    assert!(data["seat_limit"].is_null());
    assert_eq!(data["seat_unlimited"], true);
    assert_eq!(data["seats_used"], 2);
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_seat_count_is_null_not_zero() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().unwrap();
    add_agent(home.path(), "seat-a", "active");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_agent_cap_fixture(Some(3)).await;
    let agents = home.path().join("agents");
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Running as root can still list the directory; nothing to prove then.
    let listable = std::fs::read_dir(&agents).is_ok();
    let data = payload(&handler.handle("license.status", json!({}), &UserContext::admin_fallback()).await);
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !listable {
        assert!(data["seats_used"].is_null(), "{data}");
    }
    assert_eq!(data["seat_limit"], 3);
}

#[tokio::test]
async fn license_sync_response_carries_outcome_seats_and_restart_flag() {
    use crate::license_runtime::SyncOutcome;
    let home = tempfile::tempdir().unwrap();
    add_agent(home.path(), "seat-a", "active");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_agent_cap_fixture(Some(4)).await;
    for (outcome, code) in [
        (SyncOutcome::Updated, "updated"),
        (SyncOutcome::Unchanged, "unchanged"),
        (SyncOutcome::Revoked, "revoked"),
        (SyncOutcome::PendingRebind, "pending_rebind"),
        (SyncOutcome::NoLicense, "no_license"),
        (SyncOutcome::Offline { reason: "network" }, "offline"),
        (SyncOutcome::Failed { reason: "storage" }, "failed"),
        (SyncOutcome::Busy { retry_after_secs: 7 }, "busy"),
    ] {
        let data = payload(&handler.license_sync_response(&outcome).await);
        assert_eq!(data["sync"]["outcome"], code);
        assert!(!data["sync"]["message"].as_str().unwrap().is_empty());
        assert_eq!(data["seat_limit"], 4);
        assert_eq!(data["seats_used"], 1);
        assert!(data["restart_recommended"].is_boolean());
        assert!(data.get("mode").is_some(), "same shape as license.status");
    }
}
