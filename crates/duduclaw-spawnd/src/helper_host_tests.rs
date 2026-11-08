//! HX4 root engine tests (Linux, unprivileged): a fake unit cgroup tree and a
//! real `SOCK_SEQPACKET` socketpair whose other end plays the launcher. The
//! root/VM halves (`iso_root_helper_*`) need a real run and are listed in the
//! work breakdown's "From HX4" note.
use super::*;
use std::{collections::BTreeMap, io, os::fd::BorrowedFd};
use duduclaw_isolation::ids::RunId;
use duduclaw_run::helper_wire::{FromLauncher, LauncherError, Receipt, ToLauncher};

#[derive(Default)]
struct Tree { files: BTreeMap<String, String>, log: Vec<String> }
impl Tree {
    fn unit() -> Self {
        let mut tree = Self::default();
        for slot in ["hx/fg", "hx/rec", RT_RUNTIME] {
            for (file, value) in [("cgroup.events", "populated 0\nfrozen 0\n"), ("memory.events", "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n"),
                ("cgroup.kill", ""), ("memory.max", "0"), ("memory.swap.max", "0"), ("pids.max", "0"), ("cpu.max", "1000 100000")] {
                tree.files.insert(format!("{slot}/{file}"), value.into());
            }
        }
        tree
    }
}
impl CgroupTree for Tree {
    type Fd = ();
    fn mkdir(&mut self, _: &str) -> io::Result<()> { Ok(()) }
    fn exists(&mut self, path: &str) -> io::Result<bool> { Ok(self.files.contains_key(path)) }
    fn read(&mut self, path: &str) -> io::Result<String> { self.files.get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into()) }
    fn write(&mut self, path: &str, value: &str) -> io::Result<()> {
        self.log.push(format!("write {path} {value}"));
        let entry = self.files.get_mut(path).ok_or(io::ErrorKind::NotFound)?;
        *entry = value.into();
        if let Some(slot) = path.strip_suffix("/cgroup.kill") {
            self.files.insert(format!("{slot}/cgroup.events"), "populated 0\nfrozen 0\n".into());
        }
        Ok(())
    }
    fn open_write(&mut self, _: &str) -> io::Result<()> { Ok(()) }
}

const FG_LIMITS: SlotLimits = SlotLimits { memory_bytes: 1 << 26, tasks: 16, cpu_quota_percent: 50 };
const REC_LIMITS: SlotLimits = SlotLimits { memory_bytes: 1 << 28, tasks: 64, cpu_quota_percent: 100 };
fn fixture() -> (HelperEngine<Tree>, OwnedFd) {
    use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
    let (spawnd, launcher) = socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
    let helpers = vec![LinkedHelper { index: 3, slot: HelperSlot::Foreground, limits: FG_LIMITS, timeout_ms: 60_000 },
        LinkedHelper { index: 5, slot: HelperSlot::Recording, limits: REC_LIMITS, timeout_ms: 120_000 }];
    let engine = HelperEngine::new(RunHandle(RunId([1; 16])), HelperLink { control: spawnd, tree: Tree::unit(), helpers })
        .unwrap().with_settle(|| {});
    (engine, launcher)
}
fn id(n: u8) -> HelperCallId { HelperCallId::new([n; 16]).unwrap() }
fn plan(n: u8, index: u8, slot: HelperSlot) -> CallPlan {
    CallPlan { call: id(n), index, slot, requested_ms: 30_000, input_bytes: 5, run_remaining_ms: 600_000, termination_grace_ms: 15_000 }
}
fn memfd() -> OwnedFd { crate::fds::sealed_plan(b"input").unwrap() }
/// The launcher end receives exactly one `Start` with its 3 fds.
fn launcher_start(launcher: &OwnedFd) -> (Start, Vec<OwnedFd>) {
    let (bytes, fds, receipt) = helper_wire::linux::recv(launcher.as_fd(), helper_wire::MAX_FRAME_FDS, true).unwrap().unwrap();
    match helper_wire::decode_to_launcher(&bytes, receipt).unwrap() {
        ToLauncher::Start(start) => (start, fds),
        other => panic!("{other:?}"),
    }
}
fn say(launcher: &OwnedFd, frame: FromLauncher, fds: &[BorrowedFd<'_>]) {
    helper_wire::linux::send(launcher.as_fd(), &frame.encode().unwrap(), fds).unwrap();
}
fn nothing_sent(launcher: &OwnedFd) {
    let error = helper_wire::linux::recv(launcher.as_fd(), helper_wire::MAX_FRAME_FDS, true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}
fn ended(engine: &mut HelperEngine<Tree>) -> Vec<(HelperCallId, HelperOutcome)> {
    engine.take_events().into_iter().filter_map(|event| match event {
        HelperEvent::Ended { call, outcome, .. } => Some((call, outcome)), _ => None }).collect()
}

#[test]
fn helper_start_writes_signed_limits_and_sends_exact_fds() {
    let (mut engine, launcher) = fixture();
    let started = engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    assert_eq!(started.effective_timeout_ms, 30_000, "min(tool 30 s, binding 60 s, run budget)");
    let (start, fds) = launcher_start(&launcher);
    assert_eq!(start, Start { call: id(1), helper: 0, slot: HelperSlot::Foreground, timeout_ms: 30_000 });
    assert_eq!(fds.len(), helper_wire::START_FDS);
    crate::fds::validate_sealed(fds[0].as_fd(), 5).unwrap();
    // Limits came from the signed binding, written before Start.
    let tree = &engine.link.tree;
    assert_eq!(tree.files["hx/fg/memory.max"], (1u64 << 26).to_string());
    assert_eq!(tree.files["hx/fg/pids.max"], "16");
    assert_eq!(tree.files["hx/fg/cpu.max"], "50000 100000");
    assert_eq!(tree.files["hx/rec/pids.max"], "0", "the other slot stays closed");
    // The gateway's read ends see the helper's writes.
    nix::unistd::write(&fds[1], b"out").unwrap();
    drop(fds);
    let mut buffer = [0u8; 8];
    assert_eq!(nix::unistd::read(started.stdout.as_raw_fd(), &mut buffer).unwrap(), 3);
    // Wrong slot for the index, and an index outside this run's table.
    assert_eq!(engine.start(0, plan(2, 3, HelperSlot::Recording), memfd()).err(), Some(ErrorCode::InvalidRequest));
    assert_eq!(engine.start(0, plan(2, 9, HelperSlot::Foreground), memfd()).err(), Some(ErrorCode::HelperUnavailable));
    // The slot is busy until the root-side end.
    assert_eq!(engine.start(0, plan(2, 3, HelperSlot::Foreground), memfd()).err(), Some(ErrorCode::HelperBusy));
    say(&launcher, FromLauncher::Started { call: id(1) }, &[]);
    say(&launcher, FromLauncher::Ended { call: id(1), end: End::Exited(0) }, &[]);
    engine.service(10);
    assert_eq!(ended(&mut engine), vec![(id(1), HelperOutcome::Exited(0))]);
    // Killed and drained after every call, then closed again.
    assert!(engine.link.tree.log.iter().any(|line| line == "write hx/fg/cgroup.kill 1"));
    assert_eq!(engine.link.tree.files["hx/fg/pids.max"], "0");
    // A populated slot refuses Start.
    engine.link.tree.files.insert("hx/fg/cgroup.events".into(), "populated 1\nfrozen 0\n".into());
    assert_eq!(engine.start(20, plan(3, 3, HelperSlot::Foreground), memfd()).err(), Some(ErrorCode::HelperBusy));
}

#[test]
fn memfd_seals_revalidated() {
    use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
    let (mut engine, launcher) = fixture();
    let unsealed = memfd_create(c"unsealed", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING).unwrap();
    nix::unistd::write(&unsealed, b"input").unwrap();
    let (pipe, _writer) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).unwrap();
    for bad in [unsealed, pipe, crate::fds::sealed_plan(b"inputs").unwrap()] {
        assert_eq!(engine.start(0, plan(1, 3, HelperSlot::Foreground), bad).err(), Some(ErrorCode::InvalidFds));
    }
    // Nothing reached the launcher and no slot was touched.
    nothing_sent(&launcher);
    assert!(engine.link.tree.log.is_empty());
    assert!(engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).is_ok());
}

#[test]
fn control_channel_closed_on_cmsg_or_truncation() {
    // (a) any fd on a launcher frame, (b) MSG_TRUNC, (c) EOF (launcher died),
    // (d) a launcher `Error` frame. Rate and state-table cases are covered by
    // `helper_wire::tests`; every receive error takes this same close path.
    for case in 0..4 {
        let (mut engine, launcher) = fixture();
        let _started = engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
        let _ = launcher_start(&launcher);
        match case {
            0 => {
                let stray = std::fs::File::open("/dev/null").unwrap();
                say(&launcher, FromLauncher::Started { call: id(1) }, &[stray.as_fd()]);
            }
            1 => {
                let oversized = vec![0u8; helper_wire::MAX_FRAME_BYTES + 1];
                let sent = unsafe { libc::send(launcher.as_raw_fd(), oversized.as_ptr().cast(), oversized.len(), 0) };
                assert_eq!(sent as usize, oversized.len());
            }
            2 => {}
            _ => say(&launcher, FromLauncher::Error { code: LauncherError::Internal }, &[]),
        }
        let launcher = if case == 2 { drop(launcher); None } else { Some(launcher) };
        engine.service(5);
        assert!(engine.is_closed(), "case {case}");
        let events = engine.take_events();
        // The live call ends root-side (never started ⇒ StartFailed), both slots killed.
        assert!(events.contains(&HelperEvent::Ended { run: RunHandle(RunId([1; 16])), call: id(1), outcome: HelperOutcome::StartFailed }), "case {case}");
        assert!(events.contains(&HelperEvent::Closed { run: RunHandle(RunId([1; 16])), code: ErrorCode::HelperUnavailable }));
        for slot in ["hx/fg", "hx/rec"] {
            assert!(engine.link.tree.log.iter().any(|line| *line == format!("write {slot}/cgroup.kill 1")), "case {case} {slot}");
        }
        // Later calls: helper_unavailable; nothing more is sent.
        assert_eq!(engine.start(10, plan(2, 5, HelperSlot::Recording), memfd()).err(), Some(ErrorCode::HelperUnavailable));
        if let Some(launcher) = launcher {
            // The launcher sees EOF (spawnd shut its end down).
            assert!(helper_wire::linux::recv(launcher.as_fd(), 4, true).unwrap().is_none(), "case {case}");
        }
    }
    // A plain Receipt with no cmsg and a valid frame keeps the channel open.
    let (mut engine, launcher) = fixture();
    engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    say(&launcher, FromLauncher::Started { call: id(1) }, &[]);
    engine.service(5);
    assert!(!engine.is_closed());
    assert_eq!(Receipt::plain().fds, 0);
}

#[test]
fn watchdog_kills_slot_with_stopped_launcher() {
    // Unprivileged half of the root test: the launcher never answers (stopped).
    let (mut engine, launcher) = fixture();
    let started = engine.start(1_000, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    say(&launcher, FromLauncher::Started { call: id(1) }, &[]);
    let fires = 1_000 + started.effective_timeout_ms + WATCHDOG_GRACE_MS;
    engine.service(fires - 1);
    assert!(ended(&mut engine).is_empty(), "not before effective + grace");
    engine.service(fires);
    assert_eq!(ended(&mut engine), vec![(id(1), HelperOutcome::TimedOut)]);
    assert!(engine.link.tree.log.iter().any(|line| line == "write hx/fg/cgroup.kill 1"));
    // The kill hint went out; the launcher's late frames are accepted, not fatal.
    match helper_wire::decode_to_launcher(&helper_wire::linux::recv(launcher.as_fd(), 4, true).unwrap().unwrap().0, Receipt::plain()) {
        Ok(ToLauncher::Kill { call }) => assert_eq!(call, id(1)),
        other => panic!("{other:?}"),
    }
    say(&launcher, FromLauncher::Ended { call: id(1), end: End::Exited(0) }, &[]);
    engine.service(fires + 1);
    assert!(!engine.is_closed());
    assert!(ended(&mut engine).is_empty(), "a late Ended changes nothing");
    // The slot is free again.
    assert!(engine.start(fires + 2, plan(2, 3, HelperSlot::Foreground), memfd()).is_ok());
}

#[test]
fn oom_reported_from_memory_events_not_frame() {
    let (mut engine, launcher) = fixture();
    engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    say(&launcher, FromLauncher::Started { call: id(1) }, &[]);
    // The kernel OOM-killed the group; the launcher (a hint) claims a clean exit.
    engine.link.tree.files.insert("hx/fg/memory.events".into(), "low 0\nhigh 0\nmax 3\noom 1\noom_kill 1\n".into());
    say(&launcher, FromLauncher::Ended { call: id(1), end: End::Exited(0) }, &[]);
    engine.service(5);
    assert_eq!(ended(&mut engine), vec![(id(1), HelperOutcome::OomKilled)]);
    // A later call measures its own delta from the new counter.
    engine.start(6, plan(2, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    say(&launcher, FromLauncher::Started { call: id(2) }, &[]);
    say(&launcher, FromLauncher::Ended { call: id(2), end: End::Exited(3) }, &[]);
    engine.service(7);
    assert_eq!(ended(&mut engine), vec![(id(2), HelperOutcome::Exited(3))]);
}

#[test]
fn fg_kill_leaves_rec_running() {
    let (mut engine, launcher) = fixture();
    engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    let rec = engine.start(0, plan(2, 5, HelperSlot::Recording), memfd()).unwrap();
    assert_eq!(rec.effective_timeout_ms, 30_000);
    let _ = launcher_start(&launcher);
    say(&launcher, FromLauncher::Started { call: id(1) }, &[]);
    say(&launcher, FromLauncher::Started { call: id(2) }, &[]);
    engine.service(1);
    engine.signal(id(1), HelperSignal::Cancel).unwrap();
    assert_eq!(ended(&mut engine), vec![(id(1), HelperOutcome::Signalled(9))]);
    assert!(engine.link.tree.log.iter().any(|line| line == "write hx/fg/cgroup.kill 1"));
    assert!(!engine.link.tree.log.iter().any(|line| line == "write hx/rec/cgroup.kill 1"), "rec untouched");
    // Recording keeps running; Stop asks for a flush, Stop on fg is refused by the table.
    engine.signal(id(2), HelperSignal::Stop).unwrap();
    let _kill = helper_wire::linux::recv(launcher.as_fd(), 4, true).unwrap().unwrap();
    match helper_wire::decode_to_launcher(&helper_wire::linux::recv(launcher.as_fd(), 4, true).unwrap().unwrap().0, Receipt::plain()) {
        Ok(ToLauncher::Stop { call }) => assert_eq!(call, id(2)),
        other => panic!("{other:?}"),
    }
    say(&launcher, FromLauncher::Ended { call: id(2), end: End::Exited(0) }, &[]);
    engine.service(2);
    assert_eq!(ended(&mut engine), vec![(id(2), HelperOutcome::Exited(0))]);
    assert!(!engine.is_closed());
}

#[test]
fn runtime_exit_reports_draining_and_refuses_start() {
    let (mut engine, launcher) = fixture();
    engine.service(0);
    assert!(!engine.is_draining(), "rt-runtime never populated yet");
    engine.link.tree.files.insert(format!("{RT_RUNTIME}/cgroup.events"), "populated 1\nfrozen 0\n".into());
    engine.service(1);
    engine.link.tree.files.insert(format!("{RT_RUNTIME}/cgroup.events"), "populated 0\nfrozen 0\n".into());
    engine.service(2);
    assert!(engine.is_draining());
    assert_eq!(engine.take_events(), vec![HelperEvent::Draining { run: RunHandle(RunId([1; 16])) }]);
    assert_eq!(engine.start(3, plan(1, 3, HelperSlot::Foreground), memfd()).err(), Some(ErrorCode::NotReady));
    nothing_sent(&launcher);
    // The launcher's own Draining is accepted once (a second one would close the channel).
    let (mut engine, launcher) = fixture();
    say(&launcher, FromLauncher::Draining, &[]);
    engine.service(1);
    assert!(engine.is_draining() && !engine.is_closed());
    // Shutdown ends live calls RunEnded and emits no Closed.
    let (mut engine, launcher) = fixture();
    engine.start(0, plan(1, 3, HelperSlot::Foreground), memfd()).unwrap();
    let _ = launcher_start(&launcher);
    engine.shutdown();
    assert_eq!(engine.take_events(), vec![HelperEvent::Ended { run: RunHandle(RunId([1; 16])), call: id(1), outcome: HelperOutcome::RunEnded }]);
}
