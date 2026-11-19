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
    /// R8z-5: like the production dispatcher (it resolves the run's turn in
    /// the blocking pool before every handler), every handler here yields,
    /// notifications included. `fail` is refused; `wait` never answers.
    struct Yielding;
    impl crate::server::Dispatcher for Yielding {
        fn require_complete_services(&self) -> Result<(), VerificationError> { Ok(()) }
        fn dispatch<'a>(&'a self, _context: RequestContext, _slug: &'a str, request: Value)
            -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if request["method"] == "wait" { std::future::pending::<()>().await; }
                if request["method"] == "fail" { return Err(VerificationError::BindingMismatch); }
                Ok(request.get("id").map(|id| json!({"jsonrpc":"2.0", "id":id, "result":{}})))
            })
        }
    }
    fn live_connection() -> (Arc<Boundary>, CapabilityId, Connection, tokio::net::UnixStream) {
        let (boundary, id, connection, client, _) = live_connection_with_authority();
        (boundary, id, connection, client)
    }
    fn live_connection_with_authority() -> (Arc<Boundary>, CapabilityId, Connection, tokio::net::UnixStream, Arc<Live>) {
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
        (boundary, id, connection, tokio::net::UnixStream::from_std(client).unwrap(), live)
    }

    /// R8z-5 (the real-host defect): the API worker awaits the `initialize`
    /// reply first, then sends `notifications/initialized` and at once `tools/list`. With a handler
    /// that yields, the notification used to count as a request in flight and
    /// `tools/list` as a second one: the session closed and the worker saw
    /// "MCP disconnected". Both requests are now answered, in order.
    #[tokio::test]
    async fn r8z5_a_yielding_initialized_notification_does_not_refuse_the_next_request() {
        let (boundary, id, connection, client) = live_connection();
        let serving = tokio::spawn(async { crate::server::serve(connection, &Yielding).await });
        let mut reader = tokio::io::BufReader::new(client);
        reader.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n").await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 1);
        line.clear();
        reader.get_mut().write_all(concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
        ).as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 2);
        // The worker's own order: the notification and the request written
        // separately after the initialize reply.
        reader.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        reader.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}\n").await.unwrap();
        line.clear();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 3);
        drop(reader);
        assert!(serving.await.unwrap().is_ok());
        assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
    }

    #[tokio::test]
    async fn r8z5_a_frame_behind_an_unanswered_initialize_is_still_refused() {
        let (_, _, connection, mut client) = live_connection();
        let serving = tokio::spawn(async { crate::server::serve(connection, &Yielding).await });
        client.write_all(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
        ).as_bytes()).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), serving).await.unwrap().unwrap().is_err());
    }

    /// R8z-5: one session fed `input` (then the peer's write side closed when
    /// `close`), served with the checked loop.
    async fn session_end(connection: &mut Connection, client: tokio::net::UnixStream,
        dispatcher: &dyn crate::server::Dispatcher, input: Vec<u8>, close: bool, deadline: Duration)
        -> crate::server::SessionOutcome {
        let (_read, mut write) = client.into_split();
        let feed = async move {
            let _ = write.write_all(&input).await;
            if close { let _ = write.shutdown().await; }
            // Keep the write half until the session has ended.
            std::future::pending::<()>().await
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                outcome = crate::server::serve_with_deadline_checked(connection, dispatcher, deadline) => outcome,
                _ = feed => unreachable!(),
            }
        }).await.expect("the session must end")
    }
    async fn step_of(dispatcher: &dyn crate::server::Dispatcher, input: &[u8], close: bool)
        -> crate::server::SessionOutcome {
        let (_boundary, _id, mut connection, client) = live_connection();
        session_end(&mut connection, client, dispatcher, input.to_vec(), close, Duration::from_secs(5)).await
    }

    /// R8z-5: each exit of the serving loop names its fixed step; the error
    /// is the one `serve` returns.
    #[tokio::test]
    async fn r8z5_every_reachable_session_exit_names_its_step() {
        use crate::server::SessionStep as S;
        let line = |method: &str, id: Option<u32>| match id {
            Some(id) => format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}\n"),
            None => format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\"}}\n"),
        };
        // The normal end: a request answered and its reply read, then the
        // peer closes (the runtime exits between requests).
        let (_boundary, _id, mut connection, client) = live_connection();
        let request = line("initialize", Some(1));
        let peer = async move {
            let mut reader = tokio::io::BufReader::new(client);
            reader.get_mut().write_all(request.as_bytes()).await.unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).await.unwrap();
            assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["id"], 1);
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(crate::server::serve_with_deadline_checked(&mut connection, &Yielding, Duration::from_secs(5)), peer)
        }).await.unwrap();
        assert_eq!(outcome, Ok(None));
        // Closing before reading the reply loses it: abnormal.
        assert_eq!(step_of(&Yielding, line("initialize", Some(1)).as_bytes(), true).await,
            Ok(Some(S::PeerClosedInRequest)));
        // A notification then close is the normal end too (nothing unanswered).
        assert_eq!(step_of(&Yielding, line("notifications/initialized", None).as_bytes(), true).await, Ok(None));
        assert_eq!(step_of(&Yielding, b"", true).await, Ok(Some(S::PeerClosedBeforeRequest)));
        assert_eq!(step_of(&Yielding, line("wait", Some(1)).as_bytes(), true).await, Ok(Some(S::PeerClosedInRequest)));
        assert_eq!(step_of(&Yielding, b"[]\n", false).await, Err((S::FrameInvalid, VerificationError::Invalid)));
        assert_eq!(step_of(&Yielding, b"{\"jsonrpc\"", true).await, Err((S::FramePartial, VerificationError::Invalid)));
        assert_eq!(step_of(&Yielding, &vec![b'x'; crate::server::MAX_REQUEST_BYTES + 1], false).await,
            Err((S::FrameTooLarge, VerificationError::Invalid)));
        assert_eq!(step_of(&Yielding, line("fail", Some(1)).as_bytes(), false).await,
            Err((S::Dispatcher(VerificationError::BindingMismatch), VerificationError::BindingMismatch)));
        let two = format!("{}{}", line("wait", Some(1)), line("wait", Some(2)));
        assert_eq!(step_of(&Yielding, two.as_bytes(), false).await, Err((S::SecondRequest, VerificationError::Invalid)));
        let many: String = (0..121).map(|_| line("notifications/initialized", None)).collect();
        assert_eq!(step_of(&Echo, many.as_bytes(), false).await, Err((S::WindowLimit, VerificationError::Unavailable)));
        struct Incomplete;
        impl crate::server::Dispatcher for Incomplete {
            fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: Value)
                -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
                panic!("startup must reject before handler")
            }
        }
        assert_eq!(step_of(&Incomplete, b"", false).await, Err((S::Readiness, VerificationError::Unavailable)));
        // The request deadline.
        let (_boundary, _id, mut connection, client) = live_connection();
        assert_eq!(session_end(&mut connection, client, &Yielding, line("wait", Some(1)).into_bytes(), false,
            Duration::from_millis(150)).await, Err((S::RequestTimeout, VerificationError::Unavailable)));
        // No socket on the activated connection.
        let (_boundary, _id, mut connection, client) = live_connection();
        connection.socket = None;
        assert_eq!(session_end(&mut connection, client, &Yielding, vec![], false, Duration::from_secs(5)).await,
            Err((S::SocketMissing, VerificationError::Invalid)));
    }

    /// R8z-5: the run's revocation, and the live watch refusing (the run no
    /// longer running; the live authority unreadable), name their steps.
    #[tokio::test]
    async fn r8z5_revocation_and_the_live_watch_name_their_steps() {
        use crate::server::SessionStep as S;
        use crate::launch_steps::ContextStep as C;
        let (boundary, _id, mut connection, client, _live) = live_connection_with_authority();
        let revoke = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            boundary.revoke_run(reservation().run).unwrap();
            std::future::pending::<()>().await
        };
        let outcome = tokio::select! {
            outcome = session_end(&mut connection, client, &Yielding, vec![], false, Duration::from_secs(5)) => outcome,
            _ = revoke => unreachable!(),
        };
        assert_eq!(outcome, Err((S::Revoked, VerificationError::Revoked)));
        for unreadable in [false, true] {
            let (_boundary, _id, mut connection, client, live) = live_connection_with_authority();
            let change = async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let mut state = live.0.lock().unwrap();
                if unreadable { state.2 = true; } else { state.0 = RunStatus::Ended(RunOutcome::Cancelled); }
                drop(state);
                std::future::pending::<()>().await
            };
            let outcome = tokio::select! {
                outcome = session_end(&mut connection, client, &Yielding, vec![], false, Duration::from_secs(5)) => outcome,
                _ = change => unreachable!(),
            };
            // The refusing read also revokes the capability; whichever of the
            // two wakes the session first, the step is the live check (the
            // error is the one `serve` always returned for that branch).
            let (step, error) = outcome.unwrap_err();
            if unreadable {
                assert_eq!(step, S::Context(C::LiveSnapshot));
                assert!(matches!(error, VerificationError::Unavailable | VerificationError::Revoked), "{error:?}");
            } else {
                assert_eq!((step, error), (S::Context(C::LiveMismatch), VerificationError::Revoked));
            }
        }
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

/// R8z-3: a live authority whose principal, binding and agent slug are chosen.
struct Shaped { principal: Principal, binding: EmployeeBinding, slug: &'static str }
impl LiveAuthority for Shaped {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        Ok(LiveSnapshot { employee, run, control: ControlConnectionId([3; 16]),
            status: RunStatus::Reserved { expires_at_unix_ms: 130_000 }, principal: self.principal.clone(),
            binding: self.binding.clone(), policy_revision: 1, agent_slug: self.slug.into() })
    }
}

/// R8z-3: every refusal of `issue` names its check, and the error is the
/// one `issue` returns for the same inputs.
#[test]
fn r8z3_issue_refusals_name_their_check_with_the_unchanged_error() {
    use VerificationError as E;
    let other = || Text::new("other".into()).unwrap();
    // (mutation, step, the error the former single condition chain returned;
    // `None`: whatever `validate_grants` returns).
    let cases = [(0, IssueStep::LiveSnapshot, Some(E::Unavailable)), (1, IssueStep::NotReserved, Some(E::Invalid)),
        (2, IssueStep::Reservation, Some(E::BindingMismatch)), (3, IssueStep::Expiry, Some(E::BindingMismatch)),
        (4, IssueStep::Time, Some(E::BindingMismatch)), (5, IssueStep::Time, Some(E::InvalidTime)),
        (6, IssueStep::Principal, Some(E::BindingMismatch)), (7, IssueStep::Binding, Some(E::BindingMismatch)),
        (8, IssueStep::PolicyRevision, Some(E::BindingMismatch)), (9, IssueStep::Grants, None)];
    for (mode, step, error) in cases {
        let live = Arc::new(Live(Mutex::new((RunStatus::Reserved { expires_at_unix_ms: 130_000 }, 1, false))));
        let boundary = Boundary::new(live.clone()).unwrap();
        let (mut res, mut now) = (reservation(), 100);
        let grant = || {
            let mut grant = policy();
            match mode {
                6 => grant.principal = Principal::Human { user_id: other(), generation: 1 },
                7 => grant.binding = EmployeeBinding::Personal(other()),
                8 => grant.policy_revision = 2,
                9 => grant.resources = List::new(vec![ResourceGrant::Connector {
                    account: Text::new("account".into()).unwrap(), binding: EmployeeBinding::Personal(other()) }]).unwrap(),
                _ => {}
            }
            grant
        };
        match mode {
            0 => live.0.lock().unwrap().2 = true,
            1 => live.0.lock().unwrap().0 = RunStatus::Running,
            2 => res.control = ControlConnectionId([7; 16]),
            3 => res.expires_at_unix_ms = 130_001,
            4 => now = 130,
            5 => now = u64::MAX,
            _ => {}
        }
        let (refused, checked) = boundary.issue_checked(&res, grant(), now).err().unwrap();
        assert_eq!(refused, step, "mode {mode}");
        if let Some(error) = error { assert_eq!(checked, error, "mode {mode}"); }
        assert_eq!(boundary.issue(&res, grant(), now).err(), Some(checked), "mode {mode}");
    }
    // The second capability for one run.
    let (boundary, _, _) = setup();
    assert_eq!(boundary.issue_checked(&reservation(), policy(), 100).err(),
        Some((IssueStep::Duplicate, VerificationError::Invalid)));
    // Slug and binding/principal admission come from the live snapshot.
    let tenant = |name: &str| Text::new(name.to_owned()).unwrap();
    for (shaped, step) in [
        (Shaped { principal: principal(), binding: binding(), slug: "../x" }, IssueStep::AgentSlug),
        (Shaped { principal: Principal::Background { tenant_id: tenant("a"), generation: 1 },
            binding: EmployeeBinding::Shared(tenant("b")), slug: "alice" }, IssueStep::BindingPrincipal),
    ] {
        let mut grant = policy();
        grant.principal = shaped.principal.clone();
        grant.binding = shaped.binding.clone();
        let boundary = Boundary::new(Arc::new(shaped)).unwrap();
        assert_eq!(boundary.issue_checked(&reservation(), grant, 100).err(),
            Some((step, VerificationError::BindingMismatch)));
    }
}

/// R8z-3: refusals of the capability activation name their check.
#[test]
fn r8z3_activation_refusals_name_their_check() {
    use BoundaryActivateStep as S;
    // Reserved, not Running: the live check refuses (and revokes).
    let (boundary, _, id) = setup();
    assert_eq!(boundary.activate_inner_checked(id, &reservation(), 101).err().map(|r| r.0), Some(S::LiveMismatch));
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    assert_eq!(boundary.activate_inner_checked(CapabilityId(Sha256Digest([9; 32])), &reservation(), 101).err().map(|r| r.0),
        Some(S::Unknown));
    let mut other = reservation();
    other.run = RunHandle(RunId([7; 16]));
    assert_eq!(boundary.activate_inner_checked(id, &other, 101).err(), Some((S::Binding, VerificationError::BindingMismatch)));
    assert_eq!(boundary.activate_inner_checked(id, &reservation(), 200).err(), Some((S::Time, VerificationError::InvalidTime)));
    let (boundary, live, id) = setup();
    live.0.lock().unwrap().0 = RunStatus::Running;
    live.0.lock().unwrap().2 = true;
    assert_eq!(boundary.activate_inner_checked(id, &reservation(), 101).err(),
        Some((S::LiveSnapshot, VerificationError::Unavailable)));
}

/// R8z-4: a fresh boundary whose run is reserved until 130 s (as `setup`).
fn reserved() -> (Arc<Boundary>, Arc<Live>) {
    let live = Arc::new(Live(Mutex::new((RunStatus::Reserved { expires_at_unix_ms: 130_000 }, 1, false))));
    (Boundary::new(live.clone()).unwrap(), live)
}

/// R8z-4 regression: the grant's `not_before` is stamped when the per-run
/// authorization is issued (t = 100); the capability is issued after the run
/// was reserved, a second or more later. Issue must succeed, the record's
/// window starts at the issue (never before it), keeps the grant's expiry,
/// and the capability activates afterwards.
#[test]
fn r8z4_issue_after_the_grant_second_ticked_succeeds_and_activates() {
    let stamped = policy().not_before;
    for (issue_at, activate_at) in [(stamped + 1, stamped + 5), (stamped + 5, stamped + 6)] {
        let (boundary, live) = reserved();
        let id = boundary.issue_checked(&reservation(), policy(), issue_at)
            .unwrap_or_else(|refusal| panic!("issue at {issue_at}: {refusal:?}"));
        let (record, _, _) = proof(&boundary, id);
        assert_eq!(record.issued_at, issue_at);
        assert_eq!(record.not_before, issue_at, "never valid before it is issued");
        assert_eq!(record.expires_at, policy().expires_at, "the grant's expiry is unchanged");
        assert!(record.canonical_bytes().is_ok());
        live.0.lock().unwrap().0 = RunStatus::Running;
        let connection = boundary.activate_inner(id, &reservation(), activate_at)
            .unwrap_or_else(|error| panic!("activate at {activate_at}: {error:?}"));
        assert!(connection.context(activate_at).is_ok());
    }
    // Only narrowed: before its issue second the capability is not valid
    // (a backwards clock step between issue and activation is a refusal).
    let (boundary, live) = reserved();
    let id = boundary.issue(&reservation(), policy(), stamped + 5).unwrap();
    live.0.lock().unwrap().0 = RunStatus::Running;
    assert_eq!(boundary.activate_inner_checked(id, &reservation(), stamped + 4).err(),
        Some((BoundaryActivateStep::Time, VerificationError::InvalidTime)));
}

/// R8z-4: a grant whose `not_before` is later than the issue was accepted
/// before this change (the record kept `issued_at < not_before`) and still
/// is; the record keeps the grant's later `not_before`, so it cannot activate
/// before then.
#[test]
fn r8z4_a_grant_valid_later_keeps_its_not_before() {
    let mut later = policy();
    later.not_before = 110;
    let (boundary, live) = reserved();
    let id = boundary.issue_checked(&reservation(), later, 100).unwrap();
    let (record, _, _) = proof(&boundary, id);
    assert_eq!((record.issued_at, record.not_before, record.expires_at), (100, 110, 200));
    live.0.lock().unwrap().0 = RunStatus::Running;
    assert_eq!(boundary.activate_inner_checked(id, &reservation(), 105).err(),
        Some((BoundaryActivateStep::Time, VerificationError::InvalidTime)));
    let mut later = policy();
    later.not_before = 110;
    let (boundary, live) = reserved();
    let id = boundary.issue(&reservation(), later, 100).unwrap();
    live.0.lock().unwrap().0 = RunStatus::Running;
    assert!(boundary.activate_inner(id, &reservation(), 110).is_ok());
}

/// R8z-4: a grant whose window has passed (`now >= expires_at`), or is
/// empty, is still refused by the encoding with the unchanged error
/// (`Invalid`, formerly reported as `issue.encode`, now `issue.encode.interval`).
#[test]
fn r8z4_issue_at_or_after_the_grant_expiry_is_still_refused() {
    let interval = Some((IssueStep::Encode(EncodeCheck::Interval), VerificationError::Invalid));
    for now in [120, 121, 129] {
        let mut short = policy();
        short.expires_at = 120;
        let (boundary, _) = reserved();
        assert_eq!(boundary.issue_checked(&reservation(), short, now).err(), interval, "now {now}");
        let mut short = policy();
        short.expires_at = 120;
        let (boundary, _) = reserved();
        assert_eq!(boundary.issue(&reservation(), short, now).err(), Some(VerificationError::Invalid));
    }
    // The last second of the window still issues.
    let mut short = policy();
    short.expires_at = 120;
    assert!(reserved().0.issue(&reservation(), short, 119).is_ok());
    // An empty grant window.
    let mut empty = policy();
    empty.not_before = 150;
    empty.expires_at = 150;
    assert_eq!(reserved().0.issue_checked(&reservation(), empty, 100).err(), interval);
}

/// R8z-4: each rule of the capability encoding names its own sub-step.
/// `interval` and `duplicate_grant` are reached through `issue_checked`;
/// `resource_mode` is not (`validate_grants` applies the same rule first, as
/// `issue.grants`), so it and `other` are checked on `EncodeCheck::of`.
#[test]
fn r8z4_encode_refusals_name_their_rule() {
    // Duplicate tool grant: passes `validate_grants`, refused by the encoding.
    let mut twice = policy();
    twice.tools = List::new(vec![Text::new("file_read".into()).unwrap(), Text::new("file_read".into()).unwrap()]).unwrap();
    assert_eq!(reserved().0.issue_checked(&reservation(), twice, 100).err(),
        Some((IssueStep::Encode(EncodeCheck::DuplicateGrant), VerificationError::Invalid)));
    let mut twice = policy();
    twice.tools = List::new(vec![Text::new("file_read".into()).unwrap(), Text::new("file_read".into()).unwrap()]).unwrap();
    assert_eq!(reserved().0.issue(&reservation(), twice, 100).err(), Some(VerificationError::Invalid));
    // Through the classifier, one rule violated at a time, each with the
    // same `ContractError` the encoding returns.
    let (boundary, _, id) = setup();
    let (valid, _, _) = proof(&boundary, id);
    assert!(valid.canonical_bytes().is_ok());
    assert_eq!(EncodeCheck::of(&valid), EncodeCheck::Other);
    let mut mode = valid.clone();
    mode.resources = List::new(vec![ResourceGrant::EmployeeScope { id: EmployeeId([9; 16]) }]).unwrap();
    let mut interval = valid.clone();
    interval.issued_at = interval.not_before + 1;
    let mut duplicate = valid.clone();
    duplicate.tools = List::new(vec![Text::new("a".into()).unwrap(), Text::new("a".into()).unwrap()]).unwrap();
    for (record, check, rule) in [
        (&mode, EncodeCheck::ResourceMode, mode.validate_resource_mode()),
        (&interval, EncodeCheck::Interval, interval.validate_interval()),
        (&duplicate, EncodeCheck::DuplicateGrant, duplicate.validate_distinct_grants()),
    ] {
        let refused = record.canonical_bytes().err();
        assert!(refused.is_some(), "{check:?}");
        assert_eq!(refused, rule.err(), "{check:?}");
        assert_eq!(EncodeCheck::of(record), check);
    }
    assert_eq!(interval.validate_interval(),
        Err(duduclaw_isolation::ContractError::InvalidValue("capability validity interval")));
    assert_eq!(duplicate.validate_distinct_grants(),
        Err(duduclaw_isolation::ContractError::InvalidValue("duplicate capability grant")));
    // In `canonical_bytes`' order: a record violating every rule is `resource_mode`.
    let mut all = duplicate.clone();
    all.resources = mode.resources.clone();
    all.issued_at = all.not_before + 1;
    assert_eq!(EncodeCheck::of(&all), EncodeCheck::ResourceMode);
    all.resources = valid.resources.clone();
    assert_eq!(EncodeCheck::of(&all), EncodeCheck::Interval);
}

#[test]
fn detached_context_has_no_live_io_authority() {
    let (boundary, _, id) = setup();
    let (record, _, _) = proof(&boundary, id);
    let context = RequestContext::from_verified_record(record);
    assert_eq!(context.verify_live(101), Err(VerificationError::Unavailable));
}
