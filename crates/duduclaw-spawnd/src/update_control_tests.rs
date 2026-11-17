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
    /// Generation directories that pass full verification.
    staged: Vec<Sha256Digest>,
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

#[derive(Default)]
struct FakeEffects { calls: Vec<&'static str>, fail: bool, world: Option<SharedWorld> }
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
            Some(world) if !world.lock().unwrap().staged.contains(&generation) => Err(invalid()),
            // The manifest digest is the generation digest.
            _ => Ok(generation),
        }
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
        for index in 0..8 {
            if world.slots[index] == Some(generation) { continue; }
            let pending = world.power_loss_after;
            match pending {
                Some(0) => { world.power_loss_after = None; return Err(io::Error::other("simulated power loss")); }
                Some(remaining) => world.power_loss_after = Some(remaining - 1),
                None => {}
            }
            world.slots[index] = Some(generation);
        }
        Ok(())
    }
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>> {
        Ok(self.world.as_ref().and_then(|world| world.lock().unwrap().selector))
    }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> {
        self.calls.push("readback");
        Ok(vec![])
    }
}
/// No generation effects (drain-only root tests).
struct NoEffects;
impl Effects for NoEffects {
    fn prepare(&mut self, _: &Preparation) -> io::Result<()> { Err(invalid()) }
    fn verify(&mut self, _: Sha256Digest, _: Option<&Preparation>) -> io::Result<Sha256Digest> { Err(invalid()) }
    fn publish(&mut self, _: Sha256Digest) -> io::Result<()> { Err(invalid()) }
    fn installed(&mut self) -> io::Result<Option<Sha256Digest>> { Err(invalid()) }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> { Err(invalid()) }
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
    // No selector: the journal names no previous generation.
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(7), digest(7), None, PublishState::Published).unwrap()));
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.journal(), None);
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
}

// ---------------------------------------------------------------- publication journal

fn apply_command(generation: Sha256Digest) -> ControlCommand { ControlCommand::Apply { preparation: preparation(generation) } }
fn refused_outcome<S: DrainStore + JournalStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) {
    assert!(matches!(outcome(control, command), ControlOutcome::Refused));
}
fn slots(world: &SharedWorld) -> [Option<Sha256Digest>; 8] { world.lock().unwrap().slots }
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
        // A second restart finds nothing to complete.
        let (_, again) = restart(&store, &shared);
        assert_eq!(again.startup().recovery, PublicationRecovery::AlreadyPublished);
        assert_eq!(again.effects.calls, Vec::<&str>::new());
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
    accepted(&mut control, apply_command(digest(2)));
    assert_eq!(slots(&shared), [Some(digest(2)); 8]);
    assert_eq!(serde_json::to_value(control.status()).unwrap()["publication_consistent"], true);
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

#[test]
fn update_apply_refused_while_another_generation_is_publishing() {
    let store = MemoryStore::default();
    let shared = new_world(&store, digest(1), &[digest(1), digest(2), digest(3)]);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::over(&shared)).unwrap();
    // A late failure in this process leaves `publishing` for 2 (4 of 8 replaced).
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    shared.lock().unwrap().power_loss_after = Some(4);
    refused_outcome(&mut control, apply_command(digest(2)));
    assert_eq!(store.journal().map(|journal| journal.state), Some(PublishState::Publishing));
    control.effects.calls.clear();
    accepted(&mut control, ControlCommand::Drain { generation: digest(3) });
    refused_outcome(&mut control, apply_command(digest(3)));
    assert!(control.effects.calls.is_empty());
    assert_eq!(store.journal().map(|journal| journal.generation), Some(digest(2)));
    // Rolling back to the journal's previous generation is allowed (it finishes the journal).
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    accepted(&mut control, apply_command(digest(1)));
    assert_eq!(slots(&shared), [Some(digest(1)); 8]);
    assert_eq!(store.journal(), Some(PublishJournal::new(digest(1), digest(1), Some(digest(2)), PublishState::Published).unwrap()));
    // Once published, another generation may be applied; its previous is the installed one.
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
    assert_eq!(serde_json::to_value(control.status()).unwrap(), serde_json::json!({
        "schema": 1, "admission_closed": true, "barrier": "drained", "barrier_generation": hex7,
        "journal": "published", "journal_generation": hex7, "journal_previous": "06".repeat(32),
        "publication_consistent": true, "startup_barrier": "drained", "startup_barrier_generation": hex7,
        "startup_recovery": "already_published" }));
    let records = control.startup().audit_records(42);
    assert_eq!(records.len(), 2);
    assert_eq!((records[0].event, records[0].at_unix_ms, records[0].detail.clone()),
        (crate::audit::Event::UpdateBarrierRestored, 42, Some(Detail::UpdateBarrier { generation: Some(digest(7)) })));
    assert_eq!((records[1].event, records[1].detail.clone()), (crate::audit::Event::PublicationRecovery,
        Some(Detail::PublicationRecovery { action: PublicationRecovery::AlreadyPublished, generation: Some(digest(7)),
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
}

fn activate(tree: &RootTree, control: &mut UpdateControl<DiskDrainStore, InstalledEffects>, manifest: &ReleaseManifest,
    generation: Sha256Digest, seed: u8, previous: Option<Sha256Digest>) {
    let preparation = Preparation::for_manifest(manifest, generation);
    accepted(control, ControlCommand::Prepare { preparation: preparation.clone() });
    accepted(control, ControlCommand::Drain { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), Some(generation));
    accepted(control, ControlCommand::Apply { preparation: preparation.clone() });
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
    accepted(control, ControlCommand::Resume { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), None);
    // Removed only by the Resume that follows the commit.
    assert_eq!(tree.journal(), None);
}

#[test]
#[ignore = "requires root native Linux: publishes into an isolated root-owned /run tree"]
fn iso_root_update_apply_end_to_end_two_generations() {
    let tree = RootTree::new();
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = tree.control(&barrier);
    // The updater's first preflight precedes staging; the directory does not exist yet.
    let (first, manifest) = tree.stage(1);
    let absent = Sha256Digest([0xab; 32]);
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(absent) });
    activate(&tree, &mut control, &manifest, first, 1, None);
    let (second, next) = tree.stage(2);
    activate(&tree, &mut control, &next, second, 2, Some(first));
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
    let (first, manifest) = tree.stage(1);
    let (second, next) = tree.stage(2);
    {
        let barrier = DrainBarrier::default();
        let _admission = Observer::new(&barrier, 0);
        let mut control = tree.control(&barrier);
        activate(&tree, &mut control, &manifest, first, 1, None);
    }
    let publishing = PublishJournal::new(second, second, Some(first), PublishState::Publishing).unwrap();
    for renames in 0..=7 {
        tree.assert_published(1);
        assert_eq!(tree.journal(), None);
        tree.interrupted_apply(&next, second, renames);
        assert_eq!(tree.count_published(2), renames, "after {renames} renames");
        assert_eq!(tree.count_published(1), 8 - renames, "after {renames} renames");
        assert_eq!(tree.journal(), Some(publishing));
        let barrier = DrainBarrier::default();
        let mut control = tree.control(&barrier);
        assert_eq!(control.startup().recovery, PublicationRecovery::RolledForward, "after {renames} renames");
        tree.assert_published(2);
        assert_eq!(tree.journal(), Some(PublishJournal { state: PublishState::Published, ..publishing }));
        assert_eq!(barrier.closure(), Some(DrainClosure::Generation(second)));
        // Back to generation 1 for the next round: the updater's roll-back path
        // (Apply of the installed-before generation, then its Resume).
        let _admission = Observer::new(&barrier, 0);
        accepted(&mut control, ControlCommand::Drain { generation: first });
        accepted(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&manifest, first) });
        assert_eq!(tree.journal(), Some(PublishJournal::new(first, first, Some(second), PublishState::Published).unwrap()));
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
    let mut control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::Incomplete);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
    assert_eq!(tree.count_published(3), 2);
    refused_outcome(&mut control, ControlCommand::Resume { generation: third });
    write_status(&tree.state, &control.status()).unwrap();
    let path = tree.state.join(STATUS_FILE);
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.nlink()), (0o600, 0, 1));
    let status: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(status, serde_json::json!({
        "schema": 1, "admission_closed": true, "barrier": "recovery_incomplete", "barrier_generation": null,
        "journal": "publishing", "journal_generation": hex(&third), "journal_previous": hex(&first),
        "publication_consistent": false, "startup_barrier": "drained", "startup_barrier_generation": hex(&third),
        "startup_recovery": "incomplete" }));
    // An unreadable journal is never ignored either.
    drop(control);
    std::fs::write(tree.state.join(JOURNAL_FILE), b"{\"schema\":2}").unwrap();
    let barrier = DrainBarrier::default();
    let control = tree.control(&barrier);
    assert_eq!(control.startup().recovery, PublicationRecovery::JournalUnreadable);
    assert_eq!(barrier.closure(), Some(DrainClosure::Recovery));
}
