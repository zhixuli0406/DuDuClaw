//! RM-175 — `license.sync` (Admin only) and the seat fields of
//! `license.status` / `license.sync`.
use super::*;

async fn handler_for(home: &std::path::Path) -> MethodHandler {
    let mut handler = MethodHandler::new(home.to_path_buf()).await;
    handler.license_runtime = Some(crate::license_runtime::LicenseRuntime::bootstrap(
        home.to_path_buf(), duduclaw_license::PublicKeyRegistry::new(),
    ).await);
    handler
}

fn ctx(role: UserRole) -> UserContext {
    UserContext {
        origin: duduclaw_auth::acl::ContextOrigin::Other,
        member_key_id: None,
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
    let handler = handler_for(home.path()).await;
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

/// The pre-auth (lock-screen, loopback) connection and the forced
/// password-change allowlist refuse both licence RPCs — even for a context
/// that claims Admin — before any handler runs.
#[tokio::test]
async fn license_rpcs_are_refused_on_pre_auth_and_password_change_entry_points() {
    use crate::power_local::RpcConnInfo;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
    let handler = handler_for(home.path()).await;
    let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 54321));
    for method in ["license.sync", "license.status"] {
        assert!(!crate::power_local::is_pre_auth_allowlisted(method), "{method}");
        assert!(!crate::handlers::gates::is_password_change_allowlisted(method), "{method}");
        let frame = handler
            .handle_conn(method, json!({}), &UserContext::admin_fallback(), RpcConnInfo::from_ws(loopback, true))
            .await;
        match &frame {
            WsFrame::Response { ok: false, error: Some(e), .. } => {
                assert_eq!(e.get("code").and_then(|c| c.as_str()), Some(LOGIN_REQUIRED_ERROR_CODE), "{method}");
            }
            other => panic!("{method} reached past the pre-auth gate: {other:?}"),
        }
    }
    assert!(!home.path().join(crate::license_rev::REV_FILENAME).exists());
}

/// RM-175: `license.sync` and the seat / validation fields of
/// `license.status` are dashboard RPCs only. No MCP tool (the AI-employee
/// surface) is defined for them, no MCP module or internal loopback route
/// names them, and the runtime entry point is called only from the RPC
/// handler and the change probe.
#[test]
fn license_rpcs_are_not_on_the_ai_employee_surface() {
    // Built at run time so this file never matches its own scan.
    let sync = ["license", ".sync"].concat();
    let status = ["license", ".status"].concat();
    let entry = ["sync", "_now("].concat();
    let handler_fn = ["handle_license", "_sync"].concat();

    // Registry level: every MCP tool definition.
    for tool in duduclaw_core::mcp_tool_defs::tools() {
        assert!(!tool.name.to_ascii_lowercase().contains("license"), "MCP tool {} looks licence-related", tool.name);
        let texts = std::iter::once(tool.description).chain(tool.params.iter().map(|p| p.description));
        for text in texts {
            assert!(!text.contains(&sync) && !text.contains(&status), "MCP tool {} names a licence RPC", tool.name);
        }
    }

    fn rust_sources(root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                if path.is_dir() {
                    if name != "tests" { stack.push(path); }
                } else if name.ends_with(".rs") && !name.contains("test") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    out.push((path, text));
                }
            }
        }
        out
    }

    // Source level, gateway: only the RPC arm, its handler, the gate notes,
    // the runtime and the change probe mention the method or its entry point.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let allowed = |path: &std::path::Path, needle: &str| -> bool {
        let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        let ok: &[&str] = if needle == sync.as_str() {
            &["handlers/dispatch_core.rs", "handlers/handshake_license.rs", "handlers/gates.rs", "license_runtime.rs", "license_rev.rs"]
        } else if needle == entry.as_str() {
            &["handlers/handshake_license.rs", "license_rev.rs", "license_runtime.rs"]
        } else {
            &["handlers/dispatch_core.rs", "handlers/handshake_license.rs"]
        };
        ok.contains(&rel.as_str())
    };
    for (path, text) in rust_sources(&src) {
        for needle in [&sync, &entry, &handler_fn] {
            if text.contains(needle.as_str()) {
                assert!(allowed(&path, needle), "{} reaches `{needle}`", path.display());
            }
        }
        // No internal (loopback) route is a licence route.
        assert!(!text.contains("/api/internal/license"), "{}", path.display());
    }

    // Source level, AI-employee side: the MCP server modules and the shared
    // tool definitions never name either RPC or the runtime entry point.
    let cli_src = manifest.join("../duduclaw-cli/src");
    let mut employee_side = rust_sources(&cli_src.join("mcp"));
    for entry in std::fs::read_dir(&cli_src).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_file() && name.starts_with("mcp") && name.ends_with(".rs") && !name.contains("test") {
            let text = std::fs::read_to_string(&path).unwrap();
            employee_side.push((path, text));
        }
    }
    employee_side.extend(rust_sources(&manifest.join("../duduclaw-core/src/mcp_tool_defs")));
    assert!(!employee_side.is_empty());
    for (path, text) in employee_side {
        for needle in [&sync, &status, &entry, &handler_fn] {
            assert!(!text.contains(needle.as_str()), "{} names `{needle}`", path.display());
        }
    }
}

#[tokio::test]
async fn license_status_reports_seat_limit_and_use_to_managers() {
    let home = tempfile::tempdir().unwrap();
    add_agent(home.path(), "seat-a", "active");
    add_agent(home.path(), "seat-b", "archived");
    add_agent(home.path(), "seat-c", "deleted");
    let handler = handler_for(home.path()).await;
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

#[tokio::test]
async fn license_sync_reports_present_unreadable_file_without_vendor_call() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("license.json"), "{invalid json").unwrap();
    let handler = handler_for(home.path()).await;
    let status = payload(&handler.handle("license.status", json!({}), &UserContext::admin_fallback()).await);
    assert_eq!(status["installed"], true);
    assert_eq!(status["validation_warning"], "unreadable");

    let sync = payload(&handler.handle("license.sync", json!({}), &UserContext::admin_fallback()).await);
    assert_eq!(sync["sync"]["outcome"], "unreadable_license_file");
    assert_eq!(sync["sync"]["reason"], "unreadable");
    assert!(!sync["sync"]["message"].as_str().unwrap().contains("invalid json"));
    assert_eq!(sync["installed"], true);
    assert_eq!(sync["validation_warning"], "unreadable");
    assert!(home.path().join("license.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn license_sync_reports_fifo_as_unreadable_without_opening_it() {
    let home = tempfile::tempdir().unwrap();
    nix::unistd::mkfifo(
        home.path().join("license.json").as_path(),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    ).unwrap();
    let handler = handler_for(home.path()).await;
    let sync = payload(&handler.handle("license.sync", json!({}), &UserContext::admin_fallback()).await);
    assert_eq!(sync["sync"]["outcome"], "unreadable_license_file");
    assert_eq!(sync["sync"]["reason"], "unreadable");
    assert_eq!(sync["installed"], true);
}

#[tokio::test]
async fn license_sync_reports_no_license_when_file_is_absent() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_for(home.path()).await;
    let sync = payload(&handler.handle("license.sync", json!({}), &UserContext::admin_fallback()).await);
    assert_eq!(sync["sync"]["outcome"], "no_license");
    assert_eq!(sync["installed"], false);
    assert!(sync["validation_warning"].is_null());
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_seat_count_is_null_not_zero() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().unwrap();
    add_agent(home.path(), "seat-a", "active");
    let handler = handler_for(home.path()).await;
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
    let handler = handler_for(home.path()).await;
    handler.set_agent_cap_fixture(Some(4)).await;
    for (outcome, code) in [
        (SyncOutcome::Updated, "updated"),
        (SyncOutcome::Unchanged, "unchanged"),
        (SyncOutcome::Revoked, "revoked"),
        (SyncOutcome::PendingRebind, "pending_rebind"),
        (SyncOutcome::NoLicense, "no_license"),
        (SyncOutcome::UnreadableLicenseFile { reason: "unreadable" }, "unreadable_license_file"),
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
