use super::*;

fn database(sql: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(sql).unwrap();
    conn
}
#[test]
fn capture_deterministic_fingerprints_and_metadata() {
    let conn = database(catalog::TASKS_V182);
    let first = serde_json::to_vec(&inspect(&conn, Store::Tasks).unwrap()).unwrap();
    assert_eq!(first, serde_json::to_vec(&inspect(&conn, Store::Tasks).unwrap()).unwrap());
    let found = inspect(&conn, Store::Tasks).unwrap();
    assert!(found.exact_catalog);
    assert_eq!(found.status, "code_candidate");
    assert_eq!(found.schema_sha256.unwrap().len(), 64);
    conn.execute_batch("PRAGMA application_id=17; PRAGMA user_version=9;").unwrap();
    let found = inspect(&conn, Store::Tasks).unwrap();
    assert!(!found.exact_catalog);
    assert!(found.expected_candidate.is_none());
    assert_eq!((found.application_id, found.user_version), (Some(17), Some(9)));
}
#[test]
fn capture_bounded_differences_never_emit_sql_or_rows() {
    let conn = database(catalog::TASKS_V182);
    for index in 0..40 {
        conn.execute_batch(&format!("CREATE TABLE extra_{index}(value TEXT DEFAULT 'SECRET-CANARY');")).unwrap();
    }
    conn.execute_batch("CREATE TABLE \"/absolute/秘密\"(value TEXT); INSERT INTO extra_0 VALUES('ROW-SECRET');").unwrap();
    let found = inspect(&conn, Store::Tasks).unwrap();
    assert!(!found.exact_catalog);
    assert_eq!(found.differences.len(), MAX_DIFFERENCES);
    assert!(found.omitted_differences > 0);
    let text = serde_json::to_string(&found).unwrap();
    for secret in ["SECRET-CANARY", "ROW-SECRET", "/absolute/", "CREATE TABLE"] { assert!(!text.contains(secret)); }
}
#[test]
fn capture_schema_row_limit_refuses() {
    let conn = Connection::open_in_memory().unwrap();
    for index in 0..257 { conn.execute_batch(&format!("CREATE TABLE t{index}(x TEXT);")).unwrap(); }
    assert!(matches!(inspect(&conn, Store::Users), Err(Error::Limit)));
}
#[test]
fn capture_users_gap_and_old_catalog_do_not_authorise_v182() {
    let conn = database(include_str!("users-v1793.sql"));
    let found = inspect(&conn, Store::Users).unwrap();
    assert!(found.exact_catalog);
    assert_eq!(found.status, "catalog_only_not_v182_expected");
    assert!(found.expected_candidate.is_none());
}
#[cfg(unix)]
mod filesystem {
    use super::*;
    use std::fs;
    fn home() -> tempfile::TempDir {
        // Keep the operator's spelling, including macOS's symlinked /var.
        tempfile::tempdir_in(std::env::temp_dir()).unwrap()
    }
    fn seed(home: &Path, sql: &str) {
        let conn = Connection::open(home.join("tasks.db")).unwrap();
        conn.execute_batch(sql).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        conn.close().unwrap();
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!home.join(format!("tasks.db{suffix}")).exists());
        }
    }
    fn task(report: &Report) -> &StoreReport { report.stores.iter().find(|s| s.path == "tasks.db").unwrap() }
    #[test]
    fn capture_read_only_no_sidecars_and_deterministic_report() {
        for mode in ["WAL", "DELETE"] {
            let home = home(); seed(home.path(), catalog::TASKS_V182);
            let path = home.path().join("tasks.db");
            let conn = Connection::open(&path).unwrap();
            let actual: String = conn.query_row(&format!("PRAGMA journal_mode={mode}"), [], |r| r.get(0)).unwrap();
            assert!(actual.eq_ignore_ascii_case(mode));
            conn.close().unwrap();
            let before = fs::read(&path).unwrap();
            let permissions = fs::metadata(&path).unwrap().permissions();
            let mut readonly = permissions.clone(); readonly.set_readonly(true); fs::set_permissions(&path, readonly).unwrap();
            let first = capture(home.path(), "v1.82.0").unwrap();
            assert_eq!(task(&first).status, "code_candidate");
            assert_eq!(first.encode().unwrap(), capture(home.path(), "v1.82.0").unwrap().encode().unwrap());
            assert!(!first.release_matrix_verified);
            assert_eq!(before, fs::read(&path).unwrap());
            for suffix in ["-wal", "-shm", "-journal"] { assert!(!home.path().join(format!("tasks.db{suffix}")).exists()); }
            assert!(!String::from_utf8(first.encode().unwrap()).unwrap().contains(home.path().to_str().unwrap()));
            fs::set_permissions(path, permissions).unwrap();
        }
    }
    #[test]
    fn capture_accepts_symlink_ancestor_but_not_home() {
        use std::os::unix::fs::symlink;
        let parent = home();
        let real = parent.path().join("real");
        let link = parent.path().join("ancestor");
        fs::create_dir(&real).unwrap();
        fs::create_dir(real.join("home")).unwrap();
        seed(&real.join("home"), catalog::TASKS_V182);
        symlink(&real, &link).unwrap();
        assert_eq!(task(&capture(&link.join("home"), "v1.82.0").unwrap()).status, "code_candidate");
        for path in [link.clone(), link.join("."), std::path::PathBuf::from(format!("{}/", link.display()))] {
            assert!(capture(&path, "v1.82.0").is_err());
        }
    }
    #[test]
    fn immutable_uri_preserves_special_path_bytes_and_read_only_mode() {
        use std::os::unix::ffi::OsStrExt;
        let home = home();
        let path = home.path().join(std::ffi::OsStr::from_bytes(b"space ?#%&\xE7\xA7\x98.db"));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE canary(value TEXT);").unwrap();
        conn.close().unwrap();
        let mut conn = Connection::open_with_flags(immutable_uri(&path),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX).unwrap();
        assert!(conn.execute_batch("CREATE TABLE denied(value TEXT);").is_err());
        sqlite::harden(&mut conn).unwrap();
        assert_eq!(sqlite::schema(&conn).unwrap()[0].1, "canary");
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 1);
    }
    #[test]
    fn capture_refuses_symlink_store_home_and_sidecar() {
        use std::os::unix::fs::symlink;
        let home = home();
        let elsewhere = tempfile::tempdir().unwrap(); seed(elsewhere.path(), catalog::TASKS_V182);
        symlink(elsewhere.path().join("tasks.db"), home.path().join("tasks.db")).unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_open");
        symlink(elsewhere.path(), home.path().join("linked-home")).unwrap();
        assert!(capture(&home.path().join("linked-home"), "v1.82.0").is_err());
        fs::remove_file(home.path().join("tasks.db")).unwrap(); seed(home.path(), catalog::TASKS_V182);
        symlink(elsewhere.path().join("missing"), home.path().join("tasks.db-wal")).unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_sidecar");
    }
    #[test]
    fn capture_refuses_unreadable_nonregular_oversize_and_corrupt_store() {
        use std::os::unix::fs::PermissionsExt;
        let home = home(); let path = home.path().join("tasks.db");
        fs::create_dir(&path).unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_file_or_limit");
        fs::remove_dir(&path).unwrap();
        let file = fs::File::create(&path).unwrap(); file.set_len(crate::MAX_SOURCE_BYTES + 1).unwrap(); drop(file);
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_file_or_limit");
        fs::remove_file(&path).unwrap(); seed(home.path(), catalog::TASKS_V182);
        // The generic SQLite refusal must be caused by corruption, not by a
        // parser limit that would also reject the valid catalog on this OS.
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "code_candidate");
        fs::write(&path, b"SECRET invalid sqlite").unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_sqlite_or_changed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
        // Root can read mode-000 files: refusal is asserted only where the OS denies.
        if fs::File::open(&path).is_err() {
            assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_open");
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[test]
    fn capture_refuses_nonempty_wal_and_journal_without_changing_them() {
        for suffix in ["-wal", "-journal"] {
            let home = home(); seed(home.path(), catalog::TASKS_V182);
            assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "code_candidate");
            let path = home.path().join(format!("tasks.db{suffix}"));
            fs::write(&path, b"uncheckpointed").unwrap();
            assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_sidecar");
            assert_eq!(fs::read(path).unwrap(), b"uncheckpointed");
        }
    }
    #[test]
    fn capture_refuses_hardlinked_store_and_sidecar() {
        let home = home(); seed(home.path(), catalog::TASKS_V182);
        let alias = home.path().join("alias");
        fs::hard_link(home.path().join("tasks.db"), &alias).unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_hardlink");
        fs::remove_file(&alias).unwrap();
        let sidecar = home.path().join("tasks.db-shm");
        fs::write(&sidecar, []).unwrap();
        fs::hard_link(&sidecar, alias).unwrap();
        assert_eq!(task(&capture(home.path(), "v1.82.0").unwrap()).status, "refused_hardlink");
    }
    #[test]
    fn capture_detects_ctime_sidecar_and_parent_changes() {
        use std::os::unix::fs::PermissionsExt;
        for change in ["ctime", "sidecar_add", "sidecar_remove", "sidecar_replace", "store_replace", "parent"] {
            let parent = home();
            let path = parent.path().join("home"); fs::create_dir(&path).unwrap();
            seed(&path, catalog::TASKS_V182);
            if matches!(change, "sidecar_remove" | "sidecar_replace") { fs::write(path.join("tasks.db-shm"), []).unwrap(); }
            let report = capture_with_hook(&path, "v1.82.0", |store| {
                if store != Store::Tasks { return; }
                match change {
                    "ctime" => {
                        let main = path.join("tasks.db");
                        let before = identity(&fs::metadata(&main).unwrap());
                        fs::set_permissions(&main, fs::Permissions::from_mode(0o400)).unwrap();
                        let after = identity(&fs::metadata(main).unwrap());
                        assert_eq!((before.size, before.mtime, before.mtime_ns), (after.size, after.mtime, after.mtime_ns));
                        assert_ne!((before.ctime, before.ctime_ns), (after.ctime, after.ctime_ns));
                    },
                    "sidecar_add" => fs::write(path.join("tasks.db-shm"), []).unwrap(),
                    "sidecar_remove" => fs::remove_file(path.join("tasks.db-shm")).unwrap(),
                    "sidecar_replace" => {
                        fs::rename(path.join("tasks.db-shm"), path.join("old-shm")).unwrap();
                        fs::write(path.join("tasks.db-shm"), []).unwrap();
                    },
                    "store_replace" => {
                        fs::rename(path.join("tasks.db"), path.join("old-db")).unwrap();
                        fs::copy(path.join("old-db"), path.join("tasks.db")).unwrap();
                    },
                    "parent" => {
                        fs::rename(&path, parent.path().join("old-home")).unwrap();
                        fs::create_dir(&path).unwrap();
                    },
                    _ => unreachable!(),
                }
            }).unwrap();
            assert_eq!(task(&report).status, "refused_sqlite_or_changed", "{change}");
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn capture_noatime_errno_maps_to_distinct_refusal() {
        assert_eq!(open_refusal(rustix::io::Errno::PERM), "refused_noatime");
        assert_eq!(open_refusal(rustix::io::Errno::ACCESS), "refused_open");
        assert!(source_flags().contains(rustix::fs::OFlags::NOATIME));
    }
    #[test]
    fn capture_private_copy_ignores_tmpdir_and_cleans_up() {
        // Run this test in a child so TMPDIR does not race other Rust tests.
        const CHILD: &str = "DUDUCLAW_CAPTURE_COPY_TEST_HOME";
        if let Some(path) = std::env::var_os(CHILD) {
            use std::os::unix::fs::MetadataExt;
            let path = std::path::PathBuf::from(path);
            assert_eq!(std::env::var_os("TMPDIR").unwrap().as_os_str(), path.as_os_str());
            let listing = |dir: &Path| {
                let mut names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>();
                names.sort(); names
            };
            let before = listing(&path);
            #[cfg(target_os = "linux")]
            let temp_listings = [std::path::PathBuf::from("/tmp"), std::path::PathBuf::from("/var/tmp")]
                .into_iter().filter(|p| p.is_dir()).map(|dir| {
                    // Ignore unrelated concurrent system activity; capture's
                    // fixed-prefix artifacts must never appear in trusted roots.
                    let names = listing(&dir).into_iter().filter(|n| n.to_string_lossy().starts_with("duduclaw-capture-")).collect::<Vec<_>>();
                    (dir, names)
                }).collect::<Vec<_>>();
            let file = fs::File::open(path.join("tasks.db")).unwrap();
            let copy = PrivateCopy::new(&file, &path, file.metadata().unwrap().len()).unwrap();
            assert_eq!(listing(&path), before);
            #[cfg(target_os = "linux")]
            {
                use rustix::fs::{SealFlags, fcntl_get_seals};
                assert_eq!(copy.file.metadata().unwrap().nlink(), 0);
                let required = SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL;
                assert!(fcntl_get_seals(&copy.file).unwrap().contains(required));
                assert!(copy.file.set_len(0).is_err());
                assert!(std::os::unix::fs::FileExt::write_at(&copy.file, b"x", 0).is_err());
            }
            #[cfg(not(target_os = "linux"))]
            {
                let home = fs::canonicalize(&path).unwrap();
                let dir = copy.directory.path();
                assert!(!dir.starts_with(&home) && !home.starts_with(dir));
                assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
                assert_eq!(fs::metadata(copy.path()).unwrap().mode() & 0o777, 0o600);
                assert_eq!(dir.parent().unwrap(), fs::canonicalize("/tmp").unwrap());
            }
            let conn = copy.open().unwrap();
            assert!(inspect(&conn, Store::Tasks).unwrap().exact_catalog);
            assert!(conn.execute_batch("CREATE TABLE denied(x);").is_err());
            copy.close(conn).unwrap();
            #[cfg(target_os = "linux")]
            {
                assert_eq!(copy.copied_bytes, file.metadata().unwrap().len());
                assert!(memfd::verify(&copy.file, copy.copied_bytes + 1).is_err());
                let unsealed = fs::File::open(path.join("tasks.db")).unwrap();
                assert!(memfd::verify(&unsealed, copy.copied_bytes).is_err());
                copy.verify().unwrap();
            }
            let copy_path = copy.path();
            drop(copy);
            assert!(!copy_path.exists());
            assert_eq!(listing(&path), before);
            let report = capture_with_hook(&path, "v1.82.0", |_| {
                assert_eq!(listing(&path), before, "no copy may appear in TMPDIR/home even during capture");
                #[cfg(target_os = "linux")]
                for (dir, names) in &temp_listings {
                    assert_eq!(&listing(dir).into_iter().filter(|n| n.to_string_lossy().starts_with("duduclaw-capture-")).collect::<Vec<_>>(), names);
                }
            }).unwrap();
            assert_eq!(report.private_copy, COPY_MECHANISM);
            assert_eq!(listing(&path), before);
            return;
        }
        let home = home(); seed(home.path(), catalog::TASKS_V182);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("{}::capture_private_copy_ignores_tmpdir_and_cleans_up", module_path!().split_once("::").unwrap().1), "--nocapture"])
            .env(CHILD, home.path()).env("TMPDIR", home.path()).status().unwrap();
        assert!(status.success());
    }
    #[test]
    fn capture_missing_stores_and_excluded_creating_version() {
        let home = home();
        let report = capture(home.path(), "v1.80.0").unwrap();
        assert_eq!(report.stores.len(), 10);
        assert!(report.stores.iter().all(|s| s.status == "absent"));
        assert!(report.pending_captures.iter().any(|s| s == "users.db-v1.80.0-to-v1.82.x-NEEDS-RELEASE-CAPTURE"));
        assert!(matches!(capture(home.path(), "v1.79.3"), Err(Error::InvalidRequest)));
    }
}

#[test]
fn capture_report_size_limit_refuses_instead_of_truncating_json() {
    let mut store = StoreReport::absent(Store::Users);
    store.differences.push("x".repeat(MAX_REPORT_BYTES));
    let report = Report { format_version: 1, source_version: expected::SOURCE,
        migration_target: expected::TARGET, creating_version: "v1.82.0".into(),
        release_matrix_verified: false, stores: vec![store], pending_captures: Vec::new(), private_copy: COPY_MECHANISM };
    assert_eq!(report.encode(), Err(Error::Limit));
}

#[test]
fn capture_unknown_names_are_digest_only_and_deterministic() {
    let conn = database(catalog::TASKS_V182);
    let names = ["AWS_SECRET_distinct_token_482", "private_index_629", "private_trigger_739", "private_view_841"];
    conn.execute_batch("CREATE TABLE AWS_SECRET_distinct_token_482(x TEXT);
        CREATE INDEX private_index_629 ON AWS_SECRET_distinct_token_482(x);
        CREATE TRIGGER private_trigger_739 AFTER INSERT ON AWS_SECRET_distinct_token_482 BEGIN SELECT 1; END;
        CREATE VIEW private_view_841 AS SELECT x FROM AWS_SECRET_distinct_token_482;").unwrap();
    let first = inspect(&conn, Store::Tasks).unwrap();
    let text = serde_json::to_string(&first).unwrap();
    assert_eq!(text, serde_json::to_string(&inspect(&conn, Store::Tasks).unwrap()).unwrap());
    for name in names {
        assert!(!text.contains(name));
        assert!(text.contains(&catalog::name_digest(name)));
    }
    for (ordinal, kind) in ["index", "table", "trigger", "view"].iter().enumerate() {
        assert!(text.contains(&format!("unexpected {kind} ordinal={} name_sha256=", ordinal + 1)));
    }
    assert_eq!(catalog::name_digest("abc"), "ba7816bf8f01cfea");
    conn.execute_batch("ALTER TABLE tasks ADD COLUMN private_definition_984 TEXT;").unwrap();
    let changed = serde_json::to_string(&inspect(&conn, Store::Tasks).unwrap()).unwrap();
    assert!(changed.contains("changed table tasks"));
    assert!(!changed.contains("private_definition_984"));
}
