//! Delayed cron work retains its host authority across the runtime boundary.
use super::*;

struct PrincipalInvoker {
    seen: std::sync::Mutex<Vec<Option<String>>>,
    deny_main: bool,
}

#[async_trait::async_trait]
impl AgentInvoker for PrincipalInvoker {
    async fn invoke(
        &self, home: &Path, registry: &Arc<RwLock<AgentRegistry>>, agent: &str, _prompt: &str,
    ) -> Result<String, String> {
        let principal = crate::on_behalf_of::current();
        self.seen.lock().unwrap().push(principal.clone());
        // This is the carrier the runtime supplies to MCP's send_to_agent.
        let wire = crate::on_behalf_of::env_pairs().into_iter()
            .find(|(name, _)| name == duduclaw_core::ENV_ON_BEHALF_OF).map(|(_, value)| value);
        assert_eq!(wire, principal);
        if self.deny_main {
            let queue = crate::message_queue::MessageQueue::open(home).unwrap();
            let message = crate::message_queue::QueueMessage {
                id: uuid::Uuid::new_v4().to_string(), sender: agent.into(), target: "main".into(),
                payload: "delegated from cron".into(), status: crate::message_queue::MessageStatus::Pending,
                retry_count: 0, delegation_depth: 1, origin_agent: Some(agent.into()),
                sender_agent: Some(agent.into()), error: None, response: None,
                created_at: Utc::now().to_rfc3339(), acked_at: None, completed_at: None,
                reply_channel: None, turn_id: None, session_id: None, upstream_unknown: false,
                lane: None, on_behalf_of: wire,
            };
            queue.enqueue(&message).await.unwrap();
            let queued = queue.get_by_id(&message.id).await.unwrap().unwrap();
            assert!(matches!(crate::delegation_gate::gate_bus_dispatch(
                home, registry, queued.sender_agent.as_deref(), queued.origin_agent.as_deref(),
                &queued.target, &queued.id, "sqlite_queue", queued.on_behalf_of.as_deref(),
            ).await, crate::delegation_gate::GateOutcome::Deny(_)));
        }
        Ok("Cron invocation checked.".into())
    }
}

async fn fixture() -> (tempfile::TempDir, Arc<RwLock<AgentRegistry>>, String, String) {
    let home = tempfile::tempdir().unwrap();
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let employee = users.create_user("employee@cron.invalid", "Employee", "isolated-test-password",
        duduclaw_auth::UserRole::Employee).unwrap();
    let admin = users.create_user("admin@cron.invalid", "Admin", "isolated-test-password",
        duduclaw_auth::UserRole::Admin).unwrap();
    users.bind_agent(&employee.id, "worker", duduclaw_auth::AccessLevel::Owner).unwrap();
    for (name, role) in [("main", "main"), ("worker", "worker"), ("configured", "worker")] {
        crate::trigger_route::tests::write_agent(&home.path().join("agents"),
            name, name, "", role, "");
        let path = home.path().join("agents").join(name).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("reports_to = \"\"", "reports_to = \"main\"")).unwrap();
    }
    std::fs::write(home.path().join("config.toml"),
        "[general]\ndefault_agent = 'configured'\n[delegation]\nhuman_principal = 'record'\n").unwrap();
    let mut registry = AgentRegistry::new(home.path().join("agents"));
    registry.scan().await.unwrap();
    assert_eq!(registry.list().len(), 3, "principal fixtures must load");
    (home, Arc::new(RwLock::new(registry)), format!("user:{}", employee.id), format!("user:{}", admin.id))
}

#[tokio::test]
async fn carried_cron_restores_principal_and_gateway_refuses_nested_main_delegation() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = CronTaskRow::new("carried".into(), "Daily report".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    row.on_behalf_of = Some(employee.clone());
    store.insert(&row).await.unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: true };
    dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
    assert_eq!(*invoker.seen.lock().unwrap(), vec![Some(employee)]);
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().run_count, 1);
    assert_eq!(crate::on_behalf_of::current(), None);
}

#[tokio::test]
async fn cron_pauses_non_admin_main_default_and_newly_promoted_targets() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    for target in ["main", "default", "configured", "worker"] {
        let mut row = CronTaskRow::new(target.into(), target.into(), target.into(),
            "0 9 * * *".into(), "work".into());
        row.on_behalf_of = Some(employee.clone());
        store.insert(&row).await.unwrap();
        if target == "worker" {
            crate::trigger_route::tests::write_agent(&home.path().join("agents"),
                "worker", "Worker", "", "main", "");
            registry.write().await.scan().await.unwrap();
            assert_eq!(registry.read().await.get("worker").unwrap().config.agent.role,
                duduclaw_core::types::AgentRole::Main);
        }
        dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
        let stored = store.get(&row.id).await.unwrap().unwrap();
        assert!(!stored.enabled, "{target}");
        assert_eq!(stored.last_status.as_deref(), Some("skipped"));
        assert_eq!(stored.run_count, 0);
    }
    assert!(invoker.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn old_null_and_admin_crons_still_fire_for_main() {
    let (home, registry, _, admin) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    for (id, principal) in [("old", None), ("admin", Some(admin.clone()))] {
        let mut row = CronTaskRow::new(id.into(), id.into(), "main".into(),
            "0 9 * * *".into(), "work".into());
        row.on_behalf_of = principal;
        store.insert(&row).await.unwrap();
        dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
        assert_eq!(store.get(id).await.unwrap().unwrap().run_count, 1);
    }
    assert_eq!(*invoker.seen.lock().unwrap(), vec![None, Some(admin)]);
}

#[tokio::test]
async fn cached_cron_reload_picks_up_new_host_origin_before_firing() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let cached = CronTaskRow::new("cached".into(), "old".into(), "worker".into(),
        "0 9 * * *".into(), "old".into());
    store.insert(&cached).await.unwrap();
    store.update_fields_with_principal(&cached.id, "new", "worker", &cached.cron, "new", true,
        None, Some(&employee)).await.unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: true };
    dispatch_cron_task(home.path(), &store, &registry, &cached, &invoker).await;
    assert_eq!(*invoker.seen.lock().unwrap(), vec![Some(employee)]);
}

#[tokio::test]
async fn manual_legacy_cron_inherits_the_current_host_without_erasing_stored_origin() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let row = CronTaskRow::new("manual-old".into(), "legacy".into(), "worker".into(),
        "0 9 * * *".into(), "work".into());
    store.insert(&row).await.unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: true };
    crate::on_behalf_of::scope_first(Some(employee.clone()),
        dispatch_cron_task(home.path(), &store, &registry, &row, &invoker),
    ).await;
    assert_eq!(*invoker.seen.lock().unwrap(), vec![Some(employee)]);
    // A one-off invocation does not claim ownership of future scheduled fires.
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().on_behalf_of, None);
}

#[tokio::test]
async fn scoped_legacy_cron_stops_after_operator_binding_revoked() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = CronTaskRow::new("scoped-legacy".into(), "Scoped legacy".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    row.on_behalf_of = Some(employee.clone());
    let user = employee.strip_prefix("user:").unwrap();
    row.scoped_by_user_id = Some(user.into());
    assert!(row.created_by_user_id.is_none());
    store.insert(&row).await.unwrap();
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    users.bind_agent(user, "worker", duduclaw_auth::AccessLevel::Viewer).unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
    assert!(invoker.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scoped_manual_cron_refuses_changed_snapshot() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let row = CronTaskRow::new("scoped-manual".into(), "Scoped manual".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    store.insert(&row).await.unwrap();
    store.update_fields(&row.id, &row.name, "main", &row.cron, "other task", true).await.unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    dispatch_cron_task_checked(home.path(), &store, &registry, &row, &invoker, None, Some(&employee)).await;
    assert!(invoker.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scoped_cron_pauses_when_sandbox_changes_without_registry_reload() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = CronTaskRow::new("scoped-sandbox".into(), "Scoped".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    row.on_behalf_of = Some(employee.clone());
    row.created_by_user_id = employee.strip_prefix("user:").map(str::to_string);
    row.scoped_by_user_id = row.created_by_user_id.clone();
    store.insert(&row).await.unwrap();
    let path = home.path().join("agents/worker/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("[container]", "[container]\nsandbox_enabled = true")).unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
    assert!(invoker.seen.lock().unwrap().is_empty());
    let stored = store.get(&row.id).await.unwrap().unwrap();
    assert!(!stored.enabled);
    assert_eq!(stored.run_count, 0);
    assert_eq!(stored.last_status.as_deref(), Some("skipped"));
    // A scoped manual run of a legacy job must not bypass the same gate.
    let legacy = CronTaskRow::new("legacy-sandbox".into(), "Legacy".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    store.insert(&legacy).await.unwrap();
    dispatch_cron_task_checked(home.path(), &store, &registry, &legacy, &invoker, None, Some(&employee)).await;
    assert!(invoker.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn human_origin_only_cron_refuses_sandbox_target_without_verified_admin() {
    let (home, registry, employee, admin) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let path = home.path().join("agents/worker/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("[container]", "[container]\nsandbox_enabled = true")).unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    for (index, principal) in [employee, "unmapped:line:0123456789abcdef".into(),
        "user:missing-account".into(), "malformed".into(), admin].into_iter().enumerate() {
        let mut row = CronTaskRow::new(format!("human-only-{index}"), "Human".into(),
            "worker".into(), "0 9 * * *".into(), "prepare report".into());
        store.insert(&row).await.unwrap();
        // Model existing MCP rows, including externally corrupted origins.
        rusqlite::Connection::open(home.path().join("cron_tasks.db")).unwrap()
            .execute("UPDATE cron_tasks SET on_behalf_of=?1 WHERE id=?2", [&principal, &row.id]).unwrap();
        row.on_behalf_of = Some(principal);
        assert!(row.created_by_user_id.is_none() && row.scoped_by_user_id.is_none());
        dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
        let stored = store.get(&row.id).await.unwrap().unwrap();
        if index < 4 {
            assert!(invoker.seen.lock().unwrap().is_empty());
            assert!(!stored.enabled);
            assert_eq!(stored.run_count, 0);
            assert_eq!(stored.last_status.as_deref(), Some("skipped"));
        } else {
            assert_eq!(invoker.seen.lock().unwrap().len(), 1);
            assert_eq!(stored.run_count, 1);
        }
    }
}

#[tokio::test]
async fn scoped_cron_host_target_runs_and_admin_sandbox_behavior_is_unchanged() {
    let (home, registry, employee, admin) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = CronTaskRow::new("host-scoped".into(), "Host".into(), "worker".into(),
        "0 9 * * *".into(), "prepare report".into());
    row.on_behalf_of = Some(employee.clone());
    row.scoped_by_user_id = employee.strip_prefix("user:").map(str::to_string);
    store.insert(&row).await.unwrap();
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
    assert_eq!(invoker.seen.lock().unwrap().len(), 1);
    let path = home.path().join("agents/worker/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace("[container]", "[container]\nsandbox_enabled = true")).unwrap();
    row.id = "admin-sandbox".into();
    row.scoped_by_user_id = None;
    row.on_behalf_of = Some(admin);
    store.insert(&row).await.unwrap();
    dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
    assert_eq!(invoker.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn scoped_cron_scheduler_skips_legacy_fast_rows_and_refreshes_before_dispatch() {
    let (home, registry, employee, _) = fixture().await;
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let invoker = PrincipalInvoker { seen: Default::default(), deny_main: false };
    let scheduler = CronScheduler::new(home.path().into(), store.clone(), registry.clone());
    for marker in ["principal", "creator", "actor"] {
        let mut row = CronTaskRow::new(marker.into(), marker.into(), "worker".into(),
            "0 9 * * *".into(), "report".into());
        match marker {
            "principal" => row.on_behalf_of = Some(employee.clone()),
            "creator" => row.created_by_user_id = employee.strip_prefix("user:").map(str::to_owned),
            _ => row.scoped_by_user_id = employee.strip_prefix("user:").map(str::to_owned),
        }
        store.insert(&row).await.unwrap();
        let conn = rusqlite::Connection::open(home.path().join("cron_tasks.db")).unwrap();
        conn.execute("UPDATE cron_tasks SET cron='* * * * *' WHERE id=?1", [&row.id]).unwrap();
        drop(conn);
        // Dispatch must refresh the cached, previously-valid snapshot.
        dispatch_cron_task(home.path(), &store, &registry, &row, &invoker).await;
        assert_eq!(store.get(&row.id).await.unwrap().unwrap().run_count, 0);
    }
    assert!(invoker.seen.lock().unwrap().is_empty());
    scheduler.reload().await;
    assert!(scheduler.tasks.read().await.is_empty());
    // An unmarked Admin-created row retains its existing cadence behavior.
    let admin = CronTaskRow::new("admin-fast".into(), "Admin".into(), "worker".into(),
        "* * * * *".into(), "report".into());
    store.insert(&admin).await.unwrap();
    dispatch_cron_task(home.path(), &store, &registry, &admin, &invoker).await;
    assert_eq!(invoker.seen.lock().unwrap().len(), 1);
    scheduler.reload().await;
    assert_eq!(scheduler.tasks.read().await.len(), 1);
}
