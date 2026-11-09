//! Trusted callers choose the operation; neither paths nor endpoints are wire data.
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use duduclaw_isolation::{bounded::List, ids::RequestId, worker::*};

static REQUIRED: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
static QUARANTINE_UNTIL: AtomicU64 = AtomicU64::new(0);
fn elapsed_millis() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_millis().min(u64::MAX as u128) as u64
}

struct Admission;
impl Admission {
    fn acquire() -> Result<Self, ParseError> {
        if elapsed_millis() < QUARANTINE_UNTIL.load(Ordering::Acquire) { return Err(ParseError::Unavailable); }
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ParseError::Unavailable)?;
        if elapsed_millis() < QUARANTINE_UNTIL.load(Ordering::Acquire) {
            BUSY.store(false, Ordering::Release);
            return Err(ParseError::Unavailable);
        }
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) { BUSY.store(false, Ordering::Release); }
}

/// A/G must call this before serving work in a protected installation. There is
/// intentionally no reset API, and missing workers never enable local parsing.
pub fn require_isolation() { REQUIRED.store(true, Ordering::SeqCst); }

pub fn required() -> bool {
    if std::env::var_os("DUDUCLAW_ISOLATION_REQUIRED").is_some() { require_isolation(); }
    REQUIRED.load(Ordering::SeqCst)
}

pub fn request(operation: ParseOperation, format: ContentFormat, bytes: u64) -> ParseRequest {
    ParseRequest {
        version: WORKER_PROTOCOL_VERSION,
        request_id: RequestId(*uuid::Uuid::new_v4().as_bytes()),
        operation,
        options: None,
        inputs: List::new(vec![InputFd { slot: 0, format, size_bytes: bytes }]).expect("one input"),
        outputs: List::new(vec![]).expect("no outputs"),
        limits: WorkerLimits {
            memory_bytes: crate::policy::MAX_MEMORY_BYTES.try_into().expect("positive memory"),
            wall_time_ms: crate::policy::MAX_WALL_TIME_MS.try_into().expect("positive timeout"),
            max_result_text_bytes: (MAX_RESULT_TEXT_BYTES as u32).try_into().expect("positive text"),
        },
    }
}

/// Blocking bounded invocation. Async callers must use their blocking pool.
/// The caller authorizes the input file before handing it here. This function
/// snapshots bytes; it is not a resource authorizer or a pathname sandbox.
pub fn invoke(request: &ParseRequest, files: Vec<File>) -> Result<ParseResponse, ParseError> {
    let _admission = Admission::acquire()?;
    invoke_admitted(request, files)
}

/// Integrator-owned launch seam. A/B-managed implementations retain their host
/// resource lease until worker teardown is proven, including after caller drop.
/// The fixed socket launcher uses service limits plus a process-local quarantine
/// on transport failure; it does not claim a synchronous cancellation receipt.
/// No local parser fallback is permitted.
pub trait WorkerLauncher: Send + Sync {
    /// Protected startup must refuse launchers whose sessions cannot recheck
    /// authority at the final transport I/O and while waiting for a response.
    fn supports_live_authority(&self) -> bool { false }

    /// Reader adapters learn the exact length while snapshotting. Reserve the
    /// supplied ceiling before returning; do not account only the initial
    /// request size (which may be zero). exchange preserves id/options and
    /// replaces the input length with the verified snapshot length.
    fn launch(&self, request: &ParseRequest, input_reservation_bytes: u64)
        -> Result<Box<dyn WorkerSession>, ParseError>;
}

pub trait WorkerSession: Send {
    fn exchange(&mut self, request: &ParseRequest, files: &[File]) -> Result<ParseResponse, ParseError>;
    /// Never fall back to an unchecked exchange for an isolated invocation.
    /// Implementors call verify after blocking waits and just before each I/O.
    fn exchange_checked(&mut self, _request: &ParseRequest, _files: &[File],
        _verify: &dyn Fn() -> Result<(), ParseError>) -> Result<ParseResponse, ParseError> {
        Err(ParseError::Unavailable)
    }
}

static LAUNCHER: std::sync::OnceLock<Box<dyn WorkerLauncher>> = std::sync::OnceLock::new();

/// A/B install this once during trusted startup, before admitting requests.
/// Reconfiguration is deliberately refused, even if the previous launch failed.
pub fn install_launcher(launcher: Box<dyn WorkerLauncher>) -> Result<(), ParseError> {
    LAUNCHER.set(launcher).map_err(|_| ParseError::Unavailable)
}

/// Trusted startup installed an explicit launcher; socket existence is not readiness.
pub fn launcher_installed() -> bool { LAUNCHER.get().is_some() }

pub struct SocketActivatedLauncher;

impl WorkerLauncher for SocketActivatedLauncher {
    fn supports_live_authority(&self) -> bool { true }

    fn launch(&self, request: &ParseRequest, _input_reservation_bytes: u64)
        -> Result<Box<dyn WorkerSession>, ParseError> {
        crate::policy::validate(request)?;
        #[cfg(target_os = "linux")]
        {
            Ok(Box::new(SocketSession {
                fd: crate::transport::connect()?,
                retire_by: elapsed_millis().saturating_add(41_000),
                received_response: false,
                deadline: std::time::Instant::now()
                    + std::time::Duration::from_millis(request.limits.wall_time_ms.get() as u64),
            }))
        }
        #[cfg(not(target_os = "linux"))]
        { Err(ParseError::Unavailable) }
    }
}

#[cfg(target_os = "linux")]
struct SocketSession {
    fd: std::os::fd::OwnedFd,
    deadline: std::time::Instant,
    retire_by: u64,
    received_response: bool,
}

#[cfg(target_os = "linux")]
impl Drop for SocketSession {
    fn drop(&mut self) {
        if !self.received_response {
            // Throttle repeated failed snapshots. Backlog time is not a unit
            // deadline, so this cooldown is never proof of worker retirement.
            // Host-wide lease accounting belongs to A/B's WorkerLauncher.
            QUARANTINE_UNTIL.fetch_max(self.retire_by, Ordering::AcqRel);
        }
    }
}

#[cfg(target_os = "linux")]
impl WorkerSession for SocketSession {
    fn exchange(&mut self, request: &ParseRequest, files: &[File]) -> Result<ParseResponse, ParseError> {
        self.exchange_checked(request, files, &|| Ok(()))
    }
    fn exchange_checked(&mut self, request: &ParseRequest, files: &[File],
        verify: &dyn Fn() -> Result<(), ParseError>) -> Result<ParseResponse, ParseError> {
        use crate::transport;
        let payload = serde_json::to_vec(request).map_err(|_| ParseError::InvalidContent)?;
        transport::send_checked(&self.fd, &payload, files, self.deadline, verify)?;
        let (payload, returned) = transport::receive_checked(&self.fd, 0, self.deadline, verify)?;
        verify()?;
        if !returned.is_empty() { return Err(ParseError::InvalidContent); }
        let response = ParseResponse::decode_payload(&payload, request).map_err(|_| ParseError::InvalidContent)?;
        self.received_response = true;
        Ok(response)
    }
}

fn launch(request: &ParseRequest) -> Result<Box<dyn WorkerSession>, ParseError> {
    match LAUNCHER.get() {
        Some(launcher) => launch_with(&**launcher, request),
        #[cfg(target_os = "linux")]
        None if required() => launch_with(&crate::retirement::RetiringLauncher, request),
        None => launch_with(&SocketActivatedLauncher, request),
    }
}

fn launch_with(launcher: &dyn WorkerLauncher, request: &ParseRequest)
    -> Result<Box<dyn WorkerSession>, ParseError>
{
    crate::policy::validate(request)?;
    launcher.launch(request, crate::policy::MAX_FILE_BYTES)
}

fn exchange(session: &mut dyn WorkerSession, request: &ParseRequest, files: Vec<File>)
    -> Result<(ParseResponse, Vec<File>), ParseError>
{
    exchange_inner(session, request, files, None)
}

fn exchange_inner(session: &mut dyn WorkerSession, request: &ParseRequest, files: Vec<File>,
    verify: Option<&dyn Fn() -> Result<(), ParseError>>) -> Result<(ParseResponse, Vec<File>), ParseError>
{
    #[cfg(target_os = "linux")]
    let files = {
        let mut files = files;
        crate::transport::validate_inputs(request, &mut files)?;
        files
    };
    let response = match verify {
        Some(verify) => { verify()?; session.exchange_checked(request, &files, verify)? },
        None => session.exchange(request, &files)?,
    };
    // A custom launcher remains a transport, not a trusted result producer.
    let encoded = crate::response::encode(&response, request)?;
    let response = ParseResponse::decode_payload(&encoded, request).map_err(|_| ParseError::InvalidContent)?;
    match response.result {
        ParseResult::Failed { code } => Err(code),
        _ => Ok((response, files)),
    }
}

fn invoke_admitted(request: &ParseRequest, files: Vec<File>) -> Result<ParseResponse, ParseError> {
    if !request.outputs.as_slice().is_empty() { return Err(ParseError::UnsupportedFormat); }
    let mut session = launch(request)?;
    exchange(&mut *session, request, files).map(|(response, _)| response)
}

/// Snapshot after launch admission; no parser or caller pathname is involved.
/// The reader must be already authorized and bounded/nonblocking (e.g. a regular
/// file or byte slice). A/D own resource authorization, not this transport.
pub(crate) fn invoke_reader(request: &ParseRequest, reader: &mut impl std::io::Read)
    -> Result<ParseResponse, ParseError>
{
    if !request.outputs.as_slice().is_empty() { return Err(ParseError::UnsupportedFormat); }
    invoke_output_reader(request, reader).map(|(response, _)| response)
}

pub fn invoke_output_reader(request: &ParseRequest, reader: &mut impl std::io::Read)
    -> Result<(ParseResponse, Vec<Vec<u8>>), ParseError>
{
    invoke_output_reader_inner(request, reader, |_| Ok(()), false)
}

/// The gateway supplies its retained live authority, including for a job queued
/// on a blocking pool. Never rely on a check made before launch admission or
/// before snapshotting: either stage can wait while the caller is revoked.
pub fn invoke_output_reader_checked<R: std::io::Read>(request: &ParseRequest, reader: &mut R,
    verify: impl Fn(&R) -> Result<(), ParseError>) -> Result<(ParseResponse, Vec<Vec<u8>>), ParseError>
{
    invoke_output_reader_inner(request, reader, verify, true)
}

fn invoke_output_reader_inner<R: std::io::Read>(request: &ParseRequest, reader: &mut R,
    verify: impl Fn(&R) -> Result<(), ParseError>, guarded: bool) -> Result<(ParseResponse, Vec<Vec<u8>>), ParseError>
{
    verify(reader)?;
    if guarded && !LAUNCHER.get().map_or_else(|| SocketActivatedLauncher.supports_live_authority(),
        |launcher| launcher.supports_live_authority()) { return Err(ParseError::Unavailable); }
    let _admission = Admission::acquire()?;
    verify(reader)?;
    let session = launch(request)?;
    verify(reader)?;
    #[cfg(target_os = "linux")]
    {
        let mut session = session;
        use crate::transport;
        if request.inputs.as_slice().len() != 1 { return Err(ParseError::InvalidContent); }
        let snapshot = transport::snapshot(&mut *reader)?;
        let mut request = request.clone();
        let mut input = request.inputs.as_slice()[0].clone();
        input.size_bytes = snapshot.metadata().map_err(|_| ParseError::InvalidContent)?.len();
        request.inputs = List::new(vec![input]).map_err(|_| ParseError::InvalidContent)?;
        let mut files = vec![snapshot];
        for _ in request.outputs.as_slice() { files.push(transport::output_file()?); }
        verify(reader)?;
        let live = || verify(&*reader);
        let verifier: Option<&dyn Fn() -> Result<(), ParseError>> = if guarded { Some(&live) } else { None };
        let (response, files) = exchange_inner(&mut *session, &request, files, verifier)?;
        // The worker closes all writable fds before sending its response. Sealing
        // fails closed if any writer retained a writable mapping. From this point
        // onward the checksum and bytes cannot change under the gateway.
        let outputs = transport::verified_outputs(&request, &response, files)?;
        verify(reader)?;
        Ok((response, outputs))
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (reader, session); Err(ParseError::Unavailable) }
}

pub fn invoke_bytes_with_output(request: &ParseRequest, bytes: &[u8])
    -> Result<(ParseResponse, Vec<Vec<u8>>), ParseError>
{
    if bytes.len() as u64 > crate::policy::MAX_FILE_BYTES { return Err(ParseError::LimitExceeded); }
    invoke_output_reader(request, &mut std::io::Cursor::new(bytes))
}

fn bytes_operation(bytes: &[u8], operation: ParseOperation, format: ContentFormat) -> Result<ParseResponse, ParseError> {
    if bytes.len() as u64 > crate::policy::MAX_FILE_BYTES { return Err(ParseError::LimitExceeded); }
    invoke_reader(&request(operation, format, bytes.len() as u64), &mut std::io::Cursor::new(bytes))
}

fn text(response: ParseResponse) -> Result<String, ParseError> {
    let ParseResult::Complete { items } = response.result else { return Err(ParseError::InvalidContent); };
    let mut result = String::new();
    let mut previous = None;
    for item in items.as_slice() {
        let ParsedItem::Text { input_slot, page, text } = item else { return Err(ParseError::InvalidContent); };
        let key = (*input_slot, *page);
        if previous.is_some_and(|old| old != key) && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(text.as_str());
        if result.len() > MAX_RESULT_TEXT_BYTES { return Err(ParseError::LimitExceeded); }
        previous = Some(key);
    }
    Ok(result)
}

pub fn extract_bytes(bytes: &[u8], format: ContentFormat) -> Result<String, ParseError> {
    text(bytes_operation(bytes, ParseOperation::ExtractDocument, format)?)
}

/// Read an already authorized file, never a worker-chosen path. Local allocation
/// is bounded and only the worker sees the format parser.
pub fn extract_file(file: &mut File, format: ContentFormat, limits: WorkerLimits) -> Result<String, ParseError> {
    let metadata = file.metadata().map_err(|_| ParseError::InvalidContent)?;
    if !metadata.is_file() { return Err(ParseError::InvalidContent); }
    if metadata.len() > crate::policy::MAX_FILE_BYTES { return Err(ParseError::LimitExceeded); }
    let mut request = request(ParseOperation::ExtractDocument, format, metadata.len());
    request.limits = limits;
    text(invoke_reader(&request, file)?)
}

pub fn inspect_archive_bytes(bytes: &[u8]) -> Result<(u32, u64), ParseError> {
    let response = bytes_operation(bytes, ParseOperation::InspectSkillArchive, ContentFormat::Archive)?;
    if let ParseResult::Complete { items } = response.result {
        if let [ParsedItem::ArchiveSummary { entries, expanded_bytes, .. }] = items.as_slice() {
            return Ok((*entries, *expanded_bytes));
        }
    }
    Err(ParseError::InvalidContent)
}

pub fn inspect_image_bytes(bytes: &[u8]) -> Result<(u32, u32), ParseError> {
    let response = bytes_operation(bytes, ParseOperation::InspectImage, ContentFormat::Image)?;
    if let ParseResult::Complete { items } = response.result {
        if let [ParsedItem::Image { width, height, .. }] = items.as_slice() {
            return Ok((*width, *height));
        }
    }
    Err(ParseError::InvalidContent)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_are_bounded_and_fresh() {
        let a = request(ParseOperation::ExtractDocument, ContentFormat::Text, 10);
        let b = request(ParseOperation::ExtractDocument, ContentFormat::Text, 10);
        assert_ne!(a.request_id, b.request_id);
        assert!(crate::policy::validate(&a).is_ok());
        let mut huge = a;
        huge.limits.wall_time_ms = (crate::policy::MAX_WALL_TIME_MS + 1).try_into().unwrap();
        assert_eq!(crate::policy::validate(&huge), Err(ParseError::LimitExceeded));
    }
    #[test]
    fn text_reassembly_keeps_chunks_contiguous_and_pages_separate() {
        use duduclaw_isolation::bounded::Text;
        let request = request(ParseOperation::ExtractDocument, ContentFormat::Pdf, 0);
        let response = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new(vec![
                ParsedItem::Text { input_slot: 0, page: Some(1), text: Text::new("part".into()).unwrap() },
                ParsedItem::Text { input_slot: 0, page: Some(1), text: Text::new(" one".into()).unwrap() },
                ParsedItem::Text { input_slot: 0, page: Some(2), text: Text::new("two".into()).unwrap() },
            ]).unwrap() } };
        assert_eq!(text(response).unwrap(), "part one\ntwo");
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn required_worker_never_runs_locally_on_other_platforms() {
        assert_eq!(extract_bytes(b"hello", ContentFormat::Text), Err(ParseError::Unavailable));
    }
    #[test]
    fn unknown_reader_size_reserves_full_ceiling_before_snapshot() {
        struct Recorder(std::sync::Arc<AtomicU64>);
        impl WorkerLauncher for Recorder {
            fn launch(&self, request: &ParseRequest, reserved: u64)
                -> Result<Box<dyn WorkerSession>, ParseError> {
                assert_eq!(request.inputs.as_slice()[0].size_bytes, 0);
                self.0.store(reserved, Ordering::SeqCst);
                Err(ParseError::Unavailable)
            }
        }
        let recorded = std::sync::Arc::new(AtomicU64::new(0));
        let request = request(ParseOperation::ExtractDocument, ContentFormat::Text, 0);
        assert!(launch_with(&Recorder(recorded.clone()), &request).is_err());
        assert_eq!(recorded.load(Ordering::SeqCst), crate::policy::MAX_FILE_BYTES);
    }

    #[test]
    fn checked_session_never_falls_back_to_legacy_exchange() {
        struct Legacy;
        impl WorkerSession for Legacy {
            fn exchange(&mut self, _: &ParseRequest, _: &[File]) -> Result<ParseResponse, ParseError> {
                panic!("unchecked transport reached by protected call")
            }
        }
        let request = request(ParseOperation::ExtractDocument, ContentFormat::Text, 0);
        assert_eq!(Legacy.exchange_checked(&request, &[], &|| Ok(())), Err(ParseError::Unavailable));
    }

    #[test]
    fn revoked_reader_is_refused_before_admission_or_reading() {
        struct Unreadable;
        impl std::io::Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("revoked input was read")
            }
        }
        let request = request(ParseOperation::ExtractDocument, ContentFormat::Text, 0);
        assert_eq!(invoke_output_reader_checked(&request, &mut Unreadable,
            |_| Err(ParseError::Unavailable)), Err(ParseError::Unavailable));
    }

}
