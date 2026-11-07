use super::*;

fn set_acl(file: &File, security: PSECURITY_DESCRIPTOR) {
    let mut acl = null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    assert_ne!(unsafe { GetSecurityDescriptorDacl(security, &mut present, &mut acl, &mut defaulted) }, 0);
    assert_ne!(present, 0);
    check(unsafe { SetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        null_mut(), null_mut(), acl, null_mut()) }).unwrap();
}

fn home() -> tempfile::TempDir {
    let home = crate::fs_safe::private_test_home().unwrap();
    let file = OpenOptions::new().access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(home.path()).unwrap();
    let user = token_information(TokenUser).unwrap();
    set_acl(&file, descriptor(&user, true).unwrap().as_ptr());
    home
}

fn dacl(file: &File) -> Vec<u8> {
    let mut acl = null_mut();
    let mut security = null_mut();
    check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        null_mut(), null_mut(), &mut acl, null_mut(), &mut security) }).unwrap();
    let _security = LocalAllocation(security);
    assert!(!acl.is_null());
    unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize).to_vec() }
}

fn directory_acl_handle(path: &Path) -> File {
    OpenOptions::new().access_mode(READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path).unwrap()
}

fn add_inheritable_users_read(file: &File) {
    let mut acl = null_mut();
    let mut security = null_mut();
    check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        null_mut(), null_mut(), &mut acl, null_mut(), &mut security) }).unwrap();
    let _security = LocalAllocation(security);
    let mut users = [0u32; 17];
    let mut size = std::mem::size_of_val(&users) as u32;
    assert_ne!(unsafe { CreateWellKnownSid(WinBuiltinUsersSid, null_mut(), users.as_mut_ptr().cast(), &mut size) }, 0);
    let grant = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ, grfAccessMode: GRANT_ACCESS,
        grfInheritance: OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
        Trustee: TRUSTEE_W { pMultipleTrustee: null_mut(), MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID, TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: users.as_mut_ptr().cast() },
    };
    let mut expanded = null_mut();
    check(unsafe { SetEntriesInAclW(1, &grant, acl, &mut expanded) }).unwrap();
    let _expanded = LocalAllocation(expanded.cast());
    // SetSecurityInfo propagates this grant to existing unprotected children.
    check(unsafe { SetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        null_mut(), null_mut(), expanded, null_mut()) }).unwrap();
}

#[test]
fn private_sqlite_blocks_late_home_acl_inheritance_for_database_and_sidecars() {
    for wal in [false, true] {
        let home = home();
        let db = open(home.path(), "state.db").unwrap();
        db.execute_batch("CREATE TABLE example(value)").unwrap();
        let user = token_information(TokenUser).unwrap();
        let private = db._directory.directory_file();
        let before_acl = dacl(private);
        // This sibling proves the ACL update really propagated, rather than
        // passing only because open handles prevented all propagation.
        let control = home.path().join("inheritance-control");
        std::fs::create_dir(&control).unwrap();
        validate_security(&directory_acl_handle(&control), &user, false).unwrap();
        add_inheritable_users_read(&directory_acl_handle(home.path()));
        assert!(validate_security(&directory_acl_handle(&control), &user, false).is_err());
        assert_eq!(dacl(private), before_acl);
        validate_security(private, &user, true).unwrap();
        if wal { db.execute_batch("PRAGMA journal_mode=WAL").unwrap(); }
        db.execute_batch("BEGIN IMMEDIATE; INSERT INTO example VALUES(1)").unwrap();
        for suffix in if wal { vec!["", "-wal", "-shm"] } else { vec!["", "-journal"] } {
            let file = db._directory.open_read_checked(&format!("state.db{suffix}"), true)
                .unwrap().expect("write transaction must create the sidecar");
            validate_entry(&file, false).unwrap();
            validate_security(&file, &user, suffix.is_empty()).unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }
}

#[test]
fn private_sqlite_accepts_home_with_inheritable_users_read() {
    let home = home();
    let home_handle = directory_acl_handle(home.path());
    add_inheritable_users_read(&home_handle);
    let before_acl = dacl(&home_handle);
    let db = open(home.path(), "state.db").unwrap();
    db.execute_batch("CREATE TABLE example(value); INSERT INTO example VALUES(1)").unwrap();
    validate_security(db._directory.directory_file(), &token_information(TokenUser).unwrap(), true).unwrap();
    assert_eq!(dacl(&home_handle), before_acl);
}

#[test]
fn private_sqlite_refuses_unprotected_directory_without_repair() {
    let home = home();
    let path = home.path().join(super::super::PRIVATE_DIRECTORY);
    std::fs::create_dir(&path).unwrap();
    let file = directory_acl_handle(&path);
    let user = token_information(TokenUser).unwrap();
    // The inherited ACL is otherwise trusted; only protection is missing.
    validate_security(&file, &user, false).unwrap();
    assert!(validate_security(&file, &user, true).is_err());
    let before_acl = dacl(&file);
    assert!(open(home.path(), "state.db").is_err());
    assert_eq!(dacl(&file), before_acl);
    assert!(validate_security(&file, &user, true).is_err());
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
}

#[test]
fn private_sqlite_refuses_secure_directory_junction() {
    let home = home();
    let outside = crate::fs_safe::private_test_home().unwrap();
    let path = home.path().join(super::super::PRIVATE_DIRECTORY);
    junction(outside.path(), &path);
    assert!(open(home.path(), "state.db").is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    std::fs::remove_dir(&path).unwrap();
}

#[test]
fn private_sqlite_creates_private_database_and_reopens() {
    // Exercise the real inherited temp ACL, including elevated default owners.
    let home = crate::fs_safe::private_test_home().unwrap();
    let db = open(home.path(), "state.db").unwrap();
    validate_security(&db._file, &token_information(TokenUser).unwrap(), true).unwrap();
    db.execute_batch("CREATE TABLE example(value); INSERT INTO example VALUES(1)").unwrap();
    drop(db);
    let db = open(home.path(), "state.db").unwrap();
    assert_eq!(db.query_row("SELECT value FROM example", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn private_sqlite_reopen_preserves_live_connection_locks() {
    for wal in [false, true] {
        let home = home();
        let live = open(home.path(), "state.db").unwrap();
        live.execute_batch("CREATE TABLE example(value)").unwrap();
        if wal { live.execute_batch("PRAGMA journal_mode=WAL").unwrap(); }
        live.execute_batch("BEGIN IMMEDIATE; INSERT INTO example VALUES(1)").unwrap();
        let other = open(home.path(), "state.db").unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert_eq!(other.execute_batch("BEGIN IMMEDIATE").unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy), "WAL={wal}");
        drop(other);
        let other = open(home.path(), "state.db").unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert_eq!(other.execute_batch("BEGIN IMMEDIATE").unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy), "WAL={wal}");
        live.execute_batch("COMMIT").unwrap();
        other.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
    }
}

fn junction(target: &Path, link: &Path) {
    // cmd's mklink does not accept canonical verbatim paths.
    let target = local_home(target).unwrap();
    let link = local_home(link).unwrap();
    let output = std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"])
        .arg(&link).arg(&target).output().unwrap();
    assert!(output.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn private_sqlite_refuses_ancestor_and_database_junctions() {
    let home = home();
    let outside = home.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let alias = home.path().join("alias");
    junction(&outside, &alias);
    assert!(open(&alias, "state.db").is_err());
    assert!(!outside.join("state.db").exists());
    std::fs::remove_dir(&alias).unwrap();
    drop(open(home.path(), "prepare.db").unwrap());
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let entry = super::super::database_path(home.path(), &format!("state.db{suffix}"));
        junction(&outside, &entry);
        assert!(open(home.path(), "state.db").is_err(), "{suffix}");
        std::fs::remove_dir(entry).unwrap();
    }
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn private_sqlite_refuses_hardlinked_database() {
    let home = home();
    drop(open(home.path(), "state.db").unwrap());
    let path = super::super::database_path(home.path(), "state.db");
    let before = std::fs::read(&path).unwrap();
    std::fs::hard_link(&path, home.path().join("alias")).unwrap();
    assert!(open(home.path(), "state.db").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn private_sqlite_refuses_extra_ace_without_repairing_it() {
    let home = home();
    drop(open(home.path(), "state.db").unwrap());
    let path = super::super::database_path(home.path(), "state.db");
    let file = OpenOptions::new().access_mode(READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(&path).unwrap();
    let user = token_information(TokenUser).unwrap();
    let security = descriptor(&user, false).unwrap();
    let mut acl = null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    assert_ne!(unsafe { GetSecurityDescriptorDacl(security.as_ptr(), &mut present, &mut acl, &mut defaulted) }, 0);
    // Add Everyone read while retaining the original private grants.
    let mut everyone = [0u32; 17];
    let mut size = std::mem::size_of_val(&everyone) as u32;
    assert_ne!(unsafe { CreateWellKnownSid(WinWorldSid, null_mut(), everyone.as_mut_ptr().cast(), &mut size) }, 0);
    let grant = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ, grfAccessMode: GRANT_ACCESS, grfInheritance: 0,
        Trustee: TRUSTEE_W { pMultipleTrustee: null_mut(), MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID, TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: everyone.as_mut_ptr().cast() },
    };
    let mut expanded = null_mut();
    check(unsafe { SetEntriesInAclW(1, &grant, acl, &mut expanded) }).unwrap();
    let _expanded = LocalAllocation(expanded.cast());
    check(unsafe { SetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        null_mut(), null_mut(), expanded, null_mut()) }).unwrap();
    let before_acl = dacl(&file);
    let before_bytes = std::fs::read(&path).unwrap();
    assert!(open(home.path(), "state.db").is_err());
    assert_eq!(dacl(&file), before_acl);
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
}

#[test]
fn private_sqlite_pins_database_and_ancestor_chain_until_close() {
    let root = home();
    let ancestor = root.path().join("ancestor");
    let home = ancestor.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = open(&home, "state.db").unwrap();
    assert!(std::fs::rename(super::super::database_path(&home, "state.db"), home.join("moved.db")).is_err());
    assert!(std::fs::rename(home.join(super::super::PRIVATE_DIRECTORY), home.join("moved-secure-db")).is_err());
    assert!(std::fs::rename(&home, ancestor.join("moved")).is_err());
    assert!(std::fs::rename(&ancestor, root.path().join("moved")).is_err());
    drop(db);
    std::fs::rename(super::super::database_path(&home, "state.db"), home.join("moved.db")).unwrap();
    std::fs::rename(&ancestor, root.path().join("moved")).unwrap();
}

#[test]
fn private_sqlite_refuses_invalid_home_and_names() {
    for path in ["relative", "C:relative", "\\\\server\\share\\home", "\\\\?\\UNC\\server\\share",
        "\\\\.\\C:\\home", "\\\\?\\GLOBALROOT\\Device\\HarddiskVolume1", "C:\\home\\..\\other", "C:\\home\\.\\other"] {
        assert!(open(Path::new(path), "state.db").is_err(), "{path}");
    }
    let home = home();
    for name in ["../state.db", "state.db:stream", "NUL", "state.db."] {
        assert!(open(home.path(), name).is_err(), "{name}");
    }
}

#[test]
fn private_sqlite_acl_policy_rejects_foreign_owner_and_unprotected_database() {
    let user = b"user".as_slice();
    let trusted = [(0, user), (0, SYSTEM_SID), (0, ADMINISTRATORS_SID)];
    assert!(acl_allowed(user, user, true, true, trusted));
    for owner in [b"foreign".as_slice(), SYSTEM_SID, ADMINISTRATORS_SID] {
        assert!(!acl_allowed(owner, user, true, true, trusted));
    }
    assert!(acl_allowed(ADMINISTRATORS_SID, user, true, false, trusted));
    assert!(!acl_allowed(b"foreign", user, true, false, trusted));
    assert!(!acl_allowed(user, user, false, true, trusted));
    assert!(!acl_allowed(user, user, true, true, [(0, b"foreign".as_slice())]));
}

