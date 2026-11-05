//! Descriptor-relative, private relay credentials and durable enrollment budget.
use std::{io, path::Path};
use serde::{Deserialize, Serialize};

// Intentionally no Debug; this is a bearer secret, never a diagnostic value.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credential { pub credential_id: String, pub secret: String }
impl Credential {
    pub(super) fn key(&self) -> Result<Vec<u8>, &'static str> {
        if self.credential_id.is_empty() || self.credential_id.len() > 128
            || !self.credential_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || self.secret.len() != 64 { return Err(super::DELIVERY_FAILED); }
        hex::decode(&self.secret).map_err(|_| super::DELIVERY_FAILED)
    }
}
impl Drop for Credential {
    fn drop(&mut self) { use zeroize::Zeroize; self.secret.zeroize(); }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnrollmentState {
    pub installation_id: String,
    pub last_enrollment_attempt: Option<i64>,
}
impl EnrollmentState {
    pub(super) fn new() -> Self {
        Self { installation_id: uuid::Uuid::new_v4().to_string(), last_enrollment_attempt: None }
    }
    pub(super) fn reserve(&mut self, now: i64) -> Result<(), &'static str> {
        // Backward wall-clock movement also fails closed. Persist BEFORE HTTP,
        // including failed enrollments, so restarts cannot reset the budget.
        if self.last_enrollment_attempt.is_some_and(|last| now.saturating_sub(last) < 3600) {
            return Err(super::DELIVERY_FAILED);
        }
        self.last_enrollment_attempt = Some(now);
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) struct Store {
    #[cfg(unix)]
    directory: std::fs::File,
    #[cfg(unix)]
    _lock: std::fs::File,
}
#[cfg(windows)]
#[path = "store_windows.rs"]
mod windows;
#[cfg(windows)]
pub(super) use windows::Store;

const CREDENTIAL_FILE: &str = "mfa-relay.json";
const STATE_FILE: &str = "mfa-relay-state.json";

#[cfg(unix)]
impl Store {
    pub(super) fn open(home: &Path) -> io::Result<Self> {
        use std::os::{fd::{AsRawFd, FromRawFd}, unix::fs::OpenOptionsExt};
        // As with other gateway state stores, ancestors of the configured home
        // are trusted. Pin home itself and refuse a symlink there or at state.
        let home = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(home)?;
        Self::validate(&home, true)?;
        if unsafe { libc::mkdirat(home.as_raw_fd(), c"state".as_ptr(), 0o700) } != 0
            && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { libc::openat(home.as_raw_fd(), c"state".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: openat returned a fresh, owned descriptor.
        let directory = unsafe { std::fs::File::from_raw_fd(fd) };
        Self::validate(&directory, true)?;
        home.sync_all()?;
        let lock = Self::open_at(&directory, "mfa-relay.lock", libc::O_RDWR | libc::O_CREAT)?;
        Self::validate(&lock, false)?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok(Self { directory, _lock: lock })
    }
    fn validate(file: &std::fs::File, directory: bool) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        #[cfg(target_os = "macos")]
        reject_macos_acl(file)?;
        let meta = file.metadata()?;
        if meta.uid() != unsafe { libc::geteuid() }
            || if directory { !meta.is_dir() || meta.mode() & 0o7022 != 0 }
                else { !meta.is_file() || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1 } {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    fn name(name: &str) -> io::Result<std::ffi::CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        std::ffi::CString::new(name).map_err(io::Error::other)
    }
    fn open_at(directory: &std::fs::File, name: &str, flags: i32) -> io::Result<std::fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = Self::name(name)?;
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: new descriptor, transferred once.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
        use std::io::Read;
        let file = match Self::open_at(&self.directory, name, libc::O_RDONLY) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Self::validate(&file, false)?;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 { return Err(io::ErrorKind::InvalidData.into()); }
        serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other)
    }
    fn write(&self, name: &str, value: &impl Serialize) -> io::Result<()> {
        use std::{io::Write, os::fd::AsRawFd};
        match Self::open_at(&self.directory, name, libc::O_RDONLY) {
            Ok(file) => Self::validate(&file, false)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(io::Error::other)?);
        if bytes.len() > 4096 { return Err(io::ErrorKind::InvalidData.into()); }
        let temp_name = format!(".mfa-relay-{}.tmp", uuid::Uuid::new_v4());
        let temp = Self::name(&temp_name)?;
        let target = Self::name(name)?;
        let result = (|| {
            let mut file = Self::open_at(&self.directory, &temp_name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            Self::validate(&file, false)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            if unsafe { libc::renameat(self.directory.as_raw_fd(), temp.as_ptr(), self.directory.as_raw_fd(), target.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.directory.sync_all()
        })();
        if result.is_err() { unsafe { libc::unlinkat(self.directory.as_raw_fd(), temp.as_ptr(), 0); } }
        result
    }
    pub(super) fn credential(&self) -> io::Result<Option<Credential>> { self.read(CREDENTIAL_FILE) }
    pub(super) fn save_credential(&self, credential: &Credential) -> io::Result<()> { self.write(CREDENTIAL_FILE, credential) }
    pub(super) fn state(&self) -> io::Result<EnrollmentState> {
        let state: EnrollmentState = self.read(STATE_FILE)?.unwrap_or_else(EnrollmentState::new);
        if uuid::Uuid::parse_str(&state.installation_id).is_err() { return Err(io::ErrorKind::InvalidData.into()); }
        Ok(state)
    }
    pub(super) fn save_state(&self, state: &EnrollmentState) -> io::Result<()> { self.write(STATE_FILE, state) }
}

// Never downgrade the credential store to path-based or public writes.
#[cfg(not(any(unix, windows)))]
impl Store {
    pub(super) fn open(_: &Path) -> io::Result<Self> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn credential(&self) -> io::Result<Option<Credential>> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn save_credential(&self, _: &Credential) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn state(&self) -> io::Result<EnrollmentState> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn save_state(&self, _: &EnrollmentState) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
}

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;

#[cfg(target_os = "macos")]
fn reject_macos_acl(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // Darwin's extended ACL API is in libc, but not exposed by the libc crate.
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(acl: *mut libc::c_void, id: libc::c_int, entry: *mut *mut libc::c_void) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // Darwin acl_get_fd returns the extended ACL, or ENOENT when none exists.
    let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) { return Ok(()) }
        return Err(error);
    }
    let mut entry = std::ptr::null_mut();
    let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let error = std::io::Error::last_os_error();
    unsafe { acl_free(acl); }
    if result == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "relay state must have no extended ACL entries"));
    }
    // Darwin reports EINVAL when the independent, valid ACL has no first entry.
    if result == -1 && error.raw_os_error() == Some(libc::EINVAL) { return Ok(()) }
    Err(error)
}


/// Called only on a blocking worker. Parallel logins wait within the overall
/// delivery deadline rather than immediately invalidating a freshly issued OTP.
pub(super) fn open_wait(home: &Path, timeout: std::time::Duration) -> io::Result<Store> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match Store::open(home) {
            Ok(store) => return Ok(store),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(20)));
            }
            Err(error) => return Err(error),
        }
    }
}
