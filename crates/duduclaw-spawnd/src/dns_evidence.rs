//! R7b: `dns_route_verified` from a live lookup through egress instead of an
//! installer receipt (design: install-evidence §8 R7b).
//!
//! A background worker asks egress, over the existing control connection, to
//! resolve the signed policy's `dns_probe_name` (policy v4) through its
//! configured route (`EgressControl::ProbeDns`). It passes only when egress
//! answers `DnsProbed` with the route this host is configured for (the
//! root-owned `/etc/duduclaw/egress.json` the installer writes from the signed
//! policy), `resolved: true` and at least one public address.
//!
//! A pass is fresh for [`PASS_FRESH_MS`] and re-probed after
//! [`PASS_REPROBE_MS`]; a failure is retried after [`FAIL_RETRY_MS`]. No
//! result yet, a stale one, a policy older than v4 (nothing is sent), an
//! unreadable configuration, no control connection or a failed exchange are
//! all "not verified".
//!
//! As `crate::selftest`: only the worker probes (started by the first
//! readiness refresh, after start-up recovery, so probing is never a
//! precondition of spawnd opening — the egress unit itself waits for
//! spawnd's readiness gate). The refresh path only loads an atomic snapshot
//! and wakes the worker. The worker never opens the control connection; the
//! lifecycle thread does.
use std::{io, sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc}, time::Duration};
use duduclaw_isolation::egress::{DnsRoute, EgressControlResponse};

/// A passed probe vouches for the route this long after it was sent.
pub const PASS_FRESH_MS: u64 = 60_000;
/// A pass is probed again this long after it was sent, so a live egress keeps
/// the evidence fresh without a gap (egress rate-limits real lookups to one
/// per 10 s; this is well above that).
pub const PASS_REPROBE_MS: u64 = 45_000;
/// A failed (or not attempted) probe is retried this long after it.
pub const FAIL_RETRY_MS: u64 = 5_000;
/// The worker checks whether a probe is due at least this often.
pub const WORKER_PERIOD_MS: u64 = 1_000;

/// What the probe needs from the installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// The signed policy's `dns_probe_name`; `None` below policy v4.
    pub name: Option<String>,
    /// The route this host is configured for.
    pub route: DnsRoute,
}

/// Side effects of one probe (fakes in tests). Called only on the worker.
pub trait Host {
    fn inputs(&mut self) -> io::Result<Inputs>;
    /// One `ProbeDns` exchange on the existing control connection; `Err` when
    /// there is none or the exchange failed.
    fn probe(&mut self, name: &str) -> io::Result<EgressControlResponse>;
    /// Monotonic milliseconds (CLOCK_BOOTTIME in production).
    fn now_ms(&mut self) -> io::Result<u64>;
}

/// The pass rule.
pub fn passes(response: &EgressControlResponse, configured: DnsRoute) -> bool {
    matches!(response, EgressControlResponse::DnsProbed { route, resolved: true, public_addresses }
        if *route == configured && *public_addresses >= 1)
}

/// The outcome of the probe sent at `at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot { pub passed: bool, pub at_ms: u64 }
impl Snapshot {
    /// Verified at `now_ms`: a pass not older than [`PASS_FRESH_MS`] and not
    /// from the future. Missing ⇒ false.
    pub fn verified_at(snapshot: Option<&Self>, now_ms: u64) -> bool {
        snapshot.is_some_and(|s| s.passed && now_ms >= s.at_ms && now_ms - s.at_ms < PASS_FRESH_MS)
    }
    /// A probe is due: none yet, a clock before it, or the pass/fail interval elapsed.
    pub fn due_at(snapshot: Option<&Self>, now_ms: u64) -> bool {
        match snapshot {
            None => true,
            Some(s) if now_ms < s.at_ms => true,
            Some(s) => now_ms - s.at_ms >= if s.passed { PASS_REPROBE_MS } else { FAIL_RETRY_MS },
        }
    }
}

/// Lock-free slot the worker stores into and the refresh path loads:
/// `at_ms << 1 | passed`, or `u64::MAX` for none.
pub struct Published(AtomicU64);
const NONE: u64 = u64::MAX;
impl Default for Published { fn default() -> Self { Self(AtomicU64::new(NONE)) } }
impl Published {
    pub fn store(&self, snapshot: Option<Snapshot>) {
        let value = match snapshot {
            Some(s) if s.at_ms < 1 << 62 => (s.at_ms << 1) | u64::from(s.passed),
            _ => NONE,
        };
        self.0.store(value, Ordering::Release);
    }
    pub fn load(&self) -> Option<Snapshot> {
        match self.0.load(Ordering::Acquire) {
            NONE => None,
            value => Some(Snapshot { passed: value & 1 == 1, at_ms: value >> 1 }),
        }
    }
}

/// One worker pass: when due, read the inputs, probe (only with a v4 probe
/// name) and publish the outcome, a failure included, so the retry cadence holds.
pub fn step(host: &mut dyn Host, published: &Published) {
    let Ok(now) = host.now_ms() else { published.store(None); return };
    if !Snapshot::due_at(published.load().as_ref(), now) { return; }
    let passed = match host.inputs() {
        Ok(Inputs { name: Some(name), route }) => host.probe(&name).is_ok_and(|response| passes(&response, route)),
        // Policy older than v4, or unreadable inputs: nothing is sent.
        _ => false,
    };
    published.store(Some(Snapshot { passed, at_ms: now }));
}

/// The background worker: a pass now, then on every wake and at least every
/// [`WORKER_PERIOD_MS`], until the evidence (the sender) is dropped.
pub fn worker_loop(host: &mut dyn Host, published: &Published, wake: &mpsc::Receiver<()>) {
    loop {
        step(host, published);
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

/// The refresh path's view: loads the snapshot, wakes the single worker when a
/// probe is due, never probes itself.
pub struct DnsRouteEvidence {
    published: Arc<Published>,
    wake: Option<mpsc::SyncSender<()>>,
    start_worker: StartWorker,
    clock: Clock,
}
impl DnsRouteEvidence {
    pub fn new(start_worker: StartWorker, clock: Clock) -> Self {
        Self { published: Arc::new(Published::default()), wake: None, start_worker, clock }
    }
    pub fn current(&mut self) -> bool {
        let snapshot = self.published.load();
        let Ok(now) = (self.clock)() else { self.request_pass(); return false };
        if Snapshot::due_at(snapshot.as_ref(), now) { self.request_pass(); }
        Snapshot::verified_at(snapshot.as_ref(), now)
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
pub use linux::{installed, InstalledHost};

#[cfg(target_os = "linux")]
mod linux {
    use duduclaw_isolation::ids::RequestId;
    use crate::host::SharedEgressClient;
    use super::*;

    fn unavailable() -> io::Error { io::Error::other("egress DNS probe unavailable") }

    /// Production side effects: the signed policy, the installed egress
    /// configuration and the shared control connection.
    pub struct InstalledHost { pub client: SharedEgressClient }
    impl Host for InstalledHost {
        fn inputs(&mut self) -> io::Result<Inputs> {
            Ok(Inputs { name: crate::installed::installed_dns_probe_name()?, route: crate::installed::configured_dns_route()? })
        }
        fn probe(&mut self, name: &str) -> io::Result<EgressControlResponse> {
            let request = RequestId(crate::server::random_id()?);
            // Held for this one exchange (see `SharedEgressClient`).
            let mut slot = self.client.lock().map_err(|_| unavailable())?;
            // Never connects: no connection means egress is unreachable.
            let client = slot.as_mut().ok_or_else(unavailable)?;
            let result = client.probe_dns(request, name);
            // A failed exchange leaves the connection shut down; drop it so the
            // lifecycle thread reconnects.
            if result.is_err() { *slot = None; }
            result
        }
        fn now_ms(&mut self) -> io::Result<u64> { crate::host_state::boottime_ms() }
    }

    /// The installed evidence over `client`, the connection `LinuxEgress` uses.
    pub fn installed(client: SharedEgressClient) -> DnsRouteEvidence {
        DnsRouteEvidence::new(Box::new(move |published: Arc<Published>, wake: mpsc::Receiver<()>| {
            let client = client.clone();
            std::thread::spawn(move || worker_loop(&mut InstalledHost { client }, &published, &wake));
        }), Box::new(crate::host_state::boottime_ms))
    }

    impl crate::host::DnsEvidence for DnsRouteEvidence {
        /// Never blocks: the latest fresh pass, or false.
        fn verified(&mut self) -> bool { self.current() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use duduclaw_isolation::egress::EgressError;

    /// A scripted host: inputs, queued probe answers, a clock, and every name sent.
    struct Fake { inputs: io::Result<Inputs>, answers: Vec<io::Result<EgressControlResponse>>, sent: Vec<String>, now: u64 }
    impl Fake {
        fn new(answers: Vec<io::Result<EgressControlResponse>>) -> Self {
            Self { inputs: Ok(inputs(Some("api.example.com"))), answers, sent: Vec::new(), now: 10_000 }
        }
    }
    impl Host for Fake {
        fn inputs(&mut self) -> io::Result<Inputs> {
            match &self.inputs { Ok(inputs) => Ok(inputs.clone()), Err(_) => Err(io::ErrorKind::PermissionDenied.into()) }
        }
        fn probe(&mut self, name: &str) -> io::Result<EgressControlResponse> {
            self.sent.push(name.to_owned());
            if self.answers.is_empty() { panic!("unexpected probe") }
            self.answers.remove(0)
        }
        fn now_ms(&mut self) -> io::Result<u64> { Ok(self.now) }
    }
    fn inputs(name: Option<&str>) -> Inputs { Inputs { name: name.map(str::to_owned), route: DnsRoute::ResolvedVarlink } }
    fn probed(route: DnsRoute, resolved: bool, public_addresses: u32) -> io::Result<EgressControlResponse> {
        Ok(EgressControlResponse::DnsProbed { route, resolved, public_addresses })
    }
    /// One pass at `host.now`, then whether the evidence is verified then.
    fn verified_after_step(host: &mut Fake, published: &Published) -> bool {
        step(host, published);
        Snapshot::verified_at(published.load().as_ref(), host.now)
    }

    #[test]
    fn dns_evidence_pass_rule() {
        let cases: [(io::Result<EgressControlResponse>, bool); 7] = [
            (probed(DnsRoute::ResolvedVarlink, true, 1), true),
            (probed(DnsRoute::ResolvedVarlink, true, 12), true),
            // Wrong route, unresolved, no public answer, a refusal, a connection error.
            (probed(DnsRoute::DedicatedDnsBroker, true, 3), false),
            (probed(DnsRoute::ResolvedVarlink, false, 0), false),
            (probed(DnsRoute::ResolvedVarlink, true, 0), false),
            (Ok(EgressControlResponse::Refused { code: EgressError::ResourceLimit }), false),
            (Err(io::ErrorKind::TimedOut.into()), false),
        ];
        for (index, (answer, expected)) in cases.into_iter().enumerate() {
            let mut host = Fake::new(vec![answer]);
            let published = Published::default();
            assert_eq!(verified_after_step(&mut host, &published), expected, "case {index}");
            assert_eq!(host.sent, ["api.example.com"]);
            // Every outcome is published with its time, a failure included.
            assert_eq!(published.load(), Some(Snapshot { passed: expected, at_ms: host.now }));
        }
        // The rule compares against the configured route, whichever it is.
        let mut host = Fake::new(vec![probed(DnsRoute::DedicatedDnsBroker, true, 1)]);
        host.inputs = Ok(Inputs { name: Some("api.example.com".into()), route: DnsRoute::DedicatedDnsBroker });
        assert!(verified_after_step(&mut host, &Published::default()));
    }

    #[test]
    fn dns_evidence_policy_without_probe_name_or_inputs_is_false_without_sending() {
        let mut host = Fake::new(vec![]);
        host.inputs = Ok(inputs(None));
        let published = Published::default();
        assert!(!verified_after_step(&mut host, &published));
        host.inputs = Err(io::ErrorKind::PermissionDenied.into());
        host.now += FAIL_RETRY_MS;
        assert!(!verified_after_step(&mut host, &published));
        assert!(host.sent.is_empty());
        assert_eq!(published.load(), Some(Snapshot { passed: false, at_ms: host.now }));
    }

    #[test]
    fn dns_evidence_pass_expires_and_is_reprobed_before_it_does() {
        let mut host = Fake::new(vec![probed(DnsRoute::ResolvedVarlink, true, 1)]);
        let published = Published::default();
        let start = host.now;
        assert!(verified_after_step(&mut host, &published));
        // Not due before the re-probe point: no probe, still verified.
        for elapsed in [1, FAIL_RETRY_MS, PASS_REPROBE_MS - 1] {
            host.now = start + elapsed;
            assert!(verified_after_step(&mut host, &published), "{elapsed}");
        }
        assert_eq!(host.sent.len(), 1);
        // The snapshot alone (a stuck worker) expires at PASS_FRESH_MS.
        assert!(Snapshot::verified_at(published.load().as_ref(), start + PASS_FRESH_MS - 1));
        assert!(!Snapshot::verified_at(published.load().as_ref(), start + PASS_FRESH_MS));
        assert!(!Snapshot::verified_at(published.load().as_ref(), start - 1));
        assert!(!Snapshot::verified_at(None, start));
        // A live worker re-probes at PASS_REPROBE_MS; a failure then withdraws the pass at once.
        host.answers = vec![probed(DnsRoute::ResolvedVarlink, true, 1), probed(DnsRoute::ResolvedVarlink, false, 0)];
        host.now = start + PASS_REPROBE_MS;
        assert!(verified_after_step(&mut host, &published));
        assert_eq!(host.sent.len(), 2);
        host.now += PASS_REPROBE_MS;
        assert!(!verified_after_step(&mut host, &published));
        assert_eq!(host.sent.len(), 3);
    }

    #[test]
    fn dns_evidence_failure_is_retried_after_five_seconds() {
        let mut host = Fake::new(vec![Err(io::ErrorKind::NotConnected.into()), probed(DnsRoute::ResolvedVarlink, true, 0),
            probed(DnsRoute::ResolvedVarlink, true, 2)]);
        let published = Published::default();
        let start = host.now;
        assert!(!verified_after_step(&mut host, &published));
        for elapsed in [0, 1, FAIL_RETRY_MS - 1] {
            host.now = start + elapsed;
            assert!(!verified_after_step(&mut host, &published));
        }
        assert_eq!(host.sent.len(), 1);
        host.now = start + FAIL_RETRY_MS;
        assert!(!verified_after_step(&mut host, &published));
        assert_eq!(host.sent.len(), 2);
        host.now += FAIL_RETRY_MS;
        assert!(verified_after_step(&mut host, &published));
        assert_eq!(host.sent.len(), 3);
        // A clock reading before the last probe makes one due (and is never fresh).
        assert!(Snapshot::due_at(published.load().as_ref(), host.now - 1));
    }

    #[test]
    fn dns_evidence_published_slot_round_trips() {
        let published = Published::default();
        assert_eq!(published.load(), None);
        for snapshot in [Snapshot { passed: true, at_ms: 0 }, Snapshot { passed: false, at_ms: 123_456 },
            Snapshot { passed: true, at_ms: (1 << 62) - 1 }] {
            published.store(Some(snapshot));
            assert_eq!(published.load(), Some(snapshot));
        }
        published.store(Some(Snapshot { passed: true, at_ms: 1 << 62 }));
        assert_eq!(published.load(), None);
        published.store(None);
        assert_eq!(published.load(), None);
    }

    /// The refresh path never probes: it loads the snapshot, starts the worker
    /// once and wakes it only when a probe is due.
    #[test]
    fn dns_evidence_refresh_reads_only_the_snapshot() {
        let started = Arc::new(Mutex::new(Vec::<(Arc<Published>, mpsc::Receiver<()>)>::new()));
        let clock = Arc::new(AtomicU64::new(10_000));
        let (record, time) = (started.clone(), clock.clone());
        let mut evidence = DnsRouteEvidence::new(
            Box::new(move |published: Arc<Published>, wake: mpsc::Receiver<()>| record.lock().unwrap().push((published, wake))),
            Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        for _ in 0..10 { assert!(!evidence.current()); }
        assert_eq!(started.lock().unwrap().len(), 1);
        let woken = || started.lock().unwrap()[0].1.try_recv().is_ok();
        assert!(woken());
        let published = started.lock().unwrap()[0].0.clone();
        let mut host = Fake::new(vec![probed(DnsRoute::ResolvedVarlink, true, 1)]);
        step(&mut host, &published);
        for _ in 0..10 { assert!(evidence.current()); }
        assert!(!woken());
        clock.store(10_000 + PASS_REPROBE_MS, Ordering::SeqCst);
        assert!(evidence.current());
        assert!(woken());
        clock.store(10_000 + PASS_FRESH_MS, Ordering::SeqCst);
        assert!(!evidence.current());
        assert_eq!(host.sent.len(), 1);
        assert_eq!(started.lock().unwrap().len(), 1);
        // A clock failure on the refresh path: not verified.
        let mut failing = DnsRouteEvidence::new(Box::new(|_: Arc<Published>, _: mpsc::Receiver<()>| {}),
            Box::new(|| -> io::Result<u64> { Err(io::ErrorKind::Other.into()) }));
        assert!(!failing.current());
    }

    #[test]
    fn dns_evidence_worker_probes_and_stops_with_the_evidence() {
        let published = Arc::new(Published::default());
        let (sender, receiver) = mpsc::sync_channel::<()>(1);
        drop(sender);
        let slot = published.clone();
        let host = std::thread::spawn(move || {
            let mut host = Fake::new(vec![probed(DnsRoute::ResolvedVarlink, true, 1)]);
            worker_loop(&mut host, &slot, &receiver);
            host
        }).join().unwrap();
        assert_eq!(host.sent.len(), 1);
        assert_eq!(published.load(), Some(Snapshot { passed: true, at_ms: 10_000 }));
    }
}
