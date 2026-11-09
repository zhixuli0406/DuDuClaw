//! HX6 root tests (D8 revision §4 [F10], H15 hosts: Ubuntu 22.04/24.04,
//! Debian 12), against a host Chromium:
//!
//!   ISO_ROOT_CHROMIUM=/path/to/chromium cargo test -p duduclaw-execution \
//!     --test recording_root -- --ignored --test-threads=1
//!
//! Security fix round (finding 5): every contained process gets the whole
//! stack the in-run path applies, in the same order and with the same code
//! (`duduclaw_run`), not only the browser layer:
//!
//! 1. NNP, the **launcher** Landlock (`helper_policy::launcher_landlock` of a
//!    recording binding: `/helpers` RX, `/helper-tmp` RW, `/dev/null`,
//!    SystemRo, `/work`), the common filter and **F_L** (`seccomp::launcher`);
//! 2. `setsid` and the **helper** Landlock (entry + browser files RX, its
//!    `/helper-tmp/<call>`, SystemRo, `/work/recordings`), then **F_H**
//!    (`seccomp::helper`, egress, with `SelfOnlyCall::NestedSandbox` and the
//!    self-only `prctl` options);
//! 3. `setpgid`, the **browser** Landlock (`recording_sandbox::BrowserLayer`)
//!    and the **browser** filter (`recording_seccomp::BrowserFilter`).
//!
//! In-run paths are mapped to a private temporary tree (`/helpers/<file>` →
//! the host Chromium file, `/helper-tmp` → `<base>/helper-tmp`, `/work` →
//! `<base>/work`). The parent profile is the root fixture's test envelope
//! (every call but the common refusals); a production profile is accepted on
//! each host by spawnd's own probe (`host_probe.rs`), which runs the signed
//! helper through a real run's launcher. A missing `ISO_ROOT_CHROMIUM` fails
//! the test (never a silent pass).
#![cfg(target_os = "linux")]
use std::{os::{fd::{AsRawFd, OwnedFd, RawFd}, unix::process::CommandExt}, path::{Path, PathBuf}, process::Command};
use duduclaw_execution::{recording_sandbox::{probe_abi, enforce_raw, BrowserLayer}, recording_seccomp::{install_raw, BrowserFilter}};
use duduclaw_run::{helper_policy::{self, HelperFs, SelfOnlyCall, LANDLOCK_CALLS, PRCTL_SELF_ONLY},
    landlock::{PathClass, Policy}, seccomp::{self, Architecture, Filter, RuntimeProfile}};

fn chromium() -> String {
    std::env::var("ISO_ROOT_CHROMIUM").expect("ISO_ROOT_CHROMIUM must name the host's Chromium binary")
}

/// `<base>/helper-tmp/<call>` standing in for the call scratch, `<base>/work`
/// with a secret the browser must never read and `recordings/` (the only
/// workspace grant of the recording helper).
fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let scratch = base.path().join("helper-tmp").join("0123456789abcdef0123456789abcdef");
    let work = base.path().join("work");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(work.join("recordings")).unwrap();
    std::fs::write(work.join("secret.html"), "<p>HX6-SECRET-WORK</p>").unwrap();
    (base, scratch, work)
}

/// Map an in-run path of the signed policy to the test tree.
fn mapped(path: &str, base: &Path, files: &[&str]) -> Vec<PathBuf> {
    if path == helper_policy::HELPERS_ROOT { return files.iter().map(PathBuf::from).collect(); }
    if path == helper_policy::HELPER_TMP_ROOT { return vec![base.join("helper-tmp")]; }
    if let Some(rest) = path.strip_prefix("/work") { return vec![base.join(format!("work{rest}"))]; }
    vec![PathBuf::from(path)]
}
/// O_PATH rule descriptors (opened before fork) and their rights, like the launcher's `landlock`.
fn rules(policy: &Policy, wanted: &[(PathBuf, PathClass)]) -> (Vec<OwnedFd>, Vec<(RawFd, u64)>) {
    use std::os::unix::fs::OpenOptionsExt;
    let (mut owned, mut raw) = (Vec::new(), Vec::new());
    for (path, class) in wanted {
        let opened = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_CLOEXEC).open(path);
        let Ok(file) = opened else {
            assert!(path == Path::new("/lib64"), "rule path {path:?} missing");
            continue;
        };
        let directory = file.metadata().unwrap().is_dir();
        let fd = OwnedFd::from(file);
        raw.push((fd.as_raw_fd(), policy.rights_on(*class, directory)));
        owned.push(fd);
    }
    (owned, raw)
}

/// Everything the in-run path installs, prepared before any fork.
struct Stack {
    policy: Policy,
    _fds: Vec<OwnedFd>,
    launcher: Vec<(RawFd, u64)>,
    helper: Vec<(RawFd, u64)>,
    common: Filter,
    f_l: Filter,
    f_h: Filter,
    browser: BrowserLayer,
    filter: BrowserFilter,
}
impl Stack {
    fn new(base: &Path, scratch: &Path, files: &[&str], forwarder_port: u16) -> Self {
        let arch = Architecture::native().unwrap();
        let policy = Policy::for_abi(duduclaw_run::landlock::probe_abi().expect("Landlock")).unwrap();
        // 1. The launcher layer of a table holding the recording binding.
        let grants = [HelperFs::SystemRo, HelperFs::WorkRecordings];
        let launcher: Vec<_> = helper_policy::launcher_landlock(&[grants.as_slice()]).into_iter()
            .flat_map(|(path, class)| mapped(path, base, files).into_iter().map(move |path| (path, class))).collect();
        // 2. The recording helper's own layer (entry and browser files RX).
        let mut helper: Vec<(PathBuf, PathClass)> = files.iter().map(|file| (PathBuf::from(file), PathClass::ReadExecute)).collect();
        helper.push((scratch.to_owned(), PathClass::Ephemeral));
        for (path, class) in helper_policy::helper_landlock(&grants) {
            helper.extend(mapped(path, base, files).into_iter().map(|path| (path, class)));
        }
        let (mut fds, launcher) = rules(&policy, &launcher);
        let (helper_fds, helper) = rules(&policy, &helper);
        fds.extend(helper_fds);
        // F_L over the test envelope parent; F_H = the parent minus every refused
        // helper call, with prctl/prlimit64/seccomp/Landlock only as self-only exceptions.
        let parent = RuntimeProfile { allowed_syscalls: (0..512).collect() };
        let parent_digest = parent.digest(arch).unwrap();
        let mut special = helper_policy::refused_helper_calls(arch, true);
        special.extend(LANDLOCK_CALLS);
        special.extend([helper_policy::prctl(arch), helper_policy::prlimit64(arch), arch.seccomp()]);
        let profile = RuntimeProfile { allowed_syscalls: (0..512).filter(|nr| !special.contains(nr)).collect() };
        let self_only = [SelfOnlyCall::Prlimit64SelfOnly, SelfOnlyCall::Prctl { options: PRCTL_SELF_ONLY.to_vec() },
            SelfOnlyCall::NestedSandbox];
        let f_h = seccomp::helper(arch, &parent, parent_digest, &profile, profile.digest(arch).unwrap(), &self_only, true)
            .expect("F_H");
        Self { policy, _fds: fds, launcher, helper, common: seccomp::common(arch),
            f_l: seccomp::launcher(arch, &parent, parent_digest).expect("F_L"), f_h,
            browser: BrowserLayer::prepare(scratch, files, forwarder_port).expect("browser layer"),
            filter: BrowserFilter::prepare().expect("browser filter") }
    }
}

/// `program args…` under the whole stack (see the module docs). The closure
/// owns copies of the prepared rules and filters; the rule descriptors, the
/// browser ruleset and the browser program stay alive in `stack` until
/// `output` returned.
#[allow(unsafe_code)]
fn contained(stack: &Stack, scratch: &Path, program: &str, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(program);
    command.args(args).env_clear().env("HOME", scratch).env("TMPDIR", scratch).env("PATH", "/usr/bin:/bin")
        .current_dir(scratch);
    let (policy, launcher, helper) = (stack.policy.clone(), stack.launcher.clone(), stack.helper.clone());
    let (common, f_l, f_h) = (stack.common.clone(), stack.f_l.clone(), stack.f_h.clone());
    let (browser, filter) = (stack.browser.raw(), stack.filter.raw());
    let in_child = move || -> std::io::Result<()> {
        let fail = || std::io::Error::from_raw_os_error(libc::EPERM);
        // SAFETY: every call below changes only this forked child; nothing allocates.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 { return Err(fail()); }
        // 1. launcher layer: Landlock, common, F_L.
        duduclaw_run::landlock::restrict_raw(&policy, &launcher).map_err(|_| fail())?;
        common.install().map_err(|_| fail())?;
        f_l.install().map_err(|_| fail())?;
        // 2. helper child: own session, helper Landlock, F_H.
        if unsafe { libc::setsid() } < 0 { return Err(fail()); }
        duduclaw_run::landlock::restrict_raw(&policy, &helper).map_err(|_| fail())?;
        f_h.install().map_err(|_| fail())?;
        // 3. browser child: own group, browser Landlock, browser filter.
        if unsafe { libc::setpgid(0, 0) } != 0 { return Err(fail()); }
        enforce_raw(browser).map_err(|_| fail())?;
        install_raw(filter)
    };
    // SAFETY: see above.
    unsafe { command.pre_exec(in_child); }
    command.output().unwrap()
}
fn stack(base: &Path, scratch: &Path, files: &[&str], port: u16) -> Stack { Stack::new(base, scratch, files, port) }

fn chromium_args(url: &str) -> Vec<&str> {
    // The driver's contained flags (pipe transport is Playwright's; here the
    // browser itself must start and stay inside the stack).
    vec!["--headless=new", "--no-sandbox", "--disable-dev-shm-usage", "--js-flags=--jitless", "--disable-gpu",
        "--no-first-run", "--disable-background-networking", "--disable-quic", "--disable-breakpad",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp", "--dump-dom", url]
}

#[test]
#[ignore = "requires root Linux with Landlock and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_launches_under_helper_sandbox() {
    let chromium = chromium();
    let (base, scratch, _work) = dirs();
    let stack = stack(base.path(), &scratch, &[chromium.as_str()], 3128);
    let output = contained(&stack,&scratch, &chromium, &chromium_args("data:text/html,<p>HX6-LAUNCHED</p>"));
    assert!(output.status.success(), "Chromium did not start under launcher ∩ helper ∩ browser layers: {:?}\n{}",
        output.status, String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("HX6-LAUNCHED"));
}

#[test]
#[ignore = "requires root Linux with Landlock and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_cannot_read_work() {
    let chromium = chromium();
    let (base, scratch, work) = dirs();
    let stack = stack(base.path(), &scratch, &[chromium.as_str()], 3128);
    let url = format!("file://{}", work.join("secret.html").display());
    let output = contained(&stack,&scratch, &chromium, &chromium_args(&url));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("HX6-SECRET-WORK"), "the browser read /work");
    // The same stack still lets it write its own scratch (trace staging).
    let shell = contained(&stack,&scratch, "/bin/sh", &["-c", &format!("echo ok > {0}/probe && cat {1}/secret.html",
        scratch.display(), work.display())]);
    assert!(scratch.join("probe").exists());
    assert!(!shell.status.success() && !String::from_utf8_lossy(&shell.stdout).contains("HX6-SECRET-WORK"));
}

/// Finding 1: Landlock mediates neither pathname UNIX sockets nor UDP on any
/// ABI; the nested browser filter refuses them (and `setsid`/`setpgid`).
#[test]
#[ignore = "requires root Linux with Landlock, python3 and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_cannot_connect_unix_or_udp() {
    let chromium = chromium();
    let (base, scratch, _work) = dirs();
    // Runtime-reachable listeners outside every layer.
    let stream_path = base.path().join("runtime.sock");
    let dgram_path = base.path().join("runtime.dgram");
    let stream = std::os::unix::net::UnixListener::bind(&stream_path).unwrap();
    let dgram = std::os::unix::net::UnixDatagram::bind(&dgram_path).unwrap();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = tcp.local_addr().unwrap().port();
    let stack = stack(base.path(), &scratch, &[chromium.as_str()], 3128);
    let probe = |script: String| contained(&stack,&scratch, "/usr/bin/python3", &["-c", &script]).status.success();
    let (stream_path, dgram_path) = (stream_path.display(), dgram_path.display());
    for (script, what) in [
        (format!("import socket; s = socket.socket(socket.AF_UNIX); s.connect('{stream_path}')"), "pathname UNIX stream"),
        (format!("import socket; s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM); s.sendto(b'x', '{dgram_path}')"), "pathname UNIX datagram"),
        (format!("import socket; a, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM); a.sendto(b'x', '{dgram_path}')"),
            "datagram socketpair to a pathname socket"),
        (format!("import socket; s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.sendto(b'x', ('127.0.0.1', {udp_port}))"),
            "loopback UDP"),
        ("import socket; socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)".to_owned(), "UDP over IPv6"),
        ("import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM, socket.IPPROTO_SCTP)".to_owned(), "SCTP stream"),
        (format!("import socket; s = socket.socket(); s.sendto(b'x', socket.MSG_FASTOPEN, ('127.0.0.1', {tcp_port}))"),
            "TCP Fast Open"),
        ("import os; os.setsid()".to_owned(), "setsid (leaving the browser's group)"),
        ("import os; os.setpgid(0, 0)".to_owned(), "setpgid"),
    ] {
        assert!(!probe(script), "{what} must be refused inside the browser tree");
    }
    // Chromium's IPC needs stream socketpairs; they stay.
    assert!(probe("import socket; a, b = socket.socketpair(); a.send(b'x'); assert b.recv(1) == b'x'".to_owned()));
    // Nothing reached any listener.
    let mut buffer = [0u8; 8];
    stream.set_nonblocking(true).unwrap(); assert!(stream.accept().is_err(), "UNIX stream listener reached");
    dgram.set_nonblocking(true).unwrap(); assert!(dgram.recv(&mut buffer).is_err(), "UNIX datagram reached");
    udp.set_nonblocking(true).unwrap(); assert!(udp.recv_from(&mut buffer).is_err(), "UDP reached");
    tcp.set_nonblocking(true).unwrap(); assert!(tcp.accept().is_err(), "TCP Fast Open reached a non-forwarder port");
}

#[test]
#[ignore = "requires root Linux with Landlock ABI >= 4 (Ubuntu 24.04 6.8) and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_tcp_connect_only_forwarder_port() {
    let abi = probe_abi().expect("Landlock");
    if abi < 4 {
        // Documented residual (threat row T5): nothing to pin below ABI 4.
        eprintln!("Landlock ABI {abi} < 4: TCP pinning unavailable on this kernel (T5 residual)");
        return;
    }
    let chromium = chromium();
    let (base, scratch, _work) = dirs();
    let forwarder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (port, other_port) = (forwarder.local_addr().unwrap().port(), other.local_addr().unwrap().port());
    let stack = stack(base.path(), &scratch, &[chromium.as_str()], port);
    let probe = |script: String| contained(&stack,&scratch, "/usr/bin/python3", &["-c", &script]).status.success();
    let connect = |target: u16| format!("import socket; socket.create_connection(('127.0.0.1', {target}), 2)");
    assert!(probe(connect(port)), "the forwarder port must stay reachable");
    assert!(!probe(connect(other_port)), "any other loopback TCP port must be refused");
    assert!(!probe("import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen(1)".into()),
        "the browser tree may not bind any TCP port");
    // Security fix round (finding 1): TCP Fast Open connects from `sendto`, not
    // `connect(2)`; it must not reach a non-forwarder port either.
    assert!(!probe(format!("import socket; s = socket.socket(); s.sendto(b'x', socket.MSG_FASTOPEN, ('127.0.0.1', {other_port}))")),
        "sendto(MSG_FASTOPEN) must be refused");
    // Chromium itself cannot fetch from a non-forwarder loopback listener.
    let url = format!("http://127.0.0.1:{other_port}/");
    let output = contained(&stack,&scratch, &chromium, &chromium_args(&url));
    other.set_nonblocking(true).unwrap();
    assert!(other.accept().is_err(), "Chromium (or Fast Open) reached a non-forwarder port: {}", String::from_utf8_lossy(&output.stderr));
}
