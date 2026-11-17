//! Real Linux generation publication and service readback. Missing integration
//! endpoints refuse before publication; no successful mock is used in production.
use std::{ffi::CString, fs::File, io::Read, os::{fd::{AsRawFd, FromRawFd}, unix::fs::MetadataExt}, path::Path,
    process::{Command, Stdio}, time::{Duration, Instant}};
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::*};
use nix::sys::socket::{self, sockopt, UnixAddr};
use crate::{Error, Result, activation_contract::{self, ActivationRuntime, GenerationPreparation,
    Preparation, ControlRequest, ControlResponse, ControlCommand, ControlOutcome, ComponentReadback, GenerationEffects}, state::GenerationActivator};
use super::{files::{Directory, verify_fd}, transport, INSTALL_ROOT};

pub const SPAWND_UPDATE_SOCKET: &str = "/run/duduclaw-spawnd/update.sock";
pub const GATEWAY_READBACK_SOCKET: &str = "/run/duduclaw-gateway-update/readback.sock";
pub const EGRESS_READBACK_SOCKET: &str = "/run/duduclaw-egress-update/readback.sock";
const UNITS: [(&str, InstallTarget); 3] = [
    ("duduclaw-egress.service", InstallTarget::Egress),
    ("duduclaw-spawnd.service", InstallTarget::Spawnd),
    ("duduclaw-gateway.service", InstallTarget::Gateway),
];
const SPAWND_UNIT: &str = "duduclaw-spawnd.service";
/// `TimeoutStartSec=` of duduclaw-spawnd.service: its start verifies the
/// installed set (hashing up to two generations) before READY=1. Pinned
/// against the unit file by spawnd's unit-file test.
pub const SPAWND_START_TIMEOUT_SECS: u64 = 300;
/// `TimeoutStopSec=` of duduclaw-spawnd.service (pinned the same way).
pub const SPAWND_STOP_TIMEOUT_SECS: u64 = 90;
/// The one deadline of [`LinuxEffects::restart`]: a full stop and start of
/// spawnd plus a margin for egress and the gateway, which start only after
/// the consistency gate (itself after spawnd's READY=1) completes.
pub const RESTART_DEADLINE: Duration = Duration::from_secs(SPAWND_STOP_TIMEOUT_SECS + SPAWND_START_TIMEOUT_SECS + 60);
const RESTART_POLL: Duration = Duration::from_millis(500);

/// F's concrete client. B1's endpoint accepts only root SO_PEERCRED and persists
/// admission closure keyed by generation before Drain replies. G is called by
/// B1 for typed, digest-bound preparation. A/C implement their readback endpoints
/// under service-owned leaves of root-owned /run; all peers reject ancillary
/// data and unknown fields. The service binds its own readback socket so peer
/// credentials describe the service, rather than a PID 1 socket activator.
pub struct SocketRuntime;
fn socket_parent(path: &str, uid: u32, gid: u32) -> Result<()> {
    let parent = Path::new(path).parent().ok_or(Error::Invalid)?;
    if uid == 0 { Directory::open(parent)?; return Ok(()); }
    let anchor = Directory::open(parent.parent().ok_or(Error::Invalid)?)?;
    let leaf = literal(parent.file_name().and_then(|v| v.to_str()).ok_or(Error::Invalid)?)?;
    let raw = unsafe { libc::openat(anchor.raw_fd(), leaf.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if raw < 0 { return Err(std::io::Error::last_os_error().into()); }
    let file = unsafe { File::from_raw_fd(raw) };
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o022 != 0 {
        return Err(Error::Maintenance);
    }
    Ok(())
}
fn exchange(path: &str, uid: u32, gid: u32, command: ControlCommand) -> Result<ControlOutcome> {
    socket_parent(path, uid, gid)?;
    let fd = transport::socket()?;
    transport::timeout(&fd)?;
    socket::connect(fd.as_raw_fd(), &UnixAddr::new(path).map_err(transport::errno)?).map_err(transport::errno)?;
    let peer = socket::getsockopt(&fd, sockopt::PeerCredentials).map_err(transport::errno)?;
    if peer.uid() != uid || peer.gid() != gid { return Err(Error::Maintenance); }
    let mut nonce = [0; 16]; File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let request = ControlRequest { schema_version: 1, nonce: RequestId(nonce), command };
    transport::send(&fd, &request)?;
    let response: ControlResponse = transport::receive(&fd)?;
    activation_contract::validate_response(&request, &response)?;
    Ok(response.outcome)
}
fn root_exchange(command: ControlCommand) -> Result<ControlOutcome> {
    exchange(SPAWND_UPDATE_SOCKET, 0, 0, command)
}
fn read_components(path: &str, uid: u32, gid: u32, generation: Sha256Digest) -> Result<Vec<ComponentReadback>> {
    match exchange(path, uid, gid, ControlCommand::Readback { generation })? {
        ControlOutcome::Readback { components } => Ok(components), _ => Err(Error::Maintenance),
    }
}
impl GenerationPreparation for SocketRuntime {
    fn preflight(&mut self, preparation: &Preparation) -> Result<()> {
        root_exchange(ControlCommand::Prepare { preparation: preparation.clone() })?; Ok(())
    }
    fn apply(&mut self, preparation: &Preparation) -> Result<()> {
        root_exchange(ControlCommand::Apply { preparation: preparation.clone() })?; Ok(())
    }
}
impl ActivationRuntime for SocketRuntime {
    fn drain(&mut self, generation: Sha256Digest) -> Result<()> {
        // Type=simple ordering is not socket readiness. Retrying the same digest
        // also allows B1 to report active runs while its durable barrier drains.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match root_exchange(ControlCommand::Drain { generation }) {
                Ok(_) => return Ok(()),
                Err(_) if Instant::now() >= deadline => return Err(Error::Maintenance),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
    fn read_back(&mut self, generation: Sha256Digest) -> Result<Vec<ComponentReadback>> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.read_back_once(generation) {
                Ok(reports) if reports.iter().all(|r| r.ready) => return Ok(reports),
                _ if Instant::now() >= deadline => return Err(Error::Maintenance),
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
    fn resume(&mut self, generation: Sha256Digest) -> Result<()> {
        root_exchange(ControlCommand::Resume { generation })?; Ok(())
    }
}

impl SocketRuntime {
    fn read_back_once(&mut self, generation: Sha256Digest) -> Result<Vec<ComponentReadback>> {
        use nix::unistd::User;
        let gateway = User::from_name(duduclaw_isolation::ids::GATEWAY_USER).map_err(transport::errno)?.ok_or(Error::Maintenance)?;
        let egress = User::from_name(duduclaw_isolation::ids::EGRESS_USER).map_err(transport::errno)?.ok_or(Error::Maintenance)?;
        if gateway.uid.as_raw() == 0 || gateway.gid.as_raw() == 0 || egress.uid.as_raw() == 0
            || egress.gid.as_raw() == 0 || gateway.uid == egress.uid { return Err(Error::Maintenance); }
        let gateway_reports = read_components(GATEWAY_READBACK_SOCKET, gateway.uid.as_raw(), gateway.gid.as_raw(), generation)?;
        let egress_reports = read_components(EGRESS_READBACK_SOCKET, egress.uid.as_raw(), egress.gid.as_raw(), generation)?;
        if gateway_reports.len() != 1 || gateway_reports[0].target != InstallTarget::Gateway
            || gateway_reports[0].effective_uid != gateway.uid.as_raw()
            || egress_reports.len() != 1 || egress_reports[0].target != InstallTarget::Egress
            || egress_reports[0].effective_uid != egress.uid.as_raw() { return Err(Error::Maintenance); }
        let mut reports = read_components(SPAWND_UPDATE_SOCKET, 0, 0, generation)?;
        // B1 must independently execute non-daemon readbacks under non-root ids;
        // it cannot substitute for the two live non-root service endpoints.
        if reports.iter().any(|r| matches!(r.target, InstallTarget::Gateway | InstallTarget::Egress | InstallTarget::UpdateSlot)) {
            return Err(Error::Maintenance);
        }
        reports.extend(gateway_reports); reports.extend(egress_reports); Ok(reports)
    }
}

fn literal(value: &str) -> Result<CString> { CString::new(value).map_err(|_| Error::Invalid) }
fn parse_current(target: &[u8]) -> Result<Sha256Digest> {
    let text = std::str::from_utf8(target).map_err(|_| Error::Maintenance)?;
    let mut parts = text.split('/');
    if parts.next() != Some("generations") { return Err(Error::Maintenance); }
    let encoded = parts.next().ok_or(Error::Maintenance)?;
    if parts.next().is_some() || encoded.len() != 64 { return Err(Error::Maintenance); }
    let bytes: [u8; 32] = hex::decode(encoded).map_err(|_| Error::Maintenance)?.try_into().map_err(|_| Error::Maintenance)?;
    if hex::encode(bytes) != encoded { return Err(Error::Maintenance); }
    Ok(Sha256Digest(bytes))
}
/// The generation `current` names below an install root (also spawnd's
/// publication journal: the generation installed before an `Apply`).
pub fn read_current_at(install: &Directory) -> Result<Sha256Digest> {
    let current = literal("current")?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstatat(install.raw_fd(), current.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_uid != 0 || stat.st_gid != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFLNK { return Err(Error::Maintenance); }
    let mut buffer = [0u8; 128];
    let size = unsafe { libc::readlinkat(install.raw_fd(), current.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) };
    if size <= 0 || size as usize >= buffer.len() { return Err(Error::Maintenance); }
    parse_current(&buffer[..size as usize])
}
pub fn read_current() -> Result<Sha256Digest> {
    read_current_at(&Directory::open(Path::new(INSTALL_ROOT))?)
}
fn publish_current(install: &Directory, committed: Sha256Digest, next: Sha256Digest) -> Result<()> {
    let current = read_current_at(install)?;
    if current != committed && current != next { return Err(Error::Maintenance); }
    let generation = install.child("generations")?.child(&hex::encode(next.0))?;
    if unsafe { libc::fchmod(generation.raw_fd(), 0o755) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    generation.sync()?;
    if current == next { install.sync()?; return Ok(()); }
    swap_current(install, next)
}
/// First `current` of a host (plan R1 `trust_init`): the same temporary symlink +
/// rename protocol as an activation. Only an absent selector or one that already
/// names `next` (an interrupted earlier run) is accepted; never another generation.
pub(crate) fn publish_initial_current(install: &Directory, next: Sha256Digest) -> Result<()> {
    match read_current_at(install) {
        Ok(current) if current == next => { install.sync()?; return Ok(()); }
        Ok(_) => return Err(Error::Maintenance),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let generation = install.child("generations")?.child(&hex::encode(next.0))?;
    if unsafe { libc::fchmod(generation.raw_fd(), 0o755) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    generation.sync()?;
    swap_current(install, next)
}
fn swap_current(install: &Directory, next: Sha256Digest) -> Result<()> {
    match install.remove("current.next") {
        Ok(()) => (), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(e),
    }
    let target = literal(&format!("generations/{}", hex::encode(next.0)))?;
    let temporary = literal("current.next")?;
    if unsafe { libc::symlinkat(target.as_ptr(), install.raw_fd(), temporary.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    install.sync()?;
    install.rename("current.next", install, "current", true)?;
    if read_current_at(install)? != next { return Err(Error::Maintenance); }
    Ok(())
}

fn verify_generation(install: &Directory, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
    use sha2::{Digest, Sha256};
    let generation = install.child("generations")?.child(&hex::encode(digest.0))?;
    let bytes = generation.bytes("manifest.json", 256 * 1024)?;
    if Sha256::digest(&bytes).as_slice() != digest.0 || crate::metadata::decode_canonical::<ReleaseManifest>(&bytes)? != *manifest {
        return Err(Error::Signature);
    }
    for artifact in &manifest.artifacts {
        let file = generation.read(&super::service::artifact_name(&artifact.install_target)?)?;
        if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(Error::Signature); }
        verify_fd(file, artifact.size_bytes, artifact.sha256)?;
    }
    generation.sync()?; install.child("generations")?.sync()?; Ok(())
}

/// The longest any one systemctl call may run.
const SYSTEMCTL_BOUND: Duration = Duration::from_secs(60);
/// The one deadline of [`quiesce_units`] (stops of the gate, egress and the
/// gateway, then PID 1's readback).
const QUIESCE_DEADLINE: Duration = Duration::from_secs(180);
fn systemctl(args: &[&str], capture: bool) -> Result<Vec<u8>> { systemctl_within(args, capture, SYSTEMCTL_BOUND) }
/// Small fixed-argv systemd adapter. Commands and children have a bounded lifetime
/// (`budget`, at most [`SYSTEMCTL_BOUND`]; nothing is spawned for a zero budget),
/// no inherited environment, no shell, no stdin and bounded diagnostic output.
/// Unit restarts are queued with `--no-block` and waited for by
/// [`restart_units`], which passes each call the time left before its deadline.
fn systemctl_within(args: &[&str], capture: bool, budget: Duration) -> Result<Vec<u8>> {
    let budget = budget.min(SYSTEMCTL_BOUND);
    if budget.is_zero() { return Err(Error::Maintenance); }
    Directory::open(Path::new("/usr/bin"))?.read("systemctl")?;
    let mut child = Command::new("/usr/bin/systemctl").env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C").args(["--no-ask-password", "--no-pager"]).args(args)
        .stdin(Stdio::null()).stdout(if capture { Stdio::piped() } else { Stdio::null() }).stderr(Stdio::null()).spawn()?;
    let output = child.stdout.take().map(|mut pipe| std::thread::spawn(move || {
        let mut bytes = Vec::new(); pipe.by_ref().take(129).read_to_end(&mut bytes).map(|_| bytes)
    }));
    let deadline = Instant::now() + budget;
    let status = loop {
        if let Some(status) = child.try_wait()? { break status; }
        if Instant::now() >= deadline { let _ = child.kill(); let _ = child.wait(); return Err(Error::Maintenance); }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() { return Err(Error::Maintenance); }
    let bytes = match output { Some(handle) => handle.join().map_err(|_| Error::Maintenance)??, None => vec![] };
    if bytes.len() > 128 { return Err(Error::Maintenance); }
    Ok(bytes)
}
fn main_pid(unit: &str) -> Result<u32> {
    let output = systemctl(&["show", "--property=MainPID", "--value", unit], true)?;
    let text = std::str::from_utf8(&output).map_err(|_| Error::Maintenance)?.strip_suffix('\n').ok_or(Error::Maintenance)?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) { return Err(Error::Maintenance); }
    let pid = text.parse::<u32>().map_err(|_| Error::Maintenance)?;
    if pid <= 1 { return Err(Error::Maintenance); } Ok(pid)
}
fn verify_unit_identities() -> Result<()> {
    for (unit, target) in &UNITS {
        let (user, group, non_root) = match target {
            InstallTarget::Gateway => (b"duduclaw\n".as_slice(), b"duduclaw\n".as_slice(), true),
            InstallTarget::Egress => (b"duduclaw-egress\n".as_slice(), b"duduclaw-egress\n".as_slice(), true),
            InstallTarget::Spawnd => (b"root\n".as_slice(), b"root\n".as_slice(), false),
            _ => return Err(Error::Invalid),
        };
        let actual_user = systemctl(&["show", "--property=User", "--value", unit], true)?;
        let actual_group = systemctl(&["show", "--property=Group", "--value", unit], true)?;
        if (actual_user != user && (non_root || actual_user != b"\n"))
            || (actual_group != group && actual_group != b"\n") { return Err(Error::Maintenance); }
        if non_root && systemctl(&["show", "--property=NoNewPrivileges", "--value", unit], true)? != b"yes\n" {
            return Err(Error::Maintenance);
        }
    }
    Ok(())
}
fn live_processes(manifest: &ReleaseManifest) -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    for (unit, target) in &UNITS {
        systemctl(&["is-active", "--quiet", unit], false)?;
        let pid = main_pid(unit)?;
        let artifact = manifest.artifacts.iter().find(|a| &a.install_target == target).ok_or(Error::Incompatible)?;
        // PID is observation only, never an authorization principal. Compare it
        // again after authenticated readiness, alongside the loaded executable.
        verify_fd(File::open(format!("/proc/{pid}/exe"))?, artifact.size_bytes, artifact.sha256)?;
        pids.push(pid);
    }
    Ok(pids)
}

/// What [`restart_units`] and [`quiesce_units`] need from PID 1 and the clock
/// (production: [`Systemctl`]). Every PID 1 call gets `budget`: the time left
/// before the caller's one deadline, at most [`SYSTEMCTL_BOUND`].
trait UnitControl {
    /// Monotonic now (production: `Instant::now()`).
    fn now(&mut self) -> Instant;
    /// Wait between polls (production: `thread::sleep`).
    fn pause(&mut self, duration: Duration);
    /// `systemctl show --property=InvocationID --value` (empty: never started).
    fn invocation(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>>;
    /// `systemctl reset-failed`: flushes the unit's start-rate counter.
    fn reset_failed(&mut self, unit: &str, budget: Duration) -> Result<()>;
    /// `systemctl --no-block restart`: the job is queued, not waited for.
    fn restart_queued(&mut self, unit: &str, budget: Duration) -> Result<()>;
    /// `systemctl is-active --quiet`.
    fn active(&mut self, unit: &str, budget: Duration) -> bool;
    /// `systemctl show --property=Job --value`: PID 1 holds a job for the unit.
    fn job_pending(&mut self, unit: &str, budget: Duration) -> Result<bool>;
    /// `systemctl stop` (waits for the stop job, within `budget`).
    fn stop(&mut self, unit: &str, budget: Duration) -> Result<()>;
    /// `systemctl show --property=ActiveState --value`.
    fn active_state(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>>;
}
/// `show --property=Job --value`: empty (no job) or a job id. Anything else is uncertain.
fn parse_job(output: &[u8]) -> Result<bool> {
    let text = output.strip_suffix(b"\n").unwrap_or(output);
    if text.is_empty() { return Ok(false); }
    if text.iter().all(u8::is_ascii_digit) && text.len() <= 10 && text.iter().any(|b| *b != b'0') { return Ok(true); }
    Err(Error::Maintenance)
}
struct Systemctl;
impl UnitControl for Systemctl {
    fn now(&mut self) -> Instant { Instant::now() }
    fn pause(&mut self, duration: Duration) { std::thread::sleep(duration) }
    fn invocation(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>> {
        systemctl_within(&["show", "--property=InvocationID", "--value", unit], true, budget)
    }
    fn reset_failed(&mut self, unit: &str, budget: Duration) -> Result<()> {
        systemctl_within(&["reset-failed", unit], false, budget).map(drop)
    }
    fn restart_queued(&mut self, unit: &str, budget: Duration) -> Result<()> {
        systemctl_within(&["--no-block", "restart", unit], false, budget).map(drop)
    }
    fn active(&mut self, unit: &str, budget: Duration) -> bool {
        systemctl_within(&["is-active", "--quiet", unit], false, budget).is_ok()
    }
    fn job_pending(&mut self, unit: &str, budget: Duration) -> Result<bool> {
        parse_job(&systemctl_within(&["show", "--property=Job", "--value", unit], true, budget)?)
    }
    fn stop(&mut self, unit: &str, budget: Duration) -> Result<()> { systemctl_within(&["stop", unit], false, budget).map(drop) }
    fn active_state(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>> {
        systemctl_within(&["show", "--property=ActiveState", "--value", unit], true, budget)
    }
}
/// The time left before `deadline` for one PID 1 call (at most
/// [`SYSTEMCTL_BOUND`]); `Err(Maintenance)` at once when nothing remains.
fn remaining(control: &mut impl UnitControl, deadline: Instant) -> Result<Duration> {
    let left = deadline.saturating_duration_since(control.now());
    if left.is_zero() { return Err(Error::Maintenance); }
    Ok(left.min(SYSTEMCTL_BOUND))
}

/// The updater's step right before `Apply` (`GenerationEffects::quiesce`):
/// stop the consistency gate (through `BindsTo=` that stops egress and the
/// gateway), then egress's socket (so nothing can socket-activate egress, which
/// would pull the gate in again), then egress and the gateway explicitly, all
/// against one deadline; then PID 1 must report every unit of
/// `ISOLATION_GATED_UNITS` `inactive` or `failed` with no pending job. Any stop
/// or readback that fails or does not confirm that is `Err(Maintenance)`, before
/// `Apply` is sent: nothing was published, and the next activation attempt
/// repeats the idempotent sequence (spawnd also refuses `Apply` on its own
/// readback of the same units).
fn quiesce_units(control: &mut impl UnitControl, deadline: Duration) -> Result<()> {
    use duduclaw_isolation::ids::{ISOLATION_GATED_UNITS, ISOLATION_READY_UNIT, EGRESS_SOCKET_UNIT, EGRESS_UNIT, ISOLATED_GATEWAY_UNIT};
    let deadline = control.now() + deadline;
    for unit in [ISOLATION_READY_UNIT, EGRESS_SOCKET_UNIT, EGRESS_UNIT, ISOLATED_GATEWAY_UNIT] {
        let budget = remaining(control, deadline)?;
        control.stop(unit, budget)?;
    }
    for unit in ISOLATION_GATED_UNITS {
        let budget = remaining(control, deadline)?;
        let state = control.active_state(unit, budget)?;
        if state != b"inactive\n" && state != b"failed\n" { return Err(Error::Maintenance); }
        let budget = remaining(control, deadline)?;
        if control.job_pending(unit, budget)? { return Err(Error::Maintenance); }
    }
    Ok(())
}
/// Restart the three units against one deadline that exceeds spawnd's whole
/// stop and start budget ([`RESTART_DEADLINE`]): the jobs are queued with
/// `--no-block` (PID 1 owns them; no client is killed half way), then every
/// unit must have no pending job (the queued restart has run, not merely an
/// automatic restart that finished in between), be `active` and have a new
/// invocation id (a start that began after this restart was requested). The
/// deadline is strict: every PID 1 call gets only the time left (at most
/// [`SYSTEMCTL_BOUND`]), polls sleep at most that long, and nothing is called
/// once it has passed. spawnd's start-rate counter is flushed first:
/// this restart follows every `Apply`, including every repairing one, and must
/// not consume the crash-loop budget (`StartLimitBurst=`) that automatic
/// restarts are counted against.
///
/// spawnd is queued first: its restart also restarts the consistency gate
/// (`PartOf=`/`BindsTo=`) and with it egress and the gateway, so their queued
/// restarts merge behind the new gate instead of starting against the old one.
///
/// On timeout `Err(Maintenance)`. Nothing is resumed: spawnd's drain record
/// still closes admission for the generation, its publication journal says
/// `published`, `current` already names the generation and the updater's
/// pending activation stays `Prepared`. The next activation attempt (or the
/// updater's start-up reconciliation) repeats the idempotent sequence, whose
/// own restart replaces any job PID 1 still holds; a spawnd that came back
/// inconsistent meanwhile is repaired by that attempt's `Apply`.
fn restart_units(control: &mut impl UnitControl, deadline: Duration, poll: Duration) -> Result<()> {
    let deadline = control.now() + deadline;
    let mut before = Vec::with_capacity(UNITS.len());
    for (unit, _) in &UNITS {
        let budget = remaining(control, deadline)?;
        before.push(control.invocation(unit, budget)?);
    }
    let budget = remaining(control, deadline)?;
    control.reset_failed(SPAWND_UNIT, budget)?;
    let budget = remaining(control, deadline)?;
    control.restart_queued(SPAWND_UNIT, budget)?;
    for (unit, _) in UNITS.iter().filter(|(unit, _)| *unit != SPAWND_UNIT) {
        let budget = remaining(control, deadline)?;
        control.restart_queued(unit, budget)?;
    }
    loop {
        let mut restarted = true;
        for ((unit, _), old) in UNITS.iter().zip(&before) {
            // The job first: once none is pending, the explicit restart has run.
            let budget = remaining(control, deadline)?;
            if control.job_pending(unit, budget).unwrap_or(true) { restarted = false; break; }
            let budget = remaining(control, deadline)?;
            let fresh = control.invocation(unit, budget).is_ok_and(|now| now.iter().any(u8::is_ascii_hexdigit) && now != *old);
            if !fresh { restarted = false; break; }
            let budget = remaining(control, deadline)?;
            if !control.active(unit, budget) { restarted = false; break; }
        }
        if restarted { return Ok(()); }
        let left = remaining(control, deadline)?;
        control.pause(poll.min(left));
    }
}

pub struct LinuxGenerationActivator<R = SocketRuntime> { committed: Sha256Digest, runtime: R }
impl LinuxGenerationActivator<SocketRuntime> {
    pub fn new(committed: Sha256Digest) -> Self { Self { committed, runtime: SocketRuntime } }
}
impl<R: ActivationRuntime> LinuxGenerationActivator<R> {
    pub fn close_admission(&mut self, digest: Sha256Digest) -> Result<()> { self.runtime.drain(digest) }
    /// Routine boot never replays migrations. Close admission before inspecting
    /// current or loaded bytes, so a mixed generation cannot keep starting work.
    pub fn verify_committed(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        self.runtime.drain(digest)?;
        let mut effects = LinuxEffects { install: Directory::open(Path::new(INSTALL_ROOT))?, committed: self.committed };
        effects.verify(manifest, digest)?;
        activation_contract::healthy_readback(&mut self.runtime, &mut effects, manifest, digest)?;
        self.runtime.resume(digest)
    }
}
struct LinuxEffects { install: Directory, committed: Sha256Digest }
impl GenerationEffects for LinuxEffects {
    fn verify(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        verify_unit_identities()?; verify_generation(&self.install, manifest, digest)
    }
    /// Runs right before `Apply` (`activation_contract::activate_generation`).
    fn quiesce(&mut self) -> Result<()> { quiesce_units(&mut Systemctl, QUIESCE_DEADLINE) }
    fn publish(&mut self, digest: Sha256Digest) -> Result<()> { publish_current(&self.install, self.committed, digest) }
    fn restart(&mut self) -> Result<()> {
        verify_unit_identities()?;
        restart_units(&mut Systemctl, RESTART_DEADLINE, RESTART_POLL)
    }
    fn processes(&mut self, manifest: &ReleaseManifest) -> Result<Vec<u32>> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match live_processes(manifest) {
                Ok(pids) => return Ok(pids),
                Err(_) if Instant::now() >= deadline => return Err(Error::Maintenance),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
    fn current(&mut self) -> Result<Sha256Digest> { read_current_at(&self.install) }
    fn sync(&mut self) -> Result<()> { self.install.sync() }
}
impl<R: ActivationRuntime> GenerationActivator for LinuxGenerationActivator<R> {
    fn preflight(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        let install = Directory::open(Path::new(INSTALL_ROOT))?;
        let current = read_current_at(&install)?;
        if current != self.committed && current != digest { return Err(Error::Maintenance); }
        manifest.validate()?;
        verify_unit_identities()?;
        self.runtime.preflight(&Preparation::for_manifest(manifest, digest))
    }
    fn activate_and_read_back(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        let mut effects = LinuxEffects { install: Directory::open(Path::new(INSTALL_ROOT))?, committed: self.committed };
        activation_contract::activate_generation(&mut self.runtime, &mut effects, manifest, digest)
    }
    fn finish_after_commit(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        let mut effects = LinuxEffects { install: Directory::open(Path::new(INSTALL_ROOT))?, committed: self.committed };
        effects.verify(manifest, digest)?;
        activation_contract::healthy_readback(&mut self.runtime, &mut effects, manifest, digest)?;
        self.runtime.resume(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn selector_requires_exact_canonical_relative_generation() {
        let valid = format!("generations/{}", "ab".repeat(32));
        assert!(parse_current(valid.as_bytes()).is_ok());
        for invalid in [format!("generations/{}", "AB".repeat(32)), format!("/{valid}"), format!("{valid}/extra"),
            format!("../{valid}"), format!("generations/{}", "ab".repeat(31))] {
            assert!(parse_current(invalid.as_bytes()).is_err());
        }
    }
    use std::{cell::RefCell, rc::Rc};
    use duduclaw_isolation::ids::ISOLATION_GATED_UNITS;
    type Log = Rc<RefCell<Vec<String>>>;
    /// Scripted PID 1 on a fake clock. After the restarts are queued, PID 1
    /// holds the explicit restart job until `settles_after` `Job` reads
    /// happened (`None`: never, as with a spawnd stuck verifying or failing);
    /// then every unit reports no job, a new invocation and `active`.
    /// `early_fresh`: an automatic restart finished between the "before" sample
    /// and the explicit job: active under a new invocation while that job is
    /// still pending. Every call takes `cost` of fake time, cut at its budget
    /// (a call that reaches its budget is killed: an error).
    struct FakeUnits { log: Log, restarted: bool, polls: usize, settles_after: Option<usize>, always_active: bool,
        early_fresh: bool, base: Instant, clock: Duration, cost: Duration, budgets: Vec<Duration>,
        /// Quiesce: `ActiveState` reported per unit (default `inactive`), units
        /// with a pending job, and a unit whose stop fails.
        states: Vec<(&'static str, &'static str)>, jobs: Vec<&'static str>, stop_fails: Option<&'static str> }
    impl FakeUnits {
        fn new() -> Self {
            Self { log: Log::default(), restarted: false, polls: 0, settles_after: None, always_active: false,
                early_fresh: false, base: Instant::now(), clock: Duration::ZERO, cost: Duration::ZERO, budgets: vec![],
                states: vec![], jobs: vec![], stop_fails: None }
        }
        fn settled(&self) -> bool { self.restarted && self.settles_after.is_some_and(|after| self.polls >= after) }
        fn fresh(&self) -> bool { self.settled() || (self.early_fresh && self.restarted) }
        /// One PID 1 call: `false` when it ran into its budget (killed).
        fn call(&mut self, entry: String, budget: Duration) -> bool {
            assert!(!budget.is_zero() && budget <= SYSTEMCTL_BOUND, "{entry}: {budget:?}");
            self.log.borrow_mut().push(entry);
            self.budgets.push(budget);
            self.clock += self.cost.min(budget);
            self.cost < budget
        }
        fn calls(&self) -> Vec<String> { self.log.borrow().clone() }
    }
    impl UnitControl for FakeUnits {
        fn now(&mut self) -> Instant { self.base + self.clock }
        fn pause(&mut self, duration: Duration) { self.clock += duration; }
        fn invocation(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>> {
            if !self.call(format!("invocation {unit}"), budget) { return Err(Error::Maintenance); }
            let digit = if self.fresh() { "b" } else { "a" };
            Ok(format!("{}\n", digit.repeat(32)).into_bytes())
        }
        fn reset_failed(&mut self, unit: &str, budget: Duration) -> Result<()> {
            if self.call(format!("reset-failed {unit}"), budget) { Ok(()) } else { Err(Error::Maintenance) }
        }
        fn restart_queued(&mut self, unit: &str, budget: Duration) -> Result<()> {
            if !self.call(format!("--no-block restart {unit}"), budget) { return Err(Error::Maintenance); }
            self.restarted = true;
            Ok(())
        }
        fn active(&mut self, unit: &str, budget: Duration) -> bool {
            self.call(format!("is-active {unit}"), budget) && (self.always_active || self.fresh())
        }
        fn job_pending(&mut self, unit: &str, budget: Duration) -> Result<bool> {
            if !self.call(format!("job {unit}"), budget) { return Err(Error::Maintenance); }
            if self.restarted { self.polls += 1; }
            Ok(self.jobs.iter().any(|job| *job == unit) || (self.restarted && !self.settled()))
        }
        fn stop(&mut self, unit: &str, budget: Duration) -> Result<()> {
            if !self.call(format!("stop {unit}"), budget) || self.stop_fails == Some(unit) { return Err(Error::Maintenance); }
            Ok(())
        }
        fn active_state(&mut self, unit: &str, budget: Duration) -> Result<Vec<u8>> {
            if !self.call(format!("active-state {unit}"), budget) { return Err(Error::Maintenance); }
            let state = self.states.iter().find(|(name, _)| *name == unit).map_or("inactive", |(_, state)| *state);
            Ok(format!("{state}\n").into_bytes())
        }
    }
    fn queued(units: &FakeUnits) -> Vec<String> {
        units.calls().into_iter().filter(|call| call.starts_with("reset-failed ") || call.starts_with("--no-block ")).collect()
    }
    #[test]
    fn restart_queues_jobs_and_waits_for_new_invocations() {
        let mut units = FakeUnits { settles_after: Some(7), ..FakeUnits::new() };
        restart_units(&mut units, Duration::from_secs(5), Duration::from_millis(1)).unwrap();
        // The invocations before, then the start-rate flush of spawnd, then
        // non-blocking restarts only: PID 1 owns the jobs, no client is killed.
        assert_eq!(units.calls().into_iter().take(3).collect::<Vec<_>>(),
            ["invocation duduclaw-egress.service", "invocation duduclaw-spawnd.service", "invocation duduclaw-gateway.service"]);
        assert_eq!(queued(&units), ["reset-failed duduclaw-spawnd.service", "--no-block restart duduclaw-spawnd.service",
            "--no-block restart duduclaw-egress.service", "--no-block restart duduclaw-gateway.service"]);
        // Accepted only after a poll that found no job, a new invocation and active for every unit.
        assert_eq!(units.calls().iter().rev().take(9).rev().map(String::as_str).collect::<Vec<_>>(),
            ["job duduclaw-egress.service", "invocation duduclaw-egress.service", "is-active duduclaw-egress.service",
                "job duduclaw-spawnd.service", "invocation duduclaw-spawnd.service", "is-active duduclaw-spawnd.service",
                "job duduclaw-gateway.service", "invocation duduclaw-gateway.service", "is-active duduclaw-gateway.service"]);
    }
    #[test]
    fn restart_rejects_active_new_invocation_while_the_job_is_pending() {
        // An automatic restart finished in between: active under a new
        // invocation from the first poll on, but the explicit job is pending.
        let mut units = FakeUnits { early_fresh: true, settles_after: Some(20), ..FakeUnits::new() };
        restart_units(&mut units, Duration::from_secs(5), Duration::from_millis(1)).unwrap();
        // Not accepted before the job left PID 1's queue: every earlier poll
        // stopped at the pending job; only the accepting poll read the rest.
        assert!(units.polls >= 20);
        assert_eq!(units.calls().iter().filter(|call| call.starts_with("is-active ")).count(), 3);
        // A job that never leaves: never accepted, even though active + new invocation.
        let mut units = FakeUnits { early_fresh: true, always_active: true, ..FakeUnits::new() };
        assert!(matches!(restart_units(&mut units, Duration::from_secs(5), Duration::from_millis(100)), Err(Error::Maintenance)));
        // A job read that fails is not "no job" either.
        assert!(parse_job(b"").is_ok_and(|pending| !pending) && parse_job(b"\n").is_ok_and(|pending| !pending));
        assert!(parse_job(b"4711\n").unwrap());
        for uncertain in [&b"0\n"[..], &b"n/a\n"[..], &b"4711 start\n"[..], &b"-1\n"[..], &b"99999999999\n"[..]] {
            assert!(parse_job(uncertain).is_err());
        }
    }
    #[test]
    fn restart_timeout_is_one_shared_deadline_and_recoverable() {
        // Active under the old invocation (the restart job has not run yet) is
        // not a restart; never settling ends at the deadline, not before.
        for always_active in [false, true] {
            let mut units = FakeUnits { always_active, ..FakeUnits::new() };
            let error = restart_units(&mut units, Duration::from_millis(60), Duration::from_millis(5)).unwrap_err();
            assert!(matches!(error, Error::Maintenance));
            assert_eq!(units.clock, Duration::from_millis(60));
            // The jobs were queued once and left to PID 1; the next attempt
            // repeats the idempotent sequence (nothing is resumed here).
            assert_eq!(queued(&units).len(), 4);
        }
        // The production deadline outlasts spawnd's whole stop and start budget.
        assert!(RESTART_DEADLINE > Duration::from_secs(SPAWND_STOP_TIMEOUT_SECS + SPAWND_START_TIMEOUT_SECS));
        assert_eq!((SPAWND_START_TIMEOUT_SECS, SPAWND_STOP_TIMEOUT_SECS), (300, 90));
    }
    #[test]
    fn restart_deadline_strictly_bounds_every_call() {
        // Slow PID 1 calls (7 s each) against a 100 s deadline: every call gets
        // only the time left, the last one is cut short, and the total never
        // exceeds the deadline.
        let deadline = Duration::from_secs(100);
        let mut units = FakeUnits { cost: Duration::from_secs(7), ..FakeUnits::new() };
        assert!(matches!(restart_units(&mut units, deadline, Duration::from_millis(500)), Err(Error::Maintenance)));
        assert!(units.clock <= deadline, "{:?}", units.clock);
        assert!(units.budgets.iter().any(|budget| *budget < Duration::from_secs(7)));
        // A call slower than the per-call bound is killed at 60 s, never later.
        let mut units = FakeUnits { cost: Duration::from_secs(90), ..FakeUnits::new() };
        assert!(matches!(restart_units(&mut units, RESTART_DEADLINE, Duration::from_millis(500)), Err(Error::Maintenance)));
        assert_eq!((units.clock, units.budgets.as_slice()), (SYSTEMCTL_BOUND, [SYSTEMCTL_BOUND].as_slice()));
        // Nothing left: Maintenance at once, nothing is called.
        let mut units = FakeUnits::new();
        assert!(matches!(restart_units(&mut units, Duration::ZERO, Duration::from_millis(500)), Err(Error::Maintenance)));
        assert!(units.calls().is_empty());
        assert!(matches!(quiesce_units(&mut units, Duration::ZERO), Err(Error::Maintenance)));
        assert!(units.calls().is_empty());
        assert!(matches!(systemctl_within(&["is-active", "--quiet", "duduclaw-spawnd.service"], false, Duration::ZERO),
            Err(Error::Maintenance)));
    }

    fn manifest() -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            minimum_version: ReleaseVersion { major: 1, minor: 79, patch: 0 }, serial: 1, channel: ReleaseChannel::Stable,
            arch: Architecture::X86_64, os: OperatingSystem::Linux, created_at_unix: 1, expires_at_unix: 2,
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1, mcp_bridge: 1,
                parse_worker: 1, importer: 1 }, artifacts: vec![], migrations: vec![] }
    }
    /// The activation sequence up to `Apply` over one log: the runtime (spawnd's
    /// endpoint) and the effects, whose quiesce is the production
    /// [`quiesce_units`] over the scripted PID 1. `Apply` ends the run.
    struct Runtime(Log);
    impl GenerationPreparation for Runtime {
        fn preflight(&mut self, _: &Preparation) -> Result<()> { self.0.borrow_mut().push("preflight".into()); Ok(()) }
        fn apply(&mut self, _: &Preparation) -> Result<()> { self.0.borrow_mut().push("apply".into()); Err(Error::Maintenance) }
    }
    impl ActivationRuntime for Runtime {
        fn drain(&mut self, _: Sha256Digest) -> Result<()> { self.0.borrow_mut().push("drain".into()); Ok(()) }
        fn read_back(&mut self, _: Sha256Digest) -> Result<Vec<ComponentReadback>> { unreachable!("after apply") }
        fn resume(&mut self, _: Sha256Digest) -> Result<()> { unreachable!("never resumed here") }
    }
    struct Effects(FakeUnits);
    impl GenerationEffects for Effects {
        fn verify(&mut self, _: &ReleaseManifest, _: Sha256Digest) -> Result<()> { self.0.log.borrow_mut().push("verify".into()); Ok(()) }
        fn quiesce(&mut self) -> Result<()> { quiesce_units(&mut self.0, QUIESCE_DEADLINE) }
        fn publish(&mut self, _: Sha256Digest) -> Result<()> { unreachable!("after apply") }
        fn restart(&mut self) -> Result<()> { unreachable!("after apply") }
        fn processes(&mut self, _: &ReleaseManifest) -> Result<Vec<u32>> { unreachable!("after apply") }
        fn current(&mut self) -> Result<Sha256Digest> { unreachable!("after apply") }
        fn sync(&mut self) -> Result<()> { unreachable!("after apply") }
    }
    fn run_until_apply(units: FakeUnits) -> (Result<()>, Vec<String>) {
        let log = units.log.clone();
        let result = activation_contract::activate_generation(&mut Runtime(log.clone()), &mut Effects(units), &manifest(),
            Sha256Digest([9; 32]));
        let entries = log.borrow().clone();
        (result, entries)
    }
    #[test]
    fn activation_stops_the_gate_and_verifies_it_down_before_apply() {
        let (_, log) = run_until_apply(FakeUnits::new());
        let mut expected: Vec<String> = ["verify", "preflight", "drain", "stop duduclaw-isolation-ready.service",
            "stop duduclaw-egress.socket", "stop duduclaw-egress.service", "stop duduclaw-gateway.service"]
            .into_iter().map(String::from).collect();
        for unit in ISOLATION_GATED_UNITS { expected.extend([format!("active-state {unit}"), format!("job {unit}")]); }
        expected.push("apply".into());
        assert_eq!(log, expected);
        // The gate goes first: its stop takes egress and the gateway down (BindsTo=).
        assert_eq!(ISOLATION_GATED_UNITS[0], "duduclaw-isolation-ready.service");
    }
    #[test]
    fn activation_aborts_before_apply_when_the_gate_cannot_be_confirmed_down() {
        let cases: [(&str, fn(&mut FakeUnits)); 6] = [
            ("gate stop fails", |units| units.stop_fails = Some("duduclaw-isolation-ready.service")),
            ("gateway still active", |units| units.states = vec![("duduclaw-gateway.service", "active")]),
            ("egress stopping", |units| units.states = vec![("duduclaw-egress.service", "deactivating")]),
            ("socket listening", |units| units.states = vec![("duduclaw-egress.socket", "listening")]),
            ("gate start queued", |units| units.jobs = vec!["duduclaw-isolation-ready.service"]),
            ("PID 1 too slow", |units| units.cost = QUIESCE_DEADLINE),
        ];
        for (case, setup) in cases {
            let mut units = FakeUnits::new();
            setup(&mut units);
            let (result, log) = run_until_apply(units);
            // Recoverable: Maintenance before anything was applied or published.
            assert!(matches!(result, Err(Error::Maintenance)), "{case}");
            assert!(!log.iter().any(|entry| entry == "apply"), "{case}: {log:?}");
            assert_eq!(log.first().map(String::as_str), Some("verify"), "{case}");
        }
        // `failed` is down (no process, no job), like `inactive`.
        let mut units = FakeUnits::new();
        units.states = vec![("duduclaw-gateway.service", "failed")];
        assert_eq!(run_until_apply(units).1.last().map(String::as_str), Some("apply"));
    }
    #[test]
    #[ignore = "Requires root Linux openat2; isolated /run tree, no production selector is touched"]
    fn iso_root_current_publication_recovers_before_and_after_atomic_rename() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("duduclaw-current-test-").tempdir_in("/run").unwrap();
        let install = Directory::open(temp.path()).unwrap();
        let generations = install.create_child("generations").unwrap();
        let old = Sha256Digest([1; 32]); let next = Sha256Digest([2; 32]);
        generations.create_child(&hex::encode(old.0)).unwrap(); generations.create_child(&hex::encode(next.0)).unwrap();
        symlink(format!("generations/{}", hex::encode(old.0)), temp.path().join("current")).unwrap();
        install.sync().unwrap();
        // A crash after creating the temporary selector leaves current intact.
        symlink(format!("generations/{}", hex::encode(next.0)), temp.path().join("current.next")).unwrap();
        install.sync().unwrap(); assert_eq!(read_current_at(&install).unwrap(), old);
        publish_current(&install, old, next).unwrap(); assert_eq!(read_current_at(&install).unwrap(), next);
        // A crash after rename is resumed without another generation switch.
        publish_current(&install, old, next).unwrap(); assert_eq!(read_current_at(&install).unwrap(), next);
        assert!(publish_current(&install, old, Sha256Digest([3; 32])).is_err());
    }
    #[test]
    #[ignore = "Requires root Linux with PID 1 systemd; creates only a uniquely named temporary test unit"]
    fn iso_root_systemd_restart_and_live_executable_readback() {
        use std::io::Write;
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let unit = format!("duduclaw-update-test-{}.service", std::process::id());
        let directory = Directory::open(Path::new("/run/systemd/system")).unwrap();
        let mut file = directory.create(&unit).unwrap();
        file.write_all(b"[Unit]\nDescription=Isolated updater restart readback test\n[Service]\nType=simple\nExecStart=/usr/bin/sleep infinity\n").unwrap();
        file.sync_all().unwrap(); directory.sync().unwrap();
        struct Cleanup<'a> { directory: &'a Directory, unit: &'a str }
        impl Drop for Cleanup<'_> { fn drop(&mut self) {
            let _ = systemctl(&["stop", self.unit], false); let _ = self.directory.remove(self.unit);
            let _ = systemctl(&["daemon-reload"], false);
        } }
        let _cleanup = Cleanup { directory: &directory, unit: &unit };
        systemctl(&["daemon-reload"], false).unwrap();
        systemctl(&["start", &unit], false).unwrap(); let old = main_pid(&unit).unwrap();
        systemctl(&["restart", &unit], false).unwrap(); let new = main_pid(&unit).unwrap();
        assert_ne!(old, new); systemctl(&["is-active", "--quiet", &unit], false).unwrap();
        let expected = File::open("/usr/bin/sleep").unwrap().metadata().unwrap();
        let actual = File::open(format!("/proc/{new}/exe")).unwrap().metadata().unwrap();
        assert_eq!((actual.dev(), actual.ino()), (expected.dev(), expected.ino()));
        // The readbacks restart and quiesce rely on: no job once a restart
        // finished, `active`; a stop leaves `inactive` and no job; a unit PID 1
        // never loaded reads as `inactive` with no job.
        let budget = Duration::from_secs(10);
        assert!(!Systemctl.job_pending(&unit, budget).unwrap());
        assert_eq!(Systemctl.active_state(&unit, budget).unwrap(), b"active\n");
        Systemctl.stop(&unit, budget).unwrap();
        assert_eq!(Systemctl.active_state(&unit, budget).unwrap(), b"inactive\n");
        assert!(!Systemctl.job_pending(&unit, budget).unwrap());
        let never = format!("duduclaw-update-test-never-{}.service", std::process::id());
        assert_eq!(Systemctl.active_state(&never, budget).unwrap(), b"inactive\n");
        assert!(!Systemctl.job_pending(&never, budget).unwrap());
        // A queued restart is pending until it ran, then gone.
        Systemctl.restart_queued(&unit, budget).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while Systemctl.job_pending(&unit, budget).unwrap() { assert!(Instant::now() < until); std::thread::sleep(RESTART_POLL); }
        assert!(Systemctl.active(&unit, budget));
    }
    #[test]
    #[ignore = "Requires root Linux systemd and /usr/bin/python3; temporary units test the production capability boundary"]
    fn iso_root_systemd_capability_bound_nondumpable_executable_readback() {
        use std::io::Write;
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let prefix = format!("duduclaw-capability-test-{}", std::process::id());
        let target = format!("{prefix}-target.service");
        let denied = format!("{prefix}-denied.service");
        let denied_dac = format!("{prefix}-denied-dac.service");
        let allowed = format!("{prefix}-allowed.service");
        let directory = Directory::open(Path::new("/run/systemd/system")).unwrap();
        struct Cleanup<'a> { directory: &'a Directory, units: Vec<String> }
        impl Drop for Cleanup<'_> { fn drop(&mut self) {
            for unit in &self.units { let _ = systemctl(&["stop", unit], false); let _ = self.directory.remove(unit); }
            let _ = systemctl(&["daemon-reload"], false);
        } }
        let mut cleanup = Cleanup { directory: &directory, units: vec![] };
        let mut file = directory.create(&target).unwrap();
        cleanup.units.push(target.clone());
        // Type=notify makes READY mean the uid drop and non-dumpable flag landed.
        let target_content = format!("[Service]\nType=notify\nUser=65534\nGroup=65534\nNotifyAccess=main\nRuntimeDirectory={prefix}\nRuntimeDirectoryMode=0700\nExecStart=/usr/bin/python3 -c \"import ctypes,os,socket,time; assert ctypes.CDLL(None).prctl(4,0)==0; listener=socket.socket(socket.AF_UNIX,socket.SOCK_SEQPACKET); listener.bind('/run/{prefix}/readback.sock'); os.chmod('/run/{prefix}/readback.sock',384); listener.listen(8); s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM); s.connect(os.environ['NOTIFY_SOCKET']); s.send(b'READY=1'); time.sleep(120)\"\n");
        file.write_all(target_content.as_bytes()).unwrap();
        file.sync_all().unwrap(); directory.sync().unwrap();
        systemctl(&["daemon-reload"], false).unwrap(); systemctl(&["start", &target], false).unwrap();
        let pid = main_pid(&target).unwrap();
        for (unit, capabilities) in [(&denied, "CAP_SETUID CAP_SETGID CAP_CHOWN"),
            (&denied_dac, "CAP_SETUID CAP_SETGID CAP_CHOWN CAP_SYS_PTRACE"),
            (&allowed, "CAP_SETUID CAP_SETGID CAP_CHOWN CAP_SYS_PTRACE CAP_DAC_OVERRIDE")] {
            let content = format!("[Service]\nType=oneshot\nRemainAfterExit=yes\nUser=root\nGroup=root\nNoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=yes\nPrivateTmp=yes\nPrivateDevices=yes\nProtectKernelTunables=yes\nProtectKernelModules=yes\nProtectKernelLogs=yes\nProtectControlGroups=yes\nRestrictSUIDSGID=yes\nLockPersonality=yes\nCapabilityBoundingSet={capabilities}\nExecStart=/usr/bin/python3 -c \"import os,socket,struct; f=os.open('/proc/{pid}/exe',os.O_RDONLY); assert os.read(f,4)==bytes([127,69,76,70]); s=socket.socket(socket.AF_UNIX,socket.SOCK_SEQPACKET); s.connect('/run/{prefix}/readback.sock'); assert struct.unpack('3i',s.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12))[1:]==(65534,65534)\"\n");
            let mut file = directory.create(unit).unwrap(); cleanup.units.push(unit.clone());
            file.write_all(content.as_bytes()).unwrap(); file.sync_all().unwrap();
        }
        directory.sync().unwrap(); systemctl(&["daemon-reload"], false).unwrap();
        assert!(systemctl(&["start", &denied], false).is_err());
        assert!(systemctl(&["start", &denied_dac], false).is_err());
        systemctl(&["start", &allowed], false).unwrap();
        systemctl(&["is-active", "--quiet", &allowed], false).unwrap();
    }
}
