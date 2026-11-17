//! R7a: the trampoline readiness facts, measured by spawnd itself instead of
//! read from an installer receipt (design: install-evidence §8 R7a).
//!
//! - `executable_verified`: `/usr/lib/duduclaw/duduclaw-run` AND
//!   `/usr/lib/duduclaw/duduclaw-run-selftest` match the committed signed
//!   generation's artifacts (sha256, size, mode), read by descriptor.
//! - `landlock_self_test` / `namespace_self_test`: the cached outcome of the
//!   fixed unit `duduclaw-isolation-selftest.service`, started through PID 1.
//! - ABI pair: the live `duduclaw_run::linux::probe::probe()`.
//!
//! The outcome is cached under (boot id, committed generation, ABI pair, both
//! artifact identities). A pass is reused for its key; a key change runs the
//! unit again; a fail is reused for [`FAIL_RETRY_MS`] and only then retried.
//! The run happens on a background thread, first requested by the first
//! readiness refresh (after start-up recovery); it is never a precondition of
//! spawnd's sockets or of the update service's readiness.
use duduclaw_isolation::ids::Sha256Digest;
use crate::bootstrap::{TrampolineReadiness, BOOTSTRAP_VERSION};

pub const TRAMPOLINE_DIRECTORY: &str = "/usr/lib/duduclaw";
/// A failed outcome is reused for this long before the unit runs again.
pub const FAIL_RETRY_MS: u64 = 60_000;
/// The unit's own `TimeoutStartSec=30` plus margin for PID 1 to record it.
pub const UNIT_DEADLINE_MS: u64 = 40_000;

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

/// The side effects `SelfTest::ensure` needs (fakes in tests).
pub trait Host {
    /// The live key; `Err` when either installed file does not match the
    /// committed generation (or the generation, boot id or ABI is unreadable).
    fn key(&mut self) -> std::io::Result<CacheKey>;
    /// Start the fixed unit once and wait for it. `Err` counts as a fail.
    fn run(&mut self) -> std::io::Result<Outcome>;
    /// Monotonic milliseconds (CLOCK_BOOTTIME in production).
    fn now_ms(&mut self) -> std::io::Result<u64>;
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
    /// Reuse the cached outcome for the live key, or run the unit once and
    /// cache its outcome (pass or fail). If the key changed while the unit ran,
    /// the outcome is discarded: it may describe neither key.
    pub fn ensure(&mut self, host: &mut dyn Host) -> std::io::Result<Facts> {
        let key = host.key()?;
        let now = host.now_ms()?;
        if self.decide(&key, now) == Decision::Run {
            let outcome = host.run().unwrap_or(Outcome::Fail);
            let at_ms = host.now_ms()?;
            self.cached = match host.key() {
                Ok(after) if after != key => None,
                // An unreadable key after the run still binds the outcome to the
                // key it ran under, so a transient error cannot cause a re-run.
                _ => Some(Cached { key: key.clone(), outcome, at_ms }),
            };
        }
        Ok(self.facts(&key))
    }
}

#[cfg(target_os = "linux")]
pub use linux::{live_key, InstalledHost, SelfTestInstallation};

#[cfg(target_os = "linux")]
mod linux {
    use std::{io, os::unix::fs::MetadataExt, path::Path, sync::{mpsc, Arc, Mutex, TryLockError}, time::Duration};
    use duduclaw_isolation::{ids::ISOLATION_SELFTEST_UNIT, ipc::ErrorCode, update::{InstallTarget, ReleaseManifest}};
    use duduclaw_updated::linux::files::{verify_fd, Directory};
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

    /// The live cache key; `Err` unless both installed binaries are the
    /// committed generation's.
    pub fn live_key() -> io::Result<CacheKey> {
        let (generation, manifest) = crate::installed::committed_generation()?;
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().to_owned();
        if boot_id.is_empty() { return Err(denied()); }
        let ready = duduclaw_run::linux::probe::probe().map_err(|_| denied())?;
        let directory = Directory::open(Path::new(TRAMPOLINE_DIRECTORY)).map_err(|_| denied())?;
        let trampoline = verified_artifact(&directory, &manifest, InstallTarget::Trampoline)?;
        let selftest = verified_artifact(&directory, &manifest, InstallTarget::TrampolineSelftest)?;
        Ok(CacheKey { boot_id, generation, detected_abi: ready.landlock_abi, selected_abi: ready.landlock_ruleset_abi,
            trampoline, selftest })
    }

    /// Production side effects: the live key and the fixed unit through PID 1.
    #[derive(Default)]
    pub struct InstalledHost { systemd: Option<crate::systemd::linux::Systemd> }
    impl Host for InstalledHost {
        fn key(&mut self) -> io::Result<CacheKey> { live_key() }
        fn run(&mut self) -> io::Result<Outcome> {
            if self.systemd.is_none() { self.systemd = Some(crate::systemd::linux::Systemd::connect()?); }
            let systemd = self.systemd.as_ref().ok_or_else(denied)?;
            let result = systemd.run_fixed_unit(ISOLATION_SELFTEST_UNIT, Duration::from_millis(UNIT_DEADLINE_MS));
            // A bus error drops the connection; the next run reconnects.
            if result.is_err() { self.systemd = None; }
            Ok(if result?.passed() { Outcome::Pass } else { Outcome::Fail })
        }
        fn now_ms(&mut self) -> io::Result<u64> { crate::host_state::boottime_ms() }
    }

    /// `NativeTrampoline`'s installation evidence. `readback` runs on the
    /// refresh path and never blocks on the unit: it recomputes the live key,
    /// reads the cache and, when the cache does not cover that key, wakes the
    /// single background worker that runs `SelfTest::ensure`.
    #[derive(Default)]
    pub struct SelfTestInstallation { shared: Arc<Mutex<SelfTest>>, wake: Option<mpsc::SyncSender<()>> }
    impl SelfTestInstallation {
        fn request_run(&mut self) {
            if self.wake.is_none() {
                let (sender, receiver) = mpsc::sync_channel::<()>(1);
                let shared = self.shared.clone();
                std::thread::spawn(move || worker(&shared, &receiver));
                self.wake = Some(sender);
            }
            let disconnected = self.wake.as_ref().is_some_and(|sender|
                matches!(sender.try_send(()), Err(mpsc::TrySendError::Disconnected(()))));
            if disconnected { self.wake = None; }
        }
    }
    /// One run at a time; requests that arrive during a run coalesce into one.
    fn worker(shared: &Mutex<SelfTest>, wake: &mpsc::Receiver<()>) {
        let mut host = InstalledHost::default();
        while wake.recv().is_ok() {
            {
                let Ok(mut selftest) = shared.lock() else { return };
                // Err: no key (files not the signed ones, or no live generation),
                // so nothing ran and nothing is cached; readiness stays false.
                let _ = selftest.ensure(&mut host);
            }
            // A request queued before this run began is answered by it. Dropping
            // it avoids a needless re-lock that would hide a fresh pass from one
            // refresh (readback treats a held lock as "run in progress").
            while wake.try_recv().is_ok() {}
        }
    }
    impl crate::native_trampoline::Installation for SelfTestInstallation {
        fn readback(&mut self) -> crate::Result<TrampolineReadiness> {
            let Ok(key) = live_key() else { return Ok(readiness(&Facts::default())) };
            let now = crate::host_state::boottime_ms().map_err(|_| ErrorCode::IsolationUnavailable)?;
            let (facts, decision) = match self.shared.try_lock() {
                Ok(selftest) => (selftest.facts(&key), selftest.decide(&key, now)),
                // A run is in progress, so the cache did not cover a live key.
                Err(TryLockError::WouldBlock) => (Facts::verified(&key, false), Decision::Reuse(Outcome::Fail)),
                Err(TryLockError::Poisoned(_)) => return Err(ErrorCode::IsolationUnavailable),
            };
            if decision == Decision::Run { self.request_run(); }
            Ok(readiness(&facts))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn artifact(seed: u8) -> ArtifactKey { ArtifactKey { sha256: Sha256Digest([seed; 32]), size: 1000, mode: 0o755 } }
    fn key() -> CacheKey {
        CacheKey { boot_id: "boot-a".into(), generation: Sha256Digest([1; 32]), detected_abi: 4, selected_abi: 4,
            trampoline: artifact(2), selftest: artifact(3) }
    }
    /// A scripted host: the key it reports, the outcomes its runs return, a clock.
    struct Fake { key: io::Result<CacheKey>, after: Option<CacheKey>, outcomes: Vec<io::Result<Outcome>>, runs: usize, now: u64 }
    impl Fake {
        fn new(outcomes: Vec<io::Result<Outcome>>) -> Self { Self { key: Ok(key()), after: None, outcomes, runs: 0, now: 1_000 } }
    }
    impl Host for Fake {
        fn key(&mut self) -> io::Result<CacheKey> {
            if self.runs > 0 { if let Some(after) = &self.after { return Ok(after.clone()); } }
            match &self.key { Ok(key) => Ok(key.clone()), Err(_) => Err(io::ErrorKind::PermissionDenied.into()) }
        }
        fn run(&mut self) -> io::Result<Outcome> {
            self.runs += 1;
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
        let changes: [fn(&mut CacheKey); 8] = [
            |k| k.boot_id = "boot-b".into(),
            |k| k.generation = Sha256Digest([9; 32]),
            |k| k.detected_abi = 5,
            |k| k.selected_abi = 3,
            |k| k.trampoline.sha256 = Sha256Digest([8; 32]),
            |k| k.trampoline.size += 1,
            |k| k.selftest.sha256 = Sha256Digest([7; 32]),
            |k| k.selftest.mode = 0o750,
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

    #[test]
    fn selftest_outcome_is_discarded_when_the_key_changed_during_the_run() {
        let mut selftest = SelfTest::default();
        let mut host = Fake::new(vec![Ok(Outcome::Pass), Ok(Outcome::Pass)]);
        let mut replaced = key(); replaced.selftest.sha256 = Sha256Digest([6; 32]);
        host.after = Some(replaced.clone());
        assert!(!selftest.ensure(&mut host).unwrap().passed);
        assert_eq!(selftest.decide(&key(), host.now), Decision::Run);
        assert_eq!(selftest.decide(&replaced, host.now), Decision::Run);
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
