//! One-shot intake of the root launcher's key. No filesystem/config fallback.
//! Call only from gateway hardening, before any employee process is started.

use std::sync::OnceLock;
use zeroize::Zeroizing;

pub const KEY_PATH: &str = "/var/lib/duduclaw-protected/root.key";
const FD_ENV: &str = "DUDUCLAW_ROOT_KEY_FD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind { NotOffered, Held, Refused }

enum State {
    NotOffered,
    // The allocation stays at the same address after mlock and for process life.
    Held(Box<Zeroizing<[u8; 32]>>),
    Refused(&'static str),
}

static STATE: OnceLock<State> = OnceLock::new();

pub fn state_kind() -> StateKind {
    match STATE.get() {
        Some(State::Held(_)) => StateKind::Held,
        Some(State::Refused(_)) => StateKind::Refused,
        _ => StateKind::NotOffered,
    }
}

pub fn refused_reason() -> Option<&'static str> {
    match STATE.get() { Some(State::Refused(reason)) => Some(reason), _ => None }
}

pub fn is_held() -> bool { state_kind() == StateKind::Held }

/// Install a synthetic key in an isolated test process. Never available in release builds.
#[cfg(all(feature = "test-root-key", debug_assertions))]
#[doc(hidden)]
pub fn test_hold_root_key() {
    assert!(STATE.set(State::Held(Box::new(Zeroizing::new([0u8; 32])))).is_ok(),
        "root-key test requires a fresh process");
}

/// Borrow the key for one operation; the storage itself is never handed out.
pub fn with_key<T>(f: impl FnOnce(&[u8; 32]) -> T) -> Option<T> {
    match STATE.get() { Some(State::Held(key)) => Some(f(key)), _ => None }
}

/// Facts gathered from the kernel, kept separate for cross-platform tests.
#[derive(Debug, Clone)]
pub struct AttestationInputs {
    pub pid: u32,
    pub uids: Option<[u32; 3]>,
    pub gids: Option<[u32; 3]>,
    pub no_new_privs: Option<u64>,
    /// Inheritable, permitted, effective, bounding and ambient, in that order.
    pub capabilities: [Option<u64>; 5],
    pub dumpable: i32,
    pub key_path_denied: bool,
    /// False also means the socket paths or mount table could not be inspected.
    pub sockets_absent: bool,
    pub docker_host_absent: bool,
}

pub fn attest(i: &AttestationInputs) -> Result<(), &'static str> {
    if i.pid != 1 { return Err("not_pid_one"); }
    let equal_nonroot = |ids: Option<[u32; 3]>| {
        ids.is_some_and(|[real, effective, saved]| real != 0 && real == effective && real == saved)
    };
    if !equal_nonroot(i.uids) { return Err("user_ids"); }
    if !equal_nonroot(i.gids) { return Err("group_ids"); }
    if i.no_new_privs.is_none() || i.capabilities.iter().any(Option::is_none) {
        return Err("proc_status");
    }
    if i.no_new_privs != Some(1) { return Err("no_new_privs"); }
    for (value, reason) in i.capabilities.iter().zip(["cap_inh", "cap_prm", "cap_eff", "cap_bnd", "cap_amb"]) {
        if *value != Some(0) { return Err(reason); }
    }
    if i.dumpable != 0 { return Err("dumpable"); }
    if !i.key_path_denied { return Err("key_path_accessible"); }
    if !i.sockets_absent { return Err("engine_socket"); }
    if !i.docker_host_absent { return Err("docker_host"); }
    Ok(())
}

/// `hardening_failure` also closes the offered descriptor without reading it.
pub(crate) fn intake(hardening_failure: Option<&'static str>) {
    STATE.get_or_init(|| {
        let Some(offered) = std::env::var_os(FD_ENV) else { return State::NotOffered; };
        #[cfg(target_os = "linux")]
        let result = linux::take(&offered, hardening_failure);
        #[cfg(not(target_os = "linux"))]
        let result: Result<Box<Zeroizing<[u8; 32]>>, &'static str> = {
            let _ = (offered, hardening_failure);
            #[cfg(unix)]
            unsafe { libc::close(3); }
            Err("inactive")
        };
        match result {
            Ok(key) => State::Held(key),
            Err(reason) => {
                if reason == "mount_scan_incomplete" {
                    tracing::warn!("Mount table or mount points could not be inspected");
                }
                tracing::warn!(reason, "Container root key refused; personal accounts remain disabled");
                State::Refused(reason)
            }
        }
    });
}

/// Mountinfo escapes whitespace and backslashes in path fields with octal codes.
/// Inspect the source root (bind mounts retain it) and the source after `-` as
/// well as the destination. Abstract Unix sockets have no filesystem entry and
/// cannot be detected here, nor can sockets inside mounted directories. This
/// synchronous check runs once at key intake.
#[cfg(any(target_os = "linux", test))]
fn check_mountinfo(
    mountinfo: Option<&str>,
    mut inspect: impl FnMut(&std::path::Path) -> std::io::Result<(bool, bool)>,
) -> Result<(), &'static str> {
    let mountinfo = mountinfo.ok_or("mount_scan_incomplete")?;
    let mut has_mount = false;
    for line in mountinfo.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        let separator = fields.iter().position(|field| *field == "-")
            .ok_or("mount_scan_incomplete")?;
        if separator < 6 || fields.len() != separator + 4
            || fields[0].parse::<u64>().is_err() || fields[1].parse::<u64>().is_err() {
            return Err("mount_scan_incomplete");
        }
        let root = mountinfo_path(fields[3]).ok_or("mount_scan_incomplete")?;
        let path = mountinfo_path(fields[4]).ok_or("mount_scan_incomplete")?;
        let source = mountinfo_path(fields[separator + 2]).ok_or("mount_scan_incomplete")?;
        if !path.starts_with('/') || !root.starts_with('/') {
            return Err("mount_scan_incomplete");
        }
        if path.ends_with(".sock") || engine_directory(&path) {
            return Err("engine_socket");
        }
        match inspect(std::path::Path::new(&path)) {
            Ok((true, _)) => return Err("engine_socket"),
            // Docker's /etc/hosts, hostname and resolv.conf file binds can
            // originate under /var/lib/docker/containers; they expose no
            // directory. Source directory names matter for directory mounts.
            Ok((false, true)) if engine_source_directory(&root) || engine_source_directory(&source) => {
                return Err("engine_socket");
            }
            Ok((false, _)) => (),
            Err(_) => return Err("mount_scan_incomplete"),
        }
        has_mount = true;
    }
    if !has_mount { return Err("mount_scan_incomplete"); }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn engine_directory(path: &str) -> bool {
    path.split('/').any(|part| {
        ["docker", "podman", "containerd"].iter().any(|engine| {
            part == *engine || part.strip_prefix(*engine)
                .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
        })
    })
}

// Docker-managed data volumes and container configuration files are not engine
// control directories. Only the remaining source components are checked.
#[cfg(any(target_os = "linux", test))]
fn engine_source_directory(path: &str) -> bool {
    let path = path.strip_prefix("/var/lib/docker/volumes/")
        .or_else(|| path.strip_prefix("/var/lib/docker/containers/"))
        .unwrap_or(path);
    engine_directory(path)
}

#[cfg(any(target_os = "linux", test))]
fn mountinfo_path(field: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut bytes = field.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            if byte == 0 { return None; }
            decoded.push(byte);
            continue;
        }
        decoded.push(match [bytes.next()?, bytes.next()?, bytes.next()?] {
            [b'0', b'4', b'0'] => b' ',
            [b'0', b'1', b'1'] => b'\t',
            [b'0', b'1', b'2'] => b'\n',
            [b'1', b'3', b'4'] => b'\\',
            _ => return None,
        });
    }
    String::from_utf8(decoded).ok()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{ffi::OsStr, io, os::unix::fs::FileTypeExt};

    // Never retry close on Linux: the descriptor may already have been reused.
    struct CloseOffer;
    impl Drop for CloseOffer {
        fn drop(&mut self) { unsafe { libc::close(3); } }
    }

    pub(super) fn take(offered: &OsStr, failure: Option<&'static str>) -> Result<Box<Zeroizing<[u8; 32]>>, &'static str> {
        let _close = CloseOffer;
        if offered != "3" { return Err("fd_contract"); }
        if let Some(reason) = failure { return Err(reason); }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid writable stat storage; fd 3 is the fixed intake contract.
        if unsafe { libc::fstat(3, stat.as_mut_ptr()) } != 0 { return Err("fd_stat"); }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG { return Err("fd_type"); }
        if stat.st_uid != 0 { return Err("fd_owner"); }
        if stat.st_mode & 0o7777 != 0o400 { return Err("fd_mode"); }
        if stat.st_nlink != 1 { return Err("fd_links"); }
        if stat.st_size != 32 { return Err("fd_size"); }
        // Allocate before reading: no non-zeroizing temporary copy of the key.
        let mut key = Box::new(Zeroizing::new([0u8; 32]));
        let mut read = 0;
        while read < 32 {
            let remaining = &mut key[read..];
            let n = unsafe { libc::read(3, remaining.as_mut_ptr().cast(), remaining.len()) };
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted { continue; }
            if n <= 0 { return Err("fd_read"); }
            read += n as usize;
        }
        attest(&gather()?)?;
        // Best effort only: cap_drop ALL usually prevents locking large regions.
        unsafe { libc::mlock(key.as_ptr().cast(), 32); }
        Ok(key)
    }

    fn gather() -> Result<AttestationInputs, &'static str> {
        let (mut ruid, mut euid, mut suid) = (0, 0, 0);
        let (mut rgid, mut egid, mut sgid) = (0, 0, 0);
        let uids = (unsafe { libc::getresuid(&mut ruid, &mut euid, &mut suid) } == 0)
            .then_some([ruid, euid, suid]);
        let gids = (unsafe { libc::getresgid(&mut rgid, &mut egid, &mut sgid) } == 0)
            .then_some([rgid, egid, sgid]);
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let field = |name: &str, radix| -> Option<u64> {
            let mut values = status.lines().filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key == name).then_some(value.trim())
            });
            let value = u64::from_str_radix(values.next()?, radix).ok()?;
            if values.next().is_some() { return None; }
            Some(value)
        };
        let key_path_denied = std::fs::File::open(KEY_PATH).err()
            .is_some_and(|e| e.raw_os_error() == Some(libc::EACCES));
        let sockets_absent = ["/var/run/docker.sock", "/run/docker.sock", "/run/podman/podman.sock"]
            .iter().all(|p| matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == io::ErrorKind::NotFound));
        check_mountinfo(
            std::fs::read_to_string("/proc/self/mountinfo").ok().as_deref(),
            |path| std::fs::symlink_metadata(path).map(|metadata| (metadata.file_type().is_socket(), metadata.is_dir())),
        )?;
        Ok(AttestationInputs {
            pid: unsafe { libc::getpid() } as u32,
            uids, gids,
            no_new_privs: field("NoNewPrivs", 10),
            capabilities: ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"].map(|name| field(name, 16)),
            dumpable: unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) },
            key_path_denied, sockets_absent,
            docker_host_absent: std::env::var_os("DOCKER_HOST").is_none(),
        })
    }
}

#[cfg(test)]
#[path = "root_key_tests.rs"]
mod tests;
