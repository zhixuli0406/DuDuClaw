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
//!   both name `g` (or nothing), the record is removed durably first, then
//!   admission reopens.
//!
//! `Apply` publishes only from the verified generation directory, hashing each
//! artifact from its descriptor while copying it, and only under the closed,
//! recorded barrier of that generation with no admitted run.
#![allow(unsafe_code)]
use std::{fs::File, io::{self, Read}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt, PermissionsExt}},
    path::{Path, PathBuf}, time::{Duration, Instant}};
use duduclaw_isolation::{ids::Sha256Digest, update::{InstallTarget, Product, ReleaseArtifact, ReleaseManifest, ReleaseVersion}};
use duduclaw_updated::{activation_contract::{ComponentReadback, ControlCommand, ControlOutcome, ControlRequest, ControlResponse, Preparation},
    linux::{activation::SPAWND_UPDATE_SOCKET, files::{copy_artifact, verify_fd, Directory}, transport, INSTALL_ROOT},
    publication::artifact_name};
use nix::{errno::Errno, sys::socket::{self, sockopt, Backlog, SockFlag, UnixAddr}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::admission::{DrainBarrier, DrainClosure};

pub const DRAIN_ROOT: &str = "/var/lib/duduclaw-spawnd";
pub const DRAIN_RECORD: &str = "drain.json";
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
    fn clear(&mut self) -> io::Result<()> {
        let directory = self.directory()?;
        match directory.remove(DRAIN_RECORD) {
            // unlinkat, then the directory is synced by `remove`.
            Ok(()) => Ok(()),
            Err(duduclaw_updated::Error::Io(error)) if missing(&error) => directory.sync().map_err(updated),
            Err(error) => Err(updated(error)),
        }
    }
}

/// Generation effects behind the control protocol (production: [`InstalledEffects`]).
pub trait Effects {
    fn prepare(&mut self, preparation: &Preparation) -> io::Result<()>;
    fn apply(&mut self, preparation: &Preparation) -> io::Result<()>;
    /// `live`: this spawnd's admission loop completed a healthy iteration recently.
    fn readback(&mut self, generation: Sha256Digest, live: bool) -> io::Result<Vec<ComponentReadback>>;
}

/// The protocol state machine. Requests are served one at a time by one thread,
/// so `Drain`, `Apply` and `Resume` never interleave.
pub struct UpdateControl<S, E> { store: S, barrier: DrainBarrier, effects: E }
impl<S: DrainStore, E: Effects> UpdateControl<S, E> {
    /// Restores the barrier from the durable record. Call before admission starts.
    pub fn new(mut store: S, barrier: DrainBarrier, effects: E) -> io::Result<Self> {
        let closure = match store.load() {
            Ok(None) => None,
            Ok(Some(generation)) => Some(DrainClosure::Generation(generation)),
            Err(_) => Some(DrainClosure::Unreadable),
        };
        if let Some(closure) = closure { barrier.close(closure).ok_or_else(invalid)?; }
        Ok(Self { store, barrier, effects })
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
                self.effects.apply(preparation)?;
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
        Ok(ControlOutcome::Drained { admission_closed: true,
            active_runs: u32::try_from(observed.admitted_runs).unwrap_or(u32::MAX) })
    }
    /// Apply's precondition: closed for exactly this generation, in memory and
    /// on disk, and a fresh admission iteration saw no admitted run.
    fn drained(&mut self, generation: Sha256Digest) -> io::Result<()> {
        if self.barrier.closure() != Some(DrainClosure::Generation(generation)) || self.store.load()? != Some(generation) {
            return Err(invalid());
        }
        let observed = self.barrier.wait_observation(self.barrier.epoch(), Instant::now(), OBSERVATION_WAIT)
            .ok_or_else(timed_out)?;
        if observed.admitted_runs != 0 { return Err(invalid()); }
        Ok(())
    }
    fn resume(&mut self, generation: Sha256Digest) -> io::Result<ControlOutcome> {
        let matches = match self.barrier.closure() {
            None => true,
            Some(DrainClosure::Generation(closed)) => closed == generation,
            Some(DrainClosure::Unreadable) => false,
        };
        if !matches { return Err(invalid()); }
        match self.store.load()? {
            None => {}
            Some(recorded) if recorded == generation => {}
            Some(_) => return Err(invalid()),
        }
        // 1. Durable removal first; 2. then admission reopens.
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
pub(crate) fn serve_one<S: DrainStore, E: Effects>(fd: OwnedFd, peer: (u32, u32), control: &mut UpdateControl<S, E>)
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

fn accept_loop<S: DrainStore, E: Effects>(listener: OwnedFd, mut control: UpdateControl<S, E>) {
    loop {
        match socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC) {
            Ok(raw) => {
                // SAFETY: accept4 returned a new descriptor owned only here.
                let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                let _ = serve_one(fd, (0, 0), &mut control);
            }
            Err(Errno::EAGAIN | Errno::EINTR | Errno::ECONNABORTED) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Restore the barrier from `drain.json`, bind `update.sock` and serve it on a
/// dedicated thread. Called after the registry lock is held and before the
/// admission loop starts; the returned barrier is attached to that loop.
pub fn start() -> io::Result<DrainBarrier> {
    let barrier = DrainBarrier::default();
    let control = UpdateControl::new(DiskDrainStore::production(), barrier.clone(), InstalledEffects::production())?;
    let listener = bind_listener()?;
    std::thread::Builder::new().name("spawnd-update-control".into())
        .spawn(move || accept_loop(listener, control))?;
    Ok(barrier)
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
pub struct InstalledEffects { install_root: PathBuf, publish_root: PathBuf, trust: Trust, own_image: Option<Sha256Digest> }
impl InstalledEffects {
    pub fn production() -> Self {
        Self { install_root: PathBuf::from(INSTALL_ROOT), publish_root: PathBuf::from("/"), trust: Trust::Recorded,
            own_image: running_image().ok() }
    }
    #[cfg(test)]
    fn for_test(install_root: &Path, publish_root: &Path) -> Self {
        Self { install_root: install_root.to_path_buf(), publish_root: publish_root.to_path_buf(),
            trust: Trust::Unsigned, own_image: running_image().ok() }
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
fn publish(generation: &Directory, artifact: &ReleaseArtifact, destination: &Directory) -> io::Result<()> {
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
    fn apply(&mut self, preparation: &Preparation) -> io::Result<()> {
        no_migrations(preparation)?;
        let directory = self.generation_directory(preparation.generation)?.ok_or_else(invalid)?;
        let (bytes, manifest) = self.manifest(&directory, preparation.generation)?;
        self.authenticate(&directory, &bytes, &manifest)?;
        bound(preparation, &manifest)?;
        for (target, relative) in publication() {
            let mut matching = manifest.artifacts.iter().filter(|artifact| artifact.install_target == target);
            let (Some(artifact), None) = (matching.next(), matching.next()) else { return Err(invalid()) };
            let destination = Directory::open(&self.publish_root.join(relative)).map_err(updated)?;
            publish(&directory, artifact, &destination)?;
        }
        Ok(())
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
