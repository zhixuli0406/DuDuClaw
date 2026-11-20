//! R8z: (1) an OpenAI-compatible main-run plan with the published record's
//! limits reaches a valid StartRun without any local refusal; (2) each local
//! refusal branch of `spawn_inner` writes exactly one warning naming its
//! fixed step, and keeps its error value.
use super::*;
use std::os::fd::AsRawFd;
use duduclaw_isolation::{egress::ProfileId, ids::Sha256Digest, ipc::Envelope};
use nix::sys::socket::{self, MsgFlags};

const RUN: RunHandle = RunHandle(RunId([31; 16]));
const EMPLOYEE: EmployeeId = EmployeeId([32; 16]);

#[derive(Clone, Default)]
struct LogCapture(Arc<StdMutex<Vec<u8>>>);
impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
impl LogCapture {
    fn text(&self) -> String { String::from_utf8(self.0.lock().unwrap().clone()).unwrap() }
}

/// The limits of the published `open_ai_compat` record from the R8z host
/// evidence (io limits are not part of the evidence; any valid values).
fn published_resources() -> ResourceLimits {
    ResourceLimits { memory_bytes: 1_073_741_824, cpu_quota_percent: 100, tasks: 256, open_files: 4096, ptys: 1,
        disk: DiskQuota::LoopFilesystem { bytes: 1_073_741_824, inodes: 65_536 },
        io_read_bytes_per_sec: 104_857_600, io_write_bytes_per_sec: 104_857_600,
        io_read_ops_per_sec: 10_000, io_write_ops_per_sec: 10_000,
        output_buffer_bytes: 65_536, output_total_bytes: 16_777_216, memfd_total_bytes: 8_388_608 }
}

/// The plan `duduclaw-spawnd` `gateway_plan::other_runtime_plan` builds for
/// `RuntimeKind::OpenAiCompat` (field for field: no workspace delivery, the
/// plain `employee-work` root, the record's resources/network/timeouts, the
/// sealed request as the only input, no PTY, MCP bridge, and no step-limit
/// request), after D's `prepare` set the reserved run.
fn api_plan(request: &[u8]) -> SpawnPolicy {
    SpawnPolicy { workspace_delivery: None, run: RUN, employee: EMPLOYEE, class: ExecutionClass::EmployeeMain,
        runtime: RuntimeKind::OpenAiCompat, argv_allowlist: vec![], environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).unwrap(),
            relative_cwd: Text::new(String::new()).unwrap() },
        resources: published_resources(), network: NetworkPolicy::Egress(ProfileId(Sha256Digest([7; 32]))), pty: false,
        capabilities: CapabilityHandover { mcp_bridge: true, egress_forwarder: true },
        timeouts: RunTimeouts { queue_ms: 60_000, startup_ms: 30_000, runtime_ms: 600_000, pty_idle_ms: None,
            termination_grace_ms: 10_000 },
        inputs: RunInputs { prompt: tests::input(request, request.len() as u64), attachments: vec![] },
        requested_max_turns: None }
}

async fn backend(admission: Option<fn() -> Result<(), BackendError>>) -> (SpawndBackend, Arc<Entry>, OwnedFd) {
    let slots = Arc::new(Semaphore::new(1));
    let (connection, peer) = Connection::test_pair();
    let entry = Arc::new(Entry { employee: EMPLOYEE, reservation_expiry: 0, features: ipc::SpawndFeatures::NONE,
        connection: Arc::new(connection), _permit: slots.clone().acquire_owned().await.unwrap(),
        state: Mutex::new(State { expires: Instant::now() + Duration::from_secs(30), started: false, task_ref: None,
            pty: false, mcp: false, output_bytes: 0, output_buffer: 0, io_received: false, stdio: None,
            terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None, workspace: None }) });
    let backend = SpawndBackend { runs: StdMutex::new(BTreeMap::from([(RUN.0, entry.clone())])), slots, admission,
        workspace_bridge: None, features: None };
    (backend, entry, peer)
}

fn subscriber(writer: LogCapture) -> impl tracing::Subscriber + Send + Sync + 'static {
    tracing_subscriber::fmt().without_time().with_ansi(false).with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone()).finish()
}

#[tokio::test]
async fn openai_compat_plan_from_the_published_record_reaches_start_run() {
    let request = br#"{"protocol":"openai-compat-v1","model":"acceptance-model"}"#;
    let plan = api_plan(request);
    // Plan level: every local precondition of StartRun holds.
    assert!(plan.workspace_delivery.is_none());
    assert_eq!(validate_workspace(&plan.workspace), Ok(()));
    assert!(plan.resources.output_total_bytes > 0 && plan.resources.output_buffer_bytes > 0
        && plan.resources.memfd_total_bytes > 0);
    assert!(request.len() as u64 <= plan.resources.memfd_total_bytes.min(MAX_MEMFD_TOTAL_BYTES));
    assert!(!plan.pty && plan.inputs.attachments.is_empty());
    // Step-limit rule of the API path: the record has no signed range, so
    // the plan carries no request (spawnd refuses any `Some(_)` then).
    assert_eq!(plan.requested_max_turns, None);

    let (backend, _entry, peer) = backend(None).await;
    let expected_bytes = request.len() as u64;
    let root = std::thread::spawn(move || {
        let mut poll = libc::pollfd { fd: peer.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one live pollfd with its exact array length.
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
        // The sealed prompt arrives as SCM_RIGHTS; plain recv discards it.
        let size = socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT).unwrap();
        bytes.truncate(size);
        let envelope: Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
        // What root validates first: the wire contract and the closed code.
        assert!(envelope.message.validate().is_ok());
        assert_eq!(envelope.message.validation_code(), Ok(()));
        let SpawnRequest::StartRun { run, employee_id, runtime_request, workspace, prompt_bytes, input_fds, pty,
            requested_max_turns, .. } = &envelope.message else { panic!("not a StartRun") };
        assert_eq!((*run, *employee_id), (RUN, EMPLOYEE));
        assert_eq!(runtime_request, &RuntimeKind::OpenAiCompat);
        assert!(workspace.is_none() && input_fds.as_slice().is_empty() && !*pty);
        assert_eq!(*prompt_bytes, expected_bytes);
        assert_eq!(*requested_max_turns, None, "no step-limit request for a record without turn_limit");
        // The serialized frame has no `requested_max_turns` key at all.
        assert!(!String::from_utf8_lossy(&bytes).contains("requested_max_turns"));
        let reply = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: envelope.request_id,
            message: SpawnResponse::Accepted { run: RUN, status: RunStatus::Running } }).unwrap();
        socket::send(peer.as_raw_fd(), &reply, MsgFlags::MSG_NOSIGNAL).unwrap();
        peer
    });
    let log = LogCapture::default();
    let guard = tracing::subscriber::set_default(subscriber(log.clone()));
    let result = backend.spawn_inner(plan, None).await;
    drop(guard);
    let _peer = root.join().unwrap();
    assert_eq!(result, Ok(RUN));
    assert!(log.text().is_empty(), "no local refusal: {}", log.text());
}

thread_local! { static ADMISSIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
/// Admits the first check only (the current-thread test runtime runs every
/// check of one `spawn_inner` on this thread).
fn admit_first_only() -> Result<(), BackendError> {
    ADMISSIONS.with(|count| {
        let seen = count.get();
        count.set(seen + 1);
        if seen == 0 { Ok(()) } else { Err(BackendError::Unavailable) }
    })
}
fn refuse_admission() -> Result<(), BackendError> { Err(BackendError::Unavailable) }

#[derive(Clone, Copy)]
enum Case {
    Admission, AdmissionBeforeSend, WorkspaceDelivery, Workspace, Entry, OtherEmployee, Started, Expired,
    OutputTotal, OutputBuffer, Memfd, PromptOverBudget, TooManyAttachments, StepLimitZero, StepLimitOverWire,
    ClosedControl,
}

/// Run one refused `spawn_inner`; returns its result and the captured log.
async fn run_case(case: Case) -> (Result<RunHandle, BackendError>, String) {
    ADMISSIONS.with(|count| count.set(0));
    type Admission = fn() -> Result<(), BackendError>;
    let admission: Option<Admission> = match case {
        Case::Admission => Some(refuse_admission as Admission),
        Case::AdmissionBeforeSend => Some(admit_first_only as Admission),
        _ => None,
    };
    let (backend, entry, _peer) = backend(admission).await;
    let mut plan = api_plan(PROMPT);
    let directory = tempfile::tempdir().unwrap();
    match case {
        Case::WorkspaceDelivery => plan.workspace_delivery = Some(WorkspaceDelivery { employee: EmployeeId([9; 16]),
            run: RUN.0, policy: plan.workspace.clone(), directory: std::fs::File::open(directory.path()).unwrap() }),
        Case::Workspace => plan.workspace.workspace = Text::new("branch-work".into()).unwrap(),
        Case::Entry => plan.run = RunHandle(RunId([99; 16])),
        Case::OtherEmployee => plan.employee = EmployeeId([9; 16]),
        Case::Started => entry.state.lock().await.started = true,
        Case::Expired => entry.state.lock().await.expires = Instant::now(),
        Case::OutputTotal => plan.resources.output_total_bytes = 0,
        Case::OutputBuffer => plan.resources.output_buffer_bytes = 0,
        Case::Memfd => plan.resources.memfd_total_bytes = 0,
        Case::PromptOverBudget => plan.resources.memfd_total_bytes = 3,
        Case::TooManyAttachments => plan.inputs.attachments = (0..=ipc::MAX_INPUT_FDS).map(|_| tests::input(b"", 0)).collect(),
        Case::StepLimitZero => plan.requested_max_turns = Some(0),
        Case::StepLimitOverWire => plan.requested_max_turns = Some(ipc::MAX_REQUESTED_MAX_TURNS + 1),
        Case::ClosedControl => entry.connection.close().await,
        Case::Admission | Case::AdmissionBeforeSend => {}
    }
    let log = LogCapture::default();
    let guard = tracing::subscriber::set_default(subscriber(log.clone()));
    let result = backend.spawn_inner(plan, None).await;
    drop(guard);
    (result, log.text())
}

/// A prompt marker that must never reach a log line (10 bytes).
const PROMPT: &[u8] = b"R8Z-PROMPT";

#[tokio::test]
async fn each_local_spawn_refusal_logs_its_fixed_step_once() {
    use local_refusal::Step;
    // (case, step, error) — the error is the one `spawn_inner` returned
    // before R8z; only the warning is new.
    let table = [
        (Case::Admission, Step::Admission, BackendError::Unavailable),
        (Case::AdmissionBeforeSend, Step::Admission, BackendError::Unavailable),
        (Case::WorkspaceDelivery, Step::WorkspaceDelivery, BackendError::Denied),
        (Case::Workspace, Step::Workspace, BackendError::Denied),
        (Case::Entry, Step::Entry, BackendError::NotFound),
        (Case::OtherEmployee, Step::ReservationState, BackendError::Denied),
        (Case::Started, Step::ReservationState, BackendError::Denied),
        (Case::Expired, Step::ReservationState, BackendError::Denied),
        (Case::OutputTotal, Step::ResourceLimits, BackendError::Denied),
        (Case::OutputBuffer, Step::ResourceLimits, BackendError::Denied),
        (Case::Memfd, Step::ResourceLimits, BackendError::Denied),
        // The 10-byte prompt exceeds a 3-byte memfd budget.
        (Case::PromptOverBudget, Step::Inputs, BackendError::Denied),
        // `prepare_inputs` refuses > MAX_INPUT_FDS before `List::new` could
        // (the `attachments` step is defence in depth behind it).
        (Case::TooManyAttachments, Step::Inputs, BackendError::Denied),
        (Case::StepLimitZero, Step::RequestInvalid, BackendError::Denied),
        (Case::StepLimitOverWire, Step::RequestInvalid, BackendError::Denied),
        (Case::ClosedControl, Step::ConnectionState, BackendError::Unavailable),
    ];
    for (case, step, error) in table {
        let (result, logs) = run_case(case).await;
        let name = step.as_str();
        assert_eq!(result, Err(error), "{name}");
        assert_eq!(logs.lines().count(), 1, "{name}: {logs}");
        assert!(logs.contains("isolated spawn refused locally") && logs.contains(local_refusal::MESSAGE), "{name}: {logs}");
        assert!(logs.contains(&format!("step=\"{name}\"")), "{name}: {logs}");
        // Fixed fields only: no prompt bytes, no run/employee id.
        assert!(!logs.contains("R8Z-PROMPT") && !logs.contains("31, 31") && !logs.contains("32, 32"), "{name}: {logs}");
    }
}
