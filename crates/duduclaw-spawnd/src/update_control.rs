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
//! admission iteration.
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
//!   process (boot id, pid, start time): `duduclaw-spawnd wait-consistent`
//!   ([`wait_consistent`], unit `duduclaw-isolation-ready.service`) completes
//!   only then, and gateway and egress are ordered after that unit.
//! Every state change is also one fixed-vocabulary `STATUS=` line to systemd
//! and stderr (journal): `consistent`, `recovering`, `inconsistent: <reason>`.
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
/// Schema of the status file (2: `set`, `closed_reason`, `owner`).
const STATUS_SCHEMA: u16 = 2;
const MAX_STATUS_BYTES: usize = 16 * 1024;
/// spawnd's exit status after an `Apply` repaired a set it started on
/// inconsistent. The unit restarts exactly this status
/// (`RestartForceExitStatus=`, `SuccessExitStatus=`); see the unit file.
pub const RESTART_AFTER_REPAIR_EXIT: i32 = 82;
/// Audit rate-limit source of this endpoint (0: control socket, 1: host).
const AUDIT_SOURCE: u8 = 2;
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

/// The process a status file belongs to. pid and start time (field 22 of
/// `/proc/<pid>/stat`, clock ticks after boot) identify one process within one
/// boot; a status whose owner is not the live spawnd is stale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner { pub boot_id: String, pub pid: u32, pub start_time: u64 }
impl Owner {
    pub(crate) fn current() -> io::Result<Self> {
        let pid = std::process::id();
        Ok(Self { boot_id: boot_id()?, pid, start_time: process_start_time(pid)? })
    }
}
fn boot_id() -> io::Result<String> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let id = text.trim();
    if id.len() != 36 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') { return Err(invalid()); }
    Ok(id.to_owned())
}
/// Field 22 (`starttime`) of `/proc/<pid>/stat`. The command name (field 2)
/// may contain spaces and parentheses, so fields are counted after the last `)`.
fn process_start_time(pid: u32) -> io::Result<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, fields) = text.rsplit_once(')').ok_or_else(invalid)?;
    // `fields` begins with field 3 (state): starttime is the 20th from there.
    fields.split_ascii_whitespace().nth(19).ok_or_else(invalid)?.parse().map_err(|_| invalid())
}

/// Root-only operator status (`/run/duduclaw-spawnd/update-status.json`):
/// rewritten at start, when everything is opened, and after every served
/// request. Digests are lowercase hex.
#[derive(Debug, Serialize)]
pub(crate) struct UpdateStatus {
    schema: u16,
    /// `consistent` (verified at this process's start and fully opened, journal
    /// absent or `published`), `recovering` (verified, not yet opened; or a
    /// repaired set waiting for the restart) or `inconsistent`.
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
    let manifest = effects.verify(generation, preparation)?;
    if expected_manifest.is_some_and(|expected| expected != manifest) { return Err(invalid()); }
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
    /// Not consistent at start: only the update endpoint is open, no admission
    /// loop exists, so no run can be admitted by this process.
    Closed,
    /// Started `Closed`, then an `Apply` repaired the set: this process exits
    /// with [`RESTART_AFTER_REPAIR_EXIT`] once the reply is sent.
    RestartPending,
}
/// What the accept loop does after one served connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum After { Continue, RestartAfterRepair }

/// The protocol state machine. Requests are served one at a time by one thread,
/// so `Drain`, `Apply` and `Resume` never interleave.
pub struct UpdateControl<S, E> { store: S, barrier: DrainBarrier, effects: E, startup: Startup, phase: Phase,
    owner: Option<Owner>,
    /// Where [`Self::report`] writes the status file (`None`: not written).
    status_root: Option<PathBuf>,
    /// The last `STATUS=` line announced (announced again only when it changes).
    reported: Option<String> }
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
            owner: Owner::current().ok(), status_root: None, reported: None })
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
    /// The admission loop of this process runs (or is about to).
    fn admission_live(&self) -> bool { matches!(self.phase, Phase::Verified | Phase::Open) }
    pub(crate) fn status(&mut self) -> UpdateStatus {
        let (barrier, barrier_generation) = barrier_words(self.barrier.closure());
        let (startup_barrier, startup_barrier_generation) = barrier_words(self.startup.drain);
        let (journal, journal_generation, journal_previous, publication_consistent) = match self.store.load_journal() {
            Ok(None) => ("absent", None, None, true),
            Ok(Some(found)) => (if found.state == PublishState::Published { "published" } else { "publishing" },
                Some(hex(&found.generation)), found.previous.as_ref().map(hex), found.state == PublishState::Published),
            Err(_) => ("unreadable", None, None, false),
        };
        let (set, closed_reason) = match self.phase {
            Phase::Verified | Phase::RestartPending => ("recovering", None),
            Phase::Closed => ("inconsistent", Some(self.startup.recovery.closed_reason().unwrap_or("not_verified"))),
            // A later Apply of this process failed half way: report it (the waiter
            // has completed already; a restart of the waiter blocks again).
            Phase::Open if !publication_consistent =>
                ("inconsistent", Some(if journal == "unreadable" { "journal_unreadable" } else { "publication_incomplete" })),
            Phase::Open => ("consistent", None),
        };
        UpdateStatus { schema: STATUS_SCHEMA, set, closed_reason, owner: self.owner.clone(),
            admission_closed: self.barrier.is_closed(), barrier, barrier_generation, journal,
            journal_generation, journal_previous, publication_consistent, startup_barrier, startup_barrier_generation,
            startup_recovery: self.startup.recovery }
    }
    /// Write the status file (when configured) and announce the `STATUS=` line
    /// when it changed. A failed write is part of the line. Returns the line.
    pub(crate) fn report(&mut self) -> String {
        let status = self.status();
        let written = match &self.status_root {
            Some(root) => write_status(root, &status).is_ok(),
            None => true,
        };
        let line = status_line(status.set, status.closed_reason, written);
        if self.reported.as_deref() != Some(line.as_str()) {
            announce(&line);
            self.reported = Some(line.clone());
        }
        line
    }
    /// Every refusal, whatever its cause, is the single `Refused` outcome.
    pub fn respond(&mut self, request: &ControlRequest) -> ControlResponse {
        let outcome = if request.schema_version == SCHEMA {
            self.handle(&request.command).unwrap_or(ControlOutcome::Refused)
        } else { ControlOutcome::Refused };
        ControlResponse { schema_version: SCHEMA, nonce: request.nonce, generation: request.command.generation(), outcome }
    }
    fn handle(&mut self, command: &ControlCommand) -> io::Result<ControlOutcome> {
        if command.generation().0 == [0; 32] { return Err(invalid()); }
        match command {
            ControlCommand::Prepare { preparation } => { self.effects.prepare(preparation)?; Ok(ControlOutcome::Prepared) }
            ControlCommand::Drain { generation } => self.drain(*generation),
            ControlCommand::Apply { preparation } => {
                self.drained(preparation.generation)?;
                let previous = self.previous_for(preparation.generation)?;
                publish_verified(&mut self.store, &mut self.effects, preparation.generation, None, previous,
                    Some(preparation))?;
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
        // nothing was or can be admitted, and no iteration will ever report.
        if !self.admission_live() { return Ok(ControlOutcome::Drained { admission_closed: true, active_runs: 0 }); }
        // 3. Count what an admission iteration that began after the closure kept.
        let observed = self.barrier.wait_observation(epoch, started, OBSERVATION_WAIT).ok_or_else(timed_out)?;
        // An unhealthy iteration's count is not evidence of a drain (the barrier
        // stays closed and recorded; the updater retries).
        if !observed.healthy { return Err(invalid()); }
        Ok(ControlOutcome::Drained { admission_closed: true,
            active_runs: u32::try_from(observed.admitted_runs).unwrap_or(u32::MAX) })
    }
    /// Apply's precondition: closed for exactly this generation, in memory and
    /// on disk, and a fresh healthy admission iteration saw no admitted run.
    fn drained(&mut self, generation: Sha256Digest) -> io::Result<()> {
        if self.barrier.closure() != Some(DrainClosure::Generation(generation)) || self.store.load()? != Some(generation) {
            return Err(invalid());
        }
        // Without an admission loop no run exists in this process (see `drain`).
        if !self.admission_live() { return Ok(()); }
        let observed = self.barrier.wait_observation(self.barrier.epoch(), Instant::now(), OBSERVATION_WAIT)
            .ok_or_else(timed_out)?;
        if !observed.healthy || observed.admitted_runs != 0 { return Err(invalid()); }
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
/// connection closed) before a repaired set's restart is requested.
pub(crate) fn serve_and_settle<S: DrainStore + JournalStore, E: Effects>(fd: OwnedFd, peer: (u32, u32),
    control: &mut UpdateControl<S, E>) -> After {
    let _ = serve_one(fd, peer, control);
    if control.restart_required() { After::RestartAfterRepair } else { After::Continue }
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
fn accept_loop(listener: OwnedFd, control: Shared) {
    loop {
        match socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC) {
            Ok(raw) => {
                // SAFETY: accept4 returned a new descriptor owned only here.
                let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                let mut control = locked(&control);
                let after = serve_and_settle(fd, (0, 0), &mut *control);
                control.report();
                if after == After::RestartAfterRepair {
                    eprintln!("duduclaw-spawnd: set repaired; restarting to verify before opening");
                    std::process::exit(RESTART_AFTER_REPAIR_EXIT);
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
pub struct Recovered { control: UpdateControl<DiskDrainStore, InstalledEffects>, barrier: DrainBarrier, lock: File }
/// Called before any socket is bound (control, allocation, update) and before
/// admission. The lifetime flock `/var/lib/duduclaw-spawnd/update.lock` makes
/// this process the only writer of the fixed destinations even before the
/// registry singleton lock is taken. Announces `recovering` first, audits the
/// barrier restored from a record and the start-up verification, then writes
/// the status file and announces the result (a failed audit emission or status
/// write is part of the announced `STATUS=` line, never silent).
pub fn recover(audit: &crate::audit::disk::AuditWorker) -> io::Result<Recovered> {
    announce(&status_line("recovering", None, true));
    let lock = Directory::open(Path::new(DRAIN_ROOT)).and_then(|directory| directory.lock()).map_err(updated)?;
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(DiskDrainStore::production(), barrier.clone(), InstalledEffects::production())?
        .with_status_root(Path::new(STATUS_ROOT));
    let mut audited = true;
    for record in control.startup().audit_records(now_ms()) { audited &= audit.emit(AUDIT_SOURCE, record); }
    let line = control.report();
    if !audited { announce(&format!("{line}; audit_write_failed")); }
    Ok(Recovered { control, barrier, lock })
}
impl Recovered {
    /// Every fixed component holds one verified generation's bytes. `false`:
    /// spawnd opens nothing but the update endpoint.
    pub fn consistent(&self) -> bool { self.control.consistent_at_start() }
    /// Bind `update.sock` and serve it on a dedicated thread (which keeps the
    /// update lock). The returned handle carries the barrier for the admission
    /// loop and marks the set open ([`Serving::opened`]).
    pub fn serve(self) -> io::Result<Serving> {
        let Self { control, barrier, lock } = self;
        let listener = bind_listener()?;
        let control: Shared = Arc::new(Mutex::new(control));
        let served = control.clone();
        let thread = std::thread::Builder::new().name("spawnd-update-control".into())
            .spawn(move || { let _lock = lock; accept_loop(listener, served) })?;
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

/// `duduclaw-spawnd wait-consistent` (unit `duduclaw-isolation-ready.service`):
/// a fresh, matching, consistent status file exists. Fresh and matching: the
/// `owner` is this boot (`/proc/sys/kernel/random/boot_id`), and `live(pid)`
/// returns the start time of a live spawnd with that pid equal to the
/// recorded one (a status of an earlier spawnd process, even with a recycled
/// pid, never matches). Missing, unreadable, unparsable, ownerless, stale or
/// not `consistent`: `false`.
pub(crate) fn status_accepted(bytes: Option<&[u8]>, boot: &str, live: impl Fn(u32) -> Option<u64>) -> bool {
    #[derive(Deserialize)]
    struct View { schema: u16, set: String, owner: Option<Owner> }
    let Some(bytes) = bytes.filter(|bytes| bytes.len() <= MAX_STATUS_BYTES) else { return false };
    let Ok(view) = serde_json::from_slice::<View>(bytes) else { return false };
    let Some(owner) = view.owner else { return false };
    view.schema == STATUS_SCHEMA && view.set == "consistent" && owner.boot_id == boot
        && live(owner.pid) == Some(owner.start_time)
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
/// The start time of `pid` when it is a process of `duduclaw-spawnd.service`
/// (its cgroup v2 path ends in that unit), otherwise `None`.
fn live_spawnd(pid: u32) -> Option<u64> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let in_unit = cgroup.lines().filter_map(|line| line.strip_prefix("0::"))
        .any(|path| path.rsplit('/').next() == Some("duduclaw-spawnd.service"));
    if !in_unit { return None; }
    process_start_time(pid).ok()
}
/// The root-only status file, read through `Directory` (root-owned ancestors,
/// no symlink, root-owned single-link regular file).
fn read_status() -> Option<Vec<u8>> {
    Directory::open(Path::new(STATUS_ROOT)).and_then(|directory| directory.bytes(STATUS_FILE, MAX_STATUS_BYTES)).ok()
}
/// Returns `Ok` only once spawnd reports the verified, opened, consistent set.
/// Waits indefinitely otherwise; an unreadable boot id is an error (never `Ok`).
/// Connects to nothing: it only reads the root-only status file and `/proc`.
pub fn wait_consistent() -> io::Result<()> {
    let boot = boot_id()?;
    eprintln!("duduclaw-spawnd: waiting for a consistent isolated set (see {STATUS_ROOT}/{STATUS_FILE})");
    wait_until(|| status_accepted(read_status().as_deref(), &boot, live_spawnd), None, Duration::from_millis(500))?;
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
