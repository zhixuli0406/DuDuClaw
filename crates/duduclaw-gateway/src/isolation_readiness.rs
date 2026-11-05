//! Composite protected-TEAM admission, shared by model and subprocess entry points.
//! Missing factories/readbacks deny execution while the dashboard remains available.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use serde::Serialize;

#[path = "isolation_readiness/integration.rs"]
mod integration;
pub use integration::{IntegrationComponent, IntegrationProbe, ProbeError, PtyRequirement,
    ServiceObservation, ServiceObservationCache, install_integration_probe};

static REQUIRED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState { Ready, Unavailable, PendingIntegration }

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Components {
    pub launch_service: ComponentState,
    pub sandbox: ComponentState,
    pub network: ComponentState,
    pub storage: ComponentState,
    pub parse_worker: ComponentState,
    pub update_service: ComponentState,
    pub mcp_boundary: ComponentState,
    pub runtime_adapters: ComponentState,
}

impl Components {
    fn all_ready(&self) -> bool {
        [self.launch_service, self.sandbox, self.network, self.storage,
            self.parse_worker, self.update_service, self.mcp_boundary,
            self.runtime_adapters].into_iter().all(|state| state == ComponentState::Ready)
    }

    fn current(host_was_verified: bool) -> Self {
        let generation = current_generation();
        let mut integration = integration::current(generation);
        // A generation rotation during a probe cannot combine old adapters
        // with the new updater observation. Boot must compose them again.
        let current = current_generation();
        if current != generation { integration.invalidate(); }
        // Integration probes can consume B1's sub-second receipt lifetime.
        // Re-read at final admission instead of retaining an earlier bool.
        let host = if host_was_verified && spawnd_verified() { ComponentState::Ready }
            else { ComponentState::Unavailable };
        Self { launch_service: host, sandbox: host, network: host, storage: host,
            parse_worker: combined_worker_state(integration.parse_worker, parse_worker_state()),
            update_service: if generation.is_some() && current == generation {
                ComponentState::Ready
            } else { ComponentState::Unavailable },
            mcp_boundary: integration.mcp_boundary,
            runtime_adapters: combined_worker_state(integration.runtime_adapters, runtime_adapters_state()) }
    }
}

fn runtime_adapters_state() -> ComponentState {
    #[cfg(target_os = "linux")]
    if crate::isolation_mcp::a_producers_ready() { return ComponentState::Ready; }
    ComponentState::PendingIntegration
}

#[derive(Clone, Debug, Serialize)]
pub struct Readiness {
    pub required: bool,
    pub ready: bool,
    pub components: Components,
}

fn applies(linux: bool, protected: bool, team: bool) -> bool { linux && protected && team }

fn latch_requirement(latch: &AtomicBool, required: bool) -> bool {
    required && !latch.swap(true, Ordering::AcqRel)
}

/// Call after resolving the boot edition, before admitting employee work. This
/// only narrows the existing native-systemd fence; Personal/local installs gain
/// no new gate and this function never clears another component's latch.
pub fn initialize(team: bool) {
    let protected = std::env::var_os(duduclaw_core::root_key::MODE_ENV).as_deref()
        == Some(std::ffi::OsStr::new("native-systemd"));
    if !applies(cfg!(target_os = "linux"), protected, team) { return; }
    duduclaw_core::execution_gate::require_isolation();
    duduclaw_parse_worker::client::require_isolation();
    if latch_requirement(&REQUIRED, true) {
        start_update_probe();
        tracing::error!("受保護團隊的隔離服務尚未就緒，員工執行已暫停；管理員仍可使用儀表板檢查服務狀態。");
    }
}

struct UpdateObservation {
    observed: Instant,
    evidence: duduclaw_updated::readiness::GenerationEvidence,
}
static UPDATE_OBSERVATION: std::sync::Mutex<Option<UpdateObservation>> = std::sync::Mutex::new(None);

fn update_fresh(observed: Option<Instant>, now: Instant) -> bool {
    observed.and_then(|at| now.checked_duration_since(at)).is_some_and(|age| age <= Duration::from_secs(2))
}
fn compatible_update(evidence: &duduclaw_updated::readiness::GenerationEvidence) -> bool {
    let p = &evidence.protocols;
    evidence.generation.0 != [0; 32]
        && duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).is_ok_and(|v| v == evidence.version)
        && [p.gateway, p.spawnd, p.trampoline, p.egress, p.updated, p.mcp_bridge, p.parse_worker, p.importer]
            .into_iter().all(|version| version == 1)
}
/// Authenticated F observation, usable by boot consumers only while fresh.
#[cfg(target_os = "linux")]
pub(crate) fn verified_generation() -> Option<duduclaw_isolation::ids::Sha256Digest> {
    current_generation()
}

fn current_generation() -> Option<duduclaw_isolation::ids::Sha256Digest> {
    let value = UPDATE_OBSERVATION.lock().ok()?;
    let observation = value.as_ref()?;
    (compatible_update(&observation.evidence) && update_fresh(Some(observation.observed), Instant::now()))
        .then_some(observation.evidence.generation)
}

fn combined_worker_state(integration: ComponentState, producers: ComponentState) -> ComponentState {
    if producers != ComponentState::Ready { ComponentState::Unavailable } else { integration }
}

fn parse_worker_state() -> ComponentState {
    #[cfg(all(target_os = "linux", feature = "isolation-worker-integration"))]
    if duduclaw_parse_worker::client::launcher_installed()
        && crate::isolation_mcp::boot_status().constructed
        && update_service_state() == ComponentState::Ready {
        return ComponentState::Ready;
    }
    ComponentState::Unavailable
}

fn update_service_state() -> ComponentState {
    match UPDATE_OBSERVATION.lock() {
        Ok(value) if value.as_ref().is_some_and(|v| compatible_update(&v.evidence)
            && update_fresh(Some(v.observed), Instant::now())) => ComponentState::Ready,
        _ => ComponentState::Unavailable,
    }
}
#[cfg(target_os = "linux")]
fn start_update_probe() {
    // One bounded blocking client, off every execution/admission thread. A
    // timeout leaves old evidence to expire at two seconds; an observed refusal
    // clears it immediately. The timestamp starts before the RPC, not after it.
    let result = std::thread::Builder::new().name("isolation-updated-probe".into()).spawn(|| loop {
        let observed = Instant::now();
        let nonce = duduclaw_isolation::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
        let next = duduclaw_updated::client::probe(nonce).ok()
            .filter(compatible_update).filter(|_| update_fresh(Some(observed), Instant::now()))
            .map(|evidence| UpdateObservation { observed, evidence });
        match UPDATE_OBSERVATION.lock() { Ok(mut value) => *value = next, Err(_) => return }
        // Poll on a one-second cadence, never accumulate outstanding requests.
        std::thread::sleep(Duration::from_secs(1).saturating_sub(observed.elapsed()));
    });
    if result.is_err() { tracing::error!("更新服務的隔離狀態檢查無法啟動，團隊執行維持暫停。"); }
}
#[cfg(not(target_os = "linux"))]
fn start_update_probe() {}

/// Connect at trusted boot even if another component is still unavailable. The
/// installed backend rechecks the composite gate before every reservation.
pub async fn connect_backend() {
    if !REQUIRED.load(Ordering::Acquire) { return; }
    #[cfg(target_os = "linux")]
    {
        let result = match duduclaw_execution::SpawndBackend::connect_with_admission(backend_admission).await {
            Ok(backend) => duduclaw_execution::selection::install_protected_backend(
                std::sync::Arc::new(backend), backend_admission),
            Err(error) => Err(error),
        };
        if result.is_err() { tracing::error!("受保護團隊無法連接隔離執行服務，員工執行維持暫停。"); }
    }
}

fn admit(required: bool, components: Option<Components>) -> Result<(), String> {
    if !required || components.is_some_and(|value| value.all_ready()) { return Ok(()); }
    Err("ISOLATION_NOT_READY: 受保護團隊的隔離服務尚未就緒，請由管理員檢查服務狀態。".into())
}

/// Covers API-only work as well as subprocesses. No request parameter can
/// override this process-wide requirement, including human-interactive calls.
pub fn check_execution() -> Result<(), String> {
    let required = REQUIRED.load(Ordering::Acquire);
    admit(required, required.then(|| Components::current(spawnd_verified())))
}

#[cfg(target_os = "linux")]
fn backend_admission() -> Result<(), duduclaw_isolation::execution::BackendError> {
    check_execution().map_err(|_| duduclaw_isolation::execution::BackendError::Unavailable)
}

/// Every read uses fresh, authenticated B1 evidence. This result contains no
/// paths, process IDs, key material, raw service errors or caller-supplied flags.
pub fn status() -> Option<Readiness> {
    if !REQUIRED.load(Ordering::Acquire) { return None; }
    let components = Components::current(spawnd_verified());
    Some(Readiness { required: true, ready: components.all_ready(), components })
}

#[cfg(target_os = "linux")]
fn spawnd_verified() -> bool {
    duduclaw_spawnd::host::EvidenceReport::read(nix::unistd::getegid().as_raw())
        .is_ok_and(|report| report.check().is_ok())
}

#[cfg(not(target_os = "linux"))]
fn spawnd_verified() -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_applies_only_to_protected_linux_team() {
        for linux in [false, true] {
            for protected in [false, true] {
                for team in [false, true] {
                    assert_eq!(applies(linux, protected, team), linux && protected && team);
                }
            }
        }
    }

    #[test]
    fn live_team_upgrade_latches_without_reopening_on_downgrade() {
        let latch = AtomicBool::new(false);
        assert!(!latch_requirement(&latch, applies(true, true, false)));
        assert!(!latch.load(Ordering::Acquire));
        assert!(latch_requirement(&latch, applies(true, true, true)));
        assert!(latch.load(Ordering::Acquire));
        assert!(!latch_requirement(&latch, applies(true, true, true)));
        assert!(!latch_requirement(&latch, applies(true, true, false)));
        assert!(latch.load(Ordering::Acquire));
    }

    #[test]
    fn complete_host_receipt_cannot_enable_half_built_installation() {
        for verified in [false, true] {
            let components = Components::current(verified);
            assert!(!components.all_ready());
            assert_eq!(components.parse_worker, ComponentState::Unavailable);
            assert_eq!(components.update_service, ComponentState::Unavailable);
            assert_eq!(components.mcp_boundary, ComponentState::PendingIntegration);
            // Missing runtime producers override a pending integration probe.
            assert_eq!(components.runtime_adapters, ComponentState::Unavailable);
        }
    }

    #[test]
    fn updater_evidence_expires_and_never_accepts_future_or_missing_observation() {
        let now = Instant::now();
        assert!(!update_fresh(None, now));
        assert!(update_fresh(Some(now), now));
        assert!(update_fresh(Some(now), now + Duration::from_secs(2)));
        assert!(!update_fresh(Some(now), now + Duration::from_millis(2001)));
        assert!(!update_fresh(Some(now + Duration::from_millis(1)), now));
    }

    #[test]
    fn worker_readiness_requires_real_producers_and_integration_evidence() {
        for integration in [ComponentState::Ready, ComponentState::Unavailable, ComponentState::PendingIntegration] {
            for producers in [ComponentState::Ready, ComponentState::Unavailable, ComponentState::PendingIntegration] {
                let state = combined_worker_state(integration, producers);
                assert_eq!(state == ComponentState::Ready,
                    integration == ComponentState::Ready && producers == ComponentState::Ready);
            }
        }
    }

    #[test]
    fn every_component_is_required() {
        let ready = Components { launch_service: ComponentState::Ready, sandbox: ComponentState::Ready,
            network: ComponentState::Ready, storage: ComponentState::Ready,
            parse_worker: ComponentState::Ready, update_service: ComponentState::Ready,
            mcp_boundary: ComponentState::Ready, runtime_adapters: ComponentState::Ready };
        assert!(ready.all_ready());
        for state in [ComponentState::Unavailable, ComponentState::PendingIntegration] {
            let mut cases = [ready; 8];
            cases[0].launch_service = state;
            cases[1].sandbox = state;
            cases[2].network = state;
            cases[3].storage = state;
            cases[4].parse_worker = state;
            cases[5].update_service = state;
            cases[6].mcp_boundary = state;
            cases[7].runtime_adapters = state;
            for case in cases { assert!(!case.all_ready()); }
        }
    }

    #[test]
    fn admission_refuses_missing_or_partial_evidence_and_preserves_local_mode() {
        assert!(admit(false, None).is_ok());
        assert!(admit(true, None).is_err());
        assert!(admit(true, Some(Components::current(true))).is_err());
        let ready = Components { launch_service: ComponentState::Ready, sandbox: ComponentState::Ready,
            network: ComponentState::Ready, storage: ComponentState::Ready,
            parse_worker: ComponentState::Ready, update_service: ComponentState::Ready,
            mcp_boundary: ComponentState::Ready, runtime_adapters: ComponentState::Ready };
        assert!(admit(true, Some(ready)).is_ok());
        let mut stale = ready;
        stale.launch_service = ComponentState::Unavailable;
        assert!(admit(true, Some(stale)).is_err());
    }

    #[test]
    fn operator_payload_has_fixed_component_states_only() {
        let components = Components::current(false);
        let value = serde_json::to_value(Readiness { required: true, ready: false, components }).unwrap();
        assert_eq!(value["required"], true);
        assert_eq!(value["ready"], false);
        assert_eq!(value["components"].as_object().unwrap().len(), 8);
        assert_eq!(value["components"]["launch_service"], "unavailable");
        assert_eq!(value["components"]["mcp_boundary"], "pending_integration");
    }
}
