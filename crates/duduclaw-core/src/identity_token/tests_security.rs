use super::*;

#[test]
fn deleted_key_never_disables_or_regenerates_identity() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
        (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
    let key = ensure_key(home.path()).unwrap();
    let token = mint_token(&key, "alice");
    assert!(home.path().join(IDENTITY_INITIALIZED_FILE).is_file());
    std::fs::remove_file(identity_key_path(home.path())).unwrap();
    for require in [false, true] {
        assert_eq!(verify_claim(home.path(), "alice", &token, require), IdentityVerdict::Rejected);
    }
    assert!(ensure_key(home.path()).is_err());
    assert!(!identity_key_path(home.path()).exists());
}

#[test]
fn upgrade_records_existing_key_without_rotating_it() {
    let home = tempfile::tempdir().unwrap();
    let original = ensure_key(home.path()).unwrap();
    std::fs::remove_file(home.path().join(IDENTITY_INITIALIZED_FILE)).unwrap();
    assert_eq!(ensure_key(home.path()).unwrap(), original);
    assert!(initialized(home.path()).unwrap());
}

#[test]
fn interrupted_initialization_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    mark_initialized(home.path()).unwrap();
    assert!(ensure_key(home.path()).is_err());
    assert_eq!(verify_claim(home.path(), "alice", "", false), IdentityVerdict::Rejected);
}

#[test]
fn atomic_install_never_overwrites_and_cleans_temporary_file() {
    let home = tempfile::tempdir().unwrap();
    install_new(home.path(), "destination", b"original").unwrap();
    assert!(install_new(home.path(), "destination", b"replacement").is_err());
    assert_eq!(std::fs::read(home.path().join("destination")).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn preplanted_temp_and_lock_symlinks_cannot_overwrite_victim() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let victim = home.path().join("victim");
    std::fs::write(&victim, b"preserve").unwrap();
    symlink(&victim, home.path().join("identity.key.tmp")).unwrap();
    ensure_key(home.path()).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
    std::fs::remove_file(home.path().join(IDENTITY_LOCK_FILE)).unwrap();
    symlink(&victim, home.path().join(IDENTITY_LOCK_FILE)).unwrap();
    assert!(ensure_key(home.path()).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
}

#[cfg(unix)]
#[test]
fn private_file_is_0600_before_any_secret_is_written() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("new-secret");
    let file = open_private(&path, true, true).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn reserved_claims_are_rejected_by_shared_verifier_in_both_modes() {
    let home = tempfile::tempdir().unwrap();
    for require in [false, true] {
        assert_eq!(verify_claim(home.path(), "dashboard", "", require), IdentityVerdict::Rejected);
    }
    let key = ensure_key(home.path()).unwrap();
    for name in crate::SYSTEM_SENDERS.iter().chain(crate::RESERVED_QUEUE_SENDERS.iter())
        .copied().chain([UNTRUSTED_AGENT_ID, "__reserved", "../alice"])
    {
        let token = mint_token(&key, name);
        for require in [false, true] {
            assert_eq!(verify_claim(home.path(), name, &token, require), IdentityVerdict::Rejected, "{name}");
        }
    }
    assert_eq!(verify_claim(home.path(), "alice", &mint_token(&key, "alice"), true), IdentityVerdict::Verified);
}

#[test]
fn signed_doctor_probe_can_still_check_mcp_startup() {
    let home = tempfile::tempdir().unwrap();
    let key = ensure_key(home.path()).unwrap();
    assert_eq!(verify_claim(home.path(), "doctor-probe", &mint_token(&key, "doctor-probe"), true),
        IdentityVerdict::Verified);
    assert_eq!(verify_claim(home.path(), "doctor-probe", "garbage", true), IdentityVerdict::Rejected);
}

#[cfg(unix)]
#[test]
fn unreadable_identity_config_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, "[delegation]\nrequire_identity_token = false\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root can read mode 000, so use the deterministic directory fixture in
    // delegation_policy's test for privileged runners instead.
    if unsafe { libc::geteuid() } != 0 {
        assert!(crate::require_identity_token_from_home(home.path()).is_err());
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn ensure_key_process_child() {
    let Some(home) = std::env::var_os("DDC_IDENTITY_TEST_CHILD_HOME") else { return };
    let home = PathBuf::from(home);
    let key = ensure_key(&home).unwrap();
    std::fs::write(home.join(format!("result-{}", std::process::id())), mint_token(&key, "alice")).unwrap();
}

#[test]
fn concurrent_processes_share_one_initial_key() {
    let home = tempfile::tempdir().unwrap();
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "identity_token::tests_security::ensure_key_process_child", "--nocapture"])
            .env("DDC_IDENTITY_TEST_CHILD_HOME", home.path())
            .stdout(std::process::Stdio::null())
            .spawn().unwrap());
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let expected = mint_token(&read_key(home.path()).unwrap().unwrap(), "alice");
    let mut count = 0;
    for entry in std::fs::read_dir(home.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().starts_with("result-") {
            assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
            count += 1;
        }
    }
    assert_eq!(count, 8);
}

#[test]
fn grace_expiry_is_rechecked_even_with_cached_false() {
    let home = tempfile::tempdir().unwrap();
    let key = ensure_key(home.path()).unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
        (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
    assert_eq!(verify_claim(home.path(), "alice", "garbage", false), IdentityVerdict::Unverified);
    for deadline in [None, Some("invalid".to_string()),
        Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()),
        Some((chrono::Utc::now() + chrono::Duration::days(31)).to_rfc3339()),
        Some((chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())]
    {
        let extra = deadline.as_ref().map(|s| format!("identity_token_grace_until = {s:?}\n")).unwrap_or_default();
        std::fs::write(home.path().join("config.toml"), format!("[delegation]\nrequire_identity_token = false\n{extra}")).unwrap();
        // The first invalid deadline latches strict, including a later valid edit.
        assert_eq!(verify_claim(home.path(), "alice", "garbage", false), IdentityVerdict::Rejected);
        assert_eq!(verify_claim(home.path(), "alice", &mint_token(&key, "alice"), false), IdentityVerdict::Verified);
    }
    std::fs::write(home.path().join("config.toml"), "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = 42\n").unwrap();
    assert!(crate::require_identity_token_from_home(home.path()).unwrap());
    assert_eq!(verify_claim(home.path(), "alice", "", false), IdentityVerdict::Rejected);
}

#[cfg(unix)]
#[test]
fn unix_key_rejects_hardlinks_and_foreign_owner() {
    let home = tempfile::tempdir().unwrap();
    ensure_key(home.path()).unwrap();
    let key_path = identity_key_path(home.path());
    let alias = home.path().join("alias");
    std::fs::hard_link(&key_path, &alias).unwrap();
    assert!(read_key(home.path()).is_err());
    assert!(ensure_key(home.path()).is_err());
    std::fs::remove_file(alias).unwrap();
    assert!(read_key(home.path()).unwrap().is_some());
    let uid = unsafe { libc::geteuid() };
    assert!(validate_unix_owner_links(uid, 1, uid).is_ok());
    assert!(validate_unix_owner_links(uid.wrapping_add(1), 1, uid).is_err());
    // A real chown requires root; the pure predicate above runs for every user.
    if uid == 0 {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(key_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(path.as_ptr(), 1, !0) }, 0);
        assert!(read_key(home.path()).is_err());
        assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, !0) }, 0);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_key_rejects_any_extended_acl_entry() {
    let home = tempfile::tempdir().unwrap();
    ensure_key(home.path()).unwrap();
    let path = identity_key_path(home.path());
    assert!(std::process::Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"]).arg(&path).status().unwrap().success());
    let file = std::fs::File::open(&path).unwrap();
    assert!(reject_macos_acl(&file).unwrap_err().to_string().contains("extended ACL"));
    assert!(read_key(home.path()).is_err());
    assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(&path).status().unwrap().success());
    assert!(read_key(home.path()).unwrap().is_some());
}
