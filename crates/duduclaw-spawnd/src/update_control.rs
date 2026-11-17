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
//! [`recover`] runs at spawnd start before any socket is bound and before
//! admission: `publishing` is rolled forward after full re-verification, or
//! rolled back to `previous` with the same procedure; when neither completes,
//! or the journal cannot be read, admission stays closed
//! ([`DrainClosure::Recovery`]), `READY=1` is withheld, and the root-only status
//! file `/run/duduclaw-spawnd/update-status.json` says so.
#![allow(unsafe_code)]
use std::{fs::File, io::{self, Read}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt, PermissionsExt}},
    path::{Path, PathBuf}, time::{Duration, Instant}};
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
                generation: self.journal.map(|journal| journal.generation),
                previous: self.journal.and_then(|journal| journal.previous) }));
        }
        records
    }
}

/// Root-only operator status (`/run/duduclaw-spawnd/update-status.json`):
/// rewritten at start and after every served request. Digests are lowercase hex.
#[derive(Debug, Serialize)]
pub(crate) struct UpdateStatus {
    schema: u16,
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

/// Start-up completion of an interrupted publication (plan R6 addendum).
fn recover_journal<S: JournalStore, E: Effects>(store: &mut S, effects: &mut E, journal: PublishJournal)
    -> PublicationRecovery {
    if journal.state == PublishState::Published { return PublicationRecovery::AlreadyPublished; }
    // Roll forward: the same generation, re-verified, with the recorded manifest.
    if publish_verified(store, effects, journal.generation, Some(journal.manifest_sha256), journal.previous, None).is_ok() {
        return PublicationRecovery::RolledForward;
    }
    // Roll back with the same procedure. The new journal names the previous
    // generation, and the interrupted one as its own fallback.
    match journal.previous {
        Some(previous) if publish_verified(store, effects, previous, None, Some(journal.generation), None).is_ok() =>
            PublicationRecovery::RolledBack,
        _ => PublicationRecovery::Incomplete,
    }
}

/// The protocol state machine. Requests are served one at a time by one thread,
/// so `Drain`, `Apply` and `Resume` never interleave.
pub struct UpdateControl<S, E> { store: S, barrier: DrainBarrier, effects: E, startup: Startup }
impl<S: DrainStore + JournalStore, E: Effects> UpdateControl<S, E> {
    /// Restores the barrier from the durable record, then settles the
    /// publication journal (roll forward, roll back, or not ready). Call before
    /// any socket is bound and before admission starts.
    pub fn new(mut store: S, barrier: DrainBarrier, mut effects: E) -> io::Result<Self> {
        let drain = match store.load() {
            Ok(None) => None,
            Ok(Some(generation)) => Some(DrainClosure::Generation(generation)),
            Err(_) => Some(DrainClosure::Unreadable),
        };
        if let Some(closure) = drain { barrier.close(closure).ok_or_else(invalid)?; }
        let (journal, recovery) = match store.load_journal() {
            Ok(None) => (None, PublicationRecovery::NoJournal),
            Ok(Some(journal)) => (Some(journal), recover_journal(&mut store, &mut effects, journal)),
            Err(_) => (None, PublicationRecovery::JournalUnreadable),
        };
        // A possibly mixed set: closed whatever the record said; only a new Drain re-keys it.
        if !recovery.consistent() { barrier.close(DrainClosure::Recovery).ok_or_else(invalid)?; }
        Ok(Self { store, barrier, effects, startup: Startup { drain, recovery, journal } })
    }
    pub fn startup(&self) -> Startup { self.startup }
    pub(crate) fn status(&mut self) -> UpdateStatus {
        let (barrier, barrier_generation) = barrier_words(self.barrier.closure());
        let (startup_barrier, startup_barrier_generation) = barrier_words(self.startup.drain);
        let (journal, journal_generation, journal_previous, publication_consistent) = match self.store.load_journal() {
            Ok(None) => ("absent", None, None, true),
            Ok(Some(found)) => (if found.state == PublishState::Published { "published" } else { "publishing" },
                Some(hex(&found.generation)), found.previous.as_ref().map(hex), found.state == PublishState::Published),
            Err(_) => ("unreadable", None, None, false),
        };
        UpdateStatus { schema: 1, admission_closed: self.barrier.is_closed(), barrier, barrier_generation, journal,
            journal_generation, journal_previous, publication_consistent, startup_barrier, startup_barrier_generation,
            startup_recovery: self.startup.recovery }
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

/// Bind `update.sock` (root 0600) in the root-owned `/run/duduclaw-spawnd`.
/// The caller holds the registry singleton lock, so an existing root socket
/// is a dead spawnd's endpoint; any other file at the path stops startup.
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

/// `notified`: `READY=1` was already sent (start-up recovery was consistent).
/// Otherwise it is sent once a served request (an operator's or the updater's
/// `Drain` + `Apply`) leaves a consistent set and a re-keyed barrier.
fn accept_loop<S: DrainStore + JournalStore, E: Effects>(listener: OwnedFd, mut control: UpdateControl<S, E>,
    mut notified: bool) {
    loop {
        match socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC) {
            Ok(raw) => {
                // SAFETY: accept4 returned a new descriptor owned only here.
                let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                let _ = serve_one(fd, (0, 0), &mut control);
                let status = control.status();
                if !notified && status.publication_consistent && status.barrier != "recovery_incomplete" {
                    notified = notify("READY=1").is_ok();
                }
                // Diagnostic only: a failed status write never changes a decision.
                let _ = write_status(Path::new(STATUS_ROOT), &status);
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
/// restored and the publication journal settled. [`Recovered::serve`] binds.
pub struct Recovered { control: UpdateControl<DiskDrainStore, InstalledEffects>, barrier: DrainBarrier, lock: File }
/// Called before any socket is bound (control, allocation, update) and before
/// admission. The lifetime flock `/var/lib/duduclaw-spawnd/update.lock` makes
/// this process the only writer of the fixed destinations even before the
/// registry singleton lock is taken. Audits the barrier restored from a record
/// and any journal found, and writes the status file (best effort).
pub fn recover(audit: &crate::audit::disk::AuditWorker) -> io::Result<Recovered> {
    let lock = Directory::open(Path::new(DRAIN_ROOT)).and_then(|directory| directory.lock()).map_err(updated)?;
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(DiskDrainStore::production(), barrier.clone(), InstalledEffects::production())?;
    for record in control.startup().audit_records(now_ms()) { let _ = audit.emit(AUDIT_SOURCE, record); }
    let _ = write_status(Path::new(STATUS_ROOT), &control.status());
    Ok(Recovered { control, barrier, lock })
}
impl Recovered {
    /// Every fixed component holds one generation's bytes. `false`: admission
    /// stays closed and `READY=1` must be withheld (units ordered after spawnd
    /// do not start on a mixed set); the update endpoint still answers root.
    pub fn ready(&self) -> bool { self.control.startup().recovery.consistent() }
    /// Bind `update.sock` and serve it on a dedicated thread (which keeps the
    /// update lock); the returned barrier is attached to the admission loop.
    /// The caller sends `READY=1` when [`Self::ready`]; otherwise the thread
    /// sends it once a consistent set is restored.
    pub fn serve(self) -> io::Result<DrainBarrier> {
        let ready = self.ready();
        let Self { control, barrier, lock } = self;
        let listener = bind_listener()?;
        std::thread::Builder::new().name("spawnd-update-control".into())
            .spawn(move || { let _lock = lock; accept_loop(listener, control, ready) })?;
        Ok(barrier)
    }
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
}
impl InstalledEffects {
    pub fn production() -> Self {
        Self { install_root: PathBuf::from(INSTALL_ROOT), publish_root: PathBuf::from("/"), trust: Trust::Recorded,
            own_image: running_image().ok(), #[cfg(test)] power_loss_after: None }
    }
    #[cfg(test)]
    fn for_test(install_root: &Path, publish_root: &Path) -> Self {
        Self { install_root: install_root.to_path_buf(), publish_root: publish_root.to_path_buf(),
            trust: Trust::Unsigned, own_image: running_image().ok(), power_loss_after: None }
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
        self.authenticate(&directory, &bytes, &manifest)?;
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
