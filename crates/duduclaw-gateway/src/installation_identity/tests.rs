use super::*;

#[test]
fn sas_is_first_16_hex_of_sha256_grouped_4_4_4_4() {
    let key = key_from_seed_for_test([0x42; 32]);
    let digest = hex::encode(Sha256::digest(key.public_key_bytes()));
    let expected: String = digest.chars().take(16).collect();
    let sas = key.fingerprint();
    assert_eq!(sas.len(), 19);
    assert_eq!(sas.replace('-', ""), expected);
    assert_eq!(sas.split('-').map(str::len).collect::<Vec<_>>(), [4, 4, 4, 4]);
    assert!(sas.bytes().all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(sas_for_hex(&key.public_key_hex()).as_deref(), Some(sas.as_str()));
    for bad in [String::new(), "AB".into(), "AB".repeat(32), "ab".repeat(31)] {
        assert_eq!(sas_for_hex(&bad), None);
    }
}

#[test]
fn public_key_is_64_lowercase_hex_and_deterministic_from_seed() {
    let a = key_from_seed_for_test([0x42; 32]);
    let b = key_from_seed_for_test([0x42; 32]);
    assert_eq!(a.public_key_hex(), b.public_key_hex());
    assert!(duduclaw_license::is_valid_installation_pubkey(&a.public_key_hex()));
    assert_ne!(a.public_key_hex(), key_from_seed_for_test([0x43; 32]).public_key_hex());
    // Ed25519 is deterministic: same seed and message, same signature.
    assert_eq!(a.sign(b"message"), b.sign(b"message"));
}

#[test]
fn relay_key_gate_requires_a_bound_and_matching_local_key() {
    let local = || Ok(Some(key_from_seed_for_test([0x42; 32])));
    let pubkey = key_from_seed_for_test([0x42; 32]).public_key_hex();
    assert_eq!(relay_key_status(None, local()).err(), Some("relay_key_unbound"));
    assert_eq!(relay_key_status(None, Ok(None)).err(), Some("relay_key_unbound"));
    assert_eq!(relay_key_status(Some(&"ab".repeat(32)), local()).err(), Some("relay_key_mismatch"));
    assert_eq!(relay_key_status(Some(&pubkey), Ok(None)).err(), Some("relay_key_mismatch"));
    assert_eq!(relay_key_status(Some(&pubkey), Err(io::ErrorKind::PermissionDenied.into())).err(),
        Some("relay_key_mismatch"));
    assert_eq!(relay_key_status(Some(&pubkey), local()).unwrap().public_key_hex(), pubkey);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::{ffi::OsStrExt, fs::{MetadataExt, PermissionsExt, symlink}};

    fn key_path(home: &Path) -> std::path::PathBuf { home.join(STATE_DIR).join(INSTALLATION_KEY_FILE) }

    fn private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn generates_once_privately_and_reloads_the_same_key() {
        let home = tempfile::tempdir().unwrap();
        assert!(load(home.path()).unwrap().is_none());
        assert!(!key_path(home.path()).exists(), "load never generates");
        let (first, created) = load_or_create(home.path()).unwrap();
        assert!(created);
        let meta = std::fs::metadata(key_path(home.path())).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o600);
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        assert_eq!(meta.nlink(), 1);
        assert_eq!(meta.len(), 32);
        assert_eq!(std::fs::metadata(home.path().join(STATE_DIR)).unwrap().mode() & 0o7777, 0o700);
        let (second, created) = load_or_create(home.path()).unwrap();
        assert!(!created);
        assert_eq!(second.public_key_hex(), first.public_key_hex());
        assert_eq!(load(home.path()).unwrap().unwrap().public_key_hex(), first.public_key_hex());
        assert_eq!(fingerprint(home.path()), Some(first.fingerprint()));
        assert_eq!(ensure_public_key_hex(home.path()), Some(first.public_key_hex()));
    }

    /// Every non-NotFound failure is an error and leaves the file untouched.
    fn assert_fails_closed_without_regenerating(home: &Path, inspect: &Path) {
        // Never open a FIFO for a blocking read here.
        let regular = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        let before = std::fs::symlink_metadata(inspect).ok().map(|m| (m.ino(), m.len()));
        let contents = regular(inspect).then(|| std::fs::read(inspect).unwrap());
        assert!(load(home).is_err());
        assert!(load_or_create(home).is_err());
        assert!(ensure_public_key_hex(home).is_none());
        assert_eq!(fingerprint(home), None);
        assert_eq!(std::fs::symlink_metadata(inspect).ok().map(|m| (m.ino(), m.len())), before);
        assert_eq!(regular(inspect).then(|| std::fs::read(inspect).unwrap()), contents);
    }

    #[test]
    fn unsafe_or_malformed_key_files_are_never_regenerated() {
        for kind in ["public", "group", "hardlink", "short", "long", "empty", "fifo", "directory", "symlink", "dangling"] {
            let home = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            drop(StateDir::open(home.path()).unwrap());
            let path = key_path(home.path());
            let mut inspect = path.clone();
            match kind {
                "public" | "group" => {
                    private_file(&path, &[1; 32]);
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(if kind == "public" { 0o604 } else { 0o640 })).unwrap();
                },
                "hardlink" => {
                    private_file(&path, &[1; 32]);
                    std::fs::hard_link(&path, home.path().join("linked.key")).unwrap();
                },
                "short" => private_file(&path, &[1; 31]),
                "long" => private_file(&path, &[1; 33]),
                "empty" => private_file(&path, b""),
                "fifo" => {
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                },
                "directory" => std::fs::create_dir(&path).unwrap(),
                "symlink" => {
                    inspect = outside.path().join("victim.key");
                    private_file(&inspect, &[9; 32]);
                    symlink(&inspect, &path).unwrap();
                },
                "dangling" => {
                    inspect = outside.path().join("absent.key");
                    symlink(&inspect, &path).unwrap();
                },
                _ => unreachable!(),
            }
            assert_fails_closed_without_regenerating(home.path(), &inspect);
            if kind == "dangling" { assert!(!inspect.exists(), "O_EXCL never follows a dangling link"); }
        }
    }

    #[test]
    fn symlinked_or_writable_state_directories_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), home.path().join(STATE_DIR)).unwrap();
        assert!(load_or_create(home.path()).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(STATE_DIR)).unwrap();
        std::fs::set_permissions(home.path().join(STATE_DIR), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_or_create(home.path()).is_err());
        assert!(!key_path(home.path()).exists());

        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        std::fs::create_dir(&actual).unwrap();
        symlink(&actual, root.path().join("alias")).unwrap();
        assert!(load_or_create(&root.path().join("alias")).is_err());
        assert!(!actual.join(STATE_DIR).exists());
    }

    #[test]
    fn unsafe_installation_lock_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        drop(StateDir::open(home.path()).unwrap());
        symlink(outside.path().join("lock"), home.path().join(STATE_DIR).join(INSTALLATION_LOCK_FILE)).unwrap();
        assert!(load_or_create(home.path()).is_err());
        assert!(!outside.path().join("lock").exists());
        assert!(!key_path(home.path()).exists());
    }

    #[test]
    fn concurrent_generation_yields_one_key() {
        let home = tempfile::tempdir().unwrap();
        let handles: Vec<_> = (0..8).map(|_| {
            let home = home.path().to_path_buf();
            std::thread::spawn(move || load_or_create(&home).map(|(key, created)| (key.public_key_hex(), created)))
        }).collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
        assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
        assert!(results.iter().all(|(key, _)| key == &results[0].0));
    }

    #[test]
    fn create_new_never_replaces_and_cleans_up_failed_writes() {
        let home = tempfile::tempdir().unwrap();
        let dir = StateDir::open(home.path()).unwrap();
        dir.create_new("example.key", b"first").unwrap();
        assert_eq!(dir.create_new("example.key", b"second").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(dir.read("example.key").unwrap().unwrap().as_slice(), b"first");
        assert!(dir.create_new("large.key", &vec![0; MAX_STATE_BYTES + 1]).is_err());
        assert!(dir.read("large.key").unwrap().is_none());
        for name in ["", ".", "..", "a/b", "a\\b"] {
            assert!(dir.create_new(name, b"x").is_err(), "{name:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extended_acl_on_the_key_is_refused_and_not_regenerated() {
        let home = tempfile::tempdir().unwrap();
        let (original, _) = load_or_create(home.path()).unwrap();
        let path = key_path(home.path());
        assert!(std::process::Command::new("/bin/chmod").args(["+a", "everyone allow read"]).arg(&path).status().unwrap().success());
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        assert!(load(home.path()).is_err());
        assert!(load_or_create(home.path()).is_err());
        assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(&path).status().unwrap().success());
        assert_eq!(load(home.path()).unwrap().unwrap().public_key_hex(), original.public_key_hex());
    }
}
