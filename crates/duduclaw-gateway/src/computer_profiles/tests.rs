use super::*;

fn current_blob(home: &Path) -> PathBuf {
    home.join(ROOT)
        .join("alice")
        .join(blob_name(state(home, "alice").unwrap().unwrap().generation))
}

fn fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    std::fs::write(home.path().join("agents/alice/agent.toml"),
        "[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\npersist_login=true\n").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[computer_use.profiles]\nenabled=true\n",
    )
    .unwrap();
    std::fs::write(home.path().join(".keyfile"), [42u8; 32]).unwrap();
    home
}

fn prepare(home: &Path, hosts: &[&str]) -> ProfileSession {
    ProfileSession::prepare(
        home,
        "alice",
        &hosts.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ProfilesConfig::active(home).unwrap(),
    )
    .unwrap()
}

#[test]
fn profile_defaults_and_invalid_config_are_off() {
    let home = tempfile::tempdir().unwrap();
    assert!(!ProfilesConfig::load(home.path()).unwrap().enabled);
    for text in [
        "[computer_use.profiles]\nenabled='true'",
        "[computer_use.profiles]\nunknown=true",
        "[computer_use.profiles]\nmax_bytes=999999999",
        "[computer_use.profiles]\nretention_days=-1",
    ] {
        std::fs::write(home.path().join("config.toml"), text).unwrap();
        assert!(ProfilesConfig::load(home.path()).is_err(), "{text}");
        assert!(ProfilesConfig::active(home.path()).is_none());
    }
}

#[test]
fn profile_external_stop_fence_keeps_the_previous_generation() {
    use std::cell::Cell;
    let home = fixture();
    let session = prepare(home.path(), &[]);
    session.save(b"good").unwrap();
    let before = std::fs::read(current_blob(home.path())).unwrap();
    let generation = state(home.path(), "alice").unwrap().unwrap().generation;
    let permitted = Cell::new(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert!(
        session
            .save_with_pointer_fault(b"unsafe", deadline, &|| permitted.get(), &|step| {
                if step == crate::fs_safe::WriteStep::Write {
                    permitted.set(false);
                }
                Ok(())
            })
            .is_err()
    );
    assert_eq!(
        state(home.path(), "alice").unwrap().unwrap().generation,
        generation
    );
    assert_eq!(std::fs::read(current_blob(home.path())).unwrap(), before);
}

#[test]
fn profile_roundtrip_is_encrypted_and_allowlist_narrowing_resets_it() {
    let home = fixture();
    let first = prepare(home.path(), &["a.example.com", "b.example.com"]);
    assert_eq!(first.restore().unwrap().1, "empty");
    first.save(b"secret-cookie-archive").unwrap();
    let sealed = std::fs::read(current_blob(home.path())).unwrap();
    assert!(!sealed.windows(13).any(|w| w == b"secret-cookie"));
    drop(first);
    let more = prepare(
        home.path(),
        &["b.example.com", "a.example.com", "c.example.com"],
    );
    assert_eq!(
        more.restore().unwrap().0.unwrap().as_slice(),
        b"secret-cookie-archive"
    );
    drop(more);
    let less = prepare(home.path(), &["a.example.com"]);
    assert_eq!(less.restore().unwrap().1, "reset_allowlist_narrowed");
    assert!(state(home.path(), "alice").unwrap().is_none());
}

#[test]
fn profile_replaced_agent_cannot_restore_predecessor_or_use_its_aad() {
    let home = fixture();
    let first = prepare(home.path(), &["a.example.com"]);
    first.save(b"cookies").unwrap();
    let sealed = std::fs::read(current_blob(home.path())).unwrap();
    let aad = first.aad();
    assert!(crate::config_crypto::open_bytes(home.path(), b"different-agent", &sealed).is_err());
    assert!(crate::config_crypto::open_bytes(home.path(), &aad, &sealed).is_ok());
    drop(first);
    std::fs::remove_file(home.path().join("agents/alice/state").join(IDENTITY)).unwrap();
    let second = prepare(home.path(), &["a.example.com"]);
    assert_ne!(second.aad(), aad);
    assert_eq!(second.restore().unwrap().1, "empty");
    assert!(crate::config_crypto::open_bytes(home.path(), &second.aad(), &sealed).is_err());
}

#[test]
fn profile_missing_key_or_oversize_never_replaces_saved_ciphertext() {
    let home = fixture();
    let mut session = prepare(home.path(), &[]);
    session.save(b"old-cookies").unwrap();
    let path = current_blob(home.path());
    let old = std::fs::read(&path).unwrap();
    session.config.max_bytes = 1;
    assert!(session.save(b"too-big").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old);
    session.config.max_bytes = 1024 * 1024;
    std::fs::remove_file(home.path().join(".keyfile")).unwrap();
    assert!(session.save(b"new-cookies").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old);
    assert!(!home.path().join(".keyfile").exists());
}

#[test]
fn profile_changed_owner_and_disabled_capability_refuse_save() {
    let home = fixture();
    let session = prepare(home.path(), &[]);
    session.save(b"old").unwrap();
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[capabilities]\ncomputer_use=false",
    )
    .unwrap();
    assert!(session.save(b"new").is_err());
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\npersist_login=true",
    )
    .unwrap();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    drop(db);
    assert!(!session.valid_owner().unwrap());
    assert!(session.save(b"new-owner").is_err());
    assert!(state(home.path(), "alice").unwrap().is_none());
}

#[test]
fn profile_one_writer_and_symlinked_state_are_refused() {
    let home = fixture();
    let first = prepare(home.path(), &[]);
    assert!(ProfileSession::prepare(home.path(), "alice", &[], ProfilesConfig::default()).is_err());
    assert!(reset(home.path(), "alice").is_err());
    drop(first);
    let path = home.path().join("agents/alice/state").join(IDENTITY);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(home.path().join(".keyfile"), path).unwrap();
    assert!(ProfileSession::prepare(home.path(), "alice", &[], ProfilesConfig::default()).is_err());
}

#[test]
fn failed_pointer_commit_and_expired_budget_keep_the_previous_snapshot() {
    let home = fixture();
    let session = prepare(home.path(), &[]);
    session.save(b"old-cookies").unwrap();
    let old = std::fs::read(current_blob(home.path())).unwrap();
    let old_generation = state(home.path(), "alice").unwrap().unwrap().generation;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert!(
        session
            .save_with_pointer_fault(b"new-cookies", deadline, &|| true, &|step| {
                if step == crate::fs_safe::WriteStep::Rename {
                    Err(io::Error::other("injected pointer failure"))
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    assert_eq!(
        state(home.path(), "alice").unwrap().unwrap().generation,
        old_generation
    );
    assert_eq!(std::fs::read(current_blob(home.path())).unwrap(), old);
    assert_eq!(
        session.restore().unwrap().0.unwrap().as_slice(),
        b"old-cookies"
    );
    assert!(
        session
            .save_before(b"late-cookies", std::time::Instant::now())
            .is_err()
    );
    std::fs::write(home.path().join("threat_level"), "RED").unwrap();
    assert!(session.save(b"unsafe-cookies").is_err());
    assert_eq!(std::fs::read(current_blob(home.path())).unwrap(), old);
}

#[test]
fn an_unobserved_suspension_cycle_invalidates_saved_logins() {
    use duduclaw_auth::{AccessLevel, UserRole, UserStatus};
    let home = fixture();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db
        .create_user(
            "owner@example.invalid",
            "Owner",
            "fixture-password",
            UserRole::Employee,
        )
        .unwrap();
    db.bind_agent(&user.id, "alice", AccessLevel::Owner)
        .unwrap();
    let session = prepare(home.path(), &[]);
    session.save(b"owner-cookies").unwrap();
    db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
    db.set_user_status(&user.id, UserStatus::Active).unwrap();
    assert!(!session.valid_owner().unwrap());
    drop(session);
    let replacement = prepare(home.path(), &[]);
    assert_eq!(replacement.restore().unwrap().1, "empty");
    assert!(state(home.path(), "alice").unwrap().is_none());
}
