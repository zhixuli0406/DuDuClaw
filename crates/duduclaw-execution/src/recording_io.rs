//! Publish only a complete bounded recording through retained workspace dirfds.
use std::{fs::File, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::Path};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn open(root: &File, path: &str, flags: i32, mode: u64) -> Result<File, ()> {
    let path = std::ffi::CString::new(path).map_err(|_| ())?;
    #[repr(C)]
    struct How { flags: u64, mode: u64, resolve: u64 }
    let how = How { flags: flags as u64, mode, resolve: 0x01 | 0x02 | 0x04 | 0x08 };
    // SAFETY: retained directory, terminated relative path and initialized ABI.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, root.as_raw_fd(), path.as_ptr(), &how, std::mem::size_of::<How>()) };
    if fd < 0 { return Err(()); }
    // SAFETY: successful syscall returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
fn mkdir(root: &File, name: &str, existing: bool) -> Result<(), ()> {
    let name = std::ffi::CString::new(name).map_err(|_| ())?;
    // SAFETY: retained directory and terminated component, no caller path.
    if unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) } != 0
        && !(existing && std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST)) { return Err(()); }
    Ok(())
}
fn unlink(root: &File, name: &str, directory: bool) {
    if let Ok(name) = std::ffi::CString::new(name) {
        // SAFETY: cleanup is limited to known entries in our exclusive staging.
        unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), if directory { libc::AT_REMOVEDIR } else { 0 }); }
    }
}
pub(crate) fn publish(workspace: &Path, temporary: &Path, id: &str) -> Result<Vec<Value>, ()> {
    if !crate::recording_helper::valid_id(id) { return Err(()); }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let root = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(workspace).map_err(|_| ())?;
    mkdir(&root, "recordings", true)?;
    let parent = open(&root, "recordings", flags, 0)?;
    let staging = format!(".record-{}", uuid::Uuid::new_v4().simple());
    mkdir(&parent, &staging, false)?;
    let destination = open(&parent, &staging, flags, 0)?;
    let names = ["trace.zip", "actions.json", "network.json"];
    let mut published = false;
    let result = (|| {
        let source = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(temporary).map_err(|_| ())?;
        let mut artifacts = vec![];
        for name in names {
            let file = open(&source, name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0)?;
            let metadata = file.metadata().map_err(|_| ())?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() == 0
                || metadata.len() > crate::recording_helper::MAX_ARTIFACT { return Err(()); }
            let mut bytes = Vec::new();
            file.take(crate::recording_helper::MAX_ARTIFACT + 1).read_to_end(&mut bytes).map_err(|_| ())?;
            if bytes.is_empty() || bytes.len() as u64 > crate::recording_helper::MAX_ARTIFACT { return Err(()); }
            let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            let mut output = open(&destination, name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600)?;
            output.write_all(&bytes).and_then(|_| output.sync_all()).map_err(|_| ())?;
            artifacts.push(json!({"name":name,"bytes":bytes.len(),"sha256":hash}));
        }
        destination.sync_all().map_err(|_| ())?;
        let from = std::ffi::CString::new(staging.clone()).map_err(|_| ())?;
        let to = std::ffi::CString::new(id).map_err(|_| ())?;
        // SAFETY: retained parent; atomic whole-directory no-clobber publication.
        if unsafe { libc::syscall(libc::SYS_renameat2, parent.as_raw_fd(), from.as_ptr(), parent.as_raw_fd(), to.as_ptr(), 1u32) } != 0 { return Err(()); }
        published = true;
        parent.sync_all().map_err(|_| ())?;
        Ok(artifacts)
    })();
    if result.is_err() && !published { for name in names { unlink(&destination, name, false); } unlink(&parent, &staging, true); }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_publication_rejects_links_and_never_overwrites() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let work = home.path().join("work"); let temporary = home.path().join("tmp");
        std::fs::create_dir(&work).unwrap(); std::fs::create_dir(&temporary).unwrap();
        let id = "ab".repeat(16);
        for name in ["trace.zip", "actions.json", "network.json"] { std::fs::write(temporary.join(name), name).unwrap(); }
        symlink(&temporary, work.join("recordings")).unwrap();
        assert!(publish(&work, &temporary, &id).is_err());
        std::fs::remove_file(work.join("recordings")).unwrap();
        let artifacts = publish(&work, &temporary, &id).unwrap();
        assert_eq!(artifacts.len(), 3);
        assert!(publish(&work, &temporary, &id).is_err());
        assert_eq!(std::fs::read(work.join("recordings").join(&id).join("trace.zip")).unwrap(), b"trace.zip");
        std::fs::remove_file(temporary.join("trace.zip")).unwrap();
        symlink("/etc/passwd", temporary.join("trace.zip")).unwrap();
        assert!(publish(&work, &temporary, &"cd".repeat(16)).is_err());
        assert!(!work.join("recordings").join("cd".repeat(16)).exists());
        assert_eq!(std::fs::read_dir(work.join("recordings")).unwrap().count(), 1);
    }
}
