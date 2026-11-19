//! R8z-7: on a real host every MCP request of an isolated run waited 6–9 s:
//! the 100 ms live watch ran full verifications back to back with each
//! request's own (holding the connection's single authority slot), and every
//! handler repeated the request's verification to resolve its turn. These
//! tests count full live verifications (`LiveAuthority::snapshot` calls: one
//! per full verification) per request and check the guarantees that stay.
use super::*;
use std::{sync::atomic::{AtomicBool, AtomicUsize, Ordering}, time::Duration};
use duduclaw_isolation::ids::RunId;

/// Counts full live verifications. The run state is read when the check
/// STARTS; `slow` then holds the check for 300 ms (a slow root read-back).
struct Counting {
    state: Mutex<RunStatus>,
    calls: AtomicUsize,
    checked_at: Mutex<Vec<std::time::Instant>>,
    slow: AtomicBool,
    entered: AtomicBool,
}
impl LiveAuthority for Counting {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        self.checked_at.lock().unwrap().push(std::time::Instant::now());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let status = self.state.lock().unwrap().clone();
        if self.slow.load(Ordering::Acquire) {
            self.entered.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(LiveSnapshot { employee, run, control: ControlConnectionId([3; 16]), status,
            principal: principal(), binding: binding(), policy_revision: 1, agent_slug: "alice".into() })
    }
}
fn principal() -> Principal { Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 } }
fn binding() -> EmployeeBinding { EmployeeBinding::Personal(Text::new("owner".into()).unwrap()) }

/// An activated connection (no socket yet) over a `Counting` authority whose
/// counter starts at zero after activation.
fn activated() -> (Arc<Boundary>, CapabilityId, Connection, Arc<Counting>, u64) {
    let now = now_seconds().unwrap();
    let res = Reservation { run: RunHandle(RunId([1; 16])), employee: EmployeeId([2; 16]),
        control: ControlConnectionId([3; 16]), expires_at_unix_ms: (now + 30) * 1000 };
    let live = Arc::new(Counting { state: Mutex::new(RunStatus::Reserved { expires_at_unix_ms: res.expires_at_unix_ms }),
        calls: AtomicUsize::new(0), checked_at: Mutex::new(Vec::new()),
        slow: AtomicBool::new(false), entered: AtomicBool::new(false) });
    let boundary = Boundary::new(live.clone()).unwrap();
    let id = boundary.issue(&res, GrantPolicy { task: None, principal: principal(), binding: binding(),
        lane: Lane::Standard, tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(),
        policy_revision: 1, not_before: now, expires_at: now + 30 }, now).unwrap();
    *live.state.lock().unwrap() = RunStatus::Running;
    let connection = boundary.activate_inner(id, &res, now).unwrap();
    live.calls.store(0, Ordering::SeqCst);
    live.checked_at.lock().unwrap().clear();
    (boundary, id, connection, live, now)
}

/// The lifecycle sweep fully checks an active entry unless that entry's own
/// connection fully checked it (successfully) within the sweep's period;
/// activation alone does not count, and expiry is checked on every pass.
#[test]
fn r8z7_the_sweep_skips_only_an_active_entry_its_own_connection_just_checked() {
    let (boundary, id, connection, live, now) = activated();
    boundary.sweep(now).unwrap();
    assert_eq!(live.calls.load(Ordering::SeqCst), 1, "an activated, never-read entry is checked by the sweep");
    assert!(connection.context(now).is_ok());
    assert_eq!(live.calls.load(Ordering::SeqCst), 2);
    boundary.sweep(now).unwrap();
    assert_eq!(live.calls.load(Ordering::SeqCst), 2, "checked by its connection within the sweep period");
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Active(_)));
    let expires = boundary.entries.lock().unwrap()[&id.0].record.expires_at;
    boundary.sweep(expires).unwrap();
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked), "expiry is never skipped");
    assert_eq!(live.calls.load(Ordering::SeqCst), 2);
}

#[cfg(target_os = "linux")]
mod served {
    use super::*;
    use std::{future::Future, pin::Pin};
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use crate::server::SessionStep as S;
    use crate::launch_steps::ContextStep as C;

    /// A connection ready to serve with the given watch period.
    fn served(period: Duration) -> (Arc<Boundary>, Connection, tokio::net::UnixStream, Arc<Counting>) {
        let (boundary, _id, mut connection, live, _now) = activated();
        let (host, client) = std::os::unix::net::UnixStream::pair().unwrap();
        connection.socket = Some(host);
        connection.watch_period = period;
        client.set_nonblocking(true).unwrap();
        (boundary, connection, tokio::net::UnixStream::from_std(client).unwrap(), live)
    }

    /// Like the production dispatcher: every handler yields, and (when
    /// `consume`) resolves its turn once, consuming the dispatch proof.
    struct Counted { consume: bool, dispatched: AtomicUsize, proofs: AtomicUsize, kept: Mutex<Vec<RequestContext>> }
    impl Counted {
        fn new(consume: bool) -> Arc<Self> {
            Arc::new(Self { consume, dispatched: AtomicUsize::new(0), proofs: AtomicUsize::new(0), kept: Mutex::new(Vec::new()) })
        }
    }
    impl crate::server::Dispatcher for Counted {
        fn require_complete_services(&self) -> Result<(), VerificationError> { Ok(()) }
        fn dispatch<'a>(&'a self, context: RequestContext, _: &'a str, request: Value)
            -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
            Box::pin(async move {
                self.dispatched.fetch_add(1, Ordering::SeqCst);
                if self.consume {
                    if context.take_dispatch_proof() { self.proofs.fetch_add(1, Ordering::SeqCst); }
                    assert!(!context.take_dispatch_proof(), "the dispatch proof is single use");
                }
                self.kept.lock().unwrap().push(context.clone());
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(request.get("id").map(|id| json!({"jsonrpc":"2.0", "id":id, "result":{}})))
            })
        }
    }

    fn line(method: &str, id: Option<u64>) -> String {
        match id {
            Some(id) => format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}\n"),
            None => format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\"}}\n"),
        }
    }

    async fn answered(reader: &mut tokio::io::BufReader<tokio::net::UnixStream>, frames: &str, id: u64) {
        reader.get_mut().write_all(frames.as_bytes()).await.unwrap();
        let mut reply = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut reply)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["id"], id);
    }

    /// The exact count: admission 1; a request 2 (its pre-dispatch read and
    /// the read before its reply is published); a notification 1 (nothing to
    /// publish). The watch period is made long so it cannot add reads here;
    /// the dispatch itself adds none (its turn resolution has the proof).
    #[tokio::test]
    async fn r8z7_each_request_runs_exactly_its_own_full_verifications() {
        let (boundary, connection, client, live) = served(Duration::from_secs(3600));
        let dispatcher = Counted::new(true);
        let serving = {
            let dispatcher = dispatcher.clone();
            tokio::spawn(async move { crate::server::serve(connection, dispatcher.as_ref()).await })
        };
        let calls = || live.calls.load(Ordering::SeqCst);
        let mut reader = tokio::io::BufReader::new(client);
        answered(&mut reader, &line("initialize", Some(1)), 1).await;
        assert_eq!(calls(), 1 + 2, "admission, then initialize's own two");
        answered(&mut reader, &format!("{}{}", line("notifications/initialized", None), line("tools/list", Some(2))), 2).await;
        assert_eq!(calls(), 3 + 1 + 2, "the notification one, tools/list two");
        for id in 3..8 { answered(&mut reader, &line("tools/call", Some(id)), id).await; }
        assert_eq!(calls(), 6 + 2 * 5, "N sequential requests: 2N");
        assert_eq!(dispatcher.dispatched.load(Ordering::SeqCst), 8);
        assert_eq!(dispatcher.proofs.load(Ordering::SeqCst), 8, "every dispatch carried exactly one proof");
        drop(reader);
        assert!(serving.await.unwrap().is_ok());
        assert!(matches!(boundary.entries.lock().unwrap().values().next().unwrap().state, State::Revoked));
    }

    /// The proof exists only on the pre-dispatch read's context, once, and
    /// never outlives the handler: a clone a handler kept cannot present it.
    #[tokio::test]
    async fn r8z7_the_dispatch_proof_is_single_use_and_ends_with_its_handler() {
        let (_boundary, connection, client, _live) = served(Duration::from_secs(3600));
        let verifier = connection.verifier();
        assert!(!verifier.context().await.unwrap().take_dispatch_proof(), "watch/admission/post reads carry none");
        let dispatch = verifier.dispatch_context_checked().await.unwrap();
        let clone = dispatch.clone();
        assert!(dispatch.take_dispatch_proof());
        assert!(!clone.take_dispatch_proof(), "shared by every clone, single use");
        assert!(!connection.context(now_seconds().unwrap()).unwrap().take_dispatch_proof());
        drop(verifier);
        // A handler that keeps its context without resolving its turn.
        let dispatcher = Counted::new(false);
        let serving = {
            let dispatcher = dispatcher.clone();
            tokio::spawn(async move { crate::server::serve(connection, dispatcher.as_ref()).await })
        };
        let mut reader = tokio::io::BufReader::new(client);
        answered(&mut reader, &line("tools/list", Some(1)), 1).await;
        answered(&mut reader, &format!("{}{}", line("notifications/initialized", None), line("ping", Some(2))), 2).await;
        let kept = dispatcher.kept.lock().unwrap().clone();
        assert_eq!(kept.len(), 3);
        for context in kept { assert!(!context.take_dispatch_proof(), "retired when its handler ended"); }
        drop(reader);
        assert!(serving.await.unwrap().is_ok());
    }

    /// A watch read that STARTED before the request frame was read is never
    /// used for that request, even if it completes (successfully) after the
    /// frame was read: the run ends while the request waits behind it, and
    /// the request's own read refuses it before any handler runs.
    #[tokio::test]
    async fn r8z7_a_change_while_the_request_waits_refuses_it_and_an_earlier_read_is_not_reused() {
        let (_boundary, connection, mut client, live) = served(Duration::from_millis(10));
        let dispatcher = Counted::new(true);
        let serving = {
            let dispatcher = dispatcher.clone();
            tokio::spawn(async move { crate::server::serve_checked(connection, dispatcher.as_ref()).await })
        };
        while live.calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(2)).await; }
        tokio::time::sleep(Duration::from_millis(30)).await;
        live.slow.store(true, Ordering::Release);
        // A watch read is now in flight; it read `Running` when it started.
        while !live.entered.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(2)).await; }
        let before = live.calls.load(Ordering::SeqCst);
        client.write_all(line("tools/list", Some(1)).as_bytes()).await.unwrap();
        // The frame is read while that watch read still runs (300 ms).
        tokio::time::sleep(Duration::from_millis(50)).await;
        live.slow.store(false, Ordering::Release);
        *live.state.lock().unwrap() = RunStatus::Ended(duduclaw_isolation::execution::RunOutcome::Cancelled);
        let outcome = tokio::time::timeout(Duration::from_secs(5), serving).await.unwrap().unwrap();
        assert_eq!(outcome, Err((S::Context(C::LiveMismatch), VerificationError::Revoked)));
        assert_eq!(dispatcher.dispatched.load(Ordering::SeqCst), 0, "no handler ran");
        assert!(live.calls.load(Ordering::SeqCst) > before, "the request ran its own read after the stale one");
    }

    /// The watch keeps its period on an idle session (paced, not back to
    /// back) and still closes it on a live change within about one period.
    #[tokio::test]
    async fn r8z7_the_watch_still_closes_an_idle_session_within_its_period() {
        // Allow timer resolution and scheduler/blocking-pool jitter on real clocks.
        const EARLY_JITTER: Duration = Duration::from_millis(20);
        const LATE_JITTER: Duration = Duration::from_millis(80);
        const CLOSE_JITTER: Duration = Duration::from_millis(150);
        let (_boundary, connection, _client, live) = served(WATCH_PERIOD);
        let dispatcher = Counted::new(true);
        let serving = tokio::spawn(async move { crate::server::serve_checked(connection, dispatcher.as_ref()).await });
        while live.calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(2)).await; }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let idle = live.calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(550)).await;
        let watched = live.calls.load(Ordering::SeqCst) - idle;
        assert!((2..=7).contains(&watched), "about one watch read per 100 ms, got {watched}");
        {
            let checked_at = live.checked_at.lock().unwrap();
            // The first read is admission; all subsequent reads are idle watches.
            let watches = &checked_at[1..];
            assert!(watches.len() >= 4, "need at least three watch intervals, got {} reads", watches.len());
            for pair in watches.windows(2) {
                let interval = pair[1].duration_since(pair[0]);
                assert!(interval >= WATCH_PERIOD - EARLY_JITTER && interval <= WATCH_PERIOD + LATE_JITTER,
                    "idle watch interval {interval:?} outside {:?}..={:?}",
                    WATCH_PERIOD - EARLY_JITTER, WATCH_PERIOD + LATE_JITTER);
            }
        }
        let changed = std::time::Instant::now();
        *live.state.lock().unwrap() = RunStatus::Ended(duduclaw_isolation::execution::RunOutcome::Cancelled);
        let outcome = tokio::time::timeout(Duration::from_secs(2), serving).await.unwrap().unwrap();
        assert_eq!(outcome, Err((S::Context(C::LiveMismatch), VerificationError::Revoked)));
        assert!(changed.elapsed() <= WATCH_PERIOD + CLOSE_JITTER, "closed after {:?}", changed.elapsed());
        assert!(changed.elapsed() < Duration::from_millis(500), "closed after {:?}", changed.elapsed());
    }

    /// A request's reads satisfy the watch: while they complete more often
    /// than the period, the watch runs none of its own; once they stop it
    /// runs one period after the last one.
    #[tokio::test]
    async fn r8z7_request_reads_satisfy_the_watch_so_it_never_runs_back_to_back_with_them() {
        let (_boundary, connection, _client, live) = served(Duration::from_millis(200));
        let verifier = connection.verifier();
        let reads = async {
            for _ in 0..10 {
                verifier.context().await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            biased;
            _ = verifier.watch_checked() => panic!("the watch must not run while reads keep it satisfied"),
            () = reads => {}
        }
        assert_eq!(live.calls.load(Ordering::SeqCst), 10);
        let started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(2), verifier.watch_checked()).await.unwrap().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(150), "ran after {:?}", started.elapsed());
        assert_eq!(live.calls.load(Ordering::SeqCst), 11);
    }
}
