//! Fixed installation inventory. Publication/activation belongs to signed bootstrap.
// Keep this module std-only so its policy tests can run without building the CLI.
pub(super) const BINARIES: &[&str] = &[
    "/usr/lib/duduclaw/duduclaw-spawnd",
    "/usr/lib/duduclaw/duduclaw-run",
    "/opt/duduclaw/bin/duduclaw-egress",
    "/usr/libexec/duduclaw/duduclaw-parse-worker",
    "/usr/lib/duduclaw/current/duduclaw-import",
    "/usr/lib/duduclaw/current/duduclaw-migrate",
    "/usr/lib/duduclaw/current/duduclaw-mcp-bridge",
    "/usr/libexec/duduclaw-updated-launch",
];

pub(super) const ACCOUNTS: &[&str] = &[
    "duduclaw-runsup", "duduclaw-egress", "duduclaw-dl", "duduclaw-dns",
];

// Parent-first; none of these directories belongs to an employee or gateway.
pub(super) const DIRECTORIES: &[(&str, u32)] = &[
    ("/usr/lib/duduclaw", 0o755),
    ("/usr/lib/duduclaw/parse-root", 0o755),
    ("/usr/lib/duduclaw/parse-root-tree", 0o755),
    ("/var/lib/duduclaw-spawnd", 0o700),
    ("/var/lib/duduclaw-spawnd/etc", 0o700),
    ("/var/log/duduclaw-spawnd", 0o700),
    ("/etc/duduclaw", 0o755),
    ("/etc/duduclaw-updated", 0o700),
    ("/var/lib/duduclaw-updated", 0o700),
    ("/opt/duduclaw-isolated", 0o755),
    ("/opt/duduclaw-isolated/generations", 0o755),
    ("/opt/duduclaw-isolated/slots", 0o700),
    ("/opt/duduclaw-isolated/slots/a", 0o700),
    ("/opt/duduclaw-isolated/slots/b", 0o700),
    ("/var/lib/duduclaw-migrate", 0o700),
];

pub(super) const UNITS: &[(&str, &str)] = &[
    ("duduclaw-spawnd.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.service")),
    ("duduclaw-spawnd.socket", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.socket")),
    ("duduclaw.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw.slice")),
    ("duduclaw-tcb.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-tcb.slice")),
    ("duduclaw-employees.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-employees.slice")),
    ("duduclaw-egress.socket", include_str!("../../../duduclaw-egress/systemd/duduclaw-egress.socket")),
    ("duduclaw-egress.service", include_str!("../../../duduclaw-egress/systemd/duduclaw-egress.service")),
    ("duduclaw-dns-broker.socket", include_str!("../../../duduclaw-egress/systemd/duduclaw-dns-broker.socket")),
    ("duduclaw-dns-broker.service", include_str!("../../../duduclaw-egress/systemd/duduclaw-dns-broker.service")),
    ("duduclaw-parse.socket", include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse.socket")),
    ("duduclaw-parse@.service", include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse@.service")),
    ("duduclaw-updated.service", include_str!("../../../../scripts/linux-protected/updated/duduclaw-updated.service")),
    ("duduclaw-import@.service", include_str!("../../../duduclaw-import/systemd/duduclaw-import@.service")),
    ("duduclaw-migrate-expire@.service", include_str!("../../../duduclaw-migration/systemd/duduclaw-migrate-expire@.service")),
    ("duduclaw-migrate-expire@.timer", include_str!("../../../duduclaw-migration/systemd/duduclaw-migrate-expire@.timer")),
];

pub(super) fn system_id_limit(defs: &str, key: &str) -> Result<u32, &'static str> {
    let mut found = None;
    for line in defs.lines() {
        let fields: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        if fields.first().copied() == Some(key) {
            if fields.len() != 2 || found.is_some() { return Err("ambiguous system ID range"); }
            found = Some(fields[1].parse::<u32>().map_err(|_| "invalid system ID range")?);
        }
    }
    match found {
        Some(value) if value > 1 && value < 61184 => Ok(value),
        _ => Err("explicit static system ID range required in /etc/login.defs"),
    }
}

pub(super) fn validate_system_ids(uid: u32, gid: u32, defs: &str) -> Result<(), &'static str> {
    let uid_max = system_id_limit(defs, "SYS_UID_MAX")?;
    let gid_max = system_id_limit(defs, "SYS_GID_MAX")?;
    if uid == 0 || gid == 0 || uid >= uid_max || gid >= gid_max {
        return Err("service identities must be below the system ID limits");
    }
    Ok(())
}

// G retains a root-owned enclosing directory so the gateway cannot replace
// its own home. Production callers always pass root ownership as 0:0.
#[cfg(unix)]
pub(super) fn migrated_home_is_private(parent: &std::path::Path, root_uid: u32, root_gid: u32,
    gateway_uid: u32, gateway_gid: u32) -> std::io::Result<bool> {
    use std::{fs, os::unix::fs::MetadataExt};
    for (path, uid, gid, mode) in [(parent.to_path_buf(), root_uid, root_gid, 0o711),
        (parent.join(".duduclaw"), gateway_uid, gateway_gid, 0o700)] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid
            || metadata.gid() != gid || metadata.mode() & 0o7777 != mode { return Ok(false); }
    }
    Ok(true)
}

// systemd leaves empty mountpoints after a worker exits. Only this bounded
// skeleton may survive a reinstall; never delete unexpected content recursively.
#[cfg(unix)]
pub(super) fn parse_root_is_reusable(path: &std::path::Path, uid: u32, gid: u32) -> std::io::Result<bool> {
    use std::{fs, os::unix::fs::MetadataExt};
    let base = fs::symlink_metadata(path)?;
    if !base.is_dir() || base.file_type().is_symlink() || base.uid() != uid || base.gid() != gid
        || base.mode() & 0o022 != 0 { return Ok(false); }
    let allowed = [".tmp", "dev", "etc", "home", "lib", "lib64", "proc", "root", "run", "sys", "tmp", "usr", "var", "var/tmp"];
    let mut pending = vec![path.to_path_buf()];
    let mut seen = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            seen += 1;
            if seen > allowed.len() { return Ok(false); }
            let child = entry.path();
            let relative = child.strip_prefix(path).ok().and_then(|value| value.to_str());
            if !relative.is_some_and(|value| allowed.contains(&value)) { return Ok(false); }
            let meta = fs::symlink_metadata(&child)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
                || meta.dev() != base.dev() || meta.mode() & 0o022 != 0 { return Ok(false); }
            pending.push(child);
        }
    }
    Ok(true)
}

// The caller fixes root to / and expected ownership to 0:0. Tests use an
// isolated tree; there is no environment-controlled production destination.
#[cfg(unix)]
pub(super) fn published_components(root: &std::path::Path, uid: u32, gid: u32)
    -> std::io::Result<bool> {
    use std::{fs, io::{Error, ErrorKind}, os::unix::fs::MetadataExt};
    let unsafe_path = || Error::new(ErrorKind::PermissionDenied, "unsafe or incomplete isolation publication");
    let paths: Vec<_> = BINARIES.iter().map(|path| root.join(path.trim_start_matches('/'))).collect();
    let present: Vec<_> = paths.iter().map(fs::symlink_metadata).collect();
    if present.iter().all(|entry| entry.as_ref().is_err_and(|error| error.kind() == ErrorKind::NotFound)) {
        return Ok(false);
    }
    for (path, entry) in paths.iter().zip(present) {
        let meta = entry?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
            || meta.mode() & 0o7777 != 0o755 || meta.nlink() != 1 || meta.len() == 0 {
            return Err(unsafe_path());
        }
        for parent in path.parent().ok_or_else(unsafe_path)?.ancestors() {
            let meta = fs::symlink_metadata(parent)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
                || meta.mode() & 0o022 != 0 { return Err(unsafe_path()); }
            if parent == root { break; }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_protected_layout_rejects_unsafe_or_unknown_system_ranges() {
        let defs = "SYS_UID_MAX 999\nSYS_GID_MAX 999 # explicit\n";
        assert!(validate_system_ids(998, 998, defs).is_ok());
        for id in [0, 999, 1000, 61184, 65519, 70000, 99999] {
            assert!(validate_system_ids(id, 998, defs).is_err());
            assert!(validate_system_ids(998, id, defs).is_err());
        }
        for defs in ["", "# SYS_UID_MAX 999\nSYS_GID_MAX 999", "SYS_UID_MAX 999\nSYS_UID_MAX 998\nSYS_GID_MAX 999",
            "SYS_UID_MAX 70000\nSYS_GID_MAX 999", "SYS_UID_MAX 999 extra\nSYS_GID_MAX 999"] {
            assert!(validate_system_ids(500, 500, defs).is_err());
        }
    }

    #[test]
    fn service_protected_layout_covers_unit_executables_and_empty_parse_roots() {
        for (_, text) in UNITS {
            for command in text.lines().filter_map(|line| line.strip_prefix("ExecStart=")) {
                let binary = command.split_whitespace().next().unwrap();
                assert!(BINARIES.contains(&binary), "unpublished unit executable: {binary}");
            }
        }
        for root in ["/usr/lib/duduclaw/parse-root", "/usr/lib/duduclaw/parse-root-tree"] {
            assert!(DIRECTORIES.contains(&(root, 0o755)));
        }
        assert_eq!(ACCOUNTS.len(), 4);
        assert_eq!(BINARIES.len(), 8);
    }
    #[cfg(unix)]
    struct Fixture(std::path::PathBuf);
    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!("iso-i2-layout-{}-{}-{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn populate(&self) {
            use std::{fs, os::unix::fs::PermissionsExt};
            for path in BINARIES {
                let path = self.0.join(path.trim_start_matches('/'));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"fixture executable").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fn check(&self) -> std::io::Result<bool> {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&self.0).unwrap();
            published_components(&self.0, meta.uid(), meta.gid())
        }
        fn first(&self) -> std::path::PathBuf { self.0.join(BINARIES[0].trim_start_matches('/')) }
    }
    #[cfg(unix)]
    impl Drop for Fixture { fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); } }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_requires_complete_publication() {
        let tree = Fixture::new();
        assert!(!tree.check().unwrap());
        tree.populate();
        assert!(tree.check().unwrap());
        assert!(tree.check().unwrap()); // re-install is read-only/idempotent
        std::fs::remove_file(tree.first()).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_refuses_links_modes_and_owner() {
        use std::{fs, os::unix::fs::{symlink, PermissionsExt}};
        let tree = Fixture::new(); tree.populate();
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o775)).unwrap();
        assert!(tree.check().is_err());
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(tree.check().is_err());
        #[cfg(target_os = "linux")]
        {
            fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o4755)).unwrap();
            assert!(tree.check().is_err());
        }
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(published_components(&tree.0, u32::MAX, u32::MAX).is_err());
        let alias = tree.0.join("alias"); fs::hard_link(tree.first(), &alias).unwrap();
        assert!(tree.check().is_err()); fs::remove_file(&alias).unwrap();
        fs::rename(tree.first(), &alias).unwrap(); symlink(&alias, tree.first()).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_refuses_writable_or_linked_ancestors() {
        use std::{fs, os::unix::fs::{symlink, PermissionsExt}};
        let tree = Fixture::new(); tree.populate();
        let parent = tree.first().parent().unwrap().to_path_buf();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(tree.check().is_err());
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let moved = tree.0.join("moved"); fs::rename(&parent, &moved).unwrap(); symlink(&moved, parent).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_migrated_home_keeps_root_parent_and_private_child() {
        use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt, symlink}};
        let tree = Fixture::new();
        let metadata = fs::metadata(&tree.0).unwrap();
        let child = tree.0.join(".duduclaw");
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o711)).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        let check = || migrated_home_is_private(&tree.0, metadata.uid(), metadata.gid(),
            metadata.uid(), metadata.gid()).unwrap();
        assert!(check());
        assert!(!migrated_home_is_private(&tree.0, u32::MAX, metadata.gid(), metadata.uid(), metadata.gid()).unwrap());
        assert!(!migrated_home_is_private(&tree.0, metadata.uid(), metadata.gid(), u32::MAX, metadata.gid()).unwrap());
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!check());
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!check());
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o711)).unwrap();
        fs::remove_dir(&child).unwrap();
        symlink("/tmp", &child).unwrap();
        assert!(!check());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_parse_roots_reuse_only_empty_mountpoint_skeleton() {
        use std::{fs, os::unix::fs::{MetadataExt, symlink}};
        let tree = Fixture::new();
        let meta = fs::metadata(&tree.0).unwrap();
        let check = || parse_root_is_reusable(&tree.0, meta.uid(), meta.gid()).unwrap();
        assert!(check());
        for dir in [".tmp", "dev", "etc", "usr", "var/tmp"] { fs::create_dir_all(tree.0.join(dir)).unwrap(); }
        assert!(check());
        fs::write(tree.0.join("etc/shadow"), b"not allowed").unwrap();
        assert!(!check()); fs::remove_file(tree.0.join("etc/shadow")).unwrap();
        fs::remove_dir(tree.0.join("usr")).unwrap();
        symlink("/usr", tree.0.join("usr")).unwrap();
        assert!(!check());
    }

}
