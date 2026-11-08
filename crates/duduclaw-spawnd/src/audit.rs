//! Event-only logging. There is intentionally no free-form message field.
use std::collections::BTreeMap;
use duduclaw_isolation::ids::{EmployeeId, RunId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event { Refused, Reserved, Queued, Started, Ended, Revoked, Intruder, OutputTruncated, Suppressed,
    /// H4: a sealed work-file snapshot was handed to the gateway.
    WorkFileOpened,
    /// H4: an `OpenWorkFile` was refused (detail: the wire code).
    WorkFileRefused,
    /// H4: `StopEmployeeRuns` replied (detail: runs + confirmed).
    EmployeeRunsStopped,
    /// H4: start-up anchor backfill created a missing work anchor.
    WorkAnchorBackfilled,
    /// H4: start-up anchor backfill could not create/verify an anchor.
    WorkAnchorBackfillFailed,
    /// HX4: spawnd sent `Start` for a helper call (detail: `helper`).
    HelperStarted,
    /// HX4: a helper call ended root-side (detail: `helper` with the outcome).
    HelperEnded,
    /// HX4: a `HelperCall` was refused (detail: the wire code).
    HelperRefused,
    /// HX4: the run's launcher channel or slot cgroups were lost (detail:
    /// `helper_closed` with the code and the wire/cgroup reason).
    HelperChannelClosed,
    /// HX security fix round: an accepted `HelperStop` (detail: `helper_call`).
    HelperStopRequested,
    /// HX security fix round: an accepted `HelperCancel` (detail: `helper_call`).
    HelperCancelRequested,
    /// HX6 round 3 (N2): spawnd started its recording probe in an employee's
    /// run (record's `employee`/`run`; detail: `recording_probe`).
    RecordingProbeStarted,
    /// HX6 round 3 (N2): the probe call ended (detail: `recording_probe` with
    /// the root outcome and the verdict).
    RecordingProbeVerdict,
}
/// HX6 round 3: what one probe call decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeVerdict {
    /// Exit 0, exact reply, empty stderr: the host may carry `helper_recording`.
    Passed,
    /// Exit 78/other exit, wrong output, stderr or start failure: final for this spawnd.
    Failed,
    /// Timeout, OOM or signal: retried later in another employee's run.
    Inconclusive,
    /// The last allowed inconclusive attempt: final failure for this spawnd.
    Exhausted,
    /// The run ended under the probe: not counted, another run may try.
    RunEnded,
}
/// Why a helper call ended root-side (HX security fix round, finding 7c/d).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperEndReason {
    /// The launcher reported the helper's exit status or signal.
    LauncherExit,
    /// The launcher's own timerfd fired and it killed the process group.
    LauncherTimeout,
    /// spawnd's watchdog fired (effective timeout + grace) and killed the slot as root.
    WatchdogKill,
    /// `HelperCancel` (or an orphaned start) killed the slot.
    Cancelled,
    /// The control channel closed while the call was live.
    ChannelLost,
    RunEnded,
    /// The launcher refused or failed the start (`End::StartFailed` reason).
    StartFailedSlotBusy,
    StartFailedDraining,
    StartFailedDescriptors,
    StartFailedSandbox,
    StartFailedExec,
    StartFailedInternal,
}
/// Why the launcher control channel closed (finding 7e). Wire variants mirror
/// `duduclaw_run::helper_wire::WireError`; no frame content is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperCloseReason {
    /// EOF: the launcher died or closed its end.
    Eof,
    /// `recvmsg` failed.
    Receive,
    /// `sendmsg` of a spawnd frame failed.
    Send,
    Truncated,
    ControlTruncated,
    Ancillary,
    Descriptors,
    /// Bad length, magic, kind or field.
    Malformed,
    UnknownCall,
    IllegalState,
    Rate,
    /// The launcher sent a fatal `Error` frame.
    LauncherFatal,
    /// The channel state was already closed.
    Closed,
    /// Too many abandoned calls awaiting late frames.
    LateFrames,
    /// A slot cgroup write/readback or drain failed.
    Cgroup,
}
/// Bounded, content-free detail. Never a path, file name, file content or
/// free-form text: identities and counts only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Detail {
    /// Identity of the employee file the snapshot was copied from (root-side
    /// statx), and the snapshot size. Never sent to the gateway.
    WorkSource { device_major: u32, device_minor: u32, inode: u64, size: u64 },
    Code { code: duduclaw_isolation::ipc::ErrorCode },
    Stopped { runs: u16, confirmed: bool },
    /// HX4 helper call: spawnd's call id, the signed helper index in the
    /// parent's list, the slot, the input memfd size and, once ended, the
    /// root-determined outcome. No argv, path, input or output content.
    /// `reason` and `drained` are set on `helper_ended` only (omitted otherwise).
    Helper { call: duduclaw_isolation::ipc::HelperCallId, helper: u8, slot: duduclaw_isolation::ipc::HelperSlot,
        input_bytes: u64, effective_timeout_ms: Option<u64>, outcome: Option<duduclaw_isolation::ipc::HelperOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<HelperEndReason>,
        /// `false`: the slot was not confirmed empty (`populated 0`) after the kill.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drained: Option<bool> },
    /// A refused helper op: the wire code and the `employee_id` the request
    /// claimed (the record's `employee` is root's record of the run's employee).
    HelperRefusal { code: duduclaw_isolation::ipc::ErrorCode, claimed_employee: Option<EmployeeId> },
    /// An accepted per-call `HelperStop`/`HelperCancel`.
    HelperCall { call: duduclaw_isolation::ipc::HelperCallId },
    /// The launcher channel closed: the code later calls get, and why.
    HelperClosed { code: duduclaw_isolation::ipc::ErrorCode, reason: HelperCloseReason },
    /// HX6 round 3 (N2): the recording probe's call id, its attempt number
    /// (counted inconclusive attempts so far + 1) and the Landlock ABI; on the
    /// verdict record also the root outcome and the verdict. No output content.
    RecordingProbe { call: duduclaw_isolation::ipc::HelperCallId, attempt: u8, landlock_abi: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<duduclaw_isolation::ipc::HelperOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict: Option<ProbeVerdict> },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub at_unix_ms: u64,
    pub event: Event,
    pub employee: Option<EmployeeId>,
    pub run: Option<RunId>,
    pub suppressed: u64,
    /// Omitted when `None`, so records without detail keep their exact shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
}
/// Fixed-source rate limiting prevents caller-chosen labels from growing state.
pub struct AuditBudget {
    windows: BTreeMap<u8, (u64, u32, u64)>,
    max_per_minute: u32,
}
impl AuditBudget {
    pub fn new(max_per_minute: u32) -> Self { Self { windows: BTreeMap::new(), max_per_minute } }
    /// Return suppression count on a new window, or None when throttled.
    pub fn admit(&mut self, source: u8, now: u64) -> Option<u64> {
        let state = self.windows.entry(source).or_insert((now, 0, 0));
        if now.saturating_sub(state.0) >= 60_000 {
            let suppressed = state.2;
            *state = (now, 1, 0);
            return Some(suppressed);
        }
        if state.1 >= self.max_per_minute { state.2 = state.2.saturating_add(1); return None; }
        state.1 += 1;
        Some(0)
    }
}

/// Bounded relay accounting. After the total cap, continue draining upstream
/// while discarding bytes; never block the root daemon on a slow gateway.
pub struct OutputBudget { remaining: u64, buffer_cap: usize, truncated: bool }
impl OutputBudget {
    pub fn new(total: u64, buffer_cap: usize) -> Option<Self> {
        if total == 0 || buffer_cap == 0 || buffer_cap > 1024 * 1024 { return None; }
        Some(Self { remaining: total, buffer_cap, truncated: false })
    }
    pub fn allowance(&mut self, bytes: usize, buffered: usize) -> usize {
        let allowed = bytes.min(self.remaining.min(usize::MAX as u64) as usize)
            .min(self.buffer_cap.saturating_sub(buffered));
        self.remaining -= allowed as u64;
        self.truncated |= allowed < bytes;
        allowed
    }
    pub fn truncated(&self) -> bool { self.truncated }
}

#[cfg(any(target_os = "linux", test))]
const FILE_BYTES: u64 = 50 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const ROTATE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
#[cfg(any(target_os = "linux", test))]
const RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;
#[cfg(any(target_os = "linux", test))]
const QUEUE_LENGTH: usize = 128;
#[cfg(any(target_os = "linux", test))]
const LINE_BYTES: usize = 1024;

#[cfg(any(target_os = "linux", test))]
fn expired(first_record_at: u64, now: u64) -> bool {
    now.saturating_sub(first_record_at) >= RETENTION_MS
}
#[cfg(any(target_os = "linux", test))]
fn rotate_due(first_record_at: Option<u64>, bytes: u64, append_bytes: usize, now: u64) -> bool {
    bytes >= FILE_BYTES || bytes.saturating_add(append_bytes as u64) > FILE_BYTES
        || first_record_at.is_some_and(|first| now.saturating_sub(first) >= ROTATE_MS)
}

#[cfg(any(target_os = "linux", test))]
fn first_record_time(reader: impl std::io::Read) -> std::io::Result<Option<u64>> {
    use std::io::{BufRead, BufReader};
    let mut line = Vec::new();
    BufReader::new(std::io::Read::take(reader, (LINE_BYTES + 1) as u64)).read_until(b'\n', &mut line)?;
    if line.is_empty() { return Ok(None); }
    if line.len() > LINE_BYTES || line.last() != Some(&b'\n') { return Err(std::io::ErrorKind::InvalidData.into()); }
    let first: Record = serde_json::from_slice(&line).map_err(|_| std::io::ErrorKind::InvalidData)?;
    Ok(Some(first.at_unix_ms))
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone)]
struct AuditQueue {
    sender: std::sync::mpsc::SyncSender<(u8, Record)>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
#[cfg(any(target_os = "linux", test))]
impl AuditQueue {
    fn emit(&self, source: u8, record: Record) -> bool {
        if self.sender.try_send((source, record)).is_ok() { return true; }
        let _ = self.dropped.fetch_update(std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed, |count| Some(count.saturating_add(1)));
        false
    }
}
#[cfg(any(target_os = "linux", test))]
fn audit_channel() -> (AuditQueue, std::sync::mpsc::Receiver<(u8, Record)>) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(QUEUE_LENGTH);
    (AuditQueue { sender, dropped: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)) }, receiver)
}

#[cfg(target_os = "linux")]
pub mod disk {
    use super::*;
    use std::{fs::{self, File, OpenOptions}, io::{self, Write}, os::unix::fs::{MetadataExt, OpenOptionsExt}, path::{Path, PathBuf}, sync::{atomic::Ordering, mpsc::RecvTimeoutError}, time::{Duration, SystemTime, UNIX_EPOCH}};
    use fs2::FileExt;
    use crate::registry::root_path;
    const DIRECTORY: &str = "/var/log/duduclaw-spawnd";
    const SLOTS: usize = 4;

    /// The server only calls try_send. Disk, lock and fsync latency never delay
    /// its two-second intrusion scan; dropping this handle never joins a worker.
    #[derive(Clone)]
    pub struct AuditWorker { queue: AuditQueue }
    impl AuditWorker {
        pub fn start() -> io::Result<Self> {
            checked_directory()?;
            let (queue, receiver) = audit_channel();
            let dropped = queue.dropped.clone();
            std::thread::Builder::new().name("spawnd-audit".into()).spawn(move || {
                let mut log = AuditLog::default();
                // Failure remains local to this sink; never forward raw errors,
                // payloads, or command output to another logger.
                let _ = log.sweep(now_ms());
                loop {
                    match receiver.recv_timeout(Duration::from_secs(60)) {
                        Ok((source, mut record)) => {
                            record.suppressed = record.suppressed.saturating_add(dropped.swap(0, Ordering::Relaxed));
                            let _ = log.write(source, record);
                        }
                        Err(RecvTimeoutError::Timeout) => { let _ = log.sweep(now_ms()); }
                        Err(RecvTimeoutError::Disconnected) => {
                            let _ = log.sweep(now_ms());
                            break;
                        }
                    }
                }
            })?;
            Ok(Self { queue })
        }
        /// False means a full/disconnected queue. The next admitted event reports
        /// the dropped count; callers must not retry synchronously or block.
        pub fn emit(&self, source: u8, record: Record) -> bool { self.queue.emit(source, record) }
    }

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64
    }
    fn checked_directory() -> io::Result<&'static Path> {
        let directory = Path::new(DIRECTORY);
        root_path(directory, true)?;
        if fs::metadata(directory)?.mode() & 0o077 != 0 { return Err(io::ErrorKind::PermissionDenied.into()); }
        Ok(directory)
    }

    pub struct AuditLog { budget: AuditBudget, pending_suppressed: u64 }
    impl Default for AuditLog { fn default() -> Self { Self { budget: AuditBudget::new(60), pending_suppressed: 0 } } }
    impl AuditLog {
        /// Called only by the dedicated worker. No free-form diagnostics or
        /// secret-bearing values can enter this schema.
        pub fn write(&mut self, source: u8, mut record: Record) -> io::Result<()> {
            self.pending_suppressed = self.pending_suppressed.saturating_add(record.suppressed);
            let Some(suppressed) = self.budget.admit(source, record.at_unix_ms) else { return Ok(()); };
            self.pending_suppressed = self.pending_suppressed.saturating_add(suppressed);
            record.suppressed = self.pending_suppressed;
            let mut bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
            if bytes.len() >= LINE_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
            bytes.push(b'\n');
            let directory = checked_directory()?;
            let now = now_ms();
            let result = with_file_lock(directory, || {
                sweep_locked(directory, now)?;
                let path = log_path(directory, 0);
                let existing = log_info(&path)?;
                let (first, length) = match existing { Some(info) => info, None => (None, 0) };
                if rotate_due(first, length, bytes.len(), now) {
                    rotate_locked(directory)?;
                }
                let mut file = OpenOptions::new().append(true).create(true).mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(&path)?;
                check_file(&file)?;
                file.write_all(&bytes)?;
                file.sync_data()?;
                File::open(directory)?.sync_all()
            });
            if result.is_ok() { self.pending_suppressed = 0; }
            result
        }
        /// Runs also on recv_timeout, so an idle daemon expires its oldest logs.
        pub fn sweep(&mut self, now: u64) -> io::Result<()> {
            let directory = checked_directory()?;
            with_file_lock(directory, || {
                sweep_locked(directory, now)?;
                File::open(directory)?.sync_all()
            })
        }
    }
    fn check_file(file: &File) -> io::Result<()> {
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != 0 || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    /// Timestamp comes from the first bounded JSON line, never mtime: appending
    /// fresh events must not indefinitely retain old events in the active slot.
    fn log_info(path: &Path) -> io::Result<Option<(Option<u64>, u64)>> {
        match fs::symlink_metadata(path) {
            Ok(_) => root_path(path, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let file = OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path)?;
        check_file(&file)?;
        let length = file.metadata()?.len();
        if length == 0 { return Ok(Some((None, 0))); }
        Ok(Some((first_record_time(file)?, length)))
    }
    fn sweep_locked(directory: &Path, now: u64) -> io::Result<()> {
        for slot in 0..SLOTS {
            let path = log_path(directory, slot);
            if let Some((first, bytes)) = log_info(&path)? {
                if first.is_some_and(|at| expired(at, now)) || bytes > FILE_BYTES {
                    fs::remove_file(path)?;
                }
            }
        }
        if let Some((first, bytes)) = log_info(&log_path(directory, 0))? {
            if rotate_due(first, bytes, 0, now) { rotate_locked(directory)?; }
        }
        Ok(())
    }
    fn rotate_locked(directory: &Path) -> io::Result<()> {
        remove_if_present(&log_path(directory, SLOTS - 1))?;
        for slot in (0..SLOTS - 1).rev() {
            match fs::rename(log_path(directory, slot), log_path(directory, slot + 1)) {
                Ok(()) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn remove_if_present(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn log_path(directory: &Path, slot: usize) -> PathBuf { directory.join(format!("events.{slot}.jsonl")) }
    fn with_file_lock<T>(directory: &Path, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let path = directory.join("audit.lock");
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(&path)?;
        root_path(&path, false)?;
        check_file(&lock)?;
        lock.try_lock_exclusive()?;
        action()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn age_rotation_and_retention_use_first_record_time() {
        let first = 1_000;
        assert!(!rotate_due(Some(first), 100, 20, first + ROTATE_MS - 1));
        assert!(rotate_due(Some(first), 100, 20, first + ROTATE_MS));
        // The active file's current size/latest append cannot refresh its age.
        assert!(rotate_due(Some(first), 1, 1, first + ROTATE_MS + 1));
        assert!(!expired(first, first + RETENTION_MS - 1));
        assert!(expired(first, first + RETENTION_MS));
        assert!(!rotate_due(None, FILE_BYTES - 20, 20, 0));
        assert!(rotate_due(None, FILE_BYTES - 20, 21, 0));
        assert!(rotate_due(None, FILE_BYTES, 0, 0));
        assert!(!rotate_due(None, 0, 20, u64::MAX));
        assert!(!expired(u64::MAX, 1));
    }
    #[test]
    fn retention_reads_only_one_bounded_complete_record() {
        let record = Record { at_unix_ms: 7, event: Event::Reserved, employee: None, run: None, suppressed: 0, detail: None };
        let mut line = serde_json::to_vec(&record).unwrap();
        line.push(b'\n');
        line.extend_from_slice(&vec![b'x'; LINE_BYTES * 2]);
        assert_eq!(first_record_time(line.as_slice()).unwrap(), Some(7));
        assert_eq!(first_record_time(&b""[..]).unwrap(), None);
        assert!(first_record_time(&b"{}"[..]).is_err());
        assert!(first_record_time(vec![b'x'; LINE_BYTES + 1].as_slice()).is_err());
    }
    #[test]
    fn full_audit_queue_never_waits_for_disk_worker() {
        fn record() -> Record {
            Record { at_unix_ms: 0, event: Event::Refused, employee: None, run: None, suppressed: 0, detail: None }
        }
        let (queue, receiver) = audit_channel();
        for _ in 0..QUEUE_LENGTH { assert!(queue.emit(1, record())); }
        let other = queue.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || { done_tx.send(other.emit(1, record())).unwrap(); });
        // No receiver drains the full audit channel; a blocking send would fail
        // this deadline even though the independent completion channel is empty.
        assert!(!done_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap());
        assert_eq!(queue.dropped.load(std::sync::atomic::Ordering::Relaxed), 1);
        drop(receiver);
        assert!(!queue.emit(1, record()));
    }
    #[test]
    fn bounded_output_and_event_rate() {
        let mut output = OutputBudget::new(6, 4).unwrap();
        assert_eq!(output.allowance(5, 1), 3);
        assert_eq!(output.allowance(5, 0), 3);
        assert_eq!(output.allowance(1, 0), 0);
        assert!(output.truncated());
        let mut audit = AuditBudget::new(1);
        assert_eq!(audit.admit(1, 0), Some(0));
        assert_eq!(audit.admit(1, 1), None);
        assert_eq!(audit.admit(1, 60_000), Some(1));
        let record = Record { at_unix_ms: 0, event: Event::Refused, employee: None, run: None, suppressed: 0, detail: None };
        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
    #[test]
    fn h4_work_events_carry_identity_never_paths() {
        let source = Detail::WorkSource { device_major: 7, device_minor: 4, inode: 99, size: 12 };
        let record = Record { at_unix_ms: 1, event: Event::WorkFileOpened, employee: Some(EmployeeId([1; 16])),
            run: Some(RunId([2; 16])), suppressed: 0, detail: Some(source.clone()) };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["event"], "work_file_opened");
        assert_eq!(value["detail"]["kind"], "work_source");
        let keys: Vec<_> = value["detail"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), 5, "{keys:?}");
        assert!(keys.iter().all(|key| !matches!(key.as_str(), "path" | "name" | "bytes" | "content")));
        let line = serde_json::to_vec(&record).unwrap();
        assert!(line.len() < LINE_BYTES);
        let back: Record = serde_json::from_slice(&line).unwrap();
        assert_eq!(back.detail, Some(source));
        // A detail with an unknown field (e.g. a smuggled path) never parses.
        let mut forged = value.clone();
        forged["detail"]["path"] = "secret.txt".into();
        assert!(serde_json::from_value::<Record>(forged).is_err());
        let stopped = Record { at_unix_ms: 1, event: Event::EmployeeRunsStopped, employee: None, run: None, suppressed: 0,
            detail: Some(Detail::Stopped { runs: 3, confirmed: false }) };
        assert_eq!(serde_json::to_value(stopped).unwrap()["detail"]["confirmed"], false);
    }
    #[test]
    fn hx4_helper_events_carry_ids_sizes_and_outcome_never_content() {
        use duduclaw_isolation::ipc::{HelperCallId, HelperOutcome, HelperSlot};
        let detail = Detail::Helper { call: HelperCallId::new([7; 16]).unwrap(), helper: 2, slot: HelperSlot::Recording,
            input_bytes: 12, effective_timeout_ms: None, outcome: Some(HelperOutcome::OomKilled), reason: None, drained: None };
        let record = Record { at_unix_ms: 1, event: Event::HelperEnded, employee: Some(EmployeeId([1; 16])),
            run: Some(RunId([2; 16])), suppressed: 0, detail: Some(detail.clone()) };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["event"], "helper_ended");
        assert_eq!(value["detail"]["kind"], "helper");
        assert_eq!(value["detail"]["outcome"]["kind"], "oom_killed");
        let keys: Vec<_> = value["detail"].as_object().unwrap().keys().cloned().collect();
        assert!(keys.iter().all(|key| !matches!(key.as_str(), "path" | "argv" | "env" | "name" | "content" | "output")), "{keys:?}");
        let line = serde_json::to_vec(&record).unwrap();
        assert!(line.len() < LINE_BYTES);
        assert_eq!(serde_json::from_slice::<Record>(&line).unwrap().detail, Some(detail));
        let mut forged = value;
        forged["detail"]["argv"] = "rm -rf".into();
        assert!(serde_json::from_value::<Record>(forged).is_err());
        for event in [Event::HelperStarted, Event::HelperRefused, Event::HelperChannelClosed] {
            assert!(serde_json::to_value(event).unwrap().as_str().unwrap().starts_with("helper_"));
        }
    }
    #[test]
    fn hxfix_helper_audit_reasons_claims_and_close_cause() {
        use duduclaw_isolation::ipc::{ErrorCode, HelperCallId, HelperOutcome, HelperSlot};
        let call = HelperCallId::new([7; 16]).unwrap();
        // (c, d) watchdog vs launcher timeout, start-failure cause, undrained slot.
        let ended = Detail::Helper { call, helper: 1, slot: HelperSlot::Foreground, input_bytes: 3, effective_timeout_ms: None,
            outcome: Some(HelperOutcome::StartFailed), reason: Some(HelperEndReason::StartFailedSandbox), drained: Some(false) };
        let value = serde_json::to_value(&ended).unwrap();
        assert_eq!(value["reason"], "start_failed_sandbox");
        assert_eq!(value["drained"], false);
        assert_ne!(serde_json::to_value(HelperEndReason::WatchdogKill).unwrap(), serde_json::to_value(HelperEndReason::LauncherTimeout).unwrap());
        // Started records keep their HX4 shape (no reason/drained keys).
        let started = Detail::Helper { call, helper: 1, slot: HelperSlot::Foreground, input_bytes: 3,
            effective_timeout_ms: Some(10), outcome: None, reason: None, drained: None };
        let keys = serde_json::to_value(&started).unwrap().as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert!(!keys.iter().any(|key| key == "reason" || key == "drained"), "{keys:?}");
        // (a) the claim is detail; (e) close reason; (f) per-call request.
        let records = [(Event::HelperRefused, Detail::HelperRefusal { code: ErrorCode::Unauthorized, claimed_employee: Some(EmployeeId([2; 16])) }),
            (Event::HelperChannelClosed, Detail::HelperClosed { code: ErrorCode::HelperUnavailable, reason: HelperCloseReason::Rate }),
            (Event::HelperStopRequested, Detail::HelperCall { call }), (Event::HelperCancelRequested, Detail::HelperCall { call }),
            (Event::HelperEnded, ended)];
        for (event, detail) in records {
            let record = Record { at_unix_ms: 1, event, employee: Some(EmployeeId([1; 16])), run: Some(RunId([2; 16])),
                suppressed: 0, detail: Some(detail.clone()) };
            let line = serde_json::to_vec(&record).unwrap();
            assert!(line.len() < LINE_BYTES);
            assert_eq!(serde_json::from_slice::<Record>(&line).unwrap().detail, Some(detail));
        }
        assert_eq!(serde_json::to_value(Event::HelperStopRequested).unwrap(), "helper_stop_requested");
    }
    /// HX6 round 3 (N2): the probe's start and verdict are bounded,
    /// content-free records with the employee and run whose slot it used.
    #[test]
    fn hx6r3_recording_probe_audit_records_are_bounded() {
        use duduclaw_isolation::ipc::{HelperCallId, HelperOutcome};
        let call = HelperCallId::new([7; 16]).unwrap();
        let started = Detail::RecordingProbe { call, attempt: 1, landlock_abi: 6, outcome: None, verdict: None };
        let keys = serde_json::to_value(&started).unwrap().as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert!(!keys.iter().any(|key| key == "outcome" || key == "verdict"), "{keys:?}");
        for (outcome, verdict) in [(HelperOutcome::Exited(0), ProbeVerdict::Passed), (HelperOutcome::Exited(78), ProbeVerdict::Failed),
            (HelperOutcome::TimedOut, ProbeVerdict::Inconclusive), (HelperOutcome::OomKilled, ProbeVerdict::Exhausted),
            (HelperOutcome::RunEnded, ProbeVerdict::RunEnded)] {
            for (event, detail) in [(Event::RecordingProbeStarted, started.clone()), (Event::RecordingProbeVerdict,
                Detail::RecordingProbe { call, attempt: 3, landlock_abi: 6, outcome: Some(outcome), verdict: Some(verdict) })] {
                let record = Record { at_unix_ms: 1, event, employee: Some(EmployeeId([1; 16])), run: Some(RunId([2; 16])),
                    suppressed: 0, detail: Some(detail.clone()) };
                let line = serde_json::to_vec(&record).unwrap();
                assert!(line.len() < LINE_BYTES);
                let back = serde_json::from_slice::<Record>(&line).unwrap();
                assert_eq!((back.detail, back.employee, back.run), (Some(detail), Some(EmployeeId([1; 16])), Some(RunId([2; 16]))));
            }
        }
        assert_eq!(serde_json::to_value(Event::RecordingProbeVerdict).unwrap(), "recording_probe_verdict");
        assert_eq!(serde_json::to_value(ProbeVerdict::Exhausted).unwrap(), "exhausted");
    }
    #[test]
    fn hxfix_helper_stop_cancel_requests_share_audit_rate_limit() {
        // Round 3: `server::send` emits every reply record, including accepted
        // HelperStop/HelperCancel, on source 0 through the same bounded queue
        // the worker drains into `AuditLog::write`, whose `AuditBudget` keys on
        // the source only. A flood of stop/cancel requests is therefore
        // throttled with the other server events and reported in `suppressed`.
        use duduclaw_isolation::ipc::HelperCallId;
        let call = HelperCallId::new([7; 16]).unwrap();
        let request = |n: u64| Record { at_unix_ms: n,
            event: if n % 2 == 0 { Event::HelperStopRequested } else { Event::HelperCancelRequested },
            employee: Some(EmployeeId([1; 16])), run: Some(RunId([2; 16])), suppressed: 0, detail: Some(Detail::HelperCall { call }) };
        let (queue, receiver) = audit_channel();
        for n in 0..QUEUE_LENGTH as u64 { assert!(queue.emit(0, request(n))); }
        // A full queue drops (counted), never blocks the server.
        assert!(!queue.emit(0, request(QUEUE_LENGTH as u64)));
        assert_eq!(queue.dropped.load(std::sync::atomic::Ordering::Relaxed), 1);
        // The worker's admission (the default `AuditLog` budget: 60/min/source).
        let mut budget = AuditBudget::new(60);
        let admitted = receiver.try_iter().filter(|(source, record)| {
            assert_eq!(*source, 0);
            budget.admit(*source, record.at_unix_ms).is_some()
        }).count();
        assert_eq!(admitted, 60, "helper stop/cancel events are rate limited like every server event");
        // The next window's first record carries the suppressed count.
        assert_eq!(budget.admit(0, 60_000), Some(QUEUE_LENGTH as u64 - 60));
    }
}
