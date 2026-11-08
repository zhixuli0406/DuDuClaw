//! HX in-run helper launcher (D8 revision §2.1, §2.4, §2.5; owner Q1 = A).
//!
//! An in-process function of the trampoline, never an exec'd ELF: namespace
//! init forks it after privilege readback and BEFORE its own runtime Landlock
//! (`runner::child_init`), so it keeps init's non-dumpable flag and lives in a
//! Landlock domain that is a sibling of the runtime's, not nested in it. It
//! holds no capability, prompt, attachment, stdin, MCP channel, PTY or egress
//! socket. It serves one `SOCK_SEQPACKET` control socket from spawnd
//! (`helper_wire`, frames are advisory hints [F5]) and runs bounded helpers:
//! fork → setsid + PDEATHSIG → slot cgroup → exact fds → stacked Landlock →
//! hard = soft rlimits → F_H → `execveat`.
//!
//! Syscalls: after F_L only the parent runtime profile plus L_restrict are
//! available; everything this file issues afterwards is listed in
//! `seccomp::Architecture::launcher_parent_calls` (checked at plan time).
use std::ffi::{CStr, CString};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use duduclaw_isolation::ipc::{HelperCallId, MAX_HELPER_INPUT_BYTES};
use crate::{Error, Result, fd_plan::{Destination, FdPlan, Source},
    helper_policy::{self, HelperFs, HelperSlot, HELPER_TMP_ROOT},
    helper_wire::{self, Command, End, FromLauncher, LauncherChannel, LauncherError, Start, MAX_FRAME_FDS, START_FDS},
    landlock::{PathClass, Policy}, seccomp::{self, Architecture, Filter},
    validation::{self, Access, Expected, FileKind, Identity}};
use super::{bootstrap, fd, runner::{self, FailurePage, HelperProgram, LauncherPlan, TrustedRun}};

/// The one byte the launcher writes to init's ready pipe once it is isolated.
pub(super) const READY: u8 = b'L';
const CLONE_PIDFD: u64 = 0x1000;
/// Bounded scratch cleanup: entries visited per call, directory depth.
const SCRATCH_BUDGET: usize = 100_000;
const SCRATCH_DEPTH: usize = 32;

/// Launcher-only descriptors, taken from `TrustedRun` inputs by `runner::launch`.
pub(super) struct Prepared {
    pub(super) control: OwnedFd,
    /// Per helper, in table order: root-verified executable and optional script.
    pub(super) helpers: Vec<(OwnedFd, Option<OwnedFd>)>,
    /// `hx/fg/cgroup.procs`, `hx/rec/cgroup.procs` (root-opened, write-only).
    pub(super) slots: [OwnedFd; 2],
    /// Mount id of each `/helpers/<name>` bind (`mounts::HelperReadback`).
    pub(super) binds: Vec<(CString, u64)>,
}

/// F_L and every F_H, built by init from signed data before the fork.
pub(super) struct Filters { launcher: Filter, helpers: Vec<Filter> }
impl Filters {
    pub(super) fn build(arch: Architecture, run: &TrustedRun) -> Result<Self> {
        let plan = run.launcher.as_ref().ok_or(Error::Executable)?;
        let parent = &run.program;
        if !seccomp::parent_supports_launcher(arch, &parent.seccomp) { return Err(Error::Seccomp); }
        let launcher = seccomp::launcher(arch, &parent.seccomp, parent.seccomp_digest).map_err(|_| Error::Seccomp)?;
        let helpers = plan.helpers.iter().map(|helper| seccomp::helper(arch, &parent.seccomp, parent.seccomp_digest,
            &helper.program.seccomp, helper.program.seccomp_digest, &helper.self_only, helper.egress).map_err(|_| Error::Seccomp))
            .collect::<Result<_>>()?;
        Ok(Self { launcher, helpers })
    }
}

static TERM: AtomicBool = AtomicBool::new(false);
extern "C" fn on_term(_signal: libc::c_int) { TERM.store(true, Ordering::Relaxed); }

/// The launcher child. Never returns. Before it is isolated, any failure is a
/// run failure (recorded in the `FailurePage`, init then refuses readiness);
/// afterwards its death is not fatal to the run (§2.5).
pub(super) fn serve(run: &TrustedRun, prepared: Prepared, filters: Filters, ready: OwnedFd, status: RawFd,
    failure: FailurePage, abi: u32) -> ! {
    let ready = ready.into_raw_fd();
    let launcher = match isolate(run, prepared, &filters, ready, status, abi) {
        Ok(launcher) => launcher,
        Err(error) => {
            failure.record(error);
            runner::write_status(status, bootstrap::error_status(error));
            runner::exit_now(125)
        }
    };
    // [F12] step 4: mark bit 4, unmap the page (never exec'd, so the writable
    // mapping would otherwise persist), close the status pipe, signal init.
    failure.mark_isolated(runner::LAUNCHER_ISOLATED);
    // SAFETY: `failure` is not used again in this process.
    let unmapped = unsafe { failure.unmap() };
    unsafe { libc::close(status); }
    let signalled = unmapped && unsafe { libc::write(ready, (&READY as *const u8).cast(), 1) } == 1;
    unsafe { libc::close(ready); }
    if !signalled { runner::exit_now(125); }
    launcher.run(&filters)
}

fn install_signals() -> Result<()> {
    // SIGTERM stays blocked except inside ppoll, so the drain flag is never missed.
    let mut term: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut term); libc::sigaddset(&mut term, libc::SIGTERM); }
    super::checked(unsafe { libc::sigprocmask(libc::SIG_BLOCK, &term, std::ptr::null_mut()) }, Error::Process)?;
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_term as *const () as usize;
    unsafe { libc::sigemptyset(&mut action.sa_mask); }
    super::checked(unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) }, Error::Process)?;
    // A helper closing its stdin must not kill the launcher on `stop\n`.
    let mut ignore: libc::sigaction = unsafe { std::mem::zeroed() };
    ignore.sa_sigaction = libc::SIG_IGN;
    unsafe { libc::sigemptyset(&mut ignore.sa_mask); }
    super::checked(unsafe { libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut()) }, Error::Process)
}

/// §2.1 steps 1–3 [F12].
fn isolate<'a>(run: &'a TrustedRun, prepared: Prepared, filters: &Filters, ready: RawFd, status: RawFd, abi: u32)
    -> Result<Launcher<'a>> {
    let plan = run.launcher.as_ref().ok_or(Error::Executable)?;
    install_signals()?;
    // Own session and process group: init's signal forwarding to the runtime's
    // group never reaches it, and init can kill exactly this group (§2.5 (c)).
    super::checked(unsafe { libc::setsid() }, Error::Process)?;
    if unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0 { return Err(Error::Privileges); }
    let pid = unsafe { libc::getpid() };
    let Prepared { control, helpers, slots, binds } = prepared;
    // Step 1: 0/1/2 → /dev/null (never the runtime's output pipes), then exactly
    // the status pipe, the ready pipe, the control socket, the helper
    // executables/scripts and the two slot cgroup files. Prompt, attachments,
    // stdin, MCP, PTY slave, egress socket, forwarder ELF and the rt-runtime
    // files are closed by the sweep, exactly like any other inherited fd.
    let mut keep = vec![status, ready, control.as_raw_fd()];
    for (exec, script) in &helpers { keep.push(exec.as_raw_fd()); keep.extend(script.as_ref().map(AsRawFd::as_raw_fd)); }
    keep.extend(slots.iter().map(AsRawFd::as_raw_fd));
    if keep.iter().any(|descriptor| *descriptor <= 2) { return Err(Error::Descriptors); }
    let identities = keep.iter().map(|descriptor| fd::observe(*descriptor).map(|o| (*descriptor, o.identity)))
        .collect::<Result<Vec<_>>>()?;
    let null = runner::dev_null(libc::O_RDWR)?;
    let null_identity = fd::observe(null.as_raw_fd())?.identity;
    for target in 0..=2 {
        if null.as_raw_fd() != target && unsafe { libc::dup3(null.as_raw_fd(), target, 0) } != target {
            return Err(Error::Descriptors);
        }
    }
    // Swept below unless it already is one of 0..=2; never closed twice.
    let _ = null.into_raw_fd();
    keep.extend([0, 1, 2]);
    fd::close_except(&keep)?;
    let last = keep.iter().copied().max().unwrap_or(2);
    for descriptor in 0..=last + 1 {
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if keep.contains(&descriptor) {
            if flags < 0 { return Err(Error::Descriptors); }
        } else if flags != -1 || super::errno() != libc::EBADF { return Err(Error::Descriptors); }
    }
    for descriptor in 0..=2 {
        if fd::observe(descriptor)?.identity != null_identity { return Err(Error::Descriptors); }
    }
    for (descriptor, identity) in &identities {
        if fd::observe(*descriptor)?.identity != *identity { return Err(Error::Descriptors); }
    }
    if fd::socket_option(control.as_raw_fd(), libc::SO_TYPE)? != libc::SOCK_SEQPACKET { return Err(Error::Descriptors); }
    // Step 2: `/helpers` RX, `/helper-tmp` RW, `/dev/null`, plus the union of
    // the bound helpers' grants. Never /home/employee, /config, /run, /tmp, /dev/shm.
    let grants: Vec<&[HelperFs]> = plan.helpers.iter().map(|helper| helper.filesystem.as_slice()).collect();
    let rules = helper_policy::launcher_landlock(&grants).into_iter()
        .map(|(path, class)| CString::new(path).map(|path| (path, class)).map_err(|_| Error::Landlock))
        .collect::<Result<Vec<_>>>()?;
    landlock(abi, &rules)?;
    // Step 3: common, then the self-filter F_L.
    let arch = Architecture::native().map_err(|_| Error::Seccomp)?;
    seccomp::common(arch).install().map_err(|_| Error::Seccomp)?;
    filters.launcher.install().map_err(|_| Error::Seccomp)?;
    let channel = LauncherChannel::new(plan.helpers.iter().map(|helper| helper.slot).collect())
        .map_err(|_| Error::Executable)?;
    Ok(Launcher { plan, control: Some(control), helpers, slots, binds, channel, live: [None, None], pid, drain: None, abi })
}

/// Install one Landlock layer. System paths are root-built and may be
/// usrmerge symlinks; anything under `/work` is runtime-writable and must be
/// a real directory (no symlink is followed).
fn landlock(abi: u32, rules: &[(CString, PathClass)]) -> Result<()> {
    let policy = Policy::for_abi(abi).map_err(|_| Error::Landlock)?;
    let mut owned = Vec::with_capacity(rules.len());
    let mut raw = Vec::with_capacity(rules.len());
    for (path, class) in rules {
        let workspace = path.as_bytes() == b"/work" || path.as_bytes().starts_with(b"/work/");
        let flags = libc::O_PATH | libc::O_CLOEXEC | if workspace { libc::O_NOFOLLOW | libc::O_DIRECTORY } else { 0 };
        let descriptor = unsafe { libc::open(path.as_ptr(), flags) };
        if descriptor < 0 {
            if path.as_c_str() == c"/lib64" && super::errno() == libc::ENOENT { continue; }
            return Err(Error::Landlock);
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let directory = fd::observe(descriptor.as_raw_fd())?.kind == FileKind::Directory;
        raw.push((descriptor.as_raw_fd(), policy.rights_on(*class, directory)));
        owned.push(descriptor);
    }
    crate::landlock::restrict_raw(&policy, &raw).map_err(|_| Error::Landlock)
}

fn slot_index(slot: HelperSlot) -> usize { match slot { HelperSlot::Foreground => 0, HelperSlot::Recording => 1 } }
fn kill_group(pid: i32) {
    // Before the child's setsid the group does not exist yet; signal both.
    unsafe { libc::kill(-pid, libc::SIGKILL); libc::kill(pid, libc::SIGKILL); }
}

struct Live {
    call: HelperCallId,
    slot: HelperSlot,
    pid: i32,
    pidfd: OwnedFd,
    timer: OwnedFd,
    /// Exec-status read end until EOF (exec'd) or a failure byte.
    status: Option<OwnedFd>,
    /// Recording only: the write end of the helper's stdin (`stop\n`).
    stdin: Option<OwnedFd>,
    failed: Option<LauncherError>,
    timed_out: bool,
    started: bool,
    scratch: CString,
}

struct Launcher<'a> {
    plan: &'a LauncherPlan,
    control: Option<OwnedFd>,
    helpers: Vec<(OwnedFd, Option<OwnedFd>)>,
    slots: [OwnedFd; 2],
    binds: Vec<(CString, u64)>,
    channel: LauncherChannel,
    live: [Option<Live>; 2],
    pid: i32,
    drain: Option<OwnedFd>,
    abi: u32,
}

#[derive(Clone, Copy)]
enum Wake { Status(usize), Exit(usize), Timer(usize), Drain, Control }

impl Launcher<'_> {
    fn run(mut self, filters: &Filters) -> ! {
        let mut wait: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut wait); }
        loop {
            // §2.5 (b): from SIGTERM on, refuse every Start and drain.
            if TERM.load(Ordering::Relaxed) && !self.channel.is_draining() { self.begin_drain(); }
            let idle = self.live.iter().all(Option::is_none);
            if idle && (self.channel.is_draining() || self.control.is_none()) { runner::exit_now(0); }
            let mut polls = Vec::with_capacity(9);
            let mut wakes = Vec::with_capacity(9);
            if let Some(control) = &self.control { polls.push(control.as_raw_fd()); wakes.push(Wake::Control); }
            if let Some(drain) = &self.drain { polls.push(drain.as_raw_fd()); wakes.push(Wake::Drain); }
            for (index, live) in self.live.iter().enumerate() {
                let Some(live) = live else { continue; };
                if let Some(status) = &live.status { polls.push(status.as_raw_fd()); wakes.push(Wake::Status(index)); }
                polls.push(live.timer.as_raw_fd()); wakes.push(Wake::Timer(index));
                polls.push(live.pidfd.as_raw_fd()); wakes.push(Wake::Exit(index));
            }
            let mut polls: Vec<_> = polls.into_iter().map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
            let result = unsafe { libc::ppoll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, std::ptr::null(), &wait) };
            if result < 0 {
                if super::errno() == libc::EINTR { continue; }
                runner::exit_now(1);
            }
            let woken: Vec<Wake> = polls.iter().zip(&wakes).filter(|(poll, _)| poll.revents != 0).map(|(_, wake)| *wake).collect();
            // Exec status before exits (Started precedes Ended), exits before
            // timers, and control last so a freed slot can take a new Start.
            for pass in 0..5 {
                for wake in &woken {
                    match (pass, *wake) {
                        (0, Wake::Status(index)) => self.on_status(index),
                        (1, Wake::Exit(index)) => self.on_exit(index),
                        (2, Wake::Timer(index)) => self.on_timer(index),
                        (3, Wake::Drain) => self.on_drain(),
                        (4, Wake::Control) => self.on_control(filters),
                        _ => {}
                    }
                }
            }
        }
    }

    fn send(&mut self, frame: FromLauncher) {
        let Some(control) = self.control.as_ref() else { return; };
        let sent = frame.encode().ok().is_some_and(|bytes| helper_wire::linux::send(control.as_fd(), &bytes, &[]).is_ok());
        if !sent { self.close_control(); }
    }
    /// EOF, a send failure or a protocol violation: no further Start can come;
    /// kill what runs, reap it, then exit. spawnd fails later calls itself.
    fn close_control(&mut self) {
        self.control = None;
        for live in self.live.iter().flatten() { kill_group(live.pid); }
    }

    fn begin_drain(&mut self) {
        if let Some(frame) = self.channel.begin_draining() { self.send(frame); }
        for live in self.live.iter().flatten() {
            match (live.slot, &live.stdin) {
                // Recording: flush within F (`stop\n`, the driver protocol).
                (HelperSlot::Recording, Some(stdin)) => write_stop(stdin.as_raw_fd()),
                _ => kill_group(live.pid),
            }
        }
        self.drain = timer(self.plan.drain_ms).ok();
        if self.drain.is_none() { for live in self.live.iter().flatten() { kill_group(live.pid); } }
    }
    fn on_drain(&mut self) {
        if let Some(drain) = &self.drain { drain_timer(drain.as_raw_fd()); }
        for live in self.live.iter().flatten() { kill_group(live.pid); }
    }

    fn on_status(&mut self, index: usize) {
        let Some(live) = self.live[index].as_mut() else { return; };
        let Some(status) = live.status.as_ref() else { return; };
        let mut byte = 0u8;
        let count = unsafe { libc::read(status.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if count < 0 && super::errno() == libc::EINTR { return; }
        live.status = None;
        if count == 1 {
            live.failed = Some(LauncherError::from_code(u16::from(byte))
                .filter(|reason| *reason != LauncherError::Protocol).unwrap_or(LauncherError::Internal));
            return;
        }
        if count < 0 { live.failed = Some(LauncherError::Internal); return; }
        // EOF without a failure byte: `execveat` replaced the child (or it died
        // before reporting; the reap decides the end, spawnd the outcome).
        if live.failed.is_none() && !live.started {
            live.started = true;
            let call = live.call;
            if let Ok(frame) = self.channel.started(call) { self.send(frame); }
        }
    }

    fn on_exit(&mut self, index: usize) {
        // The child is gone: its exec-status pipe holds a final byte or EOF.
        if self.live[index].as_ref().is_some_and(|live| live.status.is_some()) { self.on_status(index); }
        let Some(mut live) = self.live[index].take() else { return; };
        live.status = None;
        // The helper's remaining group dies with it; spawnd's slot cgroup.kill is the enforcement.
        unsafe { libc::kill(-live.pid, libc::SIGKILL); }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let reaped = loop {
            if unsafe { libc::waitid(libc::P_PIDFD, live.pidfd.as_raw_fd() as libc::id_t, &mut info, libc::WEXITED) } == 0 { break true; }
            if super::errno() != libc::EINTR { break false; }
        };
        let end = if let Some(reason) = live.failed { End::StartFailed(reason) }
            else if live.timed_out { End::TimedOut }
            else if reaped && info.si_code == libc::CLD_EXITED { End::Exited((unsafe { info.si_status() } & 0xff) as u8) }
            else {
                let signal = if reaped { unsafe { info.si_status() } } else { libc::SIGKILL };
                End::Signalled(u8::try_from(signal).ok().filter(|signal| (1..=64).contains(signal)).unwrap_or(9))
            };
        // Per-call order on the wire: Started before any non-StartFailed Ended.
        if !matches!(end, End::StartFailed(_)) && !live.started {
            if let Ok(frame) = self.channel.started(live.call) { self.send(frame); }
        }
        remove_scratch(&live.scratch);
        if let Ok(frame) = self.channel.ended(live.call, end) { self.send(frame); }
    }

    fn on_timer(&mut self, index: usize) {
        let Some(live) = self.live[index].as_mut() else { return; };
        drain_timer(live.timer.as_raw_fd());
        // Fast path only: spawnd's watchdog writes the slot's cgroup.kill at
        // effective timeout + 2 s even if this launcher is stopped [F5].
        live.timed_out = true;
        kill_group(live.pid);
    }

    fn on_control(&mut self, filters: &Filters) {
        let Some(control) = self.control.as_ref() else { return; };
        match helper_wire::linux::recv(control.as_fd(), MAX_FRAME_FDS, true) {
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
            Err(_) | Ok(None) => self.close_control(),
            Ok(Some((bytes, fds, receipt))) => match self.channel.receive(&bytes, receipt) {
                Err(_) => {
                    drop(fds);
                    self.send(FromLauncher::Error { code: LauncherError::Protocol });
                    self.close_control();
                }
                Ok(Command::Start(start)) => self.start(start, fds, filters),
                Ok(Command::Refuse { call, reason }) => {
                    drop(fds);
                    if let Ok(frame) = self.channel.refused(call, reason) { self.send(frame); }
                }
                Ok(Command::Stop { slot, .. }) => {
                    if let Some(stdin) = self.live[slot_index(slot)].as_ref().and_then(|live| live.stdin.as_ref()) {
                        write_stop(stdin.as_raw_fd());
                    }
                }
                Ok(Command::Kill { slot, .. }) => {
                    if let Some(live) = &self.live[slot_index(slot)] { kill_group(live.pid); }
                }
                Ok(Command::Ignore) => {}
            },
        }
    }

    fn start(&mut self, start: Start, fds: Vec<OwnedFd>, filters: &Filters) {
        match self.spawn(start, fds, filters) {
            Ok(live) => self.live[slot_index(start.slot)] = Some(live),
            Err(reason) => {
                if let Ok(frame) = self.channel.ended(start.call, End::StartFailed(reason)) { self.send(frame); }
            }
        }
    }

    fn bind(&self, name: &str) -> std::result::Result<u64, LauncherError> {
        self.binds.iter().find(|(bound, _)| bound.as_bytes() == name.as_bytes()).map(|(_, mount)| *mount)
            .ok_or(LauncherError::Internal)
    }

    fn spawn(&self, start: Start, fds: Vec<OwnedFd>, filters: &Filters) -> std::result::Result<Live, LauncherError> {
        use LauncherError::{Descriptors, Internal};
        let index = usize::from(start.helper);
        let helper = self.plan.helpers.get(index).ok_or(Internal)?;
        let filter = filters.helpers.get(index).ok_or(Internal)?;
        let (exec, script) = self.helpers.get(index).ok_or(Internal)?;
        if helper.slot != start.slot || fds.len() != START_FDS { return Err(Descriptors); }
        let mut fds = fds.into_iter();
        let (input, stdout, stderr) = match (fds.next(), fds.next(), fds.next()) {
            (Some(input), Some(stdout), Some(stderr)) => (input, stdout, stderr),
            _ => return Err(Descriptors),
        };
        check_input(input.as_raw_fd())?;
        let out = check_output(stdout.as_raw_fd())?;
        if check_output(stderr.as_raw_fd())? == out { return Err(Descriptors); }
        if unsafe { libc::lseek(input.as_raw_fd(), 0, libc::SEEK_SET) } != 0 { return Err(Descriptors); }
        // The root-verified exec fd on the helper registry mount, before every exec (§2.3).
        let registry = self.plan.mounts.get(usize::from(helper.mount)).ok_or(Internal)?.mount;
        runner::verify_executable(exec.as_raw_fd(), helper.program.identity, registry).map_err(|_| Descriptors)?;
        let entry = helper_policy::helper_file_name(helper.program.argv.first().ok_or(Internal)?.as_bytes()).ok_or(Internal)?;
        let entry = (CString::new(format!("/helpers/{entry}")).map_err(|_| Internal)?, self.bind(entry)?);
        let script = match (&helper.program.script, script) {
            (Some((identity, name)), Some(fd)) => {
                validation::validate(fd::observe(fd.as_raw_fd()).map_err(|_| Descriptors)?,
                    Expected::Script { identity: *identity, registry_mount: registry }).map_err(|_| Descriptors)?;
                let name = name.to_str().map_err(|_| Internal)?;
                Some((CString::new(format!("/helpers/{name}")).map_err(|_| Internal)?, *identity, self.bind(name)?))
            }
            (None, None) => None,
            _ => return Err(Internal),
        };
        let hex = start.call.hex();
        let scratch = CString::new(hex.as_str()).map_err(|_| Internal)?;
        make_scratch(&scratch)?;
        let spawned = (|| -> std::result::Result<Live, LauncherError> {
            let scratch_path = CString::new(format!("{HELPER_TMP_ROOT}/{hex}")).map_err(|_| Internal)?;
            // [F9] every scratch path, TMPDIR and HOME live in the private call directory.
            let environment = vec![CString::new(format!("HOME={HELPER_TMP_ROOT}/{hex}")).map_err(|_| Internal)?,
                CString::new(format!("TMPDIR={HELPER_TMP_ROOT}/{hex}")).map_err(|_| Internal)?];
            let (stdin_read, stdin_write) = if start.slot == HelperSlot::Recording {
                let (read, write) = pipe()?;
                set_nonblocking(write.as_raw_fd())?;
                (Some(read), Some(write))
            } else { (None, None) };
            let (status_read, status_write) = pipe()?;
            let timer = timer(start.timeout_ms.min(helper.timeout_ms))?;
            let child = Child { helper, filter, entry, script, scratch: scratch_path, environment,
                slot: self.slots[slot_index(start.slot)].as_raw_fd(), launcher: self.pid, abi: self.abi };
            let mut pidfd: RawFd = -1;
            // Raw clone (no clone3: common returns ENOSYS); CLONE_PIDFD carries no namespace flag.
            let pid = unsafe { libc::syscall(libc::SYS_clone, (libc::SIGCHLD as u64) | CLONE_PIDFD, 0usize,
                &mut pidfd as *mut RawFd, 0usize, 0usize) };
            if pid < 0 { return Err(LauncherError::Sandbox); }
            if pid == 0 { child.exec(input, stdout, stderr, stdin_read, status_write); }
            if pidfd < 0 {
                kill_group(pid as i32);
                unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), 0); }
                return Err(LauncherError::Sandbox);
            }
            // The call's fds now live only in the helper.
            drop((input, stdout, stderr, stdin_read, status_write));
            Ok(Live { call: start.call, slot: start.slot, pid: pid as i32, pidfd: unsafe { OwnedFd::from_raw_fd(pidfd) },
                timer, status: Some(status_read), stdin: stdin_write, failed: None, timed_out: false, started: false,
                scratch: scratch.clone() })
        })();
        if spawned.is_err() { remove_scratch(&scratch); }
        spawned
    }
}

fn check_input(descriptor: RawFd) -> std::result::Result<(), LauncherError> {
    let o = fd::observe(descriptor).map_err(|_| LauncherError::Descriptors)?;
    let sealed = o.seals.is_some_and(|seals| seals & validation::REQUIRED_SEALS == validation::REQUIRED_SEALS);
    if o.kind != FileKind::Regular || o.fs_type != validation::TMPFS_MAGIC || !sealed || !o.cloexec
        || o.size > MAX_HELPER_INPUT_BYTES || !matches!(o.access, Access::Read | Access::ReadWrite) {
        return Err(LauncherError::Descriptors);
    }
    Ok(())
}
fn check_output(descriptor: RawFd) -> std::result::Result<Identity, LauncherError> {
    let o = fd::observe(descriptor).map_err(|_| LauncherError::Descriptors)?;
    validation::validate(o, Expected::Output).map_err(|_| LauncherError::Descriptors)?;
    Ok(o.identity)
}
fn pipe() -> std::result::Result<(OwnedFd, OwnedFd), LauncherError> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 { return Err(LauncherError::Sandbox); }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
fn set_nonblocking(descriptor: RawFd) -> std::result::Result<(), LauncherError> {
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(LauncherError::Sandbox);
    }
    Ok(())
}
fn timer(ms: u64) -> std::result::Result<OwnedFd, LauncherError> {
    let raw = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) };
    if raw < 0 { return Err(LauncherError::Sandbox); }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let ms = ms.max(1);
    let value = libc::itimerspec { it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 },
        it_value: libc::timespec { tv_sec: (ms / 1000) as libc::time_t, tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long } };
    if unsafe { libc::timerfd_settime(fd.as_raw_fd(), 0, &value, std::ptr::null_mut()) } != 0 { return Err(LauncherError::Sandbox); }
    Ok(fd)
}
fn drain_timer(descriptor: RawFd) {
    let mut expirations = 0u64;
    unsafe { libc::read(descriptor, (&mut expirations as *mut u64).cast(), 8); }
}
fn write_stop(descriptor: RawFd) {
    // Non-blocking end: a stopped or full helper never blocks the launcher.
    unsafe { libc::write(descriptor, b"stop\n".as_ptr().cast(), 5); }
}

fn open_scratch_root() -> Option<OwnedFd> {
    let raw = unsafe { libc::open(c"/helper-tmp".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw) })
}
/// `/helper-tmp/<call>` 0700. Call ids are fresh per run and the runtime cannot
/// reach `/helper-tmp`, so an existing entry is refused, never reused [F9].
fn make_scratch(name: &CStr) -> std::result::Result<(), LauncherError> {
    let root = open_scratch_root().ok_or(LauncherError::Sandbox)?;
    if unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) } != 0 { return Err(LauncherError::Sandbox); }
    Ok(())
}
/// No-follow removal with `openat(O_NOFOLLOW|O_DIRECTORY)` + `unlinkat` only,
/// bounded in entries and depth. Residue (if any) dies with the run's tmpfs.
fn remove_scratch(name: &CStr) {
    let Some(root) = open_scratch_root() else { return; };
    let mut budget = SCRATCH_BUDGET;
    remove_entry(root.as_raw_fd(), name, 0, &mut budget);
}
fn remove_entry(parent: RawFd, name: &CStr, depth: usize, budget: &mut usize) {
    if *budget == 0 { return; }
    *budget -= 1;
    if unsafe { libc::unlinkat(parent, name.as_ptr(), 0) } == 0 { return; }
    if super::errno() != libc::EISDIR || depth >= SCRATCH_DEPTH { return; }
    let raw = unsafe { libc::openat(parent, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 { return; }
    let directory = unsafe { OwnedFd::from_raw_fd(raw) };
    for _ in 0..64 {
        let names = list_directory(directory.as_raw_fd());
        if names.is_empty() { break; }
        for child in &names { remove_entry(directory.as_raw_fd(), child, depth + 1, budget); }
        if *budget == 0 || unsafe { libc::lseek(directory.as_raw_fd(), 0, libc::SEEK_SET) } != 0 { break; }
    }
    drop(directory);
    unsafe { libc::unlinkat(parent, name.as_ptr(), libc::AT_REMOVEDIR); }
}
/// Raw `getdents64` (no libc DIR stream, which would `fstat`).
fn list_directory(directory: RawFd) -> Vec<CString> {
    let mut names = Vec::new();
    let mut buffer = vec![0u64; 1024];
    loop {
        let read = unsafe { libc::syscall(libc::SYS_getdents64, directory, buffer.as_mut_ptr(), buffer.len() * 8) };
        if read <= 0 { return names; }
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), read as usize) };
        let mut offset = 0usize;
        while offset + 19 < bytes.len() {
            let length = usize::from(u16::from_ne_bytes([bytes[offset + 16], bytes[offset + 17]]));
            if length < 20 || offset + length > bytes.len() { return names; }
            let raw = &bytes[offset + 19..offset + length];
            let name = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
            if name != b"." && name != b".." && !name.is_empty() {
                if let Ok(name) = CString::new(name) { names.push(name); }
            }
            offset += length;
            if names.len() >= 4096 { return names; }
        }
    }
}

/// Everything a helper child needs, prepared by the launcher before fork.
struct Child<'a> {
    helper: &'a HelperProgram,
    filter: &'a Filter,
    /// `/helpers/<entry>` and its bind mount id.
    entry: (CString, u64),
    script: Option<(CString, Identity, u64)>,
    /// `/helper-tmp/<call>`.
    scratch: CString,
    environment: Vec<CString>,
    /// This call's slot `cgroup.procs`.
    slot: RawFd,
    launcher: i32,
    abi: u32,
}
impl Child<'_> {
    fn exec(self, input: OwnedFd, stdout: OwnedFd, stderr: OwnedFd, stdin: Option<OwnedFd>, status: OwnedFd) -> ! {
        // A separate reporting copy: the plan consumes `status`, and the sweep
        // closes this copy once the plan's own exec-status fd exists.
        let mut report = unsafe { libc::fcntl(status.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if report < 0 { runner::exit_now(126); }
        let reason = match self.prepare(input, stdout, stderr, stdin, status, &mut report) {
            Err(reason) => reason,
            Ok(never) => match never {},
        };
        let byte = reason.code() as u8;
        unsafe { libc::write(report, (&byte as *const u8).cast(), 1); }
        runner::exit_now(126)
    }

    fn prepare(&self, input: OwnedFd, stdout: OwnedFd, stderr: OwnedFd, stdin: Option<OwnedFd>, status: OwnedFd,
        report: &mut RawFd) -> std::result::Result<std::convert::Infallible, LauncherError> {
        use LauncherError::{Descriptors, Exec, Internal, Sandbox};
        reset_signals()?;
        // §2.4 process [F1]: own session; dies with the launcher.
        if unsafe { libc::setsid() } < 0 { return Err(Sandbox); }
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0
            || unsafe { libc::getppid() } != self.launcher { return Err(Sandbox); }
        // §2.4 cgroup [F6]: into hx/fg or hx/rec through the root-opened file,
        // before Landlock, F_H and exec.
        if unsafe { libc::write(self.slot, b"0".as_ptr().cast(), 1) } != 1 { return Err(Sandbox); }
        // The bind at /helpers/<entry> must be the root-verified file (inode on
        // the registry device) on its recorded bind mount.
        let exec = open_helper_file(&self.entry.0, self.helper.program.identity, self.entry.1, true)?;
        if let Some((path, identity, mount)) = &self.script { drop(open_helper_file(path, *identity, *mount, false)?); }
        if unsafe { libc::chdir(self.scratch.as_ptr()) } != 0 { return Err(Sandbox); }
        // Stacked Landlock under the launcher's: own code, private scratch and
        // the binding's fixed grants. No /home/employee, /config, /run, /runtime, /tmp, /dev/shm.
        let mut rules = vec![(self.entry.0.clone(), PathClass::ReadExecute), (self.scratch.clone(), PathClass::Ephemeral)];
        if let Some((path, ..)) = &self.script { rules.push((path.clone(), PathClass::ReadExecute)); }
        if self.helper.filesystem.contains(&HelperFs::WorkRecordings) {
            unsafe { libc::mkdirat(libc::AT_FDCWD, c"/work/recordings".as_ptr(), 0o700); }
        }
        for (path, class) in helper_policy::helper_landlock(&self.helper.filesystem) {
            rules.push((CString::new(path).map_err(|_| Internal)?, class));
        }
        landlock(self.abi, &rules).map_err(|_| Sandbox)?;
        // [F1] exact plan: 0 = /dev/null or the recording stdin, 1/2 = the
        // call's pipes, 3 = /dev/null, 4 = sealed input memfd; nothing else.
        let null = runner::dev_null(libc::O_RDWR).map_err(|_| Descriptors)?;
        let mut sources = Vec::new();
        let mut owned = Vec::new();
        let mut add = |descriptor: OwnedFd, destination: Destination| {
            sources.push(Source { fd: descriptor.as_raw_fd(), destination });
            owned.push(descriptor);
        };
        match stdin {
            Some(stdin) => { add(stdin, Destination::Runtime(vec![0])); add(null, Destination::Runtime(vec![3])); }
            None => add(null, Destination::Runtime(vec![0, 3])),
        }
        add(stdout, Destination::Runtime(vec![1]));
        add(stderr, Destination::Runtime(vec![2]));
        add(input, Destination::Runtime(vec![4]));
        add(exec, Destination::Executable);
        add(status, Destination::ExecStatus);
        let plan = FdPlan::new(sources, fd::soft_limit().map_err(|_| Descriptors)?).map_err(|_| Descriptors)?;
        // Reorders, closes every other fd and reads the whole table back.
        let (exec, status) = fd::reorder_for_exec(plan, owned, None).map_err(|_| Descriptors)?;
        *report = status.ok_or(Descriptors)?.into_raw_fd();
        let exec = exec.into_raw_fd();
        // §2.4 rlimits [F3]: hard = soft via prlimit64(0, …); no RLIMIT_AS / NPROC.
        limit(libc::RLIMIT_CORE as u32, 0)?;
        limit(libc::RLIMIT_NOFILE as u32, u64::from(self.helper.open_files))?;
        limit(libc::RLIMIT_FSIZE as u32, self.helper.file_size_bytes)?;
        let mut argv: Vec<_> = self.helper.program.argv.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut env: Vec<_> = self.environment.iter().map(|value| value.as_ptr()).collect();
        env.push(std::ptr::null());
        // F_H immediately before execveat; the kernel enforces F_L ∩ F_H.
        if self.filter.install().is_err() { return Err(Sandbox); }
        unsafe { libc::syscall(libc::SYS_execveat, exec, c"".as_ptr(), argv.as_ptr(), env.as_ptr(), libc::AT_EMPTY_PATH); }
        Err(Exec)
    }
}

fn reset_signals() -> std::result::Result<(), LauncherError> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    unsafe { libc::sigemptyset(&mut action.sa_mask); }
    for signal in [libc::SIGTERM, libc::SIGPIPE] {
        // SIG_IGN would survive exec; the launcher's SIGTERM block would too.
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 { return Err(LauncherError::Sandbox); }
    }
    let mut empty: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut empty); }
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) } != 0 { return Err(LauncherError::Sandbox); }
    Ok(())
}

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }
/// Open `/helpers/<name>` read-only with no symlink at all and check it is the
/// signed file: same inode and device as the root-verified registry file, on
/// the bind mount `attach_helpers` read back, root-owned, not writable or setid.
fn open_helper_file(path: &CStr, identity: Identity, mount: u64, executable: bool)
    -> std::result::Result<OwnedFd, LauncherError> {
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64, mode: 0, resolve: 0x04 | 0x02 };
    let raw = unsafe { libc::syscall(libc::SYS_openat2, libc::AT_FDCWD, path.as_ptr(), &how, std::mem::size_of::<OpenHow>()) };
    if raw < 0 { return Err(LauncherError::Descriptors); }
    let file = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
    let o = fd::observe(file.as_raw_fd()).map_err(|_| LauncherError::Descriptors)?;
    let same = o.identity.inode == identity.inode && o.identity.dev_major == identity.dev_major
        && o.identity.dev_minor == identity.dev_minor && o.identity.mount == mount;
    if !same || o.kind != FileKind::Regular || o.uid != 0 || o.mode & 0o6022 != 0 || o.access != Access::Read || !o.cloexec
        || (executable && o.mode & 0o111 == 0) { return Err(LauncherError::Descriptors); }
    if executable {
        let mut magic = [0u8; 4];
        if unsafe { libc::pread(file.as_raw_fd(), magic.as_mut_ptr().cast(), magic.len(), 0) } != 4 || magic != *b"\x7fELF" {
            return Err(LauncherError::Exec);
        }
    }
    Ok(file)
}

fn limit(resource: u32, wanted: u64) -> std::result::Result<(), LauncherError> {
    let mut current: libc::rlimit64 = unsafe { std::mem::zeroed() };
    if unsafe { libc::syscall(libc::SYS_prlimit64, 0, resource, std::ptr::null::<libc::rlimit64>(), &mut current) } != 0 {
        return Err(LauncherError::Sandbox);
    }
    // Never above the inherited hard limit: raising it would need CAP_SYS_RESOURCE.
    let value = wanted.min(current.rlim_max);
    let new = libc::rlimit64 { rlim_cur: value, rlim_max: value };
    if unsafe { libc::syscall(libc::SYS_prlimit64, 0, resource, &new, std::ptr::null_mut::<libc::rlimit64>()) } != 0 {
        return Err(LauncherError::Sandbox);
    }
    let mut back: libc::rlimit64 = unsafe { std::mem::zeroed() };
    if unsafe { libc::syscall(libc::SYS_prlimit64, 0, resource, std::ptr::null::<libc::rlimit64>(), &mut back) } != 0
        || back.rlim_cur != value || back.rlim_max != value { return Err(LauncherError::Sandbox); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scratch_listing_parses_getdents_and_cleanup_is_no_follow() {
        // Host-side check of the no-follow removal on a private temporary tree.
        let base = std::env::temp_dir().join(format!("duduclaw-hx3b-scratch-{}", std::process::id()));
        std::fs::create_dir(&base).unwrap();
        let call = base.join("call");
        std::fs::create_dir_all(call.join("a/b/c")).unwrap();
        std::fs::write(call.join("a/b/c/file"), b"x").unwrap();
        std::fs::write(call.join("top"), b"y").unwrap();
        let outside = base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"z").unwrap();
        std::os::unix::fs::symlink(&outside, call.join("link")).unwrap();
        let parent = std::fs::File::open(&base).unwrap();
        let mut names = list_directory(AsRawFd::as_raw_fd(&std::fs::File::open(&call).unwrap()));
        names.sort();
        assert_eq!(names, vec![CString::new("a").unwrap(), CString::new("link").unwrap(), CString::new("top").unwrap()]);
        let mut budget = SCRATCH_BUDGET;
        remove_entry(parent.as_raw_fd(), c"call", 0, &mut budget);
        assert!(!call.exists(), "scratch tree removed");
        assert!(outside.join("keep").exists(), "a symlink inside scratch is unlinked, never followed");
        std::fs::remove_dir_all(&base).unwrap();
    }
    #[test]
    fn helper_child_rlimits_are_hard_equal_soft() {
        // Runs in a forked child: lowering limits is irreversible for that process.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let ok = limit(libc::RLIMIT_CORE as u32, 0).is_ok() && limit(libc::RLIMIT_FSIZE as u32, 1 << 20).is_ok()
                && limit(libc::RLIMIT_NOFILE as u32, 64).is_ok();
            let mut nofile: libc::rlimit64 = unsafe { std::mem::zeroed() };
            unsafe { libc::syscall(libc::SYS_prlimit64, 0, libc::RLIMIT_NOFILE as u32, std::ptr::null::<libc::rlimit64>(), &mut nofile); }
            let raise = libc::rlimit64 { rlim_cur: 65, rlim_max: 65 };
            let raised = unsafe { libc::syscall(libc::SYS_prlimit64, 0, libc::RLIMIT_NOFILE as u32, &raise,
                std::ptr::null_mut::<libc::rlimit64>()) } == 0;
            let exact = nofile.rlim_cur == 64 && nofile.rlim_max == 64;
            // Root (CAP_SYS_RESOURCE) may raise; an unprivileged child may not.
            let expect_raise = unsafe { libc::geteuid() } == 0;
            unsafe { libc::_exit(if ok && exact && raised == expect_raise { 0 } else { 1 }); }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "{status}");
    }
}
