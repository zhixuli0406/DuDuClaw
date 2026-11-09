//! F6a: apply.db employee schema, agent.toml strip, manifest walk, fences.
use super::*;

fn memory() -> Connection { Connection::open_in_memory().unwrap() }

fn table_exists(db: &Connection, name: &str) -> bool {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [name], |r| r.get(0)).unwrap()
}

#[test]
fn employee_schema_idempotent_and_upgrades_legacy_rows() {
    let db = memory();
    db.execute_batch("CREATE TABLE confirmed_employees(label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,
        digest BLOB NOT NULL,receipt TEXT NOT NULL);
        INSERT INTO confirmed_employees VALUES('sales','op',X'01','legacy');").unwrap();
    assert!(!has_incarnation(&db).unwrap());
    for _ in 0..3 { ensure_employee_schema(&db).unwrap(); }
    assert!(has_incarnation(&db).unwrap());
    let (tenant, incarnation): (Option<String>, i64) = db.query_row(
        "SELECT tenant_id,incarnation FROM confirmed_employees WHERE label='sales'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((tenant, incarnation), (None, 0), "legacy rows stay unpinned and read incarnation 0");
    for table in ["employee_quarantine", "root_allocations", "employee_schema_version"] {
        assert!(table_exists(&db, table), "{table}");
    }
    assert_eq!(schema_version(&db).unwrap(), Some(EMPLOYEE_SCHEMA_VERSION));
}

#[test]
fn employee_schema_refuses_unknown_future_version() {
    let db = memory();
    ensure_employee_schema(&db).unwrap();
    db.execute("UPDATE employee_schema_version SET version=?1", [EMPLOYEE_SCHEMA_VERSION + 1]).unwrap();
    assert_eq!(ensure_employee_schema(&db), Err(SchemaError::Future));
    assert_eq!(check_readable(&db), Err(SchemaError::Future));
    // The stored version is never lowered by an older build.
    assert_eq!(schema_version(&db).unwrap(), Some(EMPLOYEE_SCHEMA_VERSION + 1));
}

#[test]
fn quarantine_rows_keyed_by_employee_and_incarnation() {
    let db = memory();
    ensure_employee_schema(&db).unwrap();
    let snapshot = json!({"kind": "personal"});
    let stripped = Stripped::status("absent").to_value();
    let row = |incarnation, seal| NewQuarantine { employee: EmployeeId([0xab; 16]), incarnation, label: "alice",
        owner: Some("alice-id"), tenant: "tenant", quarantined_at: 1, root: None, seal,
        principal_snapshot: &snapshot, stripped: &stripped };
    assert!(insert_quarantine(&db, &row(0, "pending")).unwrap());
    assert!(!insert_quarantine(&db, &row(0, "absent")).unwrap(), "the first row is kept");
    assert!(insert_quarantine(&db, &row(1, "absent")).unwrap(), "another incarnation is another row");
    assert_eq!(read_quarantine(&db, EmployeeId([0xab; 16]), 0).unwrap().unwrap().seal, "pending");
    // CHECKs: unknown seal states and bad hex refuse; no owner = Shared.
    assert!(insert_quarantine(&db, &row(2, "revived")).is_err());
    let shared_with_owner = NewQuarantine { owner: None, ..row(3, "absent") };
    assert!(insert_quarantine(&db, &shared_with_owner).unwrap());
    assert_eq!(read_quarantine(&db, EmployeeId([0xab; 16]), 3).unwrap().unwrap().kind, "shared");
    assert!(db.execute("INSERT INTO root_allocations(employee_hex,root_item,root_state) VALUES('XYZ','{}','allocated')", []).is_err());
    assert!(insert_root_allocation(&db, EmployeeId([1; 16]), &json!({"item": 1})).unwrap());
    assert!(!insert_root_allocation(&db, EmployeeId([1; 16]), &json!({"item": 2})).unwrap(), "once per employee id");
    assert_eq!(quarantine_name("alice", EmployeeId([0xab; 16]), 7), format!("{}-7-alice", "ab".repeat(16)));
}

#[test]
fn fence_groups_replace_only_their_own_entries() {
    let mut db = memory();
    ensure_employee_schema(&db).unwrap();
    let snapshot = json!({});
    let stripped = json!({});
    let employee = EmployeeId([2; 16]);
    insert_quarantine(&db, &NewQuarantine { employee, incarnation: 0, label: "team", owner: None, tenant: "t",
        quarantined_at: 1, root: None, seal: "absent", principal_snapshot: &snapshot, stripped: &stripped }).unwrap();
    replace_fence_group(&mut db, employee, 0, "runs:", &[RUN_STOP_FENCE.to_owned()]).unwrap();
    replace_fence_group(&mut db, employee, 0, "approvals:", &["approvals:unavailable".to_owned()]).unwrap();
    replace_fence_group(&mut db, employee, 0, "route", &["route:acct-1:unavailable".to_owned()]).unwrap();
    fn fences(db: &Connection, employee: EmployeeId) -> Vec<String> {
        read_quarantine(db, employee, 0).unwrap().unwrap().pending_fences
    }
    assert_eq!(fences(&db, employee), ["approvals:unavailable", "route:acct-1:unavailable", RUN_STOP_FENCE]);
    replace_fence_group(&mut db, employee, 0, "runs:", &[]).unwrap();
    assert_eq!(fences(&db, employee), ["approvals:unavailable", "route:acct-1:unavailable"]);
    // A fence outside its group, or a missing row, is refused.
    assert!(replace_fence_group(&mut db, employee, 0, "runs:", &["approvals:x".to_owned()]).is_err());
    assert!(replace_fence_group(&mut db, EmployeeId([3; 16]), 0, "runs:", &[]).is_err());
}

#[test]
fn strip_agent_toml_removes_channels_and_db_grants_only() {
    let text = "# keep me\n[agent]\nname = \"bot\"\n\n[capabilities]\ncomputer_use = true\ndb_sources = [\"crm\", 7, \"pg\"]\n\n\
        [channels.telegram]\nbot_token = \"T-SECRET\"\n[channels.line]\nchannel_secret_enc = \"L-SECRET\"\n";
    let (updated, stripped) = strip_agent_toml(text).unwrap();
    let updated = updated.unwrap();
    assert!(!updated.contains("SECRET") && !updated.contains("channels") && !updated.contains("db_sources"), "{updated}");
    assert!(updated.contains("# keep me") && updated.contains("computer_use = true") && updated.contains("name = \"bot\""));
    assert_eq!((stripped.channels, stripped.db_sources, stripped.agent_toml),
        (vec!["line".to_owned(), "telegram".to_owned()], vec!["crm".to_owned(), "pg".to_owned()], "stripped"));
    // Inline tables are stripped too; nothing to strip leaves the text alone.
    let (inline, stripped) = strip_agent_toml("channels = { discord = { bot_token = \"D\" } }\n").unwrap();
    assert!(inline.unwrap().trim().is_empty());
    assert_eq!(stripped.channels, vec!["discord".to_owned()]);
    assert_eq!(strip_agent_toml("[agent]\nname = \"bot\"\n").unwrap().0, None);
    assert!(strip_agent_toml("[agent\n").is_err());
}

#[cfg(unix)]
mod walker {
    use super::*;
    use std::os::fd::OwnedFd;

    fn root(path: &std::path::Path) -> OwnedFd {
        crate::fs_safe::SafeDir::open_nofollow(path, false).unwrap().duplicate_fd().unwrap()
    }
    fn euid() -> u32 { nix::unistd::geteuid().as_raw() }
    fn tree(path: &std::path::Path) -> Manifest {
        match walk::walk(root(path), euid(), MAX_ENTRIES, MAX_BYTES, MAX_DEPTH).unwrap() {
            Walked::Tree(manifest) => manifest,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn manifest_orders_raw_bytes() {
        let dir = crate::fs_safe::private_test_home().unwrap();
        let base = dir.path();
        for name in ["b", "a", "B", "a.d", "a-"] { std::fs::write(base.join(name), name).unwrap(); }
        std::fs::create_dir_all(base.join("dir/sub")).unwrap();
        std::fs::write(base.join("dir/sub/x"), "x").unwrap();
        std::os::unix::fs::symlink("../elsewhere", base.join("dir/link")).unwrap();
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            std::fs::write(base.join(std::ffi::OsStr::from_bytes(b"\xff-raw")), "raw").unwrap();
        }
        let manifest = tree(base);
        let paths: Vec<Vec<u8>> = manifest.entries.iter().map(|e| e.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "sorted by raw bytes");
        assert!(paths.contains(&b"dir/sub/x".to_vec()));
        // The symlink is recorded with its target bytes, never followed.
        let link = manifest.entries.iter().find(|e| e.path == b"dir/link").unwrap();
        assert_eq!((link.kind, link.content.as_slice()), (b'l', b"../elsewhere".as_slice()));
        // Deterministic: the same tree gives the same digest; the label and
        // the org entry are part of it.
        let again = tree(base);
        assert_eq!(manifest.digest("bot", b"org"), again.digest("bot", b"org"));
        assert_ne!(manifest.digest("bot", b"org"), manifest.digest("other", b"org"));
        assert_ne!(manifest.digest("bot", b"org"), manifest.digest("bot", b"org2"));
    }

    #[test]
    fn manifest_rejects_hardlink_foreign_uid_xattr() {
        // Hard link inside the tree.
        let dir = crate::fs_safe::private_test_home().unwrap();
        std::fs::write(dir.path().join("a"), "a").unwrap();
        std::fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
        assert_eq!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, MAX_BYTES, MAX_DEPTH).unwrap(), Walked::Invalid("hardlink"));
        // A FIFO (special file).
        let dir = crate::fs_safe::private_test_home().unwrap();
        nix::unistd::mkfifo(dir.path().join("pipe").as_path(), nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert_eq!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, MAX_BYTES, MAX_DEPTH).unwrap(), Walked::Invalid("special_file"));
        // Foreign uid: the walk compares with the expected uid (a real
        // foreign owner needs root; the comparison is the same).
        let dir = crate::fs_safe::private_test_home().unwrap();
        std::fs::write(dir.path().join("a"), "a").unwrap();
        assert_eq!(walk::walk(root(dir.path()), euid().wrapping_add(1), MAX_ENTRIES, MAX_BYTES, MAX_DEPTH).unwrap(),
            Walked::Invalid("foreign_uid"));
        // Extended attributes (Linux; skipped where the filesystem has no user xattrs).
        #[cfg(target_os = "linux")]
        {
            let dir = crate::fs_safe::private_test_home().unwrap();
            let file = dir.path().join("a");
            std::fs::write(&file, "a").unwrap();
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
            // SAFETY: NUL-terminated path and name, and a 1-byte value.
            let set = unsafe { libc::setxattr(path.as_ptr(), b"user.f6\0".as_ptr().cast(), b"1".as_ptr().cast(), 1, 0) };
            if set == 0 {
                assert_eq!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, MAX_BYTES, MAX_DEPTH).unwrap(), Walked::Invalid("xattr"));
            }
        }
    }

    #[test]
    fn manifest_bounds_report_oversize() {
        let dir = crate::fs_safe::private_test_home().unwrap();
        for name in ["a", "b", "c"] { std::fs::write(dir.path().join(name), "12345").unwrap(); }
        assert_eq!(walk::walk(root(dir.path()), euid(), 2, MAX_BYTES, MAX_DEPTH).unwrap(), Walked::Oversize, "entries");
        assert_eq!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, 14, MAX_DEPTH).unwrap(), Walked::Oversize, "bytes");
        std::fs::create_dir_all(dir.path().join("d1/d2/d3")).unwrap();
        assert_eq!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, MAX_BYTES, 2).unwrap(), Walked::Oversize, "depth");
        assert!(matches!(walk::walk(root(dir.path()), euid(), MAX_ENTRIES, MAX_BYTES, 3).unwrap(), Walked::Tree(_)));
    }
}
