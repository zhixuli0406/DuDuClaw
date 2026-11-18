//! Installer-owned create-only retirement lease; never reset its contents.
use super::*;

pub(super) const CONFIG: &str = "/usr/lib/tmpfiles.d/duduclaw-parse.conf";
pub(super) const DIRECTORY: &str = "/run/duduclaw-parse";
const LEASE: &str = "/run/duduclaw-parse/retirement.lease";
pub(super) const TEXT: &str = include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse-tmpfiles.conf");
pub(super) const INVALID: &str = "文件解析服務的 /run/duduclaw-parse 必須是 root:root 0755 目錄，/run/duduclaw-parse/retirement.lease 必須是 root:duduclaw 0660、單一硬連結的一般檔案，且不得為符號連結；已停止。";
const CREATE_FAILED: &str = "無法套用 /usr/lib/tmpfiles.d/duduclaw-parse.conf，已停止文件解析服務安裝。";

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

/// Reject existing invalid objects before tmpfiles can repair their metadata.
/// mkdir proves ownership of the runtime tree to the active creation ledger;
/// tmpfiles creates only its child, which rollback removes with that tree.
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
    mkdir(DIRECTORY, 0o755).map_err(|_| refused(INVALID))?;
    install_with(
        || atomic_install(CONFIG, 0o644, |file| { file.write_all(TEXT.as_bytes())?; Ok(()) }),
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

/// Called only after uninstall fenced and quiesced every owned unit/process.
/// Keep unknown children: remove_dir refuses a nonempty runtime directory.
pub(super) fn remove() -> Result<()> {
    match fs::symlink_metadata(DIRECTORY) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if directory_valid(&metadata) => (),
        _ => return Err(refused(INVALID)),
    }
    parents(Path::new(DIRECTORY)).map_err(|_| refused(INVALID))?;
    match fs::symlink_metadata(LEASE) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Ok(_) => {
            let output = checked("getent", &["group", "duduclaw"])?;
            let text = String::from_utf8(output.stdout).map_err(|_| refused(INVALID))?;
            let fields: Vec<_> = text.trim_end_matches('\n').split(':').collect();
            if fields.len() != 4 || fields[0] != "duduclaw" { return Err(refused(INVALID)); }
            let gid = fields[2].parse::<u32>().map_err(|_| refused(INVALID))?;
            verify(gid)?;
            fs::remove_file(LEASE).map_err(|_| refused(INVALID))?;
        }
        _ => return Err(refused(INVALID)),
    }
    fs::remove_dir(DIRECTORY).map_err(|_| refused(INVALID))?;
    Ok(())
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
}
