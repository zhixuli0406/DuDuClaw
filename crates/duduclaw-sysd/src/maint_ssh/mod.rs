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
mod events;
pub mod fsops;
pub mod host;
pub mod journal;
mod lifecycle;
pub mod timer;
pub mod verify;

pub use lifecycle::{close, open, run_supervisor, shutdown_flush, startup_cleanup, status, tick};

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
/// Persistent, root-only (0700) directory for the window state and owed
/// audit work. It must survive a reboot: a window that is followed by a
/// reboot (the `reboot` sysd verb is one call away for the gateway) is
/// closed by the next startup cleanup, which replays the journal from the
/// saved cursor across the boot. The key itself stays on tmpfs.
pub const STATE_DIR: &str = "/data/system/maint-ssh";
/// The window state (root 0600) in [`STATE_DIR`].
pub const WINDOW_FILE: &str = "window.json";
/// Audit work still owed after a close (root 0600) in [`STATE_DIR`], see
/// [`PendingAudit`].
pub const PENDING_FILE: &str = "audit-pending.json";
/// The most recent close (root 0600) in [`STATE_DIR`], reported by
/// `ssh_maint_status` as `last_close` (see [`LastClose`]). The OS fallback
/// script may write it too, with trigger `fallback_timer`.
pub const LAST_CLOSE_FILE: &str = "last-close.json";
/// When an `audit_backlog_lost` record was written, and in which boot
/// (root 0600) in [`STATE_DIR`].
pub const BACKLOG_LOST_FILE: &str = "backlog-lost.json";
/// Every `trigger` a close record can carry (sysd's own plus the OS fallback
/// script's `fallback_timer`).
pub const CLOSE_TRIGGERS: [&str; 6] = ["stop", "deadline", "startup_cleanup", "reboot", "firewall_changed", "fallback_timer"];

/// `last_close` in `ssh_maint_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastClose {
    pub window_id: Option<String>,
    pub trigger: String,
    pub events_complete: bool,
    pub events_lost: bool,
    /// Unix seconds, UTC.
    pub ts: u64,
}

/// `backlog-lost.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklogLost {
    pub ts: u64,
    pub boot_id: String,
}

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
/// A closed window's events that still cannot be read after this many tries
/// (a cursor rotated out of the journal, say) are given up: the held-back
/// records are written with `"events_complete": false`.
pub const MAX_BACKFILL_ATTEMPTS: u32 = 20;
/// `ssh_maint_open` rate limit, checked before anything else runs (so a flood
/// of calls cannot make sysd spawn the verification commands): at least this
/// long between attempts and at most [`OPEN_MAX_PER_HOUR`] per rolling hour.
/// A caller looping open/close would otherwise flood the audit file.
pub const OPEN_MIN_INTERVAL: Duration = Duration::from_secs(5);
pub const OPEN_MAX_PER_HOUR: usize = 12;

/// Where things live and who must own them. [`MaintConfig::system`] is the
/// appliance; tests point everything at a temp directory.
#[derive(Debug, Clone)]
pub struct MaintConfig {
    pub ready_marker: PathBuf,
    pub key_dir: PathBuf,
    /// [`STATE_DIR`] and its required ownership (root:root 0700).
    pub state_dir: PathBuf,
    pub state_dir_owner: Ownership,
    /// dropbear and the menu, as their `_EXE` must read in the journal.
    pub dropbear_exe: host::ExePin,
    pub menu_exe: host::ExePin,
    pub audit: AuditLog,
    /// `/run/duduclaw-maint-ssh`: root:maint 0750.
    pub dir_owner: Ownership,
    /// `authorized_keys`: root:maint 0640.
    pub key_owner: Ownership,
    /// `window.json`, `audit-pending.json`: root:root 0600.
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
            state_dir: PathBuf::from(STATE_DIR),
            state_dir_owner: Ownership { uid: 0, gid: 0, mode: 0o700 },
            dropbear_exe: host::ExePin { path: PathBuf::from(verify::DROPBEAR_PATH), dir: PathBuf::from("/usr/sbin") },
            menu_exe: host::ExePin { path: PathBuf::from(MAINT_SHELL), dir: PathBuf::from("/usr/libexec/duduclaw") },
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
    /// Boot the window was opened in; a different boot at startup cleanup
    /// means the device rebooted with the window open.
    #[serde(default)]
    pub boot_id: String,
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
    /// Failed replay attempts so far (see [`MAX_BACKFILL_ATTEMPTS`]).
    #[serde(default)]
    pub attempts: u32,
}

/// Persisted as [`PENDING_FILE`] so a sysd restart keeps retrying.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingAudit {
    pub backfill: Option<Backfill>,
    /// Records to write, in order, once `backfill` is done.
    pub records: Vec<Value>,
    /// The open record to rewrite at the top if writing them rotates the file.
    #[serde(default)]
    pub reopen: Option<Value>,
}

impl PendingAudit {
    const fn new() -> Self {
        Self { backfill: None, records: Vec::new(), reopen: None }
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
    /// Startup cleanup of a window opened in an earlier boot.
    Reboot,
    /// The tick found the maintenance egress rule (or its jump) gone.
    FirewallChanged,
}

impl CloseTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CloseTrigger::Stop => "stop",
            CloseTrigger::Deadline => "deadline",
            CloseTrigger::StartupCleanup => "startup_cleanup",
            CloseTrigger::Reboot => "reboot",
            CloseTrigger::FirewallChanged => "firewall_changed",
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

    let nft_set = |set: &str| argv(&["nft", "-j", "list", "set", "inet", "filter", set]);
    egress_rules_ok(host, cfg).await?;
    verify::input_chain_ok(&run_ok(host, &nft_chain(verify::INPUT_CHAIN)).await?)?;
    verify::source_set_ok(&run_ok(host, &nft_set(verify::SRC4_SET)).await?, verify::SRC4_SET, &verify::EXPECTED_SRC4)?;
    verify::source_set_ok(&run_ok(host, &nft_set(verify::SRC6_SET)).await?, verify::SRC6_SET, &verify::EXPECTED_SRC6)?;

    run_ok(host, &argv(&["systemctl", "start", HOST_KEY_UNIT])).await?;
    let printed = run_ok(host, &argv(&["dropbearkey", "-y", "-f", HOST_KEY])).await?;
    Ok(vec![verify::host_key_fingerprint(&printed)?])
}

fn nft_chain(chain: &str) -> Vec<String> {
    argv(&["nft", "-j", "list", "chain", "inet", "filter", chain])
}

/// The output-chain jump and the maintenance egress rule are in place.
/// Checked before opening and again by every tick while a window is open
/// (a failed `nftables` reload can drop the rule the OS adds at runtime).
async fn egress_rules_ok<H: MaintHost>(host: &H, cfg: &MaintConfig) -> Result<(), String> {
    verify::output_chain_ok(&run_ok(host, &nft_chain(verify::OUTPUT_CHAIN)).await?)?;
    verify::maint_egress_chain_ok(&run_ok(host, &nft_chain(verify::MAINT_EGRESS_CHAIN)).await?, cfg.maint_uid, MAINT_USER)
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
