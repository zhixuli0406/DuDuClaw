//! "Provably fresh host" probe for the gateway-only protected install.
//! Fresh means every trace of an earlier legacy, protected, migration or
//! isolation install is absent AND no `duduclaw` account exists. Anything
//! else, including an unreadable path or a failed account query, is "not
//! fresh", which keeps the migration refusal for an unknown version floor.
//!
//! A fresh install that fails part-way (after `useradd` created the account
//! and /var/lib/duduclaw, but before the version floor) would otherwise never
//! be fresh again. Before its first side effect it therefore writes the
//! root-owned `FRESH_PENDING` marker; while that marker validates strictly and
//! nothing beyond what the fresh install itself creates exists, a retry
//! continues the same fresh install. `record_floor` removes the marker.
use super::*;

/// Paths an earlier DuDuClaw service install, migration or updater leaves.
/// The mode marker and version lock are deliberately absent: `install` writes
/// them before it can refuse, so a retry after a refusal stays fresh.
pub(super) const RESIDUE: [&str; 11] = [
    "/var/lib/duduclaw",
    duduclaw_core::root_key::KEY_PATH,
    VERSION_FLOOR,
    VERSION_PENDING,
    BINARY,
    LAUNCHER,
    UNIT_PATH,
    duduclaw_migration::import_manifest::DURABLE_IMPORT_MANIFEST,
    "/var/lib/duduclaw-migrate",
    "/var/lib/duduclaw-updated/state.json",
    "/opt/duduclaw-isolated/current",
];

/// Root-owned 0600 marker of an interrupted fresh install.
pub(super) const FRESH_PENDING: &str = "/var/lib/duduclaw-protected/fresh-install-pending";
const FRESH_PENDING_TEXT: &[u8] = b"fresh-install-pending\n";

/// Residue an interrupted fresh install never creates before `record_floor`:
/// any of these means the host carries a floor, a key or migration/updater
/// state, so the marker cannot vouch for it. The account, /var/lib/duduclaw,
/// BINARY, LAUNCHER, UNIT_PATH and VERSION_PENDING may exist.
pub(super) const CONTINUATION_BLOCKERS: [&str; 6] = [
    VERSION_FLOOR,
    duduclaw_core::root_key::KEY_PATH,
    duduclaw_migration::import_manifest::DURABLE_IMPORT_MANIFEST,
    "/var/lib/duduclaw-migrate",
    "/var/lib/duduclaw-updated/state.json",
    "/opt/duduclaw-isolated/current",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Account { Absent, Present, Unknown }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker { Absent, Valid, Invalid }

fn under(root: &Path, path: &str) -> PathBuf { root.join(path.trim_start_matches('/')) }

/// `root` prefixes every residue path so tests can probe a scratch tree.
pub(super) fn is_fresh(root: &Path, account: impl FnOnce() -> Account) -> bool {
    RESIDUE.iter().all(|path| absent(&under(root, path)))
        && account() == Account::Absent
}

/// The install-time fact: provably fresh, or a strictly validated marker of
/// an interrupted fresh install with no floor, key or migration/updater state.
/// `owner` is the required uid/gid of the marker and every ancestor below
/// `root` (root:root in production); any marker validation failure is "not fresh".
pub(super) fn fresh_for_install(root: &Path, owner: (u32, u32), account: impl FnOnce() -> Account) -> bool {
    match marker_state(root, owner) {
        Marker::Absent => is_fresh(root, account),
        Marker::Valid => CONTINUATION_BLOCKERS.iter().all(|path| absent(&under(root, path))),
        Marker::Invalid => {
            eprintln!("警告：{FRESH_PENDING} 無法驗證，這台主機不視為全新安裝。");
            false
        }
    }
}

// Only NotFound proves absence; permission or ENOTDIR errors do not.
fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

// Same strictness as `read_version_file`: no final symlink, owner, regular
// file, exact 0600, one link, bounded size, and trusted ancestors. Exact
// content as well, so no other root-owned file can stand in for the marker.
fn marker_state(root: &Path, owner: (u32, u32)) -> Marker {
    use std::io::Read;
    let path = under(root, FRESH_PENDING);
    let mut file = match fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Marker::Absent,
        Err(_) => return Marker::Invalid,
    };
    let trusted_dir = |dir: &Path| matches!(fs::symlink_metadata(dir), Ok(metadata)
        if metadata.is_dir() && metadata.uid() == owner.0 && metadata.gid() == owner.1
            && metadata.mode() & 0o022 == 0);
    // With root "/" this is every ancestor, exactly like `parents`.
    let Some(parent) = path.parent() else { return Marker::Invalid };
    if !parent.ancestors().take_while(|ancestor| ancestor.starts_with(root)).all(trusted_dir) {
        return Marker::Invalid;
    }
    let Ok(metadata) = file.metadata() else { return Marker::Invalid };
    if !metadata.is_file() || metadata.uid() != owner.0 || metadata.gid() != owner.1
        || metadata.mode() & 0o7777 != 0o600 || metadata.nlink() != 1 || metadata.len() > 128 {
        return Marker::Invalid;
    }
    let mut bytes = Vec::new();
    if std::io::Read::by_ref(&mut file).take(129).read_to_end(&mut bytes).is_err() || bytes != FRESH_PENDING_TEXT {
        return Marker::Invalid;
    }
    Marker::Valid
}

// getent exits 2 only when the key is not in the database.
fn account_state() -> Account {
    match command("getent", &["passwd", "duduclaw"]) {
        Ok(output) if output.status.code() == Some(2) => Account::Absent,
        Ok(output) if output.status.success() => Account::Present,
        _ => Account::Unknown,
    }
}

/// Caller holds the version lock.
pub(super) fn host_is_fresh() -> bool {
    fresh_for_install(Path::new("/"), (0, 0), account_state)
}

/// Caller holds the version lock and has decided the host is fresh; written
/// before `account()` creates anything.
pub(super) fn mark_pending() -> Result<()> {
    atomic_install(FRESH_PENDING, 0o600, |file| { std::io::Write::write_all(file, FRESH_PENDING_TEXT)?; Ok(()) })
}

/// Called by `record_floor` once the floor is durable, and by `uninstall`
/// before anything else is removed, so a marker never outlives the floor or
/// key that blocks it. NotFound is success; any other error refuses. After a
/// removal the parent (the root-only protected state directory) is synced.
/// `remove_file` never follows a final symlink.
pub(super) fn clear_pending(root: &Path) -> Result<()> {
    let path = under(root, FRESH_PENDING);
    match fs::remove_file(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().ok_or_else(|| refused("missing parent"))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(root: &Path, residue: &str) {
        let path = under(root, residue);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"").unwrap();
    }

    fn owner() -> (u32, u32) { unsafe { (libc::getuid(), libc::getgid()) } }

    // Canonical so every ancestor below the prefix is a real directory.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        (dir, root)
    }

    // A valid marker as `mark_pending` writes it, owned by the test account.
    fn place_marker(root: &Path) -> PathBuf {
        let path = under(root, FRESH_PENDING);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for dir in ["var", "var/lib", "var/lib/duduclaw-protected"] {
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(&path, FRESH_PENDING_TEXT).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    // What an interrupted fresh install may leave before record_floor.
    const PARTIAL: [&str; 5] = ["/var/lib/duduclaw/.duduclaw/config.toml", BINARY, LAUNCHER, UNIT_PATH, VERSION_PENDING];

    #[test]
    fn service_protected_fresh_host_is_fresh() {
        let root = tempfile::tempdir().unwrap();
        // Files install() writes before it can refuse do not count as residue.
        place(root.path(), duduclaw_core::protected_mode::MODE_MARKER);
        place(root.path(), "/var/lib/duduclaw-protected/version-lock");
        assert!(is_fresh(root.path(), || Account::Absent));
        assert!(fresh_for_install(root.path(), owner(), || Account::Absent));
    }

    #[test]
    fn service_protected_fresh_host_refuses_each_residue_path() {
        for residue in RESIDUE {
            let root = tempfile::tempdir().unwrap();
            place(root.path(), residue);
            assert!(!is_fresh(root.path(), || Account::Absent), "{residue}");
            assert!(!fresh_for_install(root.path(), owner(), || Account::Absent), "{residue}");
        }
    }

    #[test]
    fn service_protected_fresh_host_refuses_dangling_symlink_residue() {
        for residue in RESIDUE {
            let root = tempfile::tempdir().unwrap();
            let path = under(root.path(), residue);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap();
            assert!(!is_fresh(root.path(), || Account::Absent), "{residue}");
        }
    }

    #[test]
    fn service_protected_fresh_host_refuses_account_or_probe_errors() {
        let root = tempfile::tempdir().unwrap();
        assert!(!is_fresh(root.path(), || Account::Present));
        assert!(!is_fresh(root.path(), || Account::Unknown));
        // A path that cannot be resolved (ENOTDIR) is not proof of absence.
        fs::write(root.path().join("var"), b"").unwrap();
        assert!(!is_fresh(root.path(), || Account::Absent));
    }

    #[test]
    fn service_protected_fresh_marker_continues_interrupted_install() {
        let (_dir, root) = scratch();
        for partial in PARTIAL { place(&root, partial); }
        // Without the marker this partial state is an unknown-floor host.
        assert!(!fresh_for_install(&root, owner(), || Account::Present));
        place_marker(&root);
        assert!(fresh_for_install(&root, owner(), || Account::Present));
        assert!(fresh_for_install(&root, owner(), || Account::Absent));
    }

    #[test]
    fn service_protected_fresh_marker_tampering_is_not_fresh() {
        type Tamper = fn(&Path, &Path);
        let cases: [(&str, Tamper); 7] = [
            ("group-readable", |_, marker| fs::set_permissions(marker, fs::Permissions::from_mode(0o640)).unwrap()),
            ("world-readable", |_, marker| fs::set_permissions(marker, fs::Permissions::from_mode(0o644)).unwrap()),
            ("hard link", |root, marker| fs::hard_link(marker, root.join("var/lib/duduclaw-protected/alias")).unwrap()),
            ("symlink", |root, marker| {
                let target = root.join("elsewhere");
                fs::rename(marker, &target).unwrap();
                std::os::unix::fs::symlink(&target, marker).unwrap();
            }),
            ("directory", |_, marker| { fs::remove_file(marker).unwrap(); fs::create_dir(marker).unwrap(); }),
            ("content", |_, marker| fs::write(marker, b"1.81.0").unwrap()),
            ("writable parent", |root, _| fs::set_permissions(root.join("var/lib/duduclaw-protected"),
                fs::Permissions::from_mode(0o770)).unwrap()),
        ];
        for (name, tamper) in cases {
            let (_dir, root) = scratch();
            for partial in PARTIAL { place(&root, partial); }
            let marker = place_marker(&root);
            assert!(fresh_for_install(&root, owner(), || Account::Present), "{name} control");
            tamper(&root, &marker);
            assert!(!fresh_for_install(&root, owner(), || Account::Present), "{name}");
            // A marker that fails validation never yields "fresh", even without residue.
            for partial in PARTIAL { fs::remove_file(under(&root, partial)).unwrap(); }
            fs::remove_dir_all(root.join("var/lib/duduclaw")).unwrap();
            assert!(!fresh_for_install(&root, owner(), || Account::Absent), "{name} without residue");
        }
        // Wrong owner: the scratch tree is not owned by the required account.
        let (_dir, root) = scratch();
        place_marker(&root);
        let (uid, gid) = owner();
        assert!(!fresh_for_install(&root, (uid.wrapping_add(1), gid), || Account::Absent));
        assert!(!fresh_for_install(&root, (uid, gid.wrapping_add(1)), || Account::Absent));
    }

    #[test]
    fn service_protected_fresh_marker_refuses_floor_key_or_migration_state() {
        for blocker in CONTINUATION_BLOCKERS {
            let (_dir, root) = scratch();
            for partial in PARTIAL { place(&root, partial); }
            place(&root, blocker);
            place_marker(&root);
            assert!(!fresh_for_install(&root, owner(), || Account::Present), "{blocker}");
            fs::remove_file(under(&root, blocker)).unwrap();
            std::os::unix::fs::symlink(root.join("missing"), under(&root, blocker)).unwrap();
            assert!(!fresh_for_install(&root, owner(), || Account::Present), "dangling {blocker}");
        }
    }

    #[test]
    fn service_protected_fresh_marker_removed_after_record_floor() {
        let (_dir, root) = scratch();
        for partial in PARTIAL { place(&root, partial); }
        let marker = place_marker(&root);
        // record_floor writes the floor, removes VERSION_PENDING, then the marker.
        place(&root, VERSION_FLOOR);
        fs::remove_file(under(&root, VERSION_PENDING)).unwrap();
        assert!(!fresh_for_install(&root, owner(), || Account::Present));
        clear_pending(&root).unwrap();
        assert!(absent(&marker));
        clear_pending(&root).unwrap();
        // After uninstall removes the floor, the host is an unknown-floor host.
        fs::remove_file(under(&root, VERSION_FLOOR)).unwrap();
        assert!(!fresh_for_install(&root, owner(), || Account::Present));
    }

    // uninstall clears the marker before either removal path runs, while the
    // floor still exists, so no later step leaves a marker without its blocker.
    #[test]
    fn service_protected_fresh_uninstall_clears_marker_before_floor() {
        let (_dir, root) = scratch();
        for partial in PARTIAL { place(&root, partial); }
        place(&root, VERSION_FLOOR);
        let marker = place_marker(&root);
        clear_pending(&root).unwrap();
        assert!(absent(&marker));
        // A failure right after (floor still present, or later removed) never
        // revives the earlier fresh install.
        assert!(!fresh_for_install(&root, owner(), || Account::Present));
        fs::remove_file(under(&root, VERSION_FLOOR)).unwrap();
        assert!(!fresh_for_install(&root, owner(), || Account::Present));
    }

    #[test]
    fn service_protected_fresh_clear_pending_absent_is_ok() {
        let (_dir, root) = scratch();
        // Neither the marker nor its directory exists.
        clear_pending(&root).unwrap();
        place_marker(&root);
        clear_pending(&root).unwrap();
        clear_pending(&root).unwrap();
    }

    #[test]
    fn service_protected_fresh_clear_pending_refuses_other_errors() {
        let (_dir, root) = scratch();
        let marker = place_marker(&root);
        // A directory in the marker's place cannot be unlinked: refuse, keep it.
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(clear_pending(&root).is_err());
        assert!(fs::symlink_metadata(&marker).unwrap().is_dir());
        // A symlink in its place is removed without touching the target.
        fs::remove_dir(&marker).unwrap();
        let target = root.join("elsewhere");
        fs::write(&target, FRESH_PENDING_TEXT).unwrap();
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        clear_pending(&root).unwrap();
        assert!(absent(&marker));
        assert!(target.exists());
    }
}
