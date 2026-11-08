//! Per-slot helper cgroups, created and limited by root spawnd from OUTSIDE the
//! run unit (HX D8 revision §2.4 [F6], Q2 = A). The unit keeps
//! `ProtectControlGroups=yes`; nothing inside the run reaches cgroupfs by path.
//! The run only receives four root-opened descriptors in its bootstrap.
//!
//! Layout under the unit cgroup (`Delegate=yes`):
//! - `rt-init/`    supervisor, init, forwarder, launcher (spawnd moves the
//!   trampoline here before enabling controllers: no internal processes);
//! - `rt-runtime/` runtime and descendants (`cgroup.procs` + `cgroup.kill` fds);
//! - `hx/fg`, `hx/rec` one helper slot each, `memory.oom.group=1`, closed
//!   (`memory.max 0`, `pids.max 0`) until spawnd writes a binding's limits.
//!
//! Open-time credentials (CVE-2021-4197). A process moves itself through a
//! root-opened `cgroup.procs` fd. Before upstream 1756d7994ad8 (v5.16, backported
//! to some stable/distro kernels) the kernel checked the *writer's* credentials,
//! so an employee-uid write is refused; after it, the opener's. Distribution
//! backports make kernel version numbers unreliable, so this is detected by
//! behaviour, not by a version list: an unprivileged, capability-free probe
//! process writes `0` to a root-opened `hx/fg/cgroup.procs` fd. Exit 0 means the
//! kernel uses open-time credentials; `EACCES` means it does not, and every
//! helper on this host is refused with `helper_cgroup_unavailable`. The probe
//! exercises exactly the operation HX3b relies on, on the same hierarchy.
use std::io;

pub const RT_INIT: &str = "rt-init";
pub const RT_RUNTIME: &str = "rt-runtime";
pub const HX: &str = "hx";
pub const HX_FG: &str = "hx/fg";
pub const HX_REC: &str = "hx/rec";
pub const CONTROLLERS: [&str; 3] = ["memory", "pids", "cpu"];
/// Readiness reason and per-call typed refusal (D8 revision §5). The IPC
/// `ErrorCode::HelperCgroupUnavailable` variant is added with HX4 (`ipc.rs`).
pub const UNAVAILABLE_REASON: &str = "helper_cgroup_unavailable";
/// cgroup v2 `cpu.max` period used for helper slots.
pub const CPU_PERIOD_US: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupUnavailable {
    /// No writable delegated unit cgroup (spawnd sandbox, `Delegate=`, cgroup v1).
    NoDelegation,
    /// `cgroup.kill` is missing (kernel < 5.14).
    NoCgroupKill,
    /// memory, pids or cpu cannot be enabled for the subtree.
    MissingController,
    /// The unit cgroup did not contain exactly the waiting trampoline.
    UnexpectedProcesses,
    /// The kernel checks writer credentials (no CVE-2021-4197 fix).
    OpenTimeCredentialsMissing,
    /// A write or readback did not match, or a slot did not drain in time.
    Readback,
}
impl CgroupUnavailable { pub fn as_reason(self) -> &'static str { UNAVAILABLE_REASON } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot { Foreground, Recording }
impl Slot {
    pub fn path(self) -> &'static str { match self { Self::Foreground => HX_FG, Self::Recording => HX_REC } }
}

/// Root-side operations on the unit's cgroup directory. Paths are fixed,
/// relative, and never contain `..`; the Linux implementation resolves them
/// beneath the unit directory fd with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`.
pub trait CgroupTree {
    type Fd;
    fn mkdir(&mut self, relative: &str) -> io::Result<()>;
    fn exists(&mut self, relative: &str) -> io::Result<bool>;
    fn read(&mut self, relative: &str) -> io::Result<String>;
    fn write(&mut self, relative: &str, value: &str) -> io::Result<()>;
    /// `O_WRONLY | O_CLOEXEC` open by root; the descriptor carries root's open-time credentials.
    fn open_write(&mut self, relative: &str) -> io::Result<Self::Fd>;
}

/// Bootstrap order is `duduclaw_run::helper_policy::HELPER_CGROUP_FDS`.
pub struct LeafFds<F> { pub runtime_procs: F, pub runtime_kill: F, pub fg_procs: F, pub rec_procs: F }
impl<F> LeafFds<F> {
    pub fn into_bootstrap_order(self) -> [F; duduclaw_run::helper_policy::HELPER_CGROUP_FDS] {
        [self.runtime_procs, self.runtime_kill, self.fg_procs, self.rec_procs]
    }
}

fn pids(text: &str) -> Result<Vec<u32>, CgroupUnavailable> {
    text.lines().map(|line| {
        let line = line.trim();
        if line.is_empty() || !line.bytes().all(|b| b.is_ascii_digit()) { return Err(CgroupUnavailable::Readback); }
        line.parse::<u32>().ok().filter(|pid| *pid > 0).ok_or(CgroupUnavailable::Readback)
    }).collect()
}
fn words(text: &str) -> Vec<&str> { text.split_whitespace().collect() }
fn number(text: &str) -> Result<u64, CgroupUnavailable> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) { return Err(CgroupUnavailable::Readback); }
    text.parse().map_err(|_| CgroupUnavailable::Readback)
}
fn io_to(error: CgroupUnavailable) -> impl Fn(io::Error) -> CgroupUnavailable { move |_| error }

/// Create the leaves and move the waiting trampoline into `rt-init`. Must run
/// after the unit started and before its bootstrap plan is sent, while the
/// trampoline is the unit's only process. Slots are left open for the probe;
/// call [`close_slots`] afterwards.
pub fn create_leaves<T: CgroupTree>(tree: &mut T) -> Result<(), CgroupUnavailable> {
    use CgroupUnavailable::*;
    if !tree.exists("cgroup.kill").map_err(io_to(NoDelegation))? { return Err(NoCgroupKill); }
    let available = tree.read("cgroup.controllers").map_err(io_to(NoDelegation))?;
    if !CONTROLLERS.iter().all(|c| words(&available).contains(c)) { return Err(MissingController); }
    let before = pids(&tree.read("cgroup.procs").map_err(io_to(NoDelegation))?)?;
    if before.len() != 1 { return Err(UnexpectedProcesses); }
    for leaf in [RT_INIT, RT_RUNTIME, HX] { tree.mkdir(leaf).map_err(io_to(NoDelegation))?; }
    tree.write(&format!("{RT_INIT}/cgroup.procs"), &before[0].to_string()).map_err(io_to(UnexpectedProcesses))?;
    if !pids(&tree.read("cgroup.procs").map_err(io_to(Readback))?)?.is_empty()
        || pids(&tree.read(&format!("{RT_INIT}/cgroup.procs")).map_err(io_to(Readback))?)? != before {
        return Err(UnexpectedProcesses);
    }
    let enable = CONTROLLERS.map(|c| format!("+{c}")).join(" ");
    for parent in ["", HX] {
        let file = if parent.is_empty() { "cgroup.subtree_control".to_owned() } else { format!("{parent}/cgroup.subtree_control") };
        tree.write(&file, &enable).map_err(io_to(MissingController))?;
        let enabled = tree.read(&file).map_err(io_to(Readback))?;
        if !CONTROLLERS.iter().all(|c| words(&enabled).contains(c)) { return Err(MissingController); }
    }
    for slot in [HX_FG, HX_REC] {
        tree.mkdir(slot).map_err(io_to(NoDelegation))?;
        tree.write(&format!("{slot}/memory.oom.group"), "1").map_err(io_to(Readback))?;
        if number(&tree.read(&format!("{slot}/memory.oom.group")).map_err(io_to(Readback))?)? != 1 { return Err(Readback); }
    }
    Ok(())
}
/// Root-opened bootstrap descriptors. Open only after [`create_leaves`].
pub fn open_leaf_fds<T: CgroupTree>(tree: &mut T) -> Result<LeafFds<T::Fd>, CgroupUnavailable> {
    let mut open = |file: String| tree.open_write(&file).map_err(io_to(CgroupUnavailable::NoDelegation));
    Ok(LeafFds { runtime_procs: open(format!("{RT_RUNTIME}/cgroup.procs"))?,
        runtime_kill: open(format!("{RT_RUNTIME}/cgroup.kill"))?,
        fg_procs: open(format!("{HX_FG}/cgroup.procs"))?, rec_procs: open(format!("{HX_REC}/cgroup.procs"))? })
}

/// Root-written slot ceilings from a signed `HelperBinding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotLimits { pub memory_bytes: u64, pub tasks: u32, pub cpu_quota_percent: u32 }
impl SlotLimits {
    pub const CLOSED: Self = Self { memory_bytes: 0, tasks: 0, cpu_quota_percent: 1 };
    pub fn from_binding(binding: &crate::helper_policy::HelperBinding) -> Self {
        Self { memory_bytes: binding.memory_bytes, tasks: binding.tasks, cpu_quota_percent: binding.cpu_quota_percent }
    }
    fn cpu_max(self) -> String { format!("{} {CPU_PERIOD_US}", u64::from(self.cpu_quota_percent) * CPU_PERIOD_US / 100) }
}
/// Write and read back a slot's limits. HX4 calls this before every `Start`,
/// and refuses `Start` while the slot is still populated.
pub fn write_slot_limits<T: CgroupTree>(tree: &mut T, slot: Slot, limits: SlotLimits) -> Result<(), CgroupUnavailable> {
    use CgroupUnavailable::Readback;
    let base = slot.path();
    if limits.cpu_quota_percent == 0 { return Err(Readback); }
    for (file, value) in [("memory.swap.max", "0".to_owned()), ("memory.max", limits.memory_bytes.to_string()),
        ("pids.max", limits.tasks.to_string()), ("cpu.max", limits.cpu_max())] {
        tree.write(&format!("{base}/{file}"), &value).map_err(io_to(Readback))?;
    }
    // Kernel page rounding may narrow memory.max, never widen it.
    let memory = number(&tree.read(&format!("{base}/memory.max")).map_err(io_to(Readback))?)?;
    if memory > limits.memory_bytes || number(&tree.read(&format!("{base}/memory.swap.max")).map_err(io_to(Readback))?)? != 0
        || number(&tree.read(&format!("{base}/pids.max")).map_err(io_to(Readback))?)? != u64::from(limits.tasks)
        || words(&tree.read(&format!("{base}/cpu.max")).map_err(io_to(Readback))?) != words(&limits.cpu_max()) {
        return Err(Readback);
    }
    Ok(())
}
pub fn close_slots<T: CgroupTree>(tree: &mut T) -> Result<(), CgroupUnavailable> {
    for slot in [Slot::Foreground, Slot::Recording] { write_slot_limits(tree, slot, SlotLimits::CLOSED)?; }
    Ok(())
}
/// After every call, whatever the outcome: `cgroup.kill`, then wait for the
/// recursive `populated 0`. `sleep` is injected so the loop is host-testable.
pub fn kill_and_wait<T: CgroupTree>(tree: &mut T, slot: &str, mut attempts: u32,
    mut sleep: impl FnMut()) -> Result<(), CgroupUnavailable> {
    use CgroupUnavailable::Readback;
    if ![HX_FG, HX_REC, RT_RUNTIME].contains(&slot) { return Err(Readback); }
    tree.write(&format!("{slot}/cgroup.kill"), "1").map_err(io_to(Readback))?;
    loop {
        let events = tree.read(&format!("{slot}/cgroup.events")).map_err(io_to(Readback))?;
        if crate::systemd::cgroup_empty(&events).map_err(io_to(Readback))? { return Ok(()); }
        if attempts == 0 { return Err(Readback); }
        attempts -= 1;
        sleep();
    }
}
/// `OomKilled` is derived from this root-read counter's delta, never from a launcher frame [F5].
pub fn oom_kills(memory_events: &str) -> io::Result<u64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid memory.events");
    let mut found = None;
    for line in memory_events.lines() {
        let fields = words(line);
        if fields.len() != 2 { return Err(invalid()); }
        if fields[0] == "oom_kill" {
            if found.is_some() { return Err(invalid()); }
            found = Some(number(fields[1]).map_err(|_| invalid())?);
        }
    }
    found.ok_or_else(invalid)
}

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use std::{ffi::CString, io::Read, os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd}, path::Path, sync::Mutex, time::Duration};
    const CGROUP2_SUPER_MAGIC: i64 = 0x6367_7270;
    #[repr(C)]
    struct OpenHow { flags: u64, mode: u64, resolve: u64 }
    const RESOLVE: u64 = 0x08 | 0x04 | 0x02 | 0x01; // BENEATH, NO_SYMLINKS, NO_MAGICLINKS, NO_XDEV

    fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid cgroup path") }
    fn relative(path: &str) -> io::Result<CString> {
        if path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(invalid());
        }
        CString::new(path).map_err(|_| invalid())
    }
    /// The unit cgroup directory, opened by root spawnd in its own mount namespace.
    pub struct UnitTree { root: OwnedFd }
    impl UnitTree {
        /// `cgroup` is `StartedUnit::cgroup`, already canonical and symlink-free.
        pub fn open(cgroup: &Path) -> io::Result<Self> {
            if !cgroup.starts_with("/sys/fs/cgroup") { return Err(invalid()); }
            use std::os::unix::ffi::OsStrExt;
            let path = CString::new(cgroup.as_os_str().as_bytes()).map_err(|_| invalid())?;
            let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
            if raw < 0 { return Err(io::Error::last_os_error()); }
            let root = unsafe { OwnedFd::from_raw_fd(raw) };
            let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatfs(root.as_raw_fd(), &mut fs) } != 0 || fs.f_type as i64 != CGROUP2_SUPER_MAGIC {
                return Err(invalid());
            }
            Ok(Self { root })
        }
        fn open_at(&self, path: &str, flags: i32) -> io::Result<OwnedFd> {
            let name = relative(path)?;
            let how = OpenHow { flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64, mode: 0, resolve: RESOLVE };
            let fd = unsafe { libc::syscall(libc::SYS_openat2, self.root.as_raw_fd(), name.as_ptr(), &how, std::mem::size_of::<OpenHow>()) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
        }
    }
    impl CgroupTree for UnitTree {
        type Fd = OwnedFd;
        fn mkdir(&mut self, path: &str) -> io::Result<()> {
            let (parent, name) = match path.rsplit_once('/') { Some((parent, name)) => (Some(parent), name), None => (None, path) };
            let parent = match parent { Some(parent) => self.open_at(parent, libc::O_RDONLY | libc::O_DIRECTORY)?, None => self.root.try_clone()? };
            let name = relative(name)?;
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } != 0 { return Err(io::Error::last_os_error()); }
            Ok(())
        }
        fn exists(&mut self, path: &str) -> io::Result<bool> {
            match self.open_at(path, libc::O_PATH) {
                Ok(_) => Ok(true),
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(false),
                Err(error) => Err(error),
            }
        }
        fn read(&mut self, path: &str) -> io::Result<String> {
            let mut text = String::new();
            std::fs::File::from(self.open_at(path, libc::O_RDONLY)?).take(64 * 1024 + 1).read_to_string(&mut text)?;
            if text.len() > 64 * 1024 { return Err(invalid()); }
            Ok(text)
        }
        fn write(&mut self, path: &str, value: &str) -> io::Result<()> {
            let fd = self.open_at(path, libc::O_WRONLY)?;
            // cgroupfs parses one write(2) per value; a short write is a failure.
            let written = unsafe { libc::write(fd.as_raw_fd(), value.as_ptr().cast(), value.len()) };
            if written < 0 { return Err(io::Error::last_os_error()); }
            if written as usize != value.len() { return Err(invalid()); }
            Ok(())
        }
        fn open_write(&mut self, path: &str) -> io::Result<OwnedFd> { self.open_at(path, libc::O_WRONLY) }
    }

    static OPEN_TIME_CREDENTIALS: Mutex<Option<bool>> = Mutex::new(None);
    /// Host evidence for readiness: `None` until the first helper-enabled run probed.
    pub fn open_time_credentials_observed() -> Option<bool> { OPEN_TIME_CREDENTIALS.lock().ok().and_then(|value| *value) }
    /// Run the probe once per spawnd process (the kernel cannot change without a
    /// reboot, which restarts spawnd). Probe failures are not cached.
    pub fn require_open_time_credentials(tree: &mut UnitTree) -> Result<(), CgroupUnavailable> {
        let cached = *OPEN_TIME_CREDENTIALS.lock().map_err(|_| CgroupUnavailable::Readback)?;
        let supported = match cached {
            Some(value) => value,
            None => {
                let value = probe(tree)?;
                *OPEN_TIME_CREDENTIALS.lock().map_err(|_| CgroupUnavailable::Readback)? = Some(value);
                value
            }
        };
        if supported { Ok(()) } else { Err(CgroupUnavailable::OpenTimeCredentialsMissing) }
    }
    fn probe(tree: &mut UnitTree) -> Result<bool, CgroupUnavailable> {
        let fd = tree.open_write(&format!("{HX_FG}/cgroup.procs")).map_err(io_to(CgroupUnavailable::NoDelegation))?;
        let nonce = crate::server::random_id().map_err(io_to(CgroupUnavailable::Readback))?;
        let systemd = crate::systemd::linux::Systemd::connect().map_err(io_to(CgroupUnavailable::Readback))?;
        let status = systemd.run_cgroup_probe(nonce, fd.as_fd());
        drop(fd);
        // The probe may have joined the slot; drain it before the slot is used.
        kill_and_wait(tree, HX_FG, 100, || std::thread::sleep(Duration::from_millis(10)))?;
        match status.map_err(io_to(CgroupUnavailable::Readback))? {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(CgroupUnavailable::Readback),
        }
    }

    /// Spawnd-held state of one run's helper cgroups.
    pub struct RunCgroups { pub tree: UnitTree }
    /// Full sequence for a helper-enabled run: leaves, supervisor move, controllers,
    /// open-time-credentials gate, closed slots, then the four bootstrap fds.
    pub fn prepare_run(cgroup: &Path) -> Result<(RunCgroups, [OwnedFd; duduclaw_run::helper_policy::HELPER_CGROUP_FDS]), CgroupUnavailable> {
        let mut tree = UnitTree::open(cgroup).map_err(io_to(CgroupUnavailable::NoDelegation))?;
        create_leaves(&mut tree)?;
        require_open_time_credentials(&mut tree)?;
        close_slots(&mut tree)?;
        let fds = open_leaf_fds(&mut tree)?.into_bootstrap_order();
        Ok((RunCgroups { tree }, fds))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        /// Root on a cgroup v2 host with a disposable delegated scope. On a kernel
        /// without the CVE-2021-4197 fix the probe must answer "missing" and the
        /// helper path must refuse; with it, the unprivileged move must succeed.
        #[test]
        #[ignore = "iso_root_helper_: root Linux VM, cgroup v2, systemd >= 249; record the result per H15 host"]
        fn iso_root_helpers_refused_without_open_time_credentials() {
            assert_eq!(unsafe { libc::geteuid() }, 0);
            let systemd = crate::systemd::linux::Systemd::connect().unwrap();
            // A transient run-shaped unit gives a real delegated cgroup with one waiting process.
            let nonce = crate::server::random_id().unwrap();
            let run = duduclaw_isolation::ids::RunId(nonce);
            let (input, _writer) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).unwrap();
            let null = std::fs::OpenOptions::new().write(true).open("/dev/null").unwrap();
            let unit = systemd.start_test_delegated(run, input.as_fd(), null.as_fd()).unwrap();
            let result = (|| {
                let mut tree = UnitTree::open(&unit).map_err(|_| CgroupUnavailable::NoDelegation)?;
                create_leaves(&mut tree)?;
                let outcome = require_open_time_credentials(&mut tree);
                let observed = open_time_credentials_observed();
                eprintln!("open-time credentials observed: {observed:?}; kernel {}",
                    std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim());
                match observed {
                    Some(true) => { outcome?; close_slots(&mut tree)?; open_leaf_fds(&mut tree).map(|_| ()) }
                    Some(false) => { assert_eq!(outcome, Err(CgroupUnavailable::OpenTimeCredentialsMissing)); Ok(()) }
                    None => outcome,
                }
            })();
            let _ = systemd.kill_run(run);
            result.unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    /// In-memory model of the unit cgroup. Records every operation so the test
    /// can prove all work is relative to the unit and done by spawnd's tree.
    #[derive(Default)]
    struct Fake { files: BTreeMap<String, String>, dirs: Vec<String>, log: Vec<String>, opened: Vec<String> }
    impl Fake {
        fn unit() -> Self {
            let mut fake = Self::default();
            for (file, value) in [("cgroup.kill", ""), ("cgroup.controllers", "cpuset cpu io memory pids"),
                ("cgroup.procs", "4242\n"), ("cgroup.subtree_control", "")] { fake.files.insert(file.into(), value.into()); }
            fake
        }
    }
    impl CgroupTree for Fake {
        type Fd = String;
        fn mkdir(&mut self, path: &str) -> io::Result<()> {
            assert!(!path.starts_with('/') && !path.contains(".."));
            self.log.push(format!("mkdir {path}"));
            if let Some((parent, _)) = path.rsplit_once('/') { assert!(self.dirs.iter().any(|d| d == parent)); }
            self.dirs.push(path.into());
            for file in ["cgroup.procs", "cgroup.kill", "cgroup.subtree_control", "memory.oom.group",
                "memory.max", "memory.swap.max", "pids.max", "cpu.max"] { self.files.insert(format!("{path}/{file}"), String::new()); }
            self.files.insert(format!("{path}/cgroup.events"), "populated 0\nfrozen 0\n".into());
            Ok(())
        }
        fn exists(&mut self, path: &str) -> io::Result<bool> { Ok(self.files.contains_key(path)) }
        fn read(&mut self, path: &str) -> io::Result<String> {
            self.files.get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
        }
        fn write(&mut self, path: &str, value: &str) -> io::Result<()> {
            assert!(!path.starts_with('/') && !path.contains(".."));
            self.log.push(format!("write {path} {value}"));
            let entry = self.files.get_mut(path).ok_or(io::ErrorKind::NotFound)?;
            if path.ends_with("cgroup.subtree_control") {
                *entry = value.split_whitespace().map(|c| c.trim_start_matches('+')).collect::<Vec<_>>().join(" ");
            } else if path.ends_with("/cgroup.procs") {
                // A migration: the process leaves the unit root.
                *entry = format!("{value}\n");
                self.files.insert("cgroup.procs".into(), String::new());
            } else { *entry = value.into(); }
            Ok(())
        }
        fn open_write(&mut self, path: &str) -> io::Result<String> {
            assert!(self.files.contains_key(path));
            self.opened.push(path.into());
            Ok(path.into())
        }
    }
    #[test]
    fn cgroup_leaves_created_outside_unit() {
        let mut tree = Fake::unit();
        create_leaves(&mut tree).unwrap();
        close_slots(&mut tree).unwrap();
        let fds = open_leaf_fds(&mut tree).unwrap().into_bootstrap_order();
        assert_eq!(fds, ["rt-runtime/cgroup.procs", "rt-runtime/cgroup.kill", "hx/fg/cgroup.procs", "hx/rec/cgroup.procs"]);
        assert_eq!(tree.dirs, ["rt-init", "rt-runtime", "hx", "hx/fg", "hx/rec"]);
        // The waiting trampoline moves before any controller is enabled
        // (no-internal-process rule), and the unit root is then empty.
        let position = |needle: &str| tree.log.iter().position(|line| line.starts_with(needle)).unwrap();
        assert!(position("write rt-init/cgroup.procs 4242") < position("write cgroup.subtree_control"));
        assert!(position("write cgroup.subtree_control") < position("write hx/cgroup.subtree_control"));
        assert_eq!(tree.files["cgroup.procs"], "");
        for slot in [HX_FG, HX_REC] {
            assert_eq!(tree.files[&format!("{slot}/memory.oom.group")], "1");
            // Closed until spawnd writes a binding's limits for a call.
            assert_eq!(tree.files[&format!("{slot}/memory.max")], "0");
            assert_eq!(tree.files[&format!("{slot}/pids.max")], "0");
        }
        // Only the four bootstrap files are opened; no path crosses into the run.
        assert_eq!(tree.opened.len(), duduclaw_run::helper_policy::HELPER_CGROUP_FDS);
    }
    #[test]
    fn leaves_fail_closed_without_delegation_kill_or_single_process() {
        let mut tree = Fake::unit(); tree.files.remove("cgroup.kill");
        assert_eq!(create_leaves(&mut tree), Err(CgroupUnavailable::NoCgroupKill));
        let mut tree = Fake::unit(); tree.files.insert("cgroup.controllers".into(), "cpu io".into());
        assert_eq!(create_leaves(&mut tree), Err(CgroupUnavailable::MissingController));
        for procs in ["", "1\n2\n", "x\n"] {
            let mut tree = Fake::unit(); tree.files.insert("cgroup.procs".into(), procs.into());
            assert!(create_leaves(&mut tree).is_err(), "{procs:?}");
        }
        let mut tree = Fake::unit(); tree.files.remove("cgroup.procs");
        assert_eq!(create_leaves(&mut tree), Err(CgroupUnavailable::NoDelegation));
        assert_eq!(UNAVAILABLE_REASON, CgroupUnavailable::NoDelegation.as_reason());
    }
    #[test]
    fn slot_limits_kill_and_oom_counter() {
        let mut tree = Fake::unit();
        create_leaves(&mut tree).unwrap();
        let limits = SlotLimits { memory_bytes: 1 << 30, tasks: 64, cpu_quota_percent: 150 };
        write_slot_limits(&mut tree, Slot::Foreground, limits).unwrap();
        assert_eq!(tree.files["hx/fg/cpu.max"], "150000 100000");
        assert_eq!(tree.files["hx/fg/memory.swap.max"], "0");
        kill_and_wait(&mut tree, HX_FG, 0, || {}).unwrap();
        assert!(tree.log.iter().any(|line| line == "write hx/fg/cgroup.kill 1"));
        tree.files.insert("hx/rec/cgroup.events".into(), "populated 1\n".into());
        let mut slept = 0;
        assert_eq!(kill_and_wait(&mut tree, HX_REC, 3, || slept += 1), Err(CgroupUnavailable::Readback));
        assert_eq!(slept, 3);
        assert!(kill_and_wait(&mut tree, "rt-init", 0, || {}).is_err());
        assert_eq!(oom_kills("low 0\nhigh 0\nmax 2\noom 1\noom_kill 1\noom_group_kill 1\n").unwrap(), 1);
        for text in ["", "oom_kill x", "oom_kill 1\noom_kill 2", "oom_kill 1 2"] { assert!(oom_kills(text).is_err()); }
    }
}
