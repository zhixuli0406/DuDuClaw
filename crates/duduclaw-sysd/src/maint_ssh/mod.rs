//! Maintenance-mode SSH with a dedicated account and a temporary key
//! (`ssh_maint_open`, `ssh_maint_status`, and the key side of
//! `ssh_service_stop`).
//!
//! sysd is the only authority for the key and for the SSH audit trail: the
//! gateway runs as the same uid as the AI employees, so nothing it says is
//! trusted beyond "please open / close". This module:
//!
//! - checks that the OS is really set up before opening (readiness marker,
//!   the `duduclaw-maint` account at uid/gid 4800, dropbear's merged
//!   `ExecStart` token for token, `BindsTo=duduclaw-sysd.service`, the nft
//!   egress rule and its jump, the port-22 source sets, the host key;
//!   [`verify`]);
//! - parses the key strictly ([`crate::ssh_key`]) and writes it to the tmpfs
//!   key directory without following symlinks and without replacing an
//!   existing key ([`fsops`]);
//! - keeps its own deadline on `CLOCK_BOOTTIME` (at most 24 h) and arms a
//!   PID 1 fallback timer in case sysd is not around to close
//!   ([`timer`]);
//! - reads dropbear and menu events from the journal by trusted fields only
//!   and writes logins and logouts into the root audit file itself, every
//!   [`TICK_INTERVAL`], without waiting for the gateway ([`journal`],
//!   [`audit`]);
//! - closes on request (R1/R2/R3), at its deadline (R4) and at startup when it
//!   finds leftovers (R5), always writing the missing events before the close
//!   record.
//!
//! Every SSH verb, the tick and the startup cleanup take [`SSH_LOCK`], so they
//! never interleave; a request that arrives while another runs waits for it.

pub mod audit;
pub mod fsops;
pub mod host;
pub mod journal;
pub mod timer;
pub mod verify;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::dispatch::DispatchResult;
use crate::protocol::{SysdError, SysdOpOutput};
use audit::AuditLog;
use fsops::{FsError, KeyDir, Ownership};
use host::{MaintHost, argv};
use journal::{EventKind, SshEvent};

/// The login account (DuDuClaw-OS recipe `duduclaw-maint`).
pub const MAINT_USER: &str = "duduclaw-maint";
/// Its fixed uid and gid (design §3.1: outside the dynamic system range, the
/// subuid range and the Android AID ranges).
pub const MAINT_UID: u32 = 4800;
pub const MAINT_GID: u32 = 4800;

/// Installed by the last OS PR only, so a partly merged OS never claims to be
/// ready. While it exists the legacy keyless `ssh_service_start` is refused.
pub const READY_MARKER: &str = "/usr/lib/duduclaw/maint-ssh.ready";

/// tmpfs key directory dropbear reads with `-D`.
pub const KEY_DIR: &str = "/run/duduclaw-maint-ssh";
pub const KEY_FILE: &str = "authorized_keys";
/// sysd's window state (root 0600) next to the key.
pub const WINDOW_FILE: &str = "window.json";
/// Audit work still owed after a close (root 0600), see [`PendingAudit`].
pub const PENDING_FILE: &str = "audit-pending.json";

/// Root audit file.
pub const AUDIT_LOG: &str = "/data/system/maint-ssh-audit.log";

/// dropbear host key (generated on `/data` because the root may be read-only).
pub const HOST_KEY: &str = "/data/system/dropbear/dropbear_ed25519_host_key";
pub const HOST_KEY_UNIT: &str = "dropbearkey.service";
/// A template instance name used only to read the merged unit.
pub const DROPBEAR_PROBE_UNIT: &str = "dropbear@probe.service";
pub const DROPBEAR_SOCKET: &str = "dropbear.socket";

/// TTL bounds in seconds (sysd's own; the gateway works in hours, 1–24).
pub const MIN_TTL_SECS: u64 = 60;
pub const MAX_TTL_SECS: u64 = 86_400;
pub const MAX_WINDOW_ID_LEN: usize = 64;

/// R4 period.
pub const TICK_INTERVAL: Duration = Duration::from_secs(15);

/// Logins (and, separately, logouts) written one by one per window; the rest
/// are counted and summarised in the close record (R2-L8).
pub const LOGIN_RECORD_CAP: u32 = 50;
/// Events kept for `ssh_maint_status`; when full, logins and logouts are kept
/// in preference and the rest only counted.
pub const STATUS_EVENT_CAP: usize = 200;
/// Journal lines handled per read; the rest wait for the next read.
pub const MAX_EVENTS_PER_READ: usize = 5000;
/// Distinct failing sources listed in the close record.
pub const AUTH_FAILURE_SOURCES: usize = 5;
/// Largest `window.json` read back.
const MAX_WINDOW_FILE_BYTES: usize = 64 * 1024;
/// Largest `audit-pending.json` read back.
const MAX_PENDING_FILE_BYTES: usize = 1024 * 1024;
/// `ssh_maint_open` rate limit: at least this long between attempts that pass
/// validation, and at most [`OPEN_MAX_PER_HOUR`] of them per rolling hour. A
/// caller looping open/close would otherwise flood the audit file.
pub const OPEN_MIN_INTERVAL: Duration = Duration::from_secs(5);
pub const OPEN_MAX_PER_HOUR: usize = 12;

/// Where things live and who must own them. [`MaintConfig::system`] is the
/// appliance; tests point everything at a temp directory.
#[derive(Debug, Clone)]
pub struct MaintConfig {
    pub ready_marker: PathBuf,
    pub key_dir: PathBuf,
    pub audit: AuditLog,
    /// `/run/duduclaw-maint-ssh`: root:maint 0750.
    pub dir_owner: Ownership,
    /// `authorized_keys`: root:maint 0640.
    pub key_owner: Ownership,
    /// `window.json`: root:root 0600.
    pub state_owner: Ownership,
    pub maint_uid: u32,
    pub maint_gid: u32,
    /// Programs the window depends on (the menu shell dropbear forces, the
    /// PID 1 fallback close script): each must be a regular, executable file
    /// owned by `exec_owner_uid` and not writable by group or others.
    pub required_executables: Vec<PathBuf>,
    pub exec_owner_uid: u32,
    pub open_min_interval: Duration,
    pub open_max_per_hour: usize,
}

impl MaintConfig {
    pub fn system() -> Self {
        Self {
            ready_marker: PathBuf::from(READY_MARKER),
            key_dir: PathBuf::from(KEY_DIR),
            audit: AuditLog::new(AUDIT_LOG, 0),
            dir_owner: Ownership { uid: 0, gid: MAINT_GID, mode: 0o750 },
            key_owner: Ownership { uid: 0, gid: MAINT_GID, mode: 0o640 },
            state_owner: Ownership { uid: 0, gid: 0, mode: 0o600 },
            maint_uid: MAINT_UID,
            maint_gid: MAINT_GID,
            required_executables: vec![PathBuf::from(MAINT_SHELL), PathBuf::from(timer::FALLBACK_SCRIPT)],
            exec_owner_uid: 0,
            open_min_interval: OPEN_MIN_INTERVAL,
            open_max_per_hour: OPEN_MAX_PER_HOUR,
        }
    }
}

/// The forced menu shell (dropbear `-c`, the account's login shell).
pub const MAINT_SHELL: &str = "/usr/libexec/duduclaw/maint-shell";

fn check_executable(path: &Path, owner_uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mode = meta.mode();
    if !meta.is_file() || meta.uid() != owner_uid || mode & 0o022 != 0 || mode & 0o111 == 0 {
        return Err(format!(
            "{} must be an executable regular file owned by uid {owner_uid} and not writable by group or others",
            path.display()
        ));
    }
    Ok(())
}

/// Persisted window state (`window.json`), also the in-memory copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowFile {
    pub format: u32,
    pub window_id: String,
    pub key_type: String,
    pub key_fingerprint: String,
    pub host_key_fingerprints: Vec<String>,
    pub ttl_seconds: u64,
    /// sysd's deadline on `CLOCK_BOOTTIME`, in milliseconds.
    pub deadline_boottime_ms: u64,
    pub opened_at_unix: u64,
    pub open_cursor: String,
    /// Last journal entry already handled (and written to the audit file).
    pub last_cursor: String,
    pub logins_recorded: u32,
    pub logins_over_cap: u32,
    pub logouts_recorded: u32,
    pub logouts_over_cap: u32,
    pub auth_failures: u64,
    pub auth_failure_sources: Vec<String>,
    /// The open record, rewritten at the top of the audit file on rotation.
    pub open_record: Value,
}

/// A connection that logged in and has not logged out yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub pid: Option<u32>,
    pub source: Option<String>,
    pub fingerprint: Option<String>,
    pub since_usec: u64,
}

#[derive(Debug, Default)]
struct EventBuffer {
    items: Vec<SshEvent>,
    overflow: u64,
}

impl EventBuffer {
    const fn new() -> Self {
        Self { items: Vec::new(), overflow: 0 }
    }

    fn push(&mut self, event: SshEvent) {
        if self.items.len() < STATUS_EVENT_CAP {
            self.items.push(event);
            return;
        }
        self.overflow += 1;
        if event.kind.is_session() {
            if let Some(i) = self.items.iter().position(|e| !e.kind.is_session()) {
                self.items.remove(i);
                self.items.push(event);
            }
        }
    }
}

/// Everything behind [`SSH_LOCK`].
#[derive(Debug, Default)]
pub struct MaintState {
    window: Option<WindowFile>,
    /// A startup cleanup that did not finish; the tick retries it.
    pending_cleanup: bool,
    events: EventBuffer,
    sessions: BTreeMap<u32, Session>,
    /// Sessions whose login line carried no pid (kept in order).
    unkeyed_sessions: Vec<Session>,
    /// Audit work not done yet (events still to replay, records not written).
    pending: PendingAudit,
    /// `CLOCK_BOOTTIME` ms of recent `ssh_maint_open` attempts (rate limit).
    open_attempts_ms: Vec<u64>,
}

/// Events of a closed window that could not be read before it closed: the
/// window state (with the last cursor already in the audit file) and the
/// close time; later events belong to someone else and are not read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backfill {
    pub window: WindowFile,
    pub until_usec: u64,
}

/// Persisted as [`PENDING_FILE`] so a sysd restart keeps retrying.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingAudit {
    pub backfill: Option<Backfill>,
    /// Records to write, in order, once `backfill` is done.
    pub records: Vec<Value>,
}

impl PendingAudit {
    const fn new() -> Self {
        Self { backfill: None, records: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.backfill.is_none() && self.records.is_empty()
    }
}

impl MaintState {
    pub const fn new() -> Self {
        Self {
            window: None,
            pending_cleanup: false,
            events: EventBuffer::new(),
            sessions: BTreeMap::new(),
            unkeyed_sessions: Vec::new(),
            pending: PendingAudit::new(),
            open_attempts_ms: Vec::new(),
        }
    }

    pub fn window(&self) -> Option<&WindowFile> {
        self.window.as_ref()
    }
}

/// Serialises every SSH verb, the R4 tick and the R5 startup cleanup.
pub static SSH_LOCK: tokio::sync::Mutex<MaintState> = tokio::sync::Mutex::const_new(MaintState::new());

/// Why a window was closed (audit `trigger`). The OS fallback script writes
/// `fallback_timer` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTrigger {
    Stop,
    Deadline,
    StartupCleanup,
}

impl CloseTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CloseTrigger::Stop => "stop",
            CloseTrigger::Deadline => "deadline",
            CloseTrigger::StartupCleanup => "startup_cleanup",
        }
    }
}

/// State of the readiness marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Absent,
    /// Present but unreadable or with unexpected content.
    Unknown(String),
}

pub fn readiness(marker: &Path) -> Readiness {
    match std::fs::symlink_metadata(marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Readiness::Absent,
        Err(e) => Readiness::Unknown(format!("cannot stat {}: {e}", marker.display())),
        Ok(meta) if !meta.is_file() => Readiness::Unknown(format!("{} is not a regular file", marker.display())),
        Ok(_) => match std::fs::read(marker) {
            Ok(content) if content == b"1\n" || content == b"1" => Readiness::Ready,
            Ok(_) => Readiness::Unknown(format!("{} has unexpected content", marker.display())),
            Err(e) => Readiness::Unknown(format!("cannot read {}: {e}", marker.display())),
        },
    }
}

/// The legacy keyless `ssh_service_start` only runs on images without the
/// readiness marker (anything else, including an unreadable marker, refuses).
pub fn legacy_start_allowed(marker: &Path) -> Result<(), SysdError> {
    match readiness(marker) {
        Readiness::Absent => Ok(()),
        Readiness::Ready => Err(SysdError::unsupported(
            "this image only opens SSH with a maintenance key; use ssh_maint_open",
        )),
        Readiness::Unknown(e) => Err(SysdError::unsupported(format!("cannot tell whether this image needs a maintenance key: {e}"))),
    }
}

fn busy(message: &str) -> SysdError {
    SysdError::new("busy", message)
}

fn fs_err(e: FsError) -> SysdError {
    SysdError::io(e.to_string())
}

/// Run a command that must succeed; returns its stdout.
async fn run_ok<H: MaintHost>(host: &H, cmd: &[String]) -> Result<String, String> {
    let out = host.run(cmd).await?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(format!(
            "`{}` failed: {}",
            cmd.join(" "),
            journal::truncate_chars(out.stderr.trim(), 300)
        ))
    }
}

pub fn validate_window_id(id: &str) -> Result<(), SysdError> {
    if id.is_empty() || id.len() > MAX_WINDOW_ID_LEN || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(SysdError::bad_request(format!(
            "window_id must be 1-{MAX_WINDOW_ID_LEN} characters of [A-Za-z0-9-]"
        )));
    }
    Ok(())
}

pub fn validate_ttl(ttl: u64) -> Result<(), SysdError> {
    if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&ttl) {
        return Err(SysdError::bad_request(format!("ttl_seconds must be {MIN_TTL_SECS}-{MAX_TTL_SECS}")));
    }
    Ok(())
}

/// §4.4: everything the window relies on, read from what is in effect.
/// Returns the host key fingerprints.
pub async fn verify_effective<H: MaintHost>(host: &H, cfg: &MaintConfig) -> Result<Vec<String>, String> {
    match readiness(&cfg.ready_marker) {
        Readiness::Ready => {}
        Readiness::Absent => return Err("the OS does not ship the maintenance SSH account yet".to_string()),
        Readiness::Unknown(e) => return Err(e),
    }
    if host.user_ids(MAINT_USER) != Some((cfg.maint_uid, cfg.maint_gid)) {
        return Err(format!("user {MAINT_USER} is not uid/gid {}/{}", cfg.maint_uid, cfg.maint_gid));
    }
    if host.group_id(MAINT_USER) != Some(cfg.maint_gid) {
        return Err(format!("group {MAINT_USER} is not gid {}", cfg.maint_gid));
    }
    for path in &cfg.required_executables {
        check_executable(path, cfg.exec_owner_uid)?;
    }

    let exec = run_ok(host, &argv(&["systemctl", "show", DROPBEAR_PROBE_UNIT, "-p", "ExecStart", "--value"])).await?;
    verify::exec_start_ok(&exec)?;
    for unit in [DROPBEAR_SOCKET, DROPBEAR_PROBE_UNIT] {
        let binds = run_ok(host, &argv(&["systemctl", "show", unit, "-p", "BindsTo", "--value"])).await?;
        if !verify::binds_to_ok(&binds, verify::SYSD_UNIT) {
            return Err(format!("{unit} is not BindsTo={}", verify::SYSD_UNIT));
        }
    }

    let nft_chain = |chain: &str| argv(&["nft", "-j", "list", "chain", "inet", "filter", chain]);
    let nft_set = |set: &str| argv(&["nft", "-j", "list", "set", "inet", "filter", set]);
    verify::output_chain_ok(&run_ok(host, &nft_chain(verify::OUTPUT_CHAIN)).await?)?;
    verify::maint_egress_chain_ok(&run_ok(host, &nft_chain(verify::MAINT_EGRESS_CHAIN)).await?, cfg.maint_uid, MAINT_USER)?;
    verify::input_chain_ok(&run_ok(host, &nft_chain(verify::INPUT_CHAIN)).await?)?;
    verify::source_set_ok(&run_ok(host, &nft_set(verify::SRC4_SET)).await?, verify::SRC4_SET, &verify::EXPECTED_SRC4)?;
    verify::source_set_ok(&run_ok(host, &nft_set(verify::SRC6_SET)).await?, verify::SRC6_SET, &verify::EXPECTED_SRC6)?;

    run_ok(host, &argv(&["systemctl", "start", HOST_KEY_UNIT])).await?;
    let printed = run_ok(host, &argv(&["dropbearkey", "-y", "-f", HOST_KEY])).await?;
    Ok(vec![verify::host_key_fingerprint(&printed)?])
}

/// The journal's current end, so the window reads only what comes after.
async fn journal_position<H: MaintHost>(host: &H) -> Result<String, String> {
    let out = run_ok(host, &argv(&["journalctl", "--no-pager", "-o", "json", "-n", "1"])).await?;
    out.lines()
        .rev()
        .find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            v.get("__CURSOR").and_then(Value::as_str).filter(|c| journal::valid_cursor(c)).map(str::to_string)
        })
        .ok_or_else(|| "could not read the journal position".to_string())
}

fn journal_read_argv(after: &str) -> Vec<String> {
    vec![
        "journalctl".to_string(),
        "--no-pager".to_string(),
        "-o".to_string(),
        "json".to_string(),
        format!("--output-fields={}", journal::OUTPUT_FIELDS),
        format!("--after-cursor={after}"),
        format!("_SYSTEMD_SLICE={}", journal::DROPBEAR_SLICE),
    ]
}

fn usec_to_secs(usec: u64) -> u64 {
    usec / 1_000_000
}

/// What one journal read produced for a window.
struct Read {
    next: WindowFile,
    records: Vec<Value>,
    events: Vec<SshEvent>,
    /// Everything up to `until` (or the journal's end) was read.
    complete: bool,
}

/// Read journal events after `window.last_cursor` (at most
/// [`MAX_EVENTS_PER_READ`] lines, streamed) and turn them into audit records:
/// logins/logouts (first [`LOGIN_RECORD_CAP`] each, then counted) and failure
/// counts. With `until_usec`, entries after that time are left alone.
async fn read_events<H: MaintHost>(host: &H, cfg: &MaintConfig, window: &WindowFile, until_usec: Option<u64>) -> Result<Read, String> {
    if !journal::valid_cursor(&window.last_cursor) {
        return Err("the window's journal cursor is invalid".to_string());
    }
    let out = host.run_lines(&journal_read_argv(&window.last_cursor), MAX_EVENTS_PER_READ).await?;
    let mut next = window.clone();
    let mut records = Vec::new();
    let mut events = Vec::new();
    let mut complete = !out.truncated;
    for line in &out.lines {
        let Some((cursor, event)) = journal::classify_line(line, cfg.maint_uid) else { continue };
        if let (Some(until), Some(e)) = (until_usec, &event) {
            if e.realtime_usec > until {
                complete = true;
                break;
            }
        }
        next.last_cursor = cursor;
        let Some(event) = event else { continue };
        match event.kind {
            EventKind::Login => {
                if next.logins_recorded < LOGIN_RECORD_CAP {
                    next.logins_recorded += 1;
                    let mut r = event_record("login", &next.window_id, &event);
                    r["fingerprint"] = json!(event.fingerprint);
                    records.push(r);
                } else {
                    next.logins_over_cap += 1;
                }
            }
            EventKind::Logout => {
                if next.logouts_recorded < LOGIN_RECORD_CAP {
                    next.logouts_recorded += 1;
                    records.push(event_record("logout", &next.window_id, &event));
                } else {
                    next.logouts_over_cap += 1;
                }
            }
            EventKind::AuthFailed => {
                next.auth_failures += 1;
                if let Some(src) = &event.source {
                    if next.auth_failure_sources.len() < AUTH_FAILURE_SOURCES && !next.auth_failure_sources.contains(src) {
                        next.auth_failure_sources.push(src.clone());
                    }
                }
            }
            EventKind::MenuAction => {}
        }
        events.push(event);
    }
    Ok(Read { next, records, events, complete })
}

/// Read new journal events into the open window: records go to the audit
/// file, events to the status buffer. State changes only after the audit
/// write succeeded, so a failed write is retried from the same cursor.
/// Returns whether the persisted window changed.
async fn ingest<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> Result<bool, String> {
    let Some(current) = state.window.as_ref() else { return Ok(false) };
    let read = read_events(host, cfg, current, None).await?;
    if read.next == *current {
        return Ok(false);
    }
    cfg.audit.append(read.records, Some(read.next.open_record.clone())).await?;
    for event in read.events {
        apply_session(state, &event);
        state.events.push(event);
    }
    state.window = Some(read.next);
    Ok(true)
}

fn persist_pending(cfg: &MaintConfig, pending: &PendingAudit) -> Result<(), String> {
    let Some(dir) = KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner).map_err(|e| e.to_string())? else {
        if pending.is_empty() {
            return Ok(());
        }
        return Err(format!("{} is missing", cfg.key_dir.display()));
    };
    if pending.is_empty() {
        return dir.remove(PENDING_FILE).map(|_| ()).map_err(|e| e.to_string());
    }
    let body = serde_json::to_vec(pending).map_err(|e| e.to_string())?;
    dir.write_atomic(PENDING_FILE, &body, cfg.state_owner, false).map_err(|e| e.to_string())
}

fn load_pending(cfg: &MaintConfig) -> Result<Option<PendingAudit>, String> {
    let Some(dir) = KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner).map_err(|e| e.to_string())? else { return Ok(None) };
    if !dir.exists(PENDING_FILE).map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let raw = dir.read(PENDING_FILE, cfg.state_owner, MAX_PENDING_FILE_BYTES).map_err(|e| e.to_string())?;
    serde_json::from_slice(&raw).map(Some).map_err(|e| format!("{PENDING_FILE}: {e}"))
}

/// Finish owed audit work: replay a closed window's remaining events, then
/// write the held-back records (its close record last, completed with the
/// final counters). Stops at the first failure and keeps the rest.
async fn flush_pending<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> Result<(), String> {
    if state.pending.is_empty() {
        return Ok(());
    }
    let result = flush_pending_inner(state, host, cfg).await;
    if let Err(e) = persist_pending(cfg, &state.pending) {
        warn!(error = %e, "sysd: could not save pending maintenance SSH audit work");
    }
    result
}

async fn flush_pending_inner<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> Result<(), String> {
    if let Some(bf) = state.pending.backfill.clone() {
        let read = read_events(host, cfg, &bf.window, Some(bf.until_usec)).await?;
        cfg.audit.append(read.records, None).await?;
        let complete = read.complete;
        let w = read.next;
        if complete {
            for record in state.pending.records.iter_mut() {
                if record["event"] == "close" && record["window_id"] == json!(w.window_id) {
                    fill_close_counters(record, &w, true);
                }
            }
            state.pending.backfill = None;
        } else {
            state.pending.backfill = Some(Backfill { window: w, until_usec: bf.until_usec });
            return Err("more events of a closed window remain to be written".to_string());
        }
    }
    let records = std::mem::take(&mut state.pending.records);
    if let Err(e) = cfg.audit.append(records.clone(), None).await {
        state.pending.records = records;
        return Err(e);
    }
    Ok(())
}

fn fill_close_counters(record: &mut Value, w: &WindowFile, events_complete: bool) {
    record["logins_recorded"] = json!(w.logins_recorded);
    record["logins_over_cap"] = json!(w.logins_over_cap);
    record["logouts_recorded"] = json!(w.logouts_recorded);
    record["logouts_over_cap"] = json!(w.logouts_over_cap);
    record["auth_failures"] = json!(w.auth_failures);
    record["auth_failure_sources"] = json!(w.auth_failure_sources);
    record["last_cursor"] = json!(w.last_cursor);
    record["events_complete"] = json!(events_complete);
}

fn event_record(kind: &str, window_id: &str, event: &SshEvent) -> Value {
    json!({
        "ts": usec_to_secs(event.realtime_usec),
        "event": kind,
        "window_id": window_id,
        "source": event.source,
        "pid": event.pid,
        "cursor": event.cursor,
    })
}

fn apply_session(state: &mut MaintState, event: &SshEvent) {
    match event.kind {
        EventKind::Login => {
            let s = Session {
                pid: event.pid,
                source: event.source.clone(),
                fingerprint: event.fingerprint.clone(),
                since_usec: event.realtime_usec,
            };
            match event.pid {
                Some(pid) => {
                    state.sessions.insert(pid, s);
                }
                None => state.unkeyed_sessions.push(s),
            }
        }
        EventKind::Logout => {
            let removed = event.pid.and_then(|pid| state.sessions.remove(&pid)).is_some();
            if !removed {
                if let Some(i) = state.unkeyed_sessions.iter().position(|s| s.source == event.source) {
                    state.unkeyed_sessions.remove(i);
                }
            }
        }
        _ => {}
    }
}

fn persist_window(cfg: &MaintConfig, window: &WindowFile) -> Result<(), String> {
    let dir = KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{} is missing", cfg.key_dir.display()))?;
    let body = serde_json::to_vec(window).map_err(|e| e.to_string())?;
    dir.write_atomic(WINDOW_FILE, &body, cfg.state_owner, false).map_err(|e| e.to_string())
}

async fn cancel_fallback<H: MaintHost>(host: &H) -> Result<(), String> {
    // Stopping or resetting a unit that does not exist fails; the read below
    // decides.
    host.run(&timer::stop_argv()).await?;
    let _ = host.run(&timer::reset_failed_argv()).await;
    let out = host.run(&timer::is_active_argv()).await?;
    match out.stdout.trim() {
        "inactive" | "failed" => Ok(()),
        other => Err(format!("fallback timer {} is still {other:?}", timer::FALLBACK_TIMER)),
    }
}

async fn fallback_active<H: MaintHost>(host: &H) -> Option<bool> {
    host.run(&timer::is_active_argv()).await.ok().map(|o| o.stdout.trim() == "active")
}

/// `ssh_maint_open` (design §8.1). Caller holds [`SSH_LOCK`].
pub async fn open<H: MaintHost>(
    state: &mut MaintState,
    host: &H,
    cfg: &MaintConfig,
    public_key: &str,
    ttl_seconds: u64,
    window_id: &str,
) -> DispatchResult {
    let host_key_fingerprints = verify_effective(host, cfg)
        .await
        .map_err(|e| SysdError::unsupported(format!("maintenance SSH is not available on this image: {e}")))?;
    validate_window_id(window_id)?;
    validate_ttl(ttl_seconds)?;
    let key = crate::ssh_key::parse(public_key).map_err(|e| SysdError::bad_request(format!("ssh_key:{}", e.code())))?;
    if state.window.is_some() || state.pending_cleanup || !state.pending.is_empty() {
        return Err(busy("a maintenance window is already open or still being cleaned up"));
    }
    let now_boot = host.boottime().map_err(SysdError::io)?;
    rate_limit(state, cfg, now_boot)?;
    let now_real = host.realtime().map_err(SysdError::io)?;
    let cursor = journal_position(host).await.map_err(SysdError::io)?;

    let dir = KeyDir::open_or_create(&cfg.key_dir, cfg.dir_owner).map_err(fs_err)?;
    match dir.write_atomic(KEY_FILE, key.authorized_keys_line().as_bytes(), cfg.key_owner, true) {
        Ok(()) => {}
        Err(FsError::Exists) => return Err(busy("a maintenance key is already installed")),
        Err(e) => {
            let _ = dir.remove(KEY_FILE);
            return Err(fs_err(e));
        }
    }

    let expires_at = now_real.as_secs() + ttl_seconds;
    let open_record = json!({
        "ts": now_real.as_secs(),
        "event": "open",
        "window_id": window_id,
        "key_type": key.key_type.name(),
        "key_fingerprint": key.fingerprint,
        "ttl_seconds": ttl_seconds,
        "expires_at": expires_at,
        "host_key_fingerprints": host_key_fingerprints,
    });
    let window = WindowFile {
        format: 1,
        window_id: window_id.to_string(),
        key_type: key.key_type.name().to_string(),
        key_fingerprint: key.fingerprint.clone(),
        host_key_fingerprints: host_key_fingerprints.clone(),
        ttl_seconds,
        deadline_boottime_ms: now_boot.as_millis() as u64 + ttl_seconds * 1000,
        opened_at_unix: now_real.as_secs(),
        open_cursor: cursor.clone(),
        last_cursor: cursor,
        logins_recorded: 0,
        logins_over_cap: 0,
        logouts_recorded: 0,
        logouts_over_cap: 0,
        auth_failures: 0,
        auth_failure_sources: Vec::new(),
        open_record: open_record.clone(),
    };

    // From here on every failure undoes what was done, newest first.
    let mut stage = OpenStage::KeyWritten;
    let result: Result<(), SysdError> = async {
        persist_window(cfg, &window).map_err(SysdError::io)?;
        state.window = Some(window.clone());
        stage = OpenStage::DeadlineSet;

        let _ = host.run(&timer::reset_failed_argv()).await;
        // Marked before running: a `systemd-run` that times out may still have
        // created the timer, and cancelling one that does not exist is harmless.
        stage = OpenStage::TimerArmed;
        run_ok(host, &timer::arm_argv(ttl_seconds, now_real)).await.map_err(|e| SysdError::io(format!("could not arm the fallback timer: {e}")))?;

        let started = host.ssh_start().await;
        stage = OpenStage::SshStarted;
        match started {
            Ok(out) if out.success => {}
            Ok(out) => return Err(SysdError::io(format!("could not start SSH: {}", out.stderr.trim()))),
            Err(e) => return Err(SysdError::new(e.kind, format!("could not start SSH: {}", e.message))),
        }
        cfg.audit.append(vec![open_record.clone()], Some(open_record.clone())).await.map_err(SysdError::io)?;
        Ok(())
    }
    .await;

    if let Err(e) = result {
        let problems = undo_open(state, host, &dir, stage).await;
        // Best effort: the audit file may be what failed.
        let failed = json!({
            "ts": now_real.as_secs(),
            "event": "open_failed",
            "window_id": window_id,
            "key_fingerprint": key.fingerprint,
            "error": e.kind,
            "stage": format!("{stage:?}"),
        });
        if let Err(audit_err) = cfg.audit.append(vec![failed], None).await {
            warn!(error = %audit_err, "sysd: could not record the failed maintenance SSH open");
        }
        let message = if problems.is_empty() { e.message } else { format!("{}; undoing the open: {}", e.message, problems.join("; ")) };
        return Err(SysdError::new(e.kind, message));
    }

    info!(window_id, fingerprint = %key.fingerprint, ttl_seconds, "sysd: maintenance SSH window opened");
    let body = json!({
        "login_user": MAINT_USER,
        "window_id": window_id,
        "key_type": key.key_type.name(),
        "key_fingerprint": key.fingerprint,
        "expires_in_s": ttl_seconds,
        "host_key_fingerprints": host_key_fingerprints,
    });
    Ok(SysdOpOutput { success: true, stdout: body.to_string(), stderr: String::new() })
}

/// Count an open attempt against the rate limit, refusing when it is used up.
fn rate_limit(state: &mut MaintState, cfg: &MaintConfig, now_boot: Duration) -> Result<(), SysdError> {
    let now = now_boot.as_millis() as u64;
    state.open_attempts_ms.retain(|t| now.saturating_sub(*t) < 3_600_000);
    let too_soon = state
        .open_attempts_ms
        .last()
        .is_some_and(|last| now.saturating_sub(*last) < cfg.open_min_interval.as_millis() as u64);
    if too_soon || state.open_attempts_ms.len() >= cfg.open_max_per_hour {
        return Err(SysdError::new(
            "rate_limited",
            format!(
                "too many maintenance SSH opens: at most {} per hour, {} s apart",
                cfg.open_max_per_hour,
                cfg.open_min_interval.as_secs()
            ),
        ));
    }
    state.open_attempts_ms.push(now);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OpenStage {
    KeyWritten,
    DeadlineSet,
    TimerArmed,
    SshStarted,
}

async fn undo_open<H: MaintHost>(state: &mut MaintState, host: &H, dir: &KeyDir, stage: OpenStage) -> Vec<String> {
    let mut problems = Vec::new();
    if stage >= OpenStage::SshStarted {
        match host.ssh_stop().await {
            Ok(out) if out.success => {}
            Ok(out) => problems.push(out.stderr),
            Err(e) => problems.push(e.message),
        }
    }
    if stage >= OpenStage::TimerArmed {
        if let Err(e) = cancel_fallback(host).await {
            problems.push(e);
        }
    }
    state.window = None;
    if let Err(e) = dir.remove(WINDOW_FILE) {
        problems.push(e.to_string());
    }
    if let Err(e) = dir.remove(KEY_FILE) {
        problems.push(e.to_string());
    }
    // Something could not be undone: the tick keeps closing until it can.
    state.pending_cleanup = !problems.is_empty();
    problems
}

/// Whether a key or window file is lying around.
fn leftovers(cfg: &MaintConfig) -> Result<bool, String> {
    match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner) {
        Ok(None) => Ok(false),
        Ok(Some(dir)) => Ok(dir.exists(KEY_FILE).map_err(|e| e.to_string())? || dir.exists(WINDOW_FILE).map_err(|e| e.to_string())?),
        Err(e) => Err(e.to_string()),
    }
}

/// Close the window: write the events not yet in the audit file, remove the
/// key and `window.json`, drop the deadline, cancel the fallback timer, stop
/// dropbear and its sessions and close the port (the legacy close sequence),
/// then write the close record. Every step runs even if an earlier one
/// failed; anything unclear is a problem, never "closed". Caller holds
/// [`SSH_LOCK`].
pub async fn close<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig, trigger: CloseTrigger) -> SysdOpOutput {
    let mut done: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    let dir = match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner) {
        Ok(d) => d,
        Err(e) => {
            problems.push(format!("maintenance key directory: {e}"));
            None
        }
    };
    if state.window.is_none() {
        if let Some(d) = &dir {
            match d.exists(WINDOW_FILE) {
                Ok(false) => {}
                Ok(true) => match d
                    .read(WINDOW_FILE, cfg.state_owner, MAX_WINDOW_FILE_BYTES)
                    .map_err(|e| e.to_string())
                    .and_then(|raw| serde_json::from_slice::<WindowFile>(&raw).map_err(|e| e.to_string()))
                {
                    Ok(w) => state.window = Some(w),
                    Err(e) => problems.push(format!("{WINDOW_FILE} unreadable, its events cannot be written: {e}")),
                },
                Err(e) => problems.push(e.to_string()),
            }
        }
    }
    let key_existed = match &dir {
        Some(d) => d.exists(KEY_FILE).unwrap_or(true),
        None => false,
    };
    let mut events_complete = true;
    if state.window.is_some() {
        if let Err(e) = ingest(state, host, cfg).await {
            events_complete = false;
            problems.push(format!("writing pending SSH events (kept for a retry): {e}"));
        }
    }
    let close_usec = host.realtime().map(|d| d.as_micros() as u64).unwrap_or(u64::MAX);
    let removed_fingerprint = state
        .window
        .as_ref()
        .map(|w| w.key_fingerprint.clone())
        .or_else(|| dir.as_ref().and_then(|d| installed_fingerprint(d, cfg)));

    if let Some(d) = &dir {
        match d.remove(KEY_FILE) {
            Ok(true) => done.push("removed the maintenance key".to_string()),
            Ok(false) => {}
            Err(e) => problems.push(format!("removing the maintenance key: {e}")),
        }
        if let Err(e) = d.remove(WINDOW_FILE) {
            problems.push(format!("removing {WINDOW_FILE}: {e}"));
        }
    }
    let window = state.window.take();
    let had_window = window.is_some() || key_existed || state.pending_cleanup;
    let open_sessions = state.sessions.len() + state.unkeyed_sessions.len();

    if let Err(e) = cancel_fallback(host).await {
        problems.push(e);
    }

    match host.ssh_stop().await {
        Ok(out) => {
            if !out.stdout.trim().is_empty() {
                done.push(out.stdout.trim().to_string());
            }
            if !out.success {
                problems.push(out.stderr.trim().to_string());
            }
        }
        Err(e) => problems.push(e.message),
    }

    if window.is_some() || key_existed {
        let now = close_usec / 1_000_000;
        let w = window.as_ref();
        let mut record = json!({
            "ts": now,
            "event": "close",
            "trigger": trigger.as_str(),
            "window_id": w.map(|w| w.window_id.clone()),
            "removed_fingerprint": removed_fingerprint,
            "done": done,
            "problems": problems,
            "logins_recorded": w.map(|w| w.logins_recorded),
            "logins_over_cap": w.map(|w| w.logins_over_cap),
            "logouts_recorded": w.map(|w| w.logouts_recorded),
            "logouts_over_cap": w.map(|w| w.logouts_over_cap),
            "auth_failures": w.map(|w| w.auth_failures),
            "auth_failure_sources": w.map(|w| w.auth_failure_sources.clone()),
            // Sessions still logged in when the close ended them (their
            // processes are killed, so dropbear writes no logout line).
            "sessions_open_at_close": open_sessions,
            "last_cursor": Value::Null,
            "events_complete": events_complete,
        });
        if let Some(w) = w {
            fill_close_counters(&mut record, w, events_complete);
        }
        match (events_complete, w) {
            // Events still owed: hold the close record back until they are in.
            (false, Some(w)) => {
                state.pending.backfill = Some(Backfill { window: w.clone(), until_usec: close_usec });
                state.pending.records.push(record);
            }
            _ => {
                if let Err(e) = cfg.audit.append(vec![record.clone()], None).await {
                    problems.push(format!("close record kept for a retry: {e}"));
                    state.pending.records.push(record);
                }
            }
        }
        if let Err(e) = persist_pending(cfg, &state.pending) {
            problems.push(format!("saving pending audit work: {e}"));
        }
        info!(trigger = trigger.as_str(), problems = problems.len(), "sysd: maintenance SSH window closed");
    }
    state.sessions.clear();
    state.unkeyed_sessions.clear();
    state.events = EventBuffer::new();
    // A close that left something behind (key, timer, port or sessions) is
    // retried by the tick until it succeeds, whoever asked for it. Only for
    // windows: a failed legacy close on an image without the account is
    // reported to the caller as before.
    if had_window {
        state.pending_cleanup = !problems.is_empty();
    }

    SysdOpOutput {
        success: problems.is_empty(),
        stdout: if done.is_empty() { "nothing was open".to_string() } else { done.join("; ") },
        stderr: problems.join("; "),
    }
}

fn installed_fingerprint(dir: &KeyDir, cfg: &MaintConfig) -> Option<String> {
    let raw = dir.read(KEY_FILE, cfg.key_owner, crate::ssh_key::MAX_INPUT_BYTES * 2).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let mut fields = text.split_ascii_whitespace().skip(1);
    let (ty, b64) = (fields.next()?, fields.next()?);
    crate::ssh_key::parse(&format!("{ty} {b64}")).ok().map(|k| k.fingerprint)
}

/// `ssh_maint_status`. Reads new events first (so the audit file is never
/// behind what the gateway is told). Caller holds [`SSH_LOCK`].
pub async fn status<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> DispatchResult {
    let mut problems: Vec<String> = Vec::new();
    match ingest(state, host, cfg).await {
        Ok(true) => {
            if let Some(w) = state.window.as_ref() {
                if let Err(e) = persist_window(cfg, w) {
                    problems.push(e);
                }
            }
        }
        Ok(false) => {}
        Err(e) => problems.push(format!("reading SSH events: {e}")),
    }
    let key_installed = match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner) {
        Ok(None) => Some(false),
        Ok(Some(d)) => d.exists(KEY_FILE).ok(),
        Err(e) => {
            problems.push(e.to_string());
            None
        }
    };
    let expires_in_s = match (state.window.as_ref(), host.boottime()) {
        (Some(w), Ok(now)) => Some(w.deadline_boottime_ms.saturating_sub(now.as_millis() as u64) / 1000),
        _ => None,
    };
    let ssh_active = host
        .run(&argv(&["systemctl", "is-active", DROPBEAR_SOCKET]))
        .await
        .ok()
        .map(|o| o.stdout.trim() == "active");
    let mut sessions: Vec<&Session> = state.sessions.values().collect();
    sessions.extend(state.unkeyed_sessions.iter());
    let w = state.window.as_ref();
    let body = json!({
        "ready": readiness(&cfg.ready_marker) == Readiness::Ready,
        "key_installed": key_installed,
        "key_fingerprint": w.map(|w| w.key_fingerprint.clone()),
        "window_id": w.map(|w| w.window_id.clone()),
        "expires_in_s": expires_in_s,
        "fallback_timer_active": fallback_active(host).await,
        "ssh_active": ssh_active,
        "sessions": sessions,
        "events": state.events.items,
        "event_overflow": state.events.overflow,
        "auth_failures": w.map(|w| w.auth_failures),
        "host_key_fingerprints": w.map(|w| w.host_key_fingerprints.clone()).unwrap_or_default(),
        "pending_cleanup": state.pending_cleanup,
        "pending_audit": !state.pending.is_empty(),
        "problems": problems,
    });
    Ok(SysdOpOutput { success: true, stdout: body.to_string(), stderr: String::new() })
}

/// R4: write new events, then close at the deadline. Also retries an
/// unfinished startup cleanup. Caller holds [`SSH_LOCK`].
pub async fn tick<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) {
    if let Err(e) = flush_pending(state, host, cfg).await {
        warn!(error = %e, "sysd: maintenance SSH audit work still pending; retrying next tick");
    }
    if state.window.is_none() {
        let leftover = match leftovers(cfg) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "sysd: cannot inspect the maintenance key directory");
                true
            }
        };
        if state.pending_cleanup || leftover {
            let out = close(state, host, cfg, CloseTrigger::StartupCleanup).await;
            if !out.success {
                warn!(problems = %out.stderr, "sysd: maintenance SSH cleanup incomplete; retrying next tick");
            }
        }
        return;
    }
    match ingest(state, host, cfg).await {
        Ok(true) => {
            if let Some(w) = state.window.as_ref() {
                if let Err(e) = persist_window(cfg, w) {
                    warn!(error = %e, "sysd: could not save the maintenance window state");
                }
            }
        }
        Ok(false) => {}
        Err(e) => warn!(error = %e, "sysd: could not read maintenance SSH events"),
    }
    let expired = match (state.window.as_ref(), host.boottime()) {
        (Some(w), Ok(now)) => now.as_millis() as u64 >= w.deadline_boottime_ms,
        // No trustworthy clock: close rather than keep a key without a deadline.
        (Some(_), Err(_)) => true,
        (None, _) => false,
    };
    if expired {
        let out = close(state, host, cfg, CloseTrigger::Deadline).await;
        if !out.success {
            warn!(problems = %out.stderr, "sysd: maintenance SSH deadline close incomplete; retrying next tick");
        }
    }
}

/// R5: at startup, close whatever a previous sysd left behind.
pub async fn startup_cleanup<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) {
    match load_pending(cfg) {
        Ok(Some(p)) => state.pending = p,
        Ok(None) => {}
        Err(e) => warn!(error = %e, "sysd: pending maintenance SSH audit work is unreadable"),
    }
    let leftover = match leftovers(cfg) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "sysd: cannot inspect the maintenance key directory at startup");
            true
        }
    };
    if !leftover {
        if let Err(e) = flush_pending(state, host, cfg).await {
            warn!(error = %e, "sysd: maintenance SSH audit work still pending; retrying every tick");
        }
        return;
    }
    let out = close(state, host, cfg, CloseTrigger::StartupCleanup).await;
    if let Err(e) = flush_pending(state, host, cfg).await {
        warn!(error = %e, "sysd: maintenance SSH audit work still pending; retrying every tick");
    }
    if out.success {
        info!("sysd: closed a maintenance SSH window left by a previous run");
    } else {
        warn!(problems = %out.stderr, "sysd: startup cleanup of maintenance SSH incomplete; retrying every tick");
    }
}

/// The R5 + R4 supervisor for the real system. `first` is [`SSH_LOCK`] taken
/// before the socket accepts connections, so the startup cleanup runs before
/// any SSH verb.
pub async fn run_supervisor(mut first: tokio::sync::MutexGuard<'static, MaintState>) {
    let cfg = MaintConfig::system();
    let host = host::SystemHost;
    startup_cleanup(&mut first, &host, &cfg).await;
    drop(first);
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        interval.tick().await;
        let mut state = SSH_LOCK.lock().await;
        tick(&mut state, &host, &cfg).await;
    }
}
