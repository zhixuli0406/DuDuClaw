//! Installer-owned create-only retirement lease; never reset its contents.
use super::*;
use std::{ffi::{CStr, CString}, io::Read, os::fd::FromRawFd};

pub(super) const CONFIG: &str = "/usr/lib/tmpfiles.d/duduclaw-parse.conf";
pub(super) const DIRECTORY: &str = "/run/duduclaw-parse";
const LEASE: &str = "/run/duduclaw-parse/retirement.lease";
const LEASE_NAME: &CStr = c"retirement.lease";
const SOCKET_NAME: &CStr = c"parse.sock";
pub(super) const TEXT: &str = include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse-tmpfiles.conf");
/// Review R8q F1: every text a DuDuClaw release has installed at [`CONFIG`],
/// the current one first. A change of [`TEXT`] keeps the previous text here,
/// so the file an earlier release wrote is still recognised as DuDuClaw's.
const KNOWN_CONTENTS: &[&str] = &[TEXT];
/// A configuration larger than this is never read, let alone compared.
const CONFIG_LIMIT: u64 = 4 * 1024;
/// More entries than this in the runtime directory: refused without a decision.
const RUNTIME_ENTRIES: usize = 8;
pub(super) const INVALID: &str = "文件解析服務的 /run/duduclaw-parse 必須是 root:root 0755 目錄，/run/duduclaw-parse/retirement.lease 必須是 root:duduclaw 0660、單一硬連結的一般檔案，且不得為符號連結；已停止。";
const CREATE_FAILED: &str = "無法套用 /usr/lib/tmpfiles.d/duduclaw-parse.conf，已停止文件解析服務安裝。";
pub(super) const CONFIG_FOREIGN: &str = "/usr/lib/tmpfiles.d/duduclaw-parse.conf 已有一個無法辨識的檔案擋在那裡（內容不是 DuDuClaw 安裝的版本，或它不是 root 擁有、單一硬連結、未超過大小上限的一般檔案）；已停止，未覆蓋也未刪除它。請確認它的來源並移開後再執行。";
const RUNTIME_UNKNOWN: &str = "/run/duduclaw-parse 含有無法辨識的項目，或其中的 parse.sock、retirement.lease 形態不符；已停止，這個目錄裡沒有任何項目被移除。請確認來源並處理後再執行移除。";

fn directory_valid(metadata: &fs::Metadata) -> bool {
    directory_valid_for(metadata, (0, 0))
}

fn directory_valid_for(metadata: &fs::Metadata, owner: (u32, u32)) -> bool {
    metadata.is_dir() && (metadata.uid(), metadata.gid()) == owner && metadata.mode() & 0o7777 == 0o755
}

fn lease_valid(metadata: &fs::Metadata, gid: u32) -> bool {
    lease_valid_for(metadata, 0, gid)
}

fn lease_valid_for(metadata: &fs::Metadata, uid: u32, gid: u32) -> bool {
    gid != 0 && metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == uid
        && metadata.gid() == gid && metadata.mode() & 0o7777 == 0o660
}

/// lstat both names; the directory must be verified before walking through it.
fn verify_at(directory: &Path, lease: &Path, gid: u32) -> Result<()> {
    verify_for(directory, lease, (0, 0), gid)
}

// Owner injection lets unprivileged tests exercise the same lstat rules.
fn verify_for(directory: &Path, lease: &Path, owner: (u32, u32), gid: u32) -> Result<()> {
    let failure = || refused(INVALID);
    let metadata = fs::symlink_metadata(directory).map_err(|_| failure())?;
    if !directory_valid_for(&metadata, owner) { return Err(failure()); }
    let metadata = fs::symlink_metadata(lease).map_err(|_| failure())?;
    if !lease_valid_for(&metadata, owner.0, gid) { return Err(failure()); }
    Ok(())
}

pub(super) fn verify(gid: u32) -> Result<()> {
    parents(Path::new(DIRECTORY)).map_err(|_| refused(INVALID))?;
    verify_at(Path::new(DIRECTORY), Path::new(LEASE), gid)
}

/// Review R8q F1/F2: ownership BY CONTENT of the configuration wherever no
/// creation ledger records it (the plain install path and uninstall). `None`:
/// nothing at `name`. Otherwise the object — opened no-follow, a regular,
/// single-link file owned by `owner`, not group/other writable, at most
/// [`CONFIG_LIMIT`] bytes — must hold exactly one of [`KNOWN_CONTENTS`]
/// (returned with its inode); anything else refuses with [`CONFIG_FOREIGN`]
/// and is neither rewritten nor removed by the caller.
fn config_known_at(parent: &fs::File, name: &CStr, owner: (u32, u32)) -> Result<Option<(&'static str, u64)>> {
    let foreign = || refused(CONFIG_FOREIGN);
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound { return Ok(None); }
        // ELOOP (a symbolic link) and every other failure: not provably ours.
        return Err(foreign());
    }
    // SAFETY: a fresh descriptor owned here.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|_| foreign())?;
    if !metadata.is_file() || (metadata.uid(), metadata.gid()) != owner || metadata.mode() & 0o7022 != 0
        || metadata.nlink() != 1 || metadata.len() > CONFIG_LIMIT {
        return Err(foreign());
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1).read_to_end(&mut bytes).map_err(|_| foreign())?;
    let text = KNOWN_CONTENTS.iter().copied().find(|text| text.as_bytes() == bytes.as_slice()).ok_or_else(foreign)?;
    Ok(Some((text, metadata.ino())))
}

/// The no-follow parent of [`CONFIG`] (every ancestor root-owned, never a
/// link) and the final name; `None` when `/usr/lib/tmpfiles.d` is absent.
fn config_parent() -> Result<Option<(fs::File, CString)>> {
    let parent = Path::new(CONFIG).parent().ok_or_else(|| refused(CONFIG_FOREIGN))?;
    match fs::symlink_metadata(parent) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Ok(Some(super::ledger::open_parent(CONFIG)?)),
    }
}

/// [`config_known_at`] for the fixed [`CONFIG`] (root:root).
pub(super) fn config_known() -> Result<Option<(&'static str, u64)>> {
    match config_parent()? {
        Some((parent, name)) => config_known_at(&parent, &name, (0, 0)),
        None => Ok(None),
    }
}

/// What install does with [`CONFIG`], decided from [`config_known`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigWrite {
    /// Nothing there: create it, never over an object that appears meanwhile.
    Create,
    /// Exactly the current text: left in place (same inode, not rewritten).
    Keep,
    /// An earlier DuDuClaw text: ours, replaced by the current one.
    Replace,
}

fn config_write(found: Option<&str>) -> ConfigWrite {
    match found {
        None => ConfigWrite::Create,
        Some(text) if text == TEXT => ConfigWrite::Keep,
        Some(_) => ConfigWrite::Replace,
    }
}

fn apply_config(action: ConfigWrite, create: impl FnOnce() -> Result<()>, replace: impl FnOnce() -> Result<()>) -> Result<()> {
    match action {
        ConfigWrite::Keep => Ok(()),
        ConfigWrite::Create => create(),
        ConfigWrite::Replace => replace(),
    }
}

fn write_text(file: &mut fs::File) -> Result<()> {
    file.write_all(TEXT.as_bytes())?;
    Ok(())
}

/// Reject existing invalid objects before tmpfiles can repair their metadata.
/// mkdir proves ownership of the runtime tree to the active creation ledger;
/// tmpfiles creates only its child, which rollback removes with that tree.
///
/// Review R8q F1, the two callers:
/// - the plain (non-bootstrap) install: no ledger is active, so the
///   configuration is ours only by CONTENT ([`config_known`]): absent ⇒
///   created no-clobber; the current text ⇒ left in place; an earlier
///   DuDuClaw text ⇒ replaced; anything else ⇒ [`CONFIG_FOREIGN`], before
///   anything here is created, nothing overwritten or deleted.
/// - the isolated bootstrap (S4) runs this inside `Recorder::record`, whose
///   `intend` has ALREADY refused any object at [`CONFIG`] this transaction
///   did not record; this helper only adds the content rule on top (it can
///   refuse more, never accept what the ledger refused). The one object it
///   can find there is the recorded one, whose identity (hash included)
///   still matches: it is kept, and the ledger keeps its recorded identity.
pub(super) fn install(gid: u32) -> Result<()> {
    match fs::symlink_metadata(DIRECTORY) {
        Ok(metadata) if directory_valid(&metadata) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err(refused(INVALID)),
    }
    match fs::symlink_metadata(LEASE) {
        Ok(metadata) if lease_valid(&metadata, gid) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err(refused(INVALID)),
    }
    let action = config_write(config_known()?.map(|(text, _)| text));
    mkdir(DIRECTORY, 0o755).map_err(|_| refused(INVALID))?;
    install_with(
        || apply_config(action, || atomic_create(CONFIG, 0o644, write_text),
            || atomic_install(CONFIG, 0o644, write_text)),
        checked,
        || verify(gid),
    )
}

fn install_with(
    write: impl FnOnce() -> Result<()>,
    mut run: impl FnMut(&str, &[&str]) -> Result<Output>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write()?;
    run("systemd-tmpfiles", &["--create", CONFIG]).map_err(|_| refused(CREATE_FAILED))?;
    verify()
}

/// No-follow removal of `name` below `directory`; already absent is success.
fn unlink_at(directory: &fs::File, name: &CStr, flags: libc::c_int) -> std::io::Result<()> {
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) } == 0 { return Ok(()); }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(error) }
}

/// Review R8q F2: what may leave the runtime tree, decided before any unlink.
/// `None`: no directory. Otherwise the directory (opened
/// O_DIRECTORY|O_NOFOLLOW, `owner` 0755) holds at most the lease (regular,
/// one link, `owner.0`:gid 0660, any content) and the socket (`owner.0`:gid
/// 0660); returned with which of the two are present. Any other entry or
/// shape refuses with [`RUNTIME_UNKNOWN`]. `gid` is asked only when an
/// entry exists.
fn runtime_plan(run: &fs::File, directory: &CStr, owner: (u32, u32), gid: impl FnOnce() -> Result<u32>)
    -> Result<Option<(fs::File, bool, bool)>> {
    let unknown = || refused(RUNTIME_UNKNOWN);
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(run.as_raw_fd(), directory.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound { return Ok(None); }
        return Err(unknown());
    }
    // SAFETY: a fresh descriptor owned here.
    let tree = unsafe { fs::File::from_raw_fd(fd) };
    if !directory_valid_for(&tree.metadata().map_err(|_| unknown())?, owner) { return Err(unknown()); }
    let names = super::bootstrap::directory_names(&tree, RUNTIME_ENTRIES).map_err(|_| unknown())?;
    if names.is_empty() { return Ok(Some((tree, false, false))); }
    let gid = gid()?;
    let (mut lease, mut socket) = (false, false);
    for name in &names {
        let stat = super::ledger::stat_at(tree.as_raw_fd(), name).map_err(|_| unknown())?;
        let shaped = gid != 0 && stat.uid == owner.0 && stat.gid == gid && stat.nlink == 1 && stat.mode & 0o7777 == 0o660;
        let kind = stat.mode & libc::S_IFMT;
        if name.as_c_str() == LEASE_NAME && kind == libc::S_IFREG && shaped { lease = true; }
        else if name.as_c_str() == SOCKET_NAME && kind == libc::S_IFSOCK && shaped { socket = true; }
        else { return Err(unknown()); }
    }
    Ok(Some((tree, lease, socket)))
}

/// Review R8q F2 (path-parameterised core of [`remove`]). Every check first —
/// the configuration's content and the runtime tree's complete listing — and
/// only then, in this order: the lease, the socket, the directory, the
/// configuration. A refusal of either check removes nothing at all.
fn remove_at(config: Option<(&fs::File, &CStr)>, run: &fs::File, directory: &CStr, owner: (u32, u32),
    gid: impl FnOnce() -> Result<u32>, mut unlink: impl FnMut(&fs::File, &CStr, libc::c_int) -> std::io::Result<()>)
    -> Result<()> {
    let known = match config {
        Some((parent, name)) => config_known_at(parent, name, owner)?,
        None => None,
    };
    let tree = runtime_plan(run, directory, owner, gid)?;
    if let Some((tree, lease, socket)) = tree {
        if lease { unlink(&tree, LEASE_NAME, 0)?; }
        if socket { unlink(&tree, SOCKET_NAME, 0)?; }
        tree.sync_all()?;
        // The name must still denote the directory listed above.
        let opened = super::ledger::stat_at(tree.as_raw_fd(), c"")?;
        drop(tree);
        match super::ledger::stat_at(run.as_raw_fd(), directory) {
            Ok(now) if (now.device, now.inode) == (opened.device, opened.inode) => {
                unlink(run, directory, libc::AT_REMOVEDIR)?;
                run.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(refused(RUNTIME_UNKNOWN)),
        }
    }
    if let (Some((parent, name)), Some((_, inode))) = (config, known) {
        match super::ledger::stat_at(parent.as_raw_fd(), name) {
            Ok(now) if now.inode == inode => {
                unlink(parent, name, 0)?;
                parent.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(refused(CONFIG_FOREIGN)),
        }
    }
    Ok(())
}

fn duduclaw_gid() -> Result<u32> {
    let output = checked("getent", &["group", "duduclaw"])?;
    let text = String::from_utf8(output.stdout).map_err(|_| refused(INVALID))?;
    let fields: Vec<_> = text.trim_end_matches('\n').split(':').collect();
    if fields.len() != 4 || fields[0] != "duduclaw" { return Err(refused(INVALID)); }
    fields[2].parse::<u32>().map_err(|_| refused(INVALID))
}

/// Called only after uninstall fenced and quiesced every owned unit, cgroup
/// and process (lifecycle: fence → stop → quiescent → remove), which is what
/// makes removing a dirty lease safe here. Already absent is success.
pub(super) fn remove() -> Result<()> {
    let config = config_parent()?;
    let (run, directory) = super::ledger::open_parent(DIRECTORY).map_err(|_| refused(INVALID))?;
    remove_at(config.as_ref().map(|(parent, name)| (parent, name.as_c_str())), &run, &directory, (0, 0),
        duduclaw_gid, unlink_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, os::unix::process::ExitStatusExt};

    #[test]
    fn parse_lease_install_orders_write_create_verify_before_socket_start() {
        let events = RefCell::new(Vec::new());
        install_with(|| { events.borrow_mut().push("file"); Ok(()) }, |tool, args| {
            assert_eq!(tool, "systemd-tmpfiles");
            assert_eq!(args, ["--create", CONFIG]);
            events.borrow_mut().push("create");
            Ok(Output { status: std::process::ExitStatus::from_raw(0), stdout: vec![], stderr: vec![] })
        }, || { events.borrow_mut().push("verify"); Ok(()) }).unwrap();
        events.borrow_mut().push("socket");
        assert_eq!(*events.borrow(), ["file", "create", "verify", "socket"]);
        let install = include_str!("protected_linux_install.rs");
        assert!(install.contains("parse_lease::install(gid)?;"));
        let bootstrap = include_str!("protected_linux_bootstrap.rs");
        let start = bootstrap.split("fn start_services(").nth(1).unwrap().split("// ── Resume invariants").next().unwrap();
        assert!(start.find("parse_lease::verify(gateway.gid)?").unwrap() < start.find("run_start(context").unwrap());
    }

    #[test]
    fn parse_lease_verify_failure_refuses_after_successful_command() {
        let result = install_with(|| Ok(()), |_, _| Ok(Output {
            status: std::process::ExitStatus::from_raw(0), stdout: vec![], stderr: vec![],
        }), || Err(refused(INVALID)));
        assert!(result.unwrap_err().to_string().contains(LEASE));
    }

    #[test]
    fn parse_lease_lstat_rejects_missing_wrong_mode_group_and_symlink() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("parse");
        let lease = dir.join("retirement.lease");
        let metadata = root.path().metadata().unwrap();
        let owner = (metadata.uid(), metadata.gid());
        let gid = if owner.1 == 0 { 1234 } else { owner.1 };
        let verify = || verify_for(&dir, &lease, owner, gid);
        assert!(verify().is_err());
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify().is_err());
        fs::write(&lease, b"dirty\n").unwrap();
        if owner.1 == 0 {
            let file = fs::File::open(&lease).unwrap();
            assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), owner.0, gid) }, 0);
        }
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o660)).unwrap();
        verify().unwrap();
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(verify().is_err());
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(verify_for(&dir, &lease, owner, gid.wrapping_add(1)).is_err());
        assert!(verify_for(&dir, &lease, (owner.0.wrapping_add(1), owner.1), gid).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(verify().is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::hard_link(&lease, dir.join("other")).unwrap();
        assert!(verify().is_err());
        fs::remove_file(dir.join("other")).unwrap();
        fs::remove_file(&lease).unwrap();
        std::os::unix::fs::symlink("/dev/null", &lease).unwrap();
        assert!(verify().is_err());
        fs::remove_file(&lease).unwrap();
        fs::remove_dir(&dir).unwrap();
        std::os::unix::fs::symlink(root.path(), &dir).unwrap();
        assert!(verify().is_err());
    }

    #[test]
    fn parse_lease_resume_preserves_dirty_contents_and_inode() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("parse");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let lease = dir.join("retirement.lease");
        fs::write(&lease, b"dirty\n").unwrap();
        let file = fs::File::open(&lease).unwrap();
        let owner = (file.metadata().unwrap().uid(), file.metadata().unwrap().gid());
        let gid = if owner.1 == 0 {
            assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), owner.0, 1234) }, 0);
            1234
        } else { owner.1 };
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o660)).unwrap();
        let inode = file.metadata().unwrap().ino();
        for _ in 0..2 {
            install_with(|| Ok(()), |_, _| Ok(Output {
                status: std::process::ExitStatus::from_raw(0), stdout: vec![], stderr: vec![],
            }), || verify_for(&dir, &lease, owner, gid)).unwrap();
        }
        assert_eq!(fs::read(&lease).unwrap(), b"dirty\n");
        assert_eq!(fs::symlink_metadata(&lease).unwrap().ino(), inode);
        assert!(TEXT.lines().any(|line| line.starts_with("f /run/duduclaw-parse/retirement.lease ")));
        assert!(!TEXT.lines().any(|line| line.starts_with("f+")));
    }

    // ── Review R8q F1: ownership of the configuration by content ──────────

    const NAME: &CStr = c"duduclaw-parse.conf";

    /// A temporary parent directory, its descriptor and its owner.
    fn parent() -> (tempfile::TempDir, fs::File, (u32, u32)) {
        let root = tempfile::tempdir().unwrap();
        let metadata = root.path().metadata().unwrap();
        let directory = fs::File::open(root.path()).unwrap();
        (root, directory, (metadata.uid(), metadata.gid()))
    }

    fn write_config(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn never() -> Result<()> { panic!("the configuration must not be written") }

    #[test]
    fn parse_lease_config_absent_is_created_no_clobber() {
        let (_root, directory, owner) = parent();
        assert_eq!(config_known_at(&directory, NAME, owner).unwrap(), None);
        assert_eq!(config_write(None), ConfigWrite::Create);
        let mut created = false;
        apply_config(ConfigWrite::Create, || { created = true; Ok(()) }, never).unwrap();
        assert!(created);
        // Production creates through the no-clobber writer and only replaces a known earlier text.
        let source = include_str!("protected_linux_parse_lease.rs");
        let install = source.split("pub(super) fn install(gid: u32)").nth(1).unwrap().split("fn install_with(").next().unwrap();
        assert!(install.contains("|| atomic_create(CONFIG, 0o644, write_text)"));
        assert!(install.find("config_known()?").unwrap() < install.find("mkdir(DIRECTORY").unwrap());
        let installer = include_str!("protected_linux_install.rs");
        assert!(installer.contains("temp.persist_noclobber(path)"));
    }

    #[test]
    fn parse_lease_config_identical_is_kept_in_place() {
        let (root, directory, owner) = parent();
        let path = root.path().join("duduclaw-parse.conf");
        write_config(&path, TEXT.as_bytes());
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        let found = config_known_at(&directory, NAME, owner).unwrap();
        assert_eq!(found, Some((TEXT, inode)));
        let action = config_write(found.map(|(text, _)| text));
        assert_eq!(action, ConfigWrite::Keep);
        apply_config(action, never, never).unwrap();
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::read(&path).unwrap(), TEXT.as_bytes());
        assert!(KNOWN_CONTENTS.contains(&TEXT));
        // An earlier DuDuClaw text (none today) would be ours and replaced.
        assert_eq!(config_write(Some("an earlier text\n")), ConfigWrite::Replace);
    }

    #[test]
    fn parse_lease_config_foreign_content_refused_and_untouched() {
        let (root, directory, owner) = parent();
        let path = root.path().join("duduclaw-parse.conf");
        let foreign = b"d /run/duduclaw-parse 0777 root root -\n";
        write_config(&path, foreign);
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        let error = config_known_at(&directory, NAME, owner).unwrap_err().to_string();
        assert!(error.contains(CONFIG), "{error}");
        assert_eq!(fs::read(&path).unwrap(), foreign);
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), inode);
        // A known prefix or suffix is not the known content.
        write_config(&path, format!("{TEXT}\n").as_bytes());
        assert!(config_known_at(&directory, NAME, owner).is_err());
    }

    #[test]
    fn parse_lease_config_unsafe_shapes_refused() {
        let (root, directory, owner) = parent();
        let path = root.path().join("duduclaw-parse.conf");
        // A symbolic link to an identical file.
        let elsewhere = root.path().join("elsewhere");
        write_config(&elsewhere, TEXT.as_bytes());
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(config_known_at(&directory, NAME, owner).is_err());
        fs::remove_file(&path).unwrap();
        // Another owner (uid or gid).
        write_config(&path, TEXT.as_bytes());
        config_known_at(&directory, NAME, owner).unwrap().unwrap();
        assert!(config_known_at(&directory, NAME, (owner.0.wrapping_add(1), owner.1)).is_err());
        assert!(config_known_at(&directory, NAME, (owner.0, owner.1.wrapping_add(1))).is_err());
        // Group/other writable.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(config_known_at(&directory, NAME, owner).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        // A second hard link.
        fs::hard_link(&path, root.path().join("alias")).unwrap();
        assert!(config_known_at(&directory, NAME, owner).is_err());
        fs::remove_file(root.path().join("alias")).unwrap();
        config_known_at(&directory, NAME, owner).unwrap().unwrap();
        // Oversized (never read).
        let mut big = TEXT.as_bytes().to_vec();
        big.resize(CONFIG_LIMIT as usize + 1, b'#');
        write_config(&path, &big);
        assert!(config_known_at(&directory, NAME, owner).is_err());
        // A directory.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(config_known_at(&directory, NAME, owner).is_err());
    }

    /// The ledger host of an isolated bootstrap that finds an object at
    /// CONFIG this transaction never recorded.
    struct Unrecorded;
    impl super::super::ledger::LedgerHost for Unrecorded {
        fn save(&mut self, ledger: &super::super::ledger::Ledger) -> Result<()> { ledger.validate() }
        fn present(&mut self, path: &str) -> Result<bool> { Ok(path == CONFIG) }
        fn identity(&mut self, _: &str, _: super::super::ledger::Kind) -> Result<Option<super::super::ledger::Identity>> { Ok(None) }
        fn classify(&mut self, _: &super::super::ledger::Entry) -> Result<super::super::ledger::Now> {
            Ok(super::super::ledger::Now::Other)
        }
    }

    #[test]
    fn parse_lease_bootstrap_ledger_still_refuses_preexisting_unrecorded_config() {
        use super::super::ledger::{Kind, Recorder};
        let mut recorder = Recorder::open(Unrecorded, None, &"cd".repeat(32), &"5a".repeat(32)).unwrap();
        let mut ran = false;
        let error = recorder.record(&[(CONFIG, Kind::File)], || { ran = true; Ok(()) }).unwrap_err().to_string();
        assert!(error.contains(CONFIG), "{error}");
        assert!(!ran, "the installer must not run when the ledger refuses the configuration");
        // S4 records the configuration in the same record that runs this installer.
        let bootstrap = include_str!("protected_linux_bootstrap.rs");
        let s4 = bootstrap.split("items.extend([(parse_lease::CONFIG, Kind::File)").nth(1).unwrap();
        let record = s4.find("recorder.record(&items").unwrap();
        assert!(record < s4.find("install_isolation_support_for(gateway").unwrap());
    }

    // ── Review R8q F2: uninstall decides before it removes ─────────────────

    struct Runtime {
        root: tempfile::TempDir,
        directory: fs::File,
        owner: (u32, u32),
        gid: u32,
    }

    impl Runtime {
        fn path(&self, name: &str) -> std::path::PathBuf { self.root.path().join(name) }

        /// The configuration, the runtime directory, a dirty lease and the socket.
        fn installed() -> Self {
            let (root, directory, owner) = parent();
            let gid = if owner.1 == 0 { 1234 } else { owner.1 };
            let runtime = Self { root, directory, owner, gid };
            write_config(&runtime.path("duduclaw-parse.conf"), TEXT.as_bytes());
            let tree = runtime.path("parse");
            fs::create_dir(&tree).unwrap();
            fs::set_permissions(&tree, fs::Permissions::from_mode(0o755)).unwrap();
            let lease = tree.join("retirement.lease");
            fs::write(&lease, b"dirty\n").unwrap();
            drop(std::os::unix::net::UnixListener::bind(tree.join("parse.sock")).unwrap());
            for entry in [lease, tree.join("parse.sock")] {
                if owner.1 == 0 { std::os::unix::fs::lchown(&entry, Some(owner.0), Some(gid)).unwrap(); }
                fs::set_permissions(&entry, fs::Permissions::from_mode(0o660)).unwrap();
            }
            runtime
        }

        fn remove(&self, unlink: impl FnMut(&fs::File, &CStr, libc::c_int) -> std::io::Result<()>) -> Result<()> {
            let gid = self.gid;
            remove_at(Some((&self.directory, NAME)), &self.directory, c"parse", self.owner, || Ok(gid), unlink)
        }

        fn present(&self) -> [bool; 4] {
            ["parse/retirement.lease", "parse/parse.sock", "parse", "duduclaw-parse.conf"]
                .map(|name| fs::symlink_metadata(self.path(name)).is_ok())
        }
    }

    fn no_unlink(_: &fs::File, _: &CStr, _: libc::c_int) -> std::io::Result<()> {
        panic!("nothing may be removed after a refusal")
    }

    #[test]
    fn parse_lease_uninstall_removes_lease_socket_directory_then_config() {
        let runtime = Runtime::installed();
        let mut events = Vec::new();
        runtime.remove(|directory: &fs::File, name: &CStr, flags: libc::c_int| {
            events.push(name.to_str().unwrap().to_owned());
            unlink_at(directory, name, flags)
        }).unwrap();
        assert_eq!(events, ["retirement.lease", "parse.sock", "parse", "duduclaw-parse.conf"]);
        assert_eq!(runtime.present(), [false; 4]);
        // Idempotent: an already clean host succeeds without any lookup or unlink.
        remove_at(Some((&runtime.directory, NAME)), &runtime.directory, c"parse", runtime.owner,
            || -> Result<u32> { panic!("no entry, no group lookup") }, no_unlink).unwrap();
        remove_at(None, &runtime.directory, c"parse", runtime.owner, || Ok(runtime.gid), no_unlink).unwrap();
    }

    #[test]
    fn parse_lease_uninstall_without_socket_removes_the_dirty_lease() {
        let runtime = Runtime::installed();
        fs::remove_file(runtime.path("parse/parse.sock")).unwrap();
        let mut events = Vec::new();
        runtime.remove(|directory: &fs::File, name: &CStr, flags: libc::c_int| {
            events.push(name.to_str().unwrap().to_owned());
            unlink_at(directory, name, flags)
        }).unwrap();
        assert_eq!(events, ["retirement.lease", "parse", "duduclaw-parse.conf"]);
        assert_eq!(runtime.present(), [false; 4]);
    }

    #[test]
    fn parse_lease_uninstall_unknown_child_or_shape_removes_nothing() {
        let runtime = Runtime::installed();
        fs::write(runtime.path("parse/other"), b"").unwrap();
        let error = runtime.remove(no_unlink).unwrap_err().to_string();
        assert!(error.contains(DIRECTORY), "{error}");
        assert_eq!(runtime.present(), [true; 4]);
        fs::remove_file(runtime.path("parse/other")).unwrap();
        // A wrong lease mode, a second link, the lease as a symbolic link.
        fs::set_permissions(runtime.path("parse/retirement.lease"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(runtime.remove(no_unlink).is_err());
        fs::set_permissions(runtime.path("parse/retirement.lease"), fs::Permissions::from_mode(0o660)).unwrap();
        fs::hard_link(runtime.path("parse/retirement.lease"), runtime.path("alias")).unwrap();
        assert!(runtime.remove(no_unlink).is_err());
        fs::remove_file(runtime.path("alias")).unwrap();
        // The wrong group.
        let gid = runtime.gid.wrapping_add(1);
        assert!(remove_at(Some((&runtime.directory, NAME)), &runtime.directory, c"parse", runtime.owner,
            || Ok(gid), no_unlink).is_err());
        // The runtime directory as a symbolic link.
        fs::rename(runtime.path("parse"), runtime.path("real")).unwrap();
        std::os::unix::fs::symlink(runtime.path("real"), runtime.path("parse")).unwrap();
        assert!(runtime.remove(no_unlink).unwrap_err().to_string().contains(DIRECTORY));
        assert!(fs::symlink_metadata(runtime.path("real/retirement.lease")).is_ok());
        assert!(fs::symlink_metadata(runtime.path("duduclaw-parse.conf")).is_ok());
    }

    #[test]
    fn parse_lease_uninstall_foreign_config_refuses_before_any_removal() {
        let runtime = Runtime::installed();
        let foreign = b"# an administrator's file\n";
        write_config(&runtime.path("duduclaw-parse.conf"), foreign);
        let error = runtime.remove(no_unlink).unwrap_err().to_string();
        assert!(error.contains(CONFIG), "{error}");
        assert_eq!(runtime.present(), [true; 4]);
        assert_eq!(fs::read(runtime.path("duduclaw-parse.conf")).unwrap(), foreign);
        // Also with the runtime tree already gone: the foreign file stays.
        for name in ["parse/retirement.lease", "parse/parse.sock"] { fs::remove_file(runtime.path(name)).unwrap(); }
        fs::remove_dir(runtime.path("parse")).unwrap();
        assert!(runtime.remove(no_unlink).is_err());
        assert_eq!(fs::read(runtime.path("duduclaw-parse.conf")).unwrap(), foreign);
    }

    #[test]
    fn parse_lease_uninstall_runs_after_quiescence_and_before_other_removals() {
        let lifecycle = include_str!("protected_linux_lifecycle.rs");
        let order = lifecycle.split("pub(super) fn remove_installation(").nth(1).unwrap();
        assert!(order.find("host.quiescent(units)?").unwrap() < order.find("host.remove()").unwrap());
        let uninstall = include_str!("protected_linux_uninstall.rs");
        let uninstall = uninstall.split("#[cfg(test)]").next().unwrap();
        let remove = uninstall.split("fn remove(&mut self)").nth(1).unwrap().split("pub(super) fn run()").next().unwrap();
        let lease = remove.find("parse_lease::remove()?").unwrap();
        assert!(remove.find("validate_removals(&self.files)?").unwrap() < lease);
        assert!(lease < remove.find("for file in &self.files").unwrap());
        // Only parse_lease::remove deletes the configuration (content-checked).
        let files = uninstall.split("fn removal_files()").nth(1).unwrap().split("fn validate_removals(").next().unwrap();
        assert!(!files.contains("parse_lease::CONFIG"));
        let run = uninstall.split("pub(super) fn run()").nth(1).unwrap();
        assert!(run.find("parse_lease::config_known()?").unwrap() < run.find("lifecycle::remove_installation").unwrap());
    }
}
