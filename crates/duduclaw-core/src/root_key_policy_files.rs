//! Descriptor-relative root policy storage, following the protected unit checks.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use super::{EngineSocketAction, PolicyEvent, PolicySnapshot};
use std::{fs::File, io::{self, Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::Path};
const POLICY: &str = "protected-policy.toml";
const AUDIT: &str = "protected-policy-audit.jsonl";
const POLICY_LIMIT: u64 = 4096;
const AUDIT_LIMIT: u64 = 4 * 1024 * 1024;

pub(super) struct Dir { file: File, owner: u32 }
fn validate(file: &File, directory: bool, owner: u32) -> io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != owner || m.mode() & 0o7022 != 0
        || (directory && !m.is_dir()) || (!directory && (!m.is_file() || m.nlink() != 1)) {
        return Err(io::Error::other("unsafe protected policy type, owner, mode or link count"));
    }
    Ok(())
}
fn open_at(dir: &File, name: &str, flags: i32, mode: u32) -> io::Result<File> {
    let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
    // Every caller supplies a single constant or generated basename.
    if name.to_bytes().contains(&b'/') { return Err(io::Error::other("invalid basename")); }
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(),
        flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, mode as libc::c_uint) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { File::from_raw_fd(fd) })
}
impl Dir {
    pub(super) fn open(create: bool) -> io::Result<Self> { Self::open_from(Path::new("/"), 0, create) }
    fn open_from(root: &Path, owner: u32, create: bool) -> io::Result<Self> {
        let mut directory = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root)?;
        validate(&directory, true, owner)?;
        for name in ["etc", "duduclaw"] {
            let mut created = false;
            // /etc itself must already exist. Never repair an untrusted path.
            if create && name == "duduclaw" {
                if unsafe { libc::mkdirat(directory.as_raw_fd(), c"duduclaw".as_ptr(), 0o755) } != 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::AlreadyExists { return Err(e); }
                } else { created = true; directory.sync_all()?; }
            }
            directory = open_at(&directory, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            validate(&directory, true, owner)?;
            if created {
                if unsafe { libc::fchmod(directory.as_raw_fd(), 0o755) } != 0 { return Err(io::Error::last_os_error()); }
                directory.sync_all()?;
            }
            if create && directory.metadata()?.mode() & 0o005 != 0o005 {
                return Err(io::Error::other("protected policy directory must allow the service to read and traverse it (0755)"));
            }
        }
        Ok(Self { file: directory, owner })
    }
    fn read(&self, name: &str, limit: u64) -> io::Result<String> {
        let file = open_at(&self.file, name, libc::O_RDONLY, 0)?;
        validate(&file, false, self.owner)?;
        if file.metadata()?.len() > limit { return Err(io::Error::other("protected policy file too large")); }
        let mut text = String::new();
        file.take(limit + 1).read_to_string(&mut text)?;
        if text.len() as u64 > limit { return Err(io::Error::other("protected policy file too large")); }
        Ok(text)
    }
    pub(super) fn read_policy(&self) -> io::Result<PolicySnapshot> { super::parse(&self.read(POLICY, POLICY_LIMIT)?) }
    pub(super) fn read_audit(&self) -> io::Result<Vec<PolicyEvent>> {
        let text = self.read(AUDIT, AUDIT_LIMIT)?;
        // An interrupted final append is not a committed event. Earlier durable
        // records remain importable; the writer refuses a broken audit tail.
        text.split_inclusive('\n').filter(|line| line.ends_with('\n')).map(|line| {
            let event: PolicyEvent = serde_json::from_str(line).map_err(io::Error::other)?;
            if uuid::Uuid::parse_str(&event.id).is_err()
                || chrono::DateTime::parse_from_rfc3339(&event.timestamp).is_err()
                || !matches!(event.event.as_str(), "engine_socket_policy_changed" | "engine_socket_policy_change_requested") {
                return Err(io::Error::other("invalid root policy audit event"));
            }
            Ok(event)
        }).collect()
    }
    fn atomic_policy(&self, bytes: &[u8]) -> io::Result<()> {
        match open_at(&self.file, POLICY, libc::O_RDONLY, 0) {
            Ok(file) => validate(&file, false, self.owner)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let temp = format!(".policy-{}.tmp", uuid::Uuid::new_v4());
        let temp_c = std::ffi::CString::new(temp.as_str()).unwrap();
        let result = (|| {
            let mut file = open_at(&self.file, &temp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o644)?;
            // Operator umask must not make the root policy unreadable to the service.
            if unsafe { libc::fchmod(file.as_raw_fd(), 0o644) } != 0 { return Err(io::Error::last_os_error()); }
            validate(&file, false, self.owner)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            if unsafe { libc::renameat(self.file.as_raw_fd(), temp_c.as_ptr(), self.file.as_raw_fd(), c"protected-policy.toml".as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.file.sync_all()
        })();
        if result.is_err() { unsafe { libc::unlinkat(self.file.as_raw_fd(), temp_c.as_ptr(), 0); } }
        result
    }
    pub(super) fn write_policy(&self, action: EngineSocketAction) -> io::Result<()> {
        use fs2::FileExt;
        let lock = open_at(&self.file, "protected-policy.lock", libc::O_RDWR | libc::O_CREAT, 0o600)?;
        validate(&lock, false, self.owner)?;
        lock.lock_exclusive()?;
        let mut audit = open_at(&self.file, AUDIT, libc::O_RDWR | libc::O_APPEND | libc::O_CREAT, 0o644)?;
        validate(&audit, false, self.owner)?;
        if unsafe { libc::fchmod(audit.as_raw_fd(), 0o644) } != 0 { return Err(io::Error::last_os_error()); }
        if audit.metadata()?.len() > AUDIT_LIMIT - 4096 { return Err(io::Error::other("protected policy audit full; root must archive it before changing policy")); }
        let mut old_audit = String::new();
        (&audit).take(AUDIT_LIMIT + 1).read_to_string(&mut old_audit)?;
        if !old_audit.is_empty() && !old_audit.ends_with('\n') { return Err(io::Error::other("incomplete protected policy audit tail")); }
        self.read_audit()?;
        let id = uuid::Uuid::new_v4().to_string();
        let policy = PolicySnapshot { action, change_id: Some(id.clone()) };
        let bytes = toml::to_string(&policy).map_err(io::Error::other)?;
        let event = |event: &str, event_id: String| PolicyEvent { id: event_id, event: event.into(), action,
            detail: Some(format!("root uid=0; policy change {id}")), timestamp: chrono::Utc::now().to_rfc3339() };
        let append = |file: &mut File, record: PolicyEvent| -> io::Result<()> {
            let mut bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
            bytes.push(b'\n'); file.write_all(&bytes)?; file.sync_all()
        };
        // Durable intent precedes the policy. A crash between rename and the
        // completion record leaves both this intent and change_id as evidence.
        append(&mut audit, event("engine_socket_policy_change_requested", uuid::Uuid::new_v4().to_string()))?;
        self.file.sync_all()?;
        self.atomic_policy(bytes.as_bytes())?;
        append(&mut audit, event("engine_socket_policy_changed", id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn fixture() -> (tempfile::TempDir, Dir) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("etc/duduclaw")).unwrap();
        let dir = Dir::open_from(root.path(), unsafe { libc::geteuid() }, false).unwrap();
        (root, dir)
    }
    #[test]
    fn root_key_policy_private_umask_still_allows_service_read() {
        const CHILD: &str = "DUDUCLAW_POLICY_UMASK_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "root_key::policy::files::tests::root_key_policy_private_umask_still_allows_service_read"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("etc")).unwrap();
        unsafe { libc::umask(0o077); }
        let dir = Dir::open_from(root.path(), unsafe { libc::geteuid() }, true).unwrap();
        assert_eq!(dir.file.metadata().unwrap().mode() & 0o777, 0o755);
        dir.write_policy(EngineSocketAction::Warn).unwrap();
        for name in [POLICY, AUDIT] {
            let file = open_at(&dir.file, name, libc::O_RDONLY, 0).unwrap();
            assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o644);
        }
    }

    #[test]
    fn root_key_policy_file_validation_matrix() {
        let (root, dir) = fixture();
        let path = root.path().join("etc/duduclaw").join(POLICY);
        assert_eq!(dir.read_policy().unwrap_or_default().action, EngineSocketAction::Revoke);
        std::fs::write(&path, "engine_socket_action = 'warn'\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(dir.read_policy().unwrap().action, EngineSocketAction::Warn);
        let wrong = Dir { file: dir.file.try_clone().unwrap(), owner: dir.owner.wrapping_add(1) };
        assert!(wrong.read_policy().is_err());
        for mode in [0o666, 0o664] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(dir.read_policy().is_err(), "requested mode={mode:o}, actual={:o}", std::fs::metadata(&path).unwrap().mode());
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&path, "engine_socket_action = 'bad'").unwrap();
        assert!(dir.read_policy().is_err());
        std::fs::write(&path, "x".repeat(4097)).unwrap();
        assert!(dir.read_policy().is_err());
        std::fs::hard_link(&path, path.with_extension("link")).unwrap();
        assert!(dir.read_policy().is_err());
        std::fs::remove_file(&path).unwrap();
        symlink("/dev/null", &path).unwrap();
        assert!(dir.read_policy().is_err());
        assert!(dir.write_policy(EngineSocketAction::Warn).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(dir.read_policy().is_err());
    }
    #[test]
    fn root_key_policy_parent_checks_and_atomic_audit() {
        let (root, dir) = fixture();
        dir.write_policy(EngineSocketAction::Warn).unwrap();
        let old = open_at(&dir.file, POLICY, libc::O_RDONLY, 0).unwrap();
        let first = dir.read_policy().unwrap();
        dir.write_policy(EngineSocketAction::Revoke).unwrap();
        assert_eq!(dir.read_policy().unwrap().action, EngineSocketAction::Revoke);
        let mut pinned = String::new();
        (&old).read_to_string(&mut pinned).unwrap();
        assert_eq!(super::super::parse(&pinned).unwrap().action, EngineSocketAction::Warn);
        let events = dir.read_audit().unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(Some(&events[1].id), first.change_id.as_ref());
        assert_ne!(events[1].id, events[3].id);
        assert_eq!(old.metadata().unwrap().mode() & 0o777, 0o644);
        let parent = root.path().join("etc");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Dir::open_from(root.path(), dir.owner, false).is_err());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(Dir::open_from(root.path(), dir.owner, true).is_err());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(root.path().join("etc/duduclaw"), root.path().join("etc/real")).unwrap();
        symlink("real", root.path().join("etc/duduclaw")).unwrap();
        assert!(Dir::open_from(root.path(), dir.owner, false).is_err());
    }
}
