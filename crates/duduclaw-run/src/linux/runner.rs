//! Single-threaded, one-shot launch engine. No stage has an unsandboxed fallback.
use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use duduclaw_isolation::{ids::Sha256Digest, ipc::InputFd};
use crate::{Error, Result, fd_plan::{Destination, FdPlan, Source}, landlock::Policy,
    seccomp::{self, Architecture, Filter, RuntimeProfile}, state::{Machine, Stage}, validation::{self, Expected, Identity}};
use super::{bootstrap, fd, mounts, privileges, probe};

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountIdentity { pub mount: u64, pub fs_type: i64 }
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub identity: Identity,
    pub script: Option<(Identity, CString)>,
    pub argv: Vec<CString>,
    pub environment: Vec<CString>,
    pub seccomp: RuntimeProfile,
    pub seccomp_digest: Sha256Digest,
}

/// Populated only by B1's signature-verified, root-owned registry adapter. Never
/// derive this object by deserializing a gateway SpawnPolicy or bootstrap offer.
/// fd order: five mounts, executable, optional script, prompt, attachments,
/// optional MCP, optional egress + forwarder ELF. stdout/stderr are fd 1/2.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedRun {
    pub root: mounts::RootConfig,
    pub mounts: [MountIdentity; 5],
    pub supervisor_uid: u32,
    pub supervisor_gid: u32,
    pub gateway_uid: u32,
    pub gateway_mount: u64,
    pub program: Program,
    pub forwarder: Option<Program>,
    pub prompt_bytes: u64,
    pub attachments: Vec<InputFd>,
    pub mcp: bool,
    pub proc_subset_pid: bool,
    #[serde(default)]
    pub stdin: bool,
    #[serde(default)]
    pub pty_size: Option<duduclaw_isolation::execution::PtySize>,
    /// HX in-run helper launcher (D8 revision §2.1, §5). Its descriptors follow
    /// the forwarder: control socket, helper mounts, per helper executable and
    /// optional script, then the `HELPER_CGROUP_FDS` root-opened cgroup files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherPlan>,
}

/// HX3b: `child_init` forks the in-trampoline launcher (D8 revision §2.1, Q1 = A).
pub const LAUNCHER_SUPPORTED: bool = true;
pub const MAX_HELPER_MOUNTS: usize = 4;
/// Upper bound of the signed launcher drain window F (§2.5 (b)).
pub const MAX_LAUNCHER_DRAIN_MS: u64 = 600_000;
/// Init's bound after SIGTERM to the launcher is F + this (§2.5 (c)).
pub const LAUNCHER_DRAIN_SLACK_MS: u64 = 2_000;
/// `FailurePage` isolation bits: runtime, forwarder, launcher.
pub(super) const LAUNCHER_ISOLATED: u32 = 4;

/// Root-signed helper table for one run. The index of a `HelperProgram` is the
/// `helper` byte of a launcher `Start` frame; nothing here comes from IPC.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherPlan {
    pub helpers: Vec<HelperProgram>,
    /// Exactly one helper registry clone (HX2 `prepare_helpers`); `/helpers` is
    /// built from per-file binds of it (`mounts::HelperView`).
    pub mounts: Vec<MountIdentity>,
    pub tmpfs_bytes: u64,
    /// Drain window F = parent `termination_grace_ms` − 2 s, at least 1 s (§2.5 (b)).
    pub drain_ms: u64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperProgram {
    /// `seccomp` is the signed flat F_H allowlist; `self_only` its argument-constrained exceptions.
    pub program: Program,
    pub self_only: Vec<crate::helper_policy::SelfOnlyCall>,
    /// Index into `LauncherPlan::mounts`.
    pub mount: u8,
    pub slot: crate::helper_policy::HelperSlot,
    pub filesystem: Vec<crate::helper_policy::HelperFs>,
    pub open_files: u32,
    pub file_size_bytes: u64,
    pub timeout_ms: u64,
    pub egress: bool,
}
impl LauncherPlan {
    pub fn fd_count(&self) -> usize {
        1 + self.mounts.len() + self.helpers.iter().map(|h| 1 + usize::from(h.program.script.is_some())).sum::<usize>()
            + crate::helper_policy::HELPER_CGROUP_FDS
    }
    /// `parent` is the run's runtime program: F_L and every F_H are derived from
    /// its signed profile, so they are checked here, before any fd is touched.
    fn validate(&self, parent: &Program) -> Result<()> {
        use crate::helper_policy::*;
        if self.helpers.is_empty() || self.helpers.len() > MAX_HELPER_BINDINGS || self.mounts.len() != 1
            || self.tmpfs_bytes == 0 || self.tmpfs_bytes > 1024 * 1024 * 1024
            || self.drain_ms == 0 || self.drain_ms > MAX_LAUNCHER_DRAIN_MS { return Err(Error::Executable); }
        let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
        // HX3a: F_L adds no kill/dup3/file/socket calls; refuse a parent without them.
        if !seccomp::parent_supports_launcher(arch, &parent.seccomp) { return Err(Error::Seccomp); }
        seccomp::launcher(arch, &parent.seccomp, parent.seccomp_digest).map_err(|_| Error::Seccomp)?;
        // One registry name maps to one signed file identity across the table.
        let mut names: Vec<(String, Identity)> = Vec::new();
        let mut name = |path: &[u8], identity: Identity| -> Result<()> {
            let name = helper_file_name(path).ok_or(Error::Executable)?;
            match names.iter().find(|(known, _)| known == name) {
                Some((_, known)) if *known != identity => Err(Error::Executable),
                Some(_) => Ok(()),
                None => { names.push((name.to_owned(), identity)); Ok(()) }
            }
        };
        for helper in &self.helpers {
            name(helper.program.argv.first().ok_or(Error::Executable)?.as_bytes(), helper.program.identity)?;
            if let Some((identity, path)) = &helper.program.script {
                name(format!("/helpers/{}", path.to_str().map_err(|_| Error::Executable)?).as_bytes(), *identity)?;
            }
            seccomp::helper(arch, &parent.seccomp, parent.seccomp_digest, &helper.program.seccomp,
                helper.program.seccomp_digest, &helper.self_only).map_err(|_| Error::Seccomp)?;
        }
        for helper in &self.helpers {
            let mut grants = helper.filesystem.clone();
            grants.sort_unstable(); grants.dedup();
            if usize::from(helper.mount) >= self.mounts.len() || grants.len() != helper.filesystem.len()
                || grants.is_empty() || !grants.iter().all(|grant| helper_fs_within_runtime(*grant))
                || helper.timeout_ms == 0 || helper.timeout_ms > HELPER_TIMEOUT_MAX_MS || helper.open_files == 0
                || helper.file_size_bytes == 0 || (helper.slot == HelperSlot::Recording) != helper.egress {
                return Err(Error::Executable);
            }
            // Helper paths live under /helpers (HX3b), never the runtime's /runtime.
            let program = &helper.program;
            if program.argv.is_empty() || program.argv.len() > 128 || !program.environment.is_empty()
                || !program.argv[0].as_bytes().starts_with(b"/helpers/")
                || program.argv.iter().any(|s| s.as_bytes().len() > 4096) { return Err(Error::Executable); }
            if let Some((_, path)) = &program.script {
                let relative = path.to_str().map_err(|_| Error::Executable)?;
                if relative.is_empty() || relative.split('/').any(|p| p.is_empty() || p == "." || p == "..")
                    || program.argv.get(1).map(|v| v.as_bytes()) != Some(format!("/helpers/{relative}").as_bytes()) {
                    return Err(Error::Executable);
                }
            }
            let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
            seccomp::runtime(arch, &program.seccomp, program.seccomp_digest).map_err(|_| Error::Seccomp)?;
            if helper.self_only.len() > 2 { return Err(Error::Seccomp); }
        }
        Ok(())
    }
}
impl TrustedRun {
    pub fn expected_fd_count(&self) -> usize {
        5 + 1 + usize::from(self.program.script.is_some()) + 1 + self.attachments.len()
            + usize::from(self.stdin) + usize::from(self.mcp) + if self.forwarder.is_some() { 2 } else { 0 }
            + self.launcher.as_ref().map_or(0, LauncherPlan::fd_count)
    }
    pub fn validate(&self) -> Result<()> {
        self.root.validate().map_err(|_| Error::Mount)?;
        if self.pty_size.is_some() != self.root.pty || self.pty_size.is_some_and(|size| size.validate().is_err()) {
            return Err(Error::Descriptors);
        }
        if self.stdin && self.root.pty { return Err(Error::Descriptors); }
        let uid = self.root.employee_uid;
        let gid = self.root.employee_gid;
        if self.supervisor_uid == 0 || self.supervisor_uid >= 1000 || self.supervisor_gid == 0
            || self.supervisor_gid >= 1000 || self.gateway_uid == 0 || self.gateway_uid == self.supervisor_uid
            || uid == self.gateway_uid || uid == self.supervisor_uid || gid == self.supervisor_gid
            || duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.iter()
                .any(|range| (range.first..=range.last).contains(&uid) || (range.first..=range.last).contains(&gid)) {
            return Err(Error::Privileges);
        }
        if self.expected_fd_count() > bootstrap::MAX_BOOTSTRAP_FDS { return Err(Error::DescriptorLimit); }
        let sizes: Vec<_> = self.attachments.iter().map(|input| match input {
            InputFd::SealedMemfd { bytes } => (*bytes, true), InputFd::ReadOnlySnapshot { bytes } => (*bytes, false),
        }).collect();
        validation::validate_input_budget(self.prompt_bytes, &sizes)?;
        validate_program(&self.program)?;
        if let Some(program) = &self.forwarder {
            if program.script.is_some() { return Err(Error::Executable); }
            validate_program(program)?;
        }
        if let Some(launcher) = &self.launcher { launcher.validate(&self.program)?; }
        Ok(())
    }
}

fn validate_program(program: &Program) -> Result<()> {
    if program.argv.is_empty() || program.argv.len() > 128 || program.environment.len() > 128
        || program.argv[0].as_bytes().is_empty()
        || program.argv.iter().chain(&program.environment).any(|s| s.as_bytes().len() > 4096) {
        return Err(Error::Executable);
    }
    if let Some((_, path)) = &program.script {
        let relative = path.to_str().map_err(|_| Error::Executable)?;
        if relative.is_empty() || relative.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(Error::Executable);
        }
        let path_arg = format!("/runtime/{relative}");
        if program.argv.get(1).map(|v| v.as_bytes()) != Some(path_arg.as_bytes()) { return Err(Error::Executable); }
    }
    let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
    seccomp::runtime(arch, &program.seccomp, program.seccomp_digest).map_err(|_| Error::Seccomp)?;
    Ok(())
}

struct Inputs {
    mounts: mounts::MountInputs,
    exec: OwnedFd, script: Option<OwnedFd>, prompt: OwnedFd,
    attachments: Vec<OwnedFd>, stdin: Option<OwnedFd>, mcp: Option<OwnedFd>, forwarder: Option<(OwnedFd, OwnedFd)>,
    launcher: Option<LauncherInputs>,
}
/// `cgroups` order is `helper_policy::HELPER_CGROUP_FDS`.
struct LauncherInputs {
    control: OwnedFd, registry: mounts::DetachedMount, helpers: Vec<(OwnedFd, Option<OwnedFd>)>,
    cgroups: [OwnedFd; crate::helper_policy::HELPER_CGROUP_FDS],
}
/// Root-opened `rt-runtime` files: the runtime child moves itself with `procs`
/// before exec; init writes `kill` the moment it reaps the runtime [F4].
pub(super) struct RuntimeCgroup { procs: OwnedFd, kill: OwnedFd }
const CGROUP2_SUPER_MAGIC: i64 = 0x6367_7270;
/// A root-opened, write-only cgroup v2 interface file. Moves through it rely on
/// open-time credentials (CVE-2021-4197 fix); spawnd probes that before planning.
fn verify_cgroup_file(fd: RawFd) -> Result<()> {
    let o = fd::observe(fd)?;
    if o.kind != validation::FileKind::Regular || o.access != validation::Access::Write || !o.cloexec
        || o.fs_type != CGROUP2_SUPER_MAGIC || o.uid != 0 { return Err(Error::Descriptors); }
    Ok(())
}
fn take_launcher(plan: &LauncherPlan, fds: &mut impl Iterator<Item = OwnedFd>, mcp: Option<&OwnedFd>) -> Result<LauncherInputs> {
    let control = fds.next().ok_or(Error::Descriptors)?;
    if fd::socket_option(control.as_raw_fd(), libc::SO_DOMAIN)? != libc::AF_UNIX
        || fd::socket_option(control.as_raw_fd(), libc::SO_TYPE)? != libc::SOCK_SEQPACKET { return Err(Error::Descriptors); }
    if let Some(mcp) = mcp {
        if fd::observe(control.as_raw_fd())?.identity == fd::observe(mcp.as_raw_fd())?.identity { return Err(Error::Descriptors); }
    }
    let mut registry = None;
    for expected in &plan.mounts {
        let fd = fds.next().ok_or(Error::Descriptors)?;
        validation::validate(fd::observe(fd.as_raw_fd())?, Expected::Mount {
            mount: expected.mount, fs_type: expected.fs_type, required_flags: 1 | 2 | 4 })?;
        if registry.is_some() { return Err(Error::Descriptors); }
        registry = Some(mounts::DetachedMount { fd, expected_mount_id: expected.mount, expected_fs_type: expected.fs_type });
    }
    let registry = registry.ok_or(Error::Descriptors)?;
    let mut helpers = Vec::new();
    for helper in &plan.helpers {
        let registry = plan.mounts.get(usize::from(helper.mount)).ok_or(Error::Descriptors)?.mount;
        let exec = fds.next().ok_or(Error::Descriptors)?;
        verify_executable(exec.as_raw_fd(), helper.program.identity, registry)?;
        let script = if let Some((identity, _)) = &helper.program.script {
            let script = fds.next().ok_or(Error::Descriptors)?;
            validation::validate(fd::observe(script.as_raw_fd())?, Expected::Script { identity: *identity, registry_mount: registry })?;
            Some(script)
        } else { None };
        helpers.push((exec, script));
    }
    let mut cgroup = || -> Result<OwnedFd> {
        let fd = fds.next().ok_or(Error::Descriptors)?; verify_cgroup_file(fd.as_raw_fd())?; Ok(fd)
    };
    let cgroups = [cgroup()?, cgroup()?, cgroup()?, cgroup()?];
    let identities: Vec<_> = cgroups.iter().map(|fd| fd::observe(fd.as_raw_fd()).map(|o| o.identity)).collect::<Result<_>>()?;
    if identities.iter().enumerate().any(|(i, a)| identities[..i].contains(a)) { return Err(Error::Descriptors); }
    Ok(LauncherInputs { control, registry, helpers, cgroups })
}

fn take_inputs(plan: &TrustedRun, descriptors: Vec<OwnedFd>) -> Result<Inputs> {
    if descriptors.len() != plan.expected_fd_count() { return Err(Error::Descriptors); }
    let mut fds = descriptors.into_iter();
    let mut mount = |index: usize| -> Result<mounts::DetachedMount> {
        let fd = fds.next().ok_or(Error::Descriptors)?;
        let expected = plan.mounts[index];
        let flags = if index <= 1 { 2 | 4 } else { 1 | 2 | 4 };
        validation::validate(fd::observe(fd.as_raw_fd())?, Expected::Mount {
            mount: expected.mount, fs_type: expected.fs_type, required_flags: flags })?;
        Ok(mounts::DetachedMount { fd, expected_mount_id: expected.mount, expected_fs_type: expected.fs_type })
    };
    let mounts = mounts::MountInputs { home: mount(0)?, work: mount(1)?, projection: mount(2)?, runtime: mount(3)?, etc: mount(4)? };
    let exec = fds.next().ok_or(Error::Descriptors)?;
    verify_executable(exec.as_raw_fd(), plan.program.identity, plan.mounts[3].mount)?;
    let script = if let Some((identity, _)) = &plan.program.script {
        let script = fds.next().ok_or(Error::Descriptors)?;
        validation::validate(fd::observe(script.as_raw_fd())?, Expected::Script { identity: *identity, registry_mount: plan.mounts[3].mount })?;
        Some(script)
    } else { None };
    let prompt = fds.next().ok_or(Error::Descriptors)?;
    validation::validate(fd::observe(prompt.as_raw_fd())?, Expected::Prompt { bytes: plan.prompt_bytes })?;
    if unsafe { libc::lseek(prompt.as_raw_fd(), 0, libc::SEEK_SET) } != 0 { return Err(Error::Descriptors); }
    let mut attachments = Vec::new();
    let mut actual_sizes = Vec::new();
    for input in &plan.attachments {
        let fd = fds.next().ok_or(Error::Descriptors)?;
        let o = fd::observe(fd.as_raw_fd())?;
        let expected = match input {
            InputFd::SealedMemfd { bytes } => Expected::AttachmentSealed { bytes: *bytes },
            InputFd::ReadOnlySnapshot { bytes } => Expected::Snapshot { bytes: *bytes,
                gateway_uid: plan.gateway_uid, gateway_mount: plan.gateway_mount },
        };
        validation::validate(o, expected)?;
        if unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } != 0 { return Err(Error::Descriptors); }
        actual_sizes.push((o.size, matches!(input, InputFd::SealedMemfd { .. })));
        attachments.push(fd);
    }
    validation::validate_input_budget(fd::observe(prompt.as_raw_fd())?.size, &actual_sizes)?;
    let stdin = if plan.stdin {
        let stdin = fds.next().ok_or(Error::Descriptors)?;
        let observed = fd::observe(stdin.as_raw_fd())?;
        if observed.kind != validation::FileKind::Fifo || observed.access != validation::Access::Read
            || !observed.cloexec { return Err(Error::Descriptors); }
        Some(stdin)
    } else { None };
    let mcp = if plan.mcp {
        let mcp = fds.next().ok_or(Error::Descriptors)?; fd::socketpair_endpoint(mcp.as_raw_fd())?; Some(mcp)
    } else { None };
    let forwarder = if let Some(program) = &plan.forwarder {
        let socket = fds.next().ok_or(Error::Descriptors)?; fd::socketpair_endpoint(socket.as_raw_fd())?;
        if let Some(mcp) = &mcp {
            if fd::observe(socket.as_raw_fd())?.identity == fd::observe(mcp.as_raw_fd())?.identity { return Err(Error::Descriptors); }
        }
        let exec = fds.next().ok_or(Error::Descriptors)?;
        verify_executable(exec.as_raw_fd(), program.identity, plan.mounts[3].mount)?;
        Some((socket, exec))
    } else { None };
    let launcher = match &plan.launcher {
        Some(launcher) => Some(take_launcher(launcher, &mut fds, mcp.as_ref())?),
        None => None,
    };
    if fds.next().is_some() { return Err(Error::Descriptors); }
    Ok(Inputs { mounts, exec, script, prompt, attachments, stdin, mcp, forwarder, launcher })
}

pub(super) fn verify_executable(fd: RawFd, identity: Identity, registry_mount: u64) -> Result<()> {
    validation::validate(fd::observe(fd)?, Expected::Executable { identity, registry_mount })?;
    let mut magic = [0u8; 4];
    if unsafe { libc::pread(fd, magic.as_mut_ptr().cast(), magic.len(), 0) } != 4 || magic != *b"\x7fELF" {
        return Err(Error::Executable);
    }
    Ok(())
}

/// The caller must be a new SINGLE-THREADED root trampoline with fd 0 bootstrap
/// and fd 1/2 bounded output pipes. Never invoke from an application/test thread.
/// This function never returns: all failures send a fixed record then _exit.
///
/// # Safety
/// Owns the entire process fd table, credentials, namespaces and signal handlers.
pub unsafe fn serve(registry: &mut dyn bootstrap::RootRegistry) -> ! {
    let mut machine = Machine::default();
    let result = (|| -> Result<()> {
        privileges::non_dumpable()?; machine.advance(Stage::NonDumpable)?;
        fd::close_range(3, u32::MAX)?; machine.advance(Stage::InheritedClosed)?;
        bootstrap::verify_peer(0)?; machine.advance(Stage::BootstrapVerified)?;
        if std::env::args_os().count() != 1 { return Err(Error::Bootstrap); }
        for output in [1,2] {
            fd::set_cloexec(output)?;
            validation::validate(fd::observe(output)?, Expected::Output)?;
        }
        privileges::require_root_capabilities()?;
        let ready = probe::probe()?;
        let (plan, fds) = bootstrap::handshake(0, registry)?;
        // Never parse employee-controlled data in this root process.
        launch(plan, fds, ready, machine)
    })();
    if let Err(error) = result { let _ = bootstrap::send_packet(0, &bootstrap::error_status(error), None); }
    exit_now(125)
}

fn launch(plan: TrustedRun, fds: Vec<OwnedFd>, ready: probe::Readiness, mut machine: Machine) -> Result<()> {
    plan.validate()?;
    let n = 3 + 1 + 1 + 1 + plan.attachments.len() + 1 + usize::from(plan.program.script.is_some());
    if fd::soft_limit()? < FdPlan::required_limit(n)? { return Err(Error::DescriptorLimit); }
    let input = take_inputs(&plan, fds)?;
    machine.advance(Stage::DescriptorsVerified)?;
    mounts::enter_namespaces().map_err(|_| Error::Namespace)?;
    machine.advance(Stage::Namespaces)?; machine.advance(Stage::PrivatePropagation)?;
    // [F2] Only this process (the future supervisor, outside the run's PID
    // namespace) is OOM-protected; host procfs is still visible before pivot.
    // Init inherits the value and resets it to 0 before it forks anything.
    privileges::protect_supervisor_from_oom()?;
    let Inputs { mounts: mount_inputs, exec, script, prompt, attachments, stdin, mcp, forwarder, launcher } = input;
    let mut root = mounts::build_root(mount_inputs, &plan.root).map_err(|_| Error::Mount)?;
    // HX: `/helpers`, `/helper-tmp` and the `/runtime` masks (mounts::HelperView).
    let launcher = match (launcher, &plan.launcher) {
        (Some(inputs), Some(launcher_plan)) => {
            let LauncherInputs { control, registry, helpers, cgroups } = inputs;
            let mut files = Vec::new();
            for (helper, (helper_exec, helper_script)) in launcher_plan.helpers.iter().zip(&helpers) {
                let name = crate::helper_policy::helper_file_name(helper.program.argv[0].as_bytes()).ok_or(Error::Executable)?;
                files.push((CString::new(name).map_err(|_| Error::Executable)?, helper_exec.as_raw_fd()));
                match (&helper.program.script, helper_script) {
                    (Some((_, path)), Some(fd)) => files.push((path.clone(), fd.as_raw_fd())),
                    (None, None) => {}
                    _ => return Err(Error::Executable),
                }
            }
            let mut keep = vec![exec.as_raw_fd()];
            keep.extend(script.as_ref().map(AsRawFd::as_raw_fd));
            keep.extend(forwarder.as_ref().map(|(_, forward_exec)| forward_exec.as_raw_fd()));
            let readback = root.attach_helpers(mounts::HelperView { registry, files, keep,
                tmpfs_bytes: launcher_plan.tmpfs_bytes }, &plan.root).map_err(|_| Error::Mount)?;
            let [procs, kill, foreground, recording] = cgroups;
            Some((super::launcher::Prepared { control, helpers, slots: [foreground, recording], binds: readback.binds },
                RuntimeCgroup { procs, kill }))
        }
        (None, None) => None,
        _ => return Err(Error::Descriptors),
    };
    let pty = root.take_pty();
    let slave = if let Some(pty) = pty {
        let size = plan.pty_size.ok_or(Error::Descriptors)?;
        let value = libc::winsize { ws_row: size.rows, ws_col: size.columns, ws_xpixel: 0, ws_ypixel: 0 };
        let mut actual: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCSWINSZ, &value) } != 0
            || unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCGWINSZ, &mut actual) } != 0
            || actual.ws_row != size.rows || actual.ws_col != size.columns { return Err(Error::Descriptors); }
        validation::validate(fd::observe(pty.slave.as_raw_fd())?, Expected::Pty { mount: pty.mount_id })?;
        bootstrap::send_packet(0, &crate::status::StatusRecord::PtyIdentity { mount_id: pty.mount_id }.encode()?, Some(pty.master.as_raw_fd()))?;
        drop(pty.master); Some(pty.slave)
    } else { None };
    machine.advance(Stage::RootBuilt)?;
    let root_readback = root.pivot().map_err(|_| Error::Mount)?;
    machine.advance(Stage::Pivoted)?;
    if root_readback.runtime_mount_id != plan.program.identity.mount { return Err(Error::Executable); }
    mounts::bring_loopback_up().map_err(|_| Error::Namespace)?; machine.advance(Stage::Loopback)?;
    let mut pipe = [-1; 2];
    super::checked(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_DIRECT) }, Error::Process)?;
    let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let failure = FailurePage::new()?;
    super::checked(unsafe { libc::unshare(libc::CLONE_NEWPID) }, Error::Namespace)?;
    let mut pidfd = -1;
    let mut args = CloneArgs::default(); args.flags = libc::CLONE_PIDFD as u64;
    args.pidfd = (&mut pidfd as *mut i32) as u64; args.exit_signal = libc::SIGCHLD as u64;
    let child = unsafe { libc::syscall(libc::SYS_clone3, &args, std::mem::size_of_val(&args)) };
    if child < 0 { return Err(Error::Process); }
    machine.advance(Stage::PidNamespace)?;
    if child > 0 {
        let status = read.into_raw_fd();
        let supervisor_identity = (plan.supervisor_uid, plan.supervisor_gid);
        // The parent never needs heap state again. Do not drop fd owners after
        // sweeping descriptors: a reused descriptor must never be closed twice.
        std::mem::forget((plan, exec, script, prompt, attachments, stdin, mcp, forwarder, slave, write, launcher));
        supervisor(status, pidfd, machine, supervisor_identity, failure)
    } else {
        drop(read);
        super::checked(unsafe { libc::close(0) }, Error::Descriptors)?;
        let status = write.as_raw_fd();
        if let Err(error) = failure.await_supervisor() {
            failure.record(error); write_status(status, bootstrap::error_status(error)); exit_now(125);
        }
        let result = child_init(plan, exec, script, prompt, attachments, stdin, mcp, forwarder,
            slave, launcher, ready.landlock_ruleset_abi, status, machine, failure);
        if let Err(error) = result { failure.record(error); write_status(status, bootstrap::error_status(error)); }
        exit_now(125)
    }
}

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64, pidfd: u64, child_tid: u64, parent_tid: u64, exit_signal: u64,
    stack: u64, stack_size: u64, tls: u64, set_tid: u64, set_tid_size: u64, cgroup: u64,
}

/// A fixed error word bridges the interval after the exact fd sweep and before
/// exec. It is not a capability and contains no text. Successful exec destroys
/// the anonymous mapping, so runtime code cannot modify it. It intentionally
/// lives until process exit; no munmap is needed under the supervisor filter.
/// The HX launcher is never exec'd, so it unmaps its copy after marking [F12].
#[derive(Clone, Copy)]
pub(super) struct FailurePage(*const SharedStatus);
#[repr(C)]
struct SharedStatus { error: AtomicU32, supervisor_ready: AtomicU32, isolated: AtomicU32 }
impl FailurePage {
    fn new() -> Result<Self> {
        let page = unsafe { libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS, -1, 0) };
        if page == libc::MAP_FAILED { return Err(Error::Process); }
        let word = page.cast::<SharedStatus>();
        unsafe { word.write(SharedStatus { error: AtomicU32::new(0), supervisor_ready: AtomicU32::new(0), isolated: AtomicU32::new(0) }); }
        Ok(Self(word))
    }
    pub(super) fn record(self, error: Error) {
        let _ = unsafe { &*self.0 }.error.compare_exchange(0, u32::from(bootstrap::error_code(error)), Ordering::Release, Ordering::Relaxed);
    }
    fn read(self) -> u8 {
        let code = unsafe { &*self.0 }.error.load(Ordering::Acquire);
        if code <= 16 { code as u8 } else { bootstrap::error_code(Error::Status) }
    }
    pub(super) fn mark_isolated(self, bit: u32) { unsafe { &*self.0 }.isolated.fetch_or(bit, Ordering::Release); }
    /// Runtime bit 1 always; forwarder bit 2 and launcher bit 4 exactly when planned.
    fn all_isolated(self, forwarder: bool, launcher: bool) -> bool {
        let expected = 1 | if forwarder { 2 } else { 0 } | if launcher { LAUNCHER_ISOLATED } else { 0 };
        self.read() == 0 && unsafe { &*self.0 }.isolated.load(Ordering::Acquire) == expected
    }
    /// Launcher only, after `mark_isolated`: drop this process's writable mapping.
    ///
    /// # Safety
    /// No copy of this `FailurePage` may be used afterwards in this process.
    pub(super) unsafe fn unmap(self) -> bool {
        unsafe { libc::munmap(self.0.cast_mut().cast(), 4096) == 0 }
    }
    fn allow_child(self) { unsafe { &*self.0 }.supervisor_ready.store(1, Ordering::Release); }
    fn await_supervisor(self) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(duduclaw_isolation::ipc::CONTROL_TIMEOUT_MS);
        loop {
            if self.read() != 0 { return Err(Error::Privileges); }
            if unsafe { &*self.0 }.supervisor_ready.load(Ordering::Acquire) == 1 { return Ok(()); }
            if std::time::Instant::now() >= deadline { return Err(Error::Status); }
            let interval = libc::timespec { tv_sec: 0, tv_nsec: 1_000_000 };
            if unsafe { libc::nanosleep(&interval, std::ptr::null_mut()) } != 0 && super::errno() != libc::EINTR {
                return Err(Error::Status);
            }
        }
    }
}

fn supervisor(status: RawFd, pidfd: RawFd, mut machine: Machine, identity: (u32,u32), failure: FailurePage) -> ! {
    let preparation = (|| -> Result<Filter> {
        fd::close_except(&[0, status, pidfd])?; machine.advance(Stage::SupervisorClosed)?;
        privileges::drop_all(identity.0, identity.1)?; machine.advance(Stage::SupervisorDropped)?;
        let filter = seccomp::supervisor(Architecture::native().map_err(|_| Error::Seccomp)?, status, pidfd)
            .map_err(|_| Error::Seccomp)?;
        machine.advance(Stage::SupervisorFiltered)?;
        Ok(filter)
    })();
    let filter = match preparation { Ok(filter) => filter, Err(error) => {
        failure.record(error);
        let _ = bootstrap::send_packet(0, &bootstrap::error_status(error), None); exit_now(125)
    }};
    if filter.install().is_err() {
        failure.record(Error::Seccomp);
        let _ = bootstrap::send_packet(0, &bootstrap::error_status(Error::Seccomp), None); exit_now(125);
    }
    if bootstrap::send_packet(0, &bootstrap::status(6, 0, 0), None).is_err() {
        failure.record(Error::Status); exit_now(125);
    }
    failure.allow_child();
    // No allocation, deallocation, prctl, logging, signal sending or file opens.
    let mut poll = [libc::pollfd { fd: status, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: pidfd, events: libc::POLLIN, revents: 0 }];
    loop {
        let result = unsafe { libc::ppoll(poll.as_mut_ptr(), poll.len() as libc::nfds_t, std::ptr::null(), std::ptr::null()) };
        if result < 0 { if super::errno() == libc::EINTR { continue; } exit_now(125); }
        if poll[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 12];
            let count = unsafe { libc::read(status, bytes.as_mut_ptr().cast(), bytes.len()) };
            if count != 12 || bytes[..4] != *b"DCR1" { exit_now(125); }
            if bootstrap::send_packet(0, &bytes, None).is_err() { exit_now(125); }
            continue;
        }
        if poll[0].revents & libc::POLLHUP != 0 { poll[0].fd = -1; }
        if poll[1].revents & libc::POLLIN != 0 {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe { libc::waitid(libc::P_PIDFD, pidfd as libc::id_t, &mut info, libc::WEXITED) } != 0 {
                if super::errno() == libc::EINTR { continue; } exit_now(125);
            }
            let code = unsafe { info.si_status() };
            let error = failure.read();
            if error != 0 { let _ = bootstrap::send_packet(0, &bootstrap::status(0, error, 0), None); }
            let kind = if info.si_code == libc::CLD_EXITED { 4 } else { 5 };
            let _ = bootstrap::send_packet(0, &bootstrap::status(kind, 0, code as u32), None);
            exit_now(if kind == 4 { code } else { 128 + code });
        }
        if poll.iter().any(|p| p.revents & (libc::POLLERR | libc::POLLNVAL) != 0) { exit_now(125); }
    }
}

#[allow(clippy::too_many_arguments)]
fn child_init(plan: TrustedRun, exec: OwnedFd, script: Option<OwnedFd>, prompt: OwnedFd,
    attachments: Vec<OwnedFd>, stdin: Option<OwnedFd>, mcp: Option<OwnedFd>, forwarder: Option<(OwnedFd,OwnedFd)>,
    slave: Option<OwnedFd>, launcher: Option<(super::launcher::Prepared, RuntimeCgroup)>, abi: u32, status: RawFd,
    mut machine: Machine, failure: FailurePage) -> Result<()> {
    mounts::mount_proc(plan.proc_subset_pid).map_err(|_| Error::Mount)?; machine.advance(Stage::ProcMounted)?;
    // [F2] Still root with CAP_SYS_RESOURCE: value and inherited minimum become
    // 0 before the forwarder, launcher or runtime exists (only the supervisor keeps -1000).
    privileges::reset_oom_score_adj()?;
    privileges::set_securebits()?; machine.advance(Stage::Securebits)?;
    privileges::drop_bounding()?; machine.advance(Stage::BoundingDropped)?;
    privileges::set_identity(plan.root.employee_uid, plan.root.employee_gid)?; machine.advance(Stage::IdentityDropped)?;
    privileges::clear_capabilities()?; machine.advance(Stage::CapsCleared)?;
    privileges::non_dumpable()?; machine.advance(Stage::DumpableVerified)?;
    privileges::no_new_privileges()?; machine.advance(Stage::NoNewPrivileges)?;
    privileges::verify(plan.root.employee_uid, plan.root.employee_gid)?; machine.advance(Stage::PrivilegesVerified)?;
    write_status(status, bootstrap::status(1, 0, abi));
    let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
    // HX launcher (D8 revision §2.1): forked HERE, after privilege readback and
    // BEFORE init's runtime Landlock, so its Landlock domain is a sibling of the
    // runtime's and not nested in it (the primary guard against the runtime
    // reaching the launcher's or a helper's /proc and memory). Non-dumpable is
    // inherited from `non_dumpable` above; no exec, no extra artifact (Q1 = A).
    let mut launcher_pid = None;
    let mut launcher_ready = None;
    let mut runtime_cgroup = None;
    if let Some((prepared, cgroup)) = launcher {
        let filters = super::launcher::Filters::build(arch, &plan)?;
        let mut ready = [-1; 2];
        super::checked(unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) }, Error::Process)?;
        let ready_read = unsafe { OwnedFd::from_raw_fd(ready[0]) };
        let ready_write = unsafe { OwnedFd::from_raw_fd(ready[1]) };
        let child = fork()?;
        if child == 0 {
            drop(ready_read);
            super::launcher::serve(&plan, prepared, filters, ready_write, status, failure, abi);
        }
        machine.advance(Stage::LauncherForked)?;
        // Launcher-only authority leaves init now: control socket, helper
        // executables and the slot cgroup files.
        drop((ready_write, prepared, filters));
        launcher_pid = Some(child);
        launcher_ready = Some(ready_read);
        runtime_cgroup = Some(cgroup);
    }
    restrict_filesystem(abi)?; machine.advance(Stage::Landlocked)?;
    let common = seccomp::common(arch);
    common.install().map_err(|_| Error::Seccomp)?; machine.advance(Stage::CommonFiltered)?;
    let runtime_filter = seccomp::runtime(arch, &plan.program.seccomp, plan.program.seccomp_digest).map_err(|_| Error::Seccomp)?;
    let (runtime_procs, runtime_kill) = match runtime_cgroup {
        Some(RuntimeCgroup { procs, kill }) => (Some(procs), Some(kill)),
        None => (None, None),
    };
    let init_filter = seccomp::init_with_kill(arch, status, runtime_kill.as_ref().map(AsRawFd::as_raw_fd))
        .map_err(|_| Error::Seccomp)?;
    install_signal_handlers()?;
    let mut exec_pipe = [-1; 2];
    super::checked(unsafe { libc::pipe2(exec_pipe.as_mut_ptr(), libc::O_CLOEXEC) }, Error::Process)?;
    let exec_check = unsafe { OwnedFd::from_raw_fd(exec_pipe[0]) };
    let exec_done = unsafe { OwnedFd::from_raw_fd(exec_pipe[1]) };
    let mut forwarder_pid = None;
    if let Some((socket, forward_exec)) = forwarder {
        let program = plan.forwarder.as_ref().ok_or(Error::Executable)?;
        let filter = seccomp::runtime(arch, &program.seccomp, program.seccomp_digest).map_err(|_| Error::Seccomp)?;
        let child = fork()?;
        if child == 0 {
            // Drop all runtime capability objects before entering the forwarder.
            drop((exec, script, prompt, attachments, stdin, mcp, slave));
            drop(exec_check);
            let result = execute_forwarder(program, forward_exec, socket, exec_done, &filter, failure);
            if let Err(error) = result { failure.record(error); write_status(status, bootstrap::error_status(error)); }
            exit_now(126);
        }
        forwarder_pid = Some(child);
        drop((socket, forward_exec));
    }
    let runtime = fork()?;
    machine.advance(Stage::Forked)?;
    if runtime == 0 {
        drop(exec_check);
        let result = execute_runtime(&plan.program, exec, script, prompt, attachments, stdin, mcp, slave, exec_done,
            runtime_procs, &runtime_filter, machine, failure);
        if let Err(error) = result { failure.record(error); write_status(status, bootstrap::error_status(error)); }
        exit_now(126);
    }
    RUNTIME_PID.store(runtime, Ordering::Relaxed);
    drop((exec_done, runtime_procs));
    // EOF alone is not evidence: a child can die before exec. Shared flags are
    // written only after final fd readback, Landlock and seccomp installation.
    let mut byte = 0u8;
    loop {
        let count = unsafe { libc::read(exec_check.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if count == 0 { break; }
        if count < 0 && super::errno() == libc::EINTR { continue; }
        return Err(Error::Exec);
    }
    drop(exec_check);
    // [F12] The launcher writes its ready byte only after marking bit 4,
    // unmapping its page and closing its status fd; wait (bounded) before the
    // isolation check so a slow launcher cannot race `all_isolated`.
    if let Some(ready) = launcher_ready.take() { await_launcher(ready)?; }
    if !failure.all_isolated(forwarder_pid.is_some(), launcher_pid.is_some()) { return Err(Error::Exec); }
    // A dead forwarder is a broken network boundary. A short successful runtime,
    // however, must preserve its actual outcome without issuing a ready receipt.
    for (pid, is_runtime) in forwarder_pid.into_iter().map(|pid| (pid, false)).chain([(runtime, true)]) {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, pid as u32, &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) } != 0 { return Err(Error::Process); }
        if unsafe { info.si_pid() } != 0 {
            if !is_runtime { return Err(Error::Process); }
            let code = unsafe { info.si_status() };
            exit_now(if info.si_code == libc::CLD_EXITED { code } else { 128 + code });
        }
    }
    // Isolation completion is not an exec-success receipt. Terminal status
    // still revokes the capability if exec fails or the process exits later.
    write_status(status, bootstrap::status(7, 0, 0));
    drop((exec, script, prompt, attachments, stdin, mcp, slave));
    let kill = runtime_kill.map(IntoRawFd::into_raw_fd);
    let mut keep = vec![status];
    keep.extend(kill);
    fd::close_except(&keep)?; machine.advance(Stage::InitClosed)?;
    machine.advance(Stage::InitFiltered)?;
    init_filter.install().map_err(|_| Error::Seccomp)?;
    if unblock_signals().is_err() { failure.record(Error::Process); exit_now(125); }
    write_status(status, bootstrap::status(3, 0, 0));
    unsafe { libc::close(status); }
    let drain_ms = plan.launcher.as_ref().map_or(0, |launcher| launcher.drain_ms);
    reap(runtime, forwarder_pid, launcher_pid, kill, drain_ms, failure)
}

fn await_launcher(ready: OwnedFd) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(duduclaw_isolation::ipc::CONTROL_TIMEOUT_MS);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() { return Err(Error::Process); }
        let mut poll = libc::pollfd { fd: ready.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let timeout = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX).max(1);
        let result = unsafe { libc::poll(&mut poll, 1, timeout) };
        if result < 0 { if super::errno() == libc::EINTR { continue; } return Err(Error::Process); }
        if result == 0 { continue; }
        let mut byte = 0u8;
        let count = unsafe { libc::read(ready.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if count < 0 && super::errno() == libc::EINTR { continue; }
        // EOF means the launcher died (or failed) before it was isolated.
        return if count == 1 && byte == super::launcher::READY { Ok(()) } else { Err(Error::Process) };
    }
}

/// Init's terminal loop, under the init filter. A dead forwarder ends the run
/// (broken network boundary); a dead launcher does not (§2.5: spawnd sees the
/// control socket close). On runtime exit [F4]: (a) kill `rt-runtime` through
/// the root-opened fd so its daemonized descendants (which may still hold MCP
/// fd 3) die first; (b) SIGTERM the launcher, which refuses `Start` from then
/// on and drains; (c) wait at most F + 2 s, re-armed every 200 ms so a missed
/// SIGALRM cannot leave init blocked, then SIGKILL the launcher's process group
/// and exit with the runtime's stored status. Init exit tears down the PID
/// namespace, so every helper dies with it.
fn reap(runtime: i32, forwarder: Option<i32>, mut launcher: Option<i32>, kill: Option<RawFd>, drain_ms: u64,
    failure: FailurePage) -> ! {
    let mut outcome: Option<i32> = None;
    loop {
        let mut exit = 0i32;
        let pid = unsafe { libc::waitpid(-1, &mut exit, 0) };
        if pid < 0 {
            if super::errno() == libc::EINTR {
                if outcome.is_some() && DRAIN_EXPIRED.load(Ordering::Relaxed) {
                    if let Some(launcher) = launcher { unsafe { libc::kill(-launcher, libc::SIGKILL); libc::kill(launcher, libc::SIGKILL); } }
                    break;
                }
                continue;
            }
            if outcome.is_some() { break; }
            exit_now(125);
        }
        if outcome.is_none() && forwarder == Some(pid) {
            failure.record(Error::Process);
            exit_now(125);
        }
        if launcher == Some(pid) {
            launcher = None;
            if outcome.is_some() { break; }
            continue;
        }
        if pid == runtime && outcome.is_none() {
            let code = if libc::WIFEXITED(exit) { libc::WEXITSTATUS(exit) }
                else if libc::WIFSIGNALED(exit) { 128 + libc::WTERMSIG(exit) } else { continue };
            // The namespace may reuse this pid; never forward signals to it again.
            RUNTIME_PID.store(0, Ordering::Relaxed);
            outcome = Some(code);
            if let Some(kill) = kill { unsafe { libc::write(kill, b"1".as_ptr().cast(), 1); } }
            match launcher {
                None => break,
                Some(launcher) => {
                    unsafe { libc::kill(launcher, libc::SIGTERM); }
                    arm_drain_alarm(drain_ms.saturating_add(LAUNCHER_DRAIN_SLACK_MS));
                }
            }
        }
    }
    exit_now(outcome.unwrap_or(125))
}

fn arm_drain_alarm(ms: u64) {
    let value = libc::timeval { tv_sec: (ms / 1000) as libc::time_t, tv_usec: ((ms % 1000) * 1000) as libc::suseconds_t };
    let interval = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
    let timer = libc::itimerval { it_interval: interval, it_value: value };
    // Allowed by the init filter; on failure the run still ends at RuntimeMaxUSec.
    unsafe { libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut()); }
}

fn restrict_filesystem(abi: u32) -> Result<()> {
    let policy = Policy::for_abi(abi).map_err(|_| Error::Landlock)?;
    let mut roots = Vec::new();
    // One table: HX publication check 6 is computed against exactly this list.
    for (path, class) in crate::helper_policy::RUNTIME_LANDLOCK {
        let raw = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if raw < 0 {
            if path == c"/lib64" && super::errno() == libc::ENOENT { continue; }
            return Err(Error::Landlock);
        }
        roots.push((unsafe { OwnedFd::from_raw_fd(raw) }, class));
    }
    let borrowed: Vec<_> = roots.iter().map(|(fd,class)| (fd.as_fd(),*class)).collect();
    crate::landlock::restrict(&policy, &borrowed).map_err(|_| Error::Landlock)
}

#[allow(clippy::too_many_arguments)]
fn execute_runtime(program: &Program, exec: OwnedFd, script: Option<OwnedFd>, prompt: OwnedFd,
    attachments: Vec<OwnedFd>, stdin: Option<OwnedFd>, mcp: Option<OwnedFd>, slave: Option<OwnedFd>, exec_done: OwnedFd,
    cgroup: Option<OwnedFd>, filter: &Filter, mut machine: Machine, failure: FailurePage) -> Result<()> {
    // HX [F4]: move into `rt-runtime` through the root-opened `cgroup.procs`
    // before anything else, so every runtime descendant is in that leaf.
    if let Some(procs) = cgroup {
        if unsafe { libc::write(procs.as_raw_fd(), b"0".as_ptr().cast(), 1) } != 1 { return Err(Error::Process); }
    }
    super::checked(unsafe { libc::setsid() }, Error::Process)?;
    let native = std::fs::symlink_metadata("/run/native-project");
    let cwd = match native {
        Ok(metadata) if metadata.is_dir() => c"/run/native-project",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => c"/work",
        _ => return Err(Error::Process),
    };
    super::checked(unsafe { libc::chdir(cwd.as_ptr()) }, Error::Process)?;
    machine.advance(Stage::RuntimeSession)?;
    let mut owned = Vec::new(); let mut sources = Vec::new();
    let mut add = |fd: OwnedFd, destination: Destination| {
        sources.push(Source { fd: fd.as_raw_fd(), destination }); owned.push(fd);
    };
    if let Some(slave) = slave {
        super::checked(unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0) }, Error::Process)?;
        add(slave, Destination::Runtime(vec![0,1,2]));
    } else {
        add(match stdin { Some(fd) => fd, None => dev_null(libc::O_RDONLY)? }, Destination::Runtime(vec![0]));
        add(unsafe { OwnedFd::from_raw_fd(1) }, Destination::Runtime(vec![1]));
        add(unsafe { OwnedFd::from_raw_fd(2) }, Destination::Runtime(vec![2]));
    }
    add(match mcp { Some(fd) => fd, None => dev_null(libc::O_RDONLY)? }, Destination::Runtime(vec![3]));
    add(prompt, Destination::Runtime(vec![4]));
    for (index, fd) in attachments.into_iter().enumerate() { add(fd, Destination::Runtime(vec![5 + index as i32])); }
    add(exec, Destination::Executable);
    add(exec_done, Destination::ExecStatus);
    if let Some(script) = script { add(script, Destination::Script); }
    let plan = FdPlan::new(sources, fd::soft_limit()?)?;
    let script = program.script.as_ref().map(|(identity,path)| (path.as_c_str(),identity.mount));
    let (exec, exec_done) = fd::reorder_for_exec(plan, owned, script)?; machine.advance(Stage::RuntimeFds)?;
    execute(program, exec, exec_done.ok_or(Error::Descriptors)?, filter, &mut machine, failure)
}

fn execute_forwarder(program: &Program, exec: OwnedFd, socket: OwnedFd, exec_done: OwnedFd, filter: &Filter, failure: FailurePage) -> Result<()> {
    super::checked(unsafe { libc::setsid() }, Error::Process)?;
    // Fixed 0..4 preserve the same audited shuffle; only fd 3 is authority-bearing.
    // Forwarder stdout/stderr are discarded, never inherited runtime output pipes.
    let null = dev_null(libc::O_RDWR)?;
    let sources = vec![Source { fd: null.as_raw_fd(), destination: Destination::Runtime(vec![0,1,2,4]) },
        Source { fd: socket.as_raw_fd(), destination: Destination::Runtime(vec![3]) },
        Source { fd: exec.as_raw_fd(), destination: Destination::Executable },
        Source { fd: exec_done.as_raw_fd(), destination: Destination::ExecStatus }];
    let plan = FdPlan::new(sources, fd::soft_limit()?)?;
    let (exec, exec_done) = fd::reorder_for_exec(plan, vec![null,socket,exec,exec_done], None)?;
    // Forwarder shares the runtime post-fork close/exec implementation; lifecycle
    // checks for the main runtime remain separate from this auxiliary process.
    execute_raw(program, exec, exec_done.ok_or(Error::Descriptors)?, filter, failure, 2)
}

fn execute(program: &Program, exec: OwnedFd, exec_done: OwnedFd, filter: &Filter, machine: &mut Machine, failure: FailurePage) -> Result<()> {
    machine.advance(Stage::RuntimeFiltered)?;
    execute_raw(program, exec, exec_done, filter, failure, 1)
}
fn execute_raw(program: &Program, exec: OwnedFd, exec_done: OwnedFd, filter: &Filter, failure: FailurePage, isolated_bit: u32) -> Result<()> {
    let mut argv: Vec<_> = program.argv.iter().map(|s| s.as_ptr()).collect(); argv.push(std::ptr::null());
    let mut env: Vec<_> = program.environment.iter().map(|s| s.as_ptr()).collect(); env.push(std::ptr::null());
    let exec = exec.into_raw_fd();
    let _exec_done = exec_done.into_raw_fd();
    reset_signal_handlers()?;
    // Avoid cleanup/error formatting after this point: only execveat/exit_group.
    if filter.install().is_err() { failure.record(Error::Seccomp); exit_now(126); }
    failure.mark_isolated(isolated_bit);
    unsafe { libc::syscall(libc::SYS_execveat, exec, c"".as_ptr(), argv.as_ptr(), env.as_ptr(), libc::AT_EMPTY_PATH); }
    failure.record(Error::Exec);
    exit_now(126)
}

pub(super) fn dev_null(flags: i32) -> Result<OwnedFd> {
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 { return Err(Error::Descriptors); }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let o = fd::observe(fd.as_raw_fd())?;
    if o.kind != validation::FileKind::Character || o.rdev_major != 1 || o.rdev_minor != 3 { return Err(Error::Descriptors); }
    Ok(fd)
}

pub(super) fn fork() -> Result<i32> {
    // Raw clone avoids libc fork callbacks and cannot use clone3 (common returns ENOSYS).
    let pid = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0usize, 0usize, 0usize, 0usize) };
    if pid < 0 { Err(Error::Process) } else { Ok(pid as i32) }
}

static RUNTIME_PID: AtomicI32 = AtomicI32::new(0);
static DRAIN_EXPIRED: AtomicBool = AtomicBool::new(false);
extern "C" fn drain_expired(_signal: i32) { DRAIN_EXPIRED.store(true, Ordering::Relaxed); }
extern "C" fn forward_signal(signal: i32) {
    let pid = RUNTIME_PID.load(Ordering::Relaxed);
    if pid > 1 {
        // Async-signal-safe, namespace-local PID selected by the init, never wire input.
        unsafe { if libc::kill(-pid, signal) != 0 { libc::kill(pid, signal); } }
    }
}
fn install_signal_handlers() -> Result<()> {
    let mask = termination_mask();
    super::checked(unsafe { libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) }, Error::Process)?;
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = forward_signal as *const () as usize;
    unsafe { libc::sigemptyset(&mut action.sa_mask); }
    for signal in [libc::SIGTERM,libc::SIGINT,libc::SIGHUP] {
        super::checked(unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) }, Error::Process)?;
    }
    // §2.5 (c): SIGALRM only sets a flag. No SA_RESTART, so init's waitpid
    // returns EINTR; a PID-namespace init ignores SIG_DFL SIGALRM entirely.
    let mut alarm: libc::sigaction = unsafe { std::mem::zeroed() };
    alarm.sa_sigaction = drain_expired as *const () as usize;
    unsafe { libc::sigemptyset(&mut alarm.sa_mask); }
    super::checked(unsafe { libc::sigaction(libc::SIGALRM, &alarm, std::ptr::null_mut()) }, Error::Process)?;
    Ok(())
}
fn termination_mask() -> libc::sigset_t {
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut mask); }
    for signal in [libc::SIGTERM,libc::SIGINT,libc::SIGHUP] { unsafe { libc::sigaddset(&mut mask, signal); } }
    mask
}
fn unblock_signals() -> Result<()> {
    let mask = termination_mask();
    super::checked(unsafe { libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) }, Error::Process)
}
fn reset_signal_handlers() -> Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    unsafe { libc::sigemptyset(&mut action.sa_mask); }
    for signal in [libc::SIGTERM,libc::SIGINT,libc::SIGHUP] {
        super::checked(unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) }, Error::Process)?;
    }
    unblock_signals()
}
pub(super) fn write_status(fd: RawFd, bytes: [u8;12]) {
    if unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) } != bytes.len() as isize { exit_now(125); }
}
pub(super) fn exit_now(code: i32) -> ! { unsafe { libc::_exit(code) } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eof_without_both_final_isolation_receipts_never_admits() {
        let page = FailurePage::new().unwrap();
        assert!(!page.all_isolated(false, false));
        page.mark_isolated(1);
        assert!(page.all_isolated(false, false));
        assert!(!page.all_isolated(true, false));
        page.mark_isolated(2);
        assert!(page.all_isolated(true, false));
        page.record(Error::Exec);
        assert!(!page.all_isolated(true, false));
        unsafe { libc::munmap(page.0.cast_mut().cast(), 4096); }
    }
    #[test]
    fn launcher_ready_before_all_isolated() {
        // A planned launcher must have marked bit 4; an unplanned bit 4 is as
        // wrong as a missing one (exact equality, never "at least").
        for forwarder in [false, true] {
            let page = FailurePage::new().unwrap();
            page.mark_isolated(1);
            if forwarder { page.mark_isolated(2); }
            assert!(!page.all_isolated(forwarder, true), "launcher bit missing");
            assert!(page.all_isolated(forwarder, false));
            page.mark_isolated(LAUNCHER_ISOLATED);
            assert!(page.all_isolated(forwarder, true));
            assert!(!page.all_isolated(forwarder, false), "unplanned launcher bit");
            page.record(Error::Landlock);
            assert!(!page.all_isolated(forwarder, true), "launcher setup error never admits");
            unsafe { libc::munmap(page.0.cast_mut().cast(), 4096); }
        }
        // The launcher's unmap drops only its own mapping of a forked page.
        let page = FailurePage::new().unwrap();
        // The child only touches the atomic page, munmap and _exit (fork-safe).
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            page.mark_isolated(LAUNCHER_ISOLATED);
            let unmapped = unsafe { page.unmap() };
            unsafe { libc::_exit(if unmapped { 0 } else { 1 }); }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        page.mark_isolated(1);
        assert!(page.all_isolated(false, true), "the parent still reads the child's mark");
        unsafe { libc::munmap(page.0.cast_mut().cast(), 4096); }
    }
    fn profile(calls: &[u32]) -> (RuntimeProfile, Sha256Digest) {
        let profile = RuntimeProfile { allowed_syscalls: calls.to_vec() };
        let digest = profile.digest(Architecture::native().unwrap()).unwrap();
        (profile, digest)
    }
    fn program(argv: &[&str], calls: &[u32], script: Option<&str>) -> Program {
        let (seccomp, seccomp_digest) = profile(calls);
        let identity = Identity { inode: 7, mount: 9, dev_major: 8, dev_minor: 1 };
        Program { identity, script: script.map(|name| (Identity { inode: 8, ..identity }, CString::new(name).unwrap())),
            argv: argv.iter().map(|arg| CString::new(*arg).unwrap()).collect(), environment: vec![], seccomp, seccomp_digest }
    }
    #[test]
    fn launcher_plan_requires_parent_launcher_calls_and_flat_helper_files() {
        let arch = Architecture::native().unwrap();
        let parent_calls = arch.launcher_parent_calls().to_vec();
        let parent = program(&["/runtime/claude"], &parent_calls, None);
        let helper_calls = [arch.execveat(), arch.exit_group(), arch.launcher_parent_calls()[0]];
        let helper = |argv: &[&str], script: Option<&str>| HelperProgram { program: program(argv, &helper_calls, script),
            self_only: vec![], mount: 0, slot: crate::helper_policy::HelperSlot::Foreground,
            filesystem: vec![crate::helper_policy::HelperFs::SystemRo, crate::helper_policy::HelperFs::Work],
            open_files: 64, file_size_bytes: 1 << 20, timeout_ms: 30_000, egress: false };
        let plan = |helpers: Vec<HelperProgram>| LauncherPlan { helpers, mounts: vec![MountIdentity { mount: 9, fs_type: 1 }],
            tmpfs_bytes: 1 << 20, drain_ms: 10_000 };
        assert!(plan(vec![helper(&["/helpers/program"], None)]).validate(&parent).is_ok());
        assert!(plan(vec![helper(&["/helpers/node", "/helpers/driver.cjs"], Some("driver.cjs"))]).validate(&parent).is_ok());
        // HX3a: F_L adds no kill/dup3/file/socket calls, so a parent without them is refused.
        for missing in arch.launcher_parent_calls() {
            let calls: Vec<_> = parent_calls.iter().copied().filter(|nr| *nr != missing).collect();
            let narrow = program(&["/runtime/claude"], &calls, None);
            assert_eq!(plan(vec![helper(&["/helpers/program"], None)]).validate(&narrow), Err(Error::Seccomp), "{missing}");
        }
        // Nested or non-/helpers names, and one name with two identities, are refused.
        for argv in [&["/helpers/a/b"][..], &["/runtime/program"], &["/helpers/.."]] {
            assert!(plan(vec![helper(argv, None)]).validate(&parent).is_err(), "{argv:?}");
        }
        assert!(plan(vec![helper(&["/helpers/node", "/helpers/sub/driver.cjs"], Some("sub/driver.cjs"))]).validate(&parent).is_err());
        let mut other = helper(&["/helpers/program"], None);
        other.program.identity.inode = 99;
        assert!(plan(vec![helper(&["/helpers/program"], None), other]).validate(&parent).is_err());
        // Exactly one registry mount; bounded drain window.
        let mut two = plan(vec![helper(&["/helpers/program"], None)]);
        two.mounts.push(MountIdentity { mount: 10, fs_type: 1 });
        assert!(two.validate(&parent).is_err());
        for drain_ms in [0, MAX_LAUNCHER_DRAIN_MS + 1] {
            let mut bad = plan(vec![helper(&["/helpers/program"], None)]);
            bad.drain_ms = drain_ms;
            assert!(bad.validate(&parent).is_err());
        }
        // F_H must stay within the parent (a call the parent lacks).
        let mut wide = helper(&["/helpers/program"], None);
        (wide.program.seccomp, wide.program.seccomp_digest) = profile(&[arch.execveat(), arch.exit_group(), 0x3ff]);
        assert_eq!(plan(vec![wide]).validate(&parent), Err(Error::Seccomp));
    }
    #[test]
    fn supervisor_barrier_never_releases_after_preparation_failure() {
        let page = FailurePage::new().unwrap();
        page.record(Error::Privileges);
        page.allow_child();
        assert_eq!(page.await_supervisor(), Err(Error::Privileges));
        assert_eq!(page.read(), bootstrap::error_code(Error::Privileges));
        unsafe { libc::munmap(page.0.cast_mut().cast(), 4096); }
    }
    #[test]
    fn late_exec_error_keeps_first_fixed_code_without_a_descriptor() {
        let page = FailurePage::new().unwrap();
        page.allow_child();
        assert_eq!(page.await_supervisor(), Ok(()));
        page.record(Error::Exec); page.record(Error::Process);
        assert_eq!(page.read(), bootstrap::error_code(Error::Exec));
        unsafe { libc::munmap(page.0.cast_mut().cast(), 4096); }
    }
}
