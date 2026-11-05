//! Dirfd-relative operations below an independently verified root-owned anchor.
use std::{ffi::CString, fs::File, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::MetadataExt}, path::{Component, Path}};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{ids::Sha256Digest, update::ReleaseArtifact};
use crate::{Error, Result, state::{DurableState, State}};

const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }

#[derive(Clone, Copy, PartialEq, Eq)]
enum PersistencePoint { TempRemoved, TempCreated, BytesWritten, FileSynced, Renamed, DestinationSynced, SourceSynced }
#[cfg(test)]
thread_local! { static CRASH_AT: std::cell::Cell<Option<PersistencePoint>> = const { std::cell::Cell::new(None) }; }
fn persistence_point(point: PersistencePoint) -> Result<()> {
    #[cfg(test)]
    if CRASH_AT.with(|value| value.get() == Some(point)) {
        CRASH_AT.with(|value| value.set(None));
        return Err(Error::Maintenance);
    }
    let _ = point;
    Ok(())
}

pub struct Directory(File);
fn cstring(value: &str) -> Result<CString> { CString::new(value).map_err(|_| Error::Invalid) }
fn name(value: &str) -> Result<CString> {
    if value.is_empty() || value == "." || value == ".." || value.len() > 200
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
        return Err(Error::Invalid);
    }
    cstring(value)
}
fn open_at(parent: &File, path: &CString, flags: i32, mode: u32, no_xdev: bool) -> Result<File> {
    let how = OpenHow { flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: mode as u64, resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS
            | if no_xdev { RESOLVE_NO_XDEV } else { 0 } };
    // SAFETY: all pointers reference live structures; successful fd is uniquely owned.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, parent.as_raw_fd(), path.as_ptr(), &how,
        std::mem::size_of::<OpenHow>()) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
fn owned_directory(file: &File) -> Result<()> {
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != 0 || m.gid() != 0 || m.mode() & 0o022 != 0 { return Err(Error::Maintenance); }
    Ok(())
}
fn owned_file(file: &File) -> Result<()> {
    let m = file.metadata()?;
    if !m.is_file() || m.uid() != 0 || m.gid() != 0 || m.nlink() != 1 || m.mode() & 0o022 != 0 {
        return Err(Error::Maintenance);
    }
    Ok(())
}
impl Directory {
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd { self.0.as_raw_fd() }
    /// Bounded directory enumeration through the already validated dirfd.
    pub fn entries(&self, limit: usize) -> Result<Vec<String>> {
        let duplicate = open_at(&self.0, &cstring(".")?, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)?;
        use std::os::fd::IntoRawFd;
        let raw = duplicate.into_raw_fd();
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() { unsafe { libc::close(raw); } return Err(std::io::Error::last_os_error().into()); }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
        let stream = Stream(dir);
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0; }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(0) { return Err(error.into()); }
                break;
            }
            let value = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_str().map_err(|_| Error::Invalid)?;
            if value == "." || value == ".." { continue; }
            name(value)?;
            if names.len() == limit { return Err(Error::Maintenance); }
            names.push(value.to_owned());
        }
        Ok(names)
    }
    /// Remove only a bounded flat directory of root-owned regular artifacts.
    /// Symlinks, nested trees, special files and mounts stop cleanup.
    pub fn remove_artifact_directory(&self, child: &str) -> Result<()> {
        let dir = self.child(child)?;
        let entries = dir.entries(duduclaw_isolation::update::MAX_ARTIFACTS + 8)?;
        for entry in &entries { dir.read(entry)?; }
        for entry in entries { dir.remove(&entry)?; }
        let rc = unsafe { libc::unlinkat(self.raw_fd(), name(child)?.as_ptr(), libc::AT_REMOVEDIR) };
        if rc != 0 { return Err(std::io::Error::last_os_error().into()); }
        self.sync()
    }
    pub fn reserve_space(&self, required: u64) -> Result<()> {
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.raw_fd(), stat.as_mut_ptr()) } != 0 { return Err(std::io::Error::last_os_error().into()); }
        let stat = unsafe { stat.assume_init() };
        let free = (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64).ok_or(Error::Maintenance)?;
        if free < required.checked_add(512 * 1024 * 1024).ok_or(Error::Invalid)? || stat.f_favail < 256 {
            return Err(Error::Maintenance);
        }
        Ok(())
    }
    /// Walk every ancestor without symlinks; reject group/world writable parents,
    /// including sticky /tmp. All production roots are fixed constants.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() { return Err(Error::Invalid); }
        let mut dir = File::open("/")?;
        owned_directory(&dir)?;
        for component in path.components() {
            match component {
                Component::RootDir => (),
                Component::Normal(value) => {
                    let value = value.to_str().ok_or(Error::Invalid)?;
                    dir = open_at(&dir, &cstring(value)?, libc::O_RDONLY | libc::O_DIRECTORY, 0, false)?;
                    owned_directory(&dir)?;
                }
                _ => return Err(Error::Invalid),
            }
        }
        Ok(Self(dir))
    }
    pub fn child(&self, child: &str) -> Result<Self> {
        let file = open_at(&self.0, &name(child)?, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)?;
        owned_directory(&file)?;
        Ok(Self(file))
    }
    pub fn create_child(&self, child: &str) -> Result<Self> {
        let c = name(child)?;
        let rc = unsafe { libc::mkdirat(self.0.as_raw_fd(), c.as_ptr(), 0o700) };
        if rc != 0 { return Err(std::io::Error::last_os_error().into()); }
        self.sync()?;
        self.child(child)
    }
    pub fn sync(&self) -> Result<()> { self.0.sync_all()?; Ok(()) }
    pub fn read(&self, filename: &str) -> Result<File> {
        // O_NONBLOCK prevents blocking before rejecting a FIFO/device.
        let file = open_at(&self.0, &name(filename)?, libc::O_RDONLY | libc::O_NONBLOCK, 0, true)?;
        owned_file(&file)?;
        Ok(file)
    }
    pub fn bytes(&self, filename: &str, limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.read(filename)?.take(limit as u64 + 1).read_to_end(&mut out)?;
        if out.len() > limit { return Err(Error::Invalid); }
        Ok(out)
    }
    pub fn json<T: DeserializeOwned>(&self, filename: &str) -> Result<T> {
        Ok(serde_json::from_slice(&self.bytes(filename, 1024 * 1024)?)?)
    }
    pub fn create(&self, filename: &str) -> Result<File> {
        let file = open_at(&self.0, &name(filename)?, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600, true)?;
        owned_file(&file)?;
        Ok(file)
    }
    pub fn remove(&self, filename: &str) -> Result<()> {
        let rc = unsafe { libc::unlinkat(self.0.as_raw_fd(), name(filename)?.as_ptr(), 0) };
        if rc != 0 { return Err(std::io::Error::last_os_error().into()); }
        self.sync()
    }
    pub fn rename(&self, old: &str, destination: &Self, new: &str, replace: bool) -> Result<()> {
        let rc = unsafe { libc::syscall(libc::SYS_renameat2, self.0.as_raw_fd(), name(old)?.as_ptr(),
            destination.0.as_raw_fd(), name(new)?.as_ptr(), if replace { 0 } else { libc::RENAME_NOREPLACE }) };
        if rc != 0 { return Err(std::io::Error::last_os_error().into()); }
        persistence_point(PersistencePoint::Renamed)?;
        destination.sync()?;
        persistence_point(PersistencePoint::DestinationSynced)?;
        self.sync()?;
        persistence_point(PersistencePoint::SourceSynced)
    }
    /// Callers hold the process-wide flock. A stale temp is untrusted/uncommitted;
    /// unlink it by name, never open/truncate it. The old snapshot stays intact.
    pub fn atomic_json<T: Serialize>(&self, filename: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > 1024 * 1024 { return Err(Error::Invalid); }
        let temporary = format!("{filename}.next");
        match self.remove(&temporary) {
            Ok(()) => (),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        persistence_point(PersistencePoint::TempRemoved)?;
        let mut file = self.create(&temporary)?;
        persistence_point(PersistencePoint::TempCreated)?;
        file.write_all(&bytes)?;
        persistence_point(PersistencePoint::BytesWritten)?;
        file.sync_all()?;
        persistence_point(PersistencePoint::FileSynced)?;
        self.rename(&temporary, self, filename, true)
    }
    pub fn lock(&self) -> Result<File> {
        let file = match self.create("update.lock") {
            Ok(f) => f,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => self.read("update.lock")?,
            Err(e) => return Err(e),
        };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 { return Err(Error::Busy); }
        Ok(file)
    }
}
impl DurableState for Directory {
    fn save(&mut self, next: &State) -> Result<()> { self.atomic_json("state.json", next) }
}

/// Root copies an untrusted download stream to a new root-only inode, bounds it,
/// checks size/hash and fsyncs it. Downloaders never hold the destination fd.
pub fn copy_artifact(input: &mut impl Read, output: &mut File, artifact: &ReleaseArtifact) -> Result<()> {
    if artifact.size_bytes == 0 || artifact.size_bytes > crate::state::MAX_ARTIFACT_BYTES { return Err(Error::Invalid); }
    let mut hash = Sha256::new();
    let mut remaining = artifact.size_bytes;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let cap = remaining.min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..cap])?;
        if count == 0 { return Err(Error::Signature); }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    if input.read(&mut [0; 1])? != 0 || hash.finalize().as_slice() != artifact.sha256.0 { return Err(Error::Signature); }
    output.sync_all()?;
    Ok(())
}
pub fn verify_fd(mut file: File, size: u64, digest: Sha256Digest) -> Result<File> {
    owned_file(&file)?;
    if file.metadata()?.len() != size { return Err(Error::Signature); }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        total = total.checked_add(count as u64).ok_or(Error::Signature)?;
        if total > size { return Err(Error::Signature); }
        hash.update(&buffer[..count]);
    }
    if total != size || hash.finalize().as_slice() != digest.0 { return Err(Error::Signature); }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::update::InstallTarget;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn artifact_stream_size_hash_and_bound_checks() {
        let content = b"verified bytes";
        let artifact = ReleaseArtifact { id: "gateway".into(), sha256: Sha256Digest(Sha256::digest(content).into()),
            size_bytes: content.len() as u64, mode: 0o755, install_target: InstallTarget::Gateway };
        let mut file = tempfile::tempfile().unwrap();
        assert!(copy_artifact(&mut &content[..], &mut file, &artifact).is_ok());
        for invalid in [&content[..3], b"verified bytes!", b"corrupted bytes"] {
            let mut file = tempfile::tempfile().unwrap();
            assert!(copy_artifact(&mut invalid.as_ref(), &mut file, &artifact).is_err());
        }
    }
    #[test]
    #[ignore = "Requires root on Linux with openat2; creates only its own /run temporary directory"]
    fn iso_root_dirfd_symlink_hardlink_permissions_and_atomic_replace() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("duduclaw-update-test-").tempdir_in("/run").unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = Directory::open(temp.path()).unwrap();
        let _lock = dir.lock().unwrap();
        dir.atomic_json("receipt.json", &serde_json::json!({"generation": 1})).unwrap();
        symlink("receipt.json", temp.path().join("link")).unwrap();
        assert!(dir.read("link").is_err());
        std::fs::hard_link(temp.path().join("receipt.json"), temp.path().join("hard")).unwrap();
        assert!(dir.read("receipt.json").is_err());
        std::fs::remove_file(temp.path().join("hard")).unwrap();
        // A stale malicious temp is unlinked, never truncated or followed.
        symlink("receipt.json", temp.path().join("receipt.json.next")).unwrap();
        dir.atomic_json("receipt.json", &serde_json::json!({"generation": 2})).unwrap();
        assert_eq!(dir.json::<serde_json::Value>("receipt.json").unwrap()["generation"], 2);
        std::fs::set_permissions(temp.path().join("receipt.json"), std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(dir.read("receipt.json").is_err());
        assert!(dir.read("../receipt.json").is_err());
        assert!(Directory::open(Path::new("/tmp")).is_err());
    }
    #[test]
    #[ignore = "Requires root Linux openat2 and fsync; isolated /run files only"]
    fn iso_root_atomic_snapshot_crash_at_every_persistence_point() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        for point in [PersistencePoint::TempRemoved, PersistencePoint::TempCreated,
            PersistencePoint::BytesWritten, PersistencePoint::FileSynced, PersistencePoint::Renamed,
            PersistencePoint::DestinationSynced, PersistencePoint::SourceSynced] {
            let temp = tempfile::Builder::new().prefix("updated-crash-").tempdir_in("/run").unwrap();
            let dir = Directory::open(temp.path()).unwrap();
            let _lock = dir.lock().unwrap();
            dir.atomic_json("state.json", &serde_json::json!({"floor":79,"phase":"prepared"})).unwrap();
            CRASH_AT.with(|value| value.set(Some(point)));
            assert!(dir.atomic_json("state.json", &serde_json::json!({"floor":80,"phase":"committed"})).is_err());
            // Simulated process death: discard every handle, reopen authoritative
            // bytes. A landed rename is never overwritten from stale memory.
            drop(_lock); drop(dir);
            let reopened = Directory::open(temp.path()).unwrap();
            let observed: serde_json::Value = reopened.json("state.json").unwrap();
            let landed = matches!(point, PersistencePoint::Renamed | PersistencePoint::DestinationSynced | PersistencePoint::SourceSynced);
            assert_eq!(observed["floor"], if landed { 80 } else { 79 });
            assert_eq!(observed["phase"], if landed { "committed" } else { "prepared" });
            // Reentrant whole-snapshot save discards the interrupted temporary.
            reopened.atomic_json("state.json", &observed).unwrap();
            assert_eq!(reopened.json::<serde_json::Value>("state.json").unwrap(), observed);
        }
    }
    #[test]
    #[ignore = "Requires root Linux openat2; isolated /run staging tree only"]
    fn iso_root_partial_stage_cleanup_is_bounded_reentrant_and_refuses_links() {
        let temp = tempfile::Builder::new().prefix("updated-cleanup-").tempdir_in("/run").unwrap();
        let root = Directory::open(temp.path()).unwrap();
        let stage = root.create_child("pending-generation").unwrap();
        stage.create("partial").unwrap().write_all(b"interrupted").unwrap();
        stage.create("complete").unwrap().write_all(b"verified").unwrap();
        stage.remove("partial").unwrap(); // Death halfway through cleanup.
        root.remove_artifact_directory("pending-generation").unwrap();
        let stage = root.create_child("pending-generation").unwrap();
        stage.create("kept").unwrap();
        symlink("kept", temp.path().join("pending-generation/link")).unwrap();
        assert!(root.remove_artifact_directory("pending-generation").is_err());
        assert!(stage.read("kept").is_ok());
        stage.remove("link").unwrap();
        let nested = stage.create_child("nested").unwrap();
        assert!(root.remove_artifact_directory("pending-generation").is_err());
        drop(nested);
        stage.remove_artifact_directory("nested").unwrap();
        root.remove_artifact_directory("pending-generation").unwrap();
    }
    #[test]
    #[ignore = "Requires root Linux execveat and exec-capable /root; executes a copied trusted /usr/bin/true only"]
    fn iso_root_same_verified_fd_exec_survives_path_replacement() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        assert_eq!(nix::unistd::geteuid().as_raw(), 0, "requires root");
        // /run may be noexec (Debian 12). Use a trusted parent that also passes
        // Directory::open's ancestor checks; sticky /var/tmp does not qualify.
        let parent = Directory::open(Path::new("/root")).expect("requires a root-owned, non-writable /root");
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the directory fd is live and the successful call initializes
        // the output before assume_init reads it.
        assert_eq!(unsafe { libc::fstatvfs(parent.raw_fd(), stat.as_mut_ptr()) }, 0,
            "cannot check /root mount flags: {}", std::io::Error::last_os_error());
        let stat = unsafe { stat.assume_init() };
        assert_eq!(stat.f_flag & libc::ST_NOEXEC, 0,
            "execveat fixture requires an exec-capable /root filesystem (ST_NOEXEC is set)");
        let temp = tempfile::Builder::new().prefix("updated-fd-").tempdir_in("/root").unwrap();
        let dir = Directory::open(temp.path()).unwrap();
        let mut source = File::open("/usr/bin/true").unwrap();
        let mut copied = dir.create("verified").unwrap();
        std::io::copy(&mut source, &mut copied).unwrap();
        copied.set_permissions(std::fs::Permissions::from_mode(0o700)).unwrap();
        copied.sync_all().unwrap(); drop(copied);
        let bytes = dir.bytes("verified", 1024 * 1024).unwrap();
        let verified = verify_fd(dir.read("verified").unwrap(), bytes.len() as u64, Sha256Digest(Sha256::digest(&bytes).into())).unwrap();
        // Replace the pathname with another inode after hashing. execveat must
        // still run the verified original, never the now-malicious pathname.
        dir.remove("verified").unwrap();
        dir.create("verified").unwrap().write_all(b"not an executable").unwrap();
        let raw = verified.as_raw_fd();
        let argv0 = CString::new("verified-test").unwrap();
        let empty = CString::new("").unwrap();
        let mut command = Command::new("/usr/bin/false");
        command.env_clear().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        unsafe { command.pre_exec(move || {
            let argv = [argv0.as_ptr(), std::ptr::null()];
            let envp: [*const libc::c_char; 1] = [std::ptr::null()];
            libc::syscall(libc::SYS_execveat, raw, empty.as_ptr(), argv.as_ptr(), envp.as_ptr(), libc::AT_EMPTY_PATH);
            Err(std::io::Error::last_os_error())
        }); }
        assert!(command.status().unwrap().success());
    }

}
