//! macOS integration tests on a throwaway keychain (never the login keychain).
//!
//! `SecKeychainCreate` on a temp path does not add the keychain to the user's
//! search list (design §2.3 E1), and every lookup here names that keychain
//! explicitly. The "employee" is a child process: Apple's `/usr/bin/python3`
//! calling the Security framework through ctypes with an empty environment and
//! keychain user interaction switched off — so no dialog can appear on the
//! person's screen, and a refusal shows up as an OSStatus.

use std::path::PathBuf;
use std::process::Command;

use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};

use super::marker::{self, SERVICE};
use super::tests::{TempHome, hex};
use super::*;

/// ctypes probe run as the "employee". argv: keychain path, service, account,
/// mode (`read` or `overwrite`). Prints `find=<status>` (attributes only),
/// then `read=<status> len=<n> hex=<value>` or `overwrite=<status>`.
const PROBE: &str = r#"
import ctypes, sys
sec = ctypes.CDLL('/System/Library/Frameworks/Security.framework/Security')
path, svc, acct, mode = sys.argv[1].encode(), sys.argv[2].encode(), sys.argv[3].encode(), sys.argv[4]
sec.SecKeychainSetUserInteractionAllowed(ctypes.c_ubyte(0))
kc = ctypes.c_void_p()
sec.SecKeychainOpen(path, ctypes.byref(kc))
item = ctypes.c_void_p()
print("find=%d" % sec.SecKeychainFindGenericPassword(kc, len(svc), svc, len(acct), acct, None, None, ctypes.byref(item)))
if mode == "read":
    n = ctypes.c_uint32(); data = ctypes.c_void_p(); it2 = ctypes.c_void_p()
    st = sec.SecKeychainFindGenericPassword(kc, len(svc), svc, len(acct), acct, ctypes.byref(n), ctypes.byref(data), ctypes.byref(it2))
    val = ctypes.string_at(data, n.value).hex() if st == 0 else ""
    print("read=%d len=%d hex=%s" % (st, n.value, val))
else:
    new = bytes([0x41] * 32)
    print("overwrite=%d" % sec.SecKeychainItemModifyAttributesAndData(item, None, len(new), new))
"#;

struct TempKeychain {
    dir: TempHome,
    path: PathBuf,
}

impl TempKeychain {
    fn new() -> Self {
        let dir = TempHome::new("keychain");
        let path = dir.path().join("protected-test.keychain-db");
        let password = hex(&CryptoEngine::generate_key().unwrap());
        let mut keychain: SecKeychain = CreateOptions::new()
            .password(&password)
            .prompt_user(false)
            .create(&path)
            .expect("create throwaway keychain");
        keychain.unlock(Some(&password)).expect("unlock throwaway keychain");
        Self { dir, path }
    }
}

fn employee_probe(kc: &TempKeychain, home: &TempHome, account: &str, mode: &str) -> String {
    let out = Command::new("/usr/bin/python3")
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home.path())
        .arg("-I")
        .arg("-c")
        .arg(PROBE)
        .arg(&kc.path)
        .arg(SERVICE)
        .arg(account)
        .arg(mode)
        .output()
        .expect("/usr/bin/python3 must be runnable for this test");
    assert!(out.status.success(), "probe failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn shell_dump(home: &TempHome) -> String {
    let out = Command::new("/bin/sh")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("-c")
        .arg("cat \"$0\"/* \"$0\"/.[!.]* 2>/dev/null; true")
        .arg(home.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn employee_child_process_cannot_read_the_protected_key() {
    let kc = TempKeychain::new();
    let home = TempHome::new("mac-read");
    home.hold_gateway_lock();
    let cp = CredentialProtection::with_store(home.path(), Box::new(MacKeychainStore::at_path(kc.path.clone())));

    let status = cp.enable(false).expect("enable on the throwaway keychain");
    assert_eq!(status.state, ProtectionState::Ready);
    // A cargo test binary is ad-hoc signed without hardened runtime, so code
    // could be injected into it (design §2.3 E9): the level must say so.
    assert!(!super::macos::hardened_runtime());
    assert_eq!(status.level, ProtectionLevel::UserStore);

    let account = marker::account_for_home(home.path());
    let key_hex = hex(&MacKeychainStore::at_path(kc.path.clone()).read(&account).unwrap());
    let key = cp.person_identity_gate().unwrap();
    let sealed = key.seal(b"refresh-token").unwrap();
    std::fs::write(home.path().join("store.json"), &sealed).unwrap();

    let out = employee_probe(&kc, &home, &account, "read");
    assert!(out.contains("find=0"), "the item's existence is visible: {out}");
    assert!(out.contains("read=-25293"), "decrypt must be refused (errSecAuthFailed): {out}");
    assert!(!out.contains(&key_hex), "key leaked to the child: {out}");

    let files = shell_dump(&home);
    assert!(files.contains("pk:v1:"), "the shell does see the sealed value");
    assert!(!files.contains(&key_hex), "no file in the home holds the key");

    // The gateway (trusted executable) still reads it silently.
    assert_eq!(&key.unseal(&sealed).unwrap()[..], b"refresh-token");
    drop(kc.dir);
}

#[test]
fn employee_overwriting_the_key_is_detected() {
    let kc = TempKeychain::new();
    let home = TempHome::new("mac-tamper");
    home.hold_gateway_lock();
    let cp = CredentialProtection::with_store(home.path(), Box::new(MacKeychainStore::at_path(kc.path.clone())));
    cp.enable(false).unwrap();
    let account = marker::account_for_home(home.path());

    // macOS lets any same-user program overwrite the item (design §2.3 E7).
    let out = employee_probe(&kc, &home, &account, "overwrite");
    assert!(out.contains("overwrite=0"), "overwrite is not ACL-protected on macOS: {out}");

    let s = cp.status();
    assert_eq!(s.state, ProtectionState::KeyChanged);
    assert!(!s.person_identity_allowed);
    assert_eq!(cp.person_identity_gate().unwrap_err().code, "key_changed");

    // Re-enabling replaces the planted item (it is deleted, never adopted).
    let fresh = cp.enable(true).unwrap();
    assert_eq!(fresh.state, ProtectionState::Ready);
    let out = employee_probe(&kc, &home, &account, "read");
    assert!(out.contains("read=-25293"), "the re-created item trusts only us again: {out}");
    drop(kc.dir);
}

#[test]
fn replacing_key_and_marker_then_restarting_is_caught_by_old_sealed_values() {
    let kc = TempKeychain::new();
    let home = TempHome::new("mac-swap");
    home.hold_gateway_lock();
    let cp = CredentialProtection::with_store(home.path(), Box::new(MacKeychainStore::at_path(kc.path.clone())));
    cp.enable(false).unwrap();
    let account = marker::account_for_home(home.path());
    let sealed = cp.person_identity_gate().unwrap().seal(b"token").unwrap();

    // The employee overwrites the item with a key it knows (0x41 * 32) and
    // rewrites the marker to match; then the gateway restarts.
    let out = employee_probe(&kc, &home, &account, "overwrite");
    assert!(out.contains("overwrite=0"), "{out}");
    let planted = [0x41u8; 32];
    let mut m = match marker::read_marker(home.path()) {
        marker::MarkerRead::Present(m) => m,
        other => panic!("{other:?}"),
    };
    m.key_id = marker::key_id(&planted);
    m.canary = marker::make_canary(&CryptoEngine::new(&planted).unwrap()).unwrap();
    marker::write_marker(home.path(), &m).unwrap();
    super::pin_reset(&account);

    assert_eq!(cp.status().state, ProtectionState::Ready);
    let key = cp.person_identity_gate().unwrap();
    assert_eq!(key.unseal(&sealed).unwrap_err().code, "key_changed");
    // Recovery: reset rotates to a fresh key the employee does not know.
    let fresh = cp.enable(true).unwrap();
    assert_ne!(fresh.key_id.as_deref(), Some(marker::key_id(&planted).as_str()));
    let out = employee_probe(&kc, &home, &account, "read");
    assert!(out.contains("read=-25293"), "{out}");
    drop(kc.dir);
}

#[test]
fn disable_deletes_the_keychain_item() {
    let kc = TempKeychain::new();
    let home = TempHome::new("mac-disable");
    let store = MacKeychainStore::at_path(kc.path.clone());
    let cp = CredentialProtection::with_store(home.path(), Box::new(MacKeychainStore::at_path(kc.path.clone())));
    cp.enable(false).unwrap();
    let account = marker::account_for_home(home.path());
    assert!(store.read(&account).is_ok());
    cp.disable().unwrap();
    assert_eq!(store.read(&account).unwrap_err(), StoreError::NotFound);
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    drop(kc.dir);
}
