use super::*;
use std::{io::IoSlice, sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc, Mutex}};
use duduclaw_isolation::{bounded::List, execution::{RunHandle, RunOutcome, RuntimeKind, PtySize},
    ids::{EmployeeId, RequestId, RunId},
    ipc::{CleanupReadback, ControlConnectionId, ErrorCode, IoDelivery, QueueClass, SpawnRequest, MAX_FRAME_BYTES},
    update::{Architecture, OperatingSystem, ProtocolCompatibility, ReleaseChannel}};
use duduclaw_updated::activation_contract::validate_response;
use nix::sys::socket::{AddressFamily, ControlMessage, MsgFlags, SockType};
use crate::admission::{Coordinator, Host, HostProgress, LaunchPolicy, Limits};

// ---------------------------------------------------------------- fixtures

#[derive(Debug, PartialEq)]
enum Event { Persisted { already_closed: bool }, Cleared { already_open: bool } }
#[derive(Default)]
struct Disk { bytes: Option<Vec<u8>>, fail: bool, events: Vec<Event>, watch: Option<DrainBarrier>,
    /// `publish.json`, stored as the exact bytes a disk would hold.
    journal: Option<Vec<u8>>, journal_fail: bool }
/// In-memory "disk" that records, at each durable step, what the barrier
/// already said: the protocol requires the record before the closure and the
/// removal before the reopening.
#[derive(Clone, Default)]
struct MemoryStore(Arc<Mutex<Disk>>);
impl MemoryStore {
    fn watching(barrier: &DrainBarrier) -> Self {
        let store = Self::default();
        store.0.lock().unwrap().watch = Some(barrier.clone());
        store
    }
}
impl DrainStore for MemoryStore {
    fn load(&mut self) -> io::Result<Option<Sha256Digest>> {
        self.0.lock().unwrap().bytes.as_deref().map(parse_record).transpose()
    }
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.fail { return Err(io::Error::other("injected write failure")); }
        let already_closed = disk.watch.as_ref().is_some_and(|b| b.closure() == Some(DrainClosure::Generation(generation)));
        disk.events.push(Event::Persisted { already_closed });
        disk.bytes = Some(encode_record(generation)?);
        Ok(())
    }
    fn clear(&mut self) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.fail { return Err(io::Error::other("injected unlink failure")); }
        let already_open = disk.watch.as_ref().is_some_and(|b| !b.is_closed());
        disk.events.push(Event::Cleared { already_open });
        disk.bytes = None;
        Ok(())
    }
}
impl JournalStore for MemoryStore {
    fn load_journal(&mut self) -> io::Result<Option<PublishJournal>> {
        self.0.lock().unwrap().journal.as_deref().map(|bytes| PublishJournal::parse(bytes).map_err(|_| invalid())).transpose()
    }
    fn persist_journal(&mut self, journal: &PublishJournal) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.journal_fail { return Err(io::Error::other("injected journal write failure")); }
        disk.journal = Some(journal.encode().map_err(|_| invalid())?);
        Ok(())
    }
    fn clear_journal(&mut self) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.journal_fail { return Err(io::Error::other("injected journal unlink failure")); }
        disk.journal = None;
        Ok(())
    }
}
impl MemoryStore {
    fn journal(&self) -> Option<PublishJournal> { self.clone().load_journal().unwrap() }
}

/// The eight fixed destinations and the staged generation directories, shared
/// across simulated restarts (each restart builds new effects over the same world).
#[derive(Default)]
struct World {
    /// The generation whose bytes each fixed destination holds, in `publication()` order.
    slots: [Option<Sha256Digest>; 8],
    /// The `<name>.next` temporary next to each fixed destination: the
    /// generation whose (possibly torn) copy it holds.
    temporaries: [Option<Sha256Digest>; 8],
    /// Unlinking a temporary fails (an I/O error, not "absent").
    temporaries_stuck: bool,
    /// Generation directories that pass full verification.
    staged: Vec<Sha256Digest>,
    /// Generations whose signing key the trust ledger has since revoked.
    revoked: Vec<Sha256Digest>,
    /// The install selector (`current`).
    selector: Option<Sha256Digest>,
    /// Simulated power loss in the next `publish`, after this many renames.
    power_loss_after: Option<usize>,
    /// The journal on "disk" when each `publish` began (before its first rename).
    journal_at_publish: Vec<Option<PublishJournal>>,
    disk: Option<MemoryStore>,
}
type SharedWorld = Arc<Mutex<World>>;
fn new_world(store: &MemoryStore, installed: Sha256Digest, staged: &[Sha256Digest]) -> SharedWorld {
    Arc::new(Mutex::new(World { slots: [Some(installed); 8], staged: staged.to_vec(), selector: Some(installed),
        disk: Some(store.clone()), ..World::default() }))
}
/// One fixed destination of the fake world, driven by the production
/// [`publish_slot`] / [`settled_slot`] ordering.
struct WorldSlot<'a> { world: &'a mut World, index: usize, generation: Sha256Digest }
impl Slot for WorldSlot<'_> {
    fn remove_temporary(&mut self) -> io::Result<()> {
        if self.world.temporaries[self.index].is_some() && self.world.temporaries_stuck {
            return Err(io::Error::other("injected temporary unlink failure"));
        }
        self.world.temporaries[self.index] = None;
        Ok(())
    }
    fn holds(&mut self) -> io::Result<bool> { Ok(self.world.slots[self.index] == Some(self.generation)) }
    fn write_temporary(&mut self) -> io::Result<()> {
        self.world.temporaries[self.index] = Some(self.generation);
        Ok(())
    }
    fn commit(&mut self) -> io::Result<()> {
        self.world.slots[self.index] = Some(self.world.temporaries[self.index].take().ok_or_else(invalid)?);
        Ok(())
    }
}

/// Without a world, the selected generation of a verified installed set.
const INSTALLED: Sha256Digest = Sha256Digest([0xee; 32]);
/// What the fake workload census reports (production: PID 1 and cgroupfs).
#[derive(Clone, Copy, Default)]
enum Census { #[default] Idle, Busy(Workloads), Fails }
const LIVE_RUN: Workloads = Workloads { live_run_units: 1, employees_cgroup_populated: false };
const POPULATED: Workloads = Workloads { live_run_units: 0, employees_cgroup_populated: true };
/// What PID 1 reports for the gate and the units bound to it (production:
/// `Systemd::units_down` over `GATED_UNITS`).
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Gate { #[default] Down, Up, Uncertain }
#[derive(Default)]
struct FakeEffects { calls: Vec<&'static str>, fail: bool, world: Option<SharedWorld>, census: Census, gate: Gate,
    /// Every gate check, with the journal on "disk" (over a world) at that moment.
    gate_checks: Vec<Option<PublishJournal>>,
    /// The gate state reported from the second check on (a gate that completed
    /// meanwhile); `None`: unchanged.
    gate_after_first: Option<Gate> }
impl FakeEffects {
    fn over(world: &SharedWorld) -> Self { Self { world: Some(world.clone()), ..Self::default() } }
}
impl Effects for FakeEffects {
    fn prepare(&mut self, _: &Preparation) -> io::Result<()> {
        self.calls.push("prepare");
        if self.fail { Err(invalid()) } else { Ok(()) }
    }
    fn verify(&mut self, generation: Sha256Digest, _: Option<&Preparation>) -> io::Result<Sha256Digest> {
        self.calls.push("verify");
        if self.fail { return Err(invalid()); }
        match &self.world {
            Some(world) => {
                let world = world.lock().unwrap();
                // Missing/corrupt directory, or a signer the ledger revoked.
                if !world.staged.contains(&generation) || world.revoked.contains(&generation) { return Err(invalid()); }
                Ok(generation)
            }
            // The manifest digest is the generation digest.
            None => Ok(generation),
        }
    }
    fn published(&mut self, generation: Sha256Digest) -> io::Result<()> {
        self.calls.push("published");
        let Some(world) = &self.world else { return Ok(()) };
        let mut world = world.lock().unwrap();
        for index in 0..8 {
            if !settled_slot(&mut WorldSlot { world: &mut *world, index, generation })? { return Err(invalid()); }
        }
        Ok(())
    }
    fn publish(&mut self, generation: Sha256Digest) -> io::Result<()> {
        self.calls.push("publish");
        if self.fail { return Err(invalid()); }
        let Some(world) = &self.world else { return Ok(()) };
        let mut world = world.lock().unwrap();
        let journal = world.disk.as_ref().and_then(MemoryStore::journal);
        world.journal_at_publish.push(journal);
        // The copy re-verifies the source: a corrupted generation stops here.
        if !world.staged.contains(&generation) { return Err(invalid()); }
        // Power loss after this many renames: the next component's temporary
        // is fully written, its rename never happens.
        let mut budget = world.power_loss_after.take();
        let mut before_rename = || -> io::Result<()> {
            match budget {
                Some(0) => { budget = None; Err(io::Error::other("simulated power loss")) }
                Some(remaining) => { budget = Some(remaining - 1); Ok(()) }
                None => Ok(()),
            }
        };
        let mut result = Ok(());
        for index in 0..8 {
            result = publish_slot(&mut WorldSlot { world: &mut *world, index, generation }, &mut before_rename);
            if result.is_err() { break; }
        }
        world.power_loss_after = budget;
        result
    }
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>> {
        Ok(match &self.world { Some(world) => world.lock().unwrap().selector, None => Some(INSTALLED) })
    }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> {
        self.calls.push("readback");
        Ok(vec![])
    }
    fn workloads(&mut self) -> io::Result<Workloads> {
        match self.census {
            Census::Idle => Ok(Workloads::default()),
            Census::Busy(workloads) => Ok(workloads),
            Census::Fails => Err(io::Error::other("census incomplete")),
        }
    }
    fn gated_units_down(&mut self) -> io::Result<bool> {
        let journal = self.world.as_ref().and_then(|world| world.lock().unwrap().disk.as_ref().and_then(MemoryStore::journal));
        self.gate_checks.push(journal);
        if self.gate_checks.len() > 1 { if let Some(gate) = self.gate_after_first { self.gate = gate; } }
        match self.gate {
            Gate::Down => Ok(true),
            Gate::Up => Ok(false),
            Gate::Uncertain => Err(io::Error::other("bus answer uncertain")),
        }
    }
}
/// A verified installed set and no publication (drain-only root tests).
struct NoEffects;
impl Effects for NoEffects {
    fn prepare(&mut self, _: &Preparation) -> io::Result<()> { Err(invalid()) }
    fn verify(&mut self, generation: Sha256Digest, _: Option<&Preparation>) -> io::Result<Sha256Digest> {
        if generation == INSTALLED { Ok(generation) } else { Err(invalid()) }
    }
    fn publish(&mut self, _: Sha256Digest) -> io::Result<()> { Err(invalid()) }
    fn published(&mut self, generation: Sha256Digest) -> io::Result<()> {
        if generation == INSTALLED { Ok(()) } else { Err(invalid()) }
    }
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>> { Ok(Some(INSTALLED)) }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> { Err(invalid()) }
    fn workloads(&mut self) -> io::Result<Workloads> { Ok(Workloads::default()) }
    fn gated_units_down(&mut self) -> io::Result<bool> { Ok(true) }
}

/// Stands in for the admission loop: one iteration every 5 ms with a fixed run
/// count and health.
struct Observer { stop: Arc<AtomicBool>, runs: Arc<AtomicUsize>, healthy: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>> }
impl Observer {
    fn new(barrier: &DrainBarrier, runs: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(runs));
        let healthy = Arc::new(AtomicBool::new(true));
        let (barrier, flag, admitted, health) = (barrier.clone(), stop.clone(), count.clone(), healthy.clone());
        let worker = std::thread::spawn(move || while !flag.load(Ordering::Acquire) {
            let epoch = barrier.epoch();
            barrier.observe(epoch, admitted.load(Ordering::Acquire), health.load(Ordering::Acquire));
            std::thread::sleep(Duration::from_millis(5));
        });
        Self { stop, runs: count, healthy, worker: Some(worker) }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

fn digest(n: u8) -> Sha256Digest { Sha256Digest([n; 32]) }
fn request(command: ControlCommand) -> ControlRequest { ControlRequest { schema_version: 1, nonce: RequestId([3; 16]), command } }
fn preparation(generation: Sha256Digest) -> Preparation { Preparation { generation, migrations: vec![], runtime_entries: vec![] } }
fn outcome<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) -> ControlOutcome {
    let request = request(command);
    let response = control.respond(&request);
    assert_eq!(response.nonce, request.nonce);
    assert_eq!(response.generation, request.command.generation());
    response.outcome
}
fn accepted<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) {
    let request = request(command);
    let response = control.respond(&request);
    validate_response(&request, &response).unwrap_or_else(|_| panic!("refused: {:?}", response.outcome));
}

// ---------------------------------------------------------------- drain record

#[test]
fn update_drain_record_parse_is_strict() {
    let good = encode_record(digest(7)).unwrap();
    assert_eq!(parse_record(&good).unwrap(), digest(7));
    let text = String::from_utf8(good.clone()).unwrap();
    let mut spaced = good.clone(); spaced.push(b'\n');
    for bad in [
        Vec::new(), spaced, vec![b'{'; MAX_RECORD_BYTES + 1],
        text.replace("\"version\":1", "\"version\":2").into_bytes(),
        text.replace('{', "{\"extra\":1,").into_bytes(),
        text.replace('{', "{\"version\":1,").into_bytes(),
        encode_record(digest(0)).unwrap(),
        text.replace(":[", ": [").into_bytes(),
        b"null".to_vec(),
    ] {
        assert!(parse_record(&bad).is_err(), "{}", String::from_utf8_lossy(&bad));
    }
}

#[test]
fn update_drain_persists_record_before_closing_and_replying() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::watching(&barrier);
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), None);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    // Exactly one durable write, and it happened while the barrier was still open.
    assert_eq!(store.0.lock().unwrap().events, vec![Event::Persisted { already_closed: false }]);
    assert_eq!(store.0.lock().unwrap().bytes.as_deref().map(parse_record).transpose().unwrap(), Some(digest(7)));
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // Resume removes the record before admission reopens.
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.0.lock().unwrap().events.last(), Some(&Event::Cleared { already_open: false }));
    assert_eq!(barrier.closure(), None);
    assert!(store.0.lock().unwrap().bytes.is_none());
}

#[test]
fn update_drain_write_failure_refuses_and_announces_nothing() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::watching(&barrier);
    store.0.lock().unwrap().fail = true;
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert!(matches!(outcome(&mut control, ControlCommand::Drain { generation: digest(7) }), ControlOutcome::Refused));
    assert_eq!(barrier.closure(), None);
    // A Resume whose unlink fails keeps the barrier closed.
    store.0.lock().unwrap().fail = false;
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    store.0.lock().unwrap().fail = true;
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
}

#[test]
fn update_drain_without_admission_loop_times_out_refused() {
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(MemoryStore::default(), barrier.clone(), FakeEffects::default()).unwrap();
    // No admission iteration can confirm the count: never report a drain.
    assert!(matches!(outcome(&mut control, ControlCommand::Drain { generation: digest(7) }), ControlOutcome::Refused));
    // The durable record still closed admission (fail closed), keyed to the request.
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
}

#[test]
fn update_barrier_survives_restart_and_only_matching_resume_lifts_it() {
    let store = MemoryStore::default();
    {
        let barrier = DrainBarrier::default();
        let _admission = Observer::new(&barrier, 0);
        let mut control = UpdateControl::new(store.clone(), barrier, FakeEffects::default()).unwrap();
        accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    }
    // "Restart": a new process reads the record before any admission.
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(8) }), ControlOutcome::Refused));
    assert!(barrier.is_closed());
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert!(!barrier.is_closed());
    // Idempotent: a retried Resume after the record is gone succeeds.
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });

    // An unreadable record closes admission with no generation; no Resume lifts
    // it, a new Drain rewrites the record and re-keys it.
    store.0.lock().unwrap().bytes = Some(b"{\"version\":1,\"generation\":".to_vec());
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), Some(DrainClosure::Unreadable));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    assert!(barrier.is_closed());
    accepted(&mut control, ControlCommand::Drain { generation: digest(9) });
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(9))));
    accepted(&mut control, ControlCommand::Resume { generation: digest(9) });
    assert!(!barrier.is_closed());
}

#[test]
fn update_drain_rekeys_to_the_latest_generation() {
    // A rolled-back activation drains the committed generation over a pending one.
    let barrier = DrainBarrier::default();
    let store = MemoryStore::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(2) }), ControlOutcome::Refused));
    accepted(&mut control, ControlCommand::Resume { generation: digest(1) });
}

#[test]
fn update_apply_requires_closed_recorded_generation_and_no_runs() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::default();
    let admission = Observer::new(&barrier, 1);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    // The start verified the selected generation (and wrote nothing).
    assert_eq!(control.effects.calls, vec!["verify", "published"]);
    control.effects.calls.clear();
    let apply = |generation| ControlCommand::Apply { preparation: preparation(generation) };
    // Not drained at all.
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    // Drained with an admitted run: the client sees a valid barrier but keeps retrying.
    match outcome(&mut control, ControlCommand::Drain { generation: digest(7) }) {
        ControlOutcome::Drained { admission_closed: true, active_runs: 1 } => {}
        other => panic!("{other:?}"),
    }
    let request_with_runs = request(ControlCommand::Drain { generation: digest(7) });
    assert!(validate_response(&request_with_runs, &control.respond(&request_with_runs)).is_err());
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    admission.runs.store(0, Ordering::Release);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    // Another generation's barrier never authorizes publication.
    assert!(matches!(outcome(&mut control, apply(digest(8))), ControlOutcome::Refused));
    // The durable record must agree with memory.
    let saved = store.0.lock().unwrap().bytes.take();
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    store.0.lock().unwrap().bytes = saved;
    assert!(control.effects.calls.is_empty());
    accepted(&mut control, apply(digest(7)));
    assert_eq!(control.effects.calls, vec!["verify", "publish"]);
    // An Apply on a set verified at start never asks for a restart.
    assert!(!control.restart_required());
    // The selected generation is the journal's previous.
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(7), digest(7), Some(INSTALLED), PublishState::Published).unwrap()));
    // This process accepted the publication: only its restart may resume.
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    assert!(barrier.is_closed());
    drop(control);
    drop(admission);
    // The restart (the updater's, after Apply): a new process reads the disk,
    // re-verifies the `published` journal, and only then accepts the Resume.
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert!(control.consistent_at_start());
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.journal(), None);
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
}

#[test]
fn update_superseded_refuses_resume_and_other_generations_and_changes_nothing() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2), digest(3)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    assert!(control.opened());
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    accepted(&mut control, apply_command(digest(2)));
    let published = PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Published).unwrap();
    assert_eq!(store.journal(), Some(published));
    assert_eq!(control.report(), "recovering");
    // Before the restart: Resume (whatever generation), Apply of another
    // generation, and a Prepare/Drain/Apply sequence for another generation
    // are refused; the barrier stays closed and neither the journal, the
    // drain record nor any destination changes.
    let events = store.0.lock().unwrap().events.len();
    control.effects.calls.clear();
    for command in [ControlCommand::Resume { generation: digest(2) }, ControlCommand::Resume { generation: digest(1) },
        apply_command(digest(3)), apply_command(digest(1)), ControlCommand::Resume { generation: digest(2) }] {
        refused_outcome(&mut control, command);
        assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
        assert_eq!(store.journal(), Some(published));
        assert_eq!(store.clone().load().unwrap(), Some(digest(2)));
        assert_eq!(store.0.lock().unwrap().events.len(), events);
        assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    }
    // Refused before anything was verified, published or checked.
    assert!(control.effects.calls.is_empty());
    assert!(!control.exit_required() && !control.restart_required());
    assert_eq!(control.report(), "recovering");
    // Read-only readback is served; an idempotent retry of the accepted
    // generation (a lost reply: Prepare, Drain, Apply) is served and keeps
    // the journal's previous; the barrier never opens.
    accepted(&mut control, ControlCommand::Readback { generation: digest(2) });
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(digest(2)) });
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(store.journal(), Some(published));
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(barrier.is_closed());
    drop(control);
    // Only the restarted, re-verified process resumes.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    accepted(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert_eq!(store.journal(), None);
    assert!(!barrier.is_closed());
}

// ---------------------------------------------------------------- publication journal

fn apply_command(generation: Sha256Digest) -> ControlCommand { ControlCommand::Apply { preparation: preparation(generation) } }
fn refused_outcome<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) {
    assert!(matches!(outcome(control, command), ControlOutcome::Refused));
}
fn slots(world: &SharedWorld) -> [Option<Sha256Digest>; 8] { world.lock().unwrap().slots }
fn temporaries(world: &SharedWorld) -> [Option<Sha256Digest>; 8] { world.lock().unwrap().temporaries }
/// A simulated restart: a new barrier and new effects over the same disk and world.
fn restart(store: &MemoryStore, world: &SharedWorld) -> (DrainBarrier, UpdateControl<MemoryStore, FakeEffects>) {
    let barrier = DrainBarrier::default();
    let control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(world)).unwrap();
    (barrier, control)
}
/// Generation 1 installed and selected; `Apply(2)` loses power after `renames` renames.
fn interrupted_apply(renames: usize) -> (MemoryStore, SharedWorld) {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier, FakeEffects::over(&shared)).unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    shared.lock().unwrap().power_loss_after = Some(renames);
    refused_outcome(&mut control, apply_command(digest(2)));
    let mixed = slots(&shared);
    assert_eq!(mixed.iter().filter(|slot| **slot == Some(digest(2))).count(), renames);
    assert_eq!(mixed.iter().filter(|slot| **slot == Some(digest(1))).count(), 8 - renames);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Publishing).unwrap()));
    (store, shared)
}

#[test]
fn update_journal_is_durable_before_the_first_rename_and_restart_rolls_forward() {
    // Fault between the journal write and the first rename: nothing replaced.
    let (store, shared) = interrupted_apply(0);
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    // The journal was on disk, read back, before publish touched anything.
    let publishing = PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Publishing).unwrap();
    assert_eq!(shared.lock().unwrap().journal_at_publish, vec![Some(publishing)]);
    // Restart: `publishing` is rolled forward before admission or any socket.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward);
    assert_eq!(control.effects.calls, vec!["verify", "publish"]);
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal { state: PublishState::Published, ..publishing }));
    // The drain record still holds admission closed for the generation.
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
    // The journal stays until the Resume of its generation (after the commit).
    accepted(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert_eq!(store.journal(), None);
    assert!(!barrier.is_closed());
}

#[test]
fn update_power_loss_after_each_rename_is_completed_on_restart() {
    for renames in 1..=7 {
        let (store, shared) = interrupted_apply(renames);
        let (barrier, control) = restart(&store, &shared);
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward, "after {renames} renames");
        assert_eq!(slots(&shared), [Some(digest(2)); 8], "after {renames} renames");
        assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Published));
        assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
        // A second restart re-verifies the `published` generation and runs the
        // idempotent publication again (nothing left to replace).
        let (_, again) = restart(&store, &shared);
        assert_eq!(again.startup().recovery, PublicationRecovery::Reverified);
        assert_eq!(again.effects.calls, vec!["verify", "publish"]);
        assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    }
}

#[test]
fn update_corrupt_generation_after_partial_publication_rolls_back() {
    let (store, shared) = interrupted_apply(3);
    // The new generation's directory no longer verifies.
    shared.lock().unwrap().staged.retain(|generation| *generation != digest(2));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledBack);
    assert!(control.startup().recovery.consistent());
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    // The roll-back is journaled like any publication; the interrupted generation is its fallback.
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), Some(digest(2)), PublishState::Published).unwrap()));
    // Admission stays closed: the record names generation 2, which is not installed.
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
    let _admission = Observer::new(&barrier, 0);
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(barrier.is_closed());
    // The updater re-drains and resumes the generation actually installed.
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    accepted(&mut control, ControlCommand::Resume { generation: digest(1) });
    assert_eq!(store.journal(), None);
    assert!(!barrier.is_closed());
}

/// The power loss of [`interrupted_apply`] leaves the next component's
/// temporary: a complete copy of 2 whose rename never happened.
fn torn_after(renames: usize) -> [Option<Sha256Digest>; 8] {
    let mut torn = [None; 8];
    torn[renames] = Some(digest(2));
    torn
}

#[test]
fn update_rollback_removes_the_interrupted_temporary() {
    for renames in 0..=7 {
        let (store, shared) = interrupted_apply(renames);
        assert_eq!(temporaries(&shared), torn_after(renames), "after {renames} renames");
        // Roll-back: the interrupted component already holds 1 (kept), the
        // torn copy of 2 next to it must not survive.
        shared.lock().unwrap().staged.retain(|generation| *generation != digest(2));
        let (_, control) = restart(&store, &shared);
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledBack, "after {renames} renames");
        assert!(control.consistent_at_start());
        assert_eq!(slots(&shared), [Some(digest(1)); 8], "after {renames} renames");
        assert_eq!(temporaries(&shared), [None; 8], "after {renames} renames");
        assert_eq!(store.journal(),
            Some(PublishJournal::new(digest(1), digest(1), Some(digest(2)), PublishState::Published).unwrap()));
    }
}

#[test]
fn update_roll_forward_removes_the_interrupted_temporary() {
    for renames in 0..=7 {
        let (store, shared) = interrupted_apply(renames);
        assert_eq!(temporaries(&shared), torn_after(renames), "after {renames} renames");
        let (_, control) = restart(&store, &shared);
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward, "after {renames} renames");
        assert!(control.consistent_at_start());
        assert_eq!(slots(&shared), [Some(digest(2)); 8], "after {renames} renames");
        assert_eq!(temporaries(&shared), [None; 8], "after {renames} renames");
        assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Published));
    }
}

#[test]
fn update_apply_removes_a_stale_temporary_next_to_a_kept_component() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    assert_eq!(control.startup().recovery, PublicationRecovery::NoJournal);
    // An unrelated stale copy next to a component whose fixed path already
    // holds the generation applied (every component is kept).
    shared.lock().unwrap().temporaries[3] = Some(digest(9));
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    accepted(&mut control, apply_command(digest(1)));
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(temporaries(&shared), [None; 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), None, PublishState::Published).unwrap()));
    // One that cannot be removed fails the (retried) Apply; never ignored.
    {
        let mut world = shared.lock().unwrap();
        world.temporaries[5] = Some(digest(9));
        world.temporaries_stuck = true;
    }
    refused_outcome(&mut control, apply_command(digest(1)));
    assert_eq!(temporaries(&shared)[5], Some(digest(9)));
    assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Publishing));
    drop(control);
    // Recovery cannot remove it either: not consistent.
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    assert!(!control.consistent_at_start());
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    drop(control);
    // Once it can be removed, the restart completes the publication without it.
    shared.lock().unwrap().temporaries_stuck = false;
    let (_, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward);
    assert!(control.consistent_at_start());
    assert_eq!(temporaries(&shared), [None; 8]);
}

#[test]
fn update_start_without_journal_removes_a_stale_temporary_or_is_not_consistent() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    // A copy of a newer generation left beside a kept component (an earlier
    // roll-back that did not remove it), no journal.
    shared.lock().unwrap().temporaries[5] = Some(digest(2));
    shared.lock().unwrap().temporaries_stuck = true;
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectedUnverified);
    assert!(!control.consistent_at_start());
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(temporaries(&shared)[5], Some(digest(2)));
    drop(control);
    // Removable: removed, no destination written, consistent.
    shared.lock().unwrap().temporaries_stuck = false;
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::NoJournal);
    assert!(control.consistent_at_start());
    assert_eq!(control.effects.calls, vec!["verify", "published"]);
    assert_eq!((store.journal(), barrier.closure()), (None, None));
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(temporaries(&shared), [None; 8]);
}

#[test]
fn update_unrecoverable_publication_stays_not_ready_until_completed() {
    let (store, shared) = interrupted_apply(3);
    shared.lock().unwrap().staged.clear();
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    assert!(!control.startup().recovery.consistent());
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    let status = serde_json::to_value(control.status()).unwrap();
    assert_eq!((status["barrier"].as_str(), status["journal"].as_str(), status["publication_consistent"].as_bool(),
        status["startup_recovery"].as_str(), status["admission_closed"].as_bool()),
        (Some("recovery_incomplete"), Some("publishing"), Some(false), Some("incomplete"), Some(true)));
    assert_eq!((status["set"].as_str(), status["closed_reason"].as_str()), (Some("inconsistent"), Some("publication_incomplete")));
    assert_eq!(control.report(), "inconsistent: publication_incomplete");
    // The mixed set is left exactly as found (no further renames).
    assert_eq!(slots(&shared).iter().filter(|slot| **slot == Some(digest(2))).count(), 3);
    let _admission = Observer::new(&barrier, 0);
    // No Resume lifts it, and Apply needs a re-keyed barrier and a verifiable generation.
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(2) });
    refused_outcome(&mut control, apply_command(digest(2)));
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    refused_outcome(&mut control, apply_command(digest(2)));
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(barrier.is_closed());
    // An operator restages the generation: the update endpoint completes it.
    shared.lock().unwrap().staged.push(digest(2));
    assert!(!control.restart_required());
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(serde_json::to_value(control.status()).unwrap()["publication_consistent"], true);
    // Never an in-process switch: this process asks for its restart, and
    // nothing (not even Resume) reopens admission here.
    assert!(control.restart_required());
    assert_eq!(control.report(), "recovering");
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(barrier.is_closed());
    drop(control);
    // The restart re-verifies everything; only then may Resume reopen admission.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert!(control.consistent_at_start());
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(!barrier.is_closed());
}

#[test]
fn update_unreadable_or_unknown_journal_is_not_ready() {
    let good = PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Published).unwrap().encode().unwrap();
    let text = String::from_utf8(good).unwrap();
    for bad in [text.replace('{', "{\"extra\":1,"), text.replace("\"schema\":1", "\"schema\":2"),
        text.replace("published", "committed"), text.get(..text.len() / 2).unwrap().to_owned()] {
        let store = MemoryStore::default();
        let shared = new_world(&store, digest(1), &[digest(1), digest(2)]);
        store.0.lock().unwrap().journal = Some(bad.clone().into_bytes());
        let (barrier, mut control) = restart(&store, &shared);
        assert_eq!(control.startup().recovery, PublicationRecovery::JournalUnreadable, "{bad}");
        assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
        // Never ignored: nothing was published or verified, and nothing reopens.
        assert!(control.effects.calls.is_empty());
        assert_eq!(serde_json::to_value(control.status()).unwrap()["journal"], "unreadable");
        let _admission = Observer::new(&barrier, 0);
        accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
        refused_outcome(&mut control, apply_command(digest(1)));
        refused_outcome(&mut control, ControlCommand::Resume { generation: digest(1) });
        assert!(barrier.is_closed());
        assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    }
}

/// Generation 1 installed; `Apply(2)` completed (`published`), power lost before `Resume`.
fn applied_not_resumed() -> (MemoryStore, SharedWorld) {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier, FakeEffects::over(&shared)).unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Published).unwrap()));
    (store, shared)
}

#[test]
fn update_published_journal_with_revoked_signer_is_not_trusted() {
    // The signing key of generation 2 is revoked after its Apply, before Resume:
    // a `published` journal is re-verified and rolled back to the trusted previous.
    let (store, shared) = applied_not_resumed();
    shared.lock().unwrap().revoked.push(digest(2));
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledBack);
    assert!(control.consistent_at_start());
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), Some(digest(2)), PublishState::Published).unwrap()));
    // Admission stays closed by the record of 2 until the updater re-drains and resumes 1.
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
    drop(control);
    // Both revoked: not consistent; nothing republished; nothing but the update endpoint.
    shared.lock().unwrap().revoked.push(digest(1));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    assert!(!control.consistent_at_start());
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert!(!control.opened());
    assert_eq!(control.report(), "inconsistent: publication_incomplete");
    refused_outcome(&mut control, ControlCommand::Resume { generation: digest(1) });
}

#[test]
fn update_published_journal_repairs_an_altered_destination() {
    let (store, shared) = applied_not_resumed();
    shared.lock().unwrap().slots[3] = Some(digest(9));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert_eq!(control.effects.calls, vec!["verify", "publish"]);
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    // Journaled like any publication: `publishing` was durable before the repair.
    let before = shared.lock().unwrap().journal_at_publish.last().copied().flatten();
    assert_eq!(before.map(|journal| journal.state), Some(PublishState::Publishing));
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Published).unwrap()));
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Resume { generation: digest(2) });
    assert!(!barrier.is_closed());
}

#[test]
fn update_without_journal_verifies_the_selected_generation() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    // Valid: consistent, nothing written.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::NoJournal);
    assert_eq!(control.effects.calls, vec!["verify", "published"]);
    assert_eq!((store.journal(), barrier.closure()), (None, None));
    assert_eq!(control.report(), "recovering");
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    drop(control);
    // A tampered destination: not consistent, left as found, audited with the generation.
    shared.lock().unwrap().slots[7] = Some(digest(9));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectedUnverified);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(slots(&shared)[7], Some(digest(9)));
    assert_eq!(store.journal(), None);
    assert!(!control.opened());
    assert_eq!(control.report(), "inconsistent: selected_unverified");
    assert_eq!(control.startup().audit_records(1).last().and_then(|record| record.detail.clone()),
        Some(Detail::PublicationRecovery { action: PublicationRecovery::SelectedUnverified, generation: Some(digest(1)),
            previous: None }));
    drop(control);
    // A revoked selected generation, and no selector at all: not consistent either.
    shared.lock().unwrap().slots = [Some(digest(1)); 8];
    shared.lock().unwrap().revoked.push(digest(1));
    assert_eq!(restart(&store, &shared).1.startup().recovery, PublicationRecovery::SelectedUnverified);
    shared.lock().unwrap().selector = None;
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectorAbsent);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
}

#[test]
fn update_repairing_apply_replies_then_takes_the_restart_path() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    shared.lock().unwrap().slots[6] = Some(digest(9));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectedUnverified);
    // No admission loop exists: Drain answers zero admitted runs on an idle
    // census, without waiting for an iteration.
    let started = Instant::now();
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    assert!(started.elapsed() < OBSERVATION_WAIT);
    // A refused request takes no restart path.
    let (client, server) = pair();
    transport::send(&client, &request(ControlCommand::Resume { generation: digest(1) })).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::Continue);
    assert!(matches!(transport::receive::<ControlResponse>(&client).unwrap().outcome, ControlOutcome::Refused));
    // The repairing Apply: its reply is sent before the restart is decided.
    let (client, server) = pair();
    let apply = request(apply_command(digest(1)));
    transport::send(&client, &apply).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::RestartAfterRepair);
    let response: ControlResponse = transport::receive(&client).unwrap();
    validate_response(&apply, &response).unwrap();
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), None, PublishState::Published).unwrap()));
    assert_eq!(control.report(), "recovering");
    assert!(!control.opened());
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(1))));
    drop(control);
    // The restarted process verifies everything, then opens.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Resume { generation: digest(1) });
    assert_eq!(store.journal(), None);
    assert!(!barrier.is_closed());
}

#[test]
fn update_start_binds_only_the_update_endpoint_when_not_consistent() {
    use std::os::unix::net::UnixListener;
    for consistent in [false, true] {
        // The runtime directory seam: every endpoint is bound below it.
        let runtime = tempfile::tempdir().unwrap();
        let path = |name: &str| runtime.path().join(name);
        let mut notified = Vec::new();
        let mut held = Vec::new();
        let opened = start(consistent,
            || { held.push(UnixListener::bind(path("update.sock"))?); Ok(()) },
            |state| notified.push(state.to_owned()),
            || Ok((UnixListener::bind(path("allocation.sock"))?, UnixListener::bind(path("control.sock"))?))).unwrap();
        // READY=1 always, and only after the update endpoint is bound.
        assert_eq!(notified, ["READY=1"]);
        assert_eq!(held.len(), 1);
        assert!(path("update.sock").exists());
        assert_eq!(opened.is_some(), consistent);
        assert_eq!(path("allocation.sock").exists(), consistent, "consistent={consistent}");
        assert_eq!(path("control.sock").exists(), consistent, "consistent={consistent}");
    }
    // No update endpoint: no READY=1 and nothing else.
    let mut notified = Vec::new();
    let mut admitted = false;
    assert!(start(true, || Err(io::Error::other("bind failed")), |state| notified.push(state.to_owned()),
        || { admitted = true; Ok(()) }).is_err());
    assert!(notified.is_empty() && !admitted);
}

#[test]
fn update_status_line_uses_fixed_vocabulary() {
    assert_eq!(status_line("consistent", None, true), "consistent");
    assert_eq!(status_line("recovering", None, true), "recovering");
    assert_eq!(status_line("inconsistent", Some("selector_absent"), true), "inconsistent: selector_absent");
    // A failed status-file write is reflected in STATUS=.
    assert_eq!(status_line("inconsistent", Some("journal_unreadable"), false),
        "inconsistent: journal_unreadable; status_file_write_failed");
    assert_eq!(status_line("consistent", None, false), "consistent; status_file_write_failed");
    for recovery in [PublicationRecovery::NoJournal, PublicationRecovery::Reverified, PublicationRecovery::RolledForward,
        PublicationRecovery::RolledBack] {
        assert!(recovery.consistent() && recovery.closed_reason().is_none());
    }
    for (recovery, reason) in [(PublicationRecovery::Incomplete, "publication_incomplete"),
        (PublicationRecovery::JournalUnreadable, "journal_unreadable"), (PublicationRecovery::SelectorAbsent, "selector_absent"),
        (PublicationRecovery::SelectorUnreadable, "selector_unreadable"),
        (PublicationRecovery::SelectedUnverified, "selected_unverified")] {
        assert!(!recovery.consistent());
        assert_eq!(recovery.closed_reason(), Some(reason));
    }
}

/// A systemd invocation id (32 lowercase hex digits) for test owners.
const INVOCATION: &str = "0123456789abcdef0123456789abcdef";
/// Make `control` the status owner a service start would make it (tests do
/// not run under systemd, so `$INVOCATION_ID` is not there), and return how
/// PID 1 would report that process.
fn own<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>) -> LiveSpawnd {
    control.owner = Some(Owner { boot_id: boot_id().unwrap(), pid: std::process::id(), invocation_id: INVOCATION.into() });
    LiveSpawnd { pid: std::process::id(), invocation_id: INVOCATION.into() }
}

#[test]
fn update_wait_consistent_accepts_only_a_fresh_matching_consistent_status() {
    let boot = "0123abcd-0000-4000-8000-000000000001";
    let owner = Owner { boot_id: boot.into(), pid: 4242, invocation_id: INVOCATION.into() };
    let status = |set: &str, owner: Option<&Owner>| serde_json::to_vec(&serde_json::json!({
        "schema": 3, "set": set, "closed_reason": null, "owner": owner, "admission_closed": false })).unwrap();
    let live = LiveSpawnd { pid: 4242, invocation_id: INVOCATION.into() };
    let good = status("consistent", Some(&owner));
    assert!(status_accepted(Some(&good), boot, Some(&live)));
    // Missing, unreadable, malformed.
    assert!(!status_accepted(None, boot, Some(&live)));
    assert!(!status_accepted(Some(b"{"), boot, Some(&live)));
    assert!(!status_accepted(Some(&vec![b' '; MAX_STATUS_BYTES + 1]), boot, Some(&live)));
    // Stale: another boot; no active spawnd (or the bus unavailable); the same
    // pid under a new invocation (a restart, or a recycled pid); another pid.
    assert!(!status_accepted(Some(&good), "0123abcd-0000-4000-8000-000000000002", Some(&live)));
    assert!(!status_accepted(Some(&good), boot, None));
    assert!(!status_accepted(Some(&good), boot, Some(&LiveSpawnd { pid: 4242, invocation_id: "f".repeat(32) })));
    assert!(!status_accepted(Some(&good), boot, Some(&LiveSpawnd { pid: 4243, invocation_id: INVOCATION.into() })));
    // Not consistent, no owner, an earlier schema, an owner with an unknown
    // field (a schema-2 `start_time`), an owner with a malformed invocation id.
    for set in ["recovering", "inconsistent"] {
        assert!(!status_accepted(Some(&status(set, Some(&owner))), boot, Some(&live)));
    }
    assert!(!status_accepted(Some(&status("consistent", None)), boot, Some(&live)));
    let mut value: serde_json::Value = serde_json::from_slice(&good).unwrap();
    value["schema"] = 2.into();
    assert!(!status_accepted(Some(&serde_json::to_vec(&value).unwrap()), boot, Some(&live)));
    let mut value: serde_json::Value = serde_json::from_slice(&good).unwrap();
    value["owner"]["start_time"] = 777.into();
    assert!(!status_accepted(Some(&serde_json::to_vec(&value).unwrap()), boot, Some(&live)));
    let malformed = Owner { invocation_id: INVOCATION.to_uppercase(), ..owner.clone() };
    assert!(!status_accepted(Some(&status("consistent", Some(&malformed))), boot,
        Some(&LiveSpawnd { pid: 4242, invocation_id: INVOCATION.to_uppercase() })));
    let (zeros, short): (String, String) = ("0".repeat(32), INVOCATION.chars().take(31).collect());
    for (id, valid) in [(INVOCATION, true), ("", false), (zeros.as_str(), false), (short.as_str(), false),
        ("0123456789abcdef0123456789abcdeg", false)] {
        assert_eq!(invocation_id_valid(id), valid, "{id:?}");
    }
    // The status this process writes is accepted only once everything is open.
    let current = boot_id().unwrap();
    let mut control = endpoint();
    let alive = own(&mut control);
    let written = |control: &mut UpdateControl<MemoryStore, FakeEffects>| serde_json::to_vec(&control.status()).unwrap();
    assert!(!status_accepted(Some(&written(&mut control)), &current, Some(&alive)));
    assert!(control.opened());
    assert!(status_accepted(Some(&written(&mut control)), &current, Some(&alive)));
    // Outside a service start there is no owner: never accepted.
    control.owner = None;
    assert!(!status_accepted(Some(&written(&mut control)), &current, Some(&alive)));
    // The waiter keeps waiting (here until its test timeout) and returns as soon as it is accepted.
    assert_eq!(wait_until(|| false, Some(Duration::from_millis(50)), Duration::from_millis(5)).unwrap_err().kind(),
        io::ErrorKind::TimedOut);
    let mut polls = 0;
    wait_until(|| { polls += 1; polls == 3 }, Some(Duration::from_secs(5)), Duration::from_millis(1)).unwrap();
    assert_eq!(polls, 3);
}

// ---------------------------------------------------------------- unit files

const SPAWND_UNIT: &str = include_str!("../../../scripts/linux-protected/spawnd/duduclaw-spawnd.service");
const READY_UNIT: &str = include_str!("../../../scripts/linux-protected/spawnd/duduclaw-isolation-ready.service");
const EGRESS_UNIT: &str = include_str!("../../duduclaw-egress/systemd/duduclaw-egress.service");
const UPDATED_UNIT: &str = include_str!("../../../scripts/linux-protected/updated/duduclaw-updated.service");
const AUTO_UPGRADE_UNIT: &str = include_str!("../../../scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade.service");
/// Directive lines (comments and blank lines dropped).
fn directives(unit: &str) -> Vec<&str> {
    unit.lines().filter(|line| !line.is_empty() && !line.starts_with('#')).collect()
}

#[test]
fn update_units_reaching_openat2_set_no_restrict_suid_sgid() {
    // R7g (systemd 255): under RestrictSUIDSGID= systemd's filter answers ENOSYS
    // for every openat2; these units' binaries need openat2's RESOLVE_*
    // guarantees. NoNewPrivileges=yes stays on every one of them.
    for unit in [SPAWND_UNIT, READY_UNIT, UPDATED_UNIT, AUTO_UPGRADE_UNIT] {
        let lines = directives(unit);
        assert!(!lines.iter().any(|line| line.starts_with("RestrictSUIDSGID=")), "{unit}");
        assert!(lines.contains(&"NoNewPrivileges=yes"), "{unit}");
    }
    // The updater binary drops its downloader child to duduclaw-dl (CAP_SETUID):
    // root by default, since an explicit User=root loses CAP_SETUID on systemd 255.
    for unit in [UPDATED_UNIT, AUTO_UPGRADE_UNIT] {
        assert!(!directives(unit).iter().any(|line| line.starts_with("User=") || line.starts_with("Group=")), "{unit}");
    }
    // Egress never calls openat2 (its own filter allows only openat): kept.
    assert!(directives(EGRESS_UNIT).contains(&"RestrictSUIDSGID=yes"));
}

#[test]
fn update_unit_isolation_ready_waits_for_spawnd_consistency() {
    use duduclaw_isolation::ids::ISOLATION_READY_UNIT;
    assert_eq!(ISOLATION_READY_UNIT, "duduclaw-isolation-ready.service");
    let lines = directives(READY_UNIT);
    for expected in ["Type=oneshot", "RemainAfterExit=yes", "ExecStart=/usr/lib/duduclaw/duduclaw-spawnd wait-consistent",
        "TimeoutStartSec=infinity", "User=root", "NoNewPrivileges=yes", "CapabilityBoundingSet=", "AmbientCapabilities=",
        "ProtectSystem=strict", "PrivateNetwork=yes", "IPAddressDeny=any", "PrivateDevices=yes", "ProtectHome=yes",
        "RestrictAddressFamilies=AF_UNIX", "StandardError=journal"] {
        assert!(lines.contains(&expected), "{expected}");
    }
    // Read-only, no network, nothing restarted behind the waiter's back.
    assert!(!lines.iter().any(|line| line.starts_with("ReadWritePaths=") || line.starts_with("Restart=")
        || line.contains("AF_INET")));
}

#[test]
fn update_unit_isolation_ready_lives_exactly_as_long_as_spawnd() {
    let lines = directives(READY_UNIT);
    // Stopped whenever spawnd stops or fails, restarted with it, run after its READY=1.
    for expected in ["BindsTo=duduclaw-spawnd.service", "PartOf=duduclaw-spawnd.service", "After=duduclaw-spawnd.service"] {
        assert!(lines.contains(&expected), "{expected}");
    }
    // BindsTo= replaces Requires= (a Requires= gate stayed up after a crash).
    assert!(!lines.iter().any(|line| line.starts_with("Requires=")));
    // Started by spawnd's own start (a [Unit] relation), not by an enable step.
    assert!(!READY_UNIT.lines().any(|line| line == "[Install]"));
    assert!(directives(SPAWND_UNIT).contains(&"Wants=duduclaw-isolation-ready.service"));
}

#[test]
fn update_unit_spawnd_ready_is_bounded_and_restarts_after_repair() {
    use duduclaw_updated::linux::activation::{RESTART_DEADLINE, SPAWND_START_TIMEOUT_SECS, SPAWND_STOP_TIMEOUT_SECS};
    let lines = directives(SPAWND_UNIT);
    let exit = RESTART_AFTER_REPAIR_EXIT.to_string();
    for expected in ["Type=notify".to_owned(), "NotifyAccess=main".to_owned(),
        format!("TimeoutStartSec={SPAWND_START_TIMEOUT_SECS}"), format!("TimeoutStopSec={SPAWND_STOP_TIMEOUT_SECS}"),
        "Restart=on-failure".to_owned(), format!("RestartForceExitStatus={exit}"), format!("SuccessExitStatus={exit}"),
        "StartLimitIntervalSec=300".to_owned(), "StartLimitBurst=5".to_owned(), "StandardError=journal".to_owned()] {
        assert!(lines.contains(&expected.as_str()), "{expected}");
    }
    // The updater's one restart deadline outlasts a whole stop plus start.
    assert!(RESTART_DEADLINE > Duration::from_secs(SPAWND_START_TIMEOUT_SECS + SPAWND_STOP_TIMEOUT_SECS));
    // The failure exit of an open process is a failure: restarted by
    // Restart=on-failure and counted, never "success" and never prevented.
    let failure = EXIT_SET_INCONSISTENT.to_string();
    assert_ne!(EXIT_SET_INCONSISTENT, RESTART_AFTER_REPAIR_EXIT);
    for line in &lines {
        for key in ["SuccessExitStatus=", "RestartPreventExitStatus=", "RestartForceExitStatus="] {
            if let Some(statuses) = line.strip_prefix(key) {
                assert!(!statuses.split_ascii_whitespace().any(|status| status == failure), "{line}");
            }
        }
    }
    // Only the waiter unit has no start timeout; stderr is no longer discarded.
    assert!(!lines.contains(&"TimeoutStartSec=infinity"));
    assert!(!lines.contains(&"StandardError=null"));
}

#[test]
fn update_unit_egress_and_gateway_depend_on_isolation_ready() {
    use duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES;
    // Egress carries exactly the lines the installer package adds to the gateway unit.
    assert!(EGRESS_UNIT.contains(ISOLATED_GATEWAY_UNIT_DEPENDENCIES));
    let lines = directives(EGRESS_UNIT);
    for expected in ISOLATED_GATEWAY_UNIT_DEPENDENCIES.lines() { assert!(lines.contains(&expected), "{expected}"); }
    // Stopped when the gate drops: BindsTo=, and no leftover Requires= on the gate.
    assert!(lines.contains(&"BindsTo=duduclaw-isolation-ready.service"));
    assert!(!lines.contains(&"Requires=duduclaw-isolation-ready.service"));
    // Not on spawnd directly: its READY=1 means only "update endpoint bound".
    assert!(!lines.iter().any(|line| line.contains("duduclaw-spawnd.service")));
}

#[test]
fn update_unit_updater_depends_only_on_spawnd_start() {
    let lines = directives(UPDATED_UNIT);
    assert!(lines.contains(&"Wants=duduclaw-spawnd.service"));
    assert!(lines.contains(&"After=duduclaw-spawnd.service"));
    // Requires= would propagate the updater's own `systemctl restart duduclaw-spawnd.service`;
    // BindsTo=/PartOf= would stop or restart the updater with every spawnd restart.
    assert!(!lines.iter().any(|line| line.starts_with("Requires=") || line.starts_with("BindsTo=")
        || line.starts_with("PartOf=")));
    for unit in ["duduclaw-gateway.service", "duduclaw-egress.service", "duduclaw-isolation-ready.service"] {
        assert!(!lines.iter().any(|line| line.contains(unit)), "{unit}");
    }
}

#[test]
fn update_apply_refused_while_another_generation_is_publishing() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2), digest(3)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    // A late failure in this process leaves `publishing` for 2 (4 of 8 replaced);
    // the open process stops serving (see the failure-exit test).
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    shared.lock().unwrap().power_loss_after = Some(4);
    refused_outcome(&mut control, apply_command(digest(2)));
    assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Publishing));
    assert!(control.exit_required());
    drop(control);
    // Its restart can neither complete 2 nor roll back to 1 (both unverifiable
    // for now): only the update endpoint serves, with `publishing` for 2.
    shared.lock().unwrap().staged.retain(|generation| *generation == digest(3));
    let (_barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    control.effects.calls.clear();
    accepted(&mut control, ControlCommand::Drain { generation: digest(3) });
    refused_outcome(&mut control, apply_command(digest(3)));
    assert!(control.effects.calls.is_empty());
    assert_eq!(store.journal().map(|journal| journal.generation), Some(digest(2)));
    // Rolling back to the journal's previous generation is allowed (it finishes the journal).
    shared.lock().unwrap().staged.push(digest(1));
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    accepted(&mut control, apply_command(digest(1)));
    assert!(control.restart_required());
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), Some(digest(2)), PublishState::Published).unwrap()));
    drop(control);
    // Once published (and re-verified by the restart), another generation may
    // be applied; its previous is the installed one.
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Drain { generation: digest(3) });
    accepted(&mut control, apply_command(digest(3)));
    assert_eq!(slots(&shared), [Some(digest(3)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(3), digest(3), Some(digest(1)), PublishState::Published).unwrap()));
}

#[test]
fn update_drain_and_apply_refused_when_admission_is_unhealthy() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::default();
    let admission = Observer::new(&barrier, 0);
    admission.healthy.store(false, Ordering::Release);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    control.effects.calls.clear();
    // Zero runs from an unhealthy iteration is not a drain; the barrier stays closed and recorded.
    refused_outcome(&mut control, ControlCommand::Drain { generation: digest(7) });
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    assert_eq!(store.clone().load().unwrap(), Some(digest(7)));
    admission.healthy.store(true, Ordering::Release);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    admission.healthy.store(false, Ordering::Release);
    // Let an unhealthy iteration land before Apply's fresh observation.
    std::thread::sleep(Duration::from_millis(30));
    refused_outcome(&mut control, apply_command(digest(7)));
    assert!(control.effects.calls.is_empty());
    assert_eq!(store.journal(), None);
    admission.healthy.store(true, Ordering::Release);
    std::thread::sleep(Duration::from_millis(30));
    accepted(&mut control, apply_command(digest(7)));
}

#[test]
fn update_open_apply_failure_closes_admission_and_takes_the_failure_exit() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2), digest(3)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    // A refusal before the journal (verification) leaves the set as it was:
    // the process keeps serving.
    accepted(&mut control, ControlCommand::Drain { generation: digest(9) });
    let (client, server) = pair();
    transport::send(&client, &request(apply_command(digest(9)))).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::Continue);
    assert!(matches!(transport::receive::<ControlResponse>(&client).unwrap().outcome, ControlOutcome::Refused));
    assert_eq!(store.journal(), None);
    assert_eq!(control.report(), "consistent");
    // A failure after `publishing` landed (3 of 8 replaced): the reply is sent,
    // then the failure exit is decided; admission was closed before the reply.
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    shared.lock().unwrap().power_loss_after = Some(3);
    let (client, server) = pair();
    transport::send(&client, &request(apply_command(digest(2)))).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::ExitInconsistent);
    assert!(matches!(transport::receive::<ControlResponse>(&client).unwrap().outcome, ControlOutcome::Refused));
    assert!(control.exit_required() && !control.restart_required());
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(control.report(), "inconsistent: publication_incomplete");
    let status = serde_json::to_value(control.status()).unwrap();
    assert_eq!((status["set"].as_str(), status["admission_closed"].as_bool(), status["barrier"].as_str(),
        status["journal"].as_str()), (Some("inconsistent"), Some(true), Some("recovery_incomplete"), Some("publishing")));
    // Nothing else is served, and nothing reopens admission, until it exits.
    for command in [ControlCommand::Resume { generation: digest(2) }, ControlCommand::Drain { generation: digest(2) },
        apply_command(digest(2)), ControlCommand::Readback { generation: digest(2) }] {
        let (client, server) = pair();
        transport::send(&client, &request(command)).unwrap();
        assert_eq!(serve_and_settle(server, me(), &mut control), After::ExitInconsistent);
        assert!(matches!(transport::receive::<ControlResponse>(&client).unwrap().outcome, ControlOutcome::Refused));
    }
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(slots(&shared).iter().filter(|slot| **slot == Some(digest(2))).count(), 3);
    // The exit's audit record names the journal as found; a record that did not
    // land is part of STATUS=.
    let record = inconsistent_record(&mut store.clone(), 5);
    assert_eq!((record.event, record.detail.clone()), (crate::audit::Event::UpdateSetInconsistent,
        Some(Detail::PublicationRecovery { action: PublicationRecovery::Incomplete, generation: Some(digest(2)),
            previous: Some(digest(1)) })));
    assert_eq!(report_audited(&mut control, vec![record], |_| false), "inconsistent: publication_incomplete; audit_write_failed");
    drop(control);
    // The restart (systemd's, after EXIT_SET_INCONSISTENT) completes the publication.
    let (barrier, control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward);
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(2))));
    // An unreadable journal found by a later request also ends an open process.
    drop(control);
    let (barrier, mut control) = restart(&store, &shared);
    let _admission = Observer::new(&barrier, 0);
    assert!(control.opened());
    store.0.lock().unwrap().journal = Some(b"{\"schema\":".to_vec());
    let (client, server) = pair();
    transport::send(&client, &request(ControlCommand::Readback { generation: digest(2) })).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::ExitInconsistent);
    assert_eq!(control.report(), "inconsistent: journal_unreadable");
    assert_eq!(inconsistent_record(&mut store.clone(), 6).detail,
        Some(Detail::PublicationRecovery { action: PublicationRecovery::JournalUnreadable, generation: None, previous: None }));
}

#[test]
fn update_closed_drain_and_apply_follow_the_workload_census() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    shared.lock().unwrap().slots[6] = Some(digest(9));
    let (barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectedUnverified);
    // A run unit of an earlier spawnd still live, a populated employees cgroup,
    // or a census that cannot be completed: Drain is refused (the record and
    // the closure still land: fail closed).
    for census in [Census::Busy(LIVE_RUN), Census::Busy(POPULATED), Census::Fails] {
        control.effects.census = census;
        refused_outcome(&mut control, ControlCommand::Drain { generation: digest(1) });
        assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(1))));
        assert_eq!(store.clone().load().unwrap(), Some(digest(1)));
    }
    control.effects.census = Census::Idle;
    match outcome(&mut control, ControlCommand::Drain { generation: digest(1) }) {
        ControlOutcome::Drained { admission_closed: true, active_runs: 0 } => {}
        other => panic!("{other:?}"),
    }
    // Apply re-takes the census: refused before anything is verified or written.
    control.effects.calls.clear();
    for census in [Census::Busy(LIVE_RUN), Census::Busy(POPULATED), Census::Fails] {
        control.effects.census = census;
        refused_outcome(&mut control, apply_command(digest(1)));
    }
    assert!(control.effects.calls.is_empty());
    assert_eq!(slots(&shared)[6], Some(digest(9)));
    assert!(!control.restart_required());
    control.effects.census = Census::Idle;
    accepted(&mut control, apply_command(digest(1)));
    assert!(control.restart_required());
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
}

#[test]
fn update_open_apply_also_requires_an_idle_census() {
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(MemoryStore::default(), barrier.clone(), FakeEffects::default()).unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    control.effects.calls.clear();
    // The admission loop counts zero, but PID 1 or cgroupfs still see a run.
    for census in [Census::Busy(LIVE_RUN), Census::Busy(POPULATED), Census::Fails] {
        control.effects.census = census;
        refused_outcome(&mut control, apply_command(digest(7)));
    }
    assert!(control.effects.calls.is_empty());
    assert!(!control.exit_required());
    control.effects.census = Census::Idle;
    accepted(&mut control, apply_command(digest(7)));
}

#[test]
fn update_apply_refused_while_the_gate_or_a_bound_unit_is_up_or_uncertain() {
    // The real fixed names: the gate, egress's socket and service, the gateway.
    assert_eq!(GATED_UNITS, ["duduclaw-isolation-ready.service", "duduclaw-egress.socket", "duduclaw-egress.service",
        "duduclaw-gateway.service"]);
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    // PID 1 reports one of them active/starting/with a job, or no certain answer:
    // refused after verification, before the journal and before any status change.
    for gate in [Gate::Up, Gate::Uncertain] {
        control.effects.gate = gate;
        refused_outcome(&mut control, apply_command(digest(2)));
        assert_eq!(store.journal(), None, "{gate:?}");
        assert_eq!(slots(&shared), [Some(digest(1)); 8], "{gate:?}");
        assert_eq!(control.report(), "consistent", "{gate:?}");
        assert!(!control.exit_required() && !control.restart_required());
    }
    // All inactive: accepted, and every check ran before the journal existed.
    control.effects.gate = Gate::Down;
    control.effects.gate_checks.clear();
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(control.effects.gate_checks, vec![None, None]);
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Published));
}

#[test]
fn update_accepted_apply_leaves_consistent_before_the_journal_for_good() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    let alive = own(&mut control);
    let boot = boot_id().unwrap();
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    // The gate completes against the old status between the first check and
    // the status change: the second check (after the change) refuses. The
    // status already left `consistent` and the journal was never written.
    control.effects.gate_after_first = Some(Gate::Up);
    refused_outcome(&mut control, apply_command(digest(2)));
    assert_eq!(control.effects.gate_checks.len(), 2);
    assert_eq!(store.journal(), None);
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(control.report(), "recovering");
    assert!(!status_accepted(Some(&serde_json::to_vec(&control.status()).unwrap()), &boot, Some(&alive)));
    // The updater stops the gate again and retries: accepted, still `recovering`;
    // this invocation never says `consistent` again (only its restart does).
    control.effects.gate_after_first = None;
    control.effects.gate = Gate::Down;
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(control.report(), "recovering");
    assert!(!status_accepted(Some(&serde_json::to_vec(&control.status()).unwrap()), &boot, Some(&alive)));
    assert!(!control.opened());
    assert!(!control.exit_required() && !control.restart_required());
    drop(control);
    // The restarted, re-verified process is the one that says `consistent`.
    let (_barrier, mut control) = restart(&store, &shared);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
}

#[test]
fn update_repairing_apply_also_requires_the_gate_down() {
    // Started not consistent: the gate never completed; the updater stopped it.
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1)]);
    shared.lock().unwrap().slots[6] = Some(digest(9));
    let (_barrier, mut control) = restart(&store, &shared);
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    for gate in [Gate::Up, Gate::Uncertain] {
        control.effects.gate = gate;
        refused_outcome(&mut control, apply_command(digest(1)));
        assert_eq!(store.journal(), None);
        assert!(!control.restart_required());
    }
    control.effects.gate = Gate::Down;
    control.effects.gate_checks.clear();
    accepted(&mut control, apply_command(digest(1)));
    // One check (this process never said `consistent`), then the restart path.
    assert_eq!(control.effects.gate_checks, vec![None]);
    assert!(control.restart_required());
    assert_eq!(control.report(), "recovering");
}

#[test]
fn update_employees_cgroup_census_reads_cgroupfs_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    // Not the unified hierarchy (no cgroup.controllers, or not a file): no census.
    assert!(employees_cgroup_populated(root.path()).is_err());
    std::fs::create_dir(root.path().join("cgroup.controllers")).unwrap();
    assert!(employees_cgroup_populated(root.path()).is_err());
    std::fs::remove_dir(root.path().join("cgroup.controllers")).unwrap();
    std::fs::write(root.path().join("cgroup.controllers"), "cpu io memory pids\n").unwrap();
    // No employees slice at all: nothing can run there.
    assert!(!employees_cgroup_populated(root.path()).unwrap());
    let slice = root.path().join("duduclaw.slice").join("duduclaw-employees.slice");
    std::fs::create_dir_all(&slice).unwrap();
    assert!(!employees_cgroup_populated(root.path()).unwrap());
    std::fs::write(slice.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    assert!(employees_cgroup_populated(root.path()).unwrap());
    std::fs::write(slice.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
    assert!(!employees_cgroup_populated(root.path()).unwrap());
    for malformed in ["", "populated 2\n", "frozen 0\n", "populated 0\npopulated 0\n"] {
        std::fs::write(slice.join("cgroup.events"), malformed).unwrap();
        assert!(employees_cgroup_populated(root.path()).is_err(), "{malformed:?}");
    }
    assert!(Workloads::default().idle());
    assert!(!LIVE_RUN.idle() && !POPULATED.idle());
}

#[test]
fn update_startup_audit_failure_appears_in_status() {
    let store = MemoryStore::default();
    {
        let mut disk = store.0.lock().unwrap();
        disk.bytes = Some(encode_record(digest(7)).unwrap());
        disk.journal = Some(PublishJournal::new(digest(7), digest(7), None, PublishState::Published).unwrap().encode().unwrap());
    }
    let mut control = UpdateControl::new(store, DrainBarrier::default(), FakeEffects::default()).unwrap();
    let records = control.startup().audit_records(1);
    assert_eq!(records.len(), 2);
    // Both written (acknowledged): nothing appended.
    let mut written = 0;
    assert_eq!(report_audited(&mut control, records, |_| { written += 1; true }), "recovering");
    assert_eq!(written, 2);
    // One not acknowledged (write or fsync failed, queue full, no answer in time).
    let records = control.startup().audit_records(1);
    let line = report_audited(&mut control, records,
        |record| record.event != crate::audit::Event::PublicationRecovery);
    assert_eq!(line, "recovering; audit_write_failed");
    // It stays part of every later STATUS= line of this process.
    assert!(control.opened());
    assert_eq!(control.report(), "consistent; audit_write_failed");
}

#[test]
fn update_waiter_refuses_a_non_unified_hierarchy_and_matches_only_the_live_spawnd() {
    // Not on the unified cgroup v2 hierarchy: an error at once, nothing read.
    let (mut reads, mut lookups) = (0, 0);
    let started = Instant::now();
    let error = wait_for_consistency(false, "boot", || { reads += 1; None }, || { lookups += 1; None }, None,
        Duration::from_millis(1)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), NOT_UNIFIED_MESSAGE);
    assert_eq!((reads, lookups), (0, 0));
    assert!(started.elapsed() < Duration::from_secs(1));
    // Unified: only the spawnd PID 1 reports now, with the owner's pid and
    // invocation id, completes the wait.
    let boot = "0123abcd-0000-4000-8000-000000000001";
    let owner = Owner { boot_id: boot.into(), pid: 4242, invocation_id: INVOCATION.into() };
    let bytes = serde_json::to_vec(&serde_json::json!({ "schema": 3, "set": "consistent", "owner": owner })).unwrap();
    for live in [None, Some(LiveSpawnd { pid: 4242, invocation_id: "e".repeat(32) }),
        Some(LiveSpawnd { pid: 4241, invocation_id: INVOCATION.into() })] {
        let error = wait_for_consistency(true, boot, || Some(bytes.clone()), || live.clone(),
            Some(Duration::from_millis(30)), Duration::from_millis(5)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{live:?}");
    }
    let matching = LiveSpawnd { pid: 4242, invocation_id: INVOCATION.into() };
    wait_for_consistency(true, boot, || Some(bytes.clone()), || Some(matching.clone()), Some(Duration::from_secs(5)),
        Duration::from_millis(1)).unwrap();
}

#[test]
fn update_startup_status_and_audit_records() {
    let store = MemoryStore::default();
    {
        let mut disk = store.0.lock().unwrap();
        disk.bytes = Some(encode_record(digest(7)).unwrap());
        disk.journal = Some(PublishJournal::new(digest(7), digest(7), Some(digest(6)), PublishState::Published)
            .unwrap().encode().unwrap());
    }
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier, FakeEffects::default()).unwrap();
    let hex7 = "07".repeat(32);
    let mut status = serde_json::to_value(control.status()).unwrap();
    // The owner identifies this process's service start in this boot
    // (staleness for the waiter); without `$INVOCATION_ID` there is none.
    let owner: Option<Owner> = serde_json::from_value(status.as_object_mut().unwrap().remove("owner").unwrap()).unwrap();
    assert_eq!(owner, Owner::current().ok());
    assert_eq!(status, serde_json::json!({
        "schema": 3, "set": "recovering", "closed_reason": null,
        "admission_closed": true, "barrier": "drained", "barrier_generation": hex7,
        "journal": "published", "journal_generation": hex7, "journal_previous": "06".repeat(32),
        "publication_consistent": true, "startup_barrier": "drained", "startup_barrier_generation": hex7,
        "startup_recovery": "reverified" }));
    // Verified, not yet opened: `recovering`; after `opened`: `consistent`.
    assert_eq!(control.report(), "recovering");
    assert!(control.opened());
    assert!(!control.opened());
    assert_eq!(control.report(), "consistent");
    let records = control.startup().audit_records(42);
    assert_eq!(records.len(), 2);
    assert_eq!((records[0].event, records[0].at_unix_ms, records[0].detail.clone()),
        (crate::audit::Event::UpdateBarrierRestored, 42, Some(Detail::UpdateBarrier { generation: Some(digest(7)) })));
    assert_eq!((records[1].event, records[1].detail.clone()), (crate::audit::Event::PublicationRecovery,
        Some(Detail::PublicationRecovery { action: PublicationRecovery::Reverified, generation: Some(digest(7)),
            previous: Some(digest(6)) })));
    for record in &records { assert!(serde_json::to_vec(record).unwrap().len() < 1024); }
    // An unreadable record is audited without a generation; no journal, no journal record.
    let store = MemoryStore::default();
    store.0.lock().unwrap().bytes = Some(b"{\"version\":1".to_vec());
    let mut control = UpdateControl::new(store, DrainBarrier::default(), FakeEffects::default()).unwrap();
    let records = control.startup().audit_records(1);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].detail, Some(Detail::UpdateBarrier { generation: None }));
    let status = serde_json::to_value(control.status()).unwrap();
    assert_eq!((status["barrier"].as_str(), status["startup_barrier"].as_str(), status["journal"].as_str(),
        status["startup_recovery"].as_str()), (Some("unreadable_record"), Some("unreadable_record"), Some("absent"),
        Some("no_journal")));
    // Nothing found: nothing audited.
    let control = UpdateControl::new(MemoryStore::default(), DrainBarrier::default(), FakeEffects::default()).unwrap();
    assert!(control.startup().audit_records(1).is_empty());
}

#[test]
fn update_schema_zero_generation_and_effect_errors_are_refused() {
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(MemoryStore::default(), barrier.clone(), FakeEffects::default()).unwrap();
    control.effects.calls.clear();
    let mut wrong = request(ControlCommand::Prepare { preparation: preparation(digest(1)) });
    wrong.schema_version = 2;
    let response = control.respond(&wrong);
    assert!(matches!(response.outcome, ControlOutcome::Refused));
    assert_eq!(response.schema_version, 1);
    for command in [ControlCommand::Prepare { preparation: preparation(digest(0)) },
        ControlCommand::Drain { generation: digest(0) }, ControlCommand::Readback { generation: digest(0) },
        ControlCommand::Resume { generation: digest(0) }] {
        assert!(matches!(outcome(&mut control, command), ControlOutcome::Refused));
    }
    assert!(control.effects.calls.is_empty());
    assert!(!barrier.is_closed());
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(digest(1)) });
    accepted(&mut control, ControlCommand::Readback { generation: digest(1) });
    control.effects.fail = true;
    assert!(matches!(outcome(&mut control, ControlCommand::Prepare { preparation: preparation(digest(1)) }), ControlOutcome::Refused));
}

// ---------------------------------------------------------------- admission

#[derive(Default)]
struct OneRunHost { end: bool }
impl Host for OneRunHost {
    type Inputs = ();
    type Io = ();
    fn ready(&mut self, _: EmployeeId) -> crate::Result<()> { Ok(()) }
    fn policy(&mut self, _: EmployeeId, _: &RuntimeKind, _: bool) -> crate::Result<LaunchPolicy> {
        Ok(LaunchPolicy { queue_class: QueueClass::Background, queue_ms: 60_000, startup_ms: 60_000, runtime_ms: 60_000,
            pty_idle_ms: None, ptys: 0, pty_size: None, delivery: IoDelivery { mcp: false, pty: false } })
    }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> crate::Result<CleanupReadback> {
        Ok(CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true })
    }
    fn start(&mut self, _: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: ()) -> crate::Result<()> { Ok(()) }
    fn poll(&mut self, _: RunHandle) -> crate::Result<HostProgress> {
        Ok(if self.end { HostProgress::Ended(RunOutcome::Exited(0)) } else { HostProgress::Running { last_activity_ms: 1 } })
    }
    fn revoke(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn kill(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn take_io(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> crate::Result<()> { Ok(()) }
    fn detect_intruders(&mut self) -> crate::Result<Vec<EmployeeId>> { Ok(vec![]) }
    fn forget(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
}
fn connection() -> ControlConnectionId { ControlConnectionId([1; 16]) }
fn start_request(n: u8) -> SpawnRequest {
    SpawnRequest::StartRun { run: RunHandle(RunId([n; 16])), employee_id: EmployeeId([n; 16]), runtime_request: RuntimeKind::Codex,
        task_ref: None, workspace: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false,
        requested_max_turns: None }
}
fn reserve(coordinator: &mut Coordinator<OneRunHost>, n: u8, now: u64) {
    coordinator.reserve(connection(), EmployeeId([n; 16]), RunHandle(RunId([n; 16])), now).unwrap();
}
/// Answer one request on its own thread (as in production) while this thread
/// is the admission loop: tick, then report the admitted runs of the iteration.
fn respond_while_admitting<S: DrainStore + JournalStore + Send + 'static, E: Effects + Send + 'static>(control: UpdateControl<S, E>,
    command: ControlCommand, coordinator: &mut Coordinator<OneRunHost>, barrier: &DrainBarrier, now: &mut u64)
    -> (UpdateControl<S, E>, ControlRequest, ControlResponse) {
    let request = request(command);
    let sent = request.clone();
    let worker = std::thread::spawn(move || { let mut control = control; let response = control.respond(&sent); (control, response) });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "update control did not answer");
        let epoch = barrier.epoch();
        *now += 1;
        let healthy = coordinator.tick(*now).is_ok();
        barrier.observe(epoch, coordinator.admitted_runs(), healthy);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (control, response) = worker.join().unwrap();
    (control, request, response)
}

/// Shared by the unit test (memory store) and the root test (disk store).
fn drain_waits_for_active_run<S: DrainStore + JournalStore + Clone + Send + 'static>(store: S) {
    let barrier = DrainBarrier::default();
    let mut coordinator = Coordinator::new(OneRunHost::default(), Limits::default()).unwrap();
    coordinator.set_drain_barrier(barrier.clone());
    let mut now = 100;
    reserve(&mut coordinator, 1, now);
    coordinator.start(connection(), start_request(1), (), now).unwrap();
    coordinator.tick(now).unwrap();
    reserve(&mut coordinator, 2, now);
    let control = UpdateControl::new(store.clone(), barrier.clone(), NoEffects).unwrap();
    let (control, request, response) = respond_while_admitting(control, ControlCommand::Drain { generation: digest(7) },
        &mut coordinator, &barrier, &mut now);
    // The barrier is closed and durable, but one admitted run remains: the
    // updater's client rejects this reply and retries (activation.rs:72-80).
    assert!(matches!(response.outcome, ControlOutcome::Drained { admission_closed: true, active_runs: 1 }));
    assert!(validate_response(&request, &response).is_err());
    assert_eq!(store.clone().load().unwrap(), Some(digest(7)));
    // New runs are refused; the admitted one keeps running.
    assert_eq!(coordinator.start(connection(), start_request(2), (), now), Err(ErrorCode::NotReady));
    assert_eq!(coordinator.admitted_runs(), 1);
    coordinator.host_mut_for_test().end = true;
    let (control, request, response) = respond_while_admitting(control, ControlCommand::Drain { generation: digest(7) },
        &mut coordinator, &barrier, &mut now);
    validate_response(&request, &response).unwrap();
    assert_eq!(coordinator.admitted_runs(), 0);
    drop(control);
    // Restart: the record closes a fresh admission loop before its first run.
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), NoEffects).unwrap();
    let mut coordinator = Coordinator::new(OneRunHost::default(), Limits::default()).unwrap();
    coordinator.set_drain_barrier(barrier.clone());
    reserve(&mut coordinator, 3, now);
    assert_eq!(coordinator.start(connection(), start_request(3), (), now), Err(ErrorCode::NotReady));
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.clone().load().unwrap(), None);
    coordinator.start(connection(), start_request(3), (), now).unwrap();
}

#[test]
fn update_drain_waits_for_admitted_runs_and_refuses_new_starts() {
    drain_waits_for_active_run(MemoryStore::default());
}

// ---------------------------------------------------------------- endpoint frames

fn pair() -> (OwnedFd, OwnedFd) {
    let (client, server) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
    transport::timeout(&client).unwrap();
    (client, server)
}
fn me() -> (u32, u32) {
    // SAFETY: getuid/getgid take no arguments and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}
fn raw(fd: &OwnedFd, bytes: &[u8]) {
    socket::sendmsg::<UnixAddr>(fd.as_raw_fd(), &[IoSlice::new(bytes)], &[], MsgFlags::MSG_NOSIGNAL, None).unwrap();
}
fn endpoint() -> UpdateControl<MemoryStore, FakeEffects> {
    UpdateControl::new(MemoryStore::default(), DrainBarrier::default(), FakeEffects::default()).unwrap()
}
/// The server refused the connection: it returned an error and sent nothing.
fn refused(client: &OwnedFd, result: io::Result<()>) {
    assert!(result.is_err());
    assert!(transport::receive::<ControlResponse>(client).is_err());
}

#[test]
fn update_endpoint_answers_one_valid_frame() {
    let (client, server) = pair();
    let request = request(ControlCommand::Resume { generation: digest(4) });
    transport::send(&client, &request).unwrap();
    serve_one(server, me(), &mut endpoint()).unwrap();
    let response: ControlResponse = transport::receive(&client).unwrap();
    validate_response(&request, &response).unwrap();
}

#[test]
fn update_endpoint_refuses_other_peers() {
    for peer in [(me().0.wrapping_add(1), me().1), (me().0, me().1.wrapping_add(1))] {
        let (client, server) = pair();
        transport::send(&client, &request(ControlCommand::Resume { generation: digest(4) })).unwrap();
        refused(&client, serve_one(server, peer, &mut endpoint()));
    }
}

#[test]
fn update_endpoint_refuses_unknown_fields_and_methods() {
    let valid = serde_json::to_value(request(ControlCommand::Resume { generation: digest(4) })).unwrap();
    let mut top = valid.clone(); top["extra"] = 1.into();
    let mut nested = valid.clone(); nested["command"]["path"] = "/tmp/x".into();
    let mut method = valid.clone(); method["command"]["method"] = "reboot".into();
    let mut flagged = valid;
    flagged["command"] = serde_json::json!({"method": "drain", "generation": serde_json::to_value(digest(4)).unwrap(), "force": true});
    for value in [top, nested, method, flagged] {
        let (client, server) = pair();
        raw(&client, &duduclaw_isolation::ipc::frame_payload(&serde_json::to_vec(&value).unwrap()).unwrap());
        refused(&client, serve_one(server, me(), &mut endpoint()));
    }
}

#[test]
fn update_endpoint_refuses_ancillary_data() {
    let (client, server) = pair();
    let frame = duduclaw_isolation::ipc::frame_payload(&serde_json::to_vec(&request(ControlCommand::Resume { generation: digest(4) })).unwrap()).unwrap();
    let file = File::open("/dev/null").unwrap();
    let rights = [file.as_raw_fd()];
    socket::sendmsg::<UnixAddr>(client.as_raw_fd(), &[IoSlice::new(&frame)], &[ControlMessage::ScmRights(&rights)],
        MsgFlags::MSG_NOSIGNAL, None).unwrap();
    refused(&client, serve_one(server, me(), &mut endpoint()));
}

#[test]
fn update_endpoint_refuses_short_long_and_mismatched_frames() {
    let body = serde_json::to_vec(&request(ControlCommand::Resume { generation: digest(4) })).unwrap();
    let mut trailing = duduclaw_isolation::ipc::frame_payload(&body).unwrap(); trailing.push(b' ');
    let mut claims_more = ((body.len() + 10) as u32).to_be_bytes().to_vec(); claims_more.extend_from_slice(&body);
    let mut too_long = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(); too_long.extend_from_slice(&body);
    for frame in [vec![0u8; 3], 0u32.to_be_bytes().to_vec(), trailing, claims_more, too_long, body] {
        let (client, server) = pair();
        raw(&client, &frame);
        refused(&client, serve_one(server, me(), &mut endpoint()));
    }
}

#[test]
fn update_endpoint_times_out_a_silent_peer() {
    let (client, server) = pair();
    let started = Instant::now();
    refused(&client, serve_one(server, me(), &mut endpoint()));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(10), "{elapsed:?}");
}

// ---------------------------------------------------------------- readback and publication

fn manifest_with(artifacts: Vec<ReleaseArtifact>) -> ReleaseManifest {
    ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: ReleaseVersion { major: 1, minor: 83, patch: 0 },
        minimum_version: ReleaseVersion { major: 1, minor: 82, patch: 0 }, serial: 1, channel: ReleaseChannel::Stable,
        arch: if cfg!(target_arch = "aarch64") { Architecture::Aarch64 } else { Architecture::X86_64 },
        os: OperatingSystem::Linux, created_at_unix: 1, expires_at_unix: 4_000_000_000,
        protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1, mcp_bridge: 1,
            parse_worker: 1, importer: 1 }, artifacts, migrations: vec![] }
}
fn all_targets() -> Vec<InstallTarget> {
    vec![InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline, InstallTarget::Egress,
        InstallTarget::EgressForwarder, InstallTarget::McpBridge, InstallTarget::ParseWorker, InstallTarget::Importer,
        InstallTarget::Migration, InstallTarget::UpdateSlot]
}
fn fixed_manifest() -> ReleaseManifest {
    let mut targets = all_targets();
    targets.push(InstallTarget::RuntimeEntry { registry_id: "api-worker-v1".into() });
    manifest_with(targets.into_iter().enumerate().map(|(index, target)| ReleaseArtifact {
        id: artifact_name(&target).unwrap(), sha256: Sha256Digest([index as u8 + 1; 32]), size_bytes: 1, mode: 0o755,
        install_target: target }).collect())
}

#[test]
fn update_readback_reports_only_spawnd_side_components_and_never_guesses() {
    let manifest = fixed_manifest();
    let trampoline = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Trampoline).unwrap().sha256;
    let observed = |own_image, live, euid, trampoline| Observed { own_image, live, euid, trampoline };
    let reports = components(&manifest, observed(Some(digest(42)), true, 0, Some(trampoline)));
    assert_eq!(reports.len(), manifest.artifacts.len() - 3);
    assert!(!reports.iter().any(|r| matches!(r.target, InstallTarget::Gateway | InstallTarget::Egress | InstallTarget::UpdateSlot)));
    let find = |reports: &[ComponentReadback], target: InstallTarget| reports.iter().find(|r| r.target == target).unwrap().clone();
    let own = find(&reports, InstallTarget::Spawnd);
    assert!(own.ready);
    assert_eq!((own.build_sha256, own.effective_uid, own.protocol), (digest(42), 0, duduclaw_isolation::ipc::PROTOCOL_VERSION));
    assert_eq!(own.version, duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    assert_eq!(own.artifact_id, "duduclaw-spawnd");
    let run = find(&reports, InstallTarget::Trampoline);
    assert!(run.ready);
    assert_eq!((run.build_sha256, run.version, run.effective_uid), (trampoline, manifest.version, 0));
    for target in [InstallTarget::ParseWorker, InstallTarget::McpBridge, InstallTarget::Importer, InstallTarget::Migration,
        InstallTarget::EgressForwarder, InstallTarget::RuntimeEntry { registry_id: "api-worker-v1".into() }] {
        let report = find(&reports, target);
        assert!(!report.ready);
        assert_eq!((report.build_sha256, report.effective_uid), (Sha256Digest([0; 32]), u32::MAX));
    }
    // Each missing or wrong observation makes the record not ready.
    assert!(!find(&components(&manifest, observed(Some(digest(42)), false, 0, Some(trampoline))), InstallTarget::Spawnd).ready);
    let unread = find(&components(&manifest, observed(None, true, 0, Some(trampoline))), InstallTarget::Spawnd);
    assert!(!unread.ready && unread.build_sha256 == Sha256Digest([0; 32]));
    let unprivileged = components(&manifest, observed(Some(digest(42)), true, 1000, Some(trampoline)));
    assert!(!find(&unprivileged, InstallTarget::Spawnd).ready && !find(&unprivileged, InstallTarget::Trampoline).ready);
    let replaced = find(&components(&manifest, observed(Some(digest(42)), true, 0, Some(digest(9)))), InstallTarget::Trampoline);
    assert!(!replaced.ready);
    assert_eq!((replaced.build_sha256, replaced.version), (digest(9), ReleaseVersion { major: 0, minor: 0, patch: 0 }));
    assert!(!find(&components(&manifest, observed(Some(digest(42)), true, 0, None)), InstallTarget::Trampoline).ready);
}

#[test]
fn update_publication_matches_the_installed_layout() {
    let published: Vec<String> = publication().iter()
        .map(|(target, directory)| format!("/{directory}/{}", artifact_name(target).unwrap())).collect();
    assert_eq!(published, [
        "/usr/lib/duduclaw/duduclaw-spawnd", "/usr/lib/duduclaw/duduclaw-run", "/usr/libexec/duduclaw/duduclaw-parse-worker",
        "/usr/lib/duduclaw/current/duduclaw-import", "/usr/lib/duduclaw/current/duduclaw-migrate",
        "/usr/lib/duduclaw/current/duduclaw-mcp-bridge", "/opt/duduclaw/bin/duduclaw-egress", "/opt/duduclaw/bin/duduclaw"]);
}

#[test]
fn update_preparation_must_be_the_manifest_preparation_without_migrations() {
    let mut manifest = fixed_manifest();
    let exact = Preparation::for_manifest(&manifest, digest(5));
    bound(&exact, &manifest).unwrap();
    let mut missing_entry = exact.clone(); missing_entry.runtime_entries.clear();
    assert!(bound(&missing_entry, &manifest).is_err());
    manifest.migrations.push(duduclaw_isolation::update::DataMigration { id: "m1".into(), sha256: digest(6), reversible: false });
    assert!(bound(&Preparation::for_manifest(&manifest, digest(5)), &manifest).is_err());
    assert!(runtime_entries_shape(&exact).is_ok());
    let mut not_runtime = exact; not_runtime.runtime_entries[0].install_target = InstallTarget::Gateway;
    assert!(runtime_entries_shape(&not_runtime).is_err());
}

// ---------------------------------------------------------------- root (native Linux)

/// An isolated root-owned tree under /run: install root, publication root and
/// spawnd state, all satisfying `Directory::open`'s ancestor rules.
struct RootTree { _temp: tempfile::TempDir, install: PathBuf, publish: PathBuf, state: PathBuf }
impl RootTree {
    fn new() -> Self {
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
        let temp = tempfile::Builder::new().prefix("duduclaw-update-control-").tempdir_in("/run").unwrap();
        let mkdir = |path: &Path| {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let install = temp.path().join("install");
        let publish = temp.path().join("publish");
        let state = temp.path().join("state");
        mkdir(&install.join("generations"));
        for (_, directory) in publication() {
            let mut path = publish.clone();
            for part in directory.split('/') { path.push(part); mkdir(&path); }
        }
        mkdir(&state);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { _temp: temp, install, publish, state }
    }
    fn content(target: &InstallTarget, seed: u8) -> Vec<u8> {
        format!("#!/bin/false\n{} generation {seed}\n", artifact_name(target).unwrap()).into_bytes()
    }
    /// Stage a complete generation the way the updater leaves it.
    fn stage(&self, seed: u8) -> (Sha256Digest, ReleaseManifest) {
        let manifest = manifest_with(all_targets().into_iter().map(|target| {
            let bytes = Self::content(&target, seed);
            ReleaseArtifact { id: artifact_name(&target).unwrap(), sha256: Sha256Digest(Sha256::digest(&bytes).into()),
                size_bytes: bytes.len() as u64, mode: 0o755, install_target: target }
        }).collect());
        let mut manifest = manifest; manifest.serial = u64::from(seed);
        let bytes = duduclaw_updated::metadata::canonical_bytes(&manifest).unwrap();
        let generation = Sha256Digest(Sha256::digest(&bytes).into());
        let directory = self.install.join("generations").join(hex(&generation));
        std::fs::create_dir(&directory).unwrap();
        for artifact in &manifest.artifacts {
            let path = directory.join(artifact_name(&artifact.install_target).unwrap());
            std::fs::write(&path, Self::content(&artifact.install_target, seed)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = directory.join("manifest.json");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        (generation, manifest)
    }
    fn assert_published(&self, seed: u8) {
        for (target, directory) in publication() {
            let path = self.publish.join(directory).join(artifact_name(&target).unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), Self::content(&target, seed), "{}", path.display());
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.gid(), metadata.nlink()), (0o755, 0, 0, 1));
            assert!(std::fs::symlink_metadata(path.with_file_name(format!("{}.next", artifact_name(&target).unwrap()))).is_err());
        }
    }
    fn effects(&self) -> InstalledEffects { InstalledEffects::for_test(&self.install, &self.publish) }
    fn control(&self, barrier: &DrainBarrier) -> UpdateControl<DiskDrainStore, InstalledEffects> {
        self.control_with(barrier, self.effects())
    }
    /// A (re)start: the barrier is restored and the journal settled in `new`.
    fn control_with(&self, barrier: &DrainBarrier, effects: InstalledEffects) -> UpdateControl<DiskDrainStore, InstalledEffects> {
        UpdateControl::new(DiskDrainStore::at(&self.state), barrier.clone(), effects).unwrap()
    }
    /// The updater's selector swap (`current` -> `generations/<hex>`), as root.
    fn select(&self, generation: Sha256Digest) {
        let current = self.install.join("current");
        let _ = std::fs::remove_file(&current);
        std::os::unix::fs::symlink(format!("generations/{}", hex(&generation)), &current).unwrap();
    }
    fn journal(&self) -> Option<PublishJournal> { DiskDrainStore::at(&self.state).load_journal().unwrap() }
    /// Overwrite one staged artifact so the generation no longer verifies.
    fn corrupt(&self, generation: Sha256Digest) {
        std::fs::write(self.install.join("generations").join(hex(&generation)).join("duduclaw-run"), b"#!/bin/false\ntampered\n").unwrap();
    }
    /// Undo [`Self::corrupt`] (an operator restaging the artifact).
    fn restore(&self, generation: Sha256Digest, seed: u8) {
        std::fs::write(self.install.join("generations").join(hex(&generation)).join("duduclaw-run"),
            Self::content(&InstallTarget::Trampoline, seed)).unwrap();
    }
    /// The installer's fresh install (no journal): a staged generation, its
    /// eight components at the fixed destinations, and `current` selecting it.
    fn install(&self, seed: u8) -> (Sha256Digest, ReleaseManifest) {
        let (generation, manifest) = self.stage(seed);
        for (target, directory) in publication() {
            let path = self.publish.join(directory).join(artifact_name(&target).unwrap());
            std::fs::write(&path, Self::content(&target, seed)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        self.select(generation);
        (generation, manifest)
    }
    /// Alter one fixed destination in place (same owner, mode and link count).
    fn alter_destination(&self) {
        std::fs::write(self.publish.join("opt/duduclaw/bin/duduclaw"), b"#!/bin/false\naltered\n").unwrap();
    }
    /// A start whose effects see `revoked` as revoked by the trust ledger.
    fn control_revoking(&self, barrier: &DrainBarrier, revoked: &[Sha256Digest]) -> UpdateControl<DiskDrainStore, InstalledEffects> {
        let mut effects = self.effects();
        effects.revoked = revoked.to_vec();
        self.control_with(barrier, effects)
    }
}

/// The repairing `Apply` over a socket pair: replied first, then the restart path.
fn repair_over_socket(control: &mut UpdateControl<DiskDrainStore, InstalledEffects>, preparation: Preparation) {
    let (client, server) = pair();
    let apply = request(ControlCommand::Apply { preparation });
    transport::send(&client, &apply).unwrap();
    assert_eq!(serve_and_settle(server, me(), control), After::RestartAfterRepair);
    let response: ControlResponse = transport::receive(&client).unwrap();
    validate_response(&apply, &response).unwrap();
}

/// One activation up to the Resume. The process that applied is replaced by a
/// restarted one (the updater's restart) before the Resume: the returned
/// barrier is the restarted process's, which `control` now is.
fn activate(tree: &RootTree, control: &mut UpdateControl<DiskDrainStore, InstalledEffects>, manifest: &ReleaseManifest,
    generation: Sha256Digest, seed: u8, previous: Option<Sha256Digest>) -> DrainBarrier {
    let preparation = Preparation::for_manifest(manifest, generation);
    accepted(control, ControlCommand::Prepare { preparation: preparation.clone() });
    accepted(control, ControlCommand::Drain { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), Some(generation));
    // The production gate check (PID 1 over the system bus) found the gate,
    // egress (socket and service) and the gateway down on this host.
    accepted(control, ControlCommand::Apply { preparation: preparation.clone() });
    // An accepted publication: this process no longer says `consistent`.
    assert_eq!(control.report(), "recovering");
    tree.assert_published(seed);
    let published = PublishJournal::new(generation, generation, previous, PublishState::Published).unwrap();
    assert_eq!(tree.journal(), Some(published));
    let metadata = std::fs::symlink_metadata(tree.state.join(JOURNAL_FILE)).unwrap();
    assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.nlink()), (0o600, 0, 1));
    // A retried Apply (lost reply) is idempotent and keeps the journal's previous.
    accepted(control, ControlCommand::Apply { preparation });
    tree.assert_published(seed);
    assert_eq!(tree.journal(), Some(published));
    tree.select(generation);
    let request = request(ControlCommand::Readback { generation });
    let response = control.respond(&request);
    validate_response(&request, &response).unwrap();
    let ControlOutcome::Readback { components } = response.outcome else { panic!("no readback") };
    assert_eq!(components.len(), manifest.artifacts.len() - 3);
    let own = components.iter().find(|r| r.target == InstallTarget::Spawnd).unwrap();
    assert!(own.ready && own.effective_uid == 0 && Some(own.build_sha256) == running_image().ok());
    let run = components.iter().find(|r| r.target == InstallTarget::Trampoline).unwrap();
    let expected = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Trampoline).unwrap();
    assert!(run.ready && run.build_sha256 == expected.sha256);
    // Not by the process that applied: Refused, nothing changed on disk.
    refused_outcome(control, ControlCommand::Resume { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), Some(generation));
    assert_eq!(tree.journal(), Some(published));
    // The restart: a new process re-verifies the `published` journal and opens.
    let barrier = DrainBarrier::default();
    *control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(generation)));
    assert!(control.opened());
    accepted(control, ControlCommand::Resume { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), None);
    // Removed only by the Resume that follows the commit.
    assert_eq!(tree.journal(), None);
    barrier
}

#[test]
#[ignore = "requires root native Linux: publishes into an isolated root-owned /run tree"]
fn iso_root_update_apply_end_to_end_two_generations() {
    let tree = RootTree::new();
    // A fresh install (generation 1 published by the installer, `current`
    // selecting it, no journal) verifies at start and opens.
    let (first, manifest) = tree.install(1);
    let barrier = DrainBarrier::default();
    let admission = Observer::new(&barrier, 0);
    let mut control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::NoJournal);
    assert!(control.opened());
    // The updater's first preflight precedes staging; the directory does not exist yet.
    let absent = Sha256Digest([0xab; 32]);
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(absent) });
    let (second, next) = tree.stage(2);
    let barrier = activate(&tree, &mut control, &next, second, 2, Some(first));
    // The restarted process's admission loop from here on.
    drop(admission);
    let admission = Observer::new(&barrier, 0);
    // A stale or foreign generation can never be applied once admission reopened.
    assert!(matches!(outcome(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&manifest, first) }),
        ControlOutcome::Refused));
    tree.assert_published(2);
    // A tampered staged artifact is caught by Prepare and by Apply's copy.
    let (third, last) = tree.stage(3);
    let staged = tree.install.join("generations").join(hex(&third)).join("duduclaw-run");
    std::fs::write(&staged, b"#!/bin/false\ntampered\n").unwrap();
    assert!(matches!(outcome(&mut control, ControlCommand::Prepare { preparation: Preparation::for_manifest(&last, third) }),
        ControlOutcome::Refused));
    accepted(&mut control, ControlCommand::Drain { generation: third });
    assert!(matches!(outcome(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&last, third) }),
        ControlOutcome::Refused));
    // Refused during verification, before the journal: nothing to recover.
    assert_eq!(tree.journal(), None);
    tree.assert_published(2);
    accepted(&mut control, ControlCommand::Resume { generation: third });

    // Published-journal re-verification: Apply(4) completed (`published`),
    // power lost before Resume, then the signer of 4 is revoked. The next start
    // re-verifies against the ledger and rolls back to the still-trusted 2.
    let (fourth, fourth_manifest) = tree.stage(4);
    accepted(&mut control, ControlCommand::Drain { generation: fourth });
    accepted(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&fourth_manifest, fourth) });
    tree.assert_published(4);
    drop(admission);
    drop(control);
    let barrier = DrainBarrier::default();
    let control = tree.control_revoking(&barrier, &[fourth]);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledBack);
    tree.assert_published(2);
    assert_eq!(tree.journal(), Some(PublishJournal::new(second, second, Some(fourth), PublishState::Published).unwrap()));
    drop(control);
    // A destination altered under the `published` journal is repaired at the next start.
    tree.alter_destination();
    let barrier = DrainBarrier::default();
    let admission = Observer::new(&barrier, 0);
    let mut control = tree.control_revoking(&barrier, &[fourth]);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    tree.assert_published(2);
    accepted(&mut control, ControlCommand::Drain { generation: second });
    accepted(&mut control, ControlCommand::Resume { generation: second });
    assert_eq!(tree.journal(), None);
    drop(admission);
    drop(control);

    // Restart after repair: no journal, a destination altered. The start finds
    // the selected generation unverified and opens nothing but this endpoint;
    // Drain needs no admission loop; the repairing Apply replies, then takes
    // the restart path; the restarted process verifies and opens.
    tree.alter_destination();
    let barrier = DrainBarrier::default();
    let mut control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::SelectedUnverified);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert!(!control.opened());
    accepted(&mut control, ControlCommand::Drain { generation: second });
    repair_over_socket(&mut control, Preparation::for_manifest(&next, second));
    tree.assert_published(2);
    assert_eq!(control.report(), "recovering");
    drop(control);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert!(control.opened());
    accepted(&mut control, ControlCommand::Resume { generation: second });
    assert_eq!(tree.journal(), None);
    assert!(!barrier.is_closed());
}

#[test]
#[ignore = "requires root native Linux: durable drain record in an isolated root-owned /run tree"]
fn iso_root_update_drain_waits_for_active_run() {
    let tree = RootTree::new();
    drain_waits_for_active_run(DiskStoreHandle(tree.state.clone()));
}
/// Cloneable handle over the same on-disk record (each call reopens it).
#[derive(Clone)]
struct DiskStoreHandle(PathBuf);
impl DrainStore for DiskStoreHandle {
    fn load(&mut self) -> io::Result<Option<Sha256Digest>> { DiskDrainStore::at(&self.0).load() }
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()> { DiskDrainStore::at(&self.0).persist(generation) }
    fn clear(&mut self) -> io::Result<()> { DiskDrainStore::at(&self.0).clear() }
}
impl JournalStore for DiskStoreHandle {
    fn load_journal(&mut self) -> io::Result<Option<PublishJournal>> { DiskDrainStore::at(&self.0).load_journal() }
    fn persist_journal(&mut self, journal: &PublishJournal) -> io::Result<()> {
        DiskDrainStore::at(&self.0).persist_journal(journal)
    }
    fn clear_journal(&mut self) -> io::Result<()> { DiskDrainStore::at(&self.0).clear_journal() }
}
impl RootTree {
    /// How many fixed destinations hold generation `seed`'s bytes.
    fn count_published(&self, seed: u8) -> usize {
        publication().into_iter().filter(|(target, directory)| {
            std::fs::read(self.publish.join(directory).join(artifact_name(target).unwrap())).unwrap() == Self::content(target, seed)
        }).count()
    }
    /// One `Drain` + `Apply` of `generation` that loses power after `renames` renames.
    fn interrupted_apply(&self, manifest: &ReleaseManifest, generation: Sha256Digest, renames: usize) {
        let barrier = DrainBarrier::default();
        let _admission = Observer::new(&barrier, 0);
        let mut effects = self.effects();
        effects.power_loss_after = Some(renames);
        let mut control = self.control_with(&barrier, effects);
        accepted(&mut control, ControlCommand::Drain { generation });
        refused_outcome(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(manifest, generation) });
    }
}

#[test]
#[ignore = "requires root native Linux: simulated power loss in an isolated root-owned /run tree"]
fn iso_root_update_power_loss_each_phase() {
    let tree = RootTree::new();
    // Every start verifies the installed set: begin from a fresh install of 1.
    let (first, manifest) = tree.install(1);
    let record = tree.state.join(DRAIN_RECORD);
    let restart = || { let barrier = DrainBarrier::default(); let control = tree.control(&barrier); (barrier, control) };
    // 1. Loss before the record's rename: only a stale temporary exists. No
    //    record means no barrier, and the next Drain discards the temporary.
    std::fs::write(tree.state.join("drain.json.next"), b"{\"version\":1,\"gen").unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), None);
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    assert!(std::fs::symlink_metadata(tree.state.join("drain.json.next")).is_err());
    let metadata = std::fs::symlink_metadata(&record).unwrap();
    assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.nlink()), (0o600, 0, 1));
    drop(_admission);
    drop(control);
    // 2. Loss after the record landed, before the reply: closed after restart.
    let (barrier, _control) = restart();
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // 3. A torn or tampered record: closed with no generation; only Drain re-keys.
    std::fs::write(&record, b"{\"version\":1,\"generation\":[7,7").unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), Some(DrainClosure::Unreadable));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // 4. Loss after Resume's unlink, before the reply: open after restart, and
    //    the retried Resume is accepted.
    std::fs::remove_file(&record).unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), None);
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    drop(_admission);
    // 5. Loss in the middle of Apply after N of the 8 renames (N = 0: between
    //    the journal write and the first rename), a torn `.next` left behind.
    //    The restart (before any socket or admission) finds `publishing`,
    //    re-verifies the generation and completes it; the journal says `published`.
    let (second, next) = tree.stage(2);
    let publishing = PublishJournal::new(second, second, Some(first), PublishState::Publishing).unwrap();
    for renames in 0..=7 {
        tree.assert_published(1);
        assert_eq!(tree.journal(), None);
        tree.interrupted_apply(&next, second, renames);
        assert_eq!(tree.count_published(2), renames, "after {renames} renames");
        assert_eq!(tree.count_published(1), 8 - renames, "after {renames} renames");
        assert_eq!(tree.journal(), Some(publishing));
        let (barrier, control) = restart();
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward, "after {renames} renames");
        tree.assert_published(2);
        assert_eq!(tree.journal(), Some(PublishJournal { state: PublishState::Published, ..publishing }));
        assert_eq!(barrier.closure(), Some(DrainClosure::Generation(second)));
        drop(control);
        // Power lost again (after `published`, before Resume) and one component
        // torn meanwhile: every start re-verifies the `published` journal and
        // repairs the destination with the idempotent publication.
        tree.alter_destination();
        let (barrier, mut control) = restart();
        assert_eq!(control.startup().recovery, PublicationRecovery::Reverified, "after {renames} renames");
        assert!(control.consistent_at_start());
        tree.assert_published(2);
        assert_eq!(tree.journal(), Some(PublishJournal { state: PublishState::Published, ..publishing }));
        // Back to generation 1 for the next round: the updater's roll-back path
        // (Apply of the installed-before generation, then its Resume).
        let _admission = Observer::new(&barrier, 0);
        accepted(&mut control, ControlCommand::Drain { generation: first });
        accepted(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&manifest, first) });
        assert_eq!(tree.journal(), Some(PublishJournal::new(first, first, Some(second), PublishState::Published).unwrap()));
        // Resumed only by the restarted process, never by the one that applied.
        refused_outcome(&mut control, ControlCommand::Resume { generation: first });
        assert!(barrier.is_closed());
        drop(control);
        let (_, mut control) = restart();
        accepted(&mut control, ControlCommand::Resume { generation: first });
    }
    // 6. Partial publication, then the new generation's directory is corrupted:
    //    the restart rolls back to the journal's previous generation with the
    //    same procedure, and all eight destinations hold its bytes.
    tree.interrupted_apply(&next, second, 5);
    assert_eq!(tree.count_published(2), 5);
    tree.corrupt(second);
    {
        let barrier = DrainBarrier::default();
        let mut control = tree.control(&barrier);
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledBack);
        tree.assert_published(1);
        assert_eq!(tree.journal(), Some(PublishJournal::new(first, first, Some(second), PublishState::Published).unwrap()));
        let _admission = Observer::new(&barrier, 0);
        accepted(&mut control, ControlCommand::Drain { generation: first });
        accepted(&mut control, ControlCommand::Resume { generation: first });
        assert_eq!(tree.journal(), None);
    }
    // 7. Neither the generation nor its previous can be completed: not ready
    //    (admission closed by `Recovery`), the mixed set is left as found, and
    //    the root-only status file says so.
    let (third, last) = tree.stage(3);
    tree.interrupted_apply(&last, third, 2);
    tree.corrupt(third);
    tree.corrupt(first);
    let barrier = DrainBarrier::default();
    let mut control = tree.control(&barrier).with_status_root(&tree.state);
    let alive = own(&mut control);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(tree.count_published(3), 2);
    refused_outcome(&mut control, ControlCommand::Resume { generation: third });
    assert_eq!(control.report(), "inconsistent: publication_incomplete");
    let path = tree.state.join(STATUS_FILE);
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.nlink()), (0o600, 0, 1));
    let bytes = std::fs::read(&path).unwrap();
    let mut status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let owner: Owner = serde_json::from_value(status.as_object_mut().unwrap().remove("owner").unwrap()).unwrap();
    assert_eq!((owner.pid, owner.invocation_id.as_str()), (alive.pid, alive.invocation_id.as_str()));
    assert_eq!(status, serde_json::json!({
        "schema": 3, "set": "inconsistent", "closed_reason": "publication_incomplete",
        "admission_closed": true, "barrier": "recovery_incomplete", "barrier_generation": null,
        "journal": "publishing", "journal_generation": hex(&third), "journal_previous": hex(&first),
        "publication_consistent": false, "startup_barrier": "drained", "startup_barrier_generation": hex(&third),
        "startup_recovery": "incomplete" }));
    let boot = boot_id().unwrap();
    assert!(!status_accepted(Some(&bytes), &boot, Some(&alive)));
    // 8. Restart after repair: an operator restages the generation; Drain needs
    //    no admission loop (an idle census of this host); the repairing Apply
    //    replies, then takes the restart path; the restarted process verifies
    //    everything, opens, and only then writes the `consistent` status the
    //    waiter accepts.
    tree.restore(third, 3);
    accepted(&mut control, ControlCommand::Drain { generation: third });
    repair_over_socket(&mut control, Preparation::for_manifest(&last, third));
    tree.assert_published(3);
    assert_eq!(control.report(), "recovering");
    assert!(!status_accepted(Some(&std::fs::read(&path).unwrap()), &boot, Some(&alive)));
    drop(control);
    let barrier = DrainBarrier::default();
    let mut control = tree.control(&barrier).with_status_root(&tree.state);
    // A new service start has a new invocation id: the old status never matches it.
    assert!(!status_accepted(Some(&std::fs::read(&path).unwrap()), &boot,
        Some(&LiveSpawnd { pid: alive.pid, invocation_id: "f".repeat(32) })));
    let alive = own(&mut control);
    assert_eq!(control.startup().recovery, PublicationRecovery::Reverified);
    assert_eq!(control.report(), "recovering");
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    assert!(status_accepted(Some(&std::fs::read(&path).unwrap()), &boot, Some(&alive)));
    // An unreadable journal is never ignored either.
    drop(control);
    std::fs::write(tree.state.join(JOURNAL_FILE), b"{\"schema\":2}").unwrap();
    let barrier = DrainBarrier::default();
    let control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::JournalUnreadable);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
}

#[test]
#[ignore = "requires root native Linux with the system bus: open-mode Apply failure in an isolated root-owned /run tree"]
fn iso_root_update_open_apply_failure_exits_and_restart_completes() {
    let tree = RootTree::new();
    let (first, _) = tree.install(1);
    let (second, next) = tree.stage(2);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut effects = tree.effects();
    effects.power_loss_after = Some(4);
    let mut control = tree.control_with(&barrier, effects).with_status_root(&tree.state);
    let alive = own(&mut control);
    assert!(control.opened());
    assert_eq!(control.report(), "consistent");
    let (boot, path) = (boot_id().unwrap(), tree.state.join(STATUS_FILE));
    assert!(status_accepted(Some(&std::fs::read(&path).unwrap()), &boot, Some(&alive)));
    // The production census of this host (PID 1, cgroupfs) finds nothing running.
    accepted(&mut control, ControlCommand::Drain { generation: second });
    let (client, server) = pair();
    transport::send(&client, &request(ControlCommand::Apply { preparation: Preparation::for_manifest(&next, second) })).unwrap();
    assert_eq!(serve_and_settle(server, me(), &mut control), After::ExitInconsistent);
    assert!(matches!(transport::receive::<ControlResponse>(&client).unwrap().outcome, ControlOutcome::Refused));
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(control.report(), "inconsistent: publication_incomplete");
    // The gate's waiter no longer accepts this process's status file.
    assert!(!status_accepted(Some(&std::fs::read(&path).unwrap()), &boot, Some(&alive)));
    assert_eq!(tree.count_published(2), 4);
    drop(control);
    // The restart after EXIT_SET_INCONSISTENT completes the publication.
    let barrier = DrainBarrier::default();
    let control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward);
    tree.assert_published(2);
    assert_eq!(tree.journal(), Some(PublishJournal::new(second, second, Some(first), PublishState::Published).unwrap()));
}

/// Temporary units under `/run/systemd/system` whose relation lines are the
/// production files' own, renamed: a stand-in spawnd (exits with
/// [`EXIT_SET_INCONSISTENT`] when its flag file appears, `Restart=` as in
/// production), the gate, and a dependent carrying the gateway/egress lines.
/// PID 1 is driven over the system bus (no process is spawned).
struct GateUnits { prefix: String, connection: zbus::blocking::Connection }
impl GateUnits {
    const BUS: &'static str = "org.freedesktop.systemd1";
    fn name(&self, role: &str) -> String { format!("{}-{role}.service", self.prefix) }
    fn flag(&self) -> PathBuf { PathBuf::from(format!("/run/{}.fail", self.prefix)) }
    fn manager(&self) -> zbus::blocking::Proxy<'_> {
        zbus::blocking::Proxy::new(&self.connection, Self::BUS, "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager")
            .unwrap()
    }
    fn install() -> Self {
        use duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES;
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
        let units = Self { prefix: format!("duduclaw-gate-test-{}", std::process::id()),
            connection: zbus::blocking::Connection::system().unwrap() };
        let rename = |line: &str| line.replace("duduclaw-spawnd.service", &units.name("spawnd"))
            .replace("duduclaw-isolation-ready.service", &units.name("ready"));
        let relations = |unit: &str, keys: &[&str]| directives(unit).into_iter()
            .filter(|line| keys.iter().any(|key| line.starts_with(key))).map(|line| rename(line)).collect::<Vec<_>>().join("\n");
        let spawnd = format!("[Unit]\n{}\n[Service]\nType=simple\nExecStart=/bin/sh -c 'while [ ! -e {flag} ]; do sleep 0.2; done; \
            rm -f {flag}; exit {exit}'\n{}\nRestartSec=1\n",
            relations(SPAWND_UNIT, &["Wants=", "StartLimitIntervalSec=", "StartLimitBurst="]),
            relations(SPAWND_UNIT, &["Restart=", "RestartForceExitStatus=", "SuccessExitStatus="]),
            flag = units.flag().display(), exit = EXIT_SET_INCONSISTENT);
        let ready = format!("[Unit]\n{}\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/usr/bin/true\n",
            relations(READY_UNIT, &["BindsTo=", "PartOf=", "After=", "Requires="]));
        let dependent = format!("[Unit]\n{}[Service]\nExecStart=/usr/bin/sleep infinity\n", rename(ISOLATED_GATEWAY_UNIT_DEPENDENCIES));
        for (role, content) in [("spawnd", spawnd), ("ready", ready), ("egress", dependent)] {
            std::fs::write(Path::new("/run/systemd/system").join(units.name(role)), content).unwrap();
        }
        let _: () = units.manager().call("Reload", &()).unwrap();
        units
    }
    fn job(&self, method: &str, role: &str) {
        let _: zbus::zvariant::OwnedObjectPath = self.manager().call(method, &(self.name(role).as_str(), "replace")).unwrap();
    }
    /// `(ActiveState, InvocationID)` as PID 1 reports them now.
    fn state(&self, role: &str) -> (String, Vec<u8>) {
        let path: zbus::zvariant::OwnedObjectPath = self.manager().call("LoadUnit", &(self.name(role).as_str(),)).unwrap();
        let unit = zbus::blocking::Proxy::new(&self.connection, Self::BUS, path.as_str(), "org.freedesktop.systemd1.Unit").unwrap();
        (unit.get_property("ActiveState").unwrap(), unit.get_property("InvocationID").unwrap())
    }
    fn until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !condition(self) {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// Every role active under a new invocation (compared with `before`).
    fn rearmed(&self, roles: &[&str], before: &[Vec<u8>]) -> bool {
        roles.iter().zip(before).all(|(role, old)| { let (state, id) = self.state(role); state == "active" && id != *old })
    }
}
impl Drop for GateUnits {
    fn drop(&mut self) {
        for role in ["egress", "ready", "spawnd"] {
            let _: zbus::Result<zbus::zvariant::OwnedObjectPath> = self.manager().call("StopUnit", &(self.name(role).as_str(), "replace"));
            let _ = std::fs::remove_file(Path::new("/run/systemd/system").join(self.name(role)));
        }
        let _ = std::fs::remove_file(self.flag());
        let _: zbus::Result<()> = self.manager().call("Reload", &());
    }
}

#[test]
#[ignore = "iso_root_: root systemd >= 249 VM; temporary units under /run/systemd/system with the production gate relations"]
fn iso_root_update_gate_drops_and_rearms_with_spawnd() {
    let units = GateUnits::install();
    let all: &[&str] = &["spawnd", "ready", "egress"];
    let gate: &[&str] = &["spawnd", "ready"];
    let invocations = |roles: &[&str]| roles.iter().map(|role| units.state(role).1).collect::<Vec<_>>();
    // Starting a dependent pulls in the gate and spawnd (BindsTo=), in order.
    units.job("StartUnit", "egress");
    units.until("everything active", |units| all.iter().all(|role| units.state(role).0 == "active"));
    // 1. An explicit restart of spawnd restarts the gate (PartOf=/BindsTo=) and,
    //    through it, the dependent: the gate is re-armed by the new process.
    let before = invocations(all);
    units.job("RestartUnit", "spawnd");
    units.until("gate and dependent restarted with spawnd", |units| units.rearmed(all, &before));
    // 2. The open-mode failure exit (83): the gate drops and takes the
    //    dependent down; Restart= starts a new spawnd, which pulls the gate in
    //    again (Wants=). The dependent stays down until it is started again.
    let before = invocations(gate);
    std::fs::write(units.flag(), b"").unwrap();
    units.until("dependent stopped with the gate", |units| units.state("egress").0 != "active");
    units.until("gate re-armed by the restarted spawnd", |units| units.rearmed(gate, &before));
    assert_ne!(units.state("egress").0, "active");
    units.job("StartUnit", "egress");
    units.until("dependent back after the gate", |units| units.state("egress").0 == "active");
    // 3. The updater's step before Apply: stopping the gate alone stops the
    //    dependent (BindsTo=) while spawnd keeps running; spawnd's precondition
    //    (PID 1's filtered list, the production `units_down`) sees them up
    //    before and down after, and a unit PID 1 never loaded as down.
    let systemd = crate::systemd::linux::Systemd::connect().unwrap();
    let (ready, egress) = (units.name("ready"), units.name("egress"));
    let gated = [ready.as_str(), egress.as_str()];
    assert!(!systemd.units_down(&gated).unwrap());
    let spawnd = units.state("spawnd");
    units.job("StopUnit", "ready");
    units.until("gate and dependent stopped, spawnd running",
        |units| ["ready", "egress"].iter().all(|role| units.state(role).0 == "inactive"));
    assert_eq!(units.state("spawnd"), spawnd);
    units.until("no job left on the gated units", |_| systemd.units_down(&gated).unwrap());
    let never = format!("{}-never-loaded.service", units.prefix);
    assert!(systemd.units_down(&[never.as_str()]).unwrap());
    // Only a new spawnd invocation brings the gate back (Wants=).
    let before = invocations(gate);
    units.job("RestartUnit", "spawnd");
    units.until("gate re-armed by the restarted spawnd", |units| units.rearmed(gate, &before));
    // 4. Stopping spawnd stops the gate and every dependent.
    units.job("StopUnit", "spawnd");
    units.until("gate and dependent stopped with spawnd",
        |units| ["ready", "egress"].iter().all(|role| units.state(role).0 == "inactive"));
}
