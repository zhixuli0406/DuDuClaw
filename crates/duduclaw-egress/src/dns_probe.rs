//! R7b: spawnd's `ProbeDns` control request. One real lookup of a name from the
//! signed policy through the configured route and the resolver runs use,
//! counted with the run-time public-address filter, so spawnd can report the
//! DNS route as verified from a live observation instead of a stored receipt.
//!
//! Bounded and rate limited: a lookup has a hard [`PROBE_DEADLINE`], and at
//! most one real lookup happens per [`PROBE_INTERVAL`] (within the window the
//! previous result for the same name is returned; another name is refused),
//! so the controller cannot turn egress into a query generator. Answers are
//! never logged or returned; only name, route, `resolved` and the count are.
use std::{collections::BTreeSet, net::IpAddr, time::{Duration, Instant}};
use duduclaw_isolation::egress::{validate_dns_probe_name, DnsRoute, EgressControlResponse, EgressError, EgressProfile};
use crate::resolver::Resolver;

/// Hard deadline of one probe lookup (socket connect, query and answer; the
/// resolver's own 5 s timeout is never reached). `serve_control` is serial and
/// awaits the probe inline, so a probe delays a following Register or
/// Unregister by at most this long.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(3);
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

/// Per-controller probe state: the last real lookup and its time.
pub struct DnsProbe { deadline: Duration, last: Option<Last> }
impl Default for DnsProbe {
    fn default() -> Self { Self { deadline: PROBE_DEADLINE, last: None } }
}
impl DnsProbe {
    /// A shorter deadline (tests); never longer than [`PROBE_DEADLINE`].
    pub fn with_deadline(deadline: Duration) -> Self { Self { deadline: deadline.min(PROBE_DEADLINE), last: None } }

    /// Answer one `ProbeDns { name }` at `now`. An invalid name or no single
    /// configured route is refused; within [`PROBE_INTERVAL`] of the last real
    /// lookup the cached result is returned for the same name and any other
    /// name is refused (`ResourceLimit`), without a lookup. Otherwise one
    /// lookup through `route` bounded by the deadline: an error, timeout or
    /// empty answer is `resolved: false, public_addresses: 0`. `own` is the
    /// host's own address set the run-time filter uses
    /// ([`crate::policy::own_addresses`]); when it is unavailable no answer counts.
    pub async fn answer(&mut self, name: &str, route: Option<DnsRoute>, resolver: &dyn Resolver, now: Instant,
        own: &(dyn Fn() -> Result<BTreeSet<IpAddr>, EgressError> + Sync)) -> EgressControlResponse {
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
        let lookup = tokio::time::timeout(self.deadline, resolver.resolve(name, route)).await;
        let (resolved, public_addresses) = match lookup {
            Ok(Ok(answers)) if !answers.is_empty() => {
                let count = own().map(|own| crate::policy::public_answer_count(&answers, &own)).unwrap_or(0);
                (true, count)
            }
            _ => (false, 0),
        };
        self.last = Some(Last { at: now, name: name.to_owned(), route, resolved, public_addresses });
        // Never the answers themselves.
        eprintln!("{}", serde_json::json!({ "event": "dns_probe", "name": name, "route": route,
            "resolved": resolved, "public_addresses": public_addresses }));
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
        assert_eq!(probe.answer("example.com", ROUTE, &fake, now, &own).await, probed(DnsRoute::ResolvedVarlink, true, 2));
        assert_eq!(fake.asked.lock().unwrap().as_slice(), [("example.com".to_owned(), DnsRoute::ResolvedVarlink)]);
        // The route actually used is reported.
        let broker = Fake::new(vec![Some(Ok(vec![ip("1.1.1.1")]))]);
        assert_eq!(DnsProbe::default().answer("example.com", Some(DnsRoute::DedicatedDnsBroker), &broker, now, &own).await,
            probed(DnsRoute::DedicatedDnsBroker, true, 1));
        assert_eq!(broker.asked.lock().unwrap()[0].1, DnsRoute::DedicatedDnsBroker);
        // No own-address set: resolved, but nothing counts (fail closed).
        let mut later = DnsProbe::default();
        assert_eq!(later.answer("example.com", ROUTE, &fake, now, &no_own).await, probed(DnsRoute::ResolvedVarlink, true, 0));
    }

    #[tokio::test]
    async fn probe_failure_and_timeout_are_unresolved_answers() {
        let now = Instant::now();
        for result in [Some(Err(EgressError::Unavailable)), Some(Err(EgressError::Timeout)), Some(Ok(vec![]))] {
            let fake = Fake::new(vec![result]);
            assert_eq!(DnsProbe::default().answer("does-not-exist.example.com", ROUTE, &fake, now, &own).await,
                probed(DnsRoute::ResolvedVarlink, false, 0));
        }
        // A lookup that never answers is cut off by the deadline.
        let fake = Fake::new(vec![None]);
        let started = Instant::now();
        assert_eq!(DnsProbe::with_deadline(Duration::from_millis(100)).answer("example.com", ROUTE, &fake, now, &own).await,
            probed(DnsRoute::ResolvedVarlink, false, 0));
        assert!(started.elapsed() < Duration::from_secs(2));
        // The deadline can only be narrowed.
        assert_eq!(DnsProbe::with_deadline(Duration::from_secs(60)).deadline, PROBE_DEADLINE);
        assert_eq!(DnsProbe::default().deadline, PROBE_DEADLINE);
    }

    #[tokio::test]
    async fn probe_refuses_invalid_names_and_ambiguous_routes_without_a_lookup() {
        let fake = Fake::new(vec![]);
        let mut probe = DnsProbe::default();
        for name in ["", "localhost", "Example.com", "example.com.", "10.0.0.1", "printer.local", "a.internal", "x.localhost"] {
            assert_eq!(probe.answer(name, ROUTE, &fake, Instant::now(), &own).await,
                EgressControlResponse::Refused { code: EgressError::InvalidRequest }, "{name}");
        }
        assert_eq!(probe.answer("example.com", None, &fake, Instant::now(), &own).await,
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
        let first = probe.answer("example.com", ROUTE, &fake, start, &own).await;
        assert_eq!(first, probed(DnsRoute::ResolvedVarlink, true, 1));
        // Inside the window: the cached result, no second lookup; another
        // name or another route is refused; an earlier clock is inside too.
        for offset in [Duration::ZERO, Duration::from_secs(1), PROBE_INTERVAL - Duration::from_millis(1)] {
            assert_eq!(probe.answer("example.com", ROUTE, &fake, start + offset, &own).await, first);
        }
        assert_eq!(probe.answer("other.example.com", ROUTE, &fake, start, &own).await,
            EgressControlResponse::Refused { code: EgressError::ResourceLimit });
        assert_eq!(probe.answer("example.com", Some(DnsRoute::DedicatedDnsBroker), &fake, start, &own).await,
            EgressControlResponse::Refused { code: EgressError::ResourceLimit });
        if let Some(earlier) = start.checked_sub(Duration::from_secs(5)) {
            assert_eq!(probe.answer("example.com", ROUTE, &fake, earlier, &own).await, first);
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        // After the window: one real lookup, whose (failed) result is cached in turn.
        let next = start + PROBE_INTERVAL;
        assert_eq!(probe.answer("example.com", ROUTE, &fake, next, &own).await, probed(DnsRoute::ResolvedVarlink, false, 0));
        assert_eq!(probe.answer("example.com", ROUTE, &fake, next + Duration::from_secs(9), &own).await,
            probed(DnsRoute::ResolvedVarlink, false, 0));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
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
