//! No Linux/root/keychain required: exercise the real account wrapping store.
use super::*;
use super::container_root::{ContainerRootStore, RefusedRootStore};
use super::tests::TempHome;

fn protection(home: &TempHome, root: u8) -> CredentialProtection {
    CredentialProtection::with_store(home.path(), Box::new(ContainerRootStore::new(home.path(), &[root; 32])))
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
    let store = ContainerRootStore::attested(home.path());
    let account = super::marker::account_for_home(home.path());
    // The harness has no launcher key. A production store must not retain a
    // synthetic or stale wrapping key or write a new usable blob.
    assert!(matches!(store.write_new(&account, &[7; 32]), Err(StoreError::Unavailable(_))));
    assert!(!blob(&home).exists());
}
