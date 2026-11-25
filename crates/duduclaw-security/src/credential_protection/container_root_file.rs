//! Descriptor-relative ciphertext IO. No symlink following or plaintext files.
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use duduclaw_core::trusted_home::TrustedHome;
use ring::rand::{SecureRandom, SystemRandom};
use super::{FILE_NAME, MAX_BLOB, StoreError, unavailable};

#[cfg(test)]
thread_local! {
    static AFTER_DIRECTORY_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn after_directory_open(f: impl FnOnce() + 'static) {
    AFTER_DIRECTORY_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(f)));
}

fn directory(home: &TrustedHome, create: bool) -> Result<TrustedHome, StoreError> {
    // child_directory verifies the shared pin, then mkdirat/openat(O_NOFOLLOW)
    // resolve only the single protected component beneath its retained FD.
    let directory = home.child_directory("protected", create).map_err(io_error)?;
    #[cfg(test)]
    AFTER_DIRECTORY_OPEN.with(|hook| {
        let hook = hook.borrow_mut().take();
        if let Some(hook) = hook { hook(); }
    });
    Ok(directory)
}

fn io_error(e: io::Error) -> StoreError {
    match e.raw_os_error() {
        Some(libc::ENOENT) => StoreError::NotFound,
        Some(libc::ELOOP | libc::ENOTDIR) => StoreError::Tampered,
        _ => unavailable(),
    }
}

fn name() -> CString { CString::new(FILE_NAME).expect("fixed file name") }

pub(super) fn read(home: &TrustedHome) -> Result<Vec<u8>, StoreError> {
    let protected = directory(home, false)?;
    let dir = protected.directory();
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name().as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) };
    if fd < 0 { return Err(io_error(io::Error::last_os_error())); }
    // SAFETY: openat returned a fresh descriptor, owned only by this File.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(|_| unavailable())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > MAX_BLOB { return Err(StoreError::Tampered); }
    let mut bytes = Vec::new();
    file.take(MAX_BLOB + 1).read_to_end(&mut bytes).map_err(|_| unavailable())?;
    if bytes.len() > MAX_BLOB as usize { return Err(StoreError::Tampered); }
    Ok(bytes)
}

pub(super) fn write(home: &TrustedHome, blob: &[u8]) -> Result<(), StoreError> {
    let protected = directory(home, true)?;
    let dir = protected.directory();
    let mut random = [0u8; 16];
    SystemRandom::new().fill(&mut random).map_err(|_| unavailable())?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temp = CString::new(format!(".wrapped-{suffix}.tmp")).expect("hex name");
    let fd = unsafe { libc::openat(dir.as_raw_fd(), temp.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
    if fd < 0 { return Err(unavailable()); }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| {
        if unsafe { libc::fchmod(fd, 0o600) } != 0 { return Err(unavailable()); }
        file.write_all(blob).and_then(|_| file.sync_all()).map_err(|_| unavailable())?;
        if unsafe { libc::renameat(dir.as_raw_fd(), temp.as_ptr(), dir.as_raw_fd(), name().as_ptr()) } != 0 {
            return Err(unavailable());
        }
        dir.sync_all().map_err(|_| unavailable())
    })();
    // Only unlink our random directory-relative entry, even after a rename.
    unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0); }
    result
}

pub(super) fn delete(home: &TrustedHome) -> Result<(), StoreError> {
    let protected = match directory(home, false) {
        Err(StoreError::NotFound) => return Ok(()),
        result => result?,
    };
    let dir = protected.directory();
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name().as_ptr(), 0) } == 0 {
        return dir.sync_all().map_err(|_| unavailable());
    }
    match io_error(io::Error::last_os_error()) { StoreError::NotFound => Ok(()), error => Err(error) }
}
