//! R7b part 2: `egress_second_layer` from PID 1's own view of the egress unit
//! instead of an installer receipt (display-only: `Readiness::check` does not
//! include it).
//!
//! A background worker asks PID 1 (read-only D-Bus properties, its own
//! connection) whether the running `duduclaw-egress.service` is the installed
//! unit with no drop-in and carries exactly the shipped unit's
//! `IPAddressDeny`/`IPAddressAllow` sets, and, on `dedicated_dns_broker`
//! hosts, whether `duduclaw-dns-broker.service` denies everything but
//! single-host upstreams (`Systemd::egress_second_layer`). This is the
//! configuration PID 1 holds for those units, not proof that the kernel
//! filter is attached (that readback needs a privilege spawnd does not hold).
//!
//! Same cadence and snapshot as `crate::dns_evidence` (whose slot and
//! refresh-side view are reused): a pass is fresh for
//! [`PASS_FRESH_MS`](crate::dns_evidence::PASS_FRESH_MS) and re-read before
//! it expires, a failure is retried after
//! [`FAIL_RETRY_MS`](crate::dns_evidence::FAIL_RETRY_MS). The egress unit only
//! starts after spawnd's readiness gate, so the value is `Unavailable` until
//! the first successful readback. The worker logs only when the outcome (or
//! the named reason) changes, never once per readback.
use std::{io, sync::mpsc, time::Duration};
use crate::dns_evidence::{Published, Snapshot, WORKER_PERIOD_MS};

/// Side effects of one readback (fakes in tests). Called only on the worker.
pub trait Host {
    /// One readback; `Err` names why the layer is not verified.
    fn readback(&mut self) -> io::Result<()>;
    /// Monotonic milliseconds (CLOCK_BOOTTIME in production).
    fn now_ms(&mut self) -> io::Result<u64>;
    /// One log line (spawnd's stderr in production).
    fn log(&mut self, line: &str);
}

/// The outcome last logged: `None` before the first readback, then
/// `Some(None)` for verified or `Some(Some(reason))`.
#[derive(Debug, Default)]
pub struct Logged(Option<Option<String>>);

/// One worker pass: when due, read back and publish the outcome (a failure
/// included, so the retry cadence holds); log it when it changed.
pub fn step(host: &mut dyn Host, published: &Published, logged: &mut Logged) {
    let Ok(now) = host.now_ms() else { published.store(None); return };
    if !Snapshot::due_at(published.load().as_ref(), now) { return; }
    let outcome = host.readback();
    let state = outcome.as_ref().err().map(io::Error::to_string);
    if logged.0.as_ref() != Some(&state) {
        match &state {
            None => host.log("egress second layer verified: PID 1 reports the shipped IP rules for the installed egress unit"),
            Some(reason) => host.log(&format!("egress second layer unavailable: {reason}")),
        }
        logged.0 = Some(state);
    }
    published.store(Some(Snapshot { passed: outcome.is_ok(), at_ms: now }));
}

/// The background worker: a pass now, then on every wake and at least every
/// [`WORKER_PERIOD_MS`], until the evidence (the sender) is dropped.
pub fn worker_loop(host: &mut dyn Host, published: &Published, wake: &mpsc::Receiver<()>) {
    let mut logged = Logged::default();
    loop {
        step(host, published, &mut logged);
        while wake.try_recv().is_ok() {}
        if let Err(mpsc::RecvTimeoutError::Disconnected) = wake.recv_timeout(Duration::from_millis(WORKER_PERIOD_MS)) {
            return;
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{installed, InstalledHost};

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;
    use duduclaw_isolation::egress::DnsRoute;
    use crate::{dns_evidence::DnsRouteEvidence, systemd::linux::Systemd};
    use super::*;

    /// Production side effects: the configured DNS route and PID 1, through
    /// this worker's own D-Bus connection (made on first use, dropped after a
    /// bus failure and made again on the next readback).
    #[derive(Default)]
    pub struct InstalledHost { systemd: Option<Systemd> }
    impl Host for InstalledHost {
        fn readback(&mut self) -> io::Result<()> {
            let broker = crate::installed::configured_dns_route()
                .map_err(|_| io::Error::other("egress configuration /etc/duduclaw/egress.json unreadable"))?
                == DnsRoute::DedicatedDnsBroker;
            if self.systemd.is_none() { self.systemd = Some(Systemd::connect()?); }
            let systemd = self.systemd.as_ref().ok_or_else(|| io::Error::other("systemd unavailable"))?;
            let result = systemd.egress_second_layer(broker);
            // A refusal (InvalidData) is an answer; anything else may be the bus.
            if result.as_ref().is_err_and(|error| error.kind() != io::ErrorKind::InvalidData) { self.systemd = None; }
            result
        }
        fn now_ms(&mut self) -> io::Result<u64> { crate::host_state::boottime_ms() }
        fn log(&mut self, line: &str) { eprintln!("duduclaw-spawnd: {line}"); }
    }

    /// The installed second-layer evidence: the refresh side only loads the
    /// worker's snapshot (`crate::host::DnsEvidence::verified`, never blocks)
    /// and starts the worker on its first call.
    pub fn installed() -> DnsRouteEvidence {
        DnsRouteEvidence::new(Box::new(|published: Arc<Published>, wake: mpsc::Receiver<()>| {
            std::thread::spawn(move || worker_loop(&mut InstalledHost::default(), &published, &wake));
        }), Box::new(crate::host_state::boottime_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_evidence::{FAIL_RETRY_MS, PASS_FRESH_MS, PASS_REPROBE_MS};

    /// Scripted readbacks, a clock and every logged line.
    struct Fake { answers: Vec<io::Result<()>>, reads: usize, now: u64, lines: Vec<String> }
    impl Fake {
        fn new(answers: Vec<io::Result<()>>) -> Self { Self { answers, reads: 0, now: 10_000, lines: Vec::new() } }
    }
    impl Host for Fake {
        fn readback(&mut self) -> io::Result<()> {
            self.reads += 1;
            if self.answers.is_empty() { panic!("unexpected readback") }
            self.answers.remove(0)
        }
        fn now_ms(&mut self) -> io::Result<u64> { Ok(self.now) }
        fn log(&mut self, line: &str) { self.lines.push(line.to_owned()); }
    }
    fn refused(reason: &str) -> io::Result<()> { Err(io::Error::new(io::ErrorKind::InvalidData, reason.to_owned())) }
    fn verified_after_step(host: &mut Fake, published: &Published, logged: &mut Logged) -> bool {
        step(host, published, logged);
        Snapshot::verified_at(published.load().as_ref(), host.now)
    }

    #[test]
    fn egress_second_layer_is_unavailable_until_a_readback_passes_and_retries_after_five_seconds() {
        let published = Published::default();
        let mut logged = Logged::default();
        // Before the first readback: unavailable.
        assert!(!Snapshot::verified_at(published.load().as_ref(), 10_000));
        let mut host = Fake::new(vec![
            refused("invalid unit policy: second layer duduclaw-egress.service: not loaded"),
            refused("invalid unit policy: second layer duduclaw-egress.service: ActiveState \"activating\""),
            Ok(()),
        ]);
        let start = host.now;
        assert!(!verified_after_step(&mut host, &published, &mut logged));
        for elapsed in [1, FAIL_RETRY_MS - 1] {
            host.now = start + elapsed;
            assert!(!verified_after_step(&mut host, &published, &mut logged));
        }
        assert_eq!(host.reads, 1);
        host.now = start + FAIL_RETRY_MS;
        assert!(!verified_after_step(&mut host, &published, &mut logged));
        host.now += FAIL_RETRY_MS;
        assert!(verified_after_step(&mut host, &published, &mut logged));
        assert_eq!(host.reads, 3);
        assert_eq!(host.lines, [
            "egress second layer unavailable: invalid unit policy: second layer duduclaw-egress.service: not loaded",
            "egress second layer unavailable: invalid unit policy: second layer duduclaw-egress.service: ActiveState \"activating\"",
            "egress second layer verified: PID 1 reports the shipped IP rules for the installed egress unit",
        ]);
        // A pass is fresh for 60 s; the snapshot alone (stuck worker) expires.
        let passed = host.now;
        assert!(Snapshot::verified_at(published.load().as_ref(), passed + PASS_FRESH_MS - 1));
        assert!(!Snapshot::verified_at(published.load().as_ref(), passed + PASS_FRESH_MS));
    }

    #[test]
    fn egress_second_layer_logs_only_on_change() {
        let published = Published::default();
        let mut logged = Logged::default();
        let mismatch = "invalid unit policy: second layer duduclaw-egress.service IPAddressAllow: missing [] unexpected [::/0 0.0.0.0/0]";
        let mut host = Fake::new(vec![Ok(()), Ok(()), refused(mismatch), refused(mismatch),
            Err(io::Error::other("invalid unit policy: second layer duduclaw-egress.service IPAddressDeny unreadable")), Ok(())]);
        assert!(verified_after_step(&mut host, &published, &mut logged));
        host.now += PASS_REPROBE_MS;
        assert!(verified_after_step(&mut host, &published, &mut logged));
        // A mismatch withdraws the pass at once and is logged once.
        host.now += PASS_REPROBE_MS;
        assert!(!verified_after_step(&mut host, &published, &mut logged));
        host.now += FAIL_RETRY_MS;
        assert!(!verified_after_step(&mut host, &published, &mut logged));
        // An unreadable property is its own named reason.
        host.now += FAIL_RETRY_MS;
        assert!(!verified_after_step(&mut host, &published, &mut logged));
        host.now += FAIL_RETRY_MS;
        assert!(verified_after_step(&mut host, &published, &mut logged));
        assert_eq!(host.reads, 6);
        assert_eq!(host.lines.len(), 4, "{:?}", host.lines);
        assert!(host.lines[0].starts_with("egress second layer verified"));
        assert_eq!(host.lines[1], format!("egress second layer unavailable: {mismatch}"));
        assert!(host.lines[2].ends_with("IPAddressDeny unreadable"), "{}", host.lines[2]);
        assert!(host.lines[3].starts_with("egress second layer verified"));
    }

    #[test]
    fn egress_second_layer_worker_reads_and_stops_with_the_evidence() {
        let published = std::sync::Arc::new(Published::default());
        let (sender, receiver) = mpsc::sync_channel::<()>(1);
        drop(sender);
        let slot = published.clone();
        let host = std::thread::spawn(move || {
            let mut host = Fake::new(vec![Ok(())]);
            worker_loop(&mut host, &slot, &receiver);
            host
        }).join().unwrap();
        assert_eq!(host.reads, 1);
        assert_eq!(published.load(), Some(Snapshot { passed: true, at_ms: 10_000 }));
        // The refresh side's view is the DNS evidence's (`DnsRouteEvidence`):
        // a clock failure there is never verified either.
        let mut failing = crate::dns_evidence::DnsRouteEvidence::new(
            Box::new(|_: std::sync::Arc<Published>, _: mpsc::Receiver<()>| {}),
            Box::new(|| -> io::Result<u64> { Err(io::ErrorKind::Other.into()) }));
        assert!(!failing.current());
    }
}
