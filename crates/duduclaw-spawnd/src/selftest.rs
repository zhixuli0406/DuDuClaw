//! R7a: the trampoline readiness facts, measured by spawnd itself instead of
//! read from an installer receipt (design: install-evidence §8 R7a).
//!
//! - `executable_verified`: `/usr/lib/duduclaw/duduclaw-run` AND
//!   `/usr/lib/duduclaw/duduclaw-run-selftest` match the committed signed
//!   generation's artifacts (sha256, size, mode), read by descriptor, and the
//!   installed `duduclaw-isolation-selftest.service` is the shipped text.
//! - `landlock_self_test` / `namespace_self_test`: the cached outcome of the
//!   fixed unit `duduclaw-isolation-selftest.service`, started through PID 1.
//! - ABI pair: the live `duduclaw_run::linux::probe::probe()`.
//!
//! The outcome is cached under (boot id, committed generation, ABI pair, both
//! artifact identities, unit file digest). A pass is reused for its key; a key
//! change runs the unit again; a fail is reused for [`FAIL_RETRY_MS`] and only
//! then retried.
//!
//! Everything that can block or is expensive (the updater's generation probe,
//! `probe()`, hashing, the unit file read, the unit run) happens only on one
//! background worker ([`worker_loop`]), started by the first readiness refresh
//! (after start-up recovery). It publishes a [`Snapshot`]; the refresh path
//! ([`SelfTestInstallation::current`]) only reads that snapshot and wakes the
//! worker, and reports nothing verified for a missing snapshot or one older than
//! [`SNAPSHOT_FRESH_MS`]. The self-test is never a precondition of spawnd's
//! sockets or of the update service's readiness.
use std::{io, sync::{mpsc, Arc, Mutex}, time::Duration};
use duduclaw_isolation::ids::Sha256Digest;
use crate::bootstrap::{TrampolineReadiness, BOOTSTRAP_VERSION};

pub const TRAMPOLINE_DIRECTORY: &str = "/usr/lib/duduclaw";
/// A failed outcome is reused for this long before the unit runs again.
pub const FAIL_RETRY_MS: u64 = 60_000;
/// The unit's own `TimeoutStartSec=30` plus margin for PID 1 to record it.
pub const UNIT_DEADLINE_MS: u64 = 40_000;
/// The worker measures the key (and runs the unit when needed) on every wake
/// and at least this often while spawnd runs.
pub const WORKER_PERIOD_MS: u64 = 2_000;
/// A snapshot vouches for its facts until this long after its key was
/// measured; missing or older (or a clock reading before it) is "nothing
/// verified". Covers one worker period plus two key measurements (the
/// generation probe alone must finish within 1 s), so a live worker keeps it
/// fresh and a stuck one lets the facts go false within this bound.
pub const SNAPSHOT_FRESH_MS: u64 = 8_000;

fn unverified() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "isolation self-test evidence unavailable") }

/// A signed artifact the installed file was verified against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactKey { pub sha256: Sha256Digest, pub size: u64, pub mode: u16 }

/// Everything a cached outcome is valid for. Any component change re-runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    pub boot_id: String,
    pub generation: Sha256Digest,
    pub detected_abi: u32,
    pub selected_abi: u32,
    pub trampoline: ArtifactKey,
    pub selftest: ArtifactKey,
    /// SHA-256 of the installed unit file (byte-equal to the shipped text).
    pub unit: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Pass, Fail }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision { Reuse(Outcome), Run }

#[derive(Debug, Clone)]
struct Cached { key: CacheKey, outcome: Outcome, at_ms: u64 }

/// Measured facts. `Default` is "nothing verified".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Facts { pub executable_verified: bool, pub passed: bool, pub detected_abi: u32, pub selected_abi: u32 }
impl Facts {
    /// The key was computed, so both installed files matched their artifacts.
    fn verified(key: &CacheKey, passed: bool) -> Self {
        Self { executable_verified: true, passed, detected_abi: key.detected_abi, selected_abi: key.selected_abi }
    }
}

/// The readiness record `NativeTrampoline` checks (`selected == min(detected, 7)`
/// and every flag true, in `TrampolineReadiness::check`).
pub fn readiness(facts: &Facts) -> TrampolineReadiness {
    TrampolineReadiness { version: BOOTSTRAP_VERSION, executable_verified: facts.executable_verified,
        landlock_self_test: facts.executable_verified && facts.passed,
        namespace_self_test: facts.executable_verified && facts.passed,
        detected_landlock_abi: facts.detected_abi, selected_landlock_abi: facts.selected_abi }
}

/// The side effects `SelfTest::measure` needs (fakes in tests). Called only on
/// the worker thread.
pub trait Host {
    /// The live key; `Err` when either installed file does not match the
    /// committed generation, the unit file is not the shipped text, or the
    /// generation, boot id or ABI is unreadable.
    fn key(&mut self) -> io::Result<CacheKey>;
    /// Start the fixed unit once and wait for it. `Err` counts as a fail.
    fn run(&mut self) -> io::Result<Outcome>;
    /// Monotonic milliseconds (CLOCK_BOOTTIME in production).
    fn now_ms(&mut self) -> io::Result<u64>;
}

/// What the worker last measured: the facts for `key`, whose measurement began
/// at `computed_at_ms` (boottime).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot { pub key: CacheKey, pub facts: Facts, pub computed_at_ms: u64 }
impl Snapshot {
    /// The facts `snapshot` still vouches for at `now_ms`; nothing verified when
    /// it is missing, older than [`SNAPSHOT_FRESH_MS`], or from the future.
    pub fn facts_at(snapshot: Option<&Self>, now_ms: u64) -> Facts {
        match snapshot {
            Some(snapshot) if now_ms >= snapshot.computed_at_ms
                && now_ms - snapshot.computed_at_ms < SNAPSHOT_FRESH_MS => snapshot.facts,
            _ => Facts::default(),
        }
    }
}

/// The slot the worker publishes into and the refresh path reads. The lock is
/// held only to replace or clone the value, never across the key computation
/// or the unit run.
#[derive(Default)]
pub struct Published(Mutex<Option<Snapshot>>);
impl Published {
    pub fn store(&self, snapshot: Option<Snapshot>) {
        // A poisoned slot stays poisoned, and `load` then reports nothing.
        if let Ok(mut slot) = self.0.lock() { *slot = snapshot; }
    }
    pub fn load(&self) -> Option<Snapshot> { self.0.lock().ok().and_then(|slot| slot.clone()) }
}

#[derive(Default)]
pub struct SelfTest { cached: Option<Cached> }
impl SelfTest {
    pub fn decide(&self, key: &CacheKey, now_ms: u64) -> Decision {
        match &self.cached {
            Some(cached) if cached.key == *key => match cached.outcome {
                Outcome::Pass => Decision::Reuse(Outcome::Pass),
                // Never retried within the window, also if the clock reads backwards.
                Outcome::Fail if now_ms.saturating_sub(cached.at_ms) < FAIL_RETRY_MS => Decision::Reuse(Outcome::Fail),
                Outcome::Fail => Decision::Run,
            },
            _ => Decision::Run,
        }
    }
    /// The facts for `key` from the cache only; never runs anything.
    pub fn facts(&self, key: &CacheKey) -> Facts {
        let passed = self.cached.as_ref().is_some_and(|cached| cached.key == *key && cached.outcome == Outcome::Pass);
        Facts::verified(key, passed)
    }
    fn snapshot(&self, key: CacheKey, computed_at_ms: u64) -> Snapshot {
        Snapshot { facts: self.facts(&key), key, computed_at_ms }
    }
    /// Measure the live key; reuse the cached outcome for it, or run the unit
    /// once (`running` sees the not-yet-passed snapshot first) and cache its
    /// outcome (pass or fail). The outcome is cached only when the key re-read
    /// after the run succeeds AND equals the key it ran under; otherwise the
    /// cache is cleared (it may describe neither key, or an unknown one) and
    /// `Err` is returned, so the next pass runs again.
    pub fn measure(&mut self, host: &mut dyn Host, running: &mut dyn FnMut(Snapshot)) -> io::Result<Snapshot> {
        let measured_at = host.now_ms()?;
        let key = host.key()?;
        let now = host.now_ms()?;
        if self.decide(&key, now) != Decision::Run { return Ok(self.snapshot(key, measured_at)); }
        running(Snapshot { facts: Facts::verified(&key, false), key: key.clone(), computed_at_ms: measured_at });
        let outcome = host.run().unwrap_or(Outcome::Fail);
        let measured_at = host.now_ms();
        let after = host.key();
        let at_ms = host.now_ms();
        match (measured_at, after, at_ms) {
            (Ok(measured_at), Ok(after), Ok(at_ms)) if after == key => {
                self.cached = Some(Cached { key: key.clone(), outcome, at_ms });
                Ok(self.snapshot(key, measured_at))
            }
            _ => { self.cached = None; Err(unverified()) }
        }
    }
    /// [`Self::measure`] without publishing; the facts for the live key.
    pub fn ensure(&mut self, host: &mut dyn Host) -> io::Result<Facts> {
        self.measure(host, &mut |_: Snapshot| {}).map(|snapshot| snapshot.facts)
    }
    /// One worker pass: measure, then publish the snapshot, or nothing verified
    /// (`None`) when there is no key or the run's outcome was discarded.
    pub fn publish(&mut self, host: &mut dyn Host, published: &Published) {
        let snapshot = self.measure(host, &mut |running: Snapshot| published.store(Some(running)));
        published.store(snapshot.ok());
    }
}

/// The background worker: one pass now, then one per wake (requests that arrive
/// during a pass coalesce into it) and at least every [`WORKER_PERIOD_MS`],
/// until the installation (the sender) is dropped. Only this thread computes
/// the key or runs the unit, so a stuck updater IPC or unit run stalls nothing
/// but this loop; its snapshot then goes stale.
pub fn worker_loop(host: &mut dyn Host, published: &Published, wake: &mpsc::Receiver<()>) {
    let mut selftest = SelfTest::default();
    loop {
        selftest.publish(host, published);
        while wake.try_recv().is_ok() {}
        if let Err(mpsc::RecvTimeoutError::Disconnected) = wake.recv_timeout(Duration::from_millis(WORKER_PERIOD_MS)) {
            return;
        }
    }
}

/// Starts the worker for a published slot and its wake channel.
pub type StartWorker = Box<dyn FnMut(Arc<Published>, mpsc::Receiver<()>) + Send>;
/// Monotonic milliseconds for the refresh path (CLOCK_BOOTTIME in production).
pub type Clock = Box<dyn FnMut() -> io::Result<u64> + Send>;

/// `NativeTrampoline`'s installation evidence. The refresh path never blocks:
/// it reads the published snapshot, wakes the single worker when the snapshot
/// is missing or due for a refresh, and never computes the key itself.
pub struct SelfTestInstallation {
    published: Arc<Published>,
    wake: Option<mpsc::SyncSender<()>>,
    start_worker: StartWorker,
    clock: Clock,
}
impl SelfTestInstallation {
    pub fn new(start_worker: StartWorker, clock: Clock) -> Self {
        Self { published: Arc::new(Published::default()), wake: None, start_worker, clock }
    }
    /// The facts the latest fresh snapshot vouches for; nothing verified
    /// without one.
    pub fn current(&mut self) -> Facts {
        let snapshot = self.published.load();
        let Ok(now) = (self.clock)() else { self.request_pass(); return Facts::default() };
        let due = match &snapshot {
            Some(snapshot) => now < snapshot.computed_at_ms || now - snapshot.computed_at_ms >= WORKER_PERIOD_MS,
            None => true,
        };
        if due { self.request_pass(); }
        Snapshot::facts_at(snapshot.as_ref(), now)
    }
    /// Non-blocking: starts the worker once, then at most one queued wake. A
    /// worker that has gone away is started again on the next request.
    fn request_pass(&mut self) {
        if self.wake.is_none() {
            let (sender, receiver) = mpsc::sync_channel::<()>(1);
            (self.start_worker)(self.published.clone(), receiver);
            self.wake = Some(sender);
        }
        let disconnected = self.wake.as_ref().is_some_and(|sender|
            matches!(sender.try_send(()), Err(mpsc::TrySendError::Disconnected(()))));
        if disconnected { self.wake = None; }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{live_key, InstalledHost};

#[cfg(target_os = "linux")]
mod linux {
    use std::{io, os::unix::fs::MetadataExt, path::Path, time::Duration};
    use duduclaw_isolation::{ids::ISOLATION_SELFTEST_UNIT, update::{InstallTarget, ReleaseManifest}};
    use duduclaw_updated::linux::files::{verify_fd, Directory};
    use sha2::{Digest, Sha256};
    use super::*;

    fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "isolation self-test evidence unavailable") }

    /// The exact file PID 1 executes, verified by descriptor against the one
    /// signed artifact of `target` (moved from the receipt reader, so the
    /// trampoline check no longer depends on any receipt).
    fn verified_artifact(directory: &Directory, manifest: &ReleaseManifest, target: InstallTarget) -> io::Result<ArtifactKey> {
        let mut matching = manifest.artifacts.iter().filter(|a| a.install_target == target);
        let artifact = matching.next().ok_or_else(denied)?;
        if matching.next().is_some() { return Err(denied()); }
        let name = duduclaw_updated::publication::artifact_name(&target).map_err(|_| denied())?;
        let installed = directory.read(&name).map_err(|_| denied())?;
        if installed.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(denied()); }
        verify_fd(installed, artifact.size_bytes, artifact.sha256).map_err(|_| denied())?;
        Ok(ArtifactKey { sha256: artifact.sha256, size: artifact.size_bytes, mode: artifact.mode })
    }

    /// The installed unit file's digest; `Err` unless it is byte-for-byte the
    /// shipped unit text (then there is no key: the unit never runs).
    pub(crate) fn unit_digest(installed: &[u8]) -> io::Result<Sha256Digest> {
        if !crate::systemd::fixed_unit_file_valid(installed) { return Err(denied()); }
        Ok(Sha256Digest(Sha256::digest(installed).into()))
    }

    /// The live cache key; `Err` unless both installed binaries are the
    /// committed generation's and the installed unit file is the shipped one.
    /// Blocking (updater IPC, probe, hashing): called only by the worker.
    pub fn live_key() -> io::Result<CacheKey> {
        let (generation, manifest) = crate::installed::committed_generation()?;
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().to_owned();
        if boot_id.is_empty() { return Err(denied()); }
        let ready = duduclaw_run::linux::probe::probe().map_err(|_| denied())?;
        let directory = Directory::open(Path::new(TRAMPOLINE_DIRECTORY)).map_err(|_| denied())?;
        let trampoline = verified_artifact(&directory, &manifest, InstallTarget::Trampoline)?;
        let selftest = verified_artifact(&directory, &manifest, InstallTarget::TrampolineSelftest)?;
        let unit = unit_digest(&crate::systemd::linux::installed_fixed_unit(ISOLATION_SELFTEST_UNIT)?)?;
        Ok(CacheKey { boot_id, generation, detected_abi: ready.landlock_abi, selected_abi: ready.landlock_ruleset_abi,
            trampoline, selftest, unit })
    }

    /// Production side effects: the live key and the fixed unit through PID 1.
    #[derive(Default)]
    pub struct InstalledHost { systemd: Option<crate::systemd::linux::Systemd> }
    impl Host for InstalledHost {
        fn key(&mut self) -> io::Result<CacheKey> { live_key() }
        fn run(&mut self) -> io::Result<Outcome> {
            if self.systemd.is_none() { self.systemd = Some(crate::systemd::linux::Systemd::connect()?); }
            let systemd = self.systemd.as_ref().ok_or_else(denied)?;
            // Re-checks the unit file content and PID 1's effective properties
            // immediately before StartUnit.
            let result = systemd.run_fixed_unit(ISOLATION_SELFTEST_UNIT, Duration::from_millis(UNIT_DEADLINE_MS));
            // A bus error drops the connection; the next run reconnects.
            if result.is_err() { self.systemd = None; }
            Ok(if result?.passed() { Outcome::Pass } else { Outcome::Fail })
        }
        fn now_ms(&mut self) -> io::Result<u64> { crate::host_state::boottime_ms() }
    }

    impl Default for SelfTestInstallation {
        fn default() -> Self {
            Self::new(Box::new(|published: std::sync::Arc<Published>, wake: std::sync::mpsc::Receiver<()>| {
                std::thread::spawn(move || worker_loop(&mut InstalledHost::default(), &published, &wake));
            }), Box::new(crate::host_state::boottime_ms))
        }
    }
    impl crate::native_trampoline::Installation for SelfTestInstallation {
        /// Never blocks: the latest fresh snapshot, or nothing verified.
        fn readback(&mut self) -> crate::Result<TrampolineReadiness> { Ok(readiness(&self.current())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn artifact(seed: u8) -> ArtifactKey { ArtifactKey { sha256: Sha256Digest([seed; 32]), size: 1000, mode: 0o755 } }
    fn key() -> CacheKey {
        CacheKey { boot_id: "boot-a".into(), generation: Sha256Digest([1; 32]), detected_abi: 4, selected_abi: 4,
            trampoline: artifact(2), selftest: artifact(3), unit: Sha256Digest([5; 32]) }
    }
    /// A scripted host: the key it reports (`after` / `after_err` for the read
    /// right after a run), the installed unit file, the outcomes its runs
    /// return, a clock, and how often the key was computed.
    struct Fake {
        key: io::Result<CacheKey>, after: Option<CacheKey>, after_err: bool, just_ran: bool, unit_file: Vec<u8>,
        outcomes: Vec<io::Result<Outcome>>, runs: usize, keys: usize, now: u64,
    }
    impl Fake {
        fn new(outcomes: Vec<io::Result<Outcome>>) -> Self {
            Self { key: Ok(key()), after: None, after_err: false, just_ran: false,
                unit_file: crate::systemd::ISOLATION_SELFTEST_UNIT_TEXT.as_bytes().to_vec(),
                outcomes, runs: 0, keys: 0, now: 1_000 }
        }
    }
    impl Host for Fake {
        fn key(&mut self) -> io::Result<CacheKey> {
            self.keys += 1;
            // As `live_key`: a unit file other than the shipped text yields no key.
            if !crate::systemd::fixed_unit_file_valid(&self.unit_file) { return Err(io::ErrorKind::PermissionDenied.into()); }
            if std::mem::take(&mut self.just_ran) {
                if self.after_err { return Err(io::ErrorKind::TimedOut.into()); }
                if let Some(after) = &self.after { return Ok(after.clone()); }
            }
            match &self.key { Ok(key) => Ok(key.clone()), Err(_) => Err(io::ErrorKind::PermissionDenied.into()) }
        }
        fn run(&mut self) -> io::Result<Outcome> {
            self.runs += 1;
            self.just_ran = true;
            if self.outcomes.is_empty() { panic!("unexpected run") }
            self.outcomes.remove(0)
        }
        fn now_ms(&mut self) -> io::Result<u64> { Ok(self.now) }
    }

    #[test]
    fn selftest_cache_reuses_a_pass_for_the_same_key() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Pass)]);
        let facts = selftest.ensure(&mut host).unwrap();
        assert_eq!(facts, Facts { executable_verified: true, passed: true, detected_abi: 4, selected_abi: 4 });
        for _ in 0..5 {
            host.now += 3_600_000;
            assert!(selftest.ensure(&mut host).unwrap().passed);
        }
        assert_eq!(host.runs, 1);
        assert_eq!(selftest.decide(&key(), host.now), Decision::Reuse(Outcome::Pass));
    }

    #[test]
    fn selftest_cache_reruns_on_any_key_component_change() {
        let changes: [fn(&mut CacheKey); 9] = [
            |k| k.boot_id = "boot-b".into(),
            |k| k.generation = Sha256Digest([9; 32]),
            |k| k.detected_abi = 5,
            |k| k.selected_abi = 3,
            |k| k.trampoline.sha256 = Sha256Digest([8; 32]),
            |k| k.trampoline.size += 1,
            |k| k.selftest.sha256 = Sha256Digest([7; 32]),
            |k| k.selftest.mode = 0o750,
            |k| k.unit = Sha256Digest([6; 32]),
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut selftest = SelfTest::default();
            let mut host = Fake::new(vec![Ok(Outcome::Pass), Ok(Outcome::Pass)]);
            selftest.ensure(&mut host).unwrap();
            let mut changed = key();
            change(&mut changed);
            assert_eq!(selftest.decide(&changed, host.now), Decision::Run, "case {index}");
            // The old pass never covers the new key, before or after it ran.
            assert!(!selftest.facts(&changed).passed, "case {index}");
            host.key = Ok(changed.clone());
            assert!(selftest.ensure(&mut host).unwrap().passed, "case {index}");
            assert_eq!(host.runs, 2, "case {index}");
        }
    }

    #[test]
    fn selftest_cache_retries_a_fail_only_after_sixty_seconds() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Fail), Err(io::ErrorKind::TimedOut.into()), Ok(Outcome::Pass)]);
        assert!(!selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(host.runs, 1);
        // Within the window: reused, never re-run.
        for elapsed in [0, 1, 30_000, FAIL_RETRY_MS - 1] {
            host.now = 1_000 + elapsed;
            assert_eq!(selftest.decide(&key(), host.now), Decision::Reuse(Outcome::Fail));
            assert!(!selftest.ensure(&mut host).unwrap().passed);
        }
        assert_eq!(host.runs, 1);
        // A clock reading before the failure never opens the window.
        assert_eq!(selftest.decide(&key(), 0), Decision::Reuse(Outcome::Fail));
        // After the window: one retry; a run error is cached as a fail too.
        host.now = 1_000 + FAIL_RETRY_MS;
        assert!(!selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(host.runs, 2);
        assert!(!selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(host.runs, 2);
        host.now += FAIL_RETRY_MS;
        assert!(selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(host.runs, 3);
    }

    #[test]
    fn selftest_unverified_executables_never_run_or_pass() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![]);
        host.key = Err(io::ErrorKind::PermissionDenied.into());
        assert!(selftest.ensure(&mut host).is_err());
        assert_eq!(host.runs, 0);
        let evidence = readiness(&Facts::default());
        assert!(!evidence.executable_verified && !evidence.landlock_self_test && !evidence.namespace_self_test);
        assert!(evidence.check().is_err());
    }

    /// R7a item 1a: a unit file other than the shipped text gives no key, so
    /// nothing runs and nothing is verified (a previous pass is withdrawn).
    #[test]
    fn selftest_changed_unit_file_never_runs_and_verifies_nothing() {
        let shipped = crate::systemd::ISOLATION_SELFTEST_UNIT_TEXT;
        for installed in [shipped.replace("ExecStart=/usr/lib/duduclaw/duduclaw-run-selftest", "ExecStart=/bin/true"),
            format!("{shipped}\n"), String::new()] {
            let mut selftest = SelfTest::default();
            let mut host = Fake::new(vec![Ok(Outcome::Pass)]);
            let published = Published::default();
            selftest.publish(&mut host, &published);
            assert!(published.load().unwrap().facts.passed);
            host.unit_file = installed.into_bytes();
            selftest.publish(&mut host, &published);
            assert!(published.load().is_none());
            assert_eq!(host.runs, 1);
            assert_eq!(Snapshot::facts_at(published.load().as_ref(), host.now), Facts::default());
            // Also from a cold start: no run at all.
            let mut cold = Fake::new(vec![]);
            cold.unit_file = host.unit_file.clone();
            SelfTest::default().publish(&mut cold, &published);
            assert!(published.load().is_none());
            assert_eq!(cold.runs, 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selftest_unit_digest_requires_the_shipped_unit_bytes() {
        let shipped = crate::systemd::ISOLATION_SELFTEST_UNIT_TEXT;
        assert!(super::linux::unit_digest(shipped.as_bytes()).is_ok());
        for installed in [shipped.replace("ExecStart=/usr/lib/duduclaw/duduclaw-run-selftest", "ExecStart=/bin/true"),
            format!("{shipped}\n"), String::new()] {
            assert!(super::linux::unit_digest(installed.as_bytes()).is_err());
        }
    }

    /// R7a item 3: the outcome is kept only under a key re-read successfully
    /// after the run and equal to the one it ran under.
    #[test]
    fn selftest_outcome_is_discarded_when_the_key_changed_during_the_run() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Pass), Ok(Outcome::Pass)]);
        let mut replaced = key(); replaced.selftest.sha256 = Sha256Digest([6; 32]);
        host.after = Some(replaced.clone());
        assert!(selftest.ensure(&mut host).is_err());
        assert_eq!(selftest.decide(&key(), host.now), Decision::Run);
        assert_eq!(selftest.decide(&replaced, host.now), Decision::Run);
        // The next pass runs again.
        host.after = None;
        assert!(selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(host.runs, 2);
    }

    #[test]
    fn selftest_post_run_key_error_caches_nothing() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Pass), Ok(Outcome::Pass)]);
        host.after_err = true;
        let published = Published::default();
        selftest.publish(&mut host, &published);
        assert!(published.load().is_none());
        assert_eq!(selftest.decide(&key(), host.now), Decision::Run);
        assert!(!selftest.facts(&key()).passed);
        // The next wake runs again and caches under the key it re-read.
        host.after_err = false;
        selftest.publish(&mut host, &published);
        assert_eq!(host.runs, 2);
        assert!(published.load().unwrap().facts.passed);

        // An error after a retried fail clears the fail too: no 60 s wait.
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Fail), Ok(Outcome::Fail)]);
        assert!(!selftest.ensure(&mut host).unwrap().passed);
        host.now += FAIL_RETRY_MS;
        host.after_err = true;
        assert!(selftest.ensure(&mut host).is_err());
        assert_eq!(host.runs, 2);
        assert_eq!(selftest.decide(&key(), host.now), Decision::Run);
    }

    /// The refresh path with the worker replaced by the test: started workers
    /// are recorded (slot and wake receiver) and their passes run by hand.
    struct Harness {
        installation: SelfTestInstallation,
        started: Arc<Mutex<Vec<(Arc<Published>, mpsc::Receiver<()>)>>>,
        clock: Arc<AtomicU64>,
    }
    fn harness() -> Harness {
        let started = Arc::new(Mutex::new(Vec::new()));
        let clock = Arc::new(AtomicU64::new(1_000));
        let (record, time) = (started.clone(), clock.clone());
        let installation = SelfTestInstallation::new(
            Box::new(move |published: Arc<Published>, wake: mpsc::Receiver<()>| record.lock().unwrap().push((published, wake))),
            Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        Harness { installation, started, clock }
    }
    impl Harness {
        fn woken(&self) -> bool { self.started.lock().unwrap()[0].1.try_recv().is_ok() }
        fn set_now(&self, now: u64) { self.clock.store(now, Ordering::SeqCst); }
    }

    /// R7a item 2: readback only reads the published snapshot (never the key)
    /// and reports nothing verified for a missing or stale one.
    #[test]
    fn selftest_readback_reads_only_the_snapshot_and_refuses_missing_or_stale_ones() {
        let mut h = harness();
        let mut host = Fake::new(vec![Ok(Outcome::Pass)]);
        // Nothing published: nothing verified; the worker is started once and woken.
        for _ in 0..10 { assert_eq!(h.installation.current(), Facts::default()); }
        assert_eq!(h.started.lock().unwrap().len(), 1);
        assert!(h.woken());
        assert_eq!(host.keys, 0);
        let published = h.started.lock().unwrap()[0].0.clone();
        let mut worker = SelfTest::default();
        worker.publish(&mut host, &published);
        assert_eq!((host.runs, host.keys), (1, 2));
        // Fresh pass: reported, and no wake while it is not yet due.
        for _ in 0..10 { assert!(h.installation.current().passed); }
        assert!(!h.woken());
        h.set_now(1_000 + WORKER_PERIOD_MS);
        assert!(h.installation.current().passed);
        assert!(h.woken());
        h.set_now(1_000 + SNAPSHOT_FRESH_MS - 1);
        assert!(h.installation.current().passed);
        // Stale (the worker stopped publishing): nothing verified at all.
        h.set_now(1_000 + SNAPSHOT_FRESH_MS);
        assert_eq!(h.installation.current(), Facts::default());
        assert!(readiness(&h.installation.current()).check().is_err());
        // A clock reading before the measurement is never fresh.
        h.set_now(999);
        assert_eq!(h.installation.current(), Facts::default());
        // None of these readbacks computed the key.
        assert_eq!(host.keys, 2);
        // The worker's next pass reuses the pass and refreshes the snapshot.
        host.now = 1_000 + SNAPSHOT_FRESH_MS;
        h.set_now(host.now);
        worker.publish(&mut host, &published);
        assert!(h.installation.current().passed);
        assert_eq!((host.runs, host.keys), (1, 3));
        assert_eq!(h.started.lock().unwrap().len(), 1);
        // A clock failure on the refresh path: nothing verified.
        let mut failing = SelfTestInstallation::new(Box::new(|_: Arc<Published>, _: mpsc::Receiver<()>| {}),
            Box::new(|| -> io::Result<u64> { Err(io::ErrorKind::Other.into()) }));
        assert_eq!(failing.current(), Facts::default());
    }

    #[test]
    fn selftest_worker_publishes_a_pass_reuses_it_and_stops_with_the_installation() {
        let published = Arc::new(Published::default());
        let (sender, receiver) = mpsc::sync_channel::<()>(1);
        drop(sender);
        let slot = published.clone();
        // One pass, then the dropped sender ends the loop.
        let host = std::thread::spawn(move || {
            let mut host = Fake::new(vec![Ok(Outcome::Pass)]);
            worker_loop(&mut host, &slot, &receiver);
            host
        }).join().unwrap();
        assert_eq!(host.runs, 1);
        let snapshot = published.load().unwrap();
        assert_eq!((snapshot.key.clone(), snapshot.facts.passed, snapshot.computed_at_ms), (key(), true, 1_000));

        // While the unit runs the snapshot shows the binaries verified, nothing passed.
        let mut worker = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Pass)]);
        let mut during = Vec::new();
        assert!(worker.measure(&mut host, &mut |running: Snapshot| during.push(running)).unwrap().facts.passed);
        assert_eq!(during.len(), 1);
        assert!(during[0].facts.executable_verified && !during[0].facts.passed);
        // Later passes reuse the pass and keep the snapshot fresh.
        for _ in 0..5 {
            host.now += WORKER_PERIOD_MS;
            worker.publish(&mut host, &published);
            let snapshot = published.load().unwrap();
            assert!(snapshot.facts.passed);
            assert_eq!(snapshot.computed_at_ms, host.now);
            assert!(Snapshot::facts_at(Some(&snapshot), host.now + WORKER_PERIOD_MS).passed);
        }
        assert_eq!(host.runs, 1);
    }

    #[test]
    fn selftest_trampoline_readiness_is_built_from_the_measured_facts() {
        let pass = Facts { executable_verified: true, passed: true, detected_abi: 4, selected_abi: 4 };
        let evidence = readiness(&pass);
        assert_eq!(evidence.version, BOOTSTRAP_VERSION);
        assert!(evidence.executable_verified && evidence.landlock_self_test && evidence.namespace_self_test);
        assert_eq!((evidence.detected_landlock_abi, evidence.selected_landlock_abi), (4, 4));
        assert!(evidence.check().is_ok());
        // A newer kernel: the selected ABI is the clamped one.
        assert!(readiness(&Facts { detected_abi: 8, selected_abi: 7, ..pass }).check().is_ok());
        assert!(readiness(&Facts { detected_abi: 8, selected_abi: 8, ..pass }).check().is_err());
        // Verified binaries without a cached pass, or a pass without verified binaries.
        let unproven = readiness(&Facts { passed: false, ..pass });
        assert!(unproven.executable_verified && !unproven.landlock_self_test && !unproven.namespace_self_test);
        assert!(unproven.check().is_err());
        let unverified = readiness(&Facts { executable_verified: false, ..pass });
        assert!(!unverified.landlock_self_test && unverified.check().is_err());
    }
}
