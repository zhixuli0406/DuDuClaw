//! Durable trust lifecycle. Only the root daemon/admin adapter may persist this ledger.
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{Error, Result};

pub const MAX_ROOT_HISTORY: usize = 32;
pub const MAX_RELEASE_HISTORY: usize = 256;
pub const MAX_EMERGENCY_HISTORY: usize = 32;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRoot {
    pub bytes: String,
    pub signatures: Vec<String>,
    pub accepted_at_unix: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedRelease {
    pub digest: Sha256Digest,
    pub root_version: u64,
    pub accepted_at_unix: u64,
    pub product: Product,
    pub arch: Architecture,
    pub version: ReleaseVersion,
    pub irreversible: bool,
    pub has_migrations: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmergencyIntent {
    pub serial: u64,
    pub digest: Sha256Digest,
    pub authorization_digest: Sha256Digest,
    pub from_digest: Sha256Digest,
    pub accepted_at_unix: u64,
    pub root_version: u64,
    pub authorization: String,
    pub signatures: Vec<String>,
    #[serde(default)]
    pub failed: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustLedger {
    pub schema_version: u16,
    pub pin: Sha256Digest,
    pub roots: Vec<SignedRoot>,
    pub releases: Vec<AcceptedRelease>,
    pub emergency_serial: u64,
    pub pending_emergency: Option<EmergencyIntent>,
    pub emergencies: Vec<EmergencyIntent>,
}
fn digest(bytes: &[u8]) -> Sha256Digest { Sha256Digest(Sha256::digest(bytes).into()) }
impl TrustLedger {
    /// Bootstrap pin was independently checked by G, never supplied by a socket caller.
    pub fn bootstrap(bytes: &[u8], signatures: &[String], pin: Sha256Digest, now: u64) -> Result<Self> {
        crate::trust::verify_root(bytes, signatures, pin, now)?;
        Ok(Self { schema_version: 1, pin, roots: vec![SignedRoot {
            bytes: String::from_utf8(bytes.to_vec()).map_err(|_| Error::Invalid)?,
            signatures: signatures.to_vec(), accepted_at_unix: now }], releases: vec![],
            emergency_serial: 0, pending_emergency: None, emergencies: vec![] })
    }
    fn checked_roots(&self, now: u64) -> Result<Vec<RootMetadata>> {
        if self.schema_version != 1 || self.roots.is_empty() || self.roots.len() > MAX_ROOT_HISTORY
            || self.releases.len() > MAX_RELEASE_HISTORY || self.emergencies.len() > MAX_EMERGENCY_HISTORY {
            return Err(Error::Invalid);
        }
        let mut result = Vec::new();
        let first = &self.roots[0];
        if first.accepted_at_unix > now { return Err(Error::Signature); }
        result.push(crate::trust::verify_root(first.bytes.as_bytes(), &first.signatures, self.pin, first.accepted_at_unix)?);
        for (index, signed) in self.roots.iter().enumerate().skip(1) {
            if signed.accepted_at_unix < self.roots[index - 1].accepted_at_unix || signed.accepted_at_unix > now {
                return Err(Error::Signature);
            }
            let root = crate::trust::rotate_root(result.last().ok_or(Error::Invalid)?, signed.bytes.as_bytes(),
                &signed.signatures, signed.accepted_at_unix)?;
            result.push(root);
        }
        crate::trust::validate_root(result.last().ok_or(Error::Invalid)?, now)?;
        for (index, release) in self.releases.iter().enumerate() {
            let root = result.iter().find(|r| r.version == release.root_version).ok_or(Error::Signature)?;
            if release.accepted_at_unix > now || release.accepted_at_unix >= root.expires_at_unix
                || self.releases.iter().take(index).any(|r| r.digest == release.digest) {
                return Err(Error::Signature);
            }
        }
        for (index, emergency) in self.emergencies.iter().enumerate() {
            if emergency.serial == 0 || emergency.serial > self.emergency_serial
                || self.emergencies.iter().take(index).any(|e| e.serial >= emergency.serial)
                || !self.releases.iter().any(|r| r.digest == emergency.digest)
                || emergency.accepted_at_unix > now || emergency.authorization_digest != digest(emergency.authorization.as_bytes()) {
                return Err(Error::Signature);
            }
            let root = result.iter().find(|r| r.version == emergency.root_version).ok_or(Error::Signature)?;
            let auth = crate::trust::verify_emergency(root, emergency.authorization.as_bytes(), &emergency.signatures,
                emergency.digest, emergency.serial - 1, emergency.accepted_at_unix)?;
            if auth.serial != emergency.serial { return Err(Error::Signature); }
        }
        if let Some(pending) = &self.pending_emergency {
            if pending.failed || pending.accepted_at_unix > now || pending.authorization_digest != digest(pending.authorization.as_bytes())
                || pending.serial != self.emergency_serial || pending.serial == 0
                || self.emergencies.iter().any(|e| e.serial >= pending.serial)
                || !self.releases.iter().any(|r| r.digest == pending.digest)
                || !self.releases.iter().any(|r| r.digest == pending.from_digest) {
                return Err(Error::Signature);
            }
            let root = result.last().ok_or(Error::Invalid)?;
            if root.version != pending.root_version { return Err(Error::Signature); }
            let auth = crate::trust::verify_emergency(root, pending.authorization.as_bytes(),
                &pending.signatures, pending.digest, pending.serial - 1, pending.accepted_at_unix)?;
            if auth.serial != pending.serial { return Err(Error::Signature); }
        }
        Ok(result)
    }
    pub fn current(&self, now: u64) -> Result<RootMetadata> {
        self.checked_roots(now)?.pop().ok_or(Error::Invalid)
    }
    /// A freeze/revocation is a normal adjacent, dual-threshold signed root rotation.
    pub fn rotate(&mut self, bytes: &[u8], signatures: &[String], now: u64) -> Result<()> {
        if self.pending_emergency.is_some() { return Err(Error::Busy); }
        let old = self.current(now)?;
        // Crash retry after the new root landed is idempotent, never a second rotation.
        if self.roots.last().is_some_and(|r| r.bytes.as_bytes() == bytes && r.signatures == signatures) { return Ok(()); }
        if self.roots.len() == MAX_ROOT_HISTORY { return Err(Error::Maintenance); }
        crate::trust::rotate_root(&old, bytes, signatures, now)?;
        self.roots.push(SignedRoot { bytes: String::from_utf8(bytes.to_vec()).map_err(|_| Error::Invalid)?,
            signatures: signatures.to_vec(), accepted_at_unix: now });
        Ok(())
    }
    /// Remember the exact target-authenticated digest before it can be activated.
    pub fn record_release(&mut self, bytes: &[u8], signature: &str, now: u64) -> Result<ReleaseManifest> {
        let root = self.current(now)?;
        let manifest = crate::trust::verify_release(&root, bytes, signature, now)?;
        let digest = digest(bytes);
        if self.releases.iter().any(|r| r.digest == digest) {
            self.verify_installed(bytes, signature, now)?;
            return Ok(manifest);
        }
        if self.releases.len() == MAX_RELEASE_HISTORY { return Err(Error::Maintenance); }
        self.releases.push(AcceptedRelease { digest, root_version: root.version, accepted_at_unix: now,
            product: manifest.product, arch: manifest.arch, version: manifest.version,
            irreversible: manifest.migrations.iter().any(|m| !m.reversible), has_migrations: !manifest.migrations.is_empty() });
        Ok(manifest)
    }
    /// Import G-provisioned, root-owned slot receipts before the first key rotation.
    /// The adapter must bind this receipt to the root-owned committed/pending digest.
    pub fn record_provisioned(&mut self, bytes: &[u8], signature: &str, accepted_at: u64, now: u64) -> Result<()> {
        let root = self.current(now)?;
        let manifest = crate::trust::verify_installed_release(&root, bytes, signature, accepted_at, now)?;
        let digest = digest(bytes);
        if self.releases.iter().any(|r| r.digest == digest) { return Ok(()); }
        if self.releases.len() == MAX_RELEASE_HISTORY { return Err(Error::Maintenance); }
        self.releases.push(AcceptedRelease { digest, root_version: root.version, accepted_at_unix: accepted_at,
            product: manifest.product, arch: manifest.arch, version: manifest.version,
            irreversible: manifest.migrations.iter().any(|m| !m.reversible), has_migrations: !manifest.migrations.is_empty() });
        Ok(())
    }
    /// Old target material remains usable only for an exact previously accepted digest.
    /// Current root revocations still fence old targets, and freeze blocks only new admission.
    pub fn verify_installed(&self, bytes: &[u8], signature: &str, now: u64) -> Result<ReleaseManifest> {
        let roots = self.checked_roots(now)?;
        let current = roots.last().ok_or(Error::Invalid)?;
        let record = self.releases.iter().find(|r| r.digest == digest(bytes)).ok_or(Error::Signature)?;
        let historic = roots.iter().find(|r| r.version == record.root_version).ok_or(Error::Signature)?;
        if current.revoked_key_ids.iter().any(|id| *id == historic.target_key.key_id) { return Err(Error::Signature); }
        // Historical root expiry was checked when accepted. Requiring it to remain
        // fresh would strand an installed generation after an ordinary key rotation.
        let manifest = crate::trust::verify_installed_release(historic, bytes, signature,
            record.accepted_at_unix, record.accepted_at_unix)?;
        if manifest.product != record.product || manifest.arch != record.arch || manifest.version != record.version {
            return Err(Error::Signature);
        }
        Ok(manifest)
    }
    pub fn prepare_emergency(&mut self, bytes: &[u8], signatures: &[String], target_bytes: &[u8],
        target_signature: &str, from_digest: Sha256Digest, now: u64) -> Result<EmergencyIntent> {
        let root = self.current(now)?;
        let target_digest = digest(target_bytes);
        let target = self.verify_installed(target_bytes, target_signature, now)?;
        let from = self.releases.iter().find(|r| r.digest == from_digest).ok_or(Error::Signature)?;
        if from.product != target.product || from.arch != target.arch || target.version >= from.version {
            return Err(Error::Rollback);
        }
        // Data rollback needs G's explicit migration executor; never silently omit it.
        if !target.migrations.is_empty() || self.releases.iter().any(|r| r.product == from.product && r.arch == from.arch
            && r.version > target.version && r.version <= from.version && r.has_migrations) { return Err(Error::Incompatible); }
        let authorization_digest = digest(bytes);
        if let Some(pending) = &self.pending_emergency {
            if pending.digest == target_digest && pending.from_digest == from_digest
                && pending.authorization_digest == authorization_digest {
                // Expiry gates initial admission. An exact durable intent remains
                // resumable after a crash; current root validity was checked above.
                crate::trust::verify_emergency(&root, bytes, signatures, target_digest, pending.serial - 1, pending.accepted_at_unix)?;
                return Ok(pending.clone());
            }
            return Err(Error::Busy);
        }
        if self.emergencies.len() == MAX_EMERGENCY_HISTORY { return Err(Error::Maintenance); }
        let authorization = crate::trust::verify_emergency(&root, bytes, signatures, target_digest, self.emergency_serial, now)?;
        let pending = EmergencyIntent { serial: authorization.serial, digest: target_digest, authorization_digest, from_digest,
            accepted_at_unix: now, root_version: root.version, authorization: String::from_utf8(bytes.to_vec()).map_err(|_| Error::Invalid)?,
            signatures: signatures.to_vec(), failed: false };
        self.emergency_serial = authorization.serial;
        self.pending_emergency = Some(pending.clone());
        Ok(pending)
    }
    /// A failed first updater boot consumes its authorization permanently.
    pub fn retire_emergency(&mut self, intent: &EmergencyIntent) -> Result<()> {
        let pending = self.pending_emergency.as_ref().ok_or(Error::Invalid)?;
        if pending.serial != intent.serial || pending.digest != intent.digest
            || pending.authorization_digest != intent.authorization_digest || pending.from_digest != intent.from_digest {
            return Err(Error::Invalid);
        }
        let mut record = pending.clone(); record.failed = true;
        self.emergencies.push(record);
        self.pending_emergency = None;
        Ok(())
    }
    /// Called only after the installer reports genuine restart/readback success.
    /// The ordinary version floor and serial are never lowered by this ledger.
    pub fn complete_emergency(&mut self, intent: &EmergencyIntent) -> Result<()> {
        if self.pending_emergency.is_none() && self.emergencies.iter().any(|e| !e.failed
            && e.serial == intent.serial && e.digest == intent.digest && e.authorization_digest == intent.authorization_digest
            && e.from_digest == intent.from_digest) { return Ok(()); }
        let pending = self.pending_emergency.as_ref().ok_or(Error::Invalid)?;
        if intent.failed || pending.serial != intent.serial || pending.digest != intent.digest
            || pending.authorization_digest != intent.authorization_digest || pending.from_digest != intent.from_digest {
            return Err(Error::Invalid);
        }
        self.emergencies.push(pending.clone());
        self.pending_emergency = None;
        Ok(())
    }
}

/// Implementations save the entire snapshot with file fsync, rename and directory fsync.
/// No caller may continue after a failed save: the rename may already have landed.
pub trait DurableTrust { fn save_trust(&mut self, next: &TrustLedger) -> Result<()>; }
pub struct TrustWriter<D> { disk: D, poisoned: bool }
impl<D: DurableTrust> TrustWriter<D> {
    pub fn new(disk: D) -> Self { Self { disk, poisoned: false } }
    pub fn transaction<T>(&mut self, ledger: &mut TrustLedger, f: impl FnOnce(&mut TrustLedger) -> Result<T>) -> Result<T> {
        if self.poisoned { return Err(Error::Maintenance); }
        let mut next = ledger.clone();
        let value = f(&mut next)?;
        if let Err(error) = self.disk.save_trust(&next) { self.poisoned = true; return Err(error); }
        *ledger = next;
        Ok(value)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{metadata::canonical_bytes, sshsig::test_support::{key, sign}};
    use ed25519_dalek::SigningKey as PrivateKey;

    pub(crate) fn root(seed: u8) -> (RootMetadata, Vec<PrivateKey>, PrivateKey) {
        let keys = [key(seed, Some(ROOT_SK_APPLICATION)), key(seed + 1, None), key(seed + 2, None)];
        let (target, target_key) = key(seed + 3, Some(TARGET_SK_APPLICATION));
        (RootMetadata { schema_version: 1, version: 1, expires_at_unix: 1000,
            root_threshold: 2, root_keys: keys.iter().map(|(_, k)| k.clone()).collect(),
            target_key, revoked_key_ids: vec![], frozen: false }, keys.into_iter().map(|(s, _)| s).collect(), target)
    }
    pub(crate) fn root_sign(root: &RootMetadata, keys: &[PrivateKey], bytes: &[u8], namespace: &str) -> Vec<String> {
        [1, 2].iter().map(|i| sign(&keys[*i], &root.root_keys[*i], namespace, bytes, 5)).collect()
    }
    pub(crate) fn setup() -> (TrustLedger, RootMetadata, Vec<PrivateKey>, PrivateKey) {
        let (root, keys, target) = root(1);
        let bytes = canonical_bytes(&root).unwrap();
        let signatures = root_sign(&root, &keys, &bytes, ROOT_SSHSIG_NAMESPACE);
        (TrustLedger::bootstrap(&bytes, &signatures, digest(&bytes), 100).unwrap(), root, keys, target)
    }
    fn manifest(minor: u32) -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor, patch: 0 },
            minimum_version: ReleaseVersion { major: 1, minor: 70, patch: 0 }, serial: minor as u64,
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
            created_at_unix: 100, expires_at_unix: 500,
            artifacts: vec![ReleaseArtifact { id: "gateway".into(), sha256: Sha256Digest([1; 32]),
                size_bytes: 10, mode: 0o755, install_target: InstallTarget::Gateway }],
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
                updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] }
    }
    fn release(root: &RootMetadata, target: &PrivateKey, manifest: &ReleaseManifest) -> (Vec<u8>, String) {
        let bytes = canonical_bytes(manifest).unwrap();
        let signature = sign(target, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &bytes, 5);
        (bytes, signature)
    }
    fn rotate(ledger: &mut TrustLedger, old: &RootMetadata, old_keys: &[PrivateKey], new: &RootMetadata, new_keys: &[PrivateKey]) {
        let bytes = canonical_bytes(new).unwrap();
        let signatures: Vec<_> = root_sign(old, old_keys, &bytes, ROOT_SSHSIG_NAMESPACE).into_iter()
            .chain(root_sign(new, new_keys, &bytes, ROOT_SSHSIG_NAMESPACE)).collect();
        ledger.rotate(&bytes, &signatures, 200).unwrap();
        // The exact already-landed mutation can safely be retried after restart.
        ledger.rotate(&bytes, &signatures, 200).unwrap();
    }
    #[test]
    fn historic_targets_survive_rotation_and_old_root_expiry_but_not_revocation() {
        let (mut ledger, old, old_keys, target) = setup();
        let (bytes, sig) = release(&old, &target, &manifest(80));
        ledger.record_release(&bytes, &sig, 150).unwrap();
        let (mut new, new_keys, _) = root(10); new.version = 2; new.expires_at_unix = 3000;
        rotate(&mut ledger, &old, &old_keys, &new, &new_keys);
        let ledger: TrustLedger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert_eq!(ledger.current(1500).unwrap(), new);
        assert!(ledger.verify_installed(&bytes, &sig, 1500).is_ok());
        assert!(ledger.verify_installed(&bytes, &sig, 3000).is_err());
        // A root-key rotation which revokes the old target immediately fences its history.
        let (mut revoked_ledger, old, old_keys, target) = setup();
        let (bytes, sig) = release(&old, &target, &manifest(80));
        revoked_ledger.record_release(&bytes, &sig, 150).unwrap();
        new.revoked_key_ids.push(old.target_key.key_id);
        rotate(&mut revoked_ledger, &old, &old_keys, &new, &new_keys);
        assert!(revoked_ledger.verify_installed(&bytes, &sig, 600).is_err());
    }
    #[test]
    fn freeze_persists_and_does_not_revoke_already_accepted_generation() {
        let (mut ledger, old, keys, target) = setup();
        let (bytes, sig) = release(&old, &target, &manifest(80));
        ledger.record_release(&bytes, &sig, 150).unwrap();
        let mut frozen = old.clone(); frozen.version = 2; frozen.frozen = true;
        let bytes_root = canonical_bytes(&frozen).unwrap();
        let signatures = root_sign(&old, &keys, &bytes_root, ROOT_SSHSIG_NAMESPACE);
        ledger.rotate(&bytes_root, &signatures, 200).unwrap();
        assert!(ledger.current(300).unwrap().frozen);
        assert!(ledger.verify_installed(&bytes, &sig, 600).is_ok());
        assert!(ledger.record_release(&bytes, &sig, 300).is_err());
    }
    #[test]
    fn release_history_is_exact_and_cannot_invent_acceptance_after_rotation() {
        let (mut ledger, old, old_keys, target) = setup();
        let (bytes, sig) = release(&old, &target, &manifest(80));
        ledger.record_release(&bytes, &sig, 150).unwrap();
        ledger.record_release(&bytes, &sig, 151).unwrap();
        assert_eq!(ledger.releases.len(), 1);
        assert_eq!(ledger.releases[0].accepted_at_unix, 150);
        let (other, other_sig) = release(&old, &target, &manifest(79));
        let (mut new, new_keys, _) = root(10); new.version = 2;
        rotate(&mut ledger, &old, &old_keys, &new, &new_keys);
        assert!(ledger.verify_installed(&other, &other_sig, 300).is_err());
        assert!(ledger.record_release(&other, &other_sig, 300).is_err());
        assert!(ledger.verify_installed(&bytes, &other_sig, 300).is_err());
    }
    fn emergency(root: &RootMetadata, keys: &[PrivateKey], target: Sha256Digest, serial: u64) -> (Vec<u8>, Vec<String>) {
        let bytes = canonical_bytes(&EmergencyAuthorization { schema_version: 1, serial,
            previously_signed_manifest: target, expires_at_unix: 400 }).unwrap();
        let sigs = root_sign(root, keys, &bytes, EMERGENCY_SSHSIG_NAMESPACE);
        (bytes, sigs)
    }
    #[test]
    fn emergency_is_durable_exact_single_use_and_restart_idempotent() {
        let (mut ledger, root, keys, target) = setup();
        let (old, old_sig) = release(&root, &target, &manifest(79));
        let (current, current_sig) = release(&root, &target, &manifest(80));
        ledger.record_release(&old, &old_sig, 150).unwrap();
        ledger.record_release(&current, &current_sig, 150).unwrap();
        let (authorization, sigs) = emergency(&root, &keys, digest(&old), 1);
        let intent = ledger.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 200).unwrap();
        assert_eq!(ledger.emergency_serial, 1);
        let mut resumed: TrustLedger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        resumed.current(201).unwrap();
        let retry = resumed.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 201).unwrap();
        assert_eq!(retry.serial, intent.serial);
        assert!(resumed.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 450).is_ok());
        assert!(resumed.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 1000).is_err());
        let (different, different_sigs) = emergency(&root, &keys, digest(&old), 2);
        assert!(resumed.prepare_emergency(&different, &different_sigs, &old, &old_sig, digest(&current), 201).is_err());
        resumed.complete_emergency(&intent).unwrap();
        resumed.complete_emergency(&intent).unwrap();
        assert!(resumed.pending_emergency.is_none());
        assert_eq!(resumed.emergencies.len(), 1);
        assert!(resumed.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 202).is_err());
        assert!(resumed.prepare_emergency(&different, &different_sigs, &old, &old_sig, digest(&current), 450).is_err());
    }
    #[test]
    fn failed_emergency_trial_retires_authorization_without_replay() {
        let (mut ledger, root, keys, target) = setup();
        let (old, old_sig) = release(&root, &target, &manifest(79));
        let (current, current_sig) = release(&root, &target, &manifest(80));
        ledger.record_release(&old, &old_sig, 150).unwrap();
        ledger.record_release(&current, &current_sig, 150).unwrap();
        let (auth, sigs) = emergency(&root, &keys, digest(&old), 1);
        let intent = ledger.prepare_emergency(&auth, &sigs, &old, &old_sig, digest(&current), 200).unwrap();
        ledger.retire_emergency(&intent).unwrap();
        assert!(ledger.pending_emergency.is_none()); assert!(ledger.emergencies[0].failed);
        assert_eq!(ledger.emergency_serial, 1);
        assert!(ledger.prepare_emergency(&auth, &sigs, &old, &old_sig, digest(&current), 201).is_err());
        assert!(ledger.complete_emergency(&intent).is_err());
    }
    #[test]
    fn emergency_refuses_irreversible_migration_and_cross_product_or_forward() {
        for case in 0..4 {
            let (mut ledger, root, keys, target) = setup();
            let mut old_manifest = manifest(79);
            let mut current_manifest = manifest(80);
            match case {
                0 => current_manifest.migrations.push(DataMigration { id: "irreversible".into(), sha256: Sha256Digest([8; 32]), reversible: false }),
                1 => old_manifest.product = Product::DuduclawPro,
                2 => old_manifest.version = current_manifest.version,
                _ => current_manifest.migrations.push(DataMigration { id: "reversible".into(), sha256: Sha256Digest([8; 32]), reversible: true }),
            }
            let (old, old_sig) = release(&root, &target, &old_manifest);
            let (current, current_sig) = release(&root, &target, &current_manifest);
            ledger.record_release(&old, &old_sig, 150).unwrap();
            ledger.record_release(&current, &current_sig, 150).unwrap();
            let (authorization, sigs) = emergency(&root, &keys, digest(&old), 1);
            assert!(ledger.prepare_emergency(&authorization, &sigs, &old, &old_sig, digest(&current), 200).is_err());
            assert_eq!(ledger.emergency_serial, 0);
        }
    }
    #[test]
    fn failed_trust_snapshot_cannot_be_overwritten_from_stale_memory() {
        struct Disk { value: TrustLedger, writes: usize, fail_after_rename: bool }
        impl DurableTrust for Disk {
            fn save_trust(&mut self, next: &TrustLedger) -> Result<()> {
                self.writes += 1;
                if self.fail_after_rename { self.value = next.clone(); }
                Err(Error::Maintenance)
            }
        }
        for landed in [false, true] {
            let (mut ledger, root, keys, _) = setup();
            let disk = Disk { value: ledger.clone(), writes: 0, fail_after_rename: landed };
            let mut writer = TrustWriter::new(disk);
            let mut next = root.clone(); next.version = 2; next.frozen = true;
            let bytes = canonical_bytes(&next).unwrap();
            let signatures = root_sign(&root, &keys, &bytes, ROOT_SSHSIG_NAMESPACE);
            assert!(writer.transaction(&mut ledger, |l| l.rotate(&bytes, &signatures, 200)).is_err());
            assert_eq!(ledger.roots.len(), 1);
            assert!(writer.transaction(&mut ledger, |_| Ok(())).is_err());
            assert_eq!(writer.disk.writes, 1);
            assert_eq!(writer.disk.value.current(201).unwrap().frozen, landed);
        }
    }
    #[test]
    fn ledger_refuses_corrupt_history_future_clock_and_invalid_pending() {
        let (ledger, _, _, _) = setup();
        assert!(ledger.current(99).is_err());
        let mut corrupt = ledger.clone(); corrupt.pin = Sha256Digest([0; 32]);
        assert!(corrupt.current(200).is_err());
        let mut corrupt = ledger.clone(); corrupt.schema_version = 2;
        assert!(corrupt.current(200).is_err());
        let mut corrupt = ledger; corrupt.pending_emergency = Some(EmergencyIntent { serial: 1,
            digest: Sha256Digest([0; 32]), authorization_digest: Sha256Digest([0; 32]), from_digest: Sha256Digest([0; 32]), accepted_at_unix: 100, root_version: 1, authorization: "{}".into(), signatures: vec![], failed: false });
        assert!(corrupt.current(200).is_err());
    }
}
