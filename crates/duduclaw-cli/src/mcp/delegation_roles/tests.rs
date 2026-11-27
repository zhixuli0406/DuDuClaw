use super::*;
use duduclaw_core::delegation_policy::*;

fn facts(requester: &str) -> LoopbackOwnerRoles {
    LoopbackOwnerRoles(Ok(RoleFacts { agent: "avatar".into(), owner_role: Some("admin".into()),
        requester_role: Some(requester.into()), requester_account_id: Some("account".into()), background: false }))
}
fn read() -> DelegationConfigRead {
    delegation_config_read_from_table(&"[delegation]\npolicy='none'\nunrestricted_roles=['admin']".parse().unwrap())
}
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    for agent in ["avatar", "other"] {
        crate::mcp::tests::create_test_agent(&home.path().join("agents"), agent, "");
    }
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", duduclaw_auth::UserRole::Admin).unwrap();
    rusqlite::Connection::open(home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET id='account', must_change_password=0 WHERE id=?1", [&admin.id]).unwrap();
    for agent in ["avatar", "other"] {
        let path = home.path().join("agents").join(agent).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("can_schedule_tasks = false", "can_schedule_tasks = true")).unwrap();
    }
    std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']").unwrap();
    home
}
#[test]
fn loopback_refusal_malformed_answer_and_wrong_employee_grant_no_pass() {
    let h = home();
    for bytes in [br#"{"ok":false}"#.as_slice(), br#"{"role":"admin"}"#, b"not json"] {
        let roles = LoopbackOwnerRoles(decode("avatar", bytes));
        assert!(roles.requester_role().is_err());
        assert!(!unrestricted(h.path(), "avatar", &read(), &roles));
    }
    let encoded = serde_json::to_vec(&serde_json::json!({"ok":true,"facts":facts("admin").facts().unwrap()})).unwrap();
    assert!(decode("other", &encoded).is_err());
    let roles = LoopbackOwnerRoles(decode("avatar", &encoded));
    assert!(unrestricted(h.path(), "avatar", &read(), &roles));
    assert!(!unrestricted(h.path(), "avatar", &read(), &facts("employee")));
    let mut background = facts("admin").facts().unwrap();
    background.background = true;
    assert!(!unrestricted(h.path(), "avatar", &read(), &LoopbackOwnerRoles(Ok(background))));
}
#[tokio::test]
async fn front_door_none_listing_and_call_share_turn_eligibility() {
    let h = home();
    let mut background = facts("admin").facts().unwrap();
    background.background = true;
    for (roles, allowed) in [(facts("admin"), true), (facts("employee"), false),
        (LoopbackOwnerRoles(Ok(background)), false),
        (LoopbackOwnerRoles(Err(OwnerRoleLookupError)), false)] {
        TEST_ROLES.scope((h.path().into(), "avatar".into(), roles), async {
            let policy = none_policy(h.path(), "avatar").await;
            assert_eq!(!policy.blocks("send_to_agent"), allowed);
            assert!(policy.blocks("create_agent"));
            assert!(policy.blocks("spawn_ephemeral"));
            assert_eq!(!none_blocks(h.path(), "avatar").await, allowed);
            assert_eq!(crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.is_ok(), allowed);
            assert_eq!(crate::mcp::check_delegation_allowed_blocking(h.path(), "avatar", "other", "send_to_agent").is_ok(), allowed);
            let mut principal = crate::mcp::tools_list::test_principal(false);
            principal.client_id = "avatar".into();
            let listed = crate::mcp::tools_list::visible_tools_with(
                &principal, h.path(), "avatar", true).await;
            // Same predicate/tools at list and dispatch; no reverse authority.
            for name in ["send_to_agent", "spawn_agent"] {
                assert_eq!(listed.iter().any(|tool| tool.name == name), allowed, "{name}");
                assert!(crate::mcp_dispatch::delegation_none_refuses_for(h.path(), name,
                    &serde_json::Value::Null, "avatar", || false));
            }
            // tasks_create remains listed for self-work even without a pass.
            assert!(listed.iter().any(|tool| tool.name == "tasks_create"));
            assert!(crate::mcp_dispatch::delegation_none_refuses_for(h.path(), "tasks_create",
                &serde_json::json!({"assigned_to":"other"}), "avatar", || false));
            for (tool, kind) in [("tasks_create", crate::mcp::RecordKind::Task),
                ("tasks_create_schedule", crate::mcp::RecordKind::Cron), ("create_reminder", crate::mcp::RecordKind::Reminder)] {
                assert_eq!(crate::mcp::check_record_change_allowed(h.path(), crate::mcp::RecordActor::Agent("avatar"),
                    "other", tool, kind).await.is_ok(), allowed);
            }
            assert!(crate::mcp::check_delegation_allowed(h.path(), "other", "avatar", "send_to_agent").await.is_err());
        }).await;
    }
}
#[tokio::test]
async fn front_door_one_way_pairs_preserve_permission_gate_responsibility() {
    let h = home();
    std::fs::write(h.path().join("config.toml"), "[delegation]\npolicy='hierarchy'\nallow_one_way=[['avatar','other']]").unwrap();
    assert!(crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.is_ok());
    assert!(crate::mcp::check_delegation_allowed(h.path(), "other", "avatar", "send_to_agent").await.is_err());
    let org = crate::mcp::org_snapshot_blocking(h.path(), &["avatar", "other"]);
    let read = read_delegation_config(h.path());
    let denied = evaluate_delegation(&read.rules, &org, "other", "avatar", &DelegationContext {
        overrides: &read.overrides, owner_roles: None, extra_system_senders: &[],
        boundaries: DelegationBoundaries { personal_target_allowed: true, main_target_allowed: true, can_send_cross_agent: true },
    }).unwrap_err();
    assert_eq!(denied.reason, DenyReason::ReversePairOnly);
    let path = h.path().join("agents/avatar/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("can_send_cross_agent = true", "can_send_cross_agent = false")).unwrap();
    assert!(crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.is_ok());
}

#[tokio::test]
async fn unrestricted_manager_cannot_target_main_and_send_permission_still_denies() {
    let h = home();
    rusqlite::Connection::open(h.path().join("users.db")).unwrap()
        .execute("UPDATE users SET role='manager' WHERE id='account'", []).unwrap();
    std::fs::write(h.path().join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['manager']").unwrap();
    let path = h.path().join("agents/other/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("role = \"specialist\"", "role = \"main\"")).unwrap();
    let roles = LoopbackOwnerRoles(Ok(RoleFacts { owner_role: Some("manager".into()),
        ..facts("manager").facts().unwrap() }));
    crate::mcp_isolation::ISOLATED_CALL.scope(isolated_call(h.path(), "account"),
        TEST_ROLES.scope((h.path().into(), "avatar".into(), roles.clone()), async {
        let error = crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.unwrap_err();
        assert!(error.contains("主代理"));
    })).await;
    std::fs::write(path, raw).unwrap();
    let sender = h.path().join("agents/avatar/agent.toml");
    let raw = std::fs::read_to_string(&sender).unwrap();
    std::fs::write(sender, raw.replace("can_send_cross_agent = true", "can_send_cross_agent = false")).unwrap();
    TEST_ROLES.scope((h.path().into(), "avatar".into(), roles), async {
        assert!(none_blocks(h.path(), "avatar").await);
        // The front-door delegation predicate does not add a permission gate.
        assert!(crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.is_ok());
    }).await;
}

#[cfg(unix)]
#[test]
fn unrestricted_sender_cannot_cross_personal_target_boundary() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    let mut conn = rusqlite::Connection::open(h.path().join("users.db")).unwrap();
    let tx = conn.transaction().unwrap();
    assert!(duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "other", "account").unwrap());
    assert!(duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, "other", "account").unwrap());
    tx.commit().unwrap();
    drop(conn);
    for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
        let file = h.path().join(name);
        if file.exists() { std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap(); }
    }
    let refused = crate::mcp::personal_target::personal_target_refusal(h.path(), true, "send_to_agent",
        &serde_json::json!({"agent_id":"other"}), "avatar");
    assert!(refused.is_some());
    let read = read();
    assert_eq!(evaluate_delegation(&read.rules, &duduclaw_core::MapOrgView::new(), "avatar", "other", &DelegationContext {
        overrides: &read.overrides, owner_roles: Some(&facts("admin")), extra_system_senders: &[],
        boundaries: DelegationBoundaries { personal_target_allowed: refused.is_none(),
            main_target_allowed: true, can_send_cross_agent: true },
    }).unwrap_err().reason, DenyReason::PersonalTargetNotPermitted);
}

fn isolated_call(home: &Path, user: &str) -> std::sync::Arc<crate::mcp_isolation::IsolatedCall> {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane, Principal},
        ids::{EmployeeId, RunId, Sha256Digest}};
    std::sync::Arc::new(crate::mcp_isolation::IsolatedCall { forks: None,
        home: home.into(), agent: "avatar".into(), roots: std::sync::Arc::new(duduclaw_mcp_boundary::resources::ResourceRoots::default()),
        parser: None, services: None, execution: None, worker: None, record_resolver: None,
        context: duduclaw_mcp_boundary::RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]), task: None,
            principal: Principal::Human { user_id: Text::new(user.into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Shared(Text::new("company".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX, nonce: [5; 32], jti: [6; 16],
        }),
    })
}

async fn dispatch(h: &Path, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
    use std::sync::Arc;
    let dispatcher = crate::mcp_dispatch::McpDispatcher::new(h.into(), reqwest::Client::new(),
        Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&h.join("memory.db")).unwrap()), "avatar".into(),
        Arc::new(crate::odoo_pool::OdooConnectorPool::default()), crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new());
    let mut principal = crate::mcp::tools_list::test_principal(false);
    principal.client_id = "avatar".into();
    dispatcher.dispatch_tool_call(&principal, &crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/avatar".into(), read_namespaces: vec!["internal/avatar".into()],
    }, &serde_json::json!({"name":tool,"arguments":arguments}), &serde_json::json!(1)).await
}

#[tokio::test]
async fn pass_holder_send_false_is_refused_by_mcp_permission_gate() {
    let h = home();
    // Hierarchy reaches the permission gate; none's eligibility probe may
    // independently narrow its exemption when this permission is false.
    std::fs::write(h.path().join("config.toml"), "[delegation]\npolicy='hierarchy'\nunrestricted_roles=['admin']").unwrap();
    let path = h.path().join("agents/avatar/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("can_send_cross_agent = true", "can_send_cross_agent = false")).unwrap();
    TEST_ROLES.scope((h.path().into(), "avatar".into(), facts("admin")), async {
        assert!(crate::mcp::check_delegation_allowed(h.path(), "avatar", "other", "send_to_agent").await.is_ok());
        let result = dispatch(h.path(), "send_to_agent", serde_json::json!({"agent_id":"other","message":"work"})).await;
        assert_eq!(result["error"]["code"], -32003, "{result}");
        assert!(result["error"]["message"].as_str().unwrap().contains("can_send_cross_agent"), "{result}");
        let audit = std::fs::read_to_string(h.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("permission_denied") && audit.contains("can_send_cross_agent"));
    }).await;
}

#[tokio::test]
async fn pass_holder_agent_update_keeps_none_listing_and_dispatch_behavior() {
    let h = home();
    TEST_ROLES.scope((h.path().into(), "avatar".into(), facts("admin")), async {
        let policy = none_policy(h.path(), "avatar").await;
        assert!(!policy.blocks("send_to_agent"));
        assert!(policy.blocks("agent_update"));
        let mut principal = crate::mcp::tools_list::test_principal(false);
        principal.client_id = "avatar".into();
        let listed = crate::mcp::tools_list::visible_tools_with(
            &principal, h.path(), "avatar", true).await;
        // v1.85 lists argument-dependent tools for self-work; no exemption
        // applies when dispatch supplies another employee's background edit.
        assert!(listed.iter().any(|tool| tool.name == "agent_update"));
        let args = serde_json::json!({"agent_id":"other","heartbeat_enabled":true});
        assert!(crate::mcp_dispatch::delegation_none_refuses_for(h.path(), "agent_update", &args, "avatar", || false));
        let result = dispatch(h.path(), "agent_update", args).await;
        assert_eq!(result["error"]["code"], -32003, "{result}");
        assert!(result["error"]["message"].as_str().unwrap().contains("不允許委派"), "{result}");
        let audit = std::fs::read_to_string(h.path().join("tool_calls.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = audit.lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        assert!(rows.iter().any(|row| row["tool_name"] == "agent_update"
            && row["agent_id"] == "avatar" && row["error_class"] == "delegation_disabled"
            && row["success"] == false), "{audit}");
    }).await;
}

#[test]
fn visibility_preserves_department_and_adds_only_forward_role_or_pair() {
    let h = home();
    crate::mcp::tests::create_test_agent(&h.path().join("agents"), "colleague", "");
    let path = h.path().join("agents/avatar/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("can_send_cross_agent = true", "can_send_cross_agent = false")).unwrap();
    let org = duduclaw_core::MapOrgView::new().with_agent("avatar", "", "sales")
        .with_agent("colleague", "", "sales").with_agent("other", "", "engineering");
    let roles = facts("admin");
    let absent = LoopbackOwnerRoles(Err(OwnerRoleLookupError));
    let read = delegation_config_read_from_table(&"[delegation]\npolicy='department'".parse().unwrap());
    assert!(crate::mcp::org_visible(h.path(), &read, &absent, &org, "avatar", "colleague"));
    assert!(crate::mcp::org_visible(h.path(), &read, &absent, &org, "avatar", "avatar"));
    assert!(!crate::mcp::org_visible(h.path(), &read, &absent, &org, "avatar", "other"));
    let read = delegation_config_read_from_table(&"[delegation]\npolicy='department'\nunrestricted_roles=['admin']".parse().unwrap());
    assert!(crate::mcp::org_visible(h.path(), &read, &roles, &org, "avatar", "other"));
    assert!(!crate::mcp::org_visible(h.path(), &read, &roles, &org, "other", "avatar"));
    let read = delegation_config_read_from_table(&"[delegation]\npolicy='department'\nallow_one_way=[['avatar','other']]".parse().unwrap());
    assert!(crate::mcp::org_visible(h.path(), &read, &absent, &org, "avatar", "other"));
    assert!(!crate::mcp::org_visible(h.path(), &read, &absent, &org, "other", "avatar"));
}
