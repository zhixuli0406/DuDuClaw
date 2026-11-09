//! Frozen launcher: root-only selector, verified slot fd, one durable boot attempt.
use std::{ffi::CString, fs::File, os::fd::AsRawFd, path::Path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use crate::{Error, Result, state::{State, DurableState}};
use super::{files::{Directory, verify_fd}, service::TrustBundle, STATE_ROOT, CONFIG_ROOT, INSTALL_ROOT};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotReceipt {
    pub manifest: String,
    pub signature: String,
    pub accepted_at_unix: u64,
}
fn slot_name(slot: UpdateSlot) -> &'static str { match slot { UpdateSlot::A => "a", UpdateSlot::B => "b" } }
fn verified_slot(slot: UpdateSlot, state: &State, _root: &RootMetadata) -> Result<File> {
    let config = Directory::open(Path::new(CONFIG_ROOT))?;
    let receipt: SlotReceipt = config.json(&format!("slot-{}.json", slot_name(slot)))?;
    if receipt.accepted_at_unix > super::now()? { return Err(Error::Signature); }
    // Freshness was enforced at installation; an already accepted slot must keep
    // booting after release expiry. Current root validation/revocation still applies.
    let manifest = super::trust_admin::verify_installed(receipt.manifest.as_bytes(), &receipt.signature, receipt.accepted_at_unix)?;
    let digest = Sha256Digest(Sha256::digest(receipt.manifest.as_bytes()).into());
    if manifest.product != state.product || manifest.arch != state.arch
        || !(digest == state.effective_digest() || digest == state.committed_digest || state.pending.as_ref().is_some_and(|p| p.digest == digest)) {
        return Err(Error::Signature);
    }
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::UpdateSlot).ok_or(Error::Signature)?;
    let file = Directory::open(Path::new(INSTALL_ROOT))?.child("slots")?.child(slot_name(slot))?.read("duduclaw-updated")?;
    verify_fd(file, artifact.size_bytes, artifact.sha256)
}
pub fn verify_executing_slot(state: &State, slot: UpdateSlot, root: &RootMetadata) -> Result<()> {
    if state.selector.active != slot && state.selector.trial_slot != Some(slot) { return Err(Error::Maintenance); }
    if state.selector.trial_slot == Some(slot) && state.selector.attempts != 1 { return Err(Error::Maintenance); }
    let verified = verified_slot(slot, state, root)?;
    let executing = File::open("/proc/self/exe")?;
    use std::os::unix::fs::MetadataExt;
    let a = verified.metadata()?;
    let b = executing.metadata()?;
    if a.ino() != b.ino() || a.dev() != b.dev() { return Err(Error::Maintenance); }
    Ok(())
}
pub fn launch() -> Result<()> {
    super::harden()?;
    let mut disk = Directory::open(Path::new(STATE_ROOT))?;
    let lock = disk.lock()?;
    let mut state: State = disk.json("state.json")?;
    state.validate()?;
    let root = super::service::load_trust()?;
    let mut a = verified_slot(UpdateSlot::A, &state, &root).ok();
    let mut b = verified_slot(UpdateSlot::B, &state, &root).ok();
    if state.pending.as_ref().is_some_and(|p| p.emergency) && state.selector.attempts == 0
        && state.selector.trial_slot.is_some() && super::emergency::validate_pending(&state).is_err() {
        // A rejected emergency trial must not prevent executing the verified
        // known-good fallback. boot() durably retires the unauthorized attempt.
        match state.selector.trial_slot { Some(UpdateSlot::A) => a = None, Some(UpdateSlot::B) => b = None, None => () }
    }
    // boot can return Maintenance after changing state. Persist that outcome too,
    // rather than using the success-only transaction helper.
    let outcome = state.boot(a.is_some(), b.is_some());
    disk.save(&state)?;
    let chosen = outcome?;
    let file = match chosen { UpdateSlot::A => a, UpdateSlot::B => b }.ok_or(Error::Maintenance)?;
    // The fd that was hashed is consumed by execveat, never reopened by pathname.
    // No inherited configuration is allowed to affect the newly privileged binary.
    let args = [CString::new("duduclaw-updated").unwrap(), CString::new("--serve-slot").unwrap(), CString::new(slot_name(chosen)).unwrap()];
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|s| s.as_ptr()).collect(); argv.push(std::ptr::null());
    let env = [CString::new("PATH=/usr/sbin:/usr/bin:/sbin:/bin").unwrap(), CString::new("LANG=C").unwrap()];
    let mut envp: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect(); envp.push(std::ptr::null());
    // All open files use CLOEXEC. The service acquires the lock again after exec;
    // any competing root launcher causes fail-closed startup, never double service.
    let _keep_lock = lock;
    let empty = CString::new("").unwrap();
    unsafe { libc::syscall(libc::SYS_execveat, file.as_raw_fd(), empty.as_ptr(), argv.as_ptr(), envp.as_ptr(), libc::AT_EMPTY_PATH); }
    Err(std::io::Error::last_os_error().into())
}

/// Bootstrap verification library for package G. The digest is compiled into the
/// independently hash-checked bootstrap binary, never supplied by an RPC or flag.
/// This verifies root metadata only; G must also verify BOTH migration signatures
/// and preserve the same package fd through extraction/execution before provisioning.
pub fn verify_bootstrap_root(bytes: &[u8], signatures: &[String], now: u64) -> Result<TrustBundle> {
    let pin = option_env!("DUDUCLAW_BOOTSTRAP_ROOT_SHA256").ok_or(Error::Maintenance)?;
    let raw = hex::decode(pin).map_err(|_| Error::Maintenance)?;
    let digest = Sha256Digest(raw.try_into().map_err(|_| Error::Maintenance)?);
    crate::trust::verify_root(bytes, signatures, digest, now)?;
    Ok(TrustBundle { root: String::from_utf8(bytes.to_vec()).map_err(|_| Error::Invalid)?, signatures: signatures.to_vec(), pin: digest })
}

/// Install only the inactive updater slot. Package G provisions the two empty
/// slot directories; no caller can select paths. This operation must run under
/// the update lock. A crash before the selector write leaves active untouched.
pub fn stage_inactive_slot(state: &mut State, disk: &mut crate::state::FailClosedDisk<Directory>,
    manifest_bytes: &[u8], signature: &str) -> Result<UpdateSlot> {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::PermissionsExt;
    if disk.poisoned() { return Err(Error::Maintenance); }
    let root = super::service::load_trust()?;
    if state.pending.as_ref().is_some_and(|p| p.emergency) { super::emergency::validate_pending(&state)?; }
    let accepted = state.pending.as_ref().ok_or(Error::Invalid)?.accepted_at_unix;
    let manifest = super::trust_admin::verify_installed(manifest_bytes, signature, accepted)?;
    let digest = Sha256Digest(Sha256::digest(manifest_bytes).into());
    super::emergency::validate_pending(state)?;
    if root.frozen && !state.pending.as_ref().is_some_and(|p| p.emergency) { return Err(Error::Maintenance); }
    state.check_pending_manifest(&manifest, digest)?;
    let pending = state.pending.as_ref().ok_or(Error::Invalid)?;
    if pending.digest != digest || pending.phase != UpdateJournalState::Prepared { return Err(Error::Invalid); }
    if let Some(installed) = pending.updater_slot {
        if state.selector.trial_slot == Some(installed)
            || (state.selector.trial_slot.is_none() && state.selector.active == installed) {
            verified_slot(installed, state, &root)?;
            return Ok(installed);
        }
        return Err(Error::Maintenance);
    }
    if state.selector.trial_slot.is_some() { return Err(Error::Invalid); }
    let slot = match state.selector.active { UpdateSlot::A => UpdateSlot::B, UpdateSlot::B => UpdateSlot::A };
    let install = Directory::open(Path::new(INSTALL_ROOT))?;
    let generation = install.child("generations")?.child(&hex::encode(digest.0))?;
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::UpdateSlot).ok_or(Error::Signature)?;
    let mut source = verify_fd(generation.read("duduclaw-updated")?, artifact.size_bytes, artifact.sha256)?;
    source.seek(SeekFrom::Start(0))?;
    let slots = install.child("slots")?;
    let destination = slots.child(slot_name(slot))?;
    match destination.remove("duduclaw-updated.next") {
        Ok(()) => (),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let mut target = destination.create("duduclaw-updated.next")?;
    super::files::copy_artifact(&mut source, &mut target, artifact)?;
    target.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    target.sync_all()?;
    drop(target);
    destination.rename("duduclaw-updated.next", &destination, "duduclaw-updated", true)?;
    destination.sync()?; slots.sync()?; install.sync()?;
    let receipt = SlotReceipt { manifest: String::from_utf8(manifest_bytes.to_vec()).map_err(|_| Error::Invalid)?,
        signature: signature.to_owned(), accepted_at_unix: accepted };
    Directory::open(Path::new(CONFIG_ROOT))?.atomic_json(&format!("slot-{}.json", slot_name(slot)), &receipt)?;
    verified_slot(slot, state, &root)?;
    crate::state::transaction(state, disk, |next| next.stage_trial(slot))?;
    Ok(slot)
}
