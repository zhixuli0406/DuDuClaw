use super::*;
use duduclaw_isolation::execution::RunOutcome;
use duduclaw_isolation::ids::RunId;

struct Live(Mutex<(RunStatus, u64, bool)>);
impl LiveAuthority for Live {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        let state = self.0.lock().unwrap();
        if state.2 { return Err(VerificationError::Unavailable); }
        Ok(LiveSnapshot { employee, run, control: ControlConnectionId([3; 16]),
            status: state.0.clone(), principal: principal(), binding: binding(),
            policy_revision: state.1, agent_slug: "alice".into() })
    }
}
fn principal() -> Principal { Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 } }
fn binding() -> EmployeeBinding { EmployeeBinding::Personal(Text::new("owner".into()).unwrap()) }
fn reservation() -> Reservation {
    Reservation { run: RunHandle(RunId([1; 16])), employee: EmployeeId([2; 16]),
        control: ControlConnectionId([3; 16]), expires_at_unix_ms: 130_000 }
}
fn policy() -> GrantPolicy {
    GrantPolicy { task: None, principal: principal(), binding: binding(), lane: Lane::Standard,
        tools: List::new(vec![Text::new("file_read".into()).unwrap()]).unwrap(),
        resources: List::new(vec![]).unwrap(), policy_revision: 1, not_before: 100, expires_at: 200 }
}
fn setup() -> (Arc<Boundary>, Arc<Live>, CapabilityId) {
    let live = Arc::new(Live(Mutex::new((RunStatus::Reserved { expires_at_unix_ms: 130_000 }, 1, false))));
    let boundary = Boundary::new(live.clone()).unwrap();
    let id = boundary.issue(&reservation(), policy(), 100).unwrap();
    (boundary, live, id)
}
fn proof(boundary: &Boundary, id: CapabilityId) -> (CapabilityRecord, [u8; 32], ConnectionBinding) {
    let entries = boundary.entries.lock().unwrap();
    let entry = &entries[&id.0];
    (entry.record.clone(), entry.mac, ConnectionBinding { capability: id,
        employee: entry.record.employee, run: entry.record.run, gateway_epoch: entry.record.gateway_epoch })
}
#[test]
fn pending_records_cannot_authorize_and_activation_requires_live_run() {
    let (boundary, _, id) = setup();
    let (record, mac, connection) = proof(&boundary, id);
    assert!(boundary.verify(&record, CapabilityProof(&mac), &connection, 100).is_err());
    assert!(boundary.activate_inner(id, &reservation(), 100).is_err());
}
#[test]
fn forged_identity_or_mac_is_rejected() {
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
    let (mut record, mac, binding) = proof(&boundary, id);
    record.employee = EmployeeId([9; 16]);
    assert!(boundary.verify(&record, CapabilityProof(&mac), &binding, 101).is_err());
    let (record, mut mac, binding) = proof(&boundary, id);
    mac[0] ^= 1;
    assert!(boundary.verify(&record, CapabilityProof(&mac), &binding, 101).is_err());
    assert!(connection.context(101).is_ok());
}
#[test]
fn replay_and_cross_run_theft_are_rejected() {
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    let mut other = reservation();
    other.run = RunHandle(RunId([7; 16]));
    assert!(boundary.activate_inner(id, &other, 101).is_err());
    other = reservation();
    other.control = ControlConnectionId([7; 16]);
    assert!(boundary.activate_inner(id, &other, 101).is_err());
    let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
    assert!(boundary.activate_inner(id, &reservation(), 101).is_err());
    let (record, mac, mut binding) = proof(&boundary, id);
    binding.run = RunId([9; 16]);
    assert!(boundary.verify(&record, CapabilityProof(&mac), &binding, 101).is_err());
    drop(connection);
    assert!(boundary.activate_inner(id, &reservation(), 102).is_err());
}
#[test]
fn cancelled_expired_disconnected_policy_changed_or_unreadable_runs_are_revoked() {
    for mode in 0..6 {
        let (boundary, live, id) = setup();
        live.0.lock().unwrap().0 = RunStatus::Running;
        let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
        assert!(connection.context(101).is_ok());
        match mode {
            0 => boundary.revoke_run(reservation().run).unwrap(),
            1 => live.0.lock().unwrap().0 = RunStatus::Ended(RunOutcome::Cancelled),
            2 => live.0.lock().unwrap().1 += 1,
            3 => live.0.lock().unwrap().2 = true,
            4 => boundary.revoke_control(reservation().control).unwrap(),
            _ => { assert!(connection.context(200).is_err()); }
        }
        assert!(connection.context(102).is_err());
    }
}
#[test]
fn restart_invalidates_old_records_even_with_identical_live_state() {
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    let _connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
    let (record, mac, binding) = proof(&boundary, id);
    let restarted = Boundary::new(live).unwrap();
    assert!(restarted.verify(&record, CapabilityProof(&mac), &binding, 101).is_err());
}
#[test]
fn reservation_reuse_and_expiry_fail_closed() {
    let (boundary, _, _) = setup();
    assert!(boundary.issue(&reservation(), policy(), 101).is_err());
    assert!(boundary.issue(&reservation(), policy(), 130).is_err());
}
#[test]
fn personal_and_shared_credential_rules_and_argument_authority() {
    assert!(binding_matches(&binding(), &principal()));
    assert!(!binding_matches(&binding(), &Principal::Background {
        tenant_id: Text::new("owner".into()).unwrap(), generation: 1 }));
    let (boundary, _, id) = setup();
    let (mut record, _, _) = proof(&boundary, id);
    assert!(record.personal_owner_matches("owner"));
    record.employee_binding = EmployeeBinding::Shared(Text::new("tenant".into()).unwrap());
    assert!(!record.personal_owner_matches("owner"));
    record.resources = List::new(vec![ResourceGrant::Connector {
        account: Text::new("account".into()).unwrap(), binding: binding() }]).unwrap();
    assert!(crate::resources::validate_grants(&record).is_err());
    for key in ["agent_id", "agent", "principal", "root", "cwd", "run", "run_id", "employee_id"] {
        let mut arguments = serde_json::json!({});
        arguments[key] = "other".into();
        assert!(crate::check_arguments(&arguments).is_err());
    }
    assert_eq!(crate::tool_policy::policy("codrive_run").unwrap().route, crate::tool_policy::Route::Execution);
    // HX5: these three are in-run helpers, not child-run execution tools.
    for tool in ["code_map", "office_script", "execute_program"] {
        assert_eq!(crate::tool_policy::policy(tool).unwrap().route, crate::tool_policy::Route::InRunHelper);
    }
    assert_eq!(crate::tool_policy::policy("codrive_run").unwrap().route, crate::tool_policy::Route::Execution);
    assert!(crate::tool_policy::policy("unknown").is_none());
}

#[test]
fn pending_expiry_and_control_failure_are_swept_without_activation() {
    let (boundary, _, id) = setup();
    boundary.sweep(130).unwrap();
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().2 = true;
    boundary.sweep(101).unwrap();
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
}

#[cfg(target_os = "linux")]
mod transport {
    use super::*;
    use std::{future::Future, pin::Pin, time::Duration};
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    struct Echo;
    impl crate::server::Dispatcher for Echo {
        fn require_complete_services(&self) -> Result<(), VerificationError> { Ok(()) }
        fn dispatch<'a>(&'a self, _context: RequestContext, _slug: &'a str, request: Value)
            -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
            Box::pin(async move {
                if request["method"] == "wait" { std::future::pending::<()>().await; }
                Ok(request.get("id").map(|id| json!({"jsonrpc":"2.0", "id":id, "result":{}})))
            })
        }
    }
    fn live_connection() -> (Arc<Boundary>, CapabilityId, Connection, tokio::net::UnixStream) {
        let now = now_seconds().unwrap();
        let mut res = reservation();
        res.expires_at_unix_ms = (now + 30) * 1000;
        let live = Arc::new(Live(Mutex::new((RunStatus::Reserved {
            expires_at_unix_ms: res.expires_at_unix_ms }, 1, false))));
        let boundary = Boundary::new(live.clone()).unwrap();
        let mut grants = policy();
        grants.not_before = now;
        grants.expires_at = now + 30;
        let id = boundary.issue(&res, grants, now).unwrap();
        live.0.lock().unwrap().0 = RunStatus::Running;
        let (host, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let reply = duduclaw_isolation::ipc::SpawnResponse::Io { run: res.run,
            delivery: duduclaw_isolation::ipc::IoDelivery { mcp: true, pty: false } };
        let delivery = crate::socket::McpDelivery::from_take_io(&reply, res.control, host).unwrap();
        let connection = boundary.activate(id, &res, delivery, now).unwrap();
        client.set_nonblocking(true).unwrap();
        (boundary, id, connection, tokio::net::UnixStream::from_std(client).unwrap())
    }
    #[tokio::test]
    async fn incomplete_dispatcher_cannot_activate_a_serving_endpoint() {
        struct Missing;
        impl crate::server::Dispatcher for Missing {
            fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: Value)
                -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
                panic!("startup must reject before handler")
            }
        }
        let (boundary, id, connection, _client) = live_connection();
        assert_eq!(crate::server::serve(connection, &Missing).await, Err(VerificationError::Unavailable));
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    }
    #[tokio::test]
    async fn context_readiness_refuses_before_reading_or_dispatching_and_revokes() {
        struct MissingFinite;
        impl crate::server::Dispatcher for MissingFinite {
            fn require_complete_services(&self) -> Result<(), VerificationError> { Ok(()) }
            fn require_context_services(&self, context: &RequestContext) -> Result<(), VerificationError> {
                assert_eq!(context.record().employee, EmployeeId([2; 16]));
                assert!(!context.record().employee_scope());
                Err(VerificationError::Unavailable)
            }
            fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: Value)
                -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
                panic!("mode-incomplete dispatcher must not receive calls")
            }
        }
        let (boundary, id, connection, _client) = live_connection();
        assert_eq!(crate::server::serve(connection, &MissingFinite).await, Err(VerificationError::Unavailable));
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    }
    #[tokio::test]
    async fn initialized_notification_and_list_can_arrive_in_one_read() {
        let (boundary, id, connection, mut client) = live_connection();
        let serving = tokio::spawn(async { crate::server::serve(connection, &Echo).await });
        client.write_all(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
        ).as_bytes()).await.unwrap();
        let mut reader = tokio::io::BufReader::new(client);
        for id in [1, 2] {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
            assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], id);
        }
        drop(reader);
        assert!(serving.await.unwrap().is_ok());
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    }
    #[tokio::test]
    async fn revocation_cancels_active_handler_and_idle_connection() {
        for active in [true, false] {
            let (boundary, _, connection, mut client) = live_connection();
            let serving = tokio::spawn(async { crate::server::serve(connection, &Echo).await });
            if active { client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"wait\"}\n").await.unwrap(); }
            tokio::time::sleep(Duration::from_millis(20)).await;
            boundary.revoke_run(reservation().run).unwrap();
            assert!(tokio::time::timeout(Duration::from_secs(2), serving).await.unwrap().unwrap().is_err());
        }
    }
    #[tokio::test]
    async fn disconnect_cancels_active_handler_and_revokes() {
        let (boundary, id, connection, mut client) = live_connection();
        let serving = tokio::spawn(async { crate::server::serve(connection, &Echo).await });
        client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"wait\"}\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(client);
        assert!(tokio::time::timeout(Duration::from_secs(2), serving).await.unwrap().unwrap().is_ok());
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    }
    #[tokio::test]
    async fn concurrent_inflight_requests_are_bounded_and_refused() {
        let (_, _, connection, mut client) = live_connection();
        let serving = tokio::spawn(async { crate::server::serve(connection, &Echo).await });
        client.write_all(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"wait\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"wait\"}\n"
        ).as_bytes()).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), serving).await.unwrap().unwrap().is_err());
    }
    #[test]
    fn pathname_socket_is_not_a_take_io_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(temp.path().join("socket")).unwrap();
        let client = std::os::unix::net::UnixStream::connect(temp.path().join("socket")).unwrap();
        let (host, _) = listener.accept().unwrap();
        assert!(crate::socket::validate(&client).is_err());
        assert!(crate::socket::validate(&host).is_err());
    }
    #[test]
    fn mismatched_take_io_reply_cannot_activate_reserved_capability() {
        use duduclaw_isolation::ipc::{IoDelivery, SpawnResponse};
        let (boundary, live, id) = setup();
        live.0.lock().unwrap().0 = RunStatus::Running;
        let (host, _client) = std::os::unix::net::UnixStream::pair().unwrap();
        let reply = SpawnResponse::Io { run: RunHandle(RunId([9; 16])),
            delivery: IoDelivery { mcp: true, pty: false } };
        let delivery = crate::socket::McpDelivery::from_take_io(&reply, reservation().control, host).unwrap();
        assert!(matches!(boundary.activate(id, &reservation(), delivery, 101),
            Err(VerificationError::BindingMismatch)));
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Pending));
    }
}

#[tokio::test]
async fn all_revocation_paths_notify_without_polling_and_cancel_inflight_future() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }
    for mode in 0..7 {
        let (boundary, live, id) = setup();
        live.0.lock().unwrap().0 = RunStatus::Running;
        let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
        let mut revoked = connection.revocation().unwrap();
        let context = connection.context(101).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = Dropped(dropped.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(async move {
            let handler = async move {
                let _marker = marker;
                started.send(()).unwrap();
                std::future::pending::<()>().await;
            };
            tokio::select! {
                biased;
                _ = crate::server::wait_revoked(&mut revoked) => {},
                _ = handler => panic!("inflight handler completed"),
            }
        });
        ready.await.unwrap();
        match mode {
            0 => boundary.revoke_run(reservation().run).unwrap(),
            1 => boundary.revoke_control(reservation().control).unwrap(),
            2 => boundary.revoke_all().unwrap(),
            3 => { live.0.lock().unwrap().1 += 1; assert!(context.verify_live(101).is_err()); },
            4 => { live.0.lock().unwrap().2 = true; assert!(context.verify_live(101).is_err()); },
            5 => boundary.sweep(200).unwrap(),
            _ => drop(connection),
        }
        // No polling task or timer is involved in this cancellation path.
        tokio::time::timeout(std::time::Duration::from_millis(50), serving).await.unwrap().unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(context.verify_live(101).is_err());
    }
}

#[tokio::test]
async fn revocation_is_sticky_before_the_waiter_first_polls() {
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
    let mut revoked = connection.revocation().unwrap();
    boundary.revoke_run(reservation().run).unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(50),
        crate::server::wait_revoked(&mut revoked)).await.unwrap();
}

#[test]
fn live_request_verifier_fences_effects_after_run_or_policy_change() {
    for mode in 0..5 {
        let (boundary, live, id) = setup();
        live.0.lock().unwrap().0 = RunStatus::Running;
        let connection = boundary.activate_inner(id, &reservation(), 101).unwrap();
        let context = connection.context(101).unwrap();
        assert!(context.verify_live(101).is_ok());
        match mode {
            0 => boundary.revoke_run(reservation().run).unwrap(),
            1 => live.0.lock().unwrap().0 = RunStatus::Ended(RunOutcome::Cancelled),
            2 => live.0.lock().unwrap().1 += 1,
            3 => live.0.lock().unwrap().2 = true,
            _ => drop(connection),
        }
        let mut effects = 0;
        let mut effect = || -> Result<(), VerificationError> {
            context.verify_live(101)?;
            effects += 1;
            Ok(())
        };
        assert!(effect().is_err());
        assert_eq!(effects, 0);
    }
}

#[test]
fn detached_context_has_no_live_io_authority() {
    let (boundary, _, id) = setup();
    let (record, _, _) = proof(&boundary, id);
    let context = RequestContext::from_verified_record(record);
    assert_eq!(context.verify_live(101), Err(VerificationError::Unavailable));
}
