//! Filesystem regressions for the private, descriptor-relative relay store.
use super::*;
use std::os::unix::{ffi::OsStrExt, fs::{MetadataExt, PermissionsExt, symlink}};

fn credential(id: &str) -> Credential {
    Credential { credential_id: id.into(), secret: "ab".repeat(32) }
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn credential_roundtrip_is_private_and_replacement_is_atomic() {
    let home = tempfile::tempdir().unwrap();
    // Existing gateway homes may be readable, but must never be writable by others.
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(home.path()).unwrap();
    let state_path = home.path().join("state");
    assert_eq!(std::fs::metadata(&state_path).unwrap().mode() & 0o7777, 0o700);
    assert!(store.credential().unwrap().is_none());
    store.save_credential(&credential("first")).unwrap();
    let path = state_path.join(CREDENTIAL_FILE);
    let original = std::fs::File::open(&path).unwrap();
    let original_metadata = original.metadata().unwrap();
    assert_eq!(original_metadata.mode() & 0o7777, 0o600);
    assert_eq!(original_metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(original_metadata.nlink(), 1);
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), vec!["credential_id", "secret"]);
    let loaded = store.credential().unwrap().unwrap();
    assert_eq!(loaded.credential_id, "first");
    assert_eq!(loaded.key().unwrap(), vec![0xab; 32]);
    store.save_credential(&credential("replacement")).unwrap();
    let replacement_metadata = std::fs::metadata(&path).unwrap();
    assert_ne!(original_metadata.ino(), replacement_metadata.ino());
    assert_eq!(replacement_metadata.mode() & 0o7777, 0o600);
    assert_eq!(store.credential().unwrap().unwrap().credential_id, "replacement");
    // An already-open reader retains the complete old JSON after rename.
    let old: Credential = serde_json::from_reader(original).unwrap();
    assert_eq!(old.credential_id, "first");
    assert!(!std::fs::read_dir(&state_path).unwrap().any(|entry|
        entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn symlink_home_state_credential_and_lock_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let actual = root.path().join("actual");
    std::fs::create_dir(&actual).unwrap();
    let alias = root.path().join("alias");
    symlink(&actual, &alias).unwrap();
    assert!(Store::open(&alias).is_err());

    let home = tempfile::tempdir().unwrap();
    symlink(&actual, home.path().join("state")).unwrap();
    assert!(Store::open(home.path()).is_err());
    assert!(!actual.join("mfa-relay.lock").exists());

    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let outside = root.path().join("outside.json");
    let original = br#"{"credential_id":"outside","secret":"do-not-read-or-replace"}"#;
    private_file(&outside, original);
    symlink(&outside, home.path().join("state").join(CREDENTIAL_FILE)).unwrap();
    assert!(store.credential().is_err());
    assert!(store.save_credential(&credential("new")).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
    drop(store);

    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("state")).unwrap();
    symlink(&outside, home.path().join("state/mfa-relay.lock")).unwrap();
    assert!(Store::open(home.path()).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
}

#[test]
fn credential_hardlinks_nonprivate_modes_and_fifo_are_refused() {
    for kind in ["hardlink", "public", "fifo"] {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let path = home.path().join("state").join(CREDENTIAL_FILE);
        match kind {
            "hardlink" => {
                private_file(&path, b"{}");
                std::fs::hard_link(&path, home.path().join("linked.json")).unwrap();
            },
            "public" => {
                private_file(&path, b"{}");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            },
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            },
            _ => unreachable!(),
        }
        assert!(store.credential().is_err(), "{kind}");
        assert!(store.save_credential(&credential("new")).is_err(), "{kind}");
    }
}

#[test]
fn oversize_and_malformed_credentials_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let path = home.path().join("state").join(CREDENTIAL_FILE);
    for contents in [vec![b' '; 4097], b"not json".to_vec(),
        br#"{"credential_id":"id","secret":"s","unexpected":true}"#.to_vec()] {
        private_file(&path, &contents);
        assert!(store.credential().is_err());
    }
    let oversized = Credential { credential_id: "id".into(), secret: "x".repeat(4097) };
    assert!(store.save_credential(&oversized).is_err());
    assert!(!std::fs::read_dir(home.path().join("state")).unwrap().any(|entry|
        entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn writable_directories_and_unsafe_locks_are_refused() {
    for component in ["home", "state"] {
        let home = tempfile::tempdir().unwrap();
        let path = if component == "home" { home.path().to_path_buf() } else {
            let path = home.path().join("state");
            std::fs::create_dir(&path).unwrap();
            path
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Store::open(home.path()).is_err(), "{component}");
    }
    for kind in ["public", "hardlink", "fifo"] {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("state")).unwrap();
        let path = home.path().join("state/mfa-relay.lock");
        match kind {
            "public" => {
                private_file(&path, b"");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            },
            "hardlink" => {
                private_file(&path, b"");
                std::fs::hard_link(&path, home.path().join("linked.lock")).unwrap();
            },
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            },
            _ => unreachable!(),
        }
        assert!(Store::open(home.path()).is_err(), "{kind}");
    }
}

#[test]
fn pinned_state_directory_cannot_redirect_credential_writes() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    store.save_credential(&credential("before")).unwrap();
    let pinned = home.path().join("old-state");
    std::fs::rename(home.path().join("state"), &pinned).unwrap();
    symlink(outside.path(), home.path().join("state")).unwrap();
    store.save_credential(&credential("after")).unwrap();
    assert_eq!(store.credential().unwrap().unwrap().credential_id, "after");
    let saved: Credential = serde_json::from_slice(&std::fs::read(pinned.join(CREDENTIAL_FILE)).unwrap()).unwrap();
    assert_eq!(saved.credential_id, "after");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(Store::open(home.path()).is_err());
}

#[test]
fn concurrent_store_opens_contend_until_the_holder_drops() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    assert!(Store::open(home.path()).is_err());
    store.save_credential(&credential("held")).unwrap();
    drop(store);
    let reopened = Store::open(home.path()).unwrap();
    assert_eq!(reopened.credential().unwrap().unwrap().credential_id, "held");
}

#[test]
fn enrollment_budget_is_hourly_backward_safe_and_persisted_across_restart() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut state = store.state().unwrap();
    let installation_id = state.installation_id.clone();
    assert!(uuid::Uuid::parse_str(&installation_id).is_ok());
    assert_eq!(state.last_enrollment_attempt, None);
    state.reserve(10_000).unwrap();
    store.save_state(&state).unwrap();
    assert_eq!(std::fs::metadata(home.path().join("state").join(STATE_FILE)).unwrap().mode() & 0o7777, 0o600);
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let mut state = store.state().unwrap();
    assert_eq!(state.installation_id, installation_id);
    assert_eq!(state.last_enrollment_attempt, Some(10_000));
    for now in [i64::MIN, 9_999, 10_000, 13_599] {
        assert!(state.reserve(now).is_err());
        assert_eq!(state.last_enrollment_attempt, Some(10_000));
    }
    state.reserve(13_600).unwrap();
    store.save_state(&state).unwrap();
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let mut state = store.state().unwrap();
    assert!(state.reserve(13_600).is_err());
    assert_eq!(state.last_enrollment_attempt, Some(13_600));
    assert_eq!(state.installation_id, installation_id);
}

#[cfg(target_os = "macos")]
#[test]
fn extended_and_inherited_acls_are_refused_even_with_private_modes() {
    for component in ["home", "state", "credential"] {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        store.save_credential(&credential("private")).unwrap();
        let target = match component {
            "home" => home.path().to_path_buf(),
            "state" => home.path().join("state"),
            _ => home.path().join("state").join(CREDENTIAL_FILE),
        };
        let acl = if component == "credential" { "everyone allow read" }
            else { "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit" };
        assert!(std::process::Command::new("/bin/chmod").args(["+a", acl]).arg(&target).status().unwrap().success());
        if component == "credential" {
            assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o7777, 0o600);
            assert!(store.credential().is_err());
            assert!(store.save_credential(&credential("replacement")).is_err());
        } else {
            drop(store);
            assert!(Store::open(home.path()).is_err());
        }
        // Remove the test ACL so TempDir cleanup retains normal semantics.
        assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(&target).status().unwrap().success());
    }
}

#[test]
fn enrollment_lock_wait_is_bounded_and_allows_concurrent_login_to_finish() {
    use std::time::Duration;
    let home = tempfile::tempdir().unwrap();
    let holder = Store::open(home.path()).unwrap();
    assert_eq!(open_wait(home.path(), Duration::from_millis(40)).err().unwrap().kind(), std::io::ErrorKind::TimedOut);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        drop(holder);
    });
    let acquired = open_wait(home.path(), Duration::from_secs(2)).unwrap();
    release.join().unwrap();
    acquired.save_credential(&credential("next-login")).unwrap();
}
