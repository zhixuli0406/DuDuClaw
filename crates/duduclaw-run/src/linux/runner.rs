//! Single-threaded, one-shot launch engine. No stage has an unsandboxed fallback.
use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use duduclaw_isolation::{ids::Sha256Digest, ipc::InputFd};
use crate::{Error, Result, fd_plan::{Destination, FdPlan, Source}, landlock::{PathClass, Policy},
    seccomp::{self, Architecture, Filter, RuntimeProfile}, state::{Machine, Stage}, validation::{self, Expected, Identity}};
use super::{bootstrap, fd, mounts, privileges, probe};

#[derive(Clone, Copy)]
pub struct MountIdentity { pub mount: u64, pub fs_type: i64 }
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
}
impl TrustedRun {
    pub fn expected_fd_count(&self) -> usize {
        5 + 1 + usize::from(self.program.script.is_some()) + 1 + self.attachments.len()
            + usize::from(self.mcp) + if self.forwarder.is_some() { 2 } else { 0 }
    }
    fn validate(&self) -> Result<()> {
        self.root.validate().map_err(|_| Error::Mount)?;
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
    attachments: Vec<OwnedFd>, mcp: Option<OwnedFd>, forwarder: Option<(OwnedFd, OwnedFd)>,
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
    Ok(Inputs { mounts, exec, script, prompt, attachments, mcp, forwarder })
}

fn verify_executable(fd: RawFd, identity: Identity, registry_mount: u64) -> Result<()> {
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
    let n = 3 + 1 + 1 + plan.attachments.len() + 1 + usize::from(plan.program.script.is_some());
    if fd::soft_limit()? < FdPlan::required_limit(n)? { return Err(Error::DescriptorLimit); }
    let input = take_inputs(&plan, fds)?;
    machine.advance(Stage::DescriptorsVerified)?;
    mounts::enter_namespaces().map_err(|_| Error::Namespace)?;
    machine.advance(Stage::Namespaces)?; machine.advance(Stage::PrivatePropagation)?;
    let Inputs { mounts: mount_inputs, exec, script, prompt, attachments, mcp, forwarder } = input;
    let mut root = mounts::build_root(mount_inputs, &plan.root).map_err(|_| Error::Mount)?;
    let pty = root.take_pty();
    let slave = if let Some(pty) = pty {
        validation::validate(fd::observe(pty.slave.as_raw_fd())?, Expected::Pty { mount: pty.mount_id })?;
        bootstrap::send_packet(0, &bootstrap::status(2, 0, 0), Some(pty.master.as_raw_fd()))?;
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
        std::mem::forget((plan, exec, script, prompt, attachments, mcp, forwarder, slave, write));
        supervisor(status, pidfd, machine, supervisor_identity, failure)
    } else {
        drop(read);
        super::checked(unsafe { libc::close(0) }, Error::Descriptors)?;
        let status = write.as_raw_fd();
        if let Err(error) = failure.await_supervisor() {
            failure.record(error); write_status(status, bootstrap::error_status(error)); exit_now(125);
        }
        let result = child_init(plan, exec, script, prompt, attachments, mcp, forwarder,
            slave, ready.landlock_abi, status, machine, failure);
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
#[derive(Clone, Copy)]
struct FailurePage(*const SharedStatus);
#[repr(C)]
struct SharedStatus { error: AtomicU32, supervisor_ready: AtomicU32 }
impl FailurePage {
    fn new() -> Result<Self> {
        let page = unsafe { libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS, -1, 0) };
        if page == libc::MAP_FAILED { return Err(Error::Process); }
        let word = page.cast::<SharedStatus>();
        unsafe { word.write(SharedStatus { error: AtomicU32::new(0), supervisor_ready: AtomicU32::new(0) }); }
        Ok(Self(word))
    }
    fn record(self, error: Error) {
        let _ = unsafe { &*self.0 }.error.compare_exchange(0, u32::from(bootstrap::error_code(error)), Ordering::Release, Ordering::Relaxed);
    }
    fn read(self) -> u8 {
        let code = unsafe { &*self.0 }.error.load(Ordering::Acquire);
        if code <= 16 { code as u8 } else { bootstrap::error_code(Error::Status) }
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
    attachments: Vec<OwnedFd>, mcp: Option<OwnedFd>, forwarder: Option<(OwnedFd,OwnedFd)>,
    slave: Option<OwnedFd>, abi: u32, status: RawFd, mut machine: Machine, failure: FailurePage) -> Result<()> {
    mounts::mount_proc(plan.proc_subset_pid).map_err(|_| Error::Mount)?; machine.advance(Stage::ProcMounted)?;
    privileges::set_securebits()?; machine.advance(Stage::Securebits)?;
    privileges::drop_bounding()?; machine.advance(Stage::BoundingDropped)?;
    privileges::set_identity(plan.root.employee_uid, plan.root.employee_gid)?; machine.advance(Stage::IdentityDropped)?;
    privileges::clear_capabilities()?; machine.advance(Stage::CapsCleared)?;
    privileges::non_dumpable()?; machine.advance(Stage::DumpableVerified)?;
    privileges::no_new_privileges()?; machine.advance(Stage::NoNewPrivileges)?;
    privileges::verify(plan.root.employee_uid, plan.root.employee_gid)?; machine.advance(Stage::PrivilegesVerified)?;
    write_status(status, bootstrap::status(1, 0, abi));
    restrict_filesystem(abi)?; machine.advance(Stage::Landlocked)?;
    let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
    let common = seccomp::common(arch);
    common.install().map_err(|_| Error::Seccomp)?; machine.advance(Stage::CommonFiltered)?;
    let runtime_filter = seccomp::runtime(arch, &plan.program.seccomp, plan.program.seccomp_digest).map_err(|_| Error::Seccomp)?;
    let init_filter = seccomp::init(arch, status).map_err(|_| Error::Seccomp)?;
    install_signal_handlers()?;
    let mut forwarder_pid = None;
    if let Some((socket, forward_exec)) = forwarder {
        let program = plan.forwarder.as_ref().ok_or(Error::Executable)?;
        let filter = seccomp::runtime(arch, &program.seccomp, program.seccomp_digest).map_err(|_| Error::Seccomp)?;
        let child = fork()?;
        if child == 0 {
            // Drop all runtime capability objects before entering the forwarder.
            drop((exec, script, prompt, attachments, mcp, slave));
            let result = execute_forwarder(program, forward_exec, socket, &filter, failure);
            if let Err(error) = result { failure.record(error); write_status(status, bootstrap::error_status(error)); }
            exit_now(126);
        }
        forwarder_pid = Some(child);
        drop((socket, forward_exec));
    }
    let runtime = fork()?;
    machine.advance(Stage::Forked)?;
    if runtime == 0 {
        let result = execute_runtime(&plan.program, exec, script, prompt, attachments, mcp, slave, &runtime_filter, machine, failure);
        if let Err(error) = result { failure.record(error); write_status(status, bootstrap::error_status(error)); }
        exit_now(126);
    }
    RUNTIME_PID.store(runtime, Ordering::Relaxed);
    drop((exec, script, prompt, attachments, mcp, slave));
    fd::close_except(&[status])?; machine.advance(Stage::InitClosed)?;
    machine.advance(Stage::InitFiltered)?;
    init_filter.install().map_err(|_| Error::Seccomp)?;
    if unblock_signals().is_err() { failure.record(Error::Process); exit_now(125); }
    write_status(status, bootstrap::status(3, 0, 0));
    unsafe { libc::close(status); }
    loop {
        let mut exit = 0i32;
        let pid = unsafe { libc::waitpid(-1, &mut exit, 0) };
        if forwarder_pid == Some(pid) {
            failure.record(Error::Process);
            exit_now(125);
        }
        if pid == runtime {
            if libc::WIFEXITED(exit) { exit_now(libc::WEXITSTATUS(exit)); }
            if libc::WIFSIGNALED(exit) { exit_now(128 + libc::WTERMSIG(exit)); }
        }
        if pid < 0 && super::errno() != libc::EINTR { exit_now(125); }
    }
}

fn restrict_filesystem(abi: u32) -> Result<()> {
    let policy = Policy::for_abi(abi).map_err(|_| Error::Landlock)?;
    let mut roots = Vec::new();
    for (path, class) in [(c"/usr",PathClass::ReadExecute),(c"/bin",PathClass::ReadExecute),
        (c"/sbin",PathClass::ReadExecute),(c"/lib",PathClass::ReadExecute),(c"/lib64",PathClass::ReadExecute),
        (c"/runtime",PathClass::ReadExecute),(c"/etc",PathClass::ReadOnly),(c"/config",PathClass::ReadOnly),
        (c"/proc",PathClass::ReadOnly),(c"/home/employee",PathClass::Persistent),(c"/work",PathClass::Persistent),
        (c"/tmp",PathClass::Ephemeral),(c"/run",PathClass::Ephemeral),(c"/dev/shm",PathClass::Ephemeral),
        (c"/dev",PathClass::Devices)] {
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
    attachments: Vec<OwnedFd>, mcp: Option<OwnedFd>, slave: Option<OwnedFd>, filter: &Filter,
    mut machine: Machine, failure: FailurePage) -> Result<()> {
    super::checked(unsafe { libc::setsid() }, Error::Process)?;
    super::checked(unsafe { libc::chdir(c"/work".as_ptr()) }, Error::Process)?;
    machine.advance(Stage::RuntimeSession)?;
    let mut owned = Vec::new(); let mut sources = Vec::new();
    let mut add = |fd: OwnedFd, destination: Destination| {
        sources.push(Source { fd: fd.as_raw_fd(), destination }); owned.push(fd);
    };
    if let Some(slave) = slave {
        super::checked(unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0) }, Error::Process)?;
        add(slave, Destination::Runtime(vec![0,1,2]));
    } else {
        add(dev_null(libc::O_RDONLY)?, Destination::Runtime(vec![0]));
        add(unsafe { OwnedFd::from_raw_fd(1) }, Destination::Runtime(vec![1]));
        add(unsafe { OwnedFd::from_raw_fd(2) }, Destination::Runtime(vec![2]));
    }
    add(match mcp { Some(fd) => fd, None => dev_null(libc::O_RDONLY)? }, Destination::Runtime(vec![3]));
    add(prompt, Destination::Runtime(vec![4]));
    for (index, fd) in attachments.into_iter().enumerate() { add(fd, Destination::Runtime(vec![5 + index as i32])); }
    add(exec, Destination::Executable);
    if let Some(script) = script { add(script, Destination::Script); }
    let plan = FdPlan::new(sources, fd::soft_limit()?)?;
    let script = program.script.as_ref().map(|(identity,path)| (path.as_c_str(),identity.mount));
    let exec = fd::reorder(plan, owned, script)?; machine.advance(Stage::RuntimeFds)?;
    execute(program, exec, filter, &mut machine, failure)
}

fn execute_forwarder(program: &Program, exec: OwnedFd, socket: OwnedFd, filter: &Filter, failure: FailurePage) -> Result<()> {
    super::checked(unsafe { libc::setsid() }, Error::Process)?;
    // Fixed 0..4 preserve the same audited shuffle; only fd 3 is authority-bearing.
    // Forwarder stdout/stderr are discarded, never inherited runtime output pipes.
    let null = dev_null(libc::O_RDWR)?;
    let sources = vec![Source { fd: null.as_raw_fd(), destination: Destination::Runtime(vec![0,1,2,4]) },
        Source { fd: socket.as_raw_fd(), destination: Destination::Runtime(vec![3]) },
        Source { fd: exec.as_raw_fd(), destination: Destination::Executable }];
    let plan = FdPlan::new(sources, fd::soft_limit()?)?;
    let exec = fd::reorder(plan, vec![null,socket,exec], None)?;
    // Forwarder shares the runtime post-fork close/exec implementation; lifecycle
    // checks for the main runtime remain separate from this auxiliary process.
    execute_raw(program, exec, filter, failure)
}

fn execute(program: &Program, exec: OwnedFd, filter: &Filter, machine: &mut Machine, failure: FailurePage) -> Result<()> {
    machine.advance(Stage::RuntimeFiltered)?;
    execute_raw(program, exec, filter, failure)
}
fn execute_raw(program: &Program, exec: OwnedFd, filter: &Filter, failure: FailurePage) -> Result<()> {
    let mut argv: Vec<_> = program.argv.iter().map(|s| s.as_ptr()).collect(); argv.push(std::ptr::null());
    let mut env: Vec<_> = program.environment.iter().map(|s| s.as_ptr()).collect(); env.push(std::ptr::null());
    let exec = exec.into_raw_fd();
    reset_signal_handlers()?;
    // Avoid cleanup/error formatting after this point: only execveat/exit_group.
    if filter.install().is_err() { failure.record(Error::Seccomp); exit_now(126); }
    unsafe { libc::syscall(libc::SYS_execveat, exec, c"".as_ptr(), argv.as_ptr(), env.as_ptr(), libc::AT_EMPTY_PATH); }
    failure.record(Error::Exec);
    exit_now(126)
}

fn dev_null(flags: i32) -> Result<OwnedFd> {
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 { return Err(Error::Descriptors); }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let o = fd::observe(fd.as_raw_fd())?;
    if o.kind != validation::FileKind::Character || o.rdev_major != 1 || o.rdev_minor != 3 { return Err(Error::Descriptors); }
    Ok(fd)
}

fn fork() -> Result<i32> {
    // Raw clone avoids libc fork callbacks and cannot use clone3 (common returns ENOSYS).
    let pid = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0usize, 0usize, 0usize, 0usize) };
    if pid < 0 { Err(Error::Process) } else { Ok(pid as i32) }
}

static RUNTIME_PID: AtomicI32 = AtomicI32::new(0);
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
fn write_status(fd: RawFd, bytes: [u8;12]) {
    if unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) } != bytes.len() as isize { exit_now(125); }
}
fn exit_now(code: i32) -> ! { unsafe { libc::_exit(code) } }

#[cfg(test)]
mod tests {
    use super::*;
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
