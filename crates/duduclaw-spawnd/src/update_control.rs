//! R6 root update control endpoint `/run/duduclaw-spawnd/update.sock`: the
//! server of `duduclaw_updated::linux::activation::SocketRuntime` (plan §4.6).
//!
//! Wire: the `activation_contract` types, one `SOCK_SEQPACKET` frame per
//! connection, no ancillary data, 5 s timeouts (the client's own transport
//! functions are reused, so both ends apply the same framing rules). Only a
//! uid 0 / gid 0 peer (`SO_PEERCRED`) is answered; anything else is dropped
//! without a reply.
//!
//! Drain barrier (per generation, `admission::DrainBarrier`):
//! - `Drain{g}` first writes `/var/lib/duduclaw-spawnd/drain.json` durably
//!   (temporary file, fsync, rename, directory fsync), then closes the barrier,
//!   then replies with the admitted-run count of an admission iteration that
//!   began after the closure. New `StartRun`s are refused; queued and running
//!   runs are kept. A failed write replies `Refused` and announces nothing.
//! - The record is read strictly when spawnd starts, before any admission: a
//!   valid record closes the barrier for its generation, an unreadable one
//!   closes it with an unknown generation (only a new `Drain` re-keys it).
//! - `Resume{g}` is the only way to lift it: when the barrier and the record
//!   both name `g` (or nothing) and the publication journal is absent or
//!   `published` for `g`, the journal and then the record are removed durably
//!   first, then admission reopens.
//!
//! `Apply` publishes only from the verified generation directory, hashing each
//! artifact from its descriptor while copying it, and only under the closed,
//! recorded barrier of that generation with no admitted run and a healthy
//! admission iteration, and only when the workload census ([`Workloads`], from
//! PID 1 and cgroupfs, never this process's memory) finds no live run unit and
//! an empty employees cgroup. Without an admission loop (set not consistent at
//! start) the census alone answers `Drain` and gates `Apply`: runs of an
//! earlier spawnd process may still be tearing down. A census that cannot be
//! completed refuses. After the generation verified and before its journal is
//! written, `Apply` also requires PID 1 (same bus source) to report the
//! consistency gate, the units bound to it and egress's socket ([`GATED_UNITS`])
//! inactive with no job; an open process then says `recovering` (status file
//! written first, or refused) for the rest of its life, so the gate the updater
//! stopped is re-armed only by the restarted, re-verified spawnd.
//!
//! Publication journal (`/var/lib/duduclaw-spawnd/publish.json`, format in
//! `duduclaw_updated::publish_journal`, plan R6 addendum): one rename per file is
//! atomic, the set of eight is not. So `Apply` first verifies the whole
//! generation, then durably writes `{generation, manifest_sha256, previous,
//! publishing}` and reads it back, then replaces the components, syncs every
//! destination file and directory, then rewrites the journal as `published`.
//! The journal is removed only by the `Resume` of its generation (the updater
//! resumes after it committed or rolled back), so the previous generation stays
//! known and retained until then (the updater's pruning keeps both names).
//!
//! Start (plan R6 addendum "啟動順序與「一致」的判定"): [`recover`] runs before
//! any socket is bound and before admission, and verifies the installed set
//! EVERY time: whatever the journal says (`publishing` or `published`), its
//! generation is re-verified against the trust ledger (including revocation),
//! its manifest digest must be the journal's, and all eight destinations are
//! republished idempotently; on failure the journal's `previous` is verified
//! and published with the same procedure. Without a journal, the generation
//! the `current` selector names is verified the same way and every destination
//! must already hold its bytes. The set is then consistent or not ([`start`]):
//! - `READY=1` is always sent once the root-only update endpoint is bound: it
//!   means only "the update endpoint is available" (the updater depends on it).
//! - Not consistent: nothing else is opened (no allocation socket, no control
//!   listener, no admission loop); admission is closed ([`DrainClosure::Recovery`]).
//!   A successful `Apply` repairs the set; spawnd then replies, flushes its
//!   status and exits with [`RESTART_AFTER_REPAIR_EXIT`] so systemd restarts it
//!   and the fresh start verifies everything before opening. It never switches
//!   to an open state in-process.
//! - Consistent: everything is opened, then the root-only status file
//!   `/run/duduclaw-spawnd/update-status.json` says `consistent` for this
//!   process (boot id, pid, systemd invocation id): `duduclaw-spawnd
//!   wait-consistent` ([`wait_consistent`], unit
//!   `duduclaw-isolation-ready.service`) completes only then, and gateway and
//!   egress are bound to and ordered after that unit. The unit is bound to
//!   spawnd's lifetime (see the unit file), so it is never a one-shot latch.
//! - A fully open process whose set later stops being consistent (an `Apply`
//!   that failed after its journal said `publishing`, or a journal that no
//!   longer reads) closes admission ([`DrainClosure::Recovery`]) before it
//!   replies, then writes its status (`inconsistent`), audits the event
//!   synchronously and exits with [`EXIT_SET_INCONSISTENT`]: systemd stops the
//!   gate and its dependents and restarts spawnd, whose start-up recovery
//!   completes or rolls back the publication.
//! Every state change is also one fixed-vocabulary `STATUS=` line to systemd
//! and stderr (journal): `consistent`, `recovering`, `inconsistent: <reason>`,
//! with `; status_file_write_failed` / `; audit_write_failed` appended when the
//! status file or a synchronously written audit record did not land.
#![allow(unsafe_code)]
use std::{fs::File, io::{self, Read}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt, PermissionsExt}},
    path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, Instant}};
use duduclaw_isolation::{ids::Sha256Digest, update::{InstallTarget, Product, ReleaseArtifact, ReleaseManifest, ReleaseVersion}};
use duduclaw_updated::{activation_contract::{ComponentReadback, ControlCommand, ControlOutcome, ControlRequest, ControlResponse, Preparation},
    linux::{activation::SPAWND_UPDATE_SOCKET, files::{copy_artifact, verify_fd, Directory}, transport, INSTALL_ROOT},
    publication::artifact_name,
    publish_journal::{PublishJournal, PublishState, JOURNAL_FILE, MAX_JOURNAL_BYTES}};
use nix::{errno::Errno, sys::socket::{self, sockopt, Backlog, SockFlag, UnixAddr}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{admission::{DrainBarrier, DrainClosure}, audit::{Detail, Event, PublicationRecovery, Record}};

pub const DRAIN_ROOT: &str = "/var/lib/duduclaw-spawnd";
pub const DRAIN_RECORD: &str = "drain.json";
/// Root-only (0600, atomic replace) operator status of the barrier and journal.
pub const STATUS_ROOT: &str = "/run/duduclaw-spawnd";
pub const STATUS_FILE: &str = "update-status.json";
/// Schema of the status file (3: `owner` names the systemd invocation id, not
/// a `/proc` start time).
const STATUS_SCHEMA: u16 = 3;
const MAX_STATUS_BYTES: usize = 16 * 1024;
/// spawnd's exit status after an `Apply` repaired a set it started on
/// inconsistent. The unit restarts exactly this status
/// (`RestartForceExitStatus=`, `SuccessExitStatus=`); see the unit file.
pub const RESTART_AFTER_REPAIR_EXIT: i32 = 82;
/// spawnd's exit status when a fully open process finds its set no longer
/// consistent (failed or partial `Apply`). A failure: `Restart=on-failure`
/// restarts it (counted against the crash-loop limit), and the start-up
/// recovery completes or rolls back the publication.
pub const EXIT_SET_INCONSISTENT: i32 = 83;
/// Audit rate-limit source of this endpoint (0: control socket, 1: host).
const AUDIT_SOURCE: u8 = 2;
/// Longest wait for the audit worker to confirm a synchronously written record.
const AUDIT_ACKNOWLEDGEMENT: Duration = Duration::from_secs(5);
/// The cgroup v2 mount and spawnd's employees cgroup below it (every run unit
/// lives in `duduclaw-employees-<id>.slice` under it; see `systemd::employee_slice`).
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const EMPLOYEES_CGROUP_EVENTS: &str = "duduclaw.slice/duduclaw-employees.slice/cgroup.events";
/// `statfs(2)` `f_type` of the unified cgroup v2 hierarchy.
const CGROUP2_SUPER_MAGIC: i64 = 0x6367_7270;
const SCHEMA: u16 = 1;
const RECORD_VERSION: u16 = 1;
const MAX_RECORD_BYTES: usize = 4096;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
/// Bound of one hashed executable (spawnd's own image, a published component).
const MAX_EXECUTABLE_BYTES: u64 = 768 * 1024 * 1024;
/// Longest wait for one admission-loop iteration (the loop runs every 25 ms).
const OBSERVATION_WAIT: Duration = Duration::from_secs(2);
/// spawnd reports itself ready only with an admission iteration this recent.
const LIVE_WINDOW: Duration = Duration::from_secs(2);
/// Every published component is built for this product (the only product the
/// release pipeline signs, `publication::validate_bootstrap_manifest`).
const PRODUCT: Product = Product::Duduclaw;

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "update control refused") }
fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "update control peer denied") }
fn timed_out() -> io::Error { io::Error::from(io::ErrorKind::TimedOut) }
fn missing(error: &io::Error) -> bool { error.kind() == io::ErrorKind::NotFound }
fn updated(error: duduclaw_updated::Error) -> io::Error {
    match error { duduclaw_updated::Error::Io(error) => error, _ => invalid() }
}
fn hex(digest: &Sha256Digest) -> String { digest.0.iter().map(|byte| format!("{byte:02x}")).collect() }

/// The durable barrier record. Written only by [`DiskDrainStore::persist`];
/// accepted on read only in exactly the bytes that write produces.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DrainRecord { version: u16, generation: Sha256Digest }
pub(crate) fn encode_record(generation: Sha256Digest) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&DrainRecord { version: RECORD_VERSION, generation }).map_err(|_| invalid())
}
/// Strict: bounded, known fields only, current version, non-zero generation,
/// and byte-for-byte the canonical encoding (no whitespace, order or duplicates).
pub(crate) fn parse_record(bytes: &[u8]) -> io::Result<Sha256Digest> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES { return Err(invalid()); }
    let record: DrainRecord = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if record.version != RECORD_VERSION || record.generation.0 == [0; 32]
        || encode_record(record.generation)?.as_slice() != bytes { return Err(invalid()); }
    Ok(record.generation)
}

/// Durable storage of the drain record.
pub trait DrainStore {
    /// `Ok(None)`: no record. `Err`: a record (or its directory) is unreadable or invalid.
    fn load(&mut self) -> io::Result<Option<Sha256Digest>>;
    /// The record names `generation` on disk (file and directory synced) before `Ok`.
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()>;
    /// The record is gone on disk (directory synced) before `Ok`; absent is success.
    fn clear(&mut self) -> io::Result<()>;
}

/// `/var/lib/duduclaw-spawnd/drain.json`, root 0600, through a dirfd whose every
/// ancestor is root-owned, not group/other-writable and not a symlink.
pub struct DiskDrainStore { root: PathBuf }
impl DiskDrainStore {
    pub fn production() -> Self { Self { root: PathBuf::from(DRAIN_ROOT) } }
    #[cfg(test)]
    fn at(root: &Path) -> Self { Self { root: root.to_path_buf() } }
    fn directory(&self) -> io::Result<Directory> { Directory::open(&self.root).map_err(updated) }
}
impl DrainStore for DiskDrainStore {
    fn load(&mut self) -> io::Result<Option<Sha256Digest>> {
        // Directory::bytes opens O_NOFOLLOW below the dirfd and requires a root
        // 0600-style regular file with one link; a link or foreign file is refused.
        match self.directory()?.bytes(DRAIN_RECORD, MAX_RECORD_BYTES) {
            Ok(bytes) => parse_record(&bytes).map(Some),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => Ok(None),
            Err(error) => Err(updated(error)),
        }
    }
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()> {
        let directory = self.directory()?;
        // Unlink a stale temporary, create it O_EXCL 0600, write, fsync, rename
        // over the record, fsync the directory (`Directory::atomic_json`).
        directory.atomic_json(DRAIN_RECORD, &DrainRecord { version: RECORD_VERSION, generation }).map_err(updated)?;
        // The barrier is announced only for the bytes that actually landed.
        if parse_record(&directory.bytes(DRAIN_RECORD, MAX_RECORD_BYTES).map_err(updated)?)? != generation {
            return Err(invalid());
        }
        Ok(())
    }
    fn clear(&mut self) -> io::Result<()> { self.remove_durably(DRAIN_RECORD) }
}
impl DiskDrainStore {
    fn remove_durably(&self, name: &str) -> io::Result<()> {
        let directory = self.directory()?;
        match directory.remove(name) {
            // unlinkat, then the directory is synced by `remove`.
            Ok(()) => Ok(()),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => directory.sync().map_err(updated),
            Err(error) => Err(updated(error)),
        }
    }
}

/// Durable storage of the publication journal (`publish.json`, next to the
/// drain record).
pub trait JournalStore {
    /// `Ok(None)`: no journal. `Err`: unreadable, unknown schema or field.
    fn load_journal(&mut self) -> io::Result<Option<PublishJournal>>;
    /// The journal is on disk (file and directory synced) and reads back equal before `Ok`.
    fn persist_journal(&mut self, journal: &PublishJournal) -> io::Result<()>;
    /// The journal is gone on disk (directory synced) before `Ok`; absent is success.
    fn clear_journal(&mut self) -> io::Result<()>;
}
impl JournalStore for DiskDrainStore {
    fn load_journal(&mut self) -> io::Result<Option<PublishJournal>> {
        match self.directory()?.bytes(JOURNAL_FILE, MAX_JOURNAL_BYTES) {
            Ok(bytes) => PublishJournal::parse(&bytes).map(Some).map_err(|_| invalid()),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => Ok(None),
            Err(error) => Err(updated(error)),
        }
    }
    fn persist_journal(&mut self, journal: &PublishJournal) -> io::Result<()> {
        // Only a journal that encodes canonically is written (`atomic_json` uses
        // the same serde_json encoding): temporary O_EXCL 0600, fsync, rename,
        // directory fsync; then the landed bytes are parsed strictly.
        journal.encode().map_err(|_| invalid())?;
        let directory = self.directory()?;
        directory.atomic_json(JOURNAL_FILE, journal).map_err(updated)?;
        let landed = PublishJournal::parse(&directory.bytes(JOURNAL_FILE, MAX_JOURNAL_BYTES).map_err(updated)?)
            .map_err(|_| invalid())?;
        if landed != *journal { return Err(invalid()); }
        Ok(())
    }
    fn clear_journal(&mut self) -> io::Result<()> { self.remove_durably(JOURNAL_FILE) }
}

/// Generation effects behind the control protocol (production: [`InstalledEffects`]).
pub trait Effects {
    fn prepare(&mut self, preparation: &Preparation) -> io::Result<()>;
    /// Complete verification of a staged generation: manifest digest, manifest
    /// signature against the root trust ledger, one artifact per fixed
    /// destination, every artifact from its descriptor, no data migration; with
    /// a request, also that it is exactly that manifest's preparation. Returns
    /// the SHA-256 of the manifest bytes.
    fn verify(&mut self, generation: Sha256Digest, preparation: Option<&Preparation>) -> io::Result<Sha256Digest>;
    /// Replace the eight fixed components with `generation`'s (each hashed from
    /// its descriptor while copied; a destination already holding exactly the
    /// artifact's bytes is kept), then sync every destination file and directory.
    /// Idempotent; the caller journals it.
    fn publish(&mut self, generation: Sha256Digest) -> io::Result<()>;
    /// Read-only: every fixed destination already holds exactly `generation`'s
    /// artifact (root-owned single-link regular file, mode 0755, size and
    /// SHA-256 by descriptor). Call only after [`Self::verify`] of `generation`.
    fn published(&mut self, generation: Sha256Digest) -> io::Result<()>;
    /// The generation the install selector (`current`) names; `None`: no selector.
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>>;
    /// `live`: this spawnd's admission loop completed a healthy iteration recently.
    fn readback(&mut self, generation: Sha256Digest, live: bool) -> io::Result<Vec<ComponentReadback>>;
    /// Employee workloads as PID 1 and cgroupfs see them now, whichever spawnd
    /// process started them. `Err`: the census could not be completed.
    fn workloads(&mut self) -> io::Result<Workloads>;
    /// From the census's source (PID 1 over the system bus): the consistency
    /// gate and the units bound to it ([`GATED_UNITS`]) are all inactive with no
    /// pending job. `false`: one is not; `Err`: the answer is uncertain.
    fn gated_units_down(&mut self) -> io::Result<bool>;
}
/// The consistency gate, the units bound to it and egress's activating socket
/// (fixed names, `duduclaw_isolation::ids`).
pub const GATED_UNITS: [&str; 4] = duduclaw_isolation::ids::ISOLATION_GATED_UNITS;

/// The workload census behind `Drain` (no admission loop) and `Apply`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Workloads {
    /// Run units (`duduclaw-run-<id>.service`) PID 1 lists in a live state.
    pub live_run_units: u32,
    /// `duduclaw.slice/duduclaw-employees.slice` still holds a process.
    pub employees_cgroup_populated: bool,
}
impl Workloads {
    /// No run unit is live and every run cgroup is empty or gone.
    pub fn idle(self) -> bool { self.live_run_units == 0 && !self.employees_cgroup_populated }
}
/// `true` when the employees cgroup below `root` holds a process, `false`
/// when it is empty or gone. `root` must be the unified cgroup v2 hierarchy
/// (`cgroup.controllers` is a file there); anything else cannot be counted and
/// is an error, as is an unreadable or malformed `cgroup.events`.
pub(crate) fn employees_cgroup_populated(root: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(root.join("cgroup.controllers")) {
        Ok(metadata) if metadata.is_file() => {}
        _ => return Err(invalid()),
    }
    match std::fs::read_to_string(root.join(EMPLOYEES_CGROUP_EVENTS)) {
        Ok(events) => Ok(!crate::systemd::cgroup_empty(&events)?),
        // No employees slice (or none of its parents): nothing can run there.
        Err(error) if missing(&error) => Ok(false),
        Err(error) => Err(error),
    }
}
/// Production census: PID 1's unit list over the system bus, then cgroupfs.
fn system_census() -> io::Result<Workloads> {
    let systemd = crate::systemd::linux::Systemd::connect()?;
    let live_run_units = systemd.live_run_units()?;
    Ok(Workloads { live_run_units, employees_cgroup_populated: employees_cgroup_populated(Path::new(CGROUP_ROOT))? })
}
/// Production gate check: PID 1's filtered unit list over the system bus.
fn system_gated_units_down() -> io::Result<bool> {
    crate::systemd::linux::Systemd::connect()?.units_down(&GATED_UNITS)
}

/// What [`UpdateControl::new`] found and did at start (audit and status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Startup {
    /// The barrier as restored from `drain.json`, before any recovery closure.
    pub drain: Option<DrainClosure>,
    pub recovery: PublicationRecovery,
    /// The journal as found (`None`: absent or unreadable).
    pub journal: Option<PublishJournal>,
    /// Without a journal: the generation the `current` selector named.
    pub selected: Option<Sha256Digest>,
}
impl Startup {
    /// At most two bounded, content-free records: the barrier restored from a
    /// record (its generation, or `None` for an unreadable record) and any
    /// journal found at start with the action taken.
    pub fn audit_records(&self, at_unix_ms: u64) -> Vec<Record> {
        let record = |event: Event, detail: Detail| Record { at_unix_ms, event, employee: None, run: None, suppressed: 0,
            detail: Some(detail) };
        let mut records = Vec::new();
        match self.drain {
            None => {}
            Some(DrainClosure::Generation(generation)) =>
                records.push(record(Event::UpdateBarrierRestored, Detail::UpdateBarrier { generation: Some(generation) })),
            Some(_) => records.push(record(Event::UpdateBarrierRestored, Detail::UpdateBarrier { generation: None })),
        }
        if self.recovery != PublicationRecovery::NoJournal {
            records.push(record(Event::PublicationRecovery, Detail::PublicationRecovery { action: self.recovery,
                generation: self.journal.map(|journal| journal.generation).or(self.selected),
                previous: self.journal.and_then(|journal| journal.previous) }));
        }
        records
    }
}

/// The process a status file belongs to: this boot, its pid and the systemd
/// invocation id of its service start (`$INVOCATION_ID`, 128 random bits that
/// PID 1 assigns to every start of the unit). A status whose owner is not the
/// unit's current main process and invocation is stale; a recycled pid never
/// matches a new invocation. Nothing here needs `/proc/<pid>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner { pub boot_id: String, pub pid: u32, pub invocation_id: String }
impl Owner {
    /// `Err` outside a systemd service start (no valid `$INVOCATION_ID`): the
    /// status then has no owner and the waiter never accepts it.
    pub(crate) fn current() -> io::Result<Self> {
        let invocation_id = std::env::var("INVOCATION_ID").map_err(|_| invalid())?;
        if !invocation_id_valid(&invocation_id) { return Err(invalid()); }
        Ok(Self { boot_id: boot_id()?, pid: std::process::id(), invocation_id })
    }
}
/// 32 lowercase hex digits, not all zero (systemd's `sd_id128` text form).
fn invocation_id_valid(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && id.bytes().any(|byte| byte != b'0')
}
fn boot_id() -> io::Result<String> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let id = text.trim();
    if id.len() != 36 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') { return Err(invalid()); }
    Ok(id.to_owned())
}

/// Root-only operator status (`/run/duduclaw-spawnd/update-status.json`):
/// rewritten at start, when everything is opened, and after every served
/// request. Digests are lowercase hex.
#[derive(Debug, Serialize)]
pub(crate) struct UpdateStatus {
    schema: u16,
    /// `consistent` (verified at this process's start and fully opened, journal
    /// absent or `published`), `recovering` (verified, not yet opened; a
    /// repaired set waiting for the restart; or an open process that accepted
    /// an `Apply` and waits for its restart) or `inconsistent` (also: an opened
    /// set a later request left inconsistent; this process is exiting).
    set: &'static str,
    /// With `inconsistent`: the fixed reason code.
    closed_reason: Option<&'static str>,
    /// The process this status belongs to (`None`: could not be read; the
    /// waiter then never accepts it).
    owner: Option<Owner>,
    /// New runs are refused (any barrier closure).
    admission_closed: bool,
    /// `open`, `drained` (closed for `barrier_generation`), `unreadable_record`
    /// or `recovery_incomplete`.
    barrier: &'static str,
    barrier_generation: Option<String>,
    /// `absent`, `publishing`, `published` or `unreadable`.
    journal: &'static str,
    journal_generation: Option<String>,
    journal_previous: Option<String>,
    /// Every fixed component holds one generation's bytes (journal absent or published).
    publication_consistent: bool,
    /// What `drain.json` said when this spawnd started.
    startup_barrier: &'static str,
    startup_barrier_generation: Option<String>,
    /// What this spawnd did with the journal when it started.
    startup_recovery: PublicationRecovery,
}
fn barrier_words(closure: Option<DrainClosure>) -> (&'static str, Option<String>) {
    match closure {
        None => ("open", None),
        Some(DrainClosure::Generation(generation)) => ("drained", Some(hex(&generation))),
        Some(DrainClosure::Unreadable) => ("unreadable_record", None),
        Some(DrainClosure::Recovery) => ("recovery_incomplete", None),
    }
}
fn write_status(root: &Path, status: &UpdateStatus) -> io::Result<()> {
    Directory::open(root).and_then(|directory| directory.atomic_json(STATUS_FILE, status)).map_err(updated)
}
/// The one-line `STATUS=` text (fixed vocabulary): `consistent`, `recovering`
/// or `inconsistent: <reason>`, with `; status_file_write_failed` appended
/// when the status file could not be written.
pub(crate) fn status_line(set: &str, closed_reason: Option<&str>, status_file_written: bool) -> String {
    let mut line = match closed_reason {
        Some(reason) => format!("{set}: {reason}"),
        None => set.to_owned(),
    };
    if !status_file_written { line.push_str("; status_file_write_failed"); }
    line
}
/// `STATUS=` to systemd and the same line on stderr (the unit routes it to the journal).
fn announce(line: &str) {
    let _ = notify(&format!("STATUS={line}"));
    eprintln!("duduclaw-spawnd: update status: {line}");
}

/// Verify `generation` completely, then: journal `publishing` (durable, read
/// back) → replace the components → journal `published`. Used by `Apply` and
/// by start-up recovery in both directions. `expected_manifest`: the manifest
/// digest the existing journal recorded (roll forward).
fn publish_verified<S: JournalStore, E: Effects>(store: &mut S, effects: &mut E, generation: Sha256Digest,
    expected_manifest: Option<Sha256Digest>, previous: Option<Sha256Digest>, preparation: Option<&Preparation>)
    -> io::Result<()> {
    let manifest = verified_manifest(effects, generation, expected_manifest, preparation)?;
    publish_journaled(store, effects, generation, manifest, previous)
}
/// [`publish_verified`]'s first half: the complete verification, nothing written.
fn verified_manifest<E: Effects>(effects: &mut E, generation: Sha256Digest, expected_manifest: Option<Sha256Digest>,
    preparation: Option<&Preparation>) -> io::Result<Sha256Digest> {
    let manifest = effects.verify(generation, preparation)?;
    if expected_manifest.is_some_and(|expected| expected != manifest) { return Err(invalid()); }
    Ok(manifest)
}
/// [`publish_verified`]'s second half, for a generation just verified.
fn publish_journaled<S: JournalStore, E: Effects>(store: &mut S, effects: &mut E, generation: Sha256Digest,
    manifest: Sha256Digest, previous: Option<Sha256Digest>) -> io::Result<()> {
    let mut journal = PublishJournal::new(generation, manifest, previous, PublishState::Publishing).map_err(|_| invalid())?;
    // 1. Durable before the first destination is touched.
    store.persist_journal(&journal)?;
    // 2. Every destination in place and synced (files and directories).
    effects.publish(generation)?;
    // 3. Only then `published`.
    journal.state = PublishState::Published;
    store.persist_journal(&journal)
}

/// Start-up verification with a journal (plan R6 addenda): whatever its state,
/// the journal's generation is fully re-verified (trust ledger including
/// revocation, the journal's manifest digest, every artifact) and all eight
/// destinations are republished with the idempotent procedure (a destination
/// already holding the right bytes is kept, an altered one is replaced).
fn recover_journal<S: JournalStore, E: Effects>(store: &mut S, effects: &mut E, journal: PublishJournal)
    -> PublicationRecovery {
    // Forward: the same generation, re-verified, with the recorded manifest.
    if publish_verified(store, effects, journal.generation, Some(journal.manifest_sha256), journal.previous, None).is_ok() {
        return match journal.state {
            PublishState::Published => PublicationRecovery::Reverified,
            PublishState::Publishing => PublicationRecovery::RolledForward,
        };
    }
    // Roll back with the same procedure. The new journal names the previous
    // generation, and the interrupted one as its own fallback.
    match journal.previous {
        Some(previous) if publish_verified(store, effects, previous, None, Some(journal.generation), None).is_ok() =>
            PublicationRecovery::RolledBack,
        _ => PublicationRecovery::Incomplete,
    }
}
/// Start-up verification without a journal: the generation the `current`
/// selector names is verified like a publication's (trust ledger including
/// revocation, every artifact), and every destination must already hold its
/// bytes. Nothing is written: without a journal there is no recorded intent to
/// complete, so a difference is reported (an `Apply` of that generation repairs it).
/// A host installed by `duduclaw-bootstrap install` has a selector from its
/// first boot on (`trust_init` publishes the initial `current` and records it
/// in the ledger), so "no selector" is never a fresh-install state here.
fn verify_selected<E: Effects>(effects: &mut E) -> (Option<Sha256Digest>, PublicationRecovery) {
    match effects.installed() {
        Ok(Some(generation)) => {
            let verified = effects.verify(generation, None).is_ok() && effects.published(generation).is_ok();
            (Some(generation), if verified { PublicationRecovery::NoJournal } else { PublicationRecovery::SelectedUnverified })
        }
        Ok(None) => (None, PublicationRecovery::SelectorAbsent),
        Err(_) => (None, PublicationRecovery::SelectorUnreadable),
    }
}

/// Where this process is in the start sequence (plan R6 addendum point 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Verified consistent at start; the allocation socket, control listener and
    /// admission loop are being opened.
    Verified,
    /// Verified and everything opened ([`UpdateControl::opened`]).
    Open,
    /// Was `Verified`/`Open`, then an `Apply` was accepted for publication:
    /// the status said `recovering` before its journal was written and says so
    /// for the rest of this process. The admission loop still runs (closed by
    /// the drain barrier); only the restarted, re-verified spawnd says
    /// `consistent` again, so a stopped gate is re-armed only by that invocation.
    Superseded,
    /// Not consistent at start: only the update endpoint is open, no admission
    /// loop exists, so no run can be admitted by this process.
    Closed,
    /// Started `Closed`, then an `Apply` repaired the set: this process exits
    /// with [`RESTART_AFTER_REPAIR_EXIT`] once the reply is sent.
    RestartPending,
    /// Was `Verified`/`Open`, then a request left the set not consistent:
    /// admission is closed (`Recovery`), every further request is refused, and
    /// this process exits with [`EXIT_SET_INCONSISTENT`] once the reply is sent.
    Failed,
}
/// What the accept loop does after one served connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum After { Continue, RestartAfterRepair, ExitInconsistent }

/// The protocol state machine. Requests are served one at a time by one thread,
/// so `Drain`, `Apply` and `Resume` never interleave.
pub struct UpdateControl<S, E> { store: S, barrier: DrainBarrier, effects: E, startup: Startup, phase: Phase,
    owner: Option<Owner>,
    /// Where [`Self::report`] writes the status file (`None`: not written).
    status_root: Option<PathBuf>,
    /// The last `STATUS=` line announced (announced again only when it changes).
    reported: Option<String>,
    /// A synchronously written audit record did not land (part of `STATUS=`
    /// for the rest of this process).
    audit_failed: bool }
impl<S: DrainStore + JournalStore, E: Effects> UpdateControl<S, E> {
    /// Restores the barrier from the durable record, then verifies the
    /// installed set (journal: re-verify and republish, or roll back; no
    /// journal: verify the selected generation). Call before any socket is
    /// bound and before admission starts.
    pub fn new(mut store: S, barrier: DrainBarrier, mut effects: E) -> io::Result<Self> {
        let drain = match store.load() {
            Ok(None) => None,
            Ok(Some(generation)) => Some(DrainClosure::Generation(generation)),
            Err(_) => Some(DrainClosure::Unreadable),
        };
        if let Some(closure) = drain { barrier.close(closure).ok_or_else(invalid)?; }
        let mut selected = None;
        let (journal, recovery) = match store.load_journal() {
            Ok(None) => {
                let (generation, recovery) = verify_selected(&mut effects);
                selected = generation;
                (None, recovery)
            }
            Ok(Some(journal)) => (Some(journal), recover_journal(&mut store, &mut effects, journal)),
            Err(_) => (None, PublicationRecovery::JournalUnreadable),
        };
        // A possibly mixed or unverified set: closed whatever the record said;
        // only a new Drain re-keys it, and nothing but the update endpoint opens.
        let phase = if recovery.consistent() { Phase::Verified } else {
            barrier.close(DrainClosure::Recovery).ok_or_else(invalid)?;
            Phase::Closed
        };
        Ok(Self { store, barrier, effects, startup: Startup { drain, recovery, journal, selected }, phase,
            owner: Owner::current().ok(), status_root: None, reported: None, audit_failed: false })
    }
    /// Production: the status file lives in `root` (`/run/duduclaw-spawnd`).
    fn with_status_root(mut self, root: &Path) -> Self { self.status_root = Some(root.to_path_buf()); self }
    pub fn startup(&self) -> Startup { self.startup }
    /// The set was verified consistent at this process's start: the caller
    /// opens the allocation socket, control listener and admission loop.
    pub fn consistent_at_start(&self) -> bool { self.startup.recovery.consistent() }
    /// Everything is open: from now on the status says `consistent` (while the
    /// journal stays absent or `published`). `false`: not verified at start.
    pub fn opened(&mut self) -> bool {
        if self.phase != Phase::Verified { return false; }
        self.phase = Phase::Open;
        true
    }
    /// A process that started not consistent and whose set an `Apply` has since
    /// repaired: it must exit with [`RESTART_AFTER_REPAIR_EXIT`].
    pub(crate) fn restart_required(&self) -> bool { self.phase == Phase::RestartPending }
    /// A process that was open and whose set a request left not consistent: it
    /// must exit with [`EXIT_SET_INCONSISTENT`] (admission is already closed).
    pub(crate) fn exit_required(&self) -> bool { self.phase == Phase::Failed }
    /// The admission loop of this process runs (or is about to).
    fn admission_live(&self) -> bool { matches!(self.phase, Phase::Verified | Phase::Open | Phase::Superseded) }
    pub(crate) fn status(&mut self) -> UpdateStatus {
        let (barrier, barrier_generation) = barrier_words(self.barrier.closure());
        let (startup_barrier, startup_barrier_generation) = barrier_words(self.startup.drain);
        let (journal, journal_generation, journal_previous, publication_consistent) = match self.store.load_journal() {
            Ok(None) => ("absent", None, None, true),
            Ok(Some(found)) => (if found.state == PublishState::Published { "published" } else { "publishing" },
                Some(hex(&found.generation)), found.previous.as_ref().map(hex), found.state == PublishState::Published),
            Err(_) => ("unreadable", None, None, false),
        };
        let later = if journal == "unreadable" { "journal_unreadable" } else { "publication_incomplete" };
        let (set, closed_reason) = match self.phase {
            Phase::Verified | Phase::RestartPending => ("recovering", None),
            Phase::Closed => ("inconsistent", Some(self.startup.recovery.closed_reason().unwrap_or("not_verified"))),
            // A later request of this open process left the set inconsistent:
            // admission is closed and the process exits (the gate drops with it).
            Phase::Failed => ("inconsistent", Some(later)),
            Phase::Open | Phase::Superseded if !publication_consistent => ("inconsistent", Some(later)),
            Phase::Open => ("consistent", None),
            // A publication was accepted: only the restarted spawnd may say `consistent`.
            Phase::Superseded => ("recovering", None),
        };
        UpdateStatus { schema: STATUS_SCHEMA, set, closed_reason, owner: self.owner.clone(),
            admission_closed: self.barrier.is_closed(), barrier, barrier_generation, journal,
            journal_generation, journal_previous, publication_consistent, startup_barrier, startup_barrier_generation,
            startup_recovery: self.startup.recovery }
    }
    /// Write the status file (when configured) and announce the `STATUS=` line
    /// when it changed. A failed write is part of the line. Returns the line.
    pub(crate) fn report(&mut self) -> String { self.report_written().0 }
    /// [`Self::report`], and whether the status file was written (`true` when
    /// none is configured).
    fn report_written(&mut self) -> (String, bool) {
        let status = self.status();
        let written = match &self.status_root {
            Some(root) => write_status(root, &status).is_ok(),
            None => true,
        };
        let mut line = status_line(status.set, status.closed_reason, written);
        if self.audit_failed { line.push_str("; audit_write_failed"); }
        if self.reported.as_deref() != Some(line.as_str()) {
            announce(&line);
            self.reported = Some(line.clone());
        }
        (line, written)
    }
    /// Every refusal, whatever its cause, is the single `Refused` outcome. The
    /// set is re-checked before the reply exists ([`Self::settle`]), so no run
    /// can be admitted once it is known not to be consistent.
    pub fn respond(&mut self, request: &ControlRequest) -> ControlResponse {
        let outcome = if request.schema_version == SCHEMA {
            self.handle(&request.command).unwrap_or(ControlOutcome::Refused)
        } else { ControlOutcome::Refused };
        self.settle();
        ControlResponse { schema_version: SCHEMA, nonce: request.nonce, generation: request.command.generation(), outcome }
    }
    /// An open (or opening) process keeps serving only while every fixed
    /// component holds one generation's bytes: the journal is absent or
    /// `published`. Otherwise (an `Apply` that failed after `publishing`
    /// landed, or a journal that no longer reads) admission is closed with
    /// [`DrainClosure::Recovery`] first, then the process is marked to exit.
    fn settle(&mut self) {
        if !self.admission_live() { return; }
        let consistent = match self.store.load_journal() {
            Ok(None) => true,
            Ok(Some(journal)) => journal.state == PublishState::Published,
            Err(_) => false,
        };
        if consistent { return; }
        // A poisoned barrier already reads as closed (`DrainBarrier::closure`).
        let _ = self.barrier.close(DrainClosure::Recovery);
        self.phase = Phase::Failed;
    }
    fn handle(&mut self, command: &ControlCommand) -> io::Result<ControlOutcome> {
        if command.generation().0 == [0; 32] { return Err(invalid()); }
        // A failed process answers nothing more; it exits after this reply.
        if self.phase == Phase::Failed { return Err(invalid()); }
        match command {
            ControlCommand::Prepare { preparation } => { self.effects.prepare(preparation)?; Ok(ControlOutcome::Prepared) }
            ControlCommand::Drain { generation } => self.drain(*generation),
            ControlCommand::Apply { preparation } => {
                self.drained(preparation.generation)?;
                let previous = self.previous_for(preparation.generation)?;
                let manifest = verified_manifest(&mut self.effects, preparation.generation, None, Some(preparation))?;
                self.accept_publication()?;
                publish_journaled(&mut self.store, &mut self.effects, preparation.generation, manifest, previous)?;
                // A set this process found not consistent is now complete: it
                // replies, then restarts so the next start verifies everything
                // before opening anything (never an in-process switch).
                if self.phase == Phase::Closed { self.phase = Phase::RestartPending; }
                Ok(ControlOutcome::Applied)
            }
            ControlCommand::Readback { generation } => {
                let live = self.live();
                Ok(ControlOutcome::Readback { components: self.effects.readback(*generation, live)? })
            }
            ControlCommand::Resume { generation } => self.resume(*generation),
        }
    }
    fn drain(&mut self, generation: Sha256Digest) -> io::Result<ControlOutcome> {
        let started = Instant::now();
        // 1. Durable record first; nothing is announced if it does not land.
        self.store.persist(generation)?;
        // 2. Then the in-memory barrier: no new StartRun from here on.
        let epoch = self.barrier.close(DrainClosure::Generation(generation)).ok_or_else(invalid)?;
        // No admission loop exists in this process (started not consistent):
        // nothing can be admitted here, but runs of an earlier spawnd process
        // may still be tearing down. Zero only on a complete, idle census.
        if !self.admission_live() {
            self.census_idle()?;
            return Ok(ControlOutcome::Drained { admission_closed: true, active_runs: 0 });
        }
        // 3. Count what an admission iteration that began after the closure kept.
        let observed = self.barrier.wait_observation(epoch, started, OBSERVATION_WAIT).ok_or_else(timed_out)?;
        // An unhealthy iteration's count is not evidence of a drain (the barrier
        // stays closed and recorded; the updater retries).
        if !observed.healthy { return Err(invalid()); }
        Ok(ControlOutcome::Drained { admission_closed: true,
            active_runs: u32::try_from(observed.admitted_runs).unwrap_or(u32::MAX) })
    }
    /// Apply's precondition: closed for exactly this generation, in memory and
    /// on disk; with an admission loop, a fresh healthy iteration saw no
    /// admitted run; and in every phase the census finds no live workload.
    fn drained(&mut self, generation: Sha256Digest) -> io::Result<()> {
        if self.barrier.closure() != Some(DrainClosure::Generation(generation)) || self.store.load()? != Some(generation) {
            return Err(invalid());
        }
        if self.admission_live() {
            let observed = self.barrier.wait_observation(self.barrier.epoch(), Instant::now(), OBSERVATION_WAIT)
                .ok_or_else(timed_out)?;
            if !observed.healthy || observed.admitted_runs != 0 { return Err(invalid()); }
        }
        // Independent of this process's memory (see `drain`).
        self.census_idle()
    }
    /// The census completed and found no live run unit and no populated
    /// employees cgroup; an incomplete census or any live workload refuses.
    fn census_idle(&mut self) -> io::Result<()> {
        if self.effects.workloads()?.idle() { Ok(()) } else { Err(invalid()) }
    }
    /// Apply's last precondition, after the generation verified and before its
    /// journal: PID 1 reports the consistency gate and every unit bound to it
    /// down ([`GATED_UNITS`]; the updater stopped them), so no component can
    /// load a partly published set. An open process then stops saying
    /// `consistent` for good ([`Phase::Superseded`]; the status file must land,
    /// or the Apply is refused), so the stopped gate cannot complete against
    /// this invocation again; and the gate is checked once more, in case it
    /// completed against the old status before that write. A process that
    /// started not consistent never said `consistent` (its gate is down too).
    fn accept_publication(&mut self) -> io::Result<()> {
        if !self.effects.gated_units_down()? { return Err(invalid()); }
        if !self.admission_live() { return Ok(()); }
        self.phase = Phase::Superseded;
        if !self.report_written().1 { return Err(invalid()); }
        if !self.effects.gated_units_down()? { return Err(invalid()); }
        Ok(())
    }
    /// The journal's `previous` for a publication of `generation`:
    /// - a journal of the same generation (a retried `Apply`) keeps its own;
    /// - `publishing` of another generation refuses, except the roll-back to its
    ///   `previous`, whose fallback is then the interrupted generation;
    /// - `published` of another generation (applied, not yet resumed) is installed;
    /// - no journal: the generation the install selector names.
    fn previous_for(&mut self, generation: Sha256Digest) -> io::Result<Option<Sha256Digest>> {
        Ok(match self.store.load_journal()? {
            Some(journal) if journal.generation == generation => journal.previous,
            Some(journal) if journal.state == PublishState::Publishing => {
                if journal.previous != Some(generation) { return Err(invalid()); }
                Some(journal.generation)
            }
            Some(journal) => Some(journal.generation),
            None => self.effects.installed()?.filter(|installed| *installed != generation),
        })
    }
    fn resume(&mut self, generation: Sha256Digest) -> io::Result<ControlOutcome> {
        // There is no admission to reopen in a process that started not
        // consistent: only its restart (after a repairing Apply) opens anything.
        if !self.admission_live() { return Err(invalid()); }
        let matches = match self.barrier.closure() {
            None => true,
            Some(DrainClosure::Generation(closed)) => closed == generation,
            Some(DrainClosure::Unreadable | DrainClosure::Recovery) => false,
        };
        if !matches { return Err(invalid()); }
        match self.store.load()? {
            None => {}
            Some(recorded) if recorded == generation => {}
            Some(_) => return Err(invalid()),
        }
        // Admission reopens only on a consistent set of exactly this generation:
        // a `publishing` journal, or one naming another generation, refuses.
        match self.store.load_journal()? {
            None => {}
            Some(journal) if journal.state == PublishState::Published && journal.generation == generation => {}
            Some(_) => return Err(invalid()),
        }
        // 1. The journal goes first (the updater resumes only after it committed
        //    or rolled back to `generation`); 2. the drain record; 3. admission.
        self.store.clear_journal()?;
        self.store.clear()?;
        if !self.barrier.open(generation) { return Err(invalid()); }
        Ok(ControlOutcome::Resumed)
    }
    fn live(&self) -> bool {
        self.barrier.last_observation().is_some_and(|observed| observed.healthy && observed.at.elapsed() <= LIVE_WINDOW)
    }
}

/// One connection: peer check, one request frame, one reply frame. Every
/// socket operation has the client's 5 s timeout; errors close without a reply.
pub(crate) fn serve_one<S: DrainStore + JournalStore, E: Effects>(fd: OwnedFd, peer: (u32, u32), control: &mut UpdateControl<S, E>)
    -> io::Result<()> {
    transport::timeout(&fd).map_err(updated)?;
    let credentials = socket::getsockopt(&fd, sockopt::PeerCredentials)?;
    if (credentials.uid(), credentials.gid()) != peer { return Err(denied()); }
    // Zero control space: any ancillary data sets MSG_CTRUNC and is refused;
    // a short, long or trailing frame and any unknown field are refused.
    let request: ControlRequest = transport::receive(&fd).map_err(updated)?;
    let response = control.respond(&request);
    transport::send(&fd, &response).map_err(updated)
}
/// [`serve_one`], then the decision: the reply has been sent (and the
/// connection closed) before a repaired set's restart or an inconsistent
/// set's failure exit is requested.
pub(crate) fn serve_and_settle<S: DrainStore + JournalStore, E: Effects>(fd: OwnedFd, peer: (u32, u32),
    control: &mut UpdateControl<S, E>) -> After {
    let _ = serve_one(fd, peer, control);
    if control.restart_required() { After::RestartAfterRepair }
    else if control.exit_required() { After::ExitInconsistent }
    else { After::Continue }
}
/// The audit record of a failure exit: the journal as found now.
fn inconsistent_record<S: JournalStore>(store: &mut S, at_unix_ms: u64) -> Record {
    let (action, journal) = match store.load_journal() {
        Ok(journal) => (PublicationRecovery::Incomplete, journal),
        Err(_) => (PublicationRecovery::JournalUnreadable, None),
    };
    Record { at_unix_ms, event: Event::UpdateSetInconsistent, employee: None, run: None, suppressed: 0,
        detail: Some(Detail::PublicationRecovery { action, generation: journal.map(|journal| journal.generation),
            previous: journal.and_then(|journal| journal.previous) }) }
}
/// Write `records` with `write` (production: acknowledged by the audit
/// worker) before anything reports success; a record that did not land makes
/// every later `STATUS=` line carry `; audit_write_failed`. Then report.
pub(crate) fn report_audited<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>,
    records: Vec<Record>, mut write: impl FnMut(Record) -> bool) -> String {
    for record in records {
        if !write(record) { control.audit_failed = true; }
    }
    control.report()
}

/// Bind `update.sock` (root 0600) in the root-owned `/run/duduclaw-spawnd`.
/// The caller holds the lifetime update lock (`recover`; every spawnd takes it
/// before binding anything), so an existing root socket is a dead spawnd's
/// endpoint; any other file at the path stops startup.
fn bind_listener() -> io::Result<OwnedFd> {
    let path = Path::new(SPAWND_UPDATE_SOCKET);
    let parent = Directory::open(path.parent().ok_or_else(invalid)?).map_err(updated)?;
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(invalid)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == 0 => parent.remove(name).map_err(updated)?,
        Ok(_) => return Err(denied()),
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error),
    }
    let fd = transport::socket().map_err(updated)?;
    socket::bind(fd.as_raw_fd(), &UnixAddr::new(path)?)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o600 {
        return Err(denied());
    }
    socket::listen(&fd, Backlog::new(8)?)?;
    // SO_RCVTIMEO also bounds accept(): the thread never blocks indefinitely.
    transport::timeout(&fd).map_err(updated)?;
    Ok(fd)
}

type Shared = Arc<Mutex<UpdateControl<DiskDrainStore, InstalledEffects>>>;
/// The state is shared with [`Serving::opened`]; a poisoned lock (a panic
/// while serving) ends the process (fail closed: systemd restarts it, and
/// the start verifies everything again).
fn locked<S, E>(control: &Mutex<UpdateControl<S, E>>) -> std::sync::MutexGuard<'_, UpdateControl<S, E>> {
    match control.lock() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("duduclaw-spawnd: update control state lost");
            std::process::exit(1)
        }
    }
}
/// One request at a time. After a request that repaired a set this process
/// found not consistent: the reply is already sent, the status is flushed
/// (`recovering`), then the process exits with [`RESTART_AFTER_REPAIR_EXIT`].
/// After a request that left an open set not consistent: admission was closed
/// before the reply, the reply is sent, the status is flushed
/// (`inconsistent`), the event is audited synchronously (which also flushes
/// every audit record queued before it), then the process exits with
/// [`EXIT_SET_INCONSISTENT`]. The update lock and the endpoint go with it.
fn accept_loop(listener: OwnedFd, control: Shared, audit: crate::audit::disk::AuditWorker) {
    loop {
        match socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC) {
            Ok(raw) => {
                // SAFETY: accept4 returned a new descriptor owned only here.
                let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                let mut control = locked(&control);
                let after = serve_and_settle(fd, (0, 0), &mut *control);
                match after {
                    After::Continue => { control.report(); }
                    After::RestartAfterRepair => {
                        control.report();
                        eprintln!("duduclaw-spawnd: set repaired; restarting to verify before opening");
                        std::process::exit(RESTART_AFTER_REPAIR_EXIT);
                    }
                    After::ExitInconsistent => {
                        control.report();
                        let record = inconsistent_record(&mut control.store, now_ms());
                        report_audited(&mut *control, vec![record],
                            |record| audit.emit_acknowledged(AUDIT_SOURCE, record, AUDIT_ACKNOWLEDGEMENT));
                        eprintln!("duduclaw-spawnd: installed set no longer consistent; admission closed, exiting for recovery");
                        std::process::exit(EXIT_SET_INCONSISTENT);
                    }
                }
            }
            Err(Errno::EAGAIN | Errno::EINTR | Errno::ECONNABORTED) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0)
}

/// spawnd's start-up half of the endpoint: the update lock held, the barrier
/// restored and the installed set verified. [`Recovered::serve`] binds.
pub struct Recovered { control: UpdateControl<DiskDrainStore, InstalledEffects>, barrier: DrainBarrier, lock: File,
    audit: crate::audit::disk::AuditWorker }
/// Called before any socket is bound (control, allocation, update) and before
/// admission. The lifetime flock `/var/lib/duduclaw-spawnd/update.lock` makes
/// this process the only writer of the fixed destinations even before the
/// registry singleton lock is taken. Announces `recovering` first, writes the
/// barrier restored from a record and the start-up verification to the audit
/// log synchronously (each acknowledged by the audit worker after its fsync,
/// bounded), then writes the status file and announces the result (a record
/// that did not land or a failed status write is part of the announced
/// `STATUS=` line, never silent).
pub fn recover(audit: &crate::audit::disk::AuditWorker) -> io::Result<Recovered> {
    announce(&status_line("recovering", None, true));
    let lock = Directory::open(Path::new(DRAIN_ROOT)).and_then(|directory| directory.lock()).map_err(updated)?;
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(DiskDrainStore::production(), barrier.clone(), InstalledEffects::production())?
        .with_status_root(Path::new(STATUS_ROOT));
    let records = control.startup().audit_records(now_ms());
    report_audited(&mut control, records, |record| audit.emit_acknowledged(AUDIT_SOURCE, record, AUDIT_ACKNOWLEDGEMENT));
    Ok(Recovered { control, barrier, lock, audit: audit.clone() })
}
impl Recovered {
    /// Every fixed component holds one verified generation's bytes. `false`:
    /// spawnd opens nothing but the update endpoint.
    pub fn consistent(&self) -> bool { self.control.consistent_at_start() }
    /// Bind `update.sock` and serve it on a dedicated thread (which keeps the
    /// update lock). The returned handle carries the barrier for the admission
    /// loop and marks the set open ([`Serving::opened`]).
    pub fn serve(self) -> io::Result<Serving> {
        let Self { control, barrier, lock, audit } = self;
        let listener = bind_listener()?;
        let control: Shared = Arc::new(Mutex::new(control));
        let served = control.clone();
        let thread = std::thread::Builder::new().name("spawnd-update-control".into())
            .spawn(move || { let _lock = lock; accept_loop(listener, served, audit) })?;
        Ok(Serving { barrier, control, thread })
    }
}
/// The running update endpoint.
pub struct Serving { barrier: DrainBarrier, control: Shared, thread: std::thread::JoinHandle<()> }
impl Serving {
    /// The barrier the admission loop observes.
    pub fn barrier(&self) -> DrainBarrier { self.barrier.clone() }
    /// The allocation socket, control listener and admission are open: the
    /// status file now says `consistent` for this process (the waiter of
    /// `duduclaw-isolation-ready.service` completes on it).
    pub fn opened(&self) -> io::Result<()> {
        let mut control = locked(&self.control);
        if !control.opened() { return Err(invalid()); }
        control.report();
        Ok(())
    }
    /// Not consistent: the process serves only the update endpoint until a
    /// repairing `Apply` exits it. Returns only if the endpoint thread ended.
    pub fn wait(self) -> io::Result<()> {
        let _ = self.thread.join();
        Err(io::Error::other("update control endpoint stopped"))
    }
}

/// spawnd's start sequence after [`recover`] (plan R6 addendum points 2 and 4):
/// 1. bind and serve the root-only update endpoint;
/// 2. `READY=1`, always: it means only that the update endpoint is available;
/// 3. only when the set was verified consistent: open the allocation socket,
///    control listener and admission (`open_admission`). Otherwise nothing
///    else is initialised and `Ok(None)` is returned.
pub fn start<T>(consistent: bool, bind_update: impl FnOnce() -> io::Result<()>, mut notify: impl FnMut(&str),
    open_admission: impl FnOnce() -> io::Result<T>) -> io::Result<Option<T>> {
    bind_update()?;
    notify("READY=1");
    if !consistent { return Ok(None); }
    open_admission().map(Some)
}

/// `duduclaw-spawnd.service` as PID 1 reports it right now (active, with a
/// main process): its `MainPID` and its `InvocationID`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveSpawnd { pub pid: u32, pub invocation_id: String }
/// `duduclaw-spawnd wait-consistent` (unit `duduclaw-isolation-ready.service`):
/// a fresh, matching, consistent status file exists. Fresh and matching: the
/// `owner` is this boot (`/proc/sys/kernel/random/boot_id`, a global file),
/// and PID 1's current spawnd (`live`) has exactly the owner's pid and
/// invocation id (a status of an earlier spawnd process, even with a recycled
/// pid, never matches: every start gets a new random invocation id).
/// Missing, unreadable, unparsable, ownerless, stale, no live spawnd or not
/// `consistent`: `false`.
pub(crate) fn status_accepted(bytes: Option<&[u8]>, boot: &str, live: Option<&LiveSpawnd>) -> bool {
    #[derive(Deserialize)]
    struct View { schema: u16, set: String, owner: Option<Owner> }
    let Some(bytes) = bytes.filter(|bytes| bytes.len() <= MAX_STATUS_BYTES) else { return false };
    let Ok(view) = serde_json::from_slice::<View>(bytes) else { return false };
    let (Some(owner), Some(live)) = (view.owner, live) else { return false };
    view.schema == STATUS_SCHEMA && view.set == "consistent" && owner.boot_id == boot
        && invocation_id_valid(&owner.invocation_id) && owner.pid == live.pid && owner.invocation_id == live.invocation_id
}
/// Poll `accepted` every `interval` until it holds (`Ok`) or `timeout` passes
/// (`Err`); `None`: no timeout (the unit's own start timeout is infinite).
pub(crate) fn wait_until(mut accepted: impl FnMut() -> bool, timeout: Option<Duration>, interval: Duration)
    -> io::Result<()> {
    let started = Instant::now();
    loop {
        if accepted() { return Ok(()); }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) { return Err(timed_out()); }
        std::thread::sleep(interval);
    }
}
/// The root-only status file, read through `Directory` (root-owned ancestors,
/// no symlink, root-owned single-link regular file).
fn read_status() -> Option<Vec<u8>> {
    Directory::open(Path::new(STATUS_ROOT)).and_then(|directory| directory.bytes(STATUS_FILE, MAX_STATUS_BYTES)).ok()
}
/// `/sys/fs/cgroup` is the unified cgroup v2 hierarchy (`statfs` magic). The
/// isolated install requires it (plan S1 HostPreflight, "cgroup v2"; spawnd's
/// own run census and helper slots depend on it), so the gate refuses
/// anything else instead of waiting.
fn unified_cgroup_hierarchy() -> bool {
    let Ok(path) = std::ffi::CString::new(CGROUP_ROOT) else { return false };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: a NUL-terminated path and a writable, correctly sized buffer.
    if unsafe { libc::statfs(path.as_ptr(), fs.as_mut_ptr()) } != 0 { return false; }
    // SAFETY: statfs returned 0, so it initialised the buffer.
    let fs = unsafe { fs.assume_init() };
    fs.f_type as i64 == CGROUP2_SUPER_MAGIC
}
/// The fixed refusal of a host that is not on the unified cgroup hierarchy.
pub const NOT_UNIFIED_MESSAGE: &str =
    "duduclaw-spawnd: wait-consistent requires the unified cgroup v2 hierarchy at /sys/fs/cgroup; not waiting";
/// The waiter's logic over its seams: not unified ⇒ `Err` at once (nothing is
/// read, nothing waited for); otherwise poll until a status is accepted for
/// the spawnd PID 1 reports (`live`) or `timeout` passes.
pub(crate) fn wait_for_consistency(unified: bool, boot: &str, mut read: impl FnMut() -> Option<Vec<u8>>,
    mut live: impl FnMut() -> Option<LiveSpawnd>, timeout: Option<Duration>, interval: Duration) -> io::Result<()> {
    if !unified { return Err(io::Error::new(io::ErrorKind::Unsupported, NOT_UNIFIED_MESSAGE)); }
    wait_until(|| {
        // The status first: an accepting status is then checked against the
        // spawnd that is live after it was read.
        let status = read();
        status_accepted(status.as_deref(), boot, live().as_ref())
    }, timeout, interval)
}
/// Returns `Ok` only once spawnd reports the verified, opened, consistent set.
/// Waits indefinitely otherwise; an unreadable boot id or a host that is not
/// on the unified cgroup v2 hierarchy is an error at once (never `Ok`).
/// Reads the root-only status file and asks PID 1 over the system bus for
/// `duduclaw-spawnd.service`'s state, `MainPID` and `InvocationID` (read-only
/// properties, root, no capability). It never opens `/proc/<pid>`, so neither
/// `hidepid=` nor spawnd being non-dumpable matters. Trade-off: the identity
/// is PID 1's view, which needs the system bus (spawnd is ordered after
/// `dbus.service`); while the bus is unavailable the waiter keeps waiting.
pub fn wait_consistent() -> io::Result<()> {
    if !unified_cgroup_hierarchy() {
        eprintln!("{NOT_UNIFIED_MESSAGE}");
        return Err(io::Error::new(io::ErrorKind::Unsupported, NOT_UNIFIED_MESSAGE));
    }
    let boot = boot_id()?;
    eprintln!("duduclaw-spawnd: waiting for a consistent isolated set (see {STATUS_ROOT}/{STATUS_FILE})");
    let mut systemd: Option<crate::systemd::linux::Systemd> = None;
    let live = || {
        if systemd.is_none() { systemd = crate::systemd::linux::Systemd::connect().ok(); }
        let identity = systemd.as_ref()?.spawnd_identity();
        match identity {
            Ok(identity) => identity.map(|(pid, invocation_id)| LiveSpawnd { pid, invocation_id }),
            // Reconnect on the next poll (the bus may have restarted).
            Err(_) => { systemd = None; None }
        }
    };
    wait_for_consistency(true, &boot, read_status, live, None, Duration::from_millis(500))?;
    eprintln!("duduclaw-spawnd: isolated set consistent");
    Ok(())
}

/// `sd_notify(3)` for the `Type=notify` unit: `state` is e.g. `READY=1`. Without
/// `$NOTIFY_SOCKET` (not started by systemd) nothing is sent.
pub fn notify(state: &str) -> io::Result<()> {
    use std::os::unix::{ffi::OsStrExt, net::UnixDatagram};
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else { return Ok(()) };
    let socket = UnixDatagram::unbound()?;
    let bytes = path.as_bytes();
    if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
        socket.send_to_addr(state.as_bytes(), &address)?;
    } else if bytes.first() == Some(&b'/') {
        socket.send_to(state.as_bytes(), Path::new(&path))?;
    } else {
        return Err(invalid());
    }
    Ok(())
}

/// Fixed destinations (relative to the publication root) in the plan's order:
/// spawnd, run, parse worker, `/usr/lib/duduclaw/current/*`, egress, gateway.
/// File names are `publication::artifact_name`. The egress forwarder and
/// runtime entries are used from the generation itself; the updater slot is F's.
fn publication() -> [(InstallTarget, &'static str); 8] {
    [(InstallTarget::Spawnd, "usr/lib/duduclaw"), (InstallTarget::Trampoline, "usr/lib/duduclaw"),
        (InstallTarget::ParseWorker, "usr/libexec/duduclaw"), (InstallTarget::Importer, "usr/lib/duduclaw/current"),
        (InstallTarget::Migration, "usr/lib/duduclaw/current"), (InstallTarget::McpBridge, "usr/lib/duduclaw/current"),
        (InstallTarget::Egress, "opt/duduclaw/bin"), (InstallTarget::Gateway, "opt/duduclaw/bin")]
}

#[derive(Clone, Copy)]
enum Trust {
    /// The manifest signature recorded in the root trust ledger (`verify_recorded`).
    Recorded,
    #[cfg(test)]
    Unsigned,
}

/// Production effects over `/opt/duduclaw-isolated/generations/<hex>` and `/`.
pub struct InstalledEffects {
    install_root: PathBuf, publish_root: PathBuf, trust: Trust, own_image: Option<Sha256Digest>,
    /// Fault injection: the next `publish` stops (as a power loss would) after
    /// this many renames, before the next one.
    #[cfg(test)]
    power_loss_after: Option<usize>,
    /// Stands in for the trust ledger's revocation fence (`Trust::Unsigned`):
    /// these generations fail authentication.
    #[cfg(test)]
    revoked: Vec<Sha256Digest>,
}
impl InstalledEffects {
    pub fn production() -> Self {
        Self { install_root: PathBuf::from(INSTALL_ROOT), publish_root: PathBuf::from("/"), trust: Trust::Recorded,
            own_image: running_image().ok(), #[cfg(test)] power_loss_after: None, #[cfg(test)] revoked: Vec::new() }
    }
    #[cfg(test)]
    fn for_test(install_root: &Path, publish_root: &Path) -> Self {
        Self { install_root: install_root.to_path_buf(), publish_root: publish_root.to_path_buf(),
            trust: Trust::Unsigned, own_image: running_image().ok(), power_loss_after: None, revoked: Vec::new() }
    }
    /// `Ok(None)` only when the generation directory does not exist (yet).
    fn generation_directory(&self, generation: Sha256Digest) -> io::Result<Option<Directory>> {
        let generations = Directory::open(&self.install_root).and_then(|root| root.child("generations")).map_err(updated)?;
        match generations.child(&hex(&generation)) {
            Ok(directory) => Ok(Some(directory)),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => Ok(None),
            Err(error) => Err(updated(error)),
        }
    }
    /// The canonical manifest whose SHA-256 is the generation digest.
    fn manifest(&self, directory: &Directory, generation: Sha256Digest) -> io::Result<(Vec<u8>, ReleaseManifest)> {
        let bytes = directory.bytes("manifest.json", MAX_MANIFEST_BYTES).map_err(updated)?;
        if Sha256::digest(&bytes).as_slice() != generation.0 { return Err(invalid()); }
        let manifest: ReleaseManifest = duduclaw_updated::metadata::decode_canonical(&bytes).map_err(updated)?;
        manifest.validate().map_err(|_| invalid())?;
        Ok((bytes, manifest))
    }
    fn authenticate(&self, directory: &Directory, bytes: &[u8], manifest: &ReleaseManifest) -> io::Result<()> {
        match self.trust {
            Trust::Recorded => {
                let signature = String::from_utf8(directory.bytes("manifest.json.sig", MAX_SIGNATURE_BYTES).map_err(updated)?)
                    .map_err(|_| invalid())?;
                let verified = duduclaw_updated::linux::trust_admin::verify_recorded(bytes, &signature).map_err(updated)?;
                if &verified != manifest { return Err(invalid()); }
            }
            #[cfg(test)]
            Trust::Unsigned => {}
        }
        Ok(())
    }
    fn publication_directories(&self) -> io::Result<()> {
        for (_, directory) in publication() { Directory::open(&self.publish_root.join(directory)).map_err(updated)?; }
        Ok(())
    }
}

/// The manifest-bound data-migration executor (G) is not part of this release:
/// a generation that needs one is refused, never activated without it.
fn no_migrations(preparation: &Preparation) -> io::Result<()> {
    if preparation.migrations.is_empty() { Ok(()) } else { Err(invalid()) }
}
fn runtime_entries_shape(preparation: &Preparation) -> io::Result<()> {
    for entry in &preparation.runtime_entries {
        if !matches!(entry.install_target, InstallTarget::RuntimeEntry { .. }) { return Err(invalid()); }
        artifact_name(&entry.install_target).map_err(updated)?;
    }
    Ok(())
}
/// The request is exactly the preparation of this manifest.
fn bound(preparation: &Preparation, manifest: &ReleaseManifest) -> io::Result<()> {
    let expected = Preparation::for_manifest(manifest, preparation.generation);
    if expected.migrations != preparation.migrations || expected.runtime_entries != preparation.runtime_entries {
        return Err(invalid());
    }
    no_migrations(preparation)
}
/// The one artifact a fixed destination receives.
fn fixed_artifact<'a>(manifest: &'a ReleaseManifest, target: &InstallTarget) -> io::Result<&'a ReleaseArtifact> {
    let mut matching = manifest.artifacts.iter().filter(|artifact| &artifact.install_target == target);
    let (Some(artifact), None) = (matching.next(), matching.next()) else { return Err(invalid()) };
    Ok(artifact)
}
fn verify_staged(generation: &Directory, artifact: &ReleaseArtifact) -> io::Result<()> {
    let file = generation.read(&artifact_name(&artifact.install_target).map_err(updated)?).map_err(updated)?;
    if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(invalid()); }
    verify_fd(file, artifact.size_bytes, artifact.sha256).map_err(updated)?;
    Ok(())
}
/// `true` when the fixed path already holds exactly this artifact (an
/// interrupted earlier Apply after its rename). A link, a foreign owner or a
/// special file at a fixed path stops publication.
fn already_published(destination: &Directory, name: &str, artifact: &ReleaseArtifact) -> io::Result<bool> {
    match destination.read(name) {
        Ok(file) => {
            let metadata = file.metadata()?;
            if metadata.mode() & 0o7777 != 0o755 || metadata.len() != artifact.size_bytes { return Ok(false); }
            Ok(verify_fd(file, artifact.size_bytes, artifact.sha256).is_ok())
        }
        Err(duduclaw_updated::Error::Io(error)) if missing(&error) => Ok(false),
        Err(error) => Err(updated(error)),
    }
}
/// Copy one artifact from its generation descriptor to `<name>.next`, hashing
/// the bytes as they are copied, fsync, rename over the fixed path, fsync the
/// directory. The source is re-verified at the moment of use, never earlier.
/// `before_rename` runs right before the rename (fault-injection seam).
fn publish_component(generation: &Directory, artifact: &ReleaseArtifact, destination: &Directory,
    before_rename: &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> {
    if artifact.mode != 0o755 { return Err(invalid()); }
    let name = artifact_name(&artifact.install_target).map_err(updated)?;
    let mut source = generation.read(&name).map_err(updated)?;
    let metadata = source.metadata()?;
    if metadata.mode() & 0o7777 != 0o755 || metadata.len() != artifact.size_bytes { return Err(invalid()); }
    if already_published(destination, &name, artifact)? { return Ok(()); }
    let temporary = format!("{name}.next");
    match destination.remove(&temporary) {
        Ok(()) => {}
        Err(duduclaw_updated::Error::Io(error)) if missing(&error) => {}
        Err(error) => return Err(updated(error)),
    }
    let mut output = destination.create(&temporary).map_err(updated)?;
    let copied = output.set_permissions(std::fs::Permissions::from_mode(0o755))
        .and_then(|()| copy_artifact(&mut source, &mut output, artifact).map_err(updated));
    drop(output);
    if let Err(error) = copied {
        let _ = destination.remove(&temporary);
        return Err(error);
    }
    before_rename()?;
    destination.rename(&temporary, destination, &name, true).map_err(updated)?;
    let landed = destination.read(&name).map_err(updated)?.metadata()?;
    if landed.mode() & 0o7777 != 0o755 || landed.len() != artifact.size_bytes { return Err(invalid()); }
    Ok(())
}

impl Effects for InstalledEffects {
    /// Called twice by the updater: before the generation is staged (its
    /// preflight precedes the download; then only the request and the fixed
    /// destinations can be checked, and `Apply` verifies everything), and
    /// after staging, when every artifact is verified from its descriptor.
    fn prepare(&mut self, preparation: &Preparation) -> io::Result<()> {
        no_migrations(preparation)?;
        runtime_entries_shape(preparation)?;
        self.publication_directories()?;
        let Some(directory) = self.generation_directory(preparation.generation)? else { return Ok(()) };
        let (_, manifest) = self.manifest(&directory, preparation.generation)?;
        bound(preparation, &manifest)?;
        for artifact in &manifest.artifacts { verify_staged(&directory, artifact)?; }
        Ok(())
    }
    fn verify(&mut self, generation: Sha256Digest, preparation: Option<&Preparation>) -> io::Result<Sha256Digest> {
        let directory = self.generation_directory(generation)?.ok_or_else(invalid)?;
        let (bytes, manifest) = self.manifest(&directory, generation)?;
        // Production: `verify_recorded` = `TrustLedger::verify_installed`, which
        // refuses a manifest whose signing key the current root revoked.
        self.authenticate(&directory, &bytes, &manifest)?;
        #[cfg(test)]
        if self.revoked.contains(&generation) { return Err(invalid()); }
        match preparation {
            Some(preparation) => bound(preparation, &manifest)?,
            // Start-up recovery has no request: the manifest itself must need none.
            None => no_migrations(&Preparation::for_manifest(&manifest, generation))?,
        }
        for (target, _) in publication() { fixed_artifact(&manifest, &target)?; }
        // Everything before the journal, so a bad artifact cannot stop a
        // publication half way (the copy re-verifies each one at its use).
        for artifact in &manifest.artifacts { verify_staged(&directory, artifact)?; }
        Ok(Sha256Digest(Sha256::digest(&bytes).into()))
    }
    fn publish(&mut self, generation: Sha256Digest) -> io::Result<()> {
        let directory = self.generation_directory(generation)?.ok_or_else(invalid)?;
        let (_, manifest) = self.manifest(&directory, generation)?;
        #[cfg(test)]
        let mut budget = self.power_loss_after.take();
        let mut placed = Vec::new();
        for (target, relative) in publication() {
            let artifact = fixed_artifact(&manifest, &target)?;
            let destination = Directory::open(&self.publish_root.join(relative)).map_err(updated)?;
            let mut before_rename = || -> io::Result<()> {
                #[cfg(test)]
                if let Some(remaining) = budget.as_mut() {
                    if *remaining == 0 { return Err(io::Error::other("simulated power loss")); }
                    *remaining -= 1;
                }
                Ok(())
            };
            publish_component(&directory, artifact, &destination, &mut before_rename)?;
            placed.push((destination, artifact_name(&target).map_err(updated)?));
        }
        // Durable before the journal may say `published`, including files an
        // interrupted earlier attempt renamed but did not get to sync.
        for (destination, name) in &placed {
            destination.read(name).map_err(updated)?.sync_all()?;
            destination.sync().map_err(updated)?;
        }
        Ok(())
    }
    fn published(&mut self, generation: Sha256Digest) -> io::Result<()> {
        let directory = self.generation_directory(generation)?.ok_or_else(invalid)?;
        let (_, manifest) = self.manifest(&directory, generation)?;
        for (target, relative) in publication() {
            let artifact = fixed_artifact(&manifest, &target)?;
            let destination = Directory::open(&self.publish_root.join(relative)).map_err(updated)?;
            if artifact.mode != 0o755 || !already_published(&destination, &artifact_name(&target).map_err(updated)?, artifact)? {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>> {
        let install = Directory::open(&self.install_root).map_err(updated)?;
        match duduclaw_updated::linux::activation::read_current_at(&install) {
            Ok(generation) => Ok(Some(generation)),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => Ok(None),
            Err(error) => Err(updated(error)),
        }
    }
    fn readback(&mut self, generation: Sha256Digest, live: bool) -> io::Result<Vec<ComponentReadback>> {
        let directory = self.generation_directory(generation)?.ok_or_else(invalid)?;
        let (_, manifest) = self.manifest(&directory, generation)?;
        let trampoline = Directory::open(&self.publish_root.join("usr/lib/duduclaw")).and_then(|d| d.read("duduclaw-run"))
            .map_err(updated).and_then(|file| digest_file(file, Some(0o755)));
        // SAFETY: geteuid has no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Ok(components(&manifest, Observed { own_image: self.own_image, live, euid, trampoline: trampoline.ok() }))
    }
    fn workloads(&mut self) -> io::Result<Workloads> { system_census() }
    fn gated_units_down(&mut self) -> io::Result<bool> { system_gated_units_down() }
}

/// What this process observed for its readback.
pub(crate) struct Observed {
    /// SHA-256 of `/proc/self/exe`, computed at start; `None` if unreadable.
    pub own_image: Option<Sha256Digest>,
    pub live: bool,
    pub euid: u32,
    /// SHA-256 of the published `/usr/lib/duduclaw/duduclaw-run`, by descriptor.
    pub trampoline: Option<Sha256Digest>,
}

/// spawnd's readback: itself, the trampoline it executes, and every other
/// non-daemon component of the generation. Never the gateway, egress or the
/// updater slot (they answer for themselves). A field that cannot be computed
/// makes its record `ready: false`; no value is guessed.
pub(crate) fn components(manifest: &ReleaseManifest, observed: Observed) -> Vec<ComponentReadback> {
    let unknown = ReleaseVersion { major: 0, minor: 0, patch: 0 };
    let mut out = Vec::new();
    for artifact in &manifest.artifacts {
        let record = |product, version: Option<ReleaseVersion>, protocol, build: Option<Sha256Digest>, uid: Option<u32>, ready: bool| {
            let complete = version.is_some() && build.is_some() && uid.is_some();
            ComponentReadback { artifact_id: artifact.id.clone(), target: artifact.install_target.clone(), product,
                version: version.unwrap_or(unknown), protocol, build_sha256: build.unwrap_or(Sha256Digest([0; 32])),
                effective_uid: uid.unwrap_or(u32::MAX), ready: ready && complete }
        };
        out.push(match &artifact.install_target {
            InstallTarget::Gateway | InstallTarget::Egress | InstallTarget::UpdateSlot => continue,
            // The running image and this process's own identity and liveness.
            InstallTarget::Spawnd => record(PRODUCT, duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).ok(),
                duduclaw_isolation::ipc::PROTOCOL_VERSION, observed.own_image, Some(observed.euid),
                observed.live && observed.euid == 0),
            // The bytes this root process executes as the trampoline. Product,
            // version and protocol are the signed facts of exactly these bytes,
            // so they are reported only when the observed digest is the artifact's.
            InstallTarget::Trampoline => {
                let matched = observed.trampoline == Some(artifact.sha256);
                record(manifest.product, matched.then_some(manifest.version), manifest.protocols.trampoline,
                    observed.trampoline, Some(observed.euid), matched && observed.euid == 0)
            }
            // Non-daemon, non-root components (parse worker, MCP bridge, importer,
            // migration, egress forwarder, runtime entries): no executable
            // readback under a non-root identity exists yet, so their identity
            // cannot be observed and they are not ready.
            _ => record(manifest.product, None, 0, None, None, false),
        });
    }
    out
}

/// SHA-256 of an executable by descriptor, bounded; `mode`: required permission bits.
fn digest_file(mut file: File, mode: Option<u32>) -> io::Result<Sha256Digest> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_EXECUTABLE_BYTES
        || mode.is_some_and(|mode| metadata.mode() & 0o7777 != mode) { return Err(invalid()); }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 { break; }
        total = total.checked_add(count as u64).filter(|total| *total <= MAX_EXECUTABLE_BYTES).ok_or_else(invalid)?;
        hash.update(buffer.get(..count).ok_or_else(invalid)?);
    }
    Ok(Sha256Digest(hash.finalize().into()))
}
/// The running image (`/proc/self/exe`), never a path to an installed copy.
fn running_image() -> io::Result<Sha256Digest> { digest_file(File::open("/proc/self/exe")?, None) }

#[cfg(test)]
#[path = "update_control_tests.rs"]
mod tests;
