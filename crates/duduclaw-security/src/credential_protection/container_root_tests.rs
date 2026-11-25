//! Exercise the real account wrapping store on Unix, without a launcher/keychain.
use super::*;
use super::container_root::{ContainerRootStore, RefusedRootStore};
use super::tests::TempHome;

fn protection(home: &TempHome, root: u8) -> CredentialProtection {
    protection_at(home.path(), root)
}

fn protection_at(home: &Path, root: u8) -> CredentialProtection {
    let mut cp = CredentialProtection::with_store(home, Box::new(UnsupportedStore));
    cp.store = Box::new(ContainerRootStore::new(cp.trusted_home.clone(), &[root; 32]));
    cp
}

fn blob(home: &TempHome) -> PathBuf { home.path().join("protected/container-wrapped-key.json") }

#[test]
fn roundtrip_gate_and_rotation_preserve_app_bound_fingerprint_limit() {
    let home = TempHome::new("container-roundtrip");
    home.hold_gateway_lock();
    let cp = protection(&home, 7);
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    let enabled = cp.enable(false).unwrap();
    assert_eq!(enabled.backend, BackendKind::ContainerRootKey);
    assert_eq!(enabled.level, ProtectionLevel::ProcessIsolated);
    assert!(enabled.person_identity_allowed);
    let key = cp.person_identity_gate().unwrap();
    let sealed = key.seal(b"personal refresh token").unwrap();
    assert_eq!(key.unseal(&sealed).unwrap().as_slice(), b"personal refresh token");
    let provider_key = format!("sk-{}", "x".repeat(40));
    assert!(key.credential_fingerprint("api_key", provider_key.as_bytes()).is_none());
    assert_eq!(cp.load_key_for_credential_fingerprint().unwrap_err().code, "level_too_low");
    assert_eq!(cp.enable(false).unwrap().key_id, enabled.key_id);
    let raw = std::fs::read(blob(&home)).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["account"], cp.account());
    assert!(!raw.windows(32).any(|window| window == key.key.as_slice()));
    assert_eq!(std::fs::metadata(blob(&home)).unwrap().permissions().mode() & 0o777, 0o600);
    let reset = cp.enable(true).unwrap();
    assert_ne!(reset.key_id, enabled.key_id);
    let err = cp.person_identity_gate().unwrap().unseal(&sealed).unwrap_err();
    assert_eq!(err.code, "key_changed");
    assert!(err.message_zh.contains("儲存位置中的根金鑰"));
    assert!(!err.message_zh.contains("系統金鑰圈"));
    cp.disable().unwrap();
    assert!(!blob(&home).exists());
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    cp.disable().unwrap();
}

use std::os::unix::fs::PermissionsExt;

#[test]
fn wrong_root_missing_and_corrupt_blobs_fail_closed() {
    let home = TempHome::new("container-tamper");
    home.hold_gateway_lock();
    let cp = protection(&home, 7);
    cp.enable(false).unwrap();
    let valid = std::fs::read(blob(&home)).unwrap();
    assert_eq!(protection(&home, 8).status().state, ProtectionState::KeyChanged);
    assert_eq!(protection(&home, 8).person_identity_gate().unwrap_err().code, "key_changed");
    let mut corrupt: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    corrupt["ciphertext"] = serde_json::json!(BASE64.encode([0; 60]));
    let mut version: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    version["version"] = serde_json::json!(2);
    let mut account: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    account["account"] = serde_json::json!("another-account");
    for bad in [Vec::new(), valid.iter().take(20).copied().collect(), vec![b'x'; 4097],
        serde_json::to_vec(&corrupt).unwrap(), serde_json::to_vec(&version).unwrap(), serde_json::to_vec(&account).unwrap()] {
        std::fs::write(blob(&home), bad).unwrap();
        assert_eq!(cp.status().state, ProtectionState::KeyChanged);
        assert!(!cp.status().person_identity_allowed);
    }
    std::fs::remove_file(blob(&home)).unwrap();
    assert_eq!(cp.status().state, ProtectionState::Missing);
    let err = cp.person_identity_gate().unwrap_err();
    assert_eq!(err.code, "missing");
    assert!(!err.message_zh.contains("系統金鑰圈"));
    assert_eq!(cp.enable(true).unwrap().state, ProtectionState::Ready);
}

#[test]
fn blob_from_another_home_cannot_be_relabelled() {
    let a = TempHome::new("container-account-a");
    let b = TempHome::new("container-account-b");
    let first = protection(&a, 7);
    let second = protection(&b, 7);
    first.enable(false).unwrap();
    second.enable(false).unwrap();
    let raw = std::fs::read(blob(&a)).unwrap();
    std::fs::write(blob(&b), &raw).unwrap();
    assert_eq!(second.status().state, ProtectionState::KeyChanged);
    let mut relabelled: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    relabelled["account"] = serde_json::json!(second.account());
    std::fs::write(blob(&b), serde_json::to_vec(&relabelled).unwrap()).unwrap();
    assert_eq!(second.status().state, ProtectionState::KeyChanged);
}

#[test]
fn store_never_follows_symlinks_or_overwrites_a_link_target() {
    use std::os::unix::fs::symlink;
    let home = TempHome::new("container-links");
    let cp = protection(&home, 7);
    cp.enable(false).unwrap();
    let target = home.path().join("unrelated");
    std::fs::write(&target, b"untouched").unwrap();
    std::fs::remove_file(blob(&home)).unwrap();
    symlink(&target, blob(&home)).unwrap();
    assert_eq!(cp.status().state, ProtectionState::KeyChanged);
    cp.enable(true).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    assert!(!std::fs::symlink_metadata(blob(&home)).unwrap().file_type().is_symlink());
    cp.disable().unwrap();
    std::fs::remove_dir(home.path().join("protected")).unwrap();
    let other = TempHome::new("container-directory-target");
    symlink(other.path(), home.path().join("protected")).unwrap();
    assert!(cp.enable(true).is_err());
    assert!(std::fs::read_dir(other.path()).unwrap().next().is_none());
}

#[test]
fn refused_startup_is_unavailable_even_without_a_marker() {
    let home = TempHome::new("container-refused");
    home.hold_gateway_lock();
    let cp = CredentialProtection::with_store(home.path(), Box::new(RefusedRootStore("docker_socket")));
    let s = cp.status();
    assert_eq!(s.state, ProtectionState::Unavailable);
    assert_eq!(s.backend, BackendKind::ContainerRootKey);
    assert_eq!(s.refusal_reason, Some("docker_socket"));
    assert!(!s.person_identity_allowed);
    assert!(s.detail_zh.contains("Docker 控制介面"));
    assert!(!s.detail_zh.contains("docker_socket"));
    assert_eq!(cp.person_identity_gate().unwrap_err().code, "unavailable");
    assert_eq!(cp.enable(false).unwrap_err().code, "unavailable");
    assert!(cp.enable(true).is_err());
    assert!(RefusedRootStore("docker_socket").delete("unused").is_ok());
}

#[test]
fn attested_store_has_no_wrapping_key_fallback() {
    let home = TempHome::new("attested-no-key");
    let cp = protection(&home, 7);
    let store = ContainerRootStore::attested(cp.trusted_home.clone());
    let account = cp.account();
    // The harness has no launcher key. A production store must not retain a
    // synthetic or stale wrapping key or write a new usable blob.
    assert!(matches!(store.write_new(&account, &[7; 32]), Err(StoreError::Unavailable(_))));
    assert!(!blob(&home).exists());
}

/// Retarget exactly after load_verified's home check, before the real store opens.
struct RetargetOnRead {
    store: ContainerRootStore,
    alias: PathBuf,
    target: PathBuf,
}

impl KeyStore for RetargetOnRead {
    fn backend(&self) -> BackendKind { self.store.backend() }
    fn level(&self) -> ProtectionLevel { self.store.level() }
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        std::fs::remove_file(&self.alias).unwrap();
        std::os::unix::fs::symlink(&self.target, &self.alias).unwrap();
        self.store.read(account)
    }
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        self.store.write_new(account, key)
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.store.delete(account)
    }
}

#[test]
fn real_store_refuses_retarget_between_verification_and_key_access() {
    use std::os::unix::fs::symlink;
    for person_gate in [false, true] {
        let parent = TempHome::new("container-check-race");
        let first = TempHome::new("container-check-first");
        let second = TempHome::new("container-check-second");
        let alias = parent.path().join("alias");
        symlink(first.path(), &alias).unwrap();
        let mut cp = protection_at(&alias, 7);
        cp.enable(false).unwrap();
        first.hold_gateway_lock();
        let account = cp.account();
        let original = std::fs::read(blob(&first)).unwrap();
        // Even a valid copy for A must never be opened through the redirected B.
        std::fs::create_dir(second.path().join("protected")).unwrap();
        std::fs::write(blob(&second), &original).unwrap();
        cp.store = Box::new(RetargetOnRead {
            store: ContainerRootStore::new(cp.trusted_home.clone(), &[7; 32]),
            alias,
            target: second.path().to_owned(),
        });
        if person_gate {
            assert!(cp.person_identity_gate().is_err());
        } else {
            assert_eq!(cp.status().state, ProtectionState::KeyChanged);
        }
        assert_eq!(cp.account(), account);
        assert!(cp.store.write_new(&account, &[8; 32]).is_err());
        assert!(cp.store.delete(&account).is_err());
        assert_eq!(std::fs::read(blob(&first)).unwrap(), original);
        assert_eq!(std::fs::read(blob(&second)).unwrap(), original);
        assert!(!second.path().join(".google-authority").exists());
        assert!(!second.path().join("locks").exists());
    }
}

#[test]
fn real_store_keeps_io_on_pinned_directory_after_last_path_check() {
    use std::os::unix::fs::symlink;
    for operation in ["read", "write", "delete"] {
        let parent = TempHome::new("container-open-race");
        let first = TempHome::new("container-open-first");
        let second = TempHome::new("container-open-second");
        let alias = parent.path().join("alias");
        symlink(first.path(), &alias).unwrap();
        let cp = protection_at(&alias, 7);
        cp.enable(false).unwrap();
        let account = cp.account();
        let original = std::fs::read(blob(&first)).unwrap();
        std::fs::create_dir(second.path().join("protected")).unwrap();
        std::fs::write(blob(&second), &original).unwrap();
        let target = second.path().to_owned();
        let link = alias.clone();
        // Run after the protected directory is opened, immediately before key IO.
        ContainerRootStore::after_directory_open(move || {
            std::fs::remove_file(&link).unwrap();
            symlink(&target, &link).unwrap();
        });
        match operation {
            "read" => {
                assert!(cp.store.read(&account).is_err());
                assert_eq!(std::fs::read(blob(&first)).unwrap(), original);
            }
            "write" => {
                cp.store.write_new(&account, &[8; 32]).unwrap();
                assert_ne!(std::fs::read(blob(&first)).unwrap(), original);
            }
            "delete" => {
                cp.store.delete(&account).unwrap();
                assert!(!blob(&first).exists());
            }
            _ => unreachable!(),
        }
        assert_eq!(std::fs::read_link(&alias).unwrap(), second.path());
        assert_eq!(std::fs::read(blob(&second)).unwrap(), original);
        assert_eq!(cp.account(), account);
        assert!(cp.check_home().is_err());
    }
}

fn assert_missing_home_bootstrap(home: &Path, resolved: &Path) {
    let cp = protection_at(home, 7);
    let shared_pin = cp.trusted_home.clone();
    assert!(shared_pin.get().is_none());
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    assert!(shared_pin.get().is_none());
    assert!(!resolved.exists());
    // A store cannot use an account derived from the unresolved spelling.
    assert!(cp.store.write_new(&cp.account(), &[9; 32]).is_err());
    assert!(!resolved.exists());
    duduclaw_core::config::create_home_dir(home).unwrap();
    cp.enable(false).unwrap();
    let pin = shared_pin.get().unwrap().as_ref().unwrap();
    assert!(std::ptr::eq(pin, cp.trusted_home().unwrap()));
    assert_eq!(pin.path(), std::fs::canonicalize(resolved).unwrap());
    let account = marker::account_for_resolved_home(pin.path());
    assert_eq!(cp.account(), account);
    assert_eq!(cp.read_marker().unwrap().account, account);
    let raw = std::fs::read(resolved.join("protected/container-wrapped-key.json")).unwrap();
    let wrapped: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(wrapped["account"], account);
    let bytes = cp.store.read(&account).unwrap();
    assert_eq!(cp.status().state, ProtectionState::Ready);
    duduclaw_core::gateway_instance::acquire(home).unwrap();
    assert_eq!(cp.person_identity_gate().unwrap().key.as_slice(), bytes.as_slice());
    // Reopening by the canonical spelling must derive the identical wrapping key.
    let canonical = protection_at(pin.path(), 7);
    assert_eq!(canonical.account(), account);
    assert_eq!(canonical.store.read(&account).unwrap().as_slice(), bytes.as_slice());
    cp.disable().unwrap();
    assert!(!resolved.join("protected/container-wrapped-key.json").exists());
}

#[test]
fn real_store_bootstraps_missing_home_from_relative_path() {
    let parent = TempHome::new("container-relative-bootstrap");
    let cwd = std::env::current_dir().unwrap();
    let mut home = PathBuf::new();
    for _ in cwd.ancestors().skip(1) { home.push(".."); }
    home.push(parent.path().strip_prefix("/").unwrap());
    home.push("missing/../new-home");
    assert!(home.is_relative());
    assert_missing_home_bootstrap(&home, &parent.path().join("new-home"));
}

#[test]
fn real_store_bootstraps_missing_home_under_symlinked_parent() {
    let parent = TempHome::new("container-symlink-bootstrap");
    let real = parent.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = parent.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let home = alias.join("missing/../new-home");
    assert_missing_home_bootstrap(&home, &real.join("new-home"));
}
