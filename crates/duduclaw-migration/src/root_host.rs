//! Root filesystem half of the migration transaction. No legacy parser runs here.
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}, path::{Component, Path, PathBuf}};
use duduclaw_isolation::{ids::RequestId, migration::MigrationState};
use rand::{RngCore, rngs::OsRng};
use rustix::{fd::{AsFd, OwnedFd}, fs::{self as rfs, FlockOperation, Mode, OFlags}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[path = "native_stop.rs"]
pub mod native_stop;
#[path = "root_bootstrap.rs"]
pub mod bootstrap;
#[path = "root_pipeline.rs"]
pub mod pipeline;

use crate::{Error, archive::{ArchiveWriter, decrypt}, journal::{Evidence, HostAdapter, Journal, StageOperation}, snapshot};

/// Installed by the signed bootstrap, never obtained from legacy data or the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootPlan {
    pub migration: RequestId,
    pub state_dir: PathBuf,
    pub legacy_home: PathBuf,
    pub root_key_path: PathBuf,
    pub gateway_uid: u32,
    pub gateway_gid: u32,
    pub snapshot_content_limit: u64,
    pub snapshot_encoded_limit: u64,
    pub expires_at_unix: u64,
}

impl RootPlan {
    pub fn validate(&self) -> Result<(), Error> {
        if self.gateway_uid == 0 || self.gateway_gid == 0 || self.snapshot_content_limit == 0
            || self.snapshot_encoded_limit < self.snapshot_content_limit || self.expires_at_unix == 0 {
            return Err(Error::Invalid);
        }
        for path in [&self.state_dir, &self.legacy_home, &self.root_key_path] {
            if !path.is_absolute() || path.components().any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
                || path.file_name().is_none() { return Err(Error::Invalid); }
        }
        if self.state_dir == self.legacy_home || self.state_dir.starts_with(&self.legacy_home)
            || self.legacy_home.starts_with(&self.state_dir) || self.root_key_path.starts_with(&self.legacy_home)
            || self.root_key_path.starts_with(&self.state_dir) { return Err(Error::Invalid); }
        Ok(())
    }
}

/// B1/F supply activation-source lockdown and signed-service installation; D/H
/// supply import mode, immutable employee registration and isolation readback.
/// Calls MUST be durable/idempotent on StageOperation and return actual readback.
/// A missing implementation refuses; no journal receipt can stand in for readback.
pub trait RootMigrationIntegration {
    /// Prove all required adapters/services are installed BEFORE stopping the old
    /// environment. The explicit prepare-only operator command is the exception.
    fn materialization_ready(&mut self, _plan: &RootPlan) -> Result<bool, Error> { Ok(false) }
    fn preflight(&mut self, _plan: &RootPlan) -> Result<(), Error> { Err(Error::Unavailable) }
    fn readback(&mut self, operation: StageOperation, plan: &RootPlan) -> Result<Option<Evidence>, Error>;
    fn perform(&mut self, operation: StageOperation, plan: &RootPlan) -> Result<(), Error>;
    /// Prove the new gateway uid cannot read the quarantined tree. The final
    /// IsolationVerified readback must also check the effective unit namespace.
    fn gateway_denied_legacy(&mut self, plan: &RootPlan) -> Result<bool, Error>;
    /// Live readback of every proposal and accepted content record. A receipt
    /// for an archived stream, or a past Complete state, is not sufficient.
    fn retention_ready(&mut self, _plan: &RootPlan) -> Result<bool, Error> { Err(Error::Unavailable) }
}

pub struct MissingIntegration;
impl RootMigrationIntegration for MissingIntegration {
    fn readback(&mut self, _: StageOperation, _: &RootPlan) -> Result<Option<Evidence>, Error> { Err(Error::Unavailable) }
    fn perform(&mut self, _: StageOperation, _: &RootPlan) -> Result<(), Error> { Err(Error::Unavailable) }
    fn gateway_denied_legacy(&mut self, _: &RootPlan) -> Result<bool, Error> { Err(Error::Unavailable) }
}

fn root() -> Result<(), Error> {
    if !rustix::process::getuid().is_root() || !rustix::process::geteuid().is_root() { return Err(Error::Authentication); }
    Ok(())
}
fn path_metadata(path: &Path, directory: bool, uid: u32) -> Result<fs::Metadata, Error> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() || m.uid() != uid || m.mode() & 0o022 != 0
        || (directory && !m.is_dir()) || (!directory && (!m.is_file() || m.nlink() != 1)) {
        return Err(Error::Invalid);
    }
    Ok(m)
}
fn trusted_parents(path: &Path) -> Result<(), Error> {
    for p in path.parent().ok_or(Error::Invalid)?.ancestors() { path_metadata(p, true, 0)?; }
    Ok(())
}
fn directory(path: &Path) -> Result<OwnedFd, Error> {
    rfs::open(path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
        .map_err(|_| Error::Storage)
}
fn sync_parent(path: &Path) -> Result<(), Error> { File::open(path.parent().ok_or(Error::Invalid)?)?.sync_all()?; Ok(()) }
fn private_dir(path: &Path) -> Result<(), Error> {
    trusted_parents(path)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => sync_parent(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    if path_metadata(path, true, 0)?.mode() & 0o7777 != 0o700 { return Err(Error::Invalid); }
    Ok(())
}
fn read_private(path: &Path, limit: u64, uid: u32) -> Result<Vec<u8>, Error> {
    let mut f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path)?;
    let m = f.metadata()?;
    if !m.is_file() || m.uid() != uid || m.nlink() != 1 || m.mode() & 0o7777 != 0o600 || m.len() > limit {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut f).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err(Error::Limit); }
    Ok(bytes)
}
/// Durable atomic publication. The parent is pinned under a root-only tree or
/// the stopped gateway's freshly created home; the caller owns the root lock.
fn publish(path: &Path, bytes: &[u8], uid: u32, gid: u32, replace: bool) -> Result<(), Error> {
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_file() || m.file_type().is_symlink() || m.nlink() != 1 || (m.uid() != 0 && m.uid() != uid) {
            return Err(Error::Invalid);
        }
        if !replace {
            return if read_private(path, bytes.len() as u64, uid)? == bytes { Ok(()) } else { Err(Error::Conflict) };
        }
    }
    let mut stage = tempfile::NamedTempFile::new_in(path.parent().ok_or(Error::Invalid)?)?;
    stage.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    stage.write_all(bytes)?;
    rfs::fchown(stage.as_file().as_fd(), Some(rustix::process::Uid::from_raw(uid)), Some(rustix::process::Gid::from_raw(gid)))
        .map_err(|_| Error::Storage)?;
    stage.as_file().sync_all()?;
    if replace { stage.persist(path).map_err(|_| Error::Storage)?; }
    else { stage.persist_noclobber(path).map_err(|_| Error::Conflict)?; }
    sync_parent(path)
}
fn random_stage(path: &Path, count: usize) -> Result<Vec<u8>, Error> {
    if path.try_exists()? {
        let value = read_private(path, count as u64, 0)?;
        return if value.len() == count { Ok(value) } else { Err(Error::Invalid) };
    }
    let mut value = vec![0; count];
    OsRng.try_fill_bytes(&mut value).map_err(|_| Error::Unavailable)?;
    publish(path, &value, 0, 0, false)?;
    Ok(value)
}

/// Owns the lock from before journal open through every readback and effect.
/// No process can change the migration plan while this transaction is resumed.
pub struct RootMigration<I> {
    plan: RootPlan,
    _lock: File,
    journal: Journal,
    host: RootHost<I>,
}
struct RootHost<I> { plan: RootPlan, integration: I }

const ROTATED_FILES: [(&str, u64); 4] = [("root.key", 32), ("identity.key", 32), ("jwt_secret", 64), ("config.toml", 4096)];
const KEY_RECEIPT: &str = "keys-rotated.json";
const RETIRED_KEYS: &str = "fresh-keys.removing";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyDigest { name: String, bytes: u64, sha256: [u8; 32] }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRotationReceipt { migration: RequestId, files: Vec<KeyDigest> }

impl KeyRotationReceipt {
    fn validate(&self, migration: RequestId) -> Result<(), Error> {
        if self.migration != migration || self.files.len() != ROTATED_FILES.len() { return Err(Error::Conflict); }
        for (file, (name, limit)) in self.files.iter().zip(ROTATED_FILES) {
            if file.name != name || file.bytes == 0 || file.bytes > limit
                || (name != "config.toml" && file.bytes != limit) { return Err(Error::Invalid); }
        }
        Ok(())
    }
}

impl<I: RootMigrationIntegration> RootMigration<I> {
    pub fn open(plan: RootPlan, integration: I) -> Result<Self, Error> {
        root()?;
        plan.validate()?;
        private_dir(&plan.state_dir)?;
        trusted_parents(&plan.root_key_path)?;
        // All migrations targeting this installation share one lock, even if a
        // second command supplied a different migration id or journal directory.
        let lock_path = plan.root_key_path.parent().ok_or(Error::Invalid)?.join("migration.lock");
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path)?;
        let m = lock.metadata()?;
        if !m.is_file() || m.uid() != 0 || m.nlink() != 1 || m.mode() & 0o7777 != 0o600 { return Err(Error::Invalid); }
        rfs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Conflict)?;
        let plan_path = plan.state_dir.join("plan.json");
        let encoded = serde_json::to_vec(&plan)?;
        if plan_path.try_exists()? {
            let saved: RootPlan = serde_json::from_slice(&read_private(&plan_path, 16 * 1024, 0)?)?;
            if saved != plan { return Err(Error::Conflict); }
        } else { publish(&plan_path, &encoded, 0, 0, false)?; }
        let db_path = plan.state_dir.join("journal.db");
        if db_path.try_exists()? { path_metadata(&db_path, false, 0)?; }
        let db_file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&db_path)?;
        db_file.sync_all()?;
        let journal = Journal::from_connection(rusqlite::Connection::open(&db_path)?, plan.migration)?;
        sync_parent(&db_path)?;
        Ok(Self { plan: plan.clone(), _lock: lock, journal, host: RootHost { plan, integration } })
    }
    pub fn state(&self) -> Result<MigrationState, Error> { self.journal.state() }
    pub fn plan(&self) -> &RootPlan { &self.plan }
    /// One durable stage per call. Used by the installer for structured progress.
    pub fn advance(&mut self) -> Result<MigrationState, Error> {
        self.host.integration.preflight(&self.plan)?;
        self.advance_prepared()
    }
    fn advance_prepared(&mut self) -> Result<MigrationState, Error> {
        let next = next_stage(self.state()?).ok_or(Error::Conflict)?;
        self.journal.run_one(next, &mut self.host)?;
        if matches!(next, MigrationState::ContentImported | MigrationState::Complete) {
            self.expire(chrono::Utc::now().timestamp().max(0) as u64)?;
        }
        Ok(next)
    }
    /// Completion remains pending until all confirmation and rotation obligations
    /// have live readback. Retrying uses exactly the same operation identities.
    pub fn run(&mut self) -> Result<MigrationState, Error> {
        self.host.integration.preflight(&self.plan)?;
        loop {
            let state = self.state()?;
            if state == MigrationState::Complete {
                if !self.host.integration.retention_ready(&self.plan)? {return Err(Error::Readback);}
                self.host.remove_verified_key_staging()?;
                self.expire(chrono::Utc::now().timestamp().max(0) as u64)?;
                return Ok(state);
            }
            // Waiting for an administrator is a successful, resumable outcome,
            // not an unavailable adapter and never implicit approval.
            if state == MigrationState::AwaitingAdministrator {
                if !self.host.integration.materialization_ready(&self.plan)? { return Ok(state); }
            }
            if state == MigrationState::ContentImported {
                let op = StageOperation { migration: self.plan.migration, target: MigrationState::Complete };
                match self.host.integration.readback(op, &self.plan)? {
                    Some(evidence) if evidence.validate(MigrationState::Complete).is_ok() => {},
                    _ => return Ok(state),
                }
            }
            match self.advance() {
                Err(Error::AwaitingConfirmation) if state == MigrationState::AwaitingAdministrator => return Ok(state),
                result => {result?;},
            }
        }
    }
    /// Explicit maintenance operation: stop and quarantine the installation, then
    /// leave it OFFLINE at KeysRotated. This is not a complete migration; the CLI
    /// requires --stop-after-keys to make the deliberate outage visible.
    pub fn prepare_stop_after_keys(&mut self) -> Result<MigrationState, Error> {
        loop {
            match self.state()? {
                MigrationState::KeysRotated => return Ok(MigrationState::KeysRotated),
                MigrationState::Planned | MigrationState::OldEnvironmentStopped | MigrationState::LegacyQuarantined => {
                    self.advance_prepared()?;
                }
                _ => return Err(Error::Conflict),
            }
        }
    }
    /// Timer entry point. Remove plaintext as soon as all proposal and content
    /// receipts are terminal; keep the encrypted recovery snapshot until expiry.
    /// Even the hard deadline cannot authorize deletion of unresolved content.
    pub fn expire(&mut self, now_unix: u64) -> Result<bool, Error> {
        if matches!(self.state()?, MigrationState::Planned | MigrationState::OldEnvironmentStopped) {
            return if now_unix < self.plan.expires_at_unix { Ok(false) } else { Err(Error::Conflict) };
        }
        if !self.host.integration.retention_ready(&self.plan)? {
            return if now_unix < self.plan.expires_at_unix { Ok(false) } else { Err(Error::Readback) };
        }
        let legacy = self.plan.state_dir.join("legacy");
        let snapshot = self.plan.state_dir.join("snapshot.aead");
        if legacy.try_exists()? {
            if !self.host.snapshot_verified()? { return Err(Error::Readback); }
            let parent = directory(&self.plan.state_dir)?;
            let mut count = 0;
            remove_tree(&parent, std::ffi::OsStr::new("legacy"), 0, &mut count)?;
            rfs::fsync(&parent).map_err(|_| Error::Storage)?;
        }
        // Extracted streams contain plaintext too (including rejected content).
        // Their durable per-record receipts make them unnecessary after terminality.
        if self.plan.state_dir.join("streams").try_exists()? {
            if !self.host.snapshot_verified()? { return Err(Error::Readback); }
            let parent = directory(&self.plan.state_dir)?;
            remove_tree(&parent, std::ffi::OsStr::new("streams"), 0, &mut 0)?;
            rfs::fsync(&parent).map_err(|_| Error::Storage)?;
        }
        if now_unix < self.plan.expires_at_unix { return Ok(false); }
        // Retry after a crash is safe: once plaintext is absent, remaining
        // encrypted data and the key can be deleted independently.
        if snapshot.try_exists()? { path_metadata(&snapshot, false, 0)?; fs::remove_file(&snapshot)?; sync_parent(&snapshot)?; }
        for name in ["snapshot.key"] {
            let path = self.plan.state_dir.join(name);
            if path.try_exists()? { path_metadata(&path, false, 0)?; fs::remove_file(&path)?; sync_parent(&path)?; }
        }
        publish(&self.plan.state_dir.join("retention-expired"), b"expired\n", 0, 0, false)?;
        Ok(true)
    }
    /// Manual recovery emits inert JSONL into a new root-only directory. It never
    /// recreates devices/links/capabilities or writes into any gateway directory.
    pub fn export_records(&self, destination: &Path) -> Result<PathBuf, Error> {
        root()?;
        trusted_parents(destination)?;
        if destination.starts_with(&self.plan.legacy_home) || destination.starts_with(&self.plan.state_dir)
            || destination.try_exists()? { return Err(Error::Invalid); }
        fs::DirBuilder::new().mode(0o700).create(destination)?;
        let mut stage = tempfile::NamedTempFile::new_in(destination)?;
        let key = self.host.archive_key()?;
        decrypt(File::open(self.plan.state_dir.join("snapshot.aead"))?, &mut stage, &key,
            self.plan.migration, self.plan.snapshot_encoded_limit)?;
        stage.as_file().sync_all()?;
        let output = destination.join("recovery-records.jsonl");
        stage.persist_noclobber(&output).map_err(|_| Error::Conflict)?;
        sync_parent(&output)?;
        sync_parent(destination)?;
        Ok(output)
    }
}

pub fn next_stage(state: MigrationState) -> Option<MigrationState> {
    use MigrationState as S;
    Some(match state {
        S::Planned => S::OldEnvironmentStopped, S::OldEnvironmentStopped => S::LegacyQuarantined,
        S::LegacyQuarantined => S::KeysRotated, S::KeysRotated => S::TokenRevocationRecorded,
        S::TokenRevocationRecorded => S::EmployeeProposalsStaged, S::EmployeeProposalsStaged => S::ContentArchived,
        S::ContentArchived => S::ConfirmationReady, S::ConfirmationReady => S::IsolationVerified,
        S::IsolationVerified => S::AwaitingAdministrator, S::AwaitingAdministrator => S::ContentImported, S::ContentImported => S::Complete,
        S::Complete => return None,
    })
}

impl<I: RootMigrationIntegration> RootHost<I> {
    fn archive_key(&self) -> Result<[u8; 32], Error> {
        read_private(&self.plan.state_dir.join("snapshot.key"), 32, 0)?.try_into().map_err(|_| Error::Invalid)
    }
    fn snapshot_verified(&self) -> Result<bool, Error> {
        let path = self.plan.state_dir.join("snapshot.aead");
        if !path.try_exists()? { return Ok(false); }
        path_metadata(&path, false, 0)?;
        decrypt(File::open(path)?, std::io::sink(), &self.archive_key()?, self.plan.migration,
            self.plan.snapshot_encoded_limit)?;
        Ok(true)
    }
    fn stopped(&mut self) -> Result<bool, Error> {
        Ok(matches!(self.integration.readback(StageOperation { migration: self.plan.migration,
            target: MigrationState::OldEnvironmentStopped }, &self.plan)?,
            Some(Evidence::Stopped { activation_sources_blocked: true, uid_empty: true })))
    }
    fn quarantine_and_snapshot(&mut self) -> Result<(), Error> {
        if !self.stopped()? { return Err(Error::Readback); }
        let parent_path = self.plan.legacy_home.parent().ok_or(Error::Invalid)?;
        // Only this enclosing directory changes owner. Never chown a legacy tree.
        trusted_parents(parent_path)?;
        let parent = directory(parent_path)?;
        let parent_stat = rfs::fstat(&parent).map_err(|_| Error::Storage)?;
        if parent_stat.st_uid != 0 && parent_stat.st_uid != self.plan.gateway_uid { return Err(Error::Invalid); }
        rfs::fchown(&parent, Some(rustix::process::Uid::ROOT), Some(rustix::process::Gid::ROOT)).map_err(|_| Error::Storage)?;
        rfs::fchmod(&parent, Mode::from_bits_truncate(0o700)).map_err(|_| Error::Storage)?;
        let state = directory(&self.plan.state_dir)?;
        let quarantined = self.plan.state_dir.join("legacy");
        if !quarantined.try_exists()? {
            snapshot::quarantine(&parent, self.plan.legacy_home.file_name().and_then(|n| n.to_str()).ok_or(Error::Invalid)?, &state)?;
        }
        let metadata = fs::symlink_metadata(&quarantined)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err(Error::Invalid); }
        // Its root-only enclosing directory, not recursive chown, isolates all old inodes.
        let _ = random_stage(&self.plan.state_dir.join("snapshot.key"), 32)?;
        if !self.snapshot_verified()? {
            let stage = tempfile::NamedTempFile::new_in(&self.plan.state_dir)?;
            stage.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
            let mut writer = ArchiveWriter::new(stage, &self.archive_key()?, self.plan.migration, self.plan.snapshot_encoded_limit)?;
            snapshot::walk(&directory(&quarantined)?, snapshot::Limits { max_entries: 100_000,
                max_depth: 32, max_bytes: self.plan.snapshot_content_limit }, &mut writer)?;
            let stage = writer.finish()?;
            stage.as_file().sync_all()?;
            decrypt(stage.reopen()?, std::io::sink(), &self.archive_key()?, self.plan.migration, self.plan.snapshot_encoded_limit)?;
            let archive_path = self.plan.state_dir.join("snapshot.aead");
            stage.persist_noclobber(&archive_path).map_err(|_| Error::Conflict)?;
            sync_parent(&archive_path)?;
        }
        match fs::DirBuilder::new().mode(0o700).create(&self.plan.legacy_home) {
            Ok(()) => sync_parent(&self.plan.legacy_home)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        path_metadata(&self.plan.legacy_home, true, 0)?;
        if fs::read_dir(&self.plan.legacy_home)?.next().is_some() { return Err(Error::Conflict); }
        Ok(())
    }
    fn key_material(&self) -> Result<Vec<(&'static str, Vec<u8>)>, Error> {
        // A completed readback is irreversible: never generate another set if
        // a crash happened while deleting its temporary secret copies.
        if self.plan.state_dir.join(KEY_RECEIPT).try_exists()?
            || self.plan.state_dir.join(RETIRED_KEYS).try_exists()? { return Err(Error::Conflict); }
        let dir = self.plan.state_dir.join("fresh-keys");
        private_dir(&dir)?;
        let root = random_stage(&dir.join("root.key"), 32)?;
        let identity = random_stage(&dir.join("identity.key"), 32)?;
        let jwt = random_stage(&dir.join("jwt_secret"), 64)?;
        let mcp = random_stage(&dir.join("mcp.key"), 16)?;
        let config = dir.join("config.toml");
        if !config.try_exists()? {
            let key: String = mcp.iter().map(|b| format!("{b:02x}")).collect();
            // Fresh defaults only. No legacy section is parsed or copied here.
            let value = format!("[gateway]\nbind = '127.0.0.1'\nport = 18889\n[auth]\nmfa_enforced = true\n[dispatch]\nenabled = false\n[account_loading]\ninherit_host_credentials = false\n[mcp_keys.ddc_prod_{key}]\nclient_id = 'gateway-internal'\nis_external = false\nscopes = ['admin']\ncreated_at = '{}'\n", chrono::Utc::now().to_rfc3339());
            publish(&config, value.as_bytes(), 0, 0, false)?;
        }
        Ok(vec![("root.key", root), ("identity.key", identity), ("jwt_secret", jwt), ("config.toml", read_private(&config, 4096, 0)?)])
    }
    fn rotate(&mut self) -> Result<(), Error> {
        if !self.stopped()? { return Err(Error::Readback); }
        let parent = self.plan.legacy_home.parent().ok_or(Error::Invalid)?;
        trusted_parents(&self.plan.root_key_path)?;
        // The new home may already have its final owner after an interrupted publish.
        let m = fs::symlink_metadata(&self.plan.legacy_home)?;
        if !m.is_dir() || m.file_type().is_symlink() || (m.uid() != 0 && m.uid() != self.plan.gateway_uid)
            || m.mode() & 0o7777 != 0o700 { return Err(Error::Invalid); }
        for entry in fs::read_dir(&self.plan.legacy_home)? {
            let entry = entry?;
            if !["identity.key", "jwt_secret", "config.toml"].iter().any(|n| entry.file_name() == *n) {
                return Err(Error::Conflict);
            }
        }
        for (name, bytes) in self.key_material()? {
            let is_root = name == "root.key";
            let path = if is_root { self.plan.root_key_path.clone() } else { self.plan.legacy_home.join(name) };
            let (uid, gid) = if is_root { (0, 0) } else { (self.plan.gateway_uid, self.plan.gateway_gid) };
            if is_root && path.try_exists()? && read_private(&path, 32, 0)? == bytes { continue; }
            publish(&path, &bytes, uid, gid, is_root)?;
        }
        let home = directory(&self.plan.legacy_home)?;
        rfs::fchown(&home, Some(rustix::process::Uid::from_raw(self.plan.gateway_uid)),
            Some(rustix::process::Gid::from_raw(self.plan.gateway_gid))).map_err(|_| Error::Storage)?;
        rfs::fsync(&home).map_err(|_| Error::Storage)?;
        // Gateway can traverse the root-controlled parent, but cannot replace its home.
        rfs::fchmod(directory(parent)?, Mode::from_bits_truncate(0o711)).map_err(|_| Error::Storage)?;
        sync_parent(&self.plan.legacy_home)
    }
    fn keys_verified(&self) -> Result<bool, Error> {
        let stage = self.plan.state_dir.join("fresh-keys");
        let receipt_path = self.plan.state_dir.join(KEY_RECEIPT);
        let receipt = if receipt_path.try_exists()? {
            let receipt: KeyRotationReceipt = serde_json::from_slice(&read_private(&receipt_path, 4096, 0)?)?;
            receipt.validate(self.plan.migration)?;
            receipt
        } else {
            if !stage.try_exists()? { return Ok(false); }
            path_metadata(&stage, true, 0)?;
            let mut files = Vec::new();
            for (name, cap) in ROTATED_FILES {
                let expected_path = stage.join(name);
                if !expected_path.try_exists()? { return Ok(false); }
                let bytes = read_private(&expected_path, cap, 0)?;
                files.push(KeyDigest { name: name.into(), bytes: bytes.len() as u64, sha256: Sha256::digest(&bytes).into() });
            }
            let receipt = KeyRotationReceipt { migration: self.plan.migration, files };
            receipt.validate(self.plan.migration)?;
            receipt
        };
        for (expected, (name, cap)) in receipt.files.iter().zip(ROTATED_FILES) {
            let actual = if name == "root.key" { self.plan.root_key_path.clone() } else { self.plan.legacy_home.join(name) };
            if !actual.try_exists()? { return Ok(false); }
            let uid = if name == "root.key" { 0 } else { self.plan.gateway_uid };
            let bytes = read_private(&actual, cap, uid)?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if bytes.len() as u64 != expected.bytes || digest != expected.sha256 { return Ok(false); }
        }
        // No old users/session/token/sealed-store files can be present at this stage.
        for entry in fs::read_dir(&self.plan.legacy_home)? {
            if !["identity.key", "jwt_secret", "config.toml"].iter().any(|n| entry.as_ref().is_ok_and(|e| e.file_name() == *n)) {
                return Err(Error::Conflict);
            }
        }
        let home = fs::symlink_metadata(&self.plan.legacy_home)?;
        if home.uid() == 0 { return Ok(false); }
        if path_metadata(&self.plan.legacy_home, true, self.plan.gateway_uid)?.mode() & 0o7777 != 0o700 { return Ok(false); }
        // Persist only digests before removing staging. If the journal commit
        // crashes afterwards, the next readback compares these to live keys.
        publish(&receipt_path, &serde_json::to_vec(&receipt)?, 0, 0, false)?;
        self.remove_verified_key_staging()?;
        Ok(true)
    }
    fn remove_verified_key_staging(&self) -> Result<(), Error> {
        let stage = self.plan.state_dir.join("fresh-keys");
        let retired = self.plan.state_dir.join(RETIRED_KEYS);
        if !stage.try_exists()? && !retired.try_exists()? { return Ok(()); }
        let receipt: KeyRotationReceipt = serde_json::from_slice(
            &read_private(&self.plan.state_dir.join(KEY_RECEIPT), 4096, 0)?)?;
        receipt.validate(self.plan.migration)?;
        if stage.try_exists()? {
            path_metadata(&stage, true, 0)?;
            if retired.try_exists()? { return Err(Error::Conflict); }
            // Remove the public staging name atomically, then resume bounded
            // no-follow erasure under a fixed private name after any crash.
            fs::rename(&stage, &retired)?;
            sync_parent(&retired)?;
        }
        if retired.try_exists()? {
            path_metadata(&retired, true, 0)?;
            let parent = directory(&self.plan.state_dir)?;
            remove_tree(&parent, std::ffi::OsStr::new(RETIRED_KEYS), 0, &mut 0)?;
            rfs::fsync(&parent).map_err(|_| Error::Storage)?;
        }
        Ok(())
    }
}
impl<I: RootMigrationIntegration> HostAdapter for RootHost<I> {
    fn readback(&mut self, op: StageOperation) -> Result<Option<Evidence>, Error> {
        if op.migration != self.plan.migration { return Err(Error::Conflict); }
        match op.target {
            MigrationState::LegacyQuarantined => {
                if !self.plan.state_dir.join("legacy").try_exists()? || !self.plan.legacy_home.try_exists()?
                    || !self.snapshot_verified()? { return Ok(None); }
                path_metadata(&self.plan.legacy_home, true, 0)?;
                if fs::read_dir(&self.plan.legacy_home)?.next().is_some() { return Err(Error::Conflict); }
                if !self.stopped()? || !self.integration.gateway_denied_legacy(&self.plan)? { return Err(Error::Readback); }
                Ok(Some(Evidence::Quarantined { snapshot_verified: true, fresh_home_empty: true, gateway_denied_legacy: true }))
            }
            MigrationState::KeysRotated => Ok(self.keys_verified()?.then_some(Evidence::Rotated {
                root_key: true, identity_key: true, mcp_key: true, jwt_key: true, old_auth_absent: true })),
            _ => self.integration.readback(op, &self.plan),
        }
    }
    fn perform(&mut self, op: StageOperation) -> Result<(), Error> {
        if op.migration != self.plan.migration { return Err(Error::Conflict); }
        match op.target {
            MigrationState::LegacyQuarantined => self.quarantine_and_snapshot(),
            MigrationState::KeysRotated => self.rotate(),
            _ => self.integration.perform(op, &self.plan),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(base: &Path) -> RootPlan { RootPlan { migration: RequestId([7; 16]), state_dir: base.join("migration"),
        legacy_home: base.join("gateway/data"), root_key_path: base.join("protected/root.key"), gateway_uid: 12345,
        gateway_gid: 12345, snapshot_content_limit: 1024 * 1024, snapshot_encoded_limit: 16 * 1024 * 1024,
        expires_at_unix: 2_000_000_000 } }
    #[test]
    fn root_plan_rejects_overlap_relative_and_zero_owner() {
        let mut p = plan(Path::new("/var/lib/test"));
        assert!(p.validate().is_ok());
        p.state_dir = p.legacy_home.join("child"); assert_eq!(p.validate(), Err(Error::Invalid));
        p = plan(Path::new("relative")); assert_eq!(p.validate(), Err(Error::Invalid));
        p = plan(Path::new("/var/lib/test")); p.gateway_uid = 0; assert_eq!(p.validate(), Err(Error::Invalid));
    }
    #[test]
    fn stage_runner_covers_contract_order() {
        let mut current = MigrationState::Planned;
        let mut count = 0;
        while let Some(next) = next_stage(current) { current.transition(next).unwrap(); current = next; count += 1; }
        assert_eq!(current, MigrationState::Complete); assert_eq!(count, 11);
    }
    struct AdministratorPending { outstanding:u32 }
    impl RootMigrationIntegration for AdministratorPending {
        fn preflight(&mut self,_:&RootPlan)->Result<(),Error>{Ok(())}
        fn materialization_ready(&mut self,_:&RootPlan)->Result<bool,Error>{Ok(self.outstanding == 0)}
        fn readback(&mut self,op:StageOperation,_:&RootPlan)->Result<Option<Evidence>,Error>{
            if op.target == MigrationState::ContentImported { return Ok(Some(Evidence::Materialized { all_records_terminal:self.outstanding == 0 })); }
            assert_eq!(op.target,MigrationState::Complete);
            Ok(Some(Evidence::Complete{outstanding_proposals:self.outstanding,rotation_obligations:0,
                failed_applies:0,unclaimed_personal_owners:0,isolation_readback:true}))
        }
        fn perform(&mut self,_:StageOperation,_:&RootPlan)->Result<(),Error>{panic!("completion must use live readback")}
        fn gateway_denied_legacy(&mut self,_:&RootPlan)->Result<bool,Error>{Ok(true)}
        fn retention_ready(&mut self,_:&RootPlan)->Result<bool,Error>{Ok(self.outstanding == 0)}
    }
    #[test]
    fn migration_run_returns_pending_and_only_finishes_after_live_confirmation_readback(){
        let tmp=tempfile::tempdir().unwrap();let p=plan(tmp.path());
        let db=tmp.path().join("journal.db");
        let journal=Journal::from_connection(rusqlite::Connection::open(&db).unwrap(),p.migration).unwrap();
        rusqlite::Connection::open(&db).unwrap().execute("UPDATE migration SET state=?1 WHERE singleton=1",
            [serde_json::to_string(&MigrationState::AwaitingAdministrator).unwrap()]).unwrap();
        let mut runner=RootMigration{plan:p.clone(),_lock:File::create(tmp.path().join("test-lock")).unwrap(),journal,
            host:RootHost{plan:p,integration:AdministratorPending{outstanding:1}}};
        assert_eq!(runner.run().unwrap(),MigrationState::AwaitingAdministrator);
        assert!(!runner.journal.has_pending_intent().unwrap());
        runner.host.integration.outstanding=0;
        assert_eq!(runner.run().unwrap(),MigrationState::Complete);
        assert_eq!(runner.run().unwrap(),MigrationState::Complete);
    }
    struct ControlledHost;
    impl RootMigrationIntegration for ControlledHost {
        fn preflight(&mut self, _: &RootPlan) -> Result<(), Error> { Ok(()) }
        fn readback(&mut self, op: StageOperation, _: &RootPlan) -> Result<Option<Evidence>, Error> {
            if op.target == MigrationState::OldEnvironmentStopped { Ok(Some(Evidence::Stopped { activation_sources_blocked: true, uid_empty: true })) }
            else { Err(Error::Unavailable) }
        }
        fn perform(&mut self, _: StageOperation, _: &RootPlan) -> Result<(), Error> { Err(Error::Unavailable) }
        fn gateway_denied_legacy(&mut self, _: &RootPlan) -> Result<bool, Error> { Ok(true) }
        fn retention_ready(&mut self, _: &RootPlan) -> Result<bool, Error> { Ok(true) }
    }
    #[test]
    #[ignore = "root-only filesystem operations; disposable Linux VM, no system service mutation"]
    fn iso_root_resume_quarantine_snapshot_rotation_and_lock() {
        root().unwrap();
        let tmp = tempfile::tempdir_in("/root").unwrap();
        let p = plan(tmp.path());
        fs::create_dir(tmp.path().join("gateway")).unwrap();
        fs::create_dir(tmp.path().join("protected")).unwrap();
        fs::create_dir(&p.legacy_home).unwrap();
        fs::write(p.legacy_home.join("users.db"), b"old planted authority").unwrap();
        let mut flow = RootMigration::open(p.clone(), ControlledHost).unwrap();
        assert!(matches!(RootMigration::open(p.clone(), ControlledHost), Err(Error::Conflict)));
        assert_eq!(flow.advance().unwrap(), MigrationState::OldEnvironmentStopped);
        assert_eq!(flow.advance().unwrap(), MigrationState::LegacyQuarantined);
        assert!(p.state_dir.join("legacy/users.db").is_file());
        assert!(!p.legacy_home.join("users.db").exists());
        drop(flow);
        let mut resumed = RootMigration::open(p.clone(), ControlledHost).unwrap();
        assert_eq!(resumed.advance().unwrap(), MigrationState::KeysRotated);
        let key = fs::read(&p.root_key_path).unwrap();
        assert_eq!(key.len(), 32);
        assert_eq!(fs::metadata(p.legacy_home.join("identity.key")).unwrap().uid(), p.gateway_uid);
        assert!(!p.state_dir.join("fresh-keys").exists());
        assert!(!p.state_dir.join(RETIRED_KEYS).exists());
        assert!(p.state_dir.join(KEY_RECEIPT).is_file());
        // A crash between staging deletion and the journal commit needs no new
        // key generation. Readback uses the durable digest and the live files.
        assert!(resumed.host.keys_verified().unwrap());
        assert_eq!(resumed.host.key_material().unwrap_err(), Error::Conflict);
        private_dir(&p.state_dir.join(RETIRED_KEYS)).unwrap();
        publish(&p.state_dir.join(RETIRED_KEYS).join("interrupted-secret"), b"secret", 0, 0, false).unwrap();
        assert!(resumed.host.keys_verified().unwrap());
        assert!(!p.state_dir.join(RETIRED_KEYS).exists());
        assert_eq!(resumed.advance(), Err(Error::Unavailable));
        assert_eq!(resumed.state().unwrap(), MigrationState::KeysRotated);
        let output = resumed.export_records(&tmp.path().join("recovered")).unwrap();
        assert!(fs::read_to_string(output).unwrap().lines().all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
        assert!(!resumed.expire(p.expires_at_unix - 1).unwrap());
        assert!(!p.state_dir.join("legacy").exists());
        assert!(p.state_dir.join("snapshot.aead").exists());
        assert!(p.state_dir.join("snapshot.key").exists());
        assert!(resumed.expire(p.expires_at_unix).unwrap());
        assert!(resumed.expire(p.expires_at_unix).unwrap());
        assert!(!p.state_dir.join("legacy").exists());
        assert!(!p.state_dir.join("snapshot.aead").exists());
        drop(resumed);
        assert_eq!(fs::read(p.root_key_path).unwrap(), key);
    }
    #[test]
    fn missing_terminal_content_receipt_blocks_cleanup_even_after_deadline() {
        let tmp = tempfile::tempdir().unwrap(); let p = plan(tmp.path());
        let db = tmp.path().join("journal.db");
        let journal = Journal::from_connection(rusqlite::Connection::open(&db).unwrap(), p.migration).unwrap();
        rusqlite::Connection::open(&db).unwrap().execute("UPDATE migration SET state=?1 WHERE singleton=1",
            [serde_json::to_string(&MigrationState::AwaitingAdministrator).unwrap()]).unwrap();
        fs::create_dir(&p.state_dir).unwrap();
        fs::create_dir(p.state_dir.join("legacy")).unwrap();
        fs::write(p.state_dir.join("legacy/content"), b"only plaintext copy").unwrap();
        fs::write(p.state_dir.join("snapshot.aead"), b"encrypted copy").unwrap();
        fs::write(p.state_dir.join("snapshot.key"), b"snapshot key").unwrap();
        let mut flow = RootMigration { plan: p.clone(), _lock: File::create(tmp.path().join("test-lock")).unwrap(), journal,
            host: RootHost { plan: p.clone(), integration: AdministratorPending { outstanding: 1 } } };
        assert!(!flow.expire(p.expires_at_unix - 1).unwrap());
        assert_eq!(flow.expire(p.expires_at_unix), Err(Error::Readback));
        assert_eq!(fs::read(p.state_dir.join("legacy/content")).unwrap(), b"only plaintext copy");
        assert!(p.state_dir.join("snapshot.aead").exists());
        assert!(p.state_dir.join("snapshot.key").exists());
    }
    #[test]
    fn key_receipt_binds_migration_and_exact_complete_key_set() {
        let id = RequestId([1; 16]);
        let mut receipt = KeyRotationReceipt { migration: id, files: ROTATED_FILES.iter()
            .map(|(name, cap)| KeyDigest { name: (*name).into(), bytes: *cap, sha256: [7; 32] }).collect() };
        assert!(receipt.validate(id).is_ok());
        assert_eq!(receipt.validate(RequestId([2; 16])), Err(Error::Conflict));
        receipt.files[0].bytes = 1;
        assert_eq!(receipt.validate(id), Err(Error::Invalid));
        receipt.files[0].bytes = 32;
        receipt.files[0].name = "identity.key".into();
        assert_eq!(receipt.validate(id), Err(Error::Invalid));
        receipt.files.pop();
        assert_eq!(receipt.validate(id), Err(Error::Conflict));
    }
    #[test]
    #[ignore = "root-only ownership/path validation; disposable Linux VM"]
    fn iso_root_rejects_symlink_and_plan_change() {
        root().unwrap();
        let tmp = tempfile::tempdir_in("/root").unwrap();
        let p = plan(tmp.path());
        fs::create_dir(tmp.path().join("protected")).unwrap();
        fs::create_dir(tmp.path().join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), &p.state_dir).unwrap();
        assert!(RootMigration::open(p.clone(), MissingIntegration).is_err());
        fs::remove_file(&p.state_dir).unwrap();
        let mut flow = RootMigration::open(p.clone(), MissingIntegration).unwrap();
        assert_eq!(flow.run(), Err(Error::Unavailable));
        assert_eq!(flow.state().unwrap(), MigrationState::Planned);
        drop(flow);
        let mut changed = p; changed.gateway_uid += 1;
        assert!(matches!(RootMigration::open(changed, MissingIntegration), Err(Error::Conflict)));
    }
}

/// Read only a root-owned, bounded installer plan; legacy input is never a plan.
pub fn load_plan(path: &Path) -> Result<RootPlan, Error> {
    root()?; trusted_parents(path)?;
    let plan: RootPlan = serde_json::from_slice(&read_private(path, 16 * 1024, 0)?)?;
    plan.validate()?; Ok(plan)
}

// Relative no-follow deletion never crosses a mount or recreates old metadata.
fn remove_tree(parent: &OwnedFd, name: &std::ffi::OsStr, depth: u32, count: &mut u32) -> Result<(), Error> {
    use rustix::fs::{AtFlags, ResolveFlags, FileType};
    if depth > 32 || *count >= 100_001 { return Err(Error::Limit); }
    *count += 1;
    let handle = rfs::openat2(parent, name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(), ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS | ResolveFlags::NO_XDEV)
        .map_err(|_| Error::Storage)?;
    let metadata = rfs::fstat(&handle).map_err(|_| Error::Storage)?;
    if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
        let child = rfs::openat2(parent, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(), ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS | ResolveFlags::NO_XDEV)
            .map_err(|_| Error::Storage)?;
        let mut dir = rfs::Dir::read_from(&child).map_err(|_| Error::Storage)?;
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|_| Error::Storage)?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." { continue; }
            use std::os::unix::ffi::OsStrExt;
            remove_tree(&child, std::ffi::OsStr::from_bytes(bytes), depth + 1, count)?;
        }
        rfs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|_| Error::Storage)?;
    } else { rfs::unlinkat(parent, name, AtFlags::empty()).map_err(|_| Error::Storage)?; }
    Ok(())
}
