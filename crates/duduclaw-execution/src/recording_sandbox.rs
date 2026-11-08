//! HX6 (D8 revision §4 [F10], §2.4): the nested Landlock the recording
//! helper installs on its node/Chromium child, stacked under the helper's own
//! layer (launcher ∩ helper ⊆ parent). Chromium cannot use its own sandbox
//! under the common filter (no user namespaces; NNP + nosuid block the setuid
//! sandbox), so this layer — plus F_L ∩ F_H — is the browser's containment:
//!
//! - filesystem: `/helper-tmp/<call>` RW (its own scratch only, no siblings),
//!   the signed node / Playwright bundle / Chromium files under `/helpers` RX,
//!   `/usr` `/lib*` RX, `/etc` `/proc` RO, `/dev/null` `/dev/urandom`. Never
//!   `/work`, `/home/employee`, `/config`, `/run`, `/tmp`, `/dev/shm`;
//! - network (ABI ≥ 4): TCP connect only to the parent's forwarder port, no
//!   TCP bind at all. Below ABI 4 the residual is loopback / abstract-UNIX
//!   reach to the runtime's local listeners (threat row T5);
//! - scoping (ABI ≥ 6): abstract-UNIX sockets and signals outside the
//!   browser's own domain are refused (T16 mitigation where available).
//!
//! Everything here is prepared before `fork`; the child's `pre_exec` only
//! issues `landlock_restrict_self`. Any failure fails the recording closed
//! (exit 78, `helper_recording_unsupported`): the browser never starts with a
//! wider authority.
use std::{ffi::CString, os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd}, path::Path};

const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_SYM: u64 = 1 << 12;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;
const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE;
pub const NET_BIND_TCP: u64 = 1 << 0;
pub const NET_CONNECT_TCP: u64 = 1 << 1;
const SCOPE_ABSTRACT_UNIX: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;
const RULE_PATH_BENEATH: u32 = 1;
const RULE_NET_PORT: u32 = 2;
/// Newest ABI whose rights this module names; newer kernels get this set.
const MAX_KNOWN_ABI: u32 = 7;

/// Filesystem rights the kernel of `abi` handles (all of them, so nothing the
/// rules do not grant stays possible), as in `duduclaw_run::landlock`.
pub fn handled_fs(abi: u32) -> Option<u64> {
    Some(match abi.min(MAX_KNOWN_ABI) {
        0 => return None,
        1 => (1 << 13) - 1,
        2 => (1 << 14) - 1,
        3 | 4 => (1 << 15) - 1,
        _ => (1 << 16) - 1,
    })
}
/// ABI ≥ 4: TCP bind and connect are handled (only the forwarder port is granted).
pub fn handled_net(abi: u32) -> u64 { if abi >= 4 { NET_BIND_TCP | NET_CONNECT_TCP } else { 0 } }
/// ABI ≥ 6: abstract-UNIX and signal scoping.
pub fn scoped(abi: u32) -> u64 { if abi >= 6 { SCOPE_ABSTRACT_UNIX | SCOPE_SIGNAL } else { 0 } }

/// Rule classes of the browser layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class { ReadExecute, ReadOnly, Scratch, Device }
pub fn rights(class: Class, directory: bool, handled: u64) -> u64 {
    let rights = match class {
        Class::ReadExecute => READ_FILE | READ_DIR | EXECUTE,
        Class::ReadOnly => READ_FILE | READ_DIR,
        Class::Scratch => READ_FILE | READ_DIR | EXECUTE | WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_DIR
            | MAKE_REG | MAKE_SYM | MAKE_SOCK | MAKE_FIFO | REFER | TRUNCATE,
        Class::Device => READ_FILE | WRITE_FILE,
    };
    let rights = rights & handled;
    if directory { rights } else { rights & FILE_RIGHTS }
}

/// The fixed rule list: (absolute path, class, required). Optional entries
/// (`/lib`, `/lib64`, `/lib32`) are skipped when absent or a symlink (merged
/// `/usr` on Ubuntu 22.04/24.04 and Debian 12: covered by `/usr`); a symlink is
/// never followed into a rule. Nothing else is negotiable.
/// `scratch` is `/helper-tmp/<call>`, `files` the signed helper files.
pub fn plan<'a>(scratch: &'a Path, files: &'a [&'a str]) -> Vec<(&'a Path, Class, bool)> {
    let mut rules: Vec<(&Path, Class, bool)> = vec![(scratch, Class::Scratch, true)];
    rules.extend(files.iter().map(|file| (Path::new(*file), Class::ReadExecute, true)));
    rules.extend([("/usr", Class::ReadExecute, true), ("/lib", Class::ReadExecute, false),
        ("/lib64", Class::ReadExecute, false), ("/lib32", Class::ReadExecute, false),
        ("/etc", Class::ReadOnly, true), ("/proc", Class::ReadOnly, true),
        ("/dev/null", Class::Device, true), ("/dev/urandom", Class::Device, true)]
        .map(|(path, class, required)| (Path::new(path), class, required)));
    rules
}

/// A prepared, not yet enforced, browser layer: one ruleset fd with every
/// rule added. `enforce` is the only call made in the forked child.
pub struct BrowserLayer { ruleset: OwnedFd, pub abi: u32 }

#[repr(C)]
struct RulesetAttr { handled_access_fs: u64, handled_access_net: u64, scoped: u64 }
#[repr(C, packed)]
struct PathBeneathAttr { allowed_access: u64, parent_fd: i32 }
#[repr(C)]
struct NetPortAttr { allowed_access: u64, port: u64 }

#[allow(unsafe_code)]
pub fn probe_abi() -> Option<u32> {
    // SAFETY: the VERSION query takes a null attr and size 0.
    let version = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, 1u32) };
    u32::try_from(version).ok().filter(|abi| *abi >= 1)
}

#[allow(unsafe_code)]
fn open_path(path: &Path) -> Option<(OwnedFd, bool)> {
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: terminated absolute path; O_PATH|O_NOFOLLOW never follows a final symlink.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return None; }
    // SAFETY: a successful open returns a fresh descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: zeroed stat buffer filled by the kernel for a live fd.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 { return None; }
    let kind = stat.st_mode & libc::S_IFMT;
    if kind == libc::S_IFLNK { return None; }
    Some((fd, kind == libc::S_IFDIR))
}

impl BrowserLayer {
    /// Build the layer for this host's ABI. `forwarder_port` is the parent's
    /// egress forwarder (`HTTPS_PROXY`, already checked to be 127.0.0.1).
    #[allow(unsafe_code)]
    pub fn prepare(scratch: &Path, files: &[&str], forwarder_port: u16) -> Result<Self, ()> {
        let abi = probe_abi().ok_or(())?;
        let handled = handled_fs(abi).ok_or(())?;
        let attr = RulesetAttr { handled_access_fs: handled, handled_access_net: handled_net(abi), scoped: scoped(abi) };
        // The kernel accepts a prefix of the attr: v1 = fs only, v4 adds net, v6 adds scoped.
        let size = if abi >= 6 { std::mem::size_of::<RulesetAttr>() } else if abi >= 4 { 16 } else { 8 };
        // SAFETY: attr is live and at least `size` bytes.
        let raw = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const RulesetAttr, size, 0u32) };
        if raw < 0 { return Err(()); }
        // SAFETY: a fresh ruleset descriptor owned only here.
        let ruleset = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
        let mut held = Vec::new();
        for (path, class, required) in plan(scratch, files) {
            let Some((fd, directory)) = open_path(path) else {
                if required { return Err(()); } else { continue; }
            };
            let rule = PathBeneathAttr { allowed_access: rights(class, directory, handled), parent_fd: fd.as_raw_fd() };
            // SAFETY: packed landlock_path_beneath_attr read synchronously.
            if unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr, 0u32) } != 0 { return Err(()); }
            held.push(fd);
        }
        if abi >= 4 {
            if forwarder_port == 0 { return Err(()); }
            // Connect only, to exactly the forwarder port. No bind rule: every TCP bind is refused.
            let rule = NetPortAttr { allowed_access: NET_CONNECT_TCP, port: u64::from(forwarder_port) };
            // SAFETY: landlock_net_port_attr read synchronously.
            if unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), RULE_NET_PORT,
                &rule as *const NetPortAttr, 0u32) } != 0 { return Err(()); }
        }
        drop(held);
        Ok(Self { ruleset, abi })
    }

    /// The ruleset descriptor for a `pre_exec` closure. Valid while `self`
    /// lives (the caller keeps the layer until `spawn` returned); the kernel
    /// creates it close-on-exec, so it never reaches node or Chromium.
    pub fn raw(&self) -> RawFd { self.ruleset.as_raw_fd() }
}

/// In the forked child (async-signal-safe: two syscalls, no allocation).
/// NNP is already set for the whole helper tree by the trampoline.
#[allow(unsafe_code)]
pub fn enforce_raw(ruleset: RawFd) -> std::io::Result<()> {
    // SAFETY: argument-free query of the caller's own flag.
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1 {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    // SAFETY: the parent keeps the ruleset fd open across fork; flags must be 0.
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_layer_rules_never_reach_work_or_runtime_reachable_paths() {
        let scratch = Path::new("/helper-tmp/0123456789abcdef0123456789abcdef");
        let files = ["/helpers/node", "/helpers/playwright.cjs", "/helpers/chromium"];
        let rules = plan(scratch, &files);
        for (path, class, _) in &rules {
            for forbidden in ["/work", "/home/employee", "/config", "/run", "/tmp", "/dev/shm", "/runtime"] {
                assert!(!path.starts_with(forbidden) && !Path::new(forbidden).starts_with(path), "{path:?}");
            }
            // Only its own call directory is writable; never `/helper-tmp` itself.
            assert_eq!(*class == Class::Scratch, *path == scratch, "{path:?}");
            assert_ne!(*path, Path::new("/helper-tmp"));
        }
        for abi in 1..=9 {
            let handled = handled_fs(abi).unwrap();
            for class in [Class::ReadExecute, Class::ReadOnly, Class::Scratch, Class::Device] {
                assert_eq!(rights(class, false, handled) & !FILE_RIGHTS, 0);
                assert_eq!(rights(class, true, handled) & !handled, 0);
            }
            assert_eq!(rights(Class::ReadOnly, true, handled) & (WRITE_FILE | EXECUTE), 0);
            assert_eq!(rights(Class::Device, false, handled) & EXECUTE, 0);
        }
        assert!(handled_fs(0).is_none());
    }

    /// ABI ≥ 4 handles TCP bind+connect (one connect rule = the forwarder
    /// port); below it there is nothing to pin (documented residual).
    #[test]
    fn browser_layer_pins_tcp_from_abi_4_and_scopes_from_abi_6() {
        for abi in 1..=3 { assert_eq!(handled_net(abi), 0); }
        for abi in 4..=9 { assert_eq!(handled_net(abi), NET_BIND_TCP | NET_CONNECT_TCP); }
        for abi in 1..=5 { assert_eq!(scoped(abi), 0); }
        for abi in 6..=9 { assert_eq!(scoped(abi), SCOPE_ABSTRACT_UNIX | SCOPE_SIGNAL); }
        assert_eq!(std::mem::size_of::<NetPortAttr>(), 16);
        assert_eq!(std::mem::size_of::<PathBeneathAttr>(), 12);
        assert_eq!(std::mem::size_of::<RulesetAttr>(), 24);
    }
}
