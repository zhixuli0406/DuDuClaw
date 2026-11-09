//! HX6 in-run recording (D8 revision §4): same-run stop through `(run, call)`,
//! stored results with artifact digests, run-end flush, hard kill, offline
//! parent, revocation. The backend double plays spawnd: a recording-slot call
//! ends when `HelperStop` (flush), `HelperCancel` or the run end arrives.
use super::*;
use std::{collections::HashMap, sync::{Mutex, atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering}}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, PublicOrigin, ResourceGrant}, execution::{BackendError, BackendFuture, HelperFuture, PtySize,
    RunOutcome, RunPty, RunStdio, RuntimeKind, SpawnPolicy, WaitOrigin}, ids::{EmployeeId, RunId, Sha256Digest},
    ipc::HelperCallId};
use sha2::Digest;
use crate::{execution::{AuthorizedHelperPlan, PolicyFuture}, recording_jobs::{RecordingState, SqliteRecordingJobs},
    tool_policy::{AuthorizedToolCall, ResourceResolver}};

const CALLER: RunId = RunId([3; 16]);
const LATER: RunId = RunId([4; 16]);
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
/// The record the HX6 selector signs for an interactive recording run:
/// EmployeeScope, the own Employee, `Workspace { employee-work }` and — the
/// authority for the start's navigation `url` — `PublicOrigins { * }` (signed
/// only with `[isolation] public_web`; `tool_policy::RECORDING_URL_TOOLS`).
fn context(run: RunId, live: Arc<AtomicBool>) -> RequestContext { context_with(run, live, true) }
/// `public_web = false`: no PublicOrigins grant is signed.
fn context_with(run: RunId, live: Arc<AtomicBool>, public_web: bool) -> RequestContext {
    let mut resources = vec![ResourceGrant::EmployeeScope { id: EMPLOYEE }, ResourceGrant::Employee { id: EMPLOYEE },
        ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }];
    if public_web {
        resources.push(ResourceGrant::PublicOrigins { origins: List::new(vec![PublicOrigin::any()]).unwrap() });
    }
    let record = CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run, task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(START.into()).unwrap(), Text::new(STOP.into()).unwrap()]).unwrap(),
        resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX - 1,
        nonce: [5; 32], jti: [6; 16],
    };
    record.canonical_bytes().expect("the fixture record is signable");
    RequestContext::with_transport(record, Arc::new(Live(live)), run)
}
fn live() -> Arc<AtomicBool> { Arc::new(AtomicBool::new(true)) }

/// Mirrors the gateway resolver: Employee + Workspace, and no URL authority —
/// the boundary itself selects the record's signed PublicOrigins for the url.
struct Resolver;
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<duduclaw_isolation::capability::ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }])
    }
}

fn sha(bytes: &[u8]) -> String { sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect() }
/// What the real driver + `recording_io::publish` print after a flush.
fn flushed(call: HelperCallId) -> Vec<u8> {
    let artifacts: Vec<Value> = crate::recording_jobs::ARTIFACT_NAMES.iter()
        .map(|name| json!({"name":name, "bytes":name.len(), "sha256":sha(name.as_bytes())})).collect();
    serde_json::to_vec(&json!({"id":call.hex(), "status":"recorded", "reason":"stop", "actions":0,
        "artifacts":artifacts, "navigation":"completed"})).unwrap()
}

type End = (Vec<u8>, HelperOutcome);
struct Backend {
    features: SpawndFeatures,
    absent: bool,
    /// spawnd's own recording gate refuses the call (finding 4).
    root_gate_closed: bool,
    /// The helper finds a browser process alive after the kill (finding 2): exit 75.
    surviving: AtomicBool,
    /// The helper exits 0 with bytes that do not decode (finding 6 audit).
    invalid: AtomicBool,
    next: AtomicU8,
    ended_run: AtomicBool,
    started: AtomicUsize, stopped: AtomicUsize, cancelled: AtomicUsize, killed: AtomicUsize, reserved: AtomicUsize,
    calls: Mutex<HashMap<HelperCallId, tokio::sync::watch::Sender<Option<End>>>>,
}
impl Backend {
    fn new() -> Self {
        Self { features: SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::HELPER_RECORDING), absent: false,
            root_gate_closed: false, surviving: AtomicBool::new(false), invalid: AtomicBool::new(false), next: AtomicU8::new(1), ended_run: AtomicBool::new(false), started: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0), cancelled: AtomicUsize::new(0), killed: AtomicUsize::new(0),
            reserved: AtomicUsize::new(0), calls: Mutex::new(HashMap::new()) }
    }
    fn end(&self, call: HelperCallId, end: End) {
        if let Some(sender) = self.calls.lock().unwrap().get(&call) { sender.send_replace(Some(end)); }
    }
    /// The run ends: the launcher flushed (`flush`) or was SIGKILLed first.
    fn end_run(&self, flush: bool) {
        self.ended_run.store(true, Ordering::SeqCst);
        let calls: Vec<_> = self.calls.lock().unwrap().keys().copied().collect();
        for call in calls { self.end(call, (if flush { flushed(call) } else { vec![] }, HelperOutcome::RunEnded)); }
    }
    fn count(counter: &AtomicUsize) -> usize { counter.load(Ordering::SeqCst) }
}
struct Out { rx: tokio::sync::watch::Receiver<Option<End>>, done: bool }
impl RunReader for Out {
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            if self.done { return Ok(0); }
            loop {
                if let Some((bytes, _)) = self.rx.borrow().clone() {
                    self.done = true;
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    return Ok(bytes.len());
                }
                if self.rx.changed().await.is_err() { return Ok(0); }
            }
        })
    }
}
struct Empty;
impl RunReader for Empty {
    fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> { Box::pin(async { Ok(0) }) }
}
impl ExecutionBackend for Backend {
    fn reserve(&self, _: EmployeeId) -> BackendFuture<'_, RunHandle> {
        self.reserved.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(BackendError::Denied) })
    }
    fn spawn(&self, _: SpawnPolicy) -> BackendFuture<'_, RunHandle> { Box::pin(async { Err(BackendError::Denied) }) }
    fn wait(&self, _: RunHandle, _: WaitOrigin) -> BackendFuture<'_, RunOutcome> { Box::pin(async { Err(BackendError::Denied) }) }
    // [F8] Unreachable for in-run recordings: counted to prove it.
    fn kill(&self, _: RunHandle) -> BackendFuture<'_, ()> {
        self.killed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn kill_authorized(&self, _: RunHandle, _: Arc<dyn duduclaw_isolation::execution::CancelAuthority>) -> BackendFuture<'_, ()> {
        self.killed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> { Box::pin(async { Err(BackendError::Denied) }) }
    fn take_pty(&self, _: RunHandle) -> BackendFuture<'_, RunPty> { Box::pin(async { Err(BackendError::Denied) }) }
    fn resize(&self, _: RunHandle, _: PtySize) -> BackendFuture<'_, ()> { Box::pin(async { Err(BackendError::Denied) }) }
    fn supports_helper_calls(&self) -> bool { true }
    fn helper_features(&self, run: RunHandle) -> SpawndFeatures {
        if run.0 == CALLER && !self.ended_run.load(Ordering::SeqCst) { self.features } else { SpawndFeatures::NONE }
    }
    fn start_helper(&self, plan: duduclaw_isolation::execution::HelperPlan) -> HelperFuture<'_, HelperStart> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            assert_eq!((plan.run.0, plan.employee, plan.slot), (CALLER, EMPLOYEE, HelperSlot::Recording));
            if self.absent { return Err(HelperError::Absent); }
            if self.root_gate_closed { return Err(HelperError::RecordingUnsupported); }
            let call = HelperCallId::new([self.next.fetch_add(1, Ordering::SeqCst); 16]).unwrap();
            let (sender, rx) = tokio::sync::watch::channel(None);
            self.calls.lock().unwrap().insert(call, sender);
            Ok(HelperStart { call, effective_timeout_ms: 45_000, stdout: Box::new(Out { rx, done: false }), stderr: Box::new(Empty) })
        })
    }
    fn wait_helper(&self, run: RunHandle, call: HelperCallId) -> HelperFuture<'_, HelperOutcome> {
        Box::pin(async move {
            assert_eq!(run.0, CALLER);
            let mut rx = self.calls.lock().unwrap().get(&call).map(|sender| sender.subscribe()).ok_or(HelperError::NotLive)?;
            loop {
                if let Some((_, outcome)) = rx.borrow().clone() { return Ok(outcome); }
                rx.changed().await.map_err(|_| HelperError::NotLive)?;
            }
        })
    }
    fn stop_helper(&self, run: RunHandle, call: HelperCallId) -> HelperFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(run.0, CALLER, "only the recording's own run is ever stopped");
            if self.ended_run.load(Ordering::SeqCst) { return Err(HelperError::NotLive); }
            self.stopped.fetch_add(1, Ordering::SeqCst);
            // The launcher writes `stop\n`; the driver flushes and exits 0
            // (or, finding 2, the helper refuses to publish: exit 75, no output).
            if self.surviving.load(Ordering::SeqCst) { self.end(call, (vec![], HelperOutcome::Exited(75))); }
            else if self.invalid.load(Ordering::SeqCst) { self.end(call, (b"not a result".to_vec(), HelperOutcome::Exited(0))); }
            else { self.end(call, (flushed(call), HelperOutcome::Exited(0))); }
            Ok(())
        })
    }
    fn cancel_helper(&self, _: RunHandle, call: HelperCallId) -> HelperFuture<'_, ()> {
        Box::pin(async move {
            self.cancelled.fetch_add(1, Ordering::SeqCst);
            self.end(call, (vec![], HelperOutcome::Signalled(9)));
            Ok(())
        })
    }
}

struct Factory;
impl ExecutionPlanFactory for Factory {
    fn supports(&self, name: &str) -> bool { tool_policy::IN_RUN_RECORDING_TOOLS.contains(&name) }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall, _: RunHandle) -> PolicyFuture<'a, crate::execution::AuthorizedExecutionPlan> {
        panic!("the recording never prepares a child-run plan")
    }
    fn prepare_in_run<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, AuthorizedHelperPlan> {
        Box::pin(async move {
            let record = call.context().record();
            AuthorizedHelperPlan::recording(RunHandle(record.run), record.employee,
                RuntimeKind::Registered(Text::new("duduclaw-browser-record-v1".into()).unwrap()), 45_000, b"{}".to_vec(), 65_536)
        })
    }
    fn decode_helper_result(&self, _: &str, stdout: &[u8], stderr: &[u8], outcome: &HelperOutcome) -> Result<Value, VerificationError> {
        if !stderr.is_empty() || !matches!(outcome, HelperOutcome::Exited(0) | HelperOutcome::RunEnded) {
            return Err(VerificationError::Invalid);
        }
        serde_json::from_slice(stdout).map_err(|_| VerificationError::Invalid)
    }
}

struct Fixture { _home: tempfile::TempDir, backend: Arc<Backend>, store: Arc<SqliteRecordingJobs>, bridge: ExecutionToolBridge }
fn setup(backend: Backend) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteRecordingJobs::open(&home.path().join("jobs.db")).unwrap();
    let backend = Arc::new(backend);
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory), Arc::new(Resolver)).with_recordings(store.clone());
    Fixture { _home: home, backend, store, bridge }
}
fn body(result: &Value) -> Value { serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap_or(Value::Null) }
async fn start(fixture: &Fixture, context: &RequestContext) -> HelperCallId {
    let started = fixture.bridge.execute(context, START, &json!({"url":"https://example.com","headless":true})).await;
    assert_eq!(started["isError"], false, "{started}");
    let body = body(&started);
    assert_eq!(body["status"], "recording");
    crate::recording_jobs::parse_id(body["id"].as_str().unwrap()).unwrap()
}
async fn terminal(store: &SqliteRecordingJobs, call: HelperCallId) -> crate::recording_jobs::RecordingRow {
    for _ in 0..300 {
        let row = store.admin_read(call).unwrap();
        if row.state.terminal() { return row; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("recording never ended");
}

#[tokio::test]
async fn stop_same_run_allowed() {
    let fixture = setup(Backend::new());
    let caller = context(CALLER, live());
    let call = start(&fixture, &caller).await;
    // The `:180` inversion: the caller's own run is the allowed case.
    let stopped = fixture.bridge.execute(&caller, STOP, &json!({"id":call.hex()})).await;
    assert_eq!(stopped["isError"], false, "{stopped}");
    assert_eq!(body(&stopped)["status"], "complete");
    assert_eq!(Backend::count(&fixture.backend.stopped), 1, "HelperStop for exactly that (run, call)");
    // [F8] never a run cancel, reservation or kill.
    assert_eq!((Backend::count(&fixture.backend.killed), Backend::count(&fixture.backend.reserved),
        Backend::count(&fixture.backend.cancelled)), (0, 0, 0));
    // Without the run's own transport, or as another actor, nothing is stopped.
    let other = setup(Backend::new());
    let call = start(&other, &caller).await;
    let no_transport = RequestContext::with_live_verifier(caller.record().clone(), Arc::new(Live(live())));
    assert_eq!(other.bridge.execute(&no_transport, STOP, &json!({"id":call.hex()})).await["isError"], true);
    let mut thief = caller.record().clone();
    thief.principal = Principal::Human { user_id: Text::new("thief".into()).unwrap(), generation: 1 };
    let thief = RequestContext::with_transport(thief, Arc::new(Live(live())), CALLER);
    assert_eq!(other.bridge.execute(&thief, STOP, &json!({"id":call.hex()})).await["isError"], true);
    for args in [json!({"id":call.hex(),"action":"cancel"}), json!({"id":"../recording"}), json!({"id":"0".repeat(32)})] {
        assert_eq!(other.bridge.execute(&caller, STOP, &args).await["isError"], true, "{args}");
    }
    assert_eq!(Backend::count(&other.backend.stopped), 0);
}

#[tokio::test]
async fn recording_stop_same_run_returns_artifacts() {
    let fixture = setup(Backend::new());
    let caller = context(CALLER, live());
    let call = start(&fixture, &caller).await;
    let stopped = body(&fixture.bridge.execute(&caller, STOP, &json!({"id":call.hex()})).await);
    let result = &stopped["result"];
    assert_eq!((result["status"].as_str(), result["ended_by"].as_str()), (Some("recorded"), Some("stop")));
    assert_eq!(result["id"], call.hex());
    let digests = fixture.store.artifacts(EMPLOYEE, call).unwrap();
    assert_eq!(digests.len(), 3);
    // [F11] Stored digests catch a runtime rewrite of the published files.
    assert!(crate::recording_jobs::verify_artifact(&digests, "trace.zip", b"trace.zip".to_vec()).is_ok());
    assert_eq!(crate::recording_jobs::verify_artifact(&digests, "trace.zip", b"forged!!!".to_vec()).unwrap_err().code(), "artifact_modified");
}

#[tokio::test]
async fn recording_flushed_at_run_end_then_later_stop_reads_store() {
    let fixture = setup(Backend::new());
    let call = start(&fixture, &context(CALLER, live())).await;
    fixture.backend.end_run(true);
    let row = terminal(&fixture.store, call).await;
    assert_eq!(row.state, RecordingState::Complete);
    assert_eq!(row.result.as_ref().unwrap()["ended_by"], "run_end");
    // A later run of the same actor: the stored result, no helper interaction.
    let later = body(&fixture.bridge.execute(&context(LATER, live()), STOP, &json!({"id":call.hex()})).await);
    assert_eq!((later["status"].as_str(), later["result"]["status"].as_str()), (Some("complete"), Some("recorded")));
    assert_eq!(Backend::count(&fixture.backend.stopped), 0);
    assert_eq!(Backend::count(&fixture.backend.killed), 0);
}

#[tokio::test]
async fn recording_hard_kill_stores_uncertain() {
    let fixture = setup(Backend::new());
    let call = start(&fixture, &context(CALLER, live())).await;
    fixture.backend.end_run(false);
    let row = terminal(&fixture.store, call).await;
    assert_eq!(row.state, RecordingState::Uncertain);
    assert_eq!(row.result.as_ref().unwrap(), &json!({"status":"uncertain","reason":"run_killed"}));
    assert!(fixture.store.artifacts(EMPLOYEE, call).is_err(), "artifacts are never fabricated");
    let later = body(&fixture.bridge.execute(&context(LATER, live()), STOP, &json!({"id":call.hex()})).await);
    assert_eq!(later["status"], "uncertain");
}

#[tokio::test]
async fn recording_refused_in_offline_parent() {
    let mut backend = Backend::new();
    backend.absent = true;
    let fixture = setup(backend);
    let result = fixture.bridge.execute(&context(CALLER, live()), START, &json!({"url":"https://example.com","headless":true})).await;
    assert_eq!((result["isError"].as_bool(), result["helper_status"].as_str()), (Some(true), Some("helper_parent_offline")));
    assert!(fixture.store.admin_read(HelperCallId::new([1; 16]).unwrap()).is_err(), "no job row");
    // HX6 gate: without `helper_recording` nothing is even started.
    let mut backend = Backend::new();
    backend.features = SpawndFeatures::HELPER_CALLS;
    let gated = setup(backend);
    let result = gated.bridge.execute(&context(CALLER, live()), START, &json!({"url":"https://example.com","headless":true})).await;
    assert_eq!(result["helper_status"], "helper_recording_unsupported");
    assert_eq!(Backend::count(&gated.backend.started), 0);
}

/// The start `url` needs the record's signed `PublicOrigins` grant
/// (`[isolation] public_web`): without it — even for a public URL — and for a
/// URL that grant never permits (private, loopback, credentialed, non-https),
/// the boundary refuses before any helper start or row.
#[tokio::test]
async fn recording_start_unsigned_origin_is_refused() {
    let fixture = setup(Backend::new());
    let args = json!({"url":"https://example.com","headless":true});
    let result = fixture.bridge.execute(&context_with(CALLER, live(), false), START, &args).await;
    assert_eq!(result, refused());
    for url in ["https://127.0.0.1/", "https://localhost/", "https://user:pw@example.com/", "http://example.com/"] {
        let result = fixture.bridge.execute(&context(CALLER, live()), START, &json!({"url":url,"headless":true})).await;
        assert_eq!(result, refused(), "{url}");
    }
    assert_eq!(Backend::count(&fixture.backend.started), 0);
    assert!(fixture.store.admin_read(HelperCallId::new([1; 16]).unwrap()).is_err(), "no job row");
}

/// HX6 url authority end to end: the same public start with the signed
/// PublicOrigins grant is authorized and starts the recording.
#[tokio::test]
async fn hx6url_recording_start_with_public_origins_starts_and_without_is_refused() {
    let fixture = setup(Backend::new());
    let call = start(&fixture, &context(CALLER, live())).await;
    assert_eq!(Backend::count(&fixture.backend.started), 1);
    assert!(fixture.store.admin_read(call).is_ok(), "job row for the authorized start");
    let off = fixture.bridge.execute(&context_with(CALLER, live(), false), START,
        &json!({"url":"https://example.com","headless":true})).await;
    assert_eq!(off, refused());
    assert_eq!(Backend::count(&fixture.backend.started), 1, "nothing started without public_web");
}

#[tokio::test]
async fn second_recording_same_run_ok() {
    let fixture = setup(Backend::new());
    let caller = context(CALLER, live());
    let first = start(&fixture, &caller).await;
    assert_eq!(body(&fixture.bridge.execute(&caller, STOP, &json!({"id":first.hex()})).await)["status"], "complete");
    let second = start(&fixture, &caller).await;
    assert_ne!(first, second);
    assert_eq!(body(&fixture.bridge.execute(&caller, STOP, &json!({"id":second.hex()})).await)["status"], "complete");
    // The first stored result is untouched by the second recording.
    assert_eq!(fixture.store.admin_read(first).unwrap().result.unwrap()["id"], first.hex());
    assert_eq!(Backend::count(&fixture.backend.stopped), 2);
}

#[tokio::test]
async fn revocation_stops_recording() {
    let fixture = setup(Backend::new());
    let flag = live();
    let call = start(&fixture, &context(CALLER, flag.clone())).await;
    flag.store(false, Ordering::SeqCst);
    let row = terminal(&fixture.store, call).await;
    assert_eq!(row.result.as_ref().unwrap()["ended_by"], "revoked");
    assert_eq!(Backend::count(&fixture.backend.stopped), 1, "revocation sends HelperStop");
    assert_eq!((Backend::count(&fixture.backend.killed), Backend::count(&fixture.backend.cancelled)), (0, 0));
}

#[tokio::test]
async fn admin_recording_control_stops_live_and_reads_ended() {
    let fixture = setup(Backend::new());
    let call = start(&fixture, &context(CALLER, live())).await;
    let status = control_recording(fixture.store.clone(), fixture.backend.clone(), &call.hex(), false).await.unwrap();
    assert_eq!(status["status"], "recording");
    let stopped = control_recording(fixture.store.clone(), fixture.backend.clone(), &call.hex(), true).await.unwrap();
    assert_eq!((stopped["status"].as_str(), stopped["result"]["ended_by"].as_str()), (Some("complete"), Some("admin")));
    // Ended: the stored result, no second HelperStop; Admin never starts a helper.
    control_recording(fixture.store.clone(), fixture.backend.clone(), &call.hex(), true).await.unwrap();
    assert_eq!((Backend::count(&fixture.backend.stopped), Backend::count(&fixture.backend.started)), (1, 1));
    assert!(control_recording(fixture.store.clone(), fixture.backend.clone(), "../x", true).await.is_err());
}

/// A store whose insert is slow, to drop the start request mid-insert.
struct SlowInsert { inner: Arc<SqliteRecordingJobs>, entered: Arc<tokio::sync::Notify> }
impl RecordingJobs for SlowInsert {
    fn insert(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError> {
        self.entered.notify_one();
        std::thread::sleep(Duration::from_millis(300));
        self.inner.insert(context, key)
    }
    fn read(&self, context: &RequestContext, call: HelperCallId) -> Result<RecordingRow, VerificationError> { self.inner.read(context, call) }
    fn request_stop(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError> { self.inner.request_stop(context, key) }
    fn mark_stop(&self, key: RecordingKey, by: StopBy) -> Result<(), VerificationError> { self.inner.mark_stop(key, by) }
    fn stop_by(&self, key: RecordingKey) -> Result<Option<StopBy>, VerificationError> { self.inner.stop_by(key) }
    fn finish(&self, key: RecordingKey, result: Value) -> Result<(), VerificationError> { self.inner.finish(key, result) }
    fn admin_read(&self, call: HelperCallId) -> Result<RecordingRow, VerificationError> { self.inner.admin_read(call) }
}

/// Finding 8: a start request dropped while its row insert is in flight
/// cancels the slot and finishes the row only after the insert committed,
/// so no row stays `recording` until the next gateway restart.
#[tokio::test]
async fn hx6fix_start_guard_finishes_only_after_insert_commits() {
    let home = tempfile::tempdir().unwrap();
    let inner = SqliteRecordingJobs::open(&home.path().join("jobs.db")).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let store = Arc::new(SlowInsert { inner: inner.clone(), entered: entered.clone() });
    let backend = Arc::new(Backend::new());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory), Arc::new(Resolver)).with_recordings(store);
    let caller = context(CALLER, live());
    let arguments = json!({"url":"https://example.com","headless":true});
    // Drop the request (MCP cancel) once its insert is running.
    tokio::select! {
        _ = bridge.execute(&caller, START, &arguments) => panic!("the insert is still blocked"),
        () = entered.notified() => {}
    }
    let call = HelperCallId::new([1; 16]).unwrap();
    let mut row = None;
    for _ in 0..300 {
        if let Ok(read) = inner.admin_read(call) { if read.state.terminal() { row = Some(read); break; } }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = row.expect("the abandoned row must become terminal, not stay `recording`");
    assert_eq!(row.state, RecordingState::Uncertain);
    assert_eq!(row.result.unwrap(), json!({"status":"uncertain","reason":"start_abandoned"}));
    assert_eq!(Backend::count(&backend.cancelled), 1, "the slot is cancelled, never the run");
    assert_eq!(Backend::count(&backend.killed), 0);
}

/// Finding 2: a browser process that outlived the kill (no `ECHILD`) makes
/// the helper refuse publication (exit 75); the row is uncertain, never a recording.
#[tokio::test]
async fn hx6fix_surviving_browser_tree_is_uncertain_without_artifacts() {
    let fixture = setup(Backend::new());
    fixture.backend.surviving.store(true, Ordering::SeqCst);
    let caller = context(CALLER, live());
    let call = start(&fixture, &caller).await;
    let stopped = body(&fixture.bridge.execute(&caller, STOP, &json!({"id":call.hex()})).await);
    assert_eq!(stopped["status"], "uncertain");
    assert_eq!(stopped["result"], json!({"status":"uncertain","reason":"browser_tree_survived"}));
    assert!(fixture.store.artifacts(EMPLOYEE, call).is_err());
}

struct Sink(Mutex<Vec<RecordingAuditEvent>>);
impl RecordingAudit for Sink { fn record(&self, event: RecordingAuditEvent) { self.0.lock().unwrap().push(event); } }

/// Finding 6: the gateway sees content-free facts for the employee's start
/// and stop, a revocation-triggered stop and an invalid result, each with
/// employee, run and call.
#[tokio::test]
async fn hx6fix_recording_audit_covers_start_stop_revocation_and_invalid_result() {
    let fixture = setup(Backend::new());
    let sink = Arc::new(Sink(Mutex::new(Vec::new())));
    let bridge = fixture.bridge.with_recording_audit(sink.clone());
    let arguments = json!({"url":"https://example.com","headless":true});
    let begin = |context: RequestContext| { let bridge = &bridge; let arguments = &arguments; async move {
        let started = body(&bridge.execute(&context, START, arguments).await);
        crate::recording_jobs::parse_id(started["id"].as_str().unwrap()).unwrap()
    } };
    let caller = context(CALLER, live());
    let first = begin(caller.clone()).await;
    assert_eq!(body(&bridge.execute(&caller, STOP, &json!({"id":first.hex()})).await)["status"], "complete");
    // Revocation: the collector's live check fails and it sends HelperStop.
    let flag = live();
    let second = begin(context(CALLER, flag.clone())).await;
    flag.store(false, Ordering::SeqCst);
    terminal(&fixture.store, second).await;
    // Exit 0 with bytes that do not decode: never stored as a recording.
    fixture.backend.invalid.store(true, Ordering::SeqCst);
    let third = begin(caller.clone()).await;
    assert_eq!(body(&bridge.execute(&caller, STOP, &json!({"id":third.hex()})).await)["result"]["status"], "invalid_result");
    let events = sink.0.lock().unwrap().clone();
    let kinds: Vec<_> = events.iter().map(|event| (event.kind, event.call)).collect();
    for expected in [(RecordingAuditKind::Started, first), (RecordingAuditKind::StopRequested, first),
        (RecordingAuditKind::Started, second), (RecordingAuditKind::RevokedStop, second),
        (RecordingAuditKind::Started, third), (RecordingAuditKind::StopRequested, third), (RecordingAuditKind::InvalidResult, third)]
        .map(|(kind, call)| (kind, Some(call))) {
        assert!(kinds.contains(&expected), "{expected:?} in {kinds:?}");
    }
    for event in &events {
        assert_eq!((event.employee, event.run.0), (EMPLOYEE, CALLER));
        let fields = event.fields();
        assert!(fields.get("url").is_none() && fields.get("result").is_none(), "content-free: {fields}");
    }
    let stop = events.iter().find(|event| event.kind == RecordingAuditKind::StopRequested).unwrap();
    assert_eq!((stop.detail["stop_by"].as_str(), stop.detail["outcome"].as_str()), (Some("stop"), Some("sent")));
    let revoked = events.iter().find(|event| event.kind == RecordingAuditKind::RevokedStop).unwrap();
    assert_eq!(revoked.detail["stop_by"], "revoked");
}

/// Finding 4: spawnd refuses a recording-slot call itself (probe not passed),
/// even when the gateway's view still advertised the bit; no row is written.
#[tokio::test]
async fn hx6fix_root_recording_gate_refusal_is_named() {
    let mut backend = Backend::new();
    backend.root_gate_closed = true;
    let fixture = setup(backend);
    let result = fixture.bridge.execute(&context(CALLER, live()), START, &json!({"url":"https://example.com","headless":true})).await;
    assert_eq!((result["isError"].as_bool(), result["helper_status"].as_str()), (Some(true), Some("helper_recording_unsupported")));
    assert!(fixture.store.admin_read(HelperCallId::new([1; 16]).unwrap()).is_err(), "no job row");
}

/// HX6 round 3 (audit gap 6): terminal outcomes stored only as an
/// `uncertain` reason (browser tree survived, exit 78, timeout, OOM) reach
/// the recording audit sink with the reason, outcome, employee, run and call.
#[tokio::test]
async fn hx6r3_recording_terminal_failures_are_audited() {
    let fixture = setup(Backend::new());
    let sink = Arc::new(Sink(Mutex::new(Vec::new())));
    let bridge = fixture.bridge.with_recording_audit(sink.clone());
    let caller = context(CALLER, live());
    let arguments = json!({"url":"https://example.com","headless":true});
    let mut expected = Vec::new();
    for (outcome, reason) in [(HelperOutcome::Exited(75), "browser_tree_survived"), (HelperOutcome::Exited(78), "helper_recording_unsupported"),
        (HelperOutcome::TimedOut, "helper_timed_out"), (HelperOutcome::OomKilled, "helper_oom_killed")] {
        let started = body(&bridge.execute(&caller, START, &arguments).await);
        let call = crate::recording_jobs::parse_id(started["id"].as_str().unwrap()).unwrap();
        fixture.backend.end(call, (vec![], outcome));
        let row = terminal(&fixture.store, call).await;
        assert_eq!(row.result.unwrap()["reason"], reason);
        expected.push((call, outcome, reason));
    }
    let events = sink.0.lock().unwrap().clone();
    for (call, outcome, reason) in expected {
        let event = events.iter().find(|event| event.kind == RecordingAuditKind::Failed && event.call == Some(call))
            .unwrap_or_else(|| panic!("{reason} not audited: {events:?}"));
        assert_eq!((event.employee, event.run.0), (EMPLOYEE, CALLER));
        assert_eq!(event.detail["reason"], reason);
        assert_eq!(event.detail["outcome"], serde_json::to_value(outcome).unwrap());
        let fields = event.fields();
        assert!(fields.get("url").is_none() && fields.get("result").is_none(), "content-free: {fields}");
    }
    assert!(RecordingAuditKind::Failed.warning());
}

/// HX6 round 3 (audit gap 6): start refusals (spawnd's own
/// `HelperRecordingUnsupported`, the gateway's gate, an offline parent) are
/// audited with employee and run, the code, and which gate refused.
#[tokio::test]
async fn hx6r3_recording_start_refusals_are_audited() {
    let arguments = json!({"url":"https://example.com","headless":true});
    let cases: [(fn(&mut Backend), &str, Option<&str>); 3] = [
        (|backend| backend.root_gate_closed = true, "helper_recording_unsupported", Some("spawnd")),
        (|backend| backend.features = SpawndFeatures::HELPER_CALLS, "helper_recording_unsupported", Some("gateway")),
        (|backend| backend.absent = true, "helper_parent_offline", None),
    ];
    for (configure, reason, gate) in cases {
        let mut backend = Backend::new();
        configure(&mut backend);
        let fixture = setup(backend);
        let sink = Arc::new(Sink(Mutex::new(Vec::new())));
        let bridge = fixture.bridge.with_recording_audit(sink.clone());
        let result = bridge.execute(&context(CALLER, live()), START, &arguments).await;
        assert_eq!(result["helper_status"], reason);
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "{events:?}");
        let event = &events[0];
        assert_eq!((event.kind, event.call, event.employee, event.run.0), (RecordingAuditKind::StartRefused, None, EMPLOYEE, CALLER));
        assert_eq!(event.detail["reason"], reason);
        assert_eq!(event.detail.get("gate").and_then(Value::as_str), gate);
        assert_eq!(event.fields()["call"], Value::Null);
    }
}
