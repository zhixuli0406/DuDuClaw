//! R7b: spawnd's `ProbeDns` control request. One real lookup of a name from the
//! signed policy through the configured route and the resolver runs use,
//! counted with the run-time public-address filter, so spawnd can report the
//! DNS route as verified from a live observation instead of a stored receipt.
//!
//! Bounded and rate limited: a request has an absolute [`PROBE_DEADLINE`],
//! plus [`PROBE_REPLY_GRACE`] only for sending a timeout answer, and at
//! most one real lookup happens per [`PROBE_INTERVAL`] (within the window the
//! previous result for the same name is returned; another name is refused),
//! so the controller cannot turn egress into a query generator. Answers are
//! never logged or returned; only name, route, `resolved` and the count are.
use std::{collections::BTreeSet, net::IpAddr, time::{Duration, Instant}};
use duduclaw_isolation::egress::{validate_dns_probe_name, DnsRoute, EgressControlResponse, EgressError, EgressProfile};
use crate::resolver::Resolver;

/// Absolute budget from dequeue through validation, lookup, counting, logging
/// and reply send. Expiry permits only [`PROBE_REPLY_GRACE`] to send failure.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(3);
/// Additional budget for an unresolved reply after the absolute deadline.
pub const PROBE_REPLY_GRACE: Duration = Duration::from_millis(250);
/// At most one real lookup per this window, whatever the controller sends.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// The single route every configured profile uses (the installer writes one
/// route for all profiles); `None` when there is no profile or they disagree,
/// and the probe is then refused rather than guessing.
pub fn configured_route(profiles: &[EgressProfile]) -> Option<DnsRoute> {
    let first = profiles.first()?.dns_route;
    profiles.iter().all(|profile| profile.dns_route == first).then_some(first)
}

struct Last { at: Instant, name: String, route: DnsRoute, resolved: bool, public_addresses: u32 }

type OwnAddresses = fn() -> Result<BTreeSet<IpAddr>, EgressError>;
struct PostLookup {
    name: String, route: DnsRoute, answers: Result<Vec<IpAddr>, EgressError>, own: OwnAddresses,
    reply: Option<tokio::sync::oneshot::Sender<(bool, u32)>>,
}

/// The syscalls one `std::thread` spawn needs on x86_64 / aarch64 glibc
/// (stack and guard mapping, `clone3` with the `clone` fallback, signal mask,
/// robust list, rseq, alternate signal stack, thread name). The unit's
/// `SystemCallFilter=` must admit every one, or the worker is never created
/// and no lookup can ever count (pinned by a test below). The in-process
/// filter (`seccomp::install`) denies `clone`/`clone3` afterwards.
pub const WORKER_START_SYSCALLS: [&str; 13] = ["clone3", "clone", "mmap", "mprotect", "munmap", "madvise",
    "rt_sigprocmask", "set_robust_list", "rseq", "sigaltstack", "prctl", "futex", "exit"];

/// The one fixed line egress prints before refusing to start without a worker.
pub const WORKER_UNAVAILABLE: &str = "duduclaw-egress: dns probe worker unavailable";

/// A single bounded worker: interface enumeration and stderr may block. Start
/// before seccomp (which denies clone); never join a timed-out blocking job.
/// Starts one thread ("dns-probe").
fn post_lookup_worker() -> Option<std::sync::mpsc::SyncSender<PostLookup>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<PostLookup>(1);
    let (ready, started) = std::sync::mpsc::sync_channel(0);
    std::thread::Builder::new().name("dns-probe".into()).spawn(move || {
        let _ = ready.send(());
        while let Ok(job) = receiver.recv() {
            if job.reply.as_ref().is_some_and(|reply| reply.is_closed()) { continue; }
            let (resolved, public_addresses) = match job.answers {
                Ok(answers) if !answers.is_empty() => {
                    let count = (job.own)().map(|own| crate::policy::public_answer_count(&answers, &own)).unwrap_or(0);
                    (true, count)
                }
                _ => (false, 0),
            };
            if job.reply.as_ref().is_some_and(|reply| reply.is_closed()) { continue; }
            // Never the answers themselves. A blocked log cannot hold control.
            eprintln!("{}", serde_json::json!({ "event": "dns_probe", "name": job.name, "route": job.route,
                "resolved": resolved, "public_addresses": public_addresses }));
            if let Some(reply) = job.reply { let _ = reply.send((resolved, public_addresses)); }
        }
    }).ok()?;
    started.recv().ok()?;
    Some(sender)
}

/// Per-controller probe state: the last real lookup and its time.
pub struct DnsProbe { deadline: Duration, last: Option<Last>, worker: Option<std::sync::mpsc::SyncSender<PostLookup>> }
impl Default for DnsProbe {
    fn default() -> Self { Self { deadline: PROBE_DEADLINE, last: None, worker: post_lookup_worker() } }
}
impl DnsProbe {
    /// The daemon's constructor: `None` when the worker thread cannot be
    /// created (the caller then refuses to start instead of running a probe
    /// that can never resolve). [`Default`] keeps the lenient behaviour.
    pub fn start() -> Option<Self> { Self::with_worker(post_lookup_worker()) }

    fn with_worker(worker: Option<std::sync::mpsc::SyncSender<PostLookup>>) -> Option<Self> {
        let worker = worker?;
        Some(Self { deadline: PROBE_DEADLINE, last: None, worker: Some(worker) })
    }

    /// A shorter deadline (tests); never longer than [`PROBE_DEADLINE`].
    pub fn with_deadline(deadline: Duration) -> Self { Self { deadline: deadline.min(PROBE_DEADLINE), ..Self::default() } }

    pub(crate) fn deadline_at(&self, taken: Instant) -> tokio::time::Instant { (taken + self.deadline).into() }

    /// One absolute deadline includes the reply. Only an expiry response gets
    /// grace; any failed send propagates to the controller's disconnect path.
    pub(crate) async fn answer_and_send<F, Fut>(&mut self, name: &str, route: Option<DnsRoute>, resolver: &dyn Resolver,
        taken: Instant, deadline: tokio::time::Instant, own: OwnAddresses, mut send: F) -> Result<(), EgressError>
    where F: FnMut(EgressControlResponse) -> Fut, Fut: std::future::Future<Output = Result<(), EgressError>> {
        let result = tokio::time::timeout_at(deadline, async {
            let reply = self.answer_inner(name, route, resolver, taken, own).await;
            // timeout_at polls its inner future first, even if already expired.
            if tokio::time::Instant::now() >= deadline { std::future::pending::<()>().await; }
            send(reply).await
        }).await;
        match result {
            Ok(result) => result,
            Err(_) => {
                if let Some(last) = self.last.as_mut().filter(|last| last.at == taken && last.name == name && Some(last.route) == route) {
                    last.resolved = false;
                    last.public_addresses = 0;
                    // Best-effort timeout record on the same bounded worker;
                    // no stderr write or wait on the serial control path.
                    if let Some(worker) = &self.worker {
                        let _ = worker.try_send(PostLookup { name: name.to_owned(), route: last.route,
                            answers: Err(EgressError::Timeout), own, reply: None });
                    }
                }
                let reply = match route {
                    Some(route) => EgressControlResponse::DnsProbed { route, resolved: false, public_addresses: 0 },
                    None => EgressControlResponse::Refused { code: EgressError::Unavailable },
                };
                let grace_deadline = deadline + PROBE_REPLY_GRACE;
                if tokio::time::Instant::now() >= grace_deadline { return Err(EgressError::Timeout); }
                tokio::time::timeout_at(grace_deadline, send(reply)).await.map_err(|_| EgressError::Timeout)?
            }
        }
    }

    #[cfg(test)]
    async fn answer(&mut self, name: &str, route: Option<DnsRoute>, resolver: &dyn Resolver, now: Instant,
        own: OwnAddresses) -> EgressControlResponse {
        let mut reply = None;
        let deadline = self.deadline_at(Instant::now());
        self.answer_and_send(name, route, resolver, now, deadline, own, |answer| {
            reply = Some(answer);
            std::future::ready(Ok(()))
        }).await.unwrap();
        reply.unwrap()
    }

    /// Answer one `ProbeDns { name }` at `now`. An invalid name or no single
    /// configured route is refused; within [`PROBE_INTERVAL`] of the last real
    /// lookup the cached result is returned for the same name and any other
    /// name is refused (`ResourceLimit`), without a lookup. Otherwise one
    /// lookup through `route` bounded by the deadline: an error, timeout or
    /// empty answer is `resolved: false, public_addresses: 0`. `own` is the
    /// host's own address set the run-time filter uses
    /// ([`crate::policy::own_addresses`]); when it is unavailable no answer counts.
    async fn answer_inner(&mut self, name: &str, route: Option<DnsRoute>, resolver: &dyn Resolver, now: Instant,
        own: OwnAddresses) -> EgressControlResponse {
        if validate_dns_probe_name(name).is_err() { return EgressControlResponse::Refused { code: EgressError::InvalidRequest }; }
        let Some(route) = route else { return EgressControlResponse::Refused { code: EgressError::Unavailable } };
        if let Some(last) = &self.last {
            // A clock reading before the last lookup counts as inside the window.
            if now.saturating_duration_since(last.at) < PROBE_INTERVAL {
                return if last.name == name && last.route == route {
                    EgressControlResponse::DnsProbed { route: last.route, resolved: last.resolved, public_addresses: last.public_addresses }
                } else { EgressControlResponse::Refused { code: EgressError::ResourceLimit } };
            }
        }
        // Reserve before the first await: cancellation must not reset the limit.
        self.last = Some(Last { at: now, name: name.to_owned(), route, resolved: false, public_addresses: 0 });
        let answers = resolver.resolve(name, route).await;
        let (reply, received) = tokio::sync::oneshot::channel();
        let job = PostLookup { name: name.to_owned(), route, answers, own, reply: Some(reply) };
        let (resolved, public_addresses) = if self.worker.as_ref().is_some_and(|worker| worker.try_send(job).is_ok()) {
            received.await.unwrap_or((false, 0))
        } else { (false, 0) };
        self.last = Some(Last { at: now, name: name.to_owned(), route, resolved, public_addresses });
        EgressControlResponse::DnsProbed { route, resolved, public_addresses }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, pin::Pin, sync::{atomic::{AtomicUsize, Ordering}, Mutex}};

    /// Scripted resolver: the queued results in order (`None`: never answers),
    /// and the names and routes it was asked for.
    struct Fake { results: Mutex<Vec<Option<Result<Vec<IpAddr>, EgressError>>>>, calls: AtomicUsize,
        asked: Mutex<Vec<(String, DnsRoute)>> }
    impl Fake {
        fn new(results: Vec<Option<Result<Vec<IpAddr>, EgressError>>>) -> Self {
            Self { results: Mutex::new(results), calls: AtomicUsize::new(0), asked: Mutex::new(Vec::new()) }
        }
    }
    impl Resolver for Fake {
        fn resolve<'a>(&'a self, host: &'a str, route: DnsRoute)
            -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, EgressError>> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.asked.lock().unwrap().push((host.to_owned(), route));
            let next = self.results.lock().unwrap().remove(0);
            Box::pin(async move {
                match next { Some(result) => result, None => std::future::pending().await }
            })
        }
    }
    fn ip(text: &str) -> IpAddr { text.parse().unwrap() }
    fn own() -> Result<BTreeSet<IpAddr>, EgressError> { Ok([ip("203.0.113.200"), ip("8.8.4.4")].into_iter().collect()) }
    fn no_own() -> Result<BTreeSet<IpAddr>, EgressError> { Err(EgressError::Unavailable) }
    fn probed(route: DnsRoute, resolved: bool, public_addresses: u32) -> EgressControlResponse {
        EgressControlResponse::DnsProbed { route, resolved, public_addresses }
    }
    const ROUTE: Option<DnsRoute> = Some(DnsRoute::ResolvedVarlink);

    #[tokio::test]
    async fn probe_counts_only_answers_that_pass_the_run_time_filter() {
        // 2 public + 3 private/special + 1 of the host's own public addresses.
        let answers = ["8.8.8.8", "2606:4700::1111", "10.0.0.1", "127.0.0.1", "fd00::1", "8.8.4.4"].map(ip).to_vec();
        let fake = Fake::new(vec![Some(Ok(answers.clone())), Some(Ok(answers))]);
        let mut probe = DnsProbe::default();
        let now = Instant::now();
        assert_eq!(probe.answer("example.com", ROUTE, &fake, now, own).await, probed(DnsRoute::ResolvedVarlink, true, 2));
        assert_eq!(fake.asked.lock().unwrap().as_slice(), [("example.com".to_owned(), DnsRoute::ResolvedVarlink)]);
        // The route actually used is reported.
        let broker = Fake::new(vec![Some(Ok(vec![ip("1.1.1.1")]))]);
        assert_eq!(DnsProbe::default().answer("example.com", Some(DnsRoute::DedicatedDnsBroker), &broker, now, own).await,
            probed(DnsRoute::DedicatedDnsBroker, true, 1));
        assert_eq!(broker.asked.lock().unwrap()[0].1, DnsRoute::DedicatedDnsBroker);
        // No own-address set: resolved, but nothing counts (fail closed).
        let mut later = DnsProbe::default();
        assert_eq!(later.answer("example.com", ROUTE, &fake, now, no_own).await, probed(DnsRoute::ResolvedVarlink, true, 0));
    }

    #[tokio::test]
    async fn probe_failure_and_timeout_are_unresolved_answers() {
        let now = Instant::now();
        for result in [Some(Err(EgressError::Unavailable)), Some(Err(EgressError::Timeout)), Some(Ok(vec![]))] {
            let fake = Fake::new(vec![result]);
            assert_eq!(DnsProbe::default().answer("does-not-exist.example.com", ROUTE, &fake, now, own).await,
                probed(DnsRoute::ResolvedVarlink, false, 0));
        }
        // A lookup that never answers is cut off by the deadline.
        let fake = Fake::new(vec![None]);
        let started = Instant::now();
        assert_eq!(DnsProbe::with_deadline(Duration::from_millis(100)).answer("example.com", ROUTE, &fake, now, own).await,
            probed(DnsRoute::ResolvedVarlink, false, 0));
        assert!(started.elapsed() < Duration::from_secs(2));
        // The deadline can only be narrowed.
        assert_eq!(DnsProbe::with_deadline(Duration::from_secs(60)).deadline, PROBE_DEADLINE);
        assert_eq!(DnsProbe::default().deadline, PROBE_DEADLINE);
    }

    #[tokio::test]
    async fn probe_pending_resolver_meets_deadline_and_records_attempt() {
        let fake = Fake::new(vec![None]);
        let mut probe = DnsProbe::with_deadline(Duration::from_millis(100));
        let taken = Instant::now();
        let expected = probed(DnsRoute::ResolvedVarlink, false, 0);
        assert_eq!(probe.answer("example.com", ROUTE, &fake, taken, own).await, expected);
        assert!(taken.elapsed() < Duration::from_millis(100) + PROBE_REPLY_GRACE);
        assert_eq!(probe.last.as_ref().unwrap().at, taken);
        assert_eq!(probe.answer("example.com", ROUTE, &fake, taken, own).await, expected);
        assert_eq!(probe.answer("other.example.com", ROUTE, &fake, taken, own).await,
            EgressControlResponse::Refused { code: EgressError::ResourceLimit });
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn probe_blocking_post_lookup_meets_deadline_and_records_attempt() {
        static ENTERED: AtomicUsize = AtomicUsize::new(0);
        fn blocking_own() -> Result<BTreeSet<IpAddr>, EgressError> {
            ENTERED.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(1));
            own()
        }
        let fake = Fake::new(vec![Some(Ok(vec![ip("8.8.8.8")]))]);
        let mut probe = DnsProbe::with_deadline(Duration::from_millis(100));
        let taken = Instant::now();
        let expected = probed(DnsRoute::ResolvedVarlink, false, 0);
        assert_eq!(probe.answer("example.com", ROUTE, &fake, taken, blocking_own).await, expected);
        assert_eq!(ENTERED.load(Ordering::SeqCst), 1);
        assert!(taken.elapsed() < Duration::from_millis(100) + PROBE_REPLY_GRACE);
        assert_eq!(probe.last.as_ref().unwrap().at, taken);
        assert_eq!(probe.answer("example.com", ROUTE, &fake, taken, own).await, expected);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn probe_reply_send_shares_deadline_and_uses_unresolved_grace() {
        let fake = Fake::new(vec![Some(Ok(vec![ip("8.8.8.8")]))]);
        let mut probe = DnsProbe::with_deadline(Duration::from_millis(100));
        let taken = Instant::now();
        let deadline = probe.deadline_at(taken);
        let mut replies = Vec::new();
        probe.answer_and_send("example.com", ROUTE, &fake, taken, deadline, own, |reply| {
            replies.push(reply);
            let first = replies.len() == 1;
            async move { if first { std::future::pending::<()>().await; } Ok(()) }
        }).await.unwrap();
        assert_eq!(replies, vec![probed(DnsRoute::ResolvedVarlink, true, 1), probed(DnsRoute::ResolvedVarlink, false, 0)]);
        assert!(taken.elapsed() < Duration::from_millis(100) + PROBE_REPLY_GRACE);
        assert!(!probe.last.as_ref().unwrap().resolved);
    }

    #[tokio::test]
    async fn probe_blocked_reply_exhausts_only_absolute_grace() {
        let fake = Fake::new(vec![None]);
        let mut probe = DnsProbe::with_deadline(Duration::from_millis(100));
        let taken = Instant::now();
        let deadline = probe.deadline_at(taken);
        let result = probe.answer_and_send("example.com", ROUTE, &fake, taken, deadline, own,
            |_| std::future::pending::<Result<(), EgressError>>()).await;
        assert_eq!(result, Err(EgressError::Timeout));
        assert!(taken.elapsed() >= Duration::from_millis(100) + PROBE_REPLY_GRACE);
        // Scheduling tolerance only; there is no fresh 5 s send timeout.
        assert!(taken.elapsed() < Duration::from_millis(500));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert!(!probe.last.as_ref().unwrap().resolved);
    }

    #[tokio::test]
    async fn probe_refuses_invalid_names_and_ambiguous_routes_without_a_lookup() {
        let fake = Fake::new(vec![]);
        let mut probe = DnsProbe::default();
        for name in ["", "localhost", "Example.com", "example.com.", "10.0.0.1", "printer.local", "a.internal", "x.localhost"] {
            assert_eq!(probe.answer(name, ROUTE, &fake, Instant::now(), own).await,
                EgressControlResponse::Refused { code: EgressError::InvalidRequest }, "{name}");
        }
        assert_eq!(probe.answer("example.com", None, &fake, Instant::now(), own).await,
            EgressControlResponse::Refused { code: EgressError::Unavailable });
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        // Refusals never consume the rate-limit window.
        assert!(probe.last.is_none());
    }

    #[tokio::test]
    async fn probe_is_rate_limited_to_one_real_lookup_per_window() {
        let fake = Fake::new(vec![Some(Ok(vec![ip("8.8.8.8")])), Some(Err(EgressError::Unavailable))]);
        let mut probe = DnsProbe::default();
        let start = Instant::now();
        let first = probe.answer("example.com", ROUTE, &fake, start, own).await;
        assert_eq!(first, probed(DnsRoute::ResolvedVarlink, true, 1));
        // Inside the window: the cached result, no second lookup; another
        // name or another route is refused; an earlier clock is inside too.
        for offset in [Duration::ZERO, Duration::from_secs(1), PROBE_INTERVAL - Duration::from_millis(1)] {
            assert_eq!(probe.answer("example.com", ROUTE, &fake, start + offset, own).await, first);
        }
        assert_eq!(probe.answer("other.example.com", ROUTE, &fake, start, own).await,
            EgressControlResponse::Refused { code: EgressError::ResourceLimit });
        assert_eq!(probe.answer("example.com", Some(DnsRoute::DedicatedDnsBroker), &fake, start, own).await,
            EgressControlResponse::Refused { code: EgressError::ResourceLimit });
        if let Some(earlier) = start.checked_sub(Duration::from_secs(5)) {
            assert_eq!(probe.answer("example.com", ROUTE, &fake, earlier, own).await, first);
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        // After the window: one real lookup, whose (failed) result is cached in turn.
        let next = start + PROBE_INTERVAL;
        assert_eq!(probe.answer("example.com", ROUTE, &fake, next, own).await, probed(DnsRoute::ResolvedVarlink, false, 0));
        assert_eq!(probe.answer("example.com", ROUTE, &fake, next + Duration::from_secs(9), own).await,
            probed(DnsRoute::ResolvedVarlink, false, 0));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    }

    /// Real-host defect 28: the unit's allow-list lacked clone/clone3, the
    /// worker spawn failed silently and every probe answered (false, 0).
    #[test]
    fn unit_admits_the_probe_worker_spawn_before_the_in_process_filter() {
        const UNIT: &str = include_str!("../systemd/duduclaw-egress.service");
        let filters: Vec<&str> = UNIT.lines().filter_map(|line| line.strip_prefix("SystemCallFilter=")).collect();
        assert_eq!(filters.len(), 1, "exactly one allow-list line");
        let allowed: BTreeSet<&str> = filters[0].split_whitespace().collect();
        assert!(!allowed.iter().any(|call| call.starts_with('~') || call.starts_with('@')));
        for call in WORKER_START_SYSCALLS { assert!(allowed.contains(call), "unit SystemCallFilter lacks {call}"); }
        assert!(UNIT.lines().any(|line| line == "SystemCallErrorNumber=EPERM"));
        assert!(UNIT.lines().any(|line| line.starts_with("TasksMax=")));
    }

    #[test]
    fn probe_start_requires_its_worker() {
        assert!(DnsProbe::with_worker(None).is_none());
        let probe = DnsProbe::start().expect("worker thread starts in an unrestricted test process");
        assert!(probe.worker.is_some());
        assert_eq!(probe.deadline, PROBE_DEADLINE);
        assert!(DnsProbe::default().worker.is_some());
        assert_eq!(WORKER_UNAVAILABLE, "duduclaw-egress: dns probe worker unavailable");
    }

    #[test]
    fn probe_route_is_the_single_configured_route() {
        use duduclaw_isolation::{bounded::List, egress::ProfileId, ids::Sha256Digest};
        let profile = |seed: u8, dns_route| EgressProfile { id: ProfileId(Sha256Digest([seed; 32])),
            allowed_connect_ports: List::new(vec![443.try_into().unwrap()]).unwrap(), dns_route,
            limits: crate::budget::default_limits() };
        assert_eq!(configured_route(&[]), None);
        assert_eq!(configured_route(&[profile(1, DnsRoute::DedicatedDnsBroker), profile(2, DnsRoute::DedicatedDnsBroker)]),
            Some(DnsRoute::DedicatedDnsBroker));
        assert_eq!(configured_route(&[profile(1, DnsRoute::ResolvedVarlink), profile(2, DnsRoute::DedicatedDnsBroker)]), None);
    }
}
