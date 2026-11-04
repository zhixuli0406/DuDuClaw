//! Root-only emergency preparation, sharing the ordinary A/B/readback transaction.
use std::path::Path;
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::{EmergencyAuthorization, ReleaseManifest, UpdateStatus}};
use sha2::{Digest, Sha256};
use crate::{Error, Result, state::{self, State, FailClosedDisk}, trust_store::{TrustLedger, TrustWriter, EmergencyIntent}};
use super::{files::Directory, trust_admin::{self, SignedDocument}, CONFIG_ROOT, INSTALL_ROOT, STATE_ROOT};

fn request_id(intent: &EmergencyIntent) -> RequestId {
    let mut bytes = [0; 16]; bytes.copy_from_slice(&intent.authorization_digest.0[..16]); RequestId(bytes)
}
fn target(digest: Sha256Digest) -> Result<(Vec<u8>, String)> {
    let dir = Directory::open(Path::new(INSTALL_ROOT))?.child("generations")?.child(&hex::encode(digest.0))?;
    let bytes = dir.bytes("manifest.json", 256 * 1024)?;
    if Sha256Digest(Sha256::digest(&bytes).into()) != digest { return Err(Error::Signature); }
    let sig = String::from_utf8(dir.bytes("manifest.json.sig", 16 * 1024)?).map_err(|_| Error::Invalid)?;
    Ok((bytes, sig))
}
fn document() -> Result<SignedDocument> {
    Directory::open(Path::new(CONFIG_ROOT))?.json("emergency.json")
}
fn check_document(ledger: &mut TrustLedger, intent: &EmergencyIntent) -> Result<ReleaseManifest> {
    let (bytes, signature) = target(intent.digest)?;
    let checked = ledger.prepare_emergency(intent.authorization.as_bytes(), &intent.signatures,
        &bytes, &signature, intent.from_digest, super::now()?)?;
    if checked.serial != intent.serial || checked.authorization_digest != intent.authorization_digest {
        return Err(Error::Signature);
    }
    ledger.verify_installed(&bytes, &signature, super::now()?)
}
fn verify_artifacts(manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let generation = Directory::open(Path::new(INSTALL_ROOT))?.child("generations")?.child(&hex::encode(digest.0))?;
    for artifact in &manifest.artifacts {
        let file = generation.read(&super::service::artifact_name(&artifact.install_target)?)?;
        if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(Error::Signature); }
        super::files::verify_fd(file, artifact.size_bytes, artifact.sha256)?;
    }
    generation.sync()
}
fn prepare_state(state: &mut State, disk: &mut FailClosedDisk<Directory>, ledger: &TrustLedger,
    intent: &EmergencyIntent, manifest: &ReleaseManifest) -> Result<()> {
    let receipt = ledger.releases.iter().find(|r| r.digest == intent.digest).ok_or(Error::Signature)?;
    verify_artifacts(manifest, intent.digest)?;
    state::transaction(state, disk, |next| next.prepare_emergency(manifest, intent.digest,
        receipt.accepted_at_unix, request_id(intent)))
}
/// Requires the daemon to be stopped. The CLI writes no selector: starting the
/// frozen launcher executes the same durable trial/root self-test/activation path.
pub fn prepare_from_inbox() -> Result<()> {
    super::harden()?;
    let directory = Directory::open(Path::new(STATE_ROOT))?;
    let _lock = directory.lock()?;
    let mut state: State = directory.json("state.json")?;
    state.validate()?;
    if state.pending.is_some() || state.resume_required || state.selector.trial_slot.is_some()
        || state.requests.iter().any(|r| !state::terminal(r.status)) { return Err(Error::Busy); }
    let document = document()?;
    let auth: EmergencyAuthorization = crate::metadata::decode_canonical(document.document.as_bytes())?;
    let (bytes, signature) = target(auth.previously_signed_manifest)?;
    let mut ledger = trust_admin::ledger()?;
    let mut writer = TrustWriter::new(Directory::open(Path::new(STATE_ROOT))?);
    // The serial is consumed before State.pending can be made visible. A crash in
    // between is repaired from the exact immutable intent by reconcile().
    let intent = writer.transaction(&mut ledger, |next| next.prepare_emergency(document.document.as_bytes(),
        &document.signatures, &bytes, &signature, state.effective_digest(), super::now()?))?;
    let manifest = ledger.verify_installed(&bytes, &signature, super::now()?)?;
    prepare_state(&mut state, &mut FailClosedDisk::new(directory), &ledger, &intent, &manifest)
}
/// Run while holding the daemon lock before computing recovery_request(). Never
/// reconstruct a failed trial: its request record consumes that intent forever.
pub fn reconcile(state: &mut State, disk: &mut FailClosedDisk<Directory>) -> Result<()> {
    let mut ledger = trust_admin::ledger()?;
    let Some(intent) = ledger.pending_emergency.clone() else {
        if state.pending.as_ref().is_some_and(|p| p.emergency) { return Err(Error::Maintenance); }
        if let Some(running) = state.running_generation {
            if !ledger.emergencies.iter().any(|e| e.digest == running && !e.failed) { return Err(Error::Maintenance); }
        }
        return Ok(());
    };
    if state.pending.is_none() && state.effective_digest() == intent.digest {
        return complete_committed(state);
    }
    if state.effective_digest() != intent.from_digest { return Err(Error::Maintenance); }
    if let Some(pending) = &state.pending {
        if !pending.emergency || pending.digest != intent.digest { return Err(Error::Maintenance); }
        return validate_pending(state);
    }
    if state.requests.iter().any(|r| r.id == request_id(&intent)) {
        // boot() already returned to the old verified slot before clearing pending.
        if super::activation::read_current()? != intent.from_digest { return Err(Error::Maintenance); }
        let mut writer = TrustWriter::new(Directory::open(Path::new(STATE_ROOT))?);
        writer.transaction(&mut ledger, |next| next.retire_emergency(&intent))?;
        return Ok(());
    }
    let manifest = check_document(&mut ledger, &intent)?;
    prepare_state(state, disk, &ledger, &intent, &manifest)
}
/// Call before staging and again before activation. An old target signature alone
/// never grants the emergency exception; the live root authorization is mandatory.
pub fn validate_pending(state: &State) -> Result<()> {
    let pending = state.pending.as_ref().ok_or(Error::Invalid)?;
    if !pending.emergency { return Ok(()); }
    let mut ledger = trust_admin::ledger()?;
    let intent = ledger.pending_emergency.clone().ok_or(Error::Signature)?;
    if intent.digest != pending.digest || intent.from_digest != state.effective_digest() { return Err(Error::Signature); }
    let manifest = check_document(&mut ledger, &intent)?;
    state.check_pending_manifest(&manifest, intent.digest)
}
/// Call after the state commit and before admission is resumed. Its atomic save is
/// idempotent after a crash; the ordinary floor/serial remain at their high values.
pub fn complete_committed(state: &State) -> Result<()> {
    let mut ledger = trust_admin::ledger()?;
    let Some(intent) = ledger.pending_emergency.clone() else { return Ok(()); };
    if state.pending.is_some() || state.running_generation != Some(intent.digest)
        || !state.resume_required || !state.requests.iter().any(|r| r.id == request_id(&intent) && matches!(r.status, UpdateStatus::Committed | UpdateStatus::Maintenance | UpdateStatus::Activated)) {
        return Err(Error::Maintenance);
    }
    TrustWriter::new(Directory::open(Path::new(STATE_ROOT))?).transaction(&mut ledger, |next| next.complete_emergency(&intent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::canonical_bytes, sshsig::test_support::sign,
        trust_store::tests::{setup, root_sign}, state::DurableState};
    use duduclaw_isolation::update::{RELEASE_SSHSIG_NAMESPACE, EMERGENCY_SSHSIG_NAMESPACE, UpdateSlot};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    #[ignore = "Requires root on Linux with openat2; checks separate trust/state durable commit recovery"]
    fn iso_root_emergency_snapshots_preserve_floor_across_commit_boundary() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("duduclaw-emergency-test-").tempdir_in("/run").unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = Directory::open(temp.path()).unwrap();
        let _lock = dir.lock().unwrap();
        let (mut s, old) = crate::state::tests::emergency_fixture();
        let (mut ledger, root, keys, target_key) = setup();
        let old_bytes = canonical_bytes(&old).unwrap();
        let old_sig = sign(&target_key, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &old_bytes, 5);
        let old_digest = Sha256Digest(Sha256::digest(&old_bytes).into());
        let mut current = old.clone(); current.version = s.floor; current.serial = s.serial;
        let current_bytes = canonical_bytes(&current).unwrap();
        let current_sig = sign(&target_key, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &current_bytes, 5);
        s.committed_digest = Sha256Digest(Sha256::digest(&current_bytes).into()); s.highest_digest = s.committed_digest;
        ledger.record_release(&old_bytes, &old_sig, 150).unwrap();
        ledger.record_release(&current_bytes, &current_sig, 150).unwrap();
        let auth = canonical_bytes(&EmergencyAuthorization { schema_version: 1, serial: 1,
            previously_signed_manifest: old_digest, expires_at_unix: 400 }).unwrap();
        let signatures = root_sign(&root, &keys, &auth, EMERGENCY_SSHSIG_NAMESPACE);
        let mut trust_writer = TrustWriter::new(Directory::open(temp.path()).unwrap());
        let intent = trust_writer.transaction(&mut ledger, |next| next.prepare_emergency(&auth, &signatures,
            &old_bytes, &old_sig, s.committed_digest, 160)).unwrap();
        let mut state_disk = FailClosedDisk::new(dir);
        state_disk.save(&s).unwrap();
        // Reopen after the first cross-file boundary: intent is durable, State is old.
        assert!(Directory::open(temp.path()).unwrap().json::<TrustLedger>("trust-ledger.json").unwrap().pending_emergency.is_some());
        assert!(Directory::open(temp.path()).unwrap().json::<State>("state.json").unwrap().pending.is_none());
        let id = request_id(&intent);
        state::transaction(&mut s, &mut state_disk, |next| next.prepare_emergency(&old, old_digest, 150, id)).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| next.stage_trial(UpdateSlot::B)).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| next.boot(true, true).map(|_| ())).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| next.mark_healthy(UpdateSlot::B)).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| {
            next.activated(old_digest)?; next.set_status(id, UpdateStatus::Activated)
        }).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| {
            next.commit(old_digest)?; next.set_status(id, UpdateStatus::Committed)
        }).unwrap();
        // Reopen after commit but before trust completion/Resume; success stays hidden.
        let restarted: State = Directory::open(temp.path()).unwrap().json("state.json").unwrap();
        assert_eq!(restarted.floor, current.version); assert_eq!(restarted.serial, current.serial);
        assert_eq!(restarted.effective_digest(), old_digest); assert!(restarted.resume_required);
        assert_eq!(restarted.status(id), Some(UpdateStatus::Activated));
        let mut trust_restarted: TrustLedger = Directory::open(temp.path()).unwrap().json("trust-ledger.json").unwrap();
        assert!(trust_restarted.pending_emergency.is_some());
        trust_writer.transaction(&mut trust_restarted, |next| next.complete_emergency(&intent)).unwrap();
        state::transaction(&mut s, &mut state_disk, |next| next.finish_resume()).unwrap();
        assert_eq!(s.floor, current.version); assert_eq!(s.status(id), Some(UpdateStatus::Committed));
        assert!(Directory::open(temp.path()).unwrap().json::<TrustLedger>("trust-ledger.json").unwrap().pending_emergency.is_none());
    }
}
