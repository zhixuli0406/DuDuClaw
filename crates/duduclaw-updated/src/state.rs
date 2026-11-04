//! Durable transaction policy. Every mutation is made on a copy and published only
//! after its complete snapshot is fsynced. A crash never advances an in-memory floor.
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::*};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

pub const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_GENERATION_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const REQUEST_HISTORY: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub version: ReleaseVersion,
    pub serial: u64,
    pub digest: Sha256Digest,
    pub phase: UpdateJournalState,
    pub updater_slot: Option<UpdateSlot>,
    pub accepted_at_unix: u64,
    #[serde(default)]
    pub emergency: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRecord {
    pub id: RequestId,
    pub version: ReleaseVersion,
    pub status: UpdateStatus,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: u16,
    pub product: Product,
    pub arch: Architecture,
    pub floor: ReleaseVersion,
    pub serial: u64,
    pub committed_digest: Sha256Digest,
    pub highest_serial: u64,
    pub highest_digest: Sha256Digest,
    pub pending: Option<Pending>,
    pub selector: SlotSelector,
    pub maintenance: bool,
    pub requests: Vec<RequestRecord>,
    #[serde(default)]
    pub resume_required: bool,
    #[serde(default)]
    pub running_generation: Option<Sha256Digest>,
    #[serde(default)]
    pub running_version: Option<ReleaseVersion>,
}
impl State {
    pub fn validate(&self) -> Result<()> {
        self.selector.validate()?;
        if self.running_generation.is_some() != self.running_version.is_some()
            || self.running_version.is_some_and(|version| version >= self.floor) { return Err(Error::Invalid); }
        if self.schema_version != 1 || self.highest_serial < self.serial || self.requests.len() > REQUEST_HISTORY { return Err(Error::Invalid); }
        if self.highest_serial == self.serial && self.highest_digest != self.committed_digest { return Err(Error::Invalid); }
        if let Some(pending) = &self.pending {
            if pending.emergency {
                if pending.serial == 0 || pending.version >= self.floor || pending.digest == self.effective_digest() {
                    return Err(Error::Invalid);
                }
            } else if pending.serial <= self.serial || pending.version <= self.floor
                || pending.serial != self.highest_serial || pending.digest != self.highest_digest { return Err(Error::Invalid); }
        }
        for (index, record) in self.requests.iter().enumerate() {
            if self.requests.iter().take(index).any(|r| r.id == record.id) { return Err(Error::Invalid); }
        }
        Ok(())
    }
    /// Only signed metadata enters this check; a pending retry must have identical bytes.
    pub fn check_manifest(&self, manifest: &ReleaseManifest, digest: Sha256Digest, now: u64) -> Result<()> {
        self.validate()?;
        manifest.validate()?;
        if self.maintenance { return Err(Error::Maintenance); }
        if now >= manifest.expires_at_unix || now < manifest.created_at_unix { return Err(Error::Expired); }
        if manifest.product != self.product || manifest.arch != self.arch
            || manifest.minimum_version > self.running_version.unwrap_or(self.floor) { return Err(Error::Incompatible); }
        if manifest.version <= self.floor || manifest.serial <= self.serial { return Err(Error::Rollback); }
        if manifest.serial < self.highest_serial
            || (manifest.serial == self.highest_serial && (self.pending.is_none() || digest != self.highest_digest)) {
            return Err(Error::Rollback);
        }
        if let Some(pending) = &self.pending {
            if pending.serial != manifest.serial || pending.digest != digest || pending.version != manifest.version {
                return Err(Error::Rollback);
            }
        }
        self.check_generation(manifest)
    }
    fn check_generation(&self, manifest: &ReleaseManifest) -> Result<()> {
        manifest.validate()?;
        if manifest.product != self.product || manifest.arch != self.arch { return Err(Error::Incompatible); }
        let p = &manifest.protocols;
        if [p.gateway, p.spawnd, p.trampoline, p.egress, p.updated, p.mcp_bridge, p.parse_worker, p.importer]
            .iter().any(|v| *v != 1) { return Err(Error::Incompatible); }
        // A complete generation prevents partial upgrades from mixing privileged components.
        for required in [InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline,
            InstallTarget::Egress, InstallTarget::EgressForwarder, InstallTarget::McpBridge,
            InstallTarget::ParseWorker, InstallTarget::Importer, InstallTarget::UpdateSlot] {
            if !manifest.artifacts.iter().any(|a| a.install_target == required && a.mode & 0o100 != 0) {
                return Err(Error::Incompatible);
            }
        }
        let total = manifest.artifacts.iter().try_fold(0u64, |total, a| {
            if a.size_bytes > MAX_ARTIFACT_BYTES { return None; }
            total.checked_add(a.size_bytes)
        }).ok_or(Error::Invalid)?;
        if total > MAX_GENERATION_BYTES { return Err(Error::Invalid); }
        Ok(())
    }
    pub fn effective_digest(&self) -> Sha256Digest { self.running_generation.unwrap_or(self.committed_digest) }
    /// For accepted retries, expiry was enforced at durable acceptance. Emergency
    /// authorization is separately rechecked by linux::emergency before every stage.
    pub fn check_pending_manifest(&self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
        self.validate()?;
        let pending = self.pending.as_ref().ok_or(Error::Invalid)?;
        if self.maintenance { return Err(Error::Maintenance); }
        if pending.digest != digest || pending.version != manifest.version || pending.serial != manifest.serial
            || pending.accepted_at_unix < manifest.created_at_unix || pending.accepted_at_unix >= manifest.expires_at_unix {
            return Err(Error::Signature);
        }
        if !pending.emergency { return self.check_manifest(manifest, digest, pending.accepted_at_unix); }
        self.check_generation(manifest)
    }
    /// Root-only entry: caller has already persisted and verified the exact emergency
    /// intent. Never reachable through Apply. Ordinary floor/high-watermarks stay intact.
    pub fn prepare_emergency(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest,
        accepted_at: u64, request_id: RequestId) -> Result<()> {
        self.validate()?;
        self.check_generation(manifest)?;
        if self.maintenance { return Err(Error::Maintenance); }
        if let Some(pending) = &self.pending {
            if pending.emergency && pending.digest == digest {
                self.check_pending_manifest(manifest, digest)?;
                return Ok(());
            }
            return Err(Error::Busy);
        }
        if self.resume_required || self.requests.iter().any(|r| !terminal(r.status)) { return Err(Error::Busy); }
        if manifest.version >= self.floor || accepted_at < manifest.created_at_unix || accepted_at >= manifest.expires_at_unix {
            return Err(Error::Rollback);
        }
        if self.requests.iter().any(|r| r.id == request_id) { return Err(Error::Rollback); }
        if self.requests.len() == REQUEST_HISTORY { self.requests.remove(0); }
        self.requests.push(RequestRecord { id: request_id, version: manifest.version, status: UpdateStatus::Prepared });
        self.pending = Some(Pending { version: manifest.version, serial: manifest.serial, digest,
            phase: UpdateJournalState::Prepared, updater_slot: None, accepted_at_unix: accepted_at, emergency: true });
        self.validate()
    }
    pub fn admit(&mut self, id: RequestId, version: ReleaseVersion) -> Result<UpdateStatus> {
        self.validate()?;
        if self.maintenance { return Err(Error::Maintenance); }
        if let Some(record) = self.requests.iter().find(|r| r.id == id) {
            return if record.version == version { self.status(id).ok_or(Error::Invalid) } else { Err(Error::Invalid) };
        }
        if version <= self.floor { return Err(Error::Rollback); }
        if self.pending.is_some() || self.resume_required { return Err(Error::Busy); }
        if self.requests.iter().any(|r| !terminal(r.status)) { return Err(Error::Busy); }
        if self.requests.len() == REQUEST_HISTORY { self.requests.remove(0); }
        self.requests.push(RequestRecord { id, version, status: UpdateStatus::Queued });
        Ok(UpdateStatus::Queued)
    }
    pub fn status(&self, id: RequestId) -> Option<UpdateStatus> {
        self.requests.iter().find(|r| r.id == id).map(|r| {
            if self.resume_required && r.status == UpdateStatus::Committed
                && self.requests.last().is_some_and(|last| last.id == id) { UpdateStatus::Activated } else { r.status }
        })
    }
    pub fn finish_resume(&mut self) -> Result<()> {
        if !self.resume_required || self.pending.is_some() { return Err(Error::Invalid); }
        let request = self.requests.last_mut().ok_or(Error::Invalid)?;
        if !matches!(request.status, UpdateStatus::Committed | UpdateStatus::Activated | UpdateStatus::Maintenance) {
            return Err(Error::Invalid);
        }
        request.status = UpdateStatus::Committed;
        self.resume_required = false;
        Ok(())
    }
    pub fn set_status(&mut self, id: RequestId, status: UpdateStatus) -> Result<()> {
        let record = self.requests.iter_mut().find(|r| r.id == id).ok_or(Error::Invalid)?;
        let allowed = matches!((record.status, status),
            (UpdateStatus::Queued, UpdateStatus::Downloading)
            | (UpdateStatus::Downloading, UpdateStatus::Prepared)
            | (UpdateStatus::Prepared, UpdateStatus::Activated)
            | (UpdateStatus::Activated, UpdateStatus::Committed))
            || (!terminal(record.status) && matches!(status, UpdateStatus::Failed | UpdateStatus::Maintenance))
            || (self.resume_required && record.status == UpdateStatus::Committed && status == UpdateStatus::Maintenance);
        if !allowed { return Err(Error::Invalid); }
        record.status = status;
        Ok(())
    }
    pub fn prepare(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest, now: u64) -> Result<()> {
        self.check_manifest(manifest, digest, now)?;
        if self.pending.is_none() {
            self.highest_serial = manifest.serial;
            self.highest_digest = digest;
            self.pending = Some(Pending { version: manifest.version, serial: manifest.serial,
                digest, phase: UpdateJournalState::Prepared, updater_slot: None, accepted_at_unix: now, emergency: false });
        }
        Ok(())
    }
    /// Called only after the immutable generation exists, all artifacts were rehashed,
    /// and the trial slot's files and containing directories have been fsynced.
    pub fn stage_trial(&mut self, slot: UpdateSlot) -> Result<()> {
        if self.pending.as_ref().map(|p| p.phase) != Some(UpdateJournalState::Prepared)
            || self.selector.trial_slot.is_some() || slot == self.selector.active { return Err(Error::Invalid); }
        self.pending.as_mut().ok_or(Error::Invalid)?.updater_slot = Some(slot);
        self.selector.previous = self.selector.active;
        self.selector.trial_slot = Some(slot);
        self.selector.attempts = 0;
        Ok(())
    }
    /// Persist this mutation BEFORE exec. An unsuccessful first root run consumes
    /// the sole attempt; the next boot returns to the verified known-good slot.
    pub fn boot(&mut self, a_valid: bool, b_valid: bool) -> Result<UpdateSlot> {
        self.validate()?;
        if self.maintenance { return Err(Error::Maintenance); }
        let valid = |slot| match slot { UpdateSlot::A => a_valid, UpdateSlot::B => b_valid };
        if let Some(trial) = self.selector.trial_slot {
            if self.selector.attempts == 0 && valid(trial) {
                self.selector.attempts = 1;
                return Ok(trial);
            }
            let previous = self.selector.previous;
            self.selector.trial_slot = None;
            self.selector.attempts = 0;
            self.selector.active = previous;
            if valid(previous) {
                // No generation activation/irreversible migration occurred before
                // a trial boot. Retire the failed pending attempt for a fixed release.
                if self.pending.as_ref().is_some_and(|p| p.phase == UpdateJournalState::Prepared) { self.pending = None; }
                return Ok(previous);
            }
        } else if valid(self.selector.active) { return Ok(self.selector.active); }
        // A stale other slot is not automatically authorized as a fallback.
        self.maintenance = true;
        Err(Error::Maintenance)
    }
    pub fn mark_healthy(&mut self, executing: UpdateSlot) -> Result<()> {
        if self.selector.trial_slot != Some(executing) || self.selector.attempts != 1
            || self.pending.as_ref().and_then(|p| p.updater_slot) != Some(executing) { return Err(Error::Invalid); }
        self.selector.active = executing;
        self.selector.trial_slot = None;
        self.selector.attempts = 0;
        Ok(())
    }
    pub fn activated(&mut self, digest: Sha256Digest) -> Result<()> {
        if self.selector.trial_slot.is_some() || self.maintenance { return Err(Error::Maintenance); }
        let pending = self.pending.as_mut().ok_or(Error::Invalid)?;
        if pending.digest != digest || pending.updater_slot != Some(self.selector.active) { return Err(Error::Invalid); }
        pending.phase = pending.phase.transition(UpdateJournalState::Activated)?;
        Ok(())
    }
    /// Activation's real readback and generation directory fsync must precede this.
    /// Floor, serial and journal commit are persisted in ONE atomic snapshot.
    pub fn commit(&mut self, digest: Sha256Digest) -> Result<()> {
        let pending = self.pending.as_ref().ok_or(Error::Invalid)?;
        if pending.digest != digest || pending.phase != UpdateJournalState::Activated
            || pending.updater_slot != Some(self.selector.active)
            || self.selector.trial_slot.is_some() || self.maintenance { return Err(Error::Invalid); }
        if pending.emergency {
            self.running_generation = Some(pending.digest);
            self.running_version = Some(pending.version);
        } else {
            self.floor = pending.version;
            self.serial = pending.serial;
            self.committed_digest = pending.digest;
            self.running_generation = None;
            self.running_version = None;
        }
        self.pending = None;
        self.resume_required = true;
        Ok(())
    }
    pub fn interrupted_requests(&mut self) {
        for request in &mut self.requests {
            // Queued/download transactions are safe to retry. A failed first
            // root boot retires its pending generation in boot(); only that
            // now-orphaned prepared/activated request becomes terminal.
            if matches!(request.status, UpdateStatus::Prepared | UpdateStatus::Activated)
                && !self.pending.as_ref().is_some_and(|p| p.version == request.version) {
                request.status = UpdateStatus::Failed;
            }
        }
    }
    pub fn recovery_request(&self) -> Result<Option<(RequestId, ReleaseVersion)>> {
        if let Some(pending) = &self.pending {
            let record = self.requests.iter().rev().find(|r| r.version == pending.version)
                .ok_or(Error::Maintenance)?;
            return Ok(Some((record.id, record.version)));
        }
        Ok(self.requests.iter().find(|r| !terminal(r.status)).map(|r| (r.id, r.version)))
    }
    pub fn resume_request(&mut self, id: RequestId) -> Result<()> {
        let pending = self.pending.as_ref().ok_or(Error::Invalid)?;
        let record = self.requests.iter_mut().find(|r| r.id == id && r.version == pending.version)
            .ok_or(Error::Invalid)?;
        record.status = match pending.phase {
            UpdateJournalState::Prepared => UpdateStatus::Prepared,
            UpdateJournalState::Activated => UpdateStatus::Activated,
            UpdateJournalState::Committed => return Err(Error::Invalid),
        };
        Ok(())
    }
}
pub fn terminal(status: UpdateStatus) -> bool {
    matches!(status, UpdateStatus::Committed | UpdateStatus::Failed | UpdateStatus::Maintenance)
}

/// Implementations save a whole state via file fsync, rename, directory fsync.
/// A failed save stops the service; no later action may use the mutated snapshot.
pub trait DurableState { fn save(&mut self, next: &State) -> Result<()>; }
/// A failed directory fsync can follow a successful rename. Never overwrite that
/// possibly landed snapshot from stale memory; only a fresh process may recover.
pub struct FailClosedDisk<D> { inner: D, poisoned: bool }
impl<D> FailClosedDisk<D> {
    pub fn new(inner: D) -> Self { Self { inner, poisoned: false } }
    pub fn poisoned(&self) -> bool { self.poisoned }
}
impl<D: DurableState> DurableState for FailClosedDisk<D> {
    fn save(&mut self, next: &State) -> Result<()> {
        if self.poisoned { return Err(Error::Maintenance); }
        let result = self.inner.save(next);
        if result.is_err() { self.poisoned = true; }
        result
    }
}
pub fn transaction<T>(state: &mut State, disk: &mut impl DurableState,
    operation: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    let mut next = state.clone();
    let result = operation(&mut next)?;
    next.validate()?;
    disk.save(&next)?;
    *state = next;
    Ok(result)
}

/// Package A/B/G readiness seam. The concrete platform activator must drain runs,
/// atomically switch one current generation, restart the fixed units, and compare
/// non-root product/version/build/protocol readbacks against this signed manifest.
/// An unavailable activator must fail before changing current; never report success.
pub trait GenerationActivator {
    /// Validate the complete migration/runtime plan before staging a trial.
    fn preflight(&mut self, manifest: &ReleaseManifest, _: Sha256Digest) -> Result<()> {
        if !manifest.migrations.is_empty() || manifest.artifacts.iter().any(|a| matches!(a.install_target, InstallTarget::RuntimeEntry { .. })) {
            return Err(Error::Incompatible);
        }
        Ok(())
    }
    fn activate_and_read_back(&mut self, manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()>;
    fn finish_after_commit(&mut self, _: &ReleaseManifest, _: Sha256Digest) -> Result<()> { Err(Error::Maintenance) }
}
pub struct UnavailableActivator;
impl GenerationActivator for UnavailableActivator {
    fn activate_and_read_back(&mut self, _: &ReleaseManifest, _: Sha256Digest) -> Result<()> {
        Err(Error::Maintenance)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn version(minor: u32) -> ReleaseVersion { ReleaseVersion { major: 1, minor, patch: 0 } }
    fn state() -> State {
        State { schema_version: 1, product: Product::Duduclaw, arch: Architecture::X86_64,
            floor: version(79), serial: 10, committed_digest: Sha256Digest([1; 32]),
            highest_serial: 10, highest_digest: Sha256Digest([1; 32]), pending: None,
            selector: SlotSelector { active: UpdateSlot::A, previous: UpdateSlot::A, trial_slot: None, attempts: 0 },
            maintenance: false, requests: vec![], resume_required: false, running_generation: None, running_version: None }
    }
    fn manifest() -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw, arch: Architecture::X86_64,
            os: OperatingSystem::Linux, channel: ReleaseChannel::Stable, version: version(80),
            minimum_version: version(79), serial: 11, created_at_unix: 100, expires_at_unix: 200,
            artifacts: [InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline,
                InstallTarget::Egress, InstallTarget::EgressForwarder, InstallTarget::McpBridge,
                InstallTarget::ParseWorker, InstallTarget::Importer, InstallTarget::UpdateSlot]
                .into_iter().enumerate().map(|(index, target)| ReleaseArtifact {
                    id: format!("artifact-{index}"), sha256: Sha256Digest([index as u8; 32]), size_bytes: 1,
                    mode: 0o755, install_target: target }).collect(),
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
                updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] }
    }
    fn digest() -> Sha256Digest { Sha256Digest([2; 32]) }
    #[test]
    fn manifest_version_serial_expiry_arch_product_protocol_matrix() {
        let state = state();
        for now in [100, 150, 199] { assert!(state.check_manifest(&manifest(), digest(), now).is_ok()); }
        for now in [0, 99, 200, u64::MAX] { assert!(state.check_manifest(&manifest(), digest(), now).is_err()); }
        for serial in [0, 9, 10] {
            let mut m = manifest(); m.serial = serial;
            assert!(state.check_manifest(&m, digest(), 150).is_err());
        }
        for v in [version(78), version(79)] {
            let mut m = manifest(); m.version = v; m.minimum_version = v;
            assert!(state.check_manifest(&m, digest(), 150).is_err());
        }
        for change in 0..6 {
            let mut m = manifest();
            match change {
                0 => m.arch = Architecture::Aarch64,
                1 => m.product = Product::DuduclawPro,
                2 => m.minimum_version = version(80),
                3 => m.protocols.spawnd = 2,
                4 => { m.artifacts.remove(1); },
                5 => m.artifacts[0].size_bytes = MAX_ARTIFACT_BYTES + 1,
                _ => unreachable!(),
            }
            assert!(state.check_manifest(&m, digest(), 150).is_err(), "case {change}");
        }
    }
    #[test]
    fn same_serial_only_retries_identical_pending_digest() {
        let mut s = state();
        s.prepare(&manifest(), digest(), 150).unwrap();
        assert!(s.check_manifest(&manifest(), digest(), 150).is_ok());
        assert!(s.check_manifest(&manifest(), Sha256Digest([3; 32]), 150).is_err());
        let mut next = manifest(); next.serial += 1;
        assert!(s.check_manifest(&next, digest(), 150).is_err());
    }
    #[test]
    fn request_idempotency_busy_bounded_history_and_restart() {
        let mut s = state();
        let id = RequestId([1; 16]);
        assert_eq!(s.admit(id, version(80)).unwrap(), UpdateStatus::Queued);
        assert_eq!(s.admit(id, version(80)).unwrap(), UpdateStatus::Queued);
        assert!(s.admit(id, version(81)).is_err());
        assert!(s.admit(RequestId([2; 16]), version(81)).is_err());
        assert!(s.set_status(id, UpdateStatus::Committed).is_err());
        s.interrupted_requests();
        assert_eq!(s.status(id), Some(UpdateStatus::Queued));
        s.set_status(id, UpdateStatus::Failed).unwrap();
        for index in 2..70 {
            let id = RequestId([index; 16]);
            s.admit(id, version(80)).unwrap(); s.set_status(id, UpdateStatus::Failed).unwrap();
        }
        assert_eq!(s.requests.len(), REQUEST_HISTORY);
        assert_eq!(s.status(id), None);
    }
    #[test]
    fn ab_trial_attempt_is_spent_before_exec_and_failure_rolls_back() {
        let mut s = state(); s.prepare(&manifest(), digest(), 150).unwrap();
        s.stage_trial(UpdateSlot::B).unwrap();
        assert!(s.mark_healthy(UpdateSlot::B).is_err());
        assert_eq!(s.boot(true, true).unwrap(), UpdateSlot::B);
        assert_eq!(s.selector.attempts, 1);
        assert_eq!(s.floor, version(79));
        // Simulate process death after durable attempt but before healthy.
        let mut restarted: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(restarted.boot(true, true).unwrap(), UpdateSlot::A);
        assert_eq!(restarted.selector.trial_slot, None);
        assert_eq!(restarted.floor, version(79));
        assert!(restarted.pending.is_none());
        assert!(restarted.check_manifest(&manifest(), digest(), 150).is_err());
        assert!(restarted.check_manifest(&manifest(), Sha256Digest([3; 32]), 150).is_err());
        let mut fixed = manifest(); fixed.version = version(81); fixed.serial = 12;
        assert!(restarted.check_manifest(&fixed, Sha256Digest([3; 32]), 150).is_ok());
    }
    #[test]
    fn ab_invalid_slots_enter_maintenance_and_do_not_guess_other_slot() {
        for (a, b) in [(false, false), (false, true)] {
            let mut s = state();
            assert!(s.boot(a, b).is_err()); assert!(s.maintenance);
            assert!(s.boot(true, true).is_err());
        }
        let mut s = state(); s.prepare(&manifest(), digest(), 150).unwrap(); s.stage_trial(UpdateSlot::B).unwrap();
        assert_eq!(s.boot(true, false).unwrap(), UpdateSlot::A);
        assert!(s.pending.is_none());
    }
    #[test]
    fn floor_waits_for_health_readback_activation_and_atomic_commit() {
        let mut s = state(); s.prepare(&manifest(), digest(), 150).unwrap(); s.stage_trial(UpdateSlot::B).unwrap();
        assert!(s.commit(digest()).is_err());
        assert!(s.activated(digest()).is_err());
        s.boot(true, true).unwrap(); s.mark_healthy(UpdateSlot::B).unwrap();
        assert_eq!(s.floor, version(79)); assert_eq!(s.serial, 10);
        assert!(s.commit(digest()).is_err());
        s.activated(digest()).unwrap();
        assert!(s.commit(Sha256Digest([9; 32])).is_err());
        s.commit(digest()).unwrap();
        assert_eq!(s.floor, version(80)); assert_eq!(s.serial, 11); assert!(s.pending.is_none());
        assert!(s.check_manifest(&manifest(), digest(), 150).is_err());
    }
    struct Disk { persisted: State, fail: bool }
    impl DurableState for Disk {
        fn save(&mut self, next: &State) -> Result<()> {
            if self.fail { return Err(Error::Maintenance); }
            self.persisted = next.clone(); Ok(())
        }
    }
    #[test]
    fn crashes_at_each_durable_step_never_advance_floor_prematurely() {
        for crash_step in 0..6 {
            let mut state = state();
            let mut disk = Disk { persisted: state.clone(), fail: false };
            for step in 0..6 {
                disk.fail = step == crash_step;
                let result = transaction(&mut state, &mut disk, |s| match step {
                    0 => s.prepare(&manifest(), digest(), 150),
                    1 => s.stage_trial(UpdateSlot::B),
                    2 => s.boot(true, true).map(|_| ()),
                    3 => s.mark_healthy(UpdateSlot::B),
                    4 => s.activated(digest()),
                    _ => s.commit(digest()),
                });
                if step == crash_step {
                    assert!(result.is_err());
                    assert_eq!(state.floor, version(79));
                    assert_eq!(disk.persisted.floor, version(79));
                    assert_eq!(disk.persisted.serial, 10);
                    break;
                }
                result.unwrap();
            }
        }
    }
    #[test]
    fn post_rename_failure_permanently_poisoned_writer_preserves_landed_floor() {
        struct LandedThenFailed { persisted: State, calls: usize }
        impl DurableState for LandedThenFailed {
            fn save(&mut self, next: &State) -> Result<()> {
                self.persisted = next.clone(); self.calls += 1;
                Err(Error::Io(std::io::Error::from(std::io::ErrorKind::Other)))
            }
        }
        let mut before = state(); before.prepare(&manifest(), digest(), 150).unwrap();
        before.stage_trial(UpdateSlot::B).unwrap(); before.boot(true, true).unwrap();
        before.mark_healthy(UpdateSlot::B).unwrap(); before.activated(digest()).unwrap();
        let mut memory = before.clone();
        let mut disk = FailClosedDisk::new(LandedThenFailed { persisted: before, calls: 0 });
        assert!(transaction(&mut memory, &mut disk, |s| s.commit(digest())).is_err());
        assert_eq!(memory.floor, version(79));
        assert_eq!(disk.inner.persisted.floor, version(80));
        assert!(disk.poisoned());
        assert!(disk.save(&memory).is_err());
        assert_eq!(disk.inner.calls, 1);
        assert_eq!(disk.inner.persisted.serial, 11);
    }
    pub(crate) fn emergency_fixture() -> (State, ReleaseManifest) {
        let mut s = state(); s.floor = version(80); s.serial = 11; s.highest_serial = 11;
        let mut old = manifest(); old.version = version(79); old.serial = 10;
        (s, old)
    }
    #[test]
    fn emergency_commit_preserves_floor_and_hides_success_until_resume_readback() {
        let (mut s, old) = emergency_fixture();
        let id = RequestId([7; 16]);
        s.prepare_emergency(&old, digest(), 150, id).unwrap();
        s.check_pending_manifest(&old, digest()).unwrap();
        assert!(s.check_manifest(&old, digest(), 150).is_err());
        assert!(s.admit(RequestId([8; 16]), version(81)).is_err());
        assert!(s.commit(digest()).is_err());
        s.stage_trial(UpdateSlot::B).unwrap(); s.boot(true, true).unwrap(); s.mark_healthy(UpdateSlot::B).unwrap();
        s.activated(digest()).unwrap(); s.set_status(id, UpdateStatus::Activated).unwrap();
        s.commit(digest()).unwrap(); s.set_status(id, UpdateStatus::Committed).unwrap();
        assert_eq!(s.floor, version(80)); assert_eq!(s.serial, 11); assert_eq!(s.highest_serial, 11);
        assert_eq!(s.committed_digest, Sha256Digest([1; 32])); assert_eq!(s.effective_digest(), digest());
        assert_eq!(s.status(id), Some(UpdateStatus::Activated));
        assert_eq!(s.admit(id, old.version).unwrap(), UpdateStatus::Activated);
        s.set_status(id, UpdateStatus::Maintenance).unwrap();
        assert_eq!(s.status(id), Some(UpdateStatus::Maintenance));
        s.finish_resume().unwrap();
        assert_eq!(s.status(id), Some(UpdateStatus::Committed));
        assert!(s.admit(RequestId([8; 16]), version(80)).is_err());
        let mut fixed = manifest(); fixed.version = version(81); fixed.serial = 12;
        let fixed_digest = Sha256Digest([3; 32]);
        let mut unsupported = fixed.clone(); unsupported.minimum_version = version(80);
        assert!(s.check_manifest(&unsupported, fixed_digest, 150).is_err());
        s.admit(RequestId([8; 16]), fixed.version).unwrap();
        s.prepare(&fixed, fixed_digest, 150).unwrap();
        s.stage_trial(UpdateSlot::A).unwrap(); s.boot(true, true).unwrap(); s.mark_healthy(UpdateSlot::A).unwrap();
        s.activated(fixed_digest).unwrap(); s.commit(fixed_digest).unwrap();
        assert_eq!(s.floor, version(81)); assert_eq!(s.serial, 12);
        assert!(s.running_generation.is_none()); assert_eq!(s.effective_digest(), fixed_digest);
    }
    #[test]
    fn emergency_failed_first_root_boot_preserves_previous_generation_and_floor() {
        let (mut s, old) = emergency_fixture();
        let id = RequestId([7; 16]);
        s.prepare_emergency(&old, digest(), 150, id).unwrap();
        s.stage_trial(UpdateSlot::B).unwrap(); assert_eq!(s.boot(true, true).unwrap(), UpdateSlot::B);
        let mut s: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(s.boot(true, true).unwrap(), UpdateSlot::A);
        s.interrupted_requests();
        assert!(s.pending.is_none()); assert_eq!(s.status(id), Some(UpdateStatus::Failed));
        assert_eq!(s.effective_digest(), Sha256Digest([1; 32]));
        assert_eq!(s.floor, version(80)); assert_eq!(s.highest_serial, 11);
        assert!(s.prepare_emergency(&old, digest(), 150, id).is_err());
    }
    #[test]
    fn emergency_every_durable_state_point_preserves_monotonic_floor() {
        for crash_step in 0..7 {
            let (mut memory, old) = emergency_fixture();
            let mut disk = Disk { persisted: memory.clone(), fail: false };
            let id = RequestId([7; 16]);
            for step in 0..7 {
                disk.fail = step == crash_step;
                let result = transaction(&mut memory, &mut disk, |s| match step {
                    0 => s.prepare_emergency(&old, digest(), 150, id),
                    1 => s.stage_trial(UpdateSlot::B),
                    2 => s.boot(true, true).map(|_| ()),
                    3 => s.mark_healthy(UpdateSlot::B),
                    4 => { s.activated(digest())?; s.set_status(id, UpdateStatus::Activated) },
                    5 => { s.commit(digest())?; s.set_status(id, UpdateStatus::Committed) },
                    _ => s.finish_resume(),
                });
                assert_eq!(disk.persisted.floor, version(80)); assert_eq!(disk.persisted.serial, 11);
                assert_eq!(disk.persisted.highest_serial, 11);
                if step == crash_step { assert!(result.is_err()); break; }
                result.unwrap();
            }
        }
    }
    #[test]
    fn unavailable_readiness_is_never_success() {
        assert!(UnavailableActivator.activate_and_read_back(&manifest(), digest()).is_err());
    }
}
