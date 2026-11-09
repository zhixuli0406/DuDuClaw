// HX3b root scenarios (D8 revision §7, `iso_root_helper_*`). The coordinator
// plays spawnd: it creates the cgroup leaves, the helper registry clone and the
// launcher control socketpair, then drives the launcher with real `helper_wire`
// frames through `SpawndChannel`. Debug-only fixture; never production policy.
use super::*;
use super::launch::{children, memfd, pidfd, prepare, readable, socketpair, Cleanup, RUN};
use duduclaw_isolation::ipc::{HelperCallId, HelperSlot};
use duduclaw_run::{helper_policy::{HelperFs, SelfOnlyCall, PR_SET_DUMPABLE},
    helper_wire::{self, End, FromLauncher, LauncherError, Receipt, SpawndChannel, SpawndEvent, Start, ToLauncher},
    linux::{bootstrap, sealed_registry::{self, SealedRegistry}, runner::{self, HelperProgram, LauncherPlan, MountIdentity, Program}},
    seccomp::RuntimeProfile};
use std::time::{Duration, Instant};

const HELPER_INPUT: &[u8] = b"helper-input-fixture";
/// The parent's forwarder as root puts it in the runtime environment (`policy::runtime_environment`).
pub(super) const FIXTURE_PROXY: &str = "http://127.0.0.1:18080";
const OPEN_FILES: u64 = 64;
const FILE_SIZE: u64 = 1 << 20;

/// Fixture-owned cgroup v2 leaves, laid out like spawnd's (`helper_cgroup`).
struct Cgroups(PathBuf);
impl Cgroups {
    fn new() -> Self {
        let base = PathBuf::from(format!("/sys/fs/cgroup/duduclaw-hx3b-fixture-{}", unsafe { libc::getpid() }));
        assert!(Path::new("/sys/fs/cgroup/cgroup.controllers").exists(), "HX helpers require the cgroup v2 unified hierarchy");
        for leaf in ["", "rt-init", "rt-runtime", "hx", "hx/fg", "hx/rec"] { std::fs::create_dir(base.join(leaf)).unwrap(); }
        Self(base)
    }
    fn open(&self, file: &str) -> OwnedFd {
        let path = cpath(&self.0.join(file));
        let raw = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        assert!(raw >= 0, "open {file}");
        unsafe { OwnedFd::from_raw_fd(raw) }
    }
    /// Order of `helper_policy::HELPER_CGROUP_FDS`.
    fn fds(&self) -> [OwnedFd; 4] {
        [self.open("rt-runtime/cgroup.procs"), self.open("rt-runtime/cgroup.kill"),
            self.open("hx/fg/cgroup.procs"), self.open("hx/rec/cgroup.procs")]
    }
    fn procs(&self, leaf: &str) -> Vec<i32> {
        std::fs::read_to_string(self.0.join(leaf).join("cgroup.procs")).unwrap().split_whitespace()
            .map(|pid| pid.parse().unwrap()).collect()
    }
}
impl Drop for Cgroups {
    fn drop(&mut self) {
        for leaf in ["hx/fg", "hx/rec", "rt-runtime", "rt-init"] {
            let _ = std::fs::write(self.0.join(leaf).join("cgroup.kill"), b"1");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        for leaf in ["hx/fg", "hx/rec", "hx", "rt-runtime", "rt-init", ""] {
            while std::fs::remove_dir(self.0.join(leaf)).is_err() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn status_field(pid: i32, field: &str) -> String {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    text.lines().find_map(|line| line.strip_prefix(field).map(|value| value.trim().to_owned()))
        .unwrap_or_else(|| panic!("{field} missing for {pid}"))
}
/// The pid as seen inside the run's PID namespace (last NSpid entry).
fn namespace_pid(pid: i32) -> i32 { status_field(pid, "NSpid:").split_whitespace().last().unwrap().parse().unwrap() }
fn oom_score_adj(pid: i32) -> i32 { std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).unwrap().trim().parse().unwrap() }
fn call(byte: u8) -> HelperCallId { HelperCallId::new([byte; 16]).unwrap() }

fn helper_program(identity: duduclaw_run::validation::Identity, mode: &str) -> Program {
    // Offline helper: no `socket`; prlimit64/prctl only as signed self-only
    // exceptions, so F_H ⊆ the fixture parent profile (all of 0..512).
    let arch = Architecture::native().unwrap();
    let socket = match arch { Architecture::X86_64 => 41, Architecture::Aarch64 => 198 };
    let seccomp = RuntimeProfile { allowed_syscalls: (0..512).filter(|nr| ![socket, arch.prlimit64(), arch.prctl()].contains(nr)).collect() };
    let seccomp_digest = seccomp.digest(arch).unwrap();
    Program { identity, script: None, argv: vec![CString::new("/helpers/helper").unwrap(),
        CString::new(format!("--fixture-helper={mode}")).unwrap()], environment: vec![], seccomp, seccomp_digest }
}

/// Scenario → (foreground helper mode, recording helper mode, drain window F).
fn modes(scenario: &str) -> (&'static str, &'static str, u64) {
    match scenario {
        "helper-fds" => ("fds", "sleep", 5_000),
        "helper-fs" | "helper-scratch" => ("fs", "sleep", 5_000),
        "helper-socket" | "helper-exec" => ("socket", "sleep", 5_000),
        "helper-rlimit" => ("rlimit", "sleep", 5_000),
        "helper-descendants" => ("sleep", "slow-stop", 8_000),
        "helper-stopped" => ("sleep", "sleep", 1_000),
        _ => ("sleep", "sleep", 5_000),
    }
}

struct Session {
    child: i32,
    bootstrap: OwnedFd,
    mcp: OwnedFd,
    control: OwnedFd,
    channel: SpawndChannel,
    records: Vec<(u8, u8, u32)>,
    terminal: Option<(u8, u32)>,
    mcp_bytes: Vec<u8>,
    epoch: Instant,
    cgroups: Cgroups,
    /// Read end of the trampoline's fd 1/2: kept open, must stay empty.
    output: File,
    _cleanup: Cleanup,
}
impl Session {
    fn new(root: &Path, scenario: &str) -> Self {
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }, 0);
        mounts::enter_namespaces().expect("coordinator private mount namespace");
        let (mut plan, mut descriptors, mcp, _) = prepare(root, scenario);
        // The egress (recording-slot) helper's forwarder comes from the runtime's own environment.
        plan.program.environment.push(CString::new(format!("HTTPS_PROXY={FIXTURE_PROXY}")).unwrap());
        // A distinct registry file for the helper, so /runtime must mask it.
        let mut source = File::open("/proc/self/exe").unwrap();
        let mut copy = File::create(root.join("runtime/helper")).unwrap();
        std::io::copy(&mut source, &mut copy).unwrap();
        drop((source, copy));
        std::fs::set_permissions(root.join("runtime/helper"), Permissions::from_mode(0o755)).unwrap();
        // spawnd's `prepare_helpers`: one RO clone of the same registry directory.
        let registry = detached(&root.join("runtime"), 7 | 0x0020_0000);
        let open_helper = || fd::open_beneath(registry.fd.as_raw_fd(), c"helper", libc::O_RDONLY).unwrap();
        let (foreground_exec, recording_exec) = (open_helper(), open_helper());
        let identity = fd::observe(foreground_exec.as_raw_fd()).unwrap().identity;
        let (foreground, recording, drain_ms) = modes(scenario);
        let binding = |mode: &str, slot: HelperSlot| HelperProgram { program: helper_program(identity, mode),
            self_only: vec![SelfOnlyCall::Prlimit64SelfOnly, SelfOnlyCall::Prctl { options: vec![PR_SET_DUMPABLE] }],
            mount: 0, slot, filesystem: if slot == HelperSlot::Recording { vec![HelperFs::SystemRo, HelperFs::WorkRecordings] }
                else { vec![HelperFs::SystemRo, HelperFs::Work] },
            open_files: OPEN_FILES as u32, file_size_bytes: FILE_SIZE, timeout_ms: 30_000, egress: slot == HelperSlot::Recording,
            files: vec![] };
        plan.launcher = Some(LauncherPlan { helpers: vec![binding(foreground, HelperSlot::Foreground),
            binding(recording, HelperSlot::Recording)], mounts: vec![MountIdentity { mount: registry.expected_mount_id,
            fs_type: registry.expected_fs_type }], tmpfs_bytes: 1 << 20, drain_ms });
        let (control, launcher_control) = socketpair(libc::SOCK_SEQPACKET);
        let cgroups = Cgroups::new();
        descriptors.push(launcher_control);
        descriptors.push(registry.fd);
        descriptors.push(foreground_exec);
        descriptors.push(recording_exec);
        descriptors.extend(cgroups.fds());
        let (bootstrap, child_bootstrap) = socketpair(libc::SOCK_SEQPACKET);
        let (output, write_output) = pipe();
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            mount_fixture_tmpfs(c"/tmp", c"size=8388608,nr_inodes=256,mode=1777");
            let channel = unsafe { libc::fcntl(child_bootstrap.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 100) };
            let stdout = unsafe { libc::fcntl(write_output.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 100) };
            assert!(channel >= 100 && stdout >= 100);
            assert_eq!(unsafe { libc::dup2(channel, 0) }, 0);
            assert_eq!(unsafe { libc::dup2(stdout, 1) }, 1);
            assert_eq!(unsafe { libc::dup2(stdout, 2) }, 2);
            unsafe { runner::serve(&mut SealedRegistry::default()) }
        }
        let cleanup = Cleanup;
        drop((child_bootstrap, write_output));
        assert!(readable(bootstrap.as_raw_fd(), 10_000), "bootstrap challenge timed out");
        let challenge = bootstrap::receive_challenge(bootstrap.as_raw_fd()).unwrap();
        let rights: Vec<_> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
        sealed_registry::send(bootstrap.as_raw_fd(), challenge, RUN, plan, &rights).unwrap();
        drop(descriptors);
        Self { child, bootstrap, mcp, control, channel: SpawndChannel::new(vec![HelperSlot::Foreground, HelperSlot::Recording]).unwrap(),
            records: Vec::new(), terminal: None, mcp_bytes: Vec::new(), epoch: Instant::now(), cgroups, output, _cleanup: cleanup }
    }
    fn deadline() -> Instant { Instant::now() + Duration::from_secs(20) }
    /// One poll round over bootstrap receipts and runtime MCP bytes.
    fn pump(&mut self, milliseconds: i32) {
        let mut polls = [libc::pollfd { fd: self.bootstrap.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.mcp.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if self.terminal.is_some() { polls[0].fd = -1; }
        assert!(unsafe { libc::poll(polls.as_mut_ptr(), 2, milliseconds) } >= 0);
        if polls[0].revents & libc::POLLIN != 0 {
            let (packet, rights) = bootstrap::receive(self.bootstrap.as_raw_fd()).unwrap();
            assert!(rights.is_empty());
            duduclaw_run::status::StatusRecord::decode(&packet, 0).unwrap();
            let record = (packet[4], packet[5], u32::from_be_bytes(packet[8..12].try_into().unwrap()));
            self.records.push(record);
            if matches!(record.0, 4 | 5) { self.terminal = Some((record.0, record.2)); }
        } else if polls[0].revents & libc::POLLHUP != 0 && self.terminal.is_none() {
            panic!("bootstrap EOF without terminal status: {:?}", self.records);
        }
        if polls[1].revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 256];
            let count = unsafe { libc::recv(self.mcp.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), libc::MSG_DONTWAIT) };
            if count > 0 { self.mcp_bytes.extend_from_slice(&bytes[..count as usize]); }
        }
    }
    fn wait_record(&mut self, kind: u8) {
        let deadline = Self::deadline();
        while !self.records.iter().any(|record| record.0 == kind) {
            assert!(Instant::now() < deadline, "receipt {kind} timed out: {:?}", self.records);
            assert!(self.terminal.is_none() || kind == 4, "run ended early: {:?}", self.records);
            self.pump(100);
        }
    }
    fn wait_ready(&mut self) {
        let deadline = Self::deadline();
        while !self.mcp_bytes.starts_with(b"READY") {
            assert!(Instant::now() < deadline && self.terminal.is_none(), "runtime READY: {:?}", self.records);
            self.pump(100);
        }
        self.mcp_bytes.drain(..5);
        self.wait_record(7);
    }
    fn command(&mut self, text: &str) {
        let line = format!("{text}\n");
        assert_eq!(unsafe { libc::send(self.mcp.as_raw_fd(), line.as_ptr().cast(), line.len(), libc::MSG_NOSIGNAL) }, line.len() as isize);
        if text == "G" { return; }
        let deadline = Self::deadline();
        while !self.mcp_bytes.starts_with(b"OK\n") {
            assert!(Instant::now() < deadline && self.terminal.is_none(), "runtime command {text}: {:?}", self.records);
            self.pump(100);
        }
        self.mcp_bytes.drain(..3);
    }
    fn init(&self) -> i32 {
        let init = children(self.child);
        assert_eq!(init.len(), 1, "one namespace init");
        init[0]
    }
    /// (runtime, launcher) host pids among init's children.
    fn runtime_and_launcher(&self) -> (i32, i32) {
        let runtime_leaf = self.cgroups.procs("rt-runtime");
        let kids = children(self.init());
        let runtime = *kids.iter().find(|pid| runtime_leaf.contains(pid)).expect("runtime moved into rt-runtime");
        let launcher: Vec<_> = kids.into_iter().filter(|pid| *pid != runtime).collect();
        assert_eq!(launcher.len(), 1, "exactly one launcher beside the runtime");
        (runtime, launcher[0])
    }
    fn now_ms(&self) -> u64 { self.epoch.elapsed().as_millis() as u64 }
    fn start(&mut self, byte: u8, helper: u8, slot: HelperSlot) -> (File, File) {
        let frame = self.channel.start(Start { call: call(byte), helper, slot, timeout_ms: 30_000 }).unwrap().encode().unwrap();
        let input = memfd(HELPER_INPUT);
        let (stdout, stdout_write) = pipe();
        let (stderr, stderr_write) = pipe();
        helper_wire::linux::send(self.control.as_fd(), &frame,
            &[input.as_fd(), stdout_write.as_fd(), stderr_write.as_fd()]).unwrap();
        (stdout, stderr)
    }
    fn raw_frame(&mut self) -> Option<(Vec<u8>, Receipt)> {
        let deadline = Self::deadline();
        loop {
            assert!(Instant::now() < deadline, "launcher frame timed out: {:?}", self.records);
            if readable(self.control.as_raw_fd(), 100) {
                return helper_wire::linux::recv(self.control.as_fd(), 0, true).unwrap().map(|(bytes, fds, receipt)| {
                    assert!(fds.is_empty()); (bytes, receipt)
                });
            }
            self.pump(0);
        }
    }
    fn event(&mut self) -> SpawndEvent {
        let (bytes, receipt) = self.raw_frame().expect("control channel open");
        let now = self.now_ms();
        self.channel.receive(now, &bytes, receipt).unwrap()
    }
    fn expect_started(&mut self, byte: u8) {
        assert!(matches!(self.event(), SpawndEvent::Started { call: id, .. } if id == call(byte)));
    }
    fn expect_ended(&mut self, byte: u8) -> End {
        match self.event() {
            SpawndEvent::Ended { call: id, end, .. } if id == call(byte) => end,
            other => panic!("expected Ended, got {other:?}"),
        }
    }
    fn kill(&mut self, byte: u8) {
        let frame = self.channel.kill(call(byte)).unwrap().unwrap().encode().unwrap();
        helper_wire::linux::send(self.control.as_fd(), &frame, &[]).unwrap();
    }
    fn finish(mut self) -> (u8, u32) {
        let deadline = Self::deadline();
        while self.terminal.is_none() {
            assert!(Instant::now() < deadline, "run did not end: {:?}", self.records);
            self.pump(100);
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(self.child, &mut status, 0) }, self.child);
        assert!(!self.records.iter().any(|record| record.0 == 0), "isolation error receipt: {:?}", self.records);
        // The launcher dup'd /dev/null over 0/1/2: nothing reaches the run's output pipes.
        let mut captured = Vec::new();
        let _ = unsafe { libc::fcntl(self.output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let _ = self.output.read_to_end(&mut captured);
        assert!(captured.is_empty(), "unexpected run output: {}", String::from_utf8_lossy(&captured));
        self.terminal.unwrap()
    }
}

fn read_line(file: &mut File) -> String {
    assert!(readable(file.as_raw_fd(), 10_000), "helper output timed out");
    let mut bytes = Vec::new();
    let mut byte = [0u8];
    while file.read(&mut byte).unwrap() == 1 { if byte[0] == b'\n' { break; } bytes.push(byte[0]); }
    String::from_utf8(bytes).unwrap()
}
fn run_to_exit_zero(session: &mut Session) {
    let (mut stdout, mut stderr) = session.start(1, 0, HelperSlot::Foreground);
    session.expect_started(1);
    assert_eq!(session.expect_ended(1), End::Exited(0));
    let mut output = Vec::new(); stdout.read_to_end(&mut output).unwrap();
    let mut errors = Vec::new(); stderr.read_to_end(&mut errors).unwrap();
    assert!(errors.is_empty(), "helper stderr: {}", String::from_utf8_lossy(&errors));
    assert_eq!(output, b"HELPER-OK\n");
}

pub(super) fn run(root: &Path, scenario: &str) {
    assert_eq!(std::env::args_os().count(), 1, "serve fixture must have no arguments");
    let mut session = Session::new(root, scenario);
    match scenario {
        "helper-proc" => {
            session.wait_ready();
            let (mut stdout, _stderr) = session.start(1, 0, HelperSlot::Foreground);
            session.expect_started(1);
            assert_eq!(read_line(&mut stdout), "SLEEPING");
            let (_, launcher) = session.runtime_and_launcher();
            let helper = children(launcher);
            assert_eq!(helper.len(), 1, "one helper under the launcher");
            // Same uid as the runtime: only Landlock domain scoping hides them.
            for pid in [launcher, helper[0]] {
                assert_eq!(status_field(pid, "Uid:").split_whitespace().next().unwrap(), EMPLOYEE.to_string());
                session.command(&format!("P{}", namespace_pid(pid)));
            }
            session.kill(1);
            assert_eq!(session.expect_ended(1), End::Signalled(9));
        }
        "helper-launcher-fds" => {
            // At the isolation receipt the launcher has already closed its
            // status and ready pipes (init waited for it before all_isolated).
            session.wait_record(7);
            let (_, launcher) = session.runtime_and_launcher();
            let mut fds: Vec<i32> = std::fs::read_dir(format!("/proc/{launcher}/fd")).unwrap()
                .map(|entry| entry.unwrap().file_name().to_str().unwrap().parse().unwrap()).collect();
            fds.sort_unstable();
            // 0/1/2 /dev/null + control + two helper executables + two slot cgroup files.
            assert_eq!(fds.len(), 8, "launcher fd table {fds:?}");
            let (mut sockets, mut files) = (0, 0);
            for descriptor in &fds {
                let metadata = std::fs::metadata(format!("/proc/{launcher}/fd/{descriptor}")).unwrap();
                use std::os::unix::fs::{FileTypeExt, MetadataExt};
                if *descriptor <= 2 {
                    assert!(metadata.file_type().is_char_device() && metadata.rdev() == libc::makedev(1, 3), "fd {descriptor}");
                } else if metadata.file_type().is_socket() { sockets += 1; }
                else { assert!(metadata.is_file(), "fd {descriptor} is neither file nor socket (a pipe leaked)"); files += 1; }
            }
            assert_eq!((sockets, files), (1, 4));
            assert_eq!(status_field(launcher, "Seccomp:"), "2");
            assert_eq!(status_field(launcher, "NoNewPrivs:"), "1");
            session.wait_ready();
        }
        "helper-fds" | "helper-fs" | "helper-socket" | "helper-rlimit" => {
            session.wait_ready();
            run_to_exit_zero(&mut session);
        }
        "helper-oom" => {
            session.wait_ready();
            let (runtime, launcher) = session.runtime_and_launcher();
            assert_eq!(oom_score_adj(session.child), -1000, "supervisor (outside the PID namespace) is protected");
            for pid in [session.init(), launcher, runtime] { assert_eq!(oom_score_adj(pid), 0, "{pid}"); }
            let (mut stdout, _stderr) = session.start(1, 0, HelperSlot::Foreground);
            session.expect_started(1);
            assert_eq!(read_line(&mut stdout), "SLEEPING");
            for helper in children(launcher) { assert_eq!(oom_score_adj(helper), 0); }
            session.kill(1);
            assert_eq!(session.expect_ended(1), End::Signalled(9));
        }
        "helper-exec" => {
            session.wait_ready();
            session.command("X");
            run_to_exit_zero(&mut session);
        }
        "helper-scratch" => {
            session.wait_ready();
            session.command("S");
            run_to_exit_zero(&mut session);
        }
        "helper-descendants" => {
            session.wait_ready();
            session.command("D");
            let (runtime, _) = session.runtime_and_launcher();
            let daemons: Vec<_> = session.cgroups.procs("rt-runtime").into_iter().filter(|pid| *pid != runtime).collect();
            assert!(!daemons.is_empty(), "daemonized runtime descendant in rt-runtime");
            let daemons: Vec<_> = daemons.into_iter().map(pidfd).collect();
            let init = pidfd(session.init());
            let (mut stdout, _stderr) = session.start(2, 1, HelperSlot::Recording);
            session.expect_started(2);
            assert_eq!(read_line(&mut stdout), "RECORDING");
            session.command("G");
            assert_eq!(session.event(), SpawndEvent::Draining);
            // rt-runtime is killed at runtime exit, while init still waits for
            // the recording to flush (3 s after `stop\n`).
            for daemon in &daemons { assert!(readable(daemon.as_raw_fd(), 2_000), "runtime descendant survived"); }
            assert!(!readable(init.as_raw_fd(), 0), "init must still be draining the launcher");
            assert_eq!(session.expect_ended(2), End::Exited(0), "recording flushed within F");
            assert_eq!(session.finish(), (4, 23), "runtime status preserved after the drain");
            return;
        }
        "helper-sigterm" => {
            session.wait_ready();
            let (_, launcher) = session.runtime_and_launcher();
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
            assert_eq!(session.event(), SpawndEvent::Draining);
            // spawnd's own table refuses too; send the frame directly to prove the launcher does.
            let frame = ToLauncher::Start(Start { call: call(3), helper: 0, slot: HelperSlot::Foreground, timeout_ms: 30_000 })
                .encode().unwrap();
            let input = memfd(HELPER_INPUT);
            let (_stdout, stdout_write) = pipe();
            let (_stderr, stderr_write) = pipe();
            helper_wire::linux::send(session.control.as_fd(), &frame, &[input.as_fd(), stdout_write.as_fd(), stderr_write.as_fd()]).unwrap();
            let (bytes, receipt) = session.raw_frame().expect("refusal frame");
            assert_eq!(helper_wire::decode_from_launcher(&bytes, receipt).unwrap(),
                FromLauncher::Ended { call: call(3), end: End::StartFailed(LauncherError::Draining) });
            // Idle and draining: the launcher exits; the run continues.
            assert!(session.raw_frame().is_none(), "launcher closes the control channel");
        }
        "helper-stopped" => {
            session.wait_ready();
            let (_, launcher) = session.runtime_and_launcher();
            let (mut stdout, _stderr) = session.start(2, 1, HelperSlot::Recording);
            session.expect_started(2);
            assert_eq!(read_line(&mut stdout), "SLEEPING");
            let helper = pidfd(children(launcher)[0]);
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGSTOP) }, 0);
            let stopped_at = Instant::now();
            session.command("G");
            let terminal = session.finish();
            let waited = stopped_at.elapsed();
            assert_eq!(terminal, (4, 23), "runtime status preserved");
            // F = 1 s, bound F + 2 s: never RuntimeMaxUSec, never unbounded.
            assert!(waited >= Duration::from_millis(2_500) && waited < Duration::from_secs(10), "{waited:?}");
            assert!(readable(helper.as_raw_fd(), 1_000), "helper died with the run");
            return;
        }
        _ => panic!("unknown helper scenario"),
    }
    session.command("G");
    assert_eq!(session.finish(), (4, 23));
}

// ---- helper process side (argv `--fixture-helper=<mode>`) ----

pub(super) fn helper_mode() {
    let Some(mode) = std::env::args().nth(1).and_then(|arg| arg.strip_prefix("--fixture-helper=").map(str::to_owned)) else { return; };
    let result = std::panic::catch_unwind(|| helper_checks(&mode));
    unsafe { libc::_exit(if result.is_ok() { 0 } else { 101 }); }
}
fn say(text: &[u8]) { assert_eq!(unsafe { libc::write(1, text.as_ptr().cast(), text.len()) }, text.len() as isize); }
fn helper_checks(mode: &str) {
    // Identity and process state common to every helper.
    assert_eq!(unsafe { libc::getuid() }, EMPLOYEE);
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0, "signed self-only PR_SET_DUMPABLE(0)");
    assert_eq!(unsafe { libc::getsid(0) }, unsafe { libc::getpid() });
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    assert!(status.lines().any(|line| line.split_whitespace().collect::<Vec<_>>() == ["NoNewPrivs:", "1"]));
    // §2.4 cgroup [F6]: moved into its slot leaf before Landlock, F_H and exec.
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let expected = if mode == "slow-stop" { "/hx/rec" } else { "/hx/" };
    assert!((cgroup.trim_end().ends_with("/hx/fg") || cgroup.trim_end().ends_with("/hx/rec")) && cgroup.contains(expected),
        "slot cgroup: {cgroup}");
    // [F9] HOME and TMPDIR are the private call directory, which is the cwd;
    // an egress (recording-slot) helper also gets exactly the parent's
    // forwarder (HX6 security fix round, finding 5).
    let recording = cgroup.trim_end().ends_with("/hx/rec");
    let vars: Vec<_> = std::env::vars().collect();
    assert_eq!(vars.len(), if recording { 3 } else { 2 }, "{vars:?}");
    if recording { assert_eq!(std::env::var("HTTPS_PROXY").unwrap(), FIXTURE_PROXY); }
    let home = std::env::var("HOME").unwrap();
    assert_eq!(std::env::var("TMPDIR").unwrap(), home);
    let name = home.strip_prefix("/helper-tmp/").unwrap();
    assert!(name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(std::env::current_dir().unwrap(), Path::new(&home));
    match mode {
        "fds" => {
            // [F1] 0 /dev/null, 1/2 the call's pipes, 3 /dev/null, 4 sealed input; nothing else.
            for descriptor in [0, 3] {
                let o = fd::observe(descriptor).unwrap();
                assert_eq!((o.rdev_major, o.rdev_minor), (1, 3), "fd {descriptor}");
            }
            for descriptor in [1, 2] {
                let o = fd::observe(descriptor).unwrap();
                assert_eq!(o.kind, duduclaw_run::validation::FileKind::Fifo);
                assert_eq!(o.access, duduclaw_run::validation::Access::Write);
            }
            assert_eq!(unsafe { libc::fcntl(4, libc::F_GET_SEALS) }, 15);
            let mut input = vec![0u8; HELPER_INPUT.len()];
            assert_eq!(unsafe { libc::read(4, input.as_mut_ptr().cast(), input.len()) }, input.len() as isize);
            assert_eq!(input, HELPER_INPUT);
            for descriptor in 0..=4 { assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, 0, "fd {descriptor} cloexec"); }
            for descriptor in 5..4096 {
                assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1, "fd {descriptor} leaked into the helper");
            }
        }
        "fs" => {
            for path in ["/home/employee", "/config", "/run", "/tmp", "/dev/shm", "/runtime", "/helper-tmp", "/helpers"] {
                assert_eq!(std::fs::read_dir(path).unwrap_err().raw_os_error(), Some(libc::EACCES), "{path}");
            }
            for path in ["/home/employee/x", "/tmp/x", "/dev/shm/x", "/run/x"] {
                assert_eq!(std::fs::write(path, b"x").unwrap_err().raw_os_error(), Some(libc::EACCES), "{path}");
            }
            // Its scratch starts empty (the runtime cannot reach /helper-tmp) and is writable.
            assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "scratch pre-populated");
            std::fs::write(Path::new(&home).join("scratch"), b"ok").unwrap();
            std::fs::write("/work/helper-positive", b"ok").unwrap();
            assert!(std::fs::read("/etc/hosts").is_ok() && std::fs::read_dir("/usr").is_ok());
            assert!(std::fs::read("/helpers/helper").is_ok(), "own signed code is readable");
        }
        "socket" => {
            for family in [libc::AF_INET, libc::AF_INET6, libc::AF_UNIX, libc::AF_NETLINK] {
                assert_eq!(unsafe { libc::socket(family, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) }, -1);
                assert_eq!(errno(), libc::EPERM, "family {family}");
            }
        }
        "rlimit" => {
            for (resource, value) in [(libc::RLIMIT_NOFILE, OPEN_FILES), (libc::RLIMIT_FSIZE, FILE_SIZE), (libc::RLIMIT_CORE, 0)] {
                let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { libc::getrlimit(resource, &mut limit) }, 0);
                assert_eq!((limit.rlim_cur, limit.rlim_max), (value, value), "hard = soft for {resource}");
                let raised = libc::rlimit { rlim_cur: value + 1, rlim_max: value + 1 };
                assert_eq!(unsafe { libc::setrlimit(resource, &raised) }, -1);
                assert_eq!(errno(), libc::EPERM);
            }
            // prlimit64 on any other pid is outside the signed self-only exception.
            let mut old: libc::rlimit64 = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::syscall(libc::SYS_prlimit64, 1, libc::RLIMIT_NOFILE, std::ptr::null::<libc::rlimit64>(), &mut old) }, -1);
            assert_eq!(errno(), libc::EPERM);
        }
        "sleep" => { say(b"SLEEPING\n"); loop { unsafe { libc::pause(); } } }
        "slow-stop" => {
            say(b"RECORDING\n");
            let mut seen = Vec::new();
            while !seen.ends_with(b"stop\n") {
                let mut byte = [0u8];
                assert_eq!(unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) }, 1, "stop on stdin");
                seen.push(byte[0]);
            }
            std::thread::sleep(Duration::from_secs(3));
            return;
        }
        _ => panic!("unknown helper mode"),
    }
    say(b"HELPER-OK\n");
}
