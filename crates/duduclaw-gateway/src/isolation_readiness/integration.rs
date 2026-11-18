//! Host-only integration proofs. Registration is immutable; observations expire.
//! No configuration, request argument or unit-active flag can set readiness.
use std::{sync::{Arc, Mutex, OnceLock}, time::{Duration, Instant}};
use duduclaw_isolation::ids::Sha256Digest;
use super::ComponentState;

const MAX_OBSERVATION_AGE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrationComponent { ParseWorker, McpBoundary, RuntimeAdapters, PtyTransport }

/// Derived by the runtime package from its complete, authenticated runtime
/// publication. Unknown or unreadable configuration is not `NotRequired`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtyRequirement { Required, NotRequired }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Unavailable,
    AlreadyInstalled,
    /// R8n-1: not ready for one fixed reason from the component's reason
    /// table (`published::PARSE_WORKER_REASONS` / `RUNTIME_ADAPTER_REASONS`),
    /// published as `component(reason)`. Never a path or an OS error text.
    Refused(&'static str),
}
impl ProbeError {
    pub fn reason(self) -> Option<&'static str> {
        match self { Self::Refused(reason) => Some(reason), Self::Unavailable | Self::AlreadyInstalled => None }
    }
}

/// The timestamp is captured BEFORE authenticated service readback, never when
/// a cached result is returned. This type has no wire or configuration decoder.
#[derive(Clone)]
pub struct ServiceObservation {
    generation: Sha256Digest,
    observed_at: Instant,
    pty: Option<PtyRequirement>,
}
impl ServiceObservation {
    pub fn checked(generation: Sha256Digest, observed_at: Instant) -> Self {
        Self { generation, observed_at, pty: None }
    }

    /// Runtime adapters must declare PTY requirements from the same verified
    /// publication as their protocol evidence. A generic observation does not
    /// prove the absence of PTY runtimes. No caller/config flag can opt out.
    pub fn checked_runtime(generation: Sha256Digest, observed_at: Instant, pty: PtyRequirement) -> Self {
        Self { generation, observed_at, pty: Some(pty) }
    }
}

/// A bounded background observer can publish authenticated readback without
/// doing service I/O on an admission thread. This cache never proves producer
/// completeness; the owning `IntegrationProbe` must still verify that itself.
/// Recording a failure clears prior evidence, and reads preserve its timestamp.
#[derive(Default)]
pub struct ServiceObservationCache { value: Mutex<Option<ServiceObservation>> }
impl ServiceObservationCache {
    pub fn record(&self, result: Result<ServiceObservation, ProbeError>) -> Result<(), ProbeError> {
        let mut value = self.value.lock().map_err(|_| ProbeError::Unavailable)?;
        *value = None;
        let observation = result?;
        if observation.generation.0 == [0; 32] || !fresh(observation.observed_at, Instant::now()) {
            return Err(ProbeError::Unavailable);
        }
        *value = Some(observation);
        Ok(())
    }

    pub fn read(&self, expected_generation: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
        let value = self.value.lock().map_err(|_| ProbeError::Unavailable)?;
        let observation = value.as_ref().ok_or(ProbeError::Unavailable)?;
        if observation.generation != expected_generation || !fresh(observation.observed_at, Instant::now()) {
            return Err(ProbeError::Unavailable);
        }
        Ok(observation.clone())
    }
}

/// Implemented only by the package that owns the actual installed producers.
/// Both methods run on every admission/diagnostic read and must be bounded.
/// Slow service I/O belongs in a background probe; its original observation
/// timestamp must survive caching. A failed observation must clear that cache.
pub trait IntegrationProbe: Send + Sync {
    /// Verify the complete production inventory, not merely object existence:
    /// E checks all worker operation routes and retirement-aware launch support;
    /// D checks original dispatcher completeness and live authority/ingress;
    /// runtime adapters check every configured runtime and exact supported input
    /// protocol, including signed configuration and required PTY support.
    fn verify_producers(&self) -> Result<(), ProbeError>;

    /// Read authenticated evidence for the expected generation. E checks worker
    /// service identity, containment and retirement; D checks its boundary and
    /// backend/control ownership; runtime adapters check the root publication
    /// and accepted CLI/bridge policy, returning `checked_runtime` with its PTY
    /// requirement. The separate PTY source checks allocation, fd transport,
    /// resize, cancellation and retirement. Construction or `active` alone is not a
    /// successful readback. Do not call the composite gate recursively here.
    fn verify_service(&self, expected_generation: Sha256Digest) -> Result<ServiceObservation, ProbeError>;
}

#[derive(Default)]
struct Probes {
    worker: OnceLock<Arc<dyn IntegrationProbe>>,
    mcp: OnceLock<Arc<dyn IntegrationProbe>>,
    runtime: OnceLock<Arc<dyn IntegrationProbe>>,
    pty: OnceLock<Arc<dyn IntegrationProbe>>,
}
impl Probes {
    fn slot(&self, component: IntegrationComponent) -> &OnceLock<Arc<dyn IntegrationProbe>> {
        match component {
            IntegrationComponent::ParseWorker => &self.worker,
            IntegrationComponent::McpBoundary => &self.mcp,
            IntegrationComponent::RuntimeAdapters => &self.runtime,
            IntegrationComponent::PtyTransport => &self.pty,
        }
    }
    fn install(&self, component: IntegrationComponent, probe: Arc<dyn IntegrationProbe>) -> Result<(), ProbeError> {
        self.slot(component).set(probe).map_err(|_| ProbeError::AlreadyInstalled)
    }
    fn current(&self, generation: Option<Sha256Digest>) -> States {
        let started = Instant::now();
        let worker = observation(self.worker.get(), generation, Instant::now);
        let mcp = observation(self.mcp.get(), generation, Instant::now);
        let runtime = observation(self.runtime.get(), generation, Instant::now);
        let pty = if runtime.pty == Some(PtyRequirement::Required) {
            Some(observation(self.pty.get(), generation, Instant::now))
        } else { None };
        let finished = Instant::now();
        let (worker_reason, runtime_reason) = (worker.reason, runtime.reason);
        // A later probe may consume the lifetime of an earlier observation.
        // Validate every required source against the same final clock reading.
        let parse_worker = final_state(worker, started, finished);
        let runtime_adapters = runtime_state(runtime, pty, started, finished);
        let reason = |state: ComponentState, reason: Option<&'static str>|
            reason.filter(|_| state == ComponentState::Unavailable);
        States {
            parse_worker, mcp_boundary: final_state(mcp, started, finished), runtime_adapters,
            parse_worker_reason: reason(parse_worker, worker_reason),
            runtime_adapters_reason: reason(runtime_adapters, runtime_reason),
        }
    }
}
static PROBES: Probes = Probes {
    worker: OnceLock::new(), mcp: OnceLock::new(), runtime: OnceLock::new(), pty: OnceLock::new(),
};

/// Trusted boot composition only. A package can independently register its
/// proof once; another package cannot replace it or clear a refusal.
pub fn install_integration_probe(component: IntegrationComponent, probe: Arc<dyn IntegrationProbe>)
    -> Result<(), ProbeError> {
    PROBES.install(component, probe)
}

pub(super) struct States {
    pub parse_worker: ComponentState,
    pub mcp_boundary: ComponentState,
    pub runtime_adapters: ComponentState,
    /// R8n-1: the fixed reason of a refused ParseWorker / RuntimeAdapters
    /// probe (`ProbeError::Refused`), only while that component is unavailable.
    pub parse_worker_reason: Option<&'static str>,
    pub runtime_adapters_reason: Option<&'static str>,
}
impl States {
    pub(super) fn invalidate(&mut self) {
        for state in [&mut self.parse_worker, &mut self.mcp_boundary, &mut self.runtime_adapters] {
            if *state == ComponentState::Ready { *state = ComponentState::Unavailable; }
        }
    }
}
pub(super) fn current(generation: Option<Sha256Digest>) -> States { PROBES.current(generation) }

/// The states a fresh registry holding exactly `probes` reports (tests of the
/// composite admission over faked probes; production uses [`PROBES`]).
#[cfg(test)]
pub(super) fn states_with(probes: Vec<(IntegrationComponent, Arc<dyn IntegrationProbe>)>,
    generation: Option<Sha256Digest>) -> States {
    let registry = Probes::default();
    for (component, probe) in probes { registry.install(component, probe).expect("one probe per component"); }
    registry.current(generation)
}

struct Observation {
    state: ComponentState,
    observed_at: Option<Instant>,
    pty: Option<PtyRequirement>,
    /// R8n-1: the probe's fixed refusal reason, if it gave one.
    reason: Option<&'static str>,
}
impl Observation {
    fn absent(state: ComponentState) -> Self { Self { state, observed_at: None, pty: None, reason: None } }
    fn refused(error: ProbeError) -> Self { Self { reason: error.reason(), ..Self::absent(ComponentState::Unavailable) } }
}

/// No probe ⇒ `PendingIntegration`, never ready; any error ⇒ `Unavailable`.
fn observation(probe: Option<&Arc<dyn IntegrationProbe>>, generation: Option<Sha256Digest>, now: impl Fn() -> Instant)
    -> Observation {
    let unavailable = Observation::absent(ComponentState::Unavailable);
    let Some(probe) = probe else { return Observation::absent(ComponentState::PendingIntegration); };
    let Some(generation) = generation.filter(|value| value.0 != [0; 32]) else {
        return unavailable;
    };
    let started = now();
    if let Err(error) = probe.verify_producers() { return Observation::refused(error); }
    let observation = match probe.verify_service(generation) {
        Ok(observation) => observation,
        Err(error) => return Observation::refused(error),
    };
    let finished = now();
    if observation.generation != generation
        || !fresh(started, finished) || !fresh(observation.observed_at, finished) {
        return unavailable;
    }
    Observation { state: ComponentState::Ready, observed_at: Some(observation.observed_at), pty: observation.pty, reason: None }
}

fn fresh(observed: Instant, now: Instant) -> bool {
    now.checked_duration_since(observed).is_some_and(|age| age <= MAX_OBSERVATION_AGE)
}

fn final_state(Observation { state, observed_at, .. }: Observation, started: Instant, finished: Instant)
    -> ComponentState {
    if state == ComponentState::Ready && (!fresh(started, finished)
        || observed_at.is_none_or(|at| !fresh(at, finished))) {
        ComponentState::Unavailable
    } else { state }
}

fn runtime_state(runtime: Observation, pty: Option<Observation>, started: Instant, finished: Instant)
    -> ComponentState {
    let requirement = runtime.pty;
    let state = final_state(runtime, started, finished);
    if state != ComponentState::Ready { return state; }
    match requirement {
        Some(PtyRequirement::NotRequired) => ComponentState::Ready,
        Some(PtyRequirement::Required) => pty.map(|value| final_state(value, started, finished))
            .unwrap_or(ComponentState::PendingIntegration),
        None => ComponentState::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Probe {
        producers: AtomicBool, service: AtomicBool, calls: AtomicUsize,
        generation: Sha256Digest, observed: Instant, pty: Option<PtyRequirement>,
    }
    impl IntegrationProbe for Probe {
        fn verify_producers(&self) -> Result<(), ProbeError> {
            if self.producers.load(Ordering::Acquire) { Ok(()) } else { Err(ProbeError::Unavailable) }
        }
        fn verify_service(&self, _: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if !self.service.load(Ordering::Acquire) { return Err(ProbeError::Unavailable); }
            Ok(ServiceObservation { generation: self.generation, observed_at: self.observed, pty: self.pty })
        }
    }
    fn probe(generation: Sha256Digest, observed: Instant) -> Arc<Probe> {
        probe_with_pty(generation, observed, Some(PtyRequirement::NotRequired))
    }
    fn probe_with_pty(generation: Sha256Digest, observed: Instant, pty: Option<PtyRequirement>) -> Arc<Probe> {
        Arc::new(Probe { producers: AtomicBool::new(true), service: AtomicBool::new(true),
            calls: AtomicUsize::new(0), generation, observed, pty })
    }
    fn checked(probe: &Arc<Probe>, generation: Option<Sha256Digest>, now: Instant) -> ComponentState {
        let erased: Arc<dyn IntegrationProbe> = probe.clone();
        state(Some(&erased), generation, || now)
    }
    fn state(probe: Option<&Arc<dyn IntegrationProbe>>, generation: Option<Sha256Digest>, now: impl Fn() -> Instant)
        -> ComponentState { observation(probe, generation, now).state }

    #[test]
    fn integration_proofs_require_producers_fresh_service_and_matching_generation() {
        let now = Instant::now(); let generation = Sha256Digest([1; 32]);
        assert_eq!(state(None, Some(generation), || now), ComponentState::PendingIntegration);
        let good = probe(generation, now);
        assert_eq!(checked(&good, Some(generation), now), ComponentState::Ready);
        good.producers.store(false, Ordering::Release);
        assert_eq!(checked(&good, Some(generation), now), ComponentState::Unavailable);
        assert_eq!(good.calls.load(Ordering::Acquire), 1);
        good.producers.store(true, Ordering::Release);
        good.service.store(false, Ordering::Release);
        assert_eq!(checked(&good, Some(generation), now), ComponentState::Unavailable);
        good.service.store(true, Ordering::Release);
        assert_eq!(checked(&good, Some(Sha256Digest([2; 32])), now), ComponentState::Unavailable);
        assert_eq!(checked(&good, None, now), ComponentState::Unavailable);
        assert_eq!(checked(&good, Some(Sha256Digest([0; 32])), now), ComponentState::Unavailable);
    }

    #[test]
    fn integration_observations_expire_and_cannot_come_from_the_future() {
        let now = Instant::now(); let generation = Sha256Digest([1; 32]);
        let current = probe(generation, now);
        assert_eq!(checked(&current, Some(generation), now + Duration::from_secs(2)), ComponentState::Ready);
        assert_eq!(checked(&current, Some(generation), now + Duration::from_millis(2001)), ComponentState::Unavailable);
        assert_eq!(checked(&probe(generation, now + Duration::from_millis(1)), Some(generation), now),
            ComponentState::Unavailable);
        let erased: Arc<dyn IntegrationProbe> = current;
        let ticks = AtomicUsize::new(0);
        assert_eq!(state(Some(&erased), Some(generation), || {
            now + Duration::from_secs(3 * ticks.fetch_add(1, Ordering::AcqRel) as u64)
        }), ComponentState::Unavailable);
    }

    #[test]
    fn integration_registration_is_per_component_single_assignment() {
        let registry = Probes::default(); let generation = Sha256Digest([1; 32]);
        for component in [IntegrationComponent::ParseWorker, IntegrationComponent::McpBoundary,
            IntegrationComponent::RuntimeAdapters, IntegrationComponent::PtyTransport] {
            assert_eq!(registry.install(component, probe(generation, Instant::now())), Ok(()));
            assert_eq!(registry.install(component, probe(generation, Instant::now())), Err(ProbeError::AlreadyInstalled));
        }
        let states = registry.current(Some(generation));
        assert_eq!(states.parse_worker, ComponentState::Ready);
        assert_eq!(states.mcp_boundary, ComponentState::Ready);
        assert_eq!(states.runtime_adapters, ComponentState::Ready);
        let states = registry.current(None);
        assert_eq!(states.parse_worker, ComponentState::Unavailable);
        assert_eq!(states.mcp_boundary, ComponentState::Unavailable);
        assert_eq!(states.runtime_adapters, ComponentState::Unavailable);
    }

    #[test]
    fn integration_later_probe_cannot_keep_an_earlier_observation_alive() {
        let now = Instant::now();
        let observation = || Observation { state: ComponentState::Ready, observed_at: Some(now), pty: None, reason: None };
        let started = now + Duration::from_millis(1900);
        assert_eq!(final_state(observation(), started, now + Duration::from_secs(2)), ComponentState::Ready);
        assert_eq!(final_state(observation(), started, now + Duration::from_millis(2001)), ComponentState::Unavailable);
        assert_eq!(final_state(Observation::absent(ComponentState::PendingIntegration), started,
            now + Duration::from_secs(5)), ComponentState::PendingIntegration);
    }

    #[test]
    fn integration_missing_packages_and_generation_rotation_do_not_report_ready() {
        let registry = Probes::default(); let generation = Sha256Digest([1; 32]);
        registry.install(IntegrationComponent::McpBoundary, probe(generation, Instant::now())).unwrap();
        let mut states = registry.current(Some(generation));
        assert_eq!(states.parse_worker, ComponentState::PendingIntegration);
        assert_eq!(states.runtime_adapters, ComponentState::PendingIntegration);
        assert_eq!(states.mcp_boundary, ComponentState::Ready);
        states.invalidate();
        assert_eq!(states.parse_worker, ComponentState::PendingIntegration);
        assert_eq!(states.mcp_boundary, ComponentState::Unavailable);
        assert_eq!(states.runtime_adapters, ComponentState::PendingIntegration);
    }

    /// R8n-1: a probe's fixed refusal reason reaches the component state only
    /// while it is unavailable; no probe stays pending without a reason.
    #[test]
    fn integration_refusal_reasons_are_reported_for_unavailable_components_only() {
        struct Refusing(&'static str, bool);
        impl IntegrationProbe for Refusing {
            fn verify_producers(&self) -> Result<(), ProbeError> {
                if self.1 { Err(ProbeError::Refused(self.0)) } else { Ok(()) }
            }
            fn verify_service(&self, _: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
                Err(ProbeError::Refused(self.0))
            }
        }
        fn refusing(reason: &'static str, producers: bool) -> Arc<dyn IntegrationProbe> { Arc::new(Refusing(reason, producers)) }
        fn good(generation: Sha256Digest) -> Arc<dyn IntegrationProbe> { probe(generation, Instant::now()) }
        let generation = Sha256Digest([1; 32]);
        let states = states_with(vec![], Some(generation));
        assert_eq!((states.parse_worker, states.parse_worker_reason), (ComponentState::PendingIntegration, None));
        assert_eq!((states.runtime_adapters, states.runtime_adapters_reason), (ComponentState::PendingIntegration, None));
        for producers in [true, false] {
            let states = states_with(vec![
                (IntegrationComponent::ParseWorker, refusing("socket_unavailable", producers)),
                (IntegrationComponent::RuntimeAdapters, refusing("no_main_runtime", producers))], Some(generation));
            assert_eq!((states.parse_worker, states.parse_worker_reason),
                (ComponentState::Unavailable, Some("socket_unavailable")));
            assert_eq!((states.runtime_adapters, states.runtime_adapters_reason),
                (ComponentState::Unavailable, Some("no_main_runtime")));
        }
        // A generic failure and a ready probe carry no reason.
        let states = states_with(vec![(IntegrationComponent::ParseWorker, good(generation)),
            (IntegrationComponent::RuntimeAdapters, refusing("no_main_runtime", false))], None);
        assert_eq!((states.parse_worker, states.parse_worker_reason), (ComponentState::Unavailable, None));
        assert_eq!(states.runtime_adapters_reason, None);
        let states = states_with(vec![(IntegrationComponent::ParseWorker, good(generation))], Some(generation));
        assert_eq!((states.parse_worker, states.parse_worker_reason), (ComponentState::Ready, None));
        assert_eq!(ProbeError::Unavailable.reason(), None);
        assert_eq!(ProbeError::Refused("x").reason(), Some("x"));
    }

    #[test]
    fn integration_runtime_requires_an_explicit_authenticated_pty_requirement() {
        let generation = Sha256Digest([1; 32]);
        let registry = Probes::default();
        registry.install(IntegrationComponent::RuntimeAdapters,
            probe_with_pty(generation, Instant::now(), None)).unwrap();
        registry.install(IntegrationComponent::PtyTransport, probe(generation, Instant::now())).unwrap();
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::Unavailable);
        let registry = Probes::default();
        registry.install(IntegrationComponent::RuntimeAdapters, probe(generation, Instant::now())).unwrap();
        let failed_pty = probe(generation, Instant::now());
        failed_pty.producers.store(false, Ordering::Release);
        registry.install(IntegrationComponent::PtyTransport, failed_pty.clone()).unwrap();
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::Ready);
        assert_eq!(failed_pty.calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn integration_pty_packages_register_independently_and_cannot_mask_failures() {
        let generation = Sha256Digest([1; 32]);
        let registry = Probes::default();
        let runtime = probe_with_pty(generation, Instant::now(), Some(PtyRequirement::Required));
        registry.install(IntegrationComponent::RuntimeAdapters, runtime.clone()).unwrap();
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::PendingIntegration);
        let pty = probe(generation, Instant::now());
        registry.install(IntegrationComponent::PtyTransport, pty.clone()).unwrap();
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::Ready);
        pty.service.store(false, Ordering::Release);
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::Unavailable);
        pty.service.store(true, Ordering::Release);
        runtime.producers.store(false, Ordering::Release);
        assert_eq!(registry.current(Some(generation)).runtime_adapters, ComponentState::Unavailable);
        runtime.producers.store(true, Ordering::Release);
        assert_eq!(registry.current(Some(Sha256Digest([2; 32]))).runtime_adapters, ComponentState::Unavailable);
    }

    #[test]
    fn integration_pty_observation_cannot_outlive_earlier_runtime_proof() {
        let now = Instant::now();
        let runtime = Observation { state: ComponentState::Ready, observed_at: Some(now),
            pty: Some(PtyRequirement::Required), reason: None };
        let pty = Observation { state: ComponentState::Ready,
            observed_at: Some(now + Duration::from_millis(1900)), pty: None, reason: None };
        assert_eq!(runtime_state(runtime, Some(pty), now + Duration::from_millis(1900),
            now + Duration::from_millis(2001)), ComponentState::Unavailable);
    }

    #[test]
    fn integration_cached_service_evidence_expires_and_failure_clears_it() {
        let generation = Sha256Digest([1; 32]);
        let cache = ServiceObservationCache::default();
        assert!(cache.read(generation).is_err());
        let observed = Instant::now();
        cache.record(Ok(ServiceObservation::checked_runtime(generation, observed, PtyRequirement::Required))).unwrap();
        let readback = cache.read(generation).unwrap();
        assert_eq!(readback.observed_at, observed);
        assert_eq!(readback.pty, Some(PtyRequirement::Required));
        assert!(cache.read(Sha256Digest([2; 32])).is_err());
        assert!(cache.record(Err(ProbeError::Unavailable)).is_err());
        assert!(cache.read(generation).is_err());
        for (invalid_generation, invalid_time) in [
            (Sha256Digest([0; 32]), Instant::now()),
            (generation, Instant::now() + Duration::from_secs(3)),
            (generation, Instant::now() - Duration::from_secs(3)),
        ] {
            cache.record(Ok(ServiceObservation::checked(generation, Instant::now()))).unwrap();
            assert!(cache.record(Ok(ServiceObservation::checked(invalid_generation, invalid_time))).is_err());
            assert!(cache.read(generation).is_err());
        }
        *cache.value.lock().unwrap() = Some(ServiceObservation::checked(generation,
            Instant::now() - Duration::from_secs(3)));
        assert!(cache.read(generation).is_err());
        cache.record(Ok(ServiceObservation::checked(generation, Instant::now()))).unwrap();
        assert!(cache.read(generation).is_ok());
    }
}
