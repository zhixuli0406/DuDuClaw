//! R7b part 2: the file-owner inventory spawnd takes itself, once per UID
//! allocation, instead of an installer receipt's `unowned_uids`.
//!
//! Scope: only spawnd's own state directories that can hold employee-owned
//! files ([`INVENTORY_ROOTS`]). spawnd cannot see the host's `/home`, `/root`
//! or `/tmp` by design (its unit hides them), so a file an earlier owner of an
//! id left there is outside this inventory.
//!
//! Every uid and gid owning an entry (the root directories included) that
//! falls inside the employee range is collected; an allocation skips every
//! candidate id found as either (an employee's gid is its uid).
//!
//! Safe as root: each root is opened from `/` without following any symlink
//! through root-owned, not group/other-writable components
//! (`bootstrap_mounts::trusted_directory`); entries are read through
//! directory descriptors and inspected with `fstatat(AT_SYMLINK_NOFOLLOW)`, so
//! a symlink is recorded as itself and never followed; a subdirectory is
//! entered only through `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
//! RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV)`. A mount point (a mounted
//! employee image or any other mount) is recorded by the owner `fstatat`
//! reports for it from the parent and is not entered.
//!
//! Bounded and fail-closed: at most [`MAX_ENTRIES`] entries in total and
//! [`MAX_DEPTH`] levels below a root. Exceeding a bound, or any error other
//! than a root that does not exist yet (fresh install: an empty contribution),
//! fails the whole inventory, and the allocation is refused: unscanned files
//! are never treated as absent.
use std::{collections::BTreeSet, ffi::{CStr, CString, OsStr}, io, mem,
    os::{fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd}, unix::ffi::OsStrExt}, path::{Path, PathBuf}};
use duduclaw_isolation::ids::UidRange;

/// The spawnd state directories that can hold employee-owned files.
pub const INVENTORY_ROOTS: [&str; 5] = [crate::work_anchor::WORK_ANCHOR_DIRECTORY, crate::host::STORAGE_IMAGES,
    crate::host::STORAGE_JOURNAL, crate::installed::PROJECTIONS_DIRECTORY, crate::installed::ETC_STATE_DIRECTORY];
/// Entries inspected across all roots, at most.
pub const MAX_ENTRIES: usize = 200_000;
/// Levels below a root, at most (an entry directly in a root is level 1).
pub const MAX_DEPTH: usize = 12;

/// The scan's bounds ([`Limits::INSTALLED`] in production; smaller in tests).
#[derive(Debug, Clone, Copy)]
pub struct Limits { pub entries: usize, pub depth: usize }
impl Limits { pub const INSTALLED: Self = Self { entries: MAX_ENTRIES, depth: MAX_DEPTH }; }

/// In-range owners seen, and how many entries were inspected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory { pub uids: BTreeSet<u32>, pub gids: BTreeSet<u32>, pub entries: usize }
impl Inventory {
    /// Every id found as an owning uid or gid.
    pub fn occupied(&self) -> BTreeSet<u32> { self.uids.union(&self.gids).copied().collect() }
}

/// `file-owner inventory: <what>`: how a failed scan is reported.
fn refused(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("file-owner inventory: {what}"))
}
/// A failure inside one root's walk, prefixed by [`scan`] with the root.
fn failed(what: impl std::fmt::Display) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, what.to_string()) }

/// `fstatat(directory, name, flags)`.
fn stat_at(directory: BorrowedFd<'_>, name: &CStr, flags: i32) -> io::Result<libc::stat> {
    let mut stat: libc::stat = unsafe { mem::zeroed() };
    if unsafe { libc::fstatat(directory.as_raw_fd(), name.as_ptr(), &mut stat, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}
fn is_directory(stat: &libc::stat) -> bool { stat.st_mode & libc::S_IFMT == libc::S_IFDIR }

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }
/// `name` beneath `directory` as a directory, on the same mount: no symlink,
/// no magic link, no escape, no mount crossing (EXDEV for a mount point).
fn open_same_mount_directory(directory: BorrowedFd<'_>, name: &CStr) -> io::Result<OwnedFd> {
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_XDEV };
    let raw = unsafe { libc::syscall(libc::SYS_openat2, directory.as_raw_fd(), name.as_ptr(), &how, mem::size_of::<OpenHow>()) };
    if raw < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
}

/// The names in `directory` (not `.`/`..`), read through a fresh descriptor
/// of it; more than `limit` names is the entry bound.
fn names(directory: BorrowedFd<'_>, limit: usize) -> io::Result<Vec<CString>> {
    let raw = open_same_mount_directory(directory, c".")?.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(raw); }
        return Err(error);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        unsafe { *libc::__errno_location() = 0; }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) { return Err(error); }
            return Ok(names);
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") { continue; }
        if names.len() == limit { return Err(failed(format_args!("entry bound exceeded ({limit} entries left)"))); }
        names.push(name.to_owned());
    }
}

/// Who owns an entry, from its `fstatat`: `(st_uid, st_gid)` in production;
/// tests substitute owners by relative path (they cannot chown).
pub type OwnerOf<'a> = &'a mut dyn FnMut(&Path, &libc::stat) -> (u32, u32);
/// Opens a root directory; `NotFound` means the root does not exist yet.
pub type OpenRoot<'a> = &'a dyn Fn(&Path) -> io::Result<OwnedFd>;

struct Walk<'a> { range: UidRange, limits: Limits, owner: OwnerOf<'a>, inventory: Inventory }
impl Walk<'_> {
    fn record(&mut self, path: &Path, stat: &libc::stat) {
        let (uid, gid) = (self.owner)(path, stat);
        let range = self.range;
        let inside = |id: u32| id >= range.first && id <= range.last;
        if inside(uid) { self.inventory.uids.insert(uid); }
        if inside(gid) { self.inventory.gids.insert(gid); }
    }
    /// Every entry of `directory` (at `relative`, `depth` levels below its
    /// root), recursively; the descriptor chain held is at most the depth bound.
    fn directory(&mut self, directory: BorrowedFd<'_>, relative: &Path, depth: usize) -> io::Result<()> {
        let names = names(directory, self.limits.entries - self.inventory.entries)?;
        self.inventory.entries += names.len();
        let depth = depth + 1;
        for name in names {
            let path = relative.join(OsStr::from_bytes(name.to_bytes()));
            if depth > self.limits.depth { return Err(failed(format_args!("{path:?} deeper than {} levels", self.limits.depth))); }
            let stat = stat_at(directory, &name, libc::AT_SYMLINK_NOFOLLOW)
                .map_err(|error| failed(format_args!("{path:?}: {error}")))?;
            self.record(&path, &stat);
            if !is_directory(&stat) { continue; }
            match open_same_mount_directory(directory, &name) {
                Ok(child) => {
                    let opened = stat_at(child.as_fd(), c"", libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)?;
                    if opened.st_dev != stat.st_dev || opened.st_ino != stat.st_ino {
                        return Err(failed(format_args!("{path:?} changed while scanned")));
                    }
                    self.directory(child.as_fd(), &path, depth)?;
                }
                // A mount point: its owner (as seen from here) is recorded; never entered.
                Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {}
                Err(error) => return Err(failed(format_args!("{path:?}: {error}"))),
            }
        }
        Ok(())
    }
}

/// The inventory of `roots` for `range` (see the module documentation).
/// `open_root` and `owner` are the production opener/owner except in tests.
pub fn scan(roots: &[impl AsRef<Path>], range: UidRange, limits: Limits, open_root: OpenRoot<'_>, owner: OwnerOf<'_>) -> io::Result<Inventory> {
    let mut walk = Walk { range, limits, owner, inventory: Inventory::default() };
    for root in roots {
        let root = root.as_ref();
        let directory = match open_root(root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(refused(format_args!("{}: {error}", root.display()))),
        };
        let stat = stat_at(directory.as_fd(), c"", libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)?;
        if !is_directory(&stat) { return Err(refused(format_args!("{} is not a directory", root.display()))); }
        walk.record(Path::new(""), &stat);
        walk.directory(directory.as_fd(), &PathBuf::new(), 0)
            .map_err(|error| refused(format_args!("{}: {error}", root.display())))?;
    }
    Ok(walk.inventory)
}

/// The production inventory: [`INVENTORY_ROOTS`] through the trusted-directory
/// opener, real owners, [`Limits::INSTALLED`]. A failure is logged (it refuses
/// the allocation that asked for it).
pub fn scan_installed(range: UidRange) -> io::Result<Inventory> {
    let result = scan(&INVENTORY_ROOTS, range, Limits::INSTALLED, &crate::bootstrap_mounts::trusted_directory,
        &mut |_: &Path, stat: &libc::stat| (stat.st_uid, stat.st_gid));
    if let Err(error) = &result { eprintln!("duduclaw-spawnd: UID allocation refused: {error}"); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Component};

    const RANGE: UidRange = UidRange { first: 70_000, last: 70_099 };
    /// Opens `path` from `/` without following symlinks (no ownership rule:
    /// the unprivileged suite's roots belong to the test user).
    fn open_test_root(path: &Path) -> io::Result<OwnedFd> {
        let raw = unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if raw < 0 { return Err(io::Error::last_os_error()); }
        let mut current = unsafe { OwnedFd::from_raw_fd(raw) };
        for component in path.components() {
            let Component::Normal(name) = component else { continue };
            let name = CString::new(name.as_bytes()).unwrap();
            let raw = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
            if raw < 0 { return Err(io::Error::last_os_error()); }
            current = unsafe { OwnedFd::from_raw_fd(raw) };
        }
        Ok(current)
    }
    /// A canonical temporary root (no symlink in its path).
    fn temporary() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        (directory, path)
    }
    /// `uid-<n>` / `gid-<n>` names own as that uid / gid; anything else as it really is.
    fn named_owner(seen: &mut Vec<PathBuf>) -> impl FnMut(&Path, &libc::stat) -> (u32, u32) + '_ {
        move |path: &Path, stat: &libc::stat| {
            seen.push(path.to_path_buf());
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
            if let Some(id) = name.strip_prefix("uid-") { return (id.parse().unwrap(), stat.st_gid); }
            if let Some(id) = name.strip_prefix("gid-") { return (stat.st_uid, id.parse().unwrap()); }
            (stat.st_uid, stat.st_gid)
        }
    }
    fn scan_named(roots: &[impl AsRef<Path>], limits: Limits) -> (io::Result<Inventory>, Vec<PathBuf>) {
        let mut seen = Vec::new();
        let result = scan(roots, RANGE, limits, &open_test_root, &mut named_owner(&mut seen));
        (result, seen)
    }
    fn real_ids_outside_range() {
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        assert!(!(RANGE.first..=RANGE.last).contains(&uid) && !(RANGE.first..=RANGE.last).contains(&gid),
            "the test account's own ids must be outside {RANGE:?}");
    }

    #[test]
    fn inventory_finds_in_range_owners_at_depth_and_ignores_others() {
        real_ids_outside_range();
        let (_guard, root) = temporary();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/b/c/uid-70005"), b"x").unwrap();
        fs::create_dir(root.join("a/gid-70007")).unwrap();
        fs::write(root.join("a/gid-70007/uid-1234"), b"out of range").unwrap();
        fs::write(root.join("gid-80000"), b"out of range").unwrap();
        let (inventory, seen) = scan_named(&[&root], Limits::INSTALLED);
        let inventory = inventory.unwrap();
        assert_eq!(inventory.uids, BTreeSet::from([70005]));
        assert_eq!(inventory.gids, BTreeSet::from([70007]));
        assert_eq!(inventory.occupied(), BTreeSet::from([70005, 70007]));
        // The root itself, then every entry by its path relative to the root.
        assert_eq!(inventory.entries, 7);
        assert!(seen.contains(&PathBuf::new()));
        assert!(seen.contains(&PathBuf::from("a/b/c/uid-70005")), "{seen:?}");
        assert!(seen.contains(&PathBuf::from("a/gid-70007/uid-1234")), "{seen:?}");
    }

    #[test]
    fn inventory_records_real_owners() {
        let (_guard, root) = temporary();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("d/f"), b"x").unwrap();
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let range = UidRange { first: uid.min(gid), last: uid.max(gid) };
        let inventory = scan(&[&root], range, Limits::INSTALLED, &open_test_root,
            &mut |_: &Path, stat: &libc::stat| (stat.st_uid, stat.st_gid)).unwrap();
        assert!(inventory.uids.contains(&uid) && inventory.gids.contains(&gid), "{inventory:?}");
        // A range that holds neither: nothing.
        let other = if uid.max(gid) < u32::MAX - 10 { UidRange { first: uid.max(gid) + 1, last: uid.max(gid) + 10 } }
            else { UidRange { first: 0, last: 0 } };
        if !(other.first..=other.last).contains(&uid) && !(other.first..=other.last).contains(&gid) {
            let inventory = scan(&[&root], other, Limits::INSTALLED, &open_test_root,
                &mut |_: &Path, stat: &libc::stat| (stat.st_uid, stat.st_gid)).unwrap();
            assert!(inventory.occupied().is_empty(), "{inventory:?}");
        }
    }

    #[test]
    fn inventory_never_follows_a_symlink() {
        real_ids_outside_range();
        let (_guard, root) = temporary();
        let (_outside_guard, outside) = temporary();
        fs::create_dir(outside.join("uid-70009")).unwrap();
        fs::write(outside.join("uid-70009/gid-70010"), b"x").unwrap();
        // A link to the directory holding in-range files, and a link that is itself in range.
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("uid-70009"), root.join("uid-70011")).unwrap();
        let (inventory, seen) = scan_named(&[&root], Limits::INSTALLED);
        let inventory = inventory.unwrap();
        assert_eq!(inventory.occupied(), BTreeSet::from([70011]), "{seen:?}");
        assert_eq!(inventory.entries, 2);
    }

    #[test]
    fn inventory_entry_bound_fails_closed() {
        let (_guard, root) = temporary();
        fs::create_dir(root.join("d")).unwrap();
        for index in 0..4 { fs::write(root.join(format!("d/f{index}")), b"x").unwrap(); }
        // Five entries in total (d and its four files).
        let (inventory, _) = scan_named(&[&root], Limits { entries: 5, depth: MAX_DEPTH });
        assert_eq!(inventory.unwrap().entries, 5);
        for entries in [0, 1, 4] {
            let (inventory, _) = scan_named(&[&root], Limits { entries, depth: MAX_DEPTH });
            assert!(inventory.is_err(), "{entries}");
        }
        // The bound is over all roots together.
        let (_second_guard, second) = temporary();
        fs::write(second.join("f"), b"x").unwrap();
        let (inventory, _) = scan_named(&[&root, &second], Limits { entries: 5, depth: MAX_DEPTH });
        assert!(inventory.is_err());
        let (inventory, _) = scan_named(&[&root, &second], Limits { entries: 6, depth: MAX_DEPTH });
        assert_eq!(inventory.unwrap().entries, 6);
    }

    #[test]
    fn inventory_depth_bound_fails_closed() {
        let (_guard, root) = temporary();
        let mut path = root.clone();
        for level in 1..=MAX_DEPTH { path = path.join(format!("d{level}")); }
        fs::create_dir_all(&path).unwrap();
        let (inventory, _) = scan_named(&[&root], Limits::INSTALLED);
        assert_eq!(inventory.unwrap().entries, MAX_DEPTH);
        // One more level, a file or a directory, exceeds the bound.
        fs::write(path.join("f"), b"x").unwrap();
        let (inventory, _) = scan_named(&[&root], Limits::INSTALLED);
        let error = inventory.unwrap_err();
        assert!(error.to_string().contains(&format!("deeper than {MAX_DEPTH} levels")), "{error}");
    }

    #[test]
    fn inventory_missing_root_is_empty_and_other_open_errors_fail() {
        let (_guard, root) = temporary();
        fs::write(root.join("uid-70001"), b"x").unwrap();
        let missing = root.join("not-created-yet");
        let (inventory, _) = scan_named(&[&missing], Limits::INSTALLED);
        assert_eq!(inventory.unwrap(), Inventory::default());
        let (inventory, _) = scan_named(&[&missing, &root], Limits::INSTALLED);
        assert_eq!(inventory.unwrap().occupied(), BTreeSet::from([70001]));
        // A root behind a symlink, a root that is a file, or a refused opener: not "absent".
        let link = root.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(scan_named(&[&link], Limits::INSTALLED).0.is_err());
        assert!(scan_named(&[&root.join("uid-70001")], Limits::INSTALLED).0.is_err());
        let denied = |_: &Path| -> io::Result<OwnedFd> { Err(io::ErrorKind::PermissionDenied.into()) };
        assert!(scan(&[&root], RANGE, Limits::INSTALLED, &denied, &mut |_: &Path, _: &libc::stat| (0, 0)).is_err());
    }

    #[test]
    fn inventory_roots_are_spawnd_state_directories() {
        assert_eq!(INVENTORY_ROOTS, ["/var/lib/duduclaw-spawnd/work-anchors", "/var/lib/duduclaw-spawnd/images",
            "/var/lib/duduclaw-spawnd/storage", "/var/lib/duduclaw-spawnd/projections", "/var/lib/duduclaw-spawnd/etc-state"]);
    }

    struct RemoveTree(PathBuf);
    impl Drop for RemoveTree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    /// Owners the root test gives its fixtures (all in the default employee range).
    const FILE_UID: u32 = 70_101;
    const NESTED_GID: u32 = 70_102;
    const BEHIND_MOUNT: u32 = 70_103;
    const MOUNTED_ROOT: u32 = 70_104;

    /// R7b on a real host, as root: files chowned to two in-range ids are
    /// reported; a directory bind-mounted onto a subdirectory is not entered
    /// (its in-range file is not reported) while the mount point's owner, as
    /// seen from its parent, is. The mount is made in the private mount
    /// namespace of a fresh re-exec of only this test; this process prepares
    /// and removes the fixtures.
    #[test]
    #[ignore = "iso_root_: disposable root Linux VM (DUDUCLAW_ISO_DISPOSABLE=1); chowns and bind-mounts only its own fixtures under /var/lib"]
    fn iso_root_uid_inventory_finds_foreign_owned_files_and_skips_mounts() {
        use std::{os::unix::fs::{chown, DirBuilderExt}, time::{Duration, Instant}};
        const CHILD: &str = "DUDUCLAW_ISO_UID_INVENTORY_ROOT";
        const NAME: &str = "owner_inventory::tests::iso_root_uid_inventory_finds_foreign_owned_files_and_skips_mounts";
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires native root Linux; no skip");
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
            "set DUDUCLAW_ISO_DISPOSABLE=1 only on a disposable VM");
        let range = crate::registry::Registry::default().range;
        if let Ok(base) = std::env::var(CHILD) {
            mounted_directory_is_not_entered(Path::new(&base), range);
            return;
        }
        let base = PathBuf::from(format!("/var/lib/duduclaw-r7b-inventory-{}", std::process::id()));
        let _remove = RemoveTree(base.clone());
        let (root, source) = (base.join("root"), base.join("source"));
        for directory in [&base, &root, &source] { fs::DirBuilder::new().mode(0o700).create(directory).unwrap(); }
        fs::write(root.join("a"), b"a").unwrap();
        chown(root.join("a"), Some(FILE_UID), Some(0)).unwrap();
        fs::create_dir_all(root.join("d1/d2")).unwrap();
        fs::write(root.join("d1/d2/b"), b"b").unwrap();
        chown(root.join("d1/d2/b"), Some(0), Some(NESTED_GID)).unwrap();
        fs::DirBuilder::new().mode(0o700).create(root.join("mnt")).unwrap();
        fs::write(source.join("third"), b"c").unwrap();
        chown(source.join("third"), Some(BEHIND_MOUNT), Some(BEHIND_MOUNT)).unwrap();
        chown(&source, Some(MOUNTED_ROOT), Some(0)).unwrap();
        for id in [FILE_UID, NESTED_GID, BEHIND_MOUNT, MOUNTED_ROOT] { assert!((range.first..=range.last).contains(&id)); }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([NAME, "--exact", "--ignored", "--test-threads=1", "--nocapture"])
            .env(CHILD, &base)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().expect("fresh process running only this test");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline { timed_out = true; child.kill().unwrap(); break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!timed_out, "child exceeded its deadline");
        assert!(output.status.success() && stdout.contains("1 passed"), "child failed or did not run: {stdout}{}",
            String::from_utf8_lossy(&output.stderr));
    }
    /// Child half of `iso_root_uid_inventory_finds_foreign_owned_files_and_skips_mounts`.
    fn mounted_directory_is_not_entered(base: &Path, range: UidRange) {
        let (root, source) = (base.join("root"), base.join("source"));
        let cpath = |path: &Path| CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNS) }, 0, "private mount namespace");
        assert_eq!(unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) }, 0, "private propagation");
        assert_eq!(unsafe { libc::mount(cpath(&source).as_ptr(), cpath(&root.join("mnt")).as_ptr(), std::ptr::null(),
            libc::MS_BIND, std::ptr::null()) }, 0, "bind mount");
        assert!(root.join("mnt/third").exists(), "the bind mount is visible");
        // The production opener and owners, with only the root replaced.
        let inventory = scan(&[&root], range, Limits::INSTALLED, &crate::bootstrap_mounts::trusted_directory,
            &mut |_: &Path, stat: &libc::stat| (stat.st_uid, stat.st_gid)).unwrap();
        assert_eq!(inventory.uids, BTreeSet::from([FILE_UID, MOUNTED_ROOT]), "{inventory:?}");
        assert_eq!(inventory.gids, BTreeSet::from([NESTED_GID]), "{inventory:?}");
        assert!(!inventory.occupied().contains(&BEHIND_MOUNT), "{inventory:?}");
        // root/{a, d1, d1/d2, d1/d2/b, mnt}: nothing below the mount point.
        assert_eq!(inventory.entries, 5);
    }
}
