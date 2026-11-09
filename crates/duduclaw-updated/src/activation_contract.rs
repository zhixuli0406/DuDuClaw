//! Package F's integration seam. These messages are NOT the gateway update RPC.
//! Implementations in A/B/C/G must derive readbacks from running processes and
//! retain the drain barrier durably across their own restart before acknowledging.
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::*};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub generation: Sha256Digest,
    pub migrations: Vec<DataMigration>,
    pub runtime_entries: Vec<ReleaseArtifact>,
}
impl Preparation {
    pub fn for_manifest(manifest: &ReleaseManifest, generation: Sha256Digest) -> Self {
        Self { generation, migrations: manifest.migrations.clone(), runtime_entries: manifest.artifacts.iter()
            .filter(|a| matches!(a.install_target, InstallTarget::RuntimeEntry { .. })).cloned().collect() }
    }
}

/// G applies migrations and B1 publishes runtime registrations under the closed
/// admission barrier. Both methods are idempotent by digest. Preflight has no
/// mutating migration effect. An interrupted apply must recover, never guess.
pub trait GenerationPreparation {
    fn preflight(&mut self, preparation: &Preparation) -> Result<()>;
    fn apply(&mut self, preparation: &Preparation) -> Result<()>;
}
pub trait ActivationRuntime: GenerationPreparation {
    fn drain(&mut self, generation: Sha256Digest) -> Result<()>;
    fn read_back(&mut self, generation: Sha256Digest) -> Result<Vec<ComponentReadback>>;
    fn resume(&mut self, generation: Sha256Digest) -> Result<()>;
}

/// Platform effects are separated so restart/crash injection exercises the same
/// production ordering as Linux. Verification hashes the actual installed files.
pub trait GenerationEffects {
    fn verify(&mut self, manifest: &ReleaseManifest, generation: Sha256Digest) -> Result<()>;
    fn quiesce(&mut self) -> Result<()>;
    fn publish(&mut self, generation: Sha256Digest) -> Result<()>;
    fn restart(&mut self) -> Result<()>;
    fn processes(&mut self, manifest: &ReleaseManifest) -> Result<Vec<u32>>;
    fn current(&mut self) -> Result<Sha256Digest>;
    fn sync(&mut self) -> Result<()>;
}
pub fn healthy_readback(runtime: &mut impl ActivationRuntime, effects: &mut impl GenerationEffects,
    manifest: &ReleaseManifest, generation: Sha256Digest) -> Result<()> {
    if effects.current()? != generation { return Err(Error::Maintenance); }
    let before = effects.processes(manifest)?;
    validate_readbacks(manifest, &runtime.read_back(generation)?)?;
    if before != effects.processes(manifest)? || effects.current()? != generation { return Err(Error::Maintenance); }
    effects.sync()
}
pub fn activate_generation(runtime: &mut impl ActivationRuntime, effects: &mut impl GenerationEffects,
    manifest: &ReleaseManifest, generation: Sha256Digest) -> Result<()> {
    let preparation = Preparation::for_manifest(manifest, generation);
    effects.verify(manifest, generation)?;
    runtime.preflight(&preparation)?;
    runtime.drain(generation)?;
    effects.quiesce()?;
    runtime.apply(&preparation)?;
    effects.verify(manifest, generation)?;
    effects.publish(generation)?;
    effects.restart()?;
    healthy_readback(runtime, effects, manifest, generation)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentReadback {
    pub artifact_id: String,
    pub target: InstallTarget,
    pub product: Product,
    pub version: ReleaseVersion,
    pub protocol: u16,
    /// The SHA-256 of the executable actually loaded, not of a proposed pathname.
    pub build_sha256: Sha256Digest,
    pub effective_uid: u32,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub schema_version: u16,
    pub nonce: RequestId,
    pub command: ControlCommand,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCommand {
    Prepare { preparation: Preparation },
    Apply { preparation: Preparation },
    Drain { generation: Sha256Digest },
    Readback { generation: Sha256Digest },
    Resume { generation: Sha256Digest },
}
impl ControlCommand {
    pub fn generation(&self) -> Sha256Digest {
        match self {
            Self::Prepare { preparation } | Self::Apply { preparation } => preparation.generation,
            Self::Drain { generation } | Self::Readback { generation } | Self::Resume { generation } => *generation,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlResponse {
    pub schema_version: u16,
    pub nonce: RequestId,
    pub generation: Sha256Digest,
    pub outcome: ControlOutcome,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlOutcome {
    Prepared,
    Applied,
    Drained { admission_closed: bool, active_runs: u32 },
    Readback { components: Vec<ComponentReadback> },
    Resumed,
    Refused,
}

pub fn validate_response(request: &ControlRequest, response: &ControlResponse) -> Result<()> {
    if request.schema_version != 1 || response.schema_version != 1 || request.nonce != response.nonce
        || request.command.generation() != response.generation { return Err(Error::Maintenance); }
    let valid = matches!((&request.command, &response.outcome),
        (ControlCommand::Prepare { .. }, ControlOutcome::Prepared)
        | (ControlCommand::Apply { .. }, ControlOutcome::Applied)
        | (ControlCommand::Drain { .. }, ControlOutcome::Drained { admission_closed: true, active_runs: 0 })
        | (ControlCommand::Readback { .. }, ControlOutcome::Readback { .. })
        | (ControlCommand::Resume { .. }, ControlOutcome::Resumed));
    if !valid { return Err(Error::Maintenance); }
    Ok(())
}

fn protocol(manifest: &ReleaseManifest, target: &InstallTarget) -> u16 {
    match target {
        InstallTarget::Gateway => manifest.protocols.gateway,
        InstallTarget::Spawnd | InstallTarget::RuntimeEntry { .. } => manifest.protocols.spawnd,
        InstallTarget::Trampoline => manifest.protocols.trampoline,
        InstallTarget::Egress | InstallTarget::EgressForwarder => manifest.protocols.egress,
        InstallTarget::McpBridge => manifest.protocols.mcp_bridge,
        InstallTarget::ParseWorker => manifest.protocols.parse_worker,
        InstallTarget::Importer | InstallTarget::Migration => manifest.protocols.importer,
        InstallTarget::UpdateSlot => manifest.protocols.updated,
    }
}
/// The updater slot has its own same-fd root boot/self-test path. Every other
/// signed artifact needs a real live or isolated non-root readiness readback.
pub fn validate_readbacks(manifest: &ReleaseManifest, readbacks: &[ComponentReadback]) -> Result<()> {
    manifest.validate()?;
    let expected: Vec<_> = manifest.artifacts.iter().filter(|a| a.install_target != InstallTarget::UpdateSlot).collect();
    if expected.len() != readbacks.len() { return Err(Error::Maintenance); }
    for artifact in expected {
        let mut found = readbacks.iter().filter(|r| r.artifact_id == artifact.id);
        let report = found.next().ok_or(Error::Maintenance)?;
        if found.next().is_some() || report.target != artifact.install_target || report.product != manifest.product
            || report.version != manifest.version || report.protocol != protocol(manifest, &artifact.install_target)
            || report.build_sha256 != artifact.sha256 || !report.ready { return Err(Error::Maintenance); }
        let root_component = matches!(artifact.install_target, InstallTarget::Spawnd | InstallTarget::Trampoline);
        if (report.effective_uid == 0) != root_component { return Err(Error::Maintenance); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            minimum_version: ReleaseVersion { major: 1, minor: 79, patch: 0 }, serial: 1, channel: ReleaseChannel::Stable,
            arch: Architecture::X86_64, os: OperatingSystem::Linux, created_at_unix: 1, expires_at_unix: 2,
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 },
            artifacts: vec![ReleaseArtifact { id: "gateway".into(), install_target: InstallTarget::Gateway,
                sha256: Sha256Digest([1; 32]), size_bytes: 1, mode: 0o755 }], migrations: vec![] }
    }
    fn report() -> ComponentReadback {
        let m = manifest(); let a = &m.artifacts[0];
        ComponentReadback { artifact_id: a.id.clone(), target: a.install_target.clone(), product: m.product,
            version: m.version, protocol: 1, build_sha256: a.sha256, effective_uid: 999, ready: true }
    }
    #[test]
    fn health_readback_requires_every_identity_dimension_and_exact_membership() {
        let m = manifest();
        assert!(validate_readbacks(&m, &[report()]).is_ok());
        assert!(validate_readbacks(&m, &[]).is_err());
        assert!(validate_readbacks(&m, &[report(), report()]).is_err());
        for case in 0..8 {
            let mut r = report();
            match case {
                0 => r.artifact_id = "different".into(), 1 => r.target = InstallTarget::Spawnd,
                2 => r.product = Product::DuduclawPro, 3 => r.version.patch += 1,
                4 => r.protocol += 1, 5 => r.build_sha256 = Sha256Digest([2; 32]),
                6 => r.effective_uid = 0, _ => r.ready = false,
            }
            assert!(validate_readbacks(&m, &[r]).is_err(), "case {case}");
        }
    }
    #[test]
    fn barrier_requires_nonce_generation_zero_runs_and_persistent_admission() {
        let generation = Sha256Digest([7; 32]);
        let request = ControlRequest { schema_version: 1, nonce: RequestId([8; 16]), command: ControlCommand::Drain { generation } };
        let response = ControlResponse { schema_version: 1, nonce: request.nonce, generation,
            outcome: ControlOutcome::Drained { admission_closed: true, active_runs: 0 } };
        assert!(validate_response(&request, &response).is_ok());
        for case in 0..5 {
            let mut r = response.clone();
            match case {
                0 => r.nonce = RequestId([9; 16]), 1 => r.generation = Sha256Digest([9; 32]),
                2 => r.outcome = ControlOutcome::Drained { admission_closed: false, active_runs: 0 },
                3 => r.outcome = ControlOutcome::Drained { admission_closed: true, active_runs: 1 },
                _ => r.schema_version = 2,
            }
            assert!(validate_response(&request, &r).is_err());
        }
    }
    #[test]
    fn every_activation_effect_can_crash_and_resume_without_early_admission() {
        use std::{cell::RefCell, rc::Rc};
        #[derive(Default)]
        struct World { count: usize, fail: usize, drained: bool, stopped: bool, applied: bool,
            applications: usize, selected: bool, restarted: bool, resumed: bool }
        impl World {
            fn tick(&mut self) -> Result<()> { self.count += 1;
                if self.count == self.fail { Err(Error::Maintenance) } else { Ok(()) } }
        }
        struct Runtime(Rc<RefCell<World>>);
        impl GenerationPreparation for Runtime {
            fn preflight(&mut self, _: &Preparation) -> Result<()> { self.0.borrow_mut().tick() }
            fn apply(&mut self, _: &Preparation) -> Result<()> {
                let mut w = self.0.borrow_mut(); assert!(w.drained && w.stopped);
                if !w.applied { w.applied = true; w.applications += 1; } w.tick()
            }
        }
        impl ActivationRuntime for Runtime {
            fn drain(&mut self, _: Sha256Digest) -> Result<()> { let mut w = self.0.borrow_mut(); w.drained = true; w.tick() }
            fn read_back(&mut self, _: Sha256Digest) -> Result<Vec<ComponentReadback>> {
                let mut w = self.0.borrow_mut(); assert!(w.drained && w.applied && w.selected && w.restarted);
                w.tick()?; Ok(vec![report()])
            }
            fn resume(&mut self, _: Sha256Digest) -> Result<()> { self.0.borrow_mut().resumed = true; Ok(()) }
        }
        struct Effects(Rc<RefCell<World>>);
        impl GenerationEffects for Effects {
            fn verify(&mut self, _: &ReleaseManifest, _: Sha256Digest) -> Result<()> { self.0.borrow_mut().tick() }
            fn quiesce(&mut self) -> Result<()> { let mut w = self.0.borrow_mut(); w.stopped = true; w.tick() }
            fn publish(&mut self, _: Sha256Digest) -> Result<()> { let mut w = self.0.borrow_mut(); assert!(w.applied); w.selected = true; w.tick() }
            fn restart(&mut self) -> Result<()> { let mut w = self.0.borrow_mut(); assert!(w.selected); w.restarted = true; w.tick() }
            fn processes(&mut self, _: &ReleaseManifest) -> Result<Vec<u32>> { self.0.borrow_mut().tick()?; Ok(vec![123]) }
            fn current(&mut self) -> Result<Sha256Digest> { let mut w = self.0.borrow_mut(); assert!(w.selected); w.tick()?; Ok(Sha256Digest([9; 32])) }
            fn sync(&mut self) -> Result<()> { self.0.borrow_mut().tick() }
        }
        let world = Rc::new(RefCell::new(World::default()));
        activate_generation(&mut Runtime(world.clone()), &mut Effects(world.clone()), &manifest(), Sha256Digest([9; 32])).unwrap();
        let steps = world.borrow().count;
        assert!(!world.borrow().resumed);
        for crash in 1..=steps {
            let world = Rc::new(RefCell::new(World { fail: crash, ..World::default() }));
            assert!(activate_generation(&mut Runtime(world.clone()), &mut Effects(world.clone()), &manifest(), Sha256Digest([9; 32])).is_err());
            assert!(!world.borrow().resumed);
            world.borrow_mut().fail = 0;
            // Recreate adapters, retaining only the side effects that landed.
            activate_generation(&mut Runtime(world.clone()), &mut Effects(world.clone()), &manifest(), Sha256Digest([9; 32])).unwrap();
            assert_eq!(world.borrow().applications, 1);
            assert!(!world.borrow().resumed);
        }
    }
}
