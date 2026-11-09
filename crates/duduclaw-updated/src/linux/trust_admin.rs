//! Root-only fixed-inbox trust administration. Never reachable through update RPC.
use std::path::Path;
use duduclaw_isolation::{ids::Sha256Digest, update::{RootMetadata, ReleaseManifest}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{Error, Result, trust_store::{TrustLedger, DurableTrust, TrustWriter}, state::State};
use super::{files::Directory, CONFIG_ROOT, STATE_ROOT};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapBundle { root: String, signatures: Vec<String>, pin: Sha256Digest }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDocument { pub document: String, pub signatures: Vec<String> }
impl DurableTrust for Directory {
    fn save_trust(&mut self, next: &TrustLedger) -> Result<()> { self.atomic_json("trust-ledger.json", next) }
}
fn missing(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}
/// Caller holds STATE_ROOT's update.lock for all mutations. Reads use atomic snapshots.
pub(super) fn ledger() -> Result<TrustLedger> {
    let state = Directory::open(Path::new(STATE_ROOT))?;
    match state.json::<TrustLedger>("trust-ledger.json") {
        Ok(ledger) => { ledger.current(super::now()?)?; Ok(ledger) }
        Err(error) if missing(&error) => {
            let config = Directory::open(Path::new(CONFIG_ROOT))?;
            let bundle: BootstrapBundle = config.json("trust.json")?;
            let now = super::now()?;
            let mut ledger = TrustLedger::bootstrap(bundle.root.as_bytes(), &bundle.signatures, bundle.pin, now)?;
            // Preserve accepted old targets before rotation. Only state-bound slots
            // are authoritative; an unselected stale slot cannot invent history.
            let installed: State = state.json("state.json")?;
            installed.validate()?;
            let active_name = match installed.selector.active {
                duduclaw_isolation::update::UpdateSlot::A => "slot-a.json",
                duduclaw_isolation::update::UpdateSlot::B => "slot-b.json",
            };
            let receipt: super::launcher::SlotReceipt = config.json(active_name)?;
            let digest = Sha256Digest(Sha256::digest(receipt.manifest.as_bytes()).into());
            if digest != installed.effective_digest() && digest != installed.committed_digest
                && !installed.pending.as_ref().is_some_and(|p| p.digest == digest) { return Err(Error::Signature); }
            ledger.record_provisioned(receipt.manifest.as_bytes(), &receipt.signature, receipt.accepted_at_unix, now)?;
            // An invalid inactive/trial receipt must not prevent the launcher
            // from booting its valid active fallback. New trials are recorded in
            // the ledger before staging; bootstrap only imports the active slot.
            Ok(ledger)
        }
        Err(error) => Err(error),
    }
}
pub fn load_current() -> Result<RootMetadata> { ledger()?.current(super::now()?) }
/// Persist before staging/activation. Requires the daemon's update.lock.
pub fn record_release(bytes: &[u8], signature: &str) -> Result<()> {
    let mut ledger = ledger()?;
    let mut writer = TrustWriter::new(Directory::open(Path::new(STATE_ROOT))?);
    writer.transaction(&mut ledger, |next| next.record_release(bytes, signature, super::now()?).map(|_| ()))
}
pub fn verify_recorded(bytes: &[u8], signature: &str) -> Result<ReleaseManifest> {
    ledger()?.verify_installed(bytes, signature, super::now()?)
}
pub fn verify_installed(bytes: &[u8], signature: &str, accepted_at: u64) -> Result<ReleaseManifest> {
    let ledger = ledger()?;
    // The root-owned slot receipt remains independently time-bound, even when the
    // ledger first saw the same manifest in an earlier preparation attempt.
    let manifest = ledger.verify_installed(bytes, signature, super::now()?)?;
    if accepted_at < manifest.created_at_unix || accepted_at >= manifest.expires_at_unix || accepted_at > super::now()? {
        return Err(Error::Signature);
    }
    Ok(manifest)
}
/// Root puts the signed adjacent root document in CONFIG_ROOT/root-update.json.
/// Stop the daemon first; the same nonblocking update lock prevents races with Apply.
pub fn rotate_from_inbox() -> Result<()> {
    super::harden()?;
    let disk = Directory::open(Path::new(STATE_ROOT))?;
    let _lock = disk.lock()?;
    let installed: State = disk.json("state.json")?;
    installed.validate()?;
    if installed.pending.is_some() { return Err(Error::Busy); }
    let document: SignedDocument = Directory::open(Path::new(CONFIG_ROOT))?.json("root-update.json")?;
    let mut ledger = ledger()?;
    TrustWriter::new(disk).transaction(&mut ledger,
        |next| next.rotate(document.document.as_bytes(), &document.signatures, super::now()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust_store::tests::{setup, root_sign};
    use duduclaw_isolation::update::ROOT_SSHSIG_NAMESPACE;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    #[ignore = "Requires root on Linux with openat2; uses an isolated /run directory"]
    fn iso_root_trust_rotation_snapshot_reload_and_lock_exclusion() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("duduclaw-trust-test-").tempdir_in("/run").unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = Directory::open(temp.path()).unwrap();
        let lock = dir.lock().unwrap();
        assert!(Directory::open(temp.path()).unwrap().lock().is_err());
        let (mut ledger, mut root, keys, _) = setup();
        dir.atomic_json("trust-ledger.json", &ledger).unwrap();
        root.version = 2; root.frozen = true;
        let bytes = crate::metadata::canonical_bytes(&root).unwrap();
        let signatures = root_sign(&root, &keys, &bytes, ROOT_SSHSIG_NAMESPACE);
        let mut writer = TrustWriter::new(dir);
        writer.transaction(&mut ledger, |next| next.rotate(&bytes, &signatures, 200)).unwrap();
        let reloaded: TrustLedger = Directory::open(temp.path()).unwrap().json("trust-ledger.json").unwrap();
        assert_eq!(reloaded.current(201).unwrap(), root);
        // An unfinished temporary snapshot has no authority and must not follow a link.
        symlink("trust-ledger.json", temp.path().join("trust-ledger.json.next")).unwrap();
        writer.transaction(&mut ledger, |_| Ok(())).unwrap();
        assert_eq!(Directory::open(temp.path()).unwrap().json::<TrustLedger>("trust-ledger.json").unwrap().current(201).unwrap(), root);
        drop(lock);
        assert!(Directory::open(temp.path()).unwrap().lock().is_ok());
    }

    #[test]
    #[ignore = "Requires root on Linux with openat2; simulates failure after a durable rename"]
    fn iso_root_trust_post_rename_error_keeps_durable_rotation() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("duduclaw-trust-crash-test-").tempdir_in("/run").unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        struct FailAfterSave(Directory);
        impl DurableTrust for FailAfterSave {
            fn save_trust(&mut self, next: &TrustLedger) -> Result<()> {
                self.0.atomic_json("trust-ledger.json", next)?;
                Err(Error::Maintenance)
            }
        }
        let (mut ledger, mut root, keys, _) = setup();
        let dir = Directory::open(temp.path()).unwrap();
        let _lock = dir.lock().unwrap();
        dir.atomic_json("trust-ledger.json", &ledger).unwrap();
        let mut writer = TrustWriter::new(FailAfterSave(dir));
        root.version = 2; root.frozen = true;
        let bytes = crate::metadata::canonical_bytes(&root).unwrap();
        let signatures = root_sign(&root, &keys, &bytes, ROOT_SSHSIG_NAMESPACE);
        assert!(writer.transaction(&mut ledger, |next| next.rotate(&bytes, &signatures, 200)).is_err());
        assert_eq!(ledger.current(201).unwrap().version, 1);
        assert!(writer.transaction(&mut ledger, |_| Ok(())).is_err());
        assert_eq!(Directory::open(temp.path()).unwrap().json::<TrustLedger>("trust-ledger.json").unwrap().current(201).unwrap(), root);
    }
}
