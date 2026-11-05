//! Consumers of F's signed policy artifact and root installer's measured receipts.
//! Neither gateway IPC nor employee files can publish these inputs.
use std::{ffi::CString, fs::{self, File}, io, os::fd::AsFd, os::unix::fs::MetadataExt, path::Path, sync::{Arc, Mutex}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{bounded::Text, egress::ProfileId, execution::{RunHandle, DiskQuota, NetworkPolicy, ResourceLimits, RunTimeouts, RuntimeKind},
    ids::{EmployeeId, Sha256Digest, UidRange}, ipc::{ErrorCode, QueueClass}, update::{InstallTarget, ReleaseManifest}};
use duduclaw_updated::linux::{files::Directory, service::{artifact_name, verified_committed_generation}, INSTALL_ROOT};
use crate::{Result, policy::{EmployeeEntitlement, Executable, PublishedRuntime, RootPolicy}};
const ROOT: &str = "/var/lib/duduclaw-spawnd";
const RECEIPTS: &str = "/run/duduclaw-isolation-evidence";
pub const GATEWAY_PUBLICATION: &str = "/run/duduclaw-spawnd/publication.json";
const POLICY_ID: &str = "isolation-policy-v1";
fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "installed isolation publication unavailable") }
fn os<T,E>(value: std::result::Result<T,E>) -> Result<T> { value.map_err(|_| ErrorCode::IsolationUnavailable) }
fn io<T,E>(value: std::result::Result<T,E>) -> io::Result<T> { value.map_err(|_| denied()) }
fn nonzero(value: Sha256Digest) -> bool { value.0 != [0; 32] }

#[derive(Clone)]
struct VerifiedGeneration { digest: Sha256Digest, manifest: ReleaseManifest }
static VERIFIED_GENERATION: Mutex<Option<VerifiedGeneration>> = Mutex::new(None);
fn compatible(evidence: &duduclaw_updated::readiness::GenerationEvidence) -> bool {
    let p = &evidence.protocols;
    nonzero(evidence.generation) && [p.gateway, p.spawnd, p.trampoline, p.egress, p.updated,
        p.mcp_bridge, p.parse_worker, p.importer].into_iter().all(|version| version == 1)
        && duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).is_ok_and(|version| evidence.version == version)
}
fn cache_matches(cached: &VerifiedGeneration, live: &duduclaw_updated::readiness::GenerationEvidence) -> bool {
    compatible(live) && cached.digest == live.generation && cached.manifest.product == live.product
        && cached.manifest.version == live.version && cached.manifest.protocols == live.protocols
}
fn live_generation() -> io::Result<duduclaw_updated::readiness::GenerationEvidence> {
    let nonce = duduclaw_isolation::ids::RequestId(crate::server::random_id()?);
    let evidence = io(duduclaw_updated::client::probe(nonce))?;
    if !compatible(&evidence) { return Err(denied()); }
    Ok(evidence)
}
/// Cache only the expensive verification of immutable root generation artifacts.
/// Every use requires F's authenticated live nonce probe; a failed probe discards
/// the cache. Executable descriptors still receive their normal per-run hashes.
fn committed_generation() -> io::Result<(Sha256Digest, ReleaseManifest)> {
    let started = crate::host_state::boottime_ms()?;
    let mut slot = VERIFIED_GENERATION.lock().map_err(|_| denied())?;
    let live = match live_generation() {
        Ok(live) => live,
        Err(error) => { *slot = None; return Err(error); }
    };
    if !slot.as_ref().is_some_and(|cached| cache_matches(cached, &live)) {
        *slot = None;
        let (digest, manifest) = io(verified_committed_generation())?;
        let verified = VerifiedGeneration { digest, manifest };
        // Do not publish a generation that changed during the full disk pass.
        let after = live_generation()?;
        if !cache_matches(&verified, &live) || !cache_matches(&verified, &after) { return Err(denied()); }
        *slot = Some(verified);
    }
    let now = crate::host_state::boottime_ms()?;
    // A long cold verification may populate the cache, but that observation is
    // never relabelled as fresh. The next tick must obtain a new live probe.
    if now < started || now - started >= 1000 { return Err(denied()); }
    let verified = slot.as_ref().ok_or_else(denied)?;
    Ok((verified.digest, verified.manifest.clone()))
}
fn read<T: for<'de> Deserialize<'de>>(root: &str, name: &str) -> io::Result<T> {
    let directory = io(Directory::open(Path::new(root)))?;
    let bytes = io(directory.bytes(name, 1024 * 1024))?;
    serde_json::from_slice(&bytes).map_err(|_| denied())
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "RunHandle")]
struct RunRecord(pub duduclaw_isolation::ids::RunId);
// Serde mirrors belong to this root publication, never to the IPC contract.
#[derive(Serialize, Deserialize)]
#[serde(remote = "NetworkPolicy", tag = "kind", content = "profile", rename_all = "snake_case", deny_unknown_fields)]
enum NetworkRecord { Offline, Egress(ProfileId) }
#[derive(Serialize, Deserialize)]
#[serde(remote = "DiskQuota", tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DiskRecord { LoopFilesystem { bytes: u64, inodes: u64 }, ProjectQuota { bytes: u64, inodes: u64 } }
#[derive(Serialize, Deserialize)]
#[serde(remote = "ResourceLimits", deny_unknown_fields)]
struct ResourceRecord {
    memory_bytes: u64, cpu_quota_percent: u32, tasks: u32, open_files: u32, ptys: u16,
    #[serde(with = "DiskRecord")]
    disk: DiskQuota,
    io_read_bytes_per_sec: u64, io_write_bytes_per_sec: u64, io_read_ops_per_sec: u64,
    io_write_ops_per_sec: u64, output_buffer_bytes: u32, output_total_bytes: u64, memfd_total_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "RunTimeouts", deny_unknown_fields)]
struct TimeoutRecord { queue_ms: u64, startup_ms: u64, runtime_ms: u64, pty_idle_ms: Option<u64>, termination_grace_ms: u64 }
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutableRecord {
    Elf { entry: Text<128>, digest: Sha256Digest },
    Script { interpreter: Text<128>, interpreter_digest: Sha256Digest, script: Text<128>, script_digest: Sha256Digest },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRecord {
    pub runtime: RuntimeKind, pub profile: ProfileId, pub executable: ExecutableRecord,
    pub pinned_version: Text<64>, pub fixed_args: Vec<Text<4096>>,
    #[serde(with = "ResourceRecord")]
    pub resources: ResourceLimits,
    #[serde(with = "TimeoutRecord")]
    pub timeouts: RunTimeouts,
    #[serde(with = "NetworkRecord")]
    pub network: NetworkPolicy, pub mcp: bool, pub pty_allowed: bool,
    pub queue_class: QueueClass,
    pub mcp_acceptance: Option<McpAcceptance>,
}
impl RuntimeRecord {
    pub fn to_runtime(&self) -> PublishedRuntime {
        let executable = match &self.executable {
            ExecutableRecord::Elf { entry, digest } => Executable::Elf { entry: entry.clone(), digest: *digest },
            ExecutableRecord::Script { interpreter, interpreter_digest, script, script_digest } => Executable::Script {
                interpreter: interpreter.clone(), interpreter_digest: *interpreter_digest, script: script.clone(), script_digest: *script_digest },
        };
        PublishedRuntime { runtime: self.runtime.clone(), profile: self.profile, executable, pinned_version: self.pinned_version.clone(),
            fixed_args: self.fixed_args.clone(), resources: self.resources.clone(), timeouts: self.timeouts.clone(), network: self.network.clone(),
            mcp: self.mcp, pty_allowed: self.pty_allowed, queue_class: self.queue_class }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementRecord { pub employee: EmployeeId, pub profiles: Vec<ProfileId> }
/// Signed exact-runtime acceptance, produced by exercising the CLI's own MCP
/// child launch. A direct bridge invocation cannot produce this acceptance.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpAcceptance {
    pub runtime: RuntimeKind, pub version: String, pub runtime_digest: Sha256Digest,
    pub bridge_digest: Sha256Digest, pub projection_digest: Sha256Digest,
    pub launch_policy_digest: Sha256Digest,
    pub inherited_fd: i32, pub initialize_exit: i32, pub parent_exec_exit: i32,
    pub mcp_child_exec_exit: i32, pub exclusive_config_exit: i32, pub transcript_digest: Sha256Digest,
}
impl McpAcceptance {
    fn validate_launch(&self, runtime: &RuntimeRecord, native: &NativeRecord) -> io::Result<()> {
        let mut runtime = runtime.clone();
        runtime.mcp_acceptance = None;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "domain": "duduclaw-runtime-mcp-acceptance-launch-v1", "runtime": runtime, "native": native,
        })).map_err(|_| denied())?;
        if self.launch_policy_digest != Sha256Digest(Sha256::digest(bytes).into()) { return Err(denied()); }
        Ok(())
    }
    pub fn validate(&self, projection: &duduclaw_mcp_boundary::config::RuntimeProjection) -> io::Result<()> {
        let r = &projection.registration;
        if self.runtime != r.runtime || self.version != r.version || self.runtime_digest != r.runtime_digest
            || self.bridge_digest != r.bridge_digest || self.projection_digest != projection.digest()
            || self.inherited_fd != 3 || self.initialize_exit != 0 || self.parent_exec_exit != 0
            || self.mcp_child_exec_exit != 0 || self.exclusive_config_exit != 0 || !nonzero(self.transcript_digest) { return Err(denied()); }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRunReceipt {
    pub version: u16, pub generation: Sha256Digest, pub boot_id: String, pub checked_boottime_ms: u64,
    pub employee: EmployeeId,
    #[serde(with = "RunRecord")]
    pub run: RunHandle, pub runtime: RuntimeKind, pub runtime_version: String,
    pub runtime_digest: Sha256Digest, pub bridge_digest: Sha256Digest, pub projection_digest: Sha256Digest,
    pub projection_mount_id: u64, pub recursive_readonly: bool, pub root_owned_ancestors: bool,
    pub acceptance: McpAcceptance,
}
fn mcp_path(run: RunHandle) -> std::path::PathBuf {
    let hex: String = run.0.0.iter().map(|byte| format!("{byte:02x}")).collect();
    Path::new("/run/duduclaw-spawnd").join(format!("mcp-{hex}.json"))
}
impl McpRunReceipt {
    pub fn read(run: RunHandle, gateway_gid: u32) -> io::Result<Self> {
        let receipt: Self = crate::host_state::read_json(&mcp_path(run), gateway_gid, 0o640)?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let now = crate::host_state::boottime_ms()?;
        if receipt.version != 1 || receipt.run != run || receipt.boot_id != boot.trim() || receipt.projection_mount_id == 0
            || !receipt.recursive_readonly || !receipt.root_owned_ancestors || now < receipt.checked_boottime_ms
            || now - receipt.checked_boottime_ms >= 60_000 { return Err(denied()); }
        Ok(receipt)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostPublication { version: u16, generation: Sha256Digest, entitlements: Vec<EntitlementRecord> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRecord {
    profile: ProfileId, runtime: duduclaw_run::seccomp::RuntimeProfile, runtime_digest: Sha256Digest,
    forwarder: Option<ForwarderRecord>, tmpfs_bytes: u64, tmpfs_inodes: u64, proc_subset_pid: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwarderRecord { entry: String, executable_digest: Sha256Digest, argv: Vec<String>,
    seccomp: duduclaw_run::seccomp::RuntimeProfile, seccomp_digest: Sha256Digest }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedPolicy { version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord> }

/// Root publishes this only after F verification. The consumer also requires the
/// same boot and a fresh monotonic timestamp; a stale report cannot authorize.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayPublication {
    pub version: u16, pub generation: Sha256Digest, pub boot_id: String,
    pub checked_boottime_ms: u64, pub valid_for_ms: u64,
    pub runtimes: Vec<RuntimeRecord>, pub entitlements: Vec<EntitlementRecord>,
}
impl GatewayPublication {
    pub fn policy(&self) -> Result<RootPolicy> {
        RootPolicy::from_verified_publication(self.runtimes.iter().map(RuntimeRecord::to_runtime).collect(),
            self.entitlements.iter().map(|e| EmployeeEntitlement { employee: e.employee, profiles: e.profiles.clone() }).collect())
    }
    pub fn read(gateway_gid: u32) -> io::Result<Self> {
        // The gateway projection uses a dedicated group, not F's root-only reader.
        let value: Self = crate::host_state::read_json(Path::new(GATEWAY_PUBLICATION), gateway_gid, 0o640)?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        value.validate_at(boot.trim(), crate::host_state::boottime_ms()?)?;
        io(value.policy())?;
        Ok(value)
    }
    fn validate_at(&self, boot: &str, now: u64) -> io::Result<()> {
        if self.version != 1 || !nonzero(self.generation) || self.boot_id != boot || self.valid_for_ms == 0 || self.valid_for_ms > 1000
            || now < self.checked_boottime_ms || now - self.checked_boottime_ms >= self.valid_for_ms { return Err(denied()); }
        Ok(())
    }
}
struct Bundle { generation: Sha256Digest, manifest: ReleaseManifest, directory: std::path::PathBuf,
    host: HostPublication, policy: SignedPolicy }
fn bundle() -> io::Result<Bundle> {
    let (generation, manifest) = committed_generation()?;
    let host: HostPublication = read(ROOT, "publication.json")?;
    if host.version != 1 || host.generation != generation { return Err(denied()); }
    let target = InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() };
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == target).ok_or_else(denied)?;
    if artifact.size_bytes > 1024 * 1024 || artifact.mode != 0o444 { return Err(denied()); }
    let hex: String = generation.0.iter().map(|byte| format!("{byte:02x}")).collect();
    let directory = Path::new(INSTALL_ROOT).join("generations").join(hex);
    let bytes = io(io(Directory::open(&directory))?.bytes(&io(artifact_name(&target))?, 1024 * 1024))?;
    if bytes.len() as u64 != artifact.size_bytes || Sha256Digest(Sha256::digest(&bytes).into()) != artifact.sha256 { return Err(denied()); }
    let policy: SignedPolicy = serde_json::from_slice(&bytes).map_err(|_| denied())?;
    if policy.version != 1 || policy.native.len() != policy.runtimes.len() { return Err(denied()); }
    let mut ids = std::collections::BTreeSet::new();
    for profile in &policy.native {
        if !ids.insert((profile.profile.0).0) || !policy.runtimes.iter().any(|r| r.profile == profile.profile) { return Err(denied()); }
        let arch = io(duduclaw_run::seccomp::Architecture::native())?;
        io(duduclaw_run::seccomp::runtime(arch, &profile.runtime, profile.runtime_digest))?;
    }
    Ok(Bundle { generation, manifest, directory, host, policy })
}
#[derive(Default)]
pub struct InstalledPublication { generation: Option<Sha256Digest> }
impl InstalledPublication {
    fn check_generation(&mut self, generation: Sha256Digest) -> Result<()> {
        if !nonzero(generation) || self.generation.is_some_and(|pinned| pinned != generation) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        self.generation = Some(generation);
        Ok(())
    }
    fn checked_bundle(&mut self) -> Result<Bundle> {
        let bundle = os(bundle())?;
        // A verified policy/native artifact is immutable within a generation.
        // Never mix old plans or detached mounts with a later generation. F's
        // activation restarts B1; live policy swapping is deliberately refused.
        self.check_generation(bundle.generation)?;
        Ok(bundle)
    }
}
fn checked_runtime<'a>(records: &'a [RuntimeRecord], runtime: &PublishedRuntime) -> Result<&'a RuntimeRecord> {
    let mut matching = records.iter().filter(|record| record.profile == runtime.profile);
    let record = matching.next().ok_or(ErrorCode::RuntimeDenied)?;
    if matching.next().is_some() || record.to_runtime() != *runtime { return Err(ErrorCode::RuntimeDenied); }
    Ok(record)
}
fn check_plan_runtime(plan: &crate::bootstrap::BootstrapPlan, runtime: &PublishedRuntime) -> Result<()> {
    let expected = crate::bootstrap::BootstrapPlan::from_runtime(plan.run, plan.employee, plan.uid, plan.gid,
        plan.supervisor_uid, plan.supervisor_gid, plan.nonce, runtime, plan.pty, plan.prompt_bytes, &plan.input_fds, plan.proxy_port)?;
    if expected.encode()? != plan.encode()? { return Err(ErrorCode::RuntimeDenied); }
    Ok(())
}
impl crate::host::Publication for InstalledPublication {
    fn load(&mut self) -> Result<RootPolicy> {
        let started = os(crate::host_state::boottime_ms())?;
        let b = self.checked_bundle()?;
        let report = GatewayPublication { version: 1, generation: b.generation,
            boot_id: os(fs::read_to_string("/proc/sys/kernel/random/boot_id"))?.trim().into(), checked_boottime_ms: started, valid_for_ms: 1000,
            runtimes: b.policy.runtimes, entitlements: b.host.entitlements };
        let policy = report.policy()?;
        let (_, gid) = os(crate::server::gateway_identity())?;
        os(crate::host_state::atomic_json(Path::new(GATEWAY_PUBLICATION), &report, gid, 0o640))?;
        Ok(policy)
    }
    fn snapshot_authority(&mut self, gateway_uid: u32) -> Result<crate::fds::SnapshotAuthority> {
        let (actual, _) = os(crate::server::gateway_identity())?;
        if actual != gateway_uid || gateway_uid == 0 { return Err(ErrorCode::IsolationUnavailable); }
        let path = Path::new(ROOT).join("snapshots");
        os(crate::registry::root_path(&path, true))?;
        let file = os(File::open(path))?;
        Ok(crate::fds::SnapshotAuthority { gateway_uid, mount_id: os(crate::fds::mount_id(file.as_fd()))? })
    }
    fn supervisor_identity(&mut self) -> Result<(u32, u32)> { os(crate::server::named_identity(c"duduclaw-runsup")) }
    fn anchors(&mut self) -> Result<crate::bootstrap_mounts::RootPublicationAnchors> {
        let b = self.checked_bundle()?;
        os(crate::bootstrap_mounts::RootPublicationAnchors::new(b.directory, Path::new(ROOT).join("etc-state"), Path::new(ROOT).join("projections")))
    }
    fn prepare_mcp_projection(&mut self, plan: &crate::bootstrap::BootstrapPlan, runtime: &PublishedRuntime) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let b = self.checked_bundle()?;
        let record = checked_runtime(&b.policy.runtimes, runtime)?;
        check_plan_runtime(plan, runtime)?;
        if !runtime.mcp { return Ok(()); }
        let acceptance = record.mcp_acceptance.as_ref().ok_or(ErrorCode::RuntimeDenied)?;
        let bridge = b.manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge).ok_or(ErrorCode::RuntimeDenied)?;
        let runtime_digest = match &runtime.executable {
            Executable::Elf { digest, .. } => *digest, Executable::Script { script_digest, .. } => *script_digest,
        };
        let projection = os(duduclaw_mcp_boundary::config::runtime_projection(duduclaw_mcp_boundary::config::RuntimeRegistration {
            runtime: runtime.runtime.clone(), version: runtime.pinned_version.as_str().into(), runtime_digest,
            bridge_digest: bridge.sha256, bridge_executable: "/runtime/duduclaw-mcp-bridge".into() }))?;
        os(acceptance.validate(&projection))?;
        let native = b.policy.native.iter().find(|n| n.profile == runtime.profile).ok_or(ErrorCode::RuntimeDenied)?;
        os(acceptance.validate_launch(record, native))?;
        if projection.files.iter().any(|file| file.destination != duduclaw_mcp_boundary::config::ConfigDestination::RunConfig
            || file.relative_target.contains('/')) { return Err(ErrorCode::RuntimeDenied); }
        let anchor = Path::new(ROOT).join("projections");
        os(crate::registry::root_path(&anchor, true))?;
        let hex: String = plan.employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = anchor.join(hex);
        match fs::DirBuilder::new().mode(0o755).create(&path) {
            Ok(()) => os(File::open(&anchor).and_then(|f| f.sync_all()))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
            Err(_) => return Err(ErrorCode::IsolationUnavailable),
        }
        os(crate::registry::root_path(&path, true))?;
        os(File::open(&path).and_then(|f| f.set_permissions(fs::Permissions::from_mode(0o755))))?;
        let directory = os(Directory::open(&path))?;
        for file in projection.files {
            let temporary = format!("{}.next", file.relative_target);
            match directory.remove(&temporary) {
                Ok(()) => {}, Err(duduclaw_updated::Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {},
                Err(_) => return Err(ErrorCode::IsolationUnavailable),
            }
            let mut output = os(directory.create(&temporary))?;
            os(output.write_all(file.contents.as_bytes()))?;
            os(output.set_permissions(fs::Permissions::from_mode(0o444)))?;
            os(output.sync_all())?;
            os(directory.rename(&temporary, &directory, file.relative_target, true))?;
        }
        Ok(())
    }
    fn record_mcp_projection(&mut self, plan: &crate::bootstrap::BootstrapPlan, runtime: &PublishedRuntime,
        mounts: &crate::bootstrap_mounts::PreparedMounts) -> Result<()> {
        let b = self.checked_bundle()?;
        let record = checked_runtime(&b.policy.runtimes, runtime)?;
        check_plan_runtime(plan, runtime)?;
        if !runtime.mcp { return Ok(()); }
        let acceptance = record.mcp_acceptance.clone().ok_or(ErrorCode::RuntimeDenied)?;
        let bridge = b.manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge).ok_or(ErrorCode::RuntimeDenied)?;
        let runtime_digest = match &runtime.executable {
            Executable::Elf { digest, .. } => *digest,
            Executable::Script { script_digest, .. } => *script_digest,
        };
        let registration = duduclaw_mcp_boundary::config::RuntimeRegistration { runtime: runtime.runtime.clone(),
            version: runtime.pinned_version.as_str().into(), runtime_digest, bridge_digest: bridge.sha256,
            bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
        let projection = os(duduclaw_mcp_boundary::config::runtime_projection(registration))?;
        os(acceptance.validate(&projection))?;
        let native = b.policy.native.iter().find(|n| n.profile == runtime.profile).ok_or(ErrorCode::RuntimeDenied)?;
        os(acceptance.validate_launch(record, native))?;
        let arguments: Vec<_> = runtime.fixed_args.iter().map(|arg| arg.as_str()).collect();
        let expected: Vec<_> = projection.arguments.iter().map(String::as_str).collect();
        if !arguments.ends_with(&expected) { return Err(ErrorCode::RuntimeDenied); }
        // Reject duplicate override flags before the exact, fixed suffix.
        for arg in arguments.iter().take(arguments.len() - expected.len()) {
            if matches!(*arg, "--mcp-config" | "--strict-mcp-config") { return Err(ErrorCode::RuntimeDenied); }
        }
        let projection_mount_id = os(crate::bootstrap_mounts::verify_mcp_projection(mounts, &projection))?;
        os(crate::bootstrap_mounts::open_verified(mounts.runtime.as_fd(), "duduclaw-mcp-bridge", bridge.sha256, true,
            std::time::Instant::now() + std::time::Duration::from_secs(15)))?;
        let report = McpRunReceipt { version: 1, generation: b.generation,
            boot_id: os(fs::read_to_string("/proc/sys/kernel/random/boot_id"))?.trim().into(),
            checked_boottime_ms: os(crate::host_state::boottime_ms())?, employee: plan.employee, run: plan.run,
            runtime: runtime.runtime.clone(), runtime_version: runtime.pinned_version.as_str().into(), runtime_digest,
            bridge_digest: bridge.sha256, projection_digest: projection.digest(), projection_mount_id,
            recursive_readonly: true, root_owned_ancestors: true, acceptance };
        let (_, gid) = os(crate::server::gateway_identity())?;
        os(crate::host_state::atomic_json(&mcp_path(plan.run), &report, gid, 0o640))
    }
    fn retire_mcp_projection(&mut self, run: RunHandle) -> Result<()> {
        let path = mcp_path(run);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ErrorCode::IsolationUnavailable),
            Ok(_) => { os(crate::registry::root_path(&path, false))?; os(fs::remove_file(&path))?;
                os(File::open(path.parent().ok_or(ErrorCode::IsolationUnavailable)?).and_then(|f| f.sync_all())) }
        }
    }
    fn native_profile(&mut self, runtime: &PublishedRuntime) -> Result<crate::native_trampoline::NativeProfile> {
        let b = self.checked_bundle()?;
        let record = checked_runtime(&b.policy.runtimes, runtime)?;
        let native = b.policy.native.iter().find(|n| n.profile == runtime.profile).ok_or(ErrorCode::RuntimeDenied)?;
        if runtime.mcp {
            let acceptance = record.mcp_acceptance.as_ref().ok_or(ErrorCode::RuntimeDenied)?;
            os(acceptance.validate_launch(record, native))?;
        }
        let native = b.policy.native.into_iter().find(|n| n.profile == runtime.profile).ok_or(ErrorCode::RuntimeDenied)?;
        let (uid, _) = os(crate::server::gateway_identity())?;
        let snapshot = self.snapshot_authority(uid)?;
        let forwarder = native.forwarder.map(|f| -> Result<_> {
            if f.argv.is_empty() || f.argv.len() > 128 || !b.manifest.artifacts.iter().any(|a|
                a.install_target == InstallTarget::EgressForwarder && a.sha256 == f.executable_digest
                && artifact_name(&a.install_target).is_ok_and(|name| name == f.entry)) { return Err(ErrorCode::RuntimeDenied); }
            let argv = f.argv.into_iter().map(|arg| os(CString::new(arg))).collect::<Result<Vec<_>>>()?;
            Ok(crate::native_trampoline::ForwarderProfile { entry: f.entry, executable_digest: f.executable_digest,
                argv, seccomp: f.seccomp, seccomp_digest: f.seccomp_digest })
        }).transpose()?;
        Ok(crate::native_trampoline::NativeProfile { runtime: native.runtime, runtime_digest: native.runtime_digest,
            forwarder, snapshot, tmpfs_bytes: native.tmpfs_bytes, tmpfs_inodes: native.tmpfs_inodes, proc_subset_pid: native.proc_subset_pid })
    }
}

/// Root harness receipt. Passes require an actual positive control plus denied
/// public-key login/forwarding, not a stored "ready" switch. Its producer remains
/// responsible for performing the observations and atomically renewing it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReceipt {
    version: u16, generation: Sha256Digest, boot_id: String, checked_boottime_ms: u64, valid_for_ms: u64,
    external_nss_range: Option<UidRange>, iam_reservation_digest: Option<Sha256Digest>,
    unowned_uids: Vec<u32>, owner_inventory_digest: Sha256Digest, ssh: Vec<SshReceipt>,
    trampoline: TrampolineReceipt,
    dns: DnsReceipt,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SshReceipt { account: String, control_exit: i32, public_key_exit: i32, forwarding_exit: i32, transcript_digest: Sha256Digest }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrampolineReceipt { executable_digest: Sha256Digest, landlock_exit: i32, namespace_exit: i32,
    detected_abi: u32, selected_abi: u32, transcript_digest: Sha256Digest }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DnsReceipt {
    query: Text<253>, expected_answer: Sha256Digest, observed_answer: Sha256Digest,
    route: DnsRoute, ip_address_allow: Vec<String>, second_layer: crate::readiness::EgressSecondLayer,
    transcript_digest: Sha256Digest,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DnsRoute { ResolvedVarlink, DedicatedBroker }
fn receipt() -> io::Result<InstallReceipt> {
    let (generation, manifest) = committed_generation()?;
    let receipt: InstallReceipt = read(RECEIPTS, "installation.json")?;
    // Verify the exact executable PID 1 will start, not merely an unused copy
    // under the signed generation directory.
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Trampoline).ok_or_else(denied)?;
    let installed = io(io(Directory::open(Path::new("/usr/lib/duduclaw")))?.read("duduclaw-run"))?;
    if installed.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(denied()); }
    io(duduclaw_updated::linux::files::verify_fd(installed, artifact.size_bytes, artifact.sha256))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let now = crate::host_state::boottime_ms()?;
    if receipt.version != 1 || receipt.generation != generation || receipt.boot_id != boot.trim()
        || receipt.valid_for_ms == 0 || receipt.valid_for_ms > 60_000 || now < receipt.checked_boottime_ms
        || now - receipt.checked_boottime_ms >= receipt.valid_for_ms || receipt.ssh.len() > 30_000
        || receipt.unowned_uids.len() > 30_000 || !manifest.artifacts.iter().any(|a|
            a.install_target == InstallTarget::Trampoline && a.sha256 == receipt.trampoline.executable_digest) { return Err(denied()); }
    Ok(receipt)
}
pub struct InstalledEvidence;
impl crate::accounts::InstallationEvidence for InstalledEvidence {
    fn external_nss_range_reserved_and_enumerable(&self, range: UidRange) -> io::Result<()> {
        let r = receipt()?;
        if r.external_nss_range != Some(range) || !r.iam_reservation_digest.is_some_and(nonzero) { return Err(denied()); }
        Ok(())
    }
    fn no_existing_file_owner(&self, uid: u32) -> io::Result<()> {
        let r = receipt()?;
        if !nonzero(r.owner_inventory_digest) || !r.unowned_uids.contains(&uid) { return Err(denied()); }
        Ok(())
    }
    fn ssh_public_key_and_forwarding_denied(&self, account: &str) -> io::Result<()> {
        let r = receipt()?;
        let rows: Vec<_> = r.ssh.iter().filter(|s| s.account == account).collect();
        if rows.len() != 1 || rows[0].control_exit != 0 || rows[0].public_key_exit != 255
            || rows[0].forwarding_exit != 255 || !nonzero(rows[0].transcript_digest) { return Err(denied()); }
        Ok(())
    }
}
impl crate::native_trampoline::Installation for InstalledEvidence {
    fn readback(&mut self) -> Result<crate::bootstrap::TrampolineReadiness> {
        let r = os(receipt())?.trampoline;
        let actual = os(duduclaw_run::linux::probe::probe())?;
        if r.landlock_exit != 0 || r.namespace_exit != 0 || !nonzero(r.transcript_digest)
            || r.detected_abi != actual.landlock_abi || r.selected_abi != actual.landlock_ruleset_abi { return Err(ErrorCode::IsolationUnavailable); }
        let evidence = crate::bootstrap::TrampolineReadiness { version: 1, executable_verified: true,
            landlock_self_test: true, namespace_self_test: true, detected_landlock_abi: r.detected_abi, selected_landlock_abi: r.selected_abi };
        evidence.check()?;
        Ok(evidence)
    }
}
impl crate::host::DnsEvidence for InstalledEvidence {
    fn verify(&mut self) -> Result<crate::readiness::EgressSecondLayer> {
        let r = os(receipt())?.dns;
        if r.query.as_str().is_empty() || !nonzero(r.expected_answer) || r.expected_answer != r.observed_answer
            || !nonzero(r.transcript_digest) { return Err(ErrorCode::IsolationUnavailable); }
        crate::readiness::validate_egress_ip_allow(&r.ip_address_allow)?;
        // The root harness probes the configured Unix route, including broker
        // restrictions, and binds both query and answer to this short receipt.
        match r.route { DnsRoute::ResolvedVarlink | DnsRoute::DedicatedBroker => Ok(r.second_layer) }
    }
}
pub fn install(components: &mut crate::host::Components) {
    components.publication = Box::new(InstalledPublication::default());
    components.installation = Arc::new(InstalledEvidence);
    components.egress = Box::new(crate::host::LinuxEgress::new(Box::new(InstalledEvidence)));
    components.trampoline = Box::new(crate::native_trampoline::NativeTrampoline::new(Box::new(InstalledEvidence)));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime_record() -> RuntimeRecord {
        RuntimeRecord { runtime: RuntimeKind::Claude, profile: ProfileId(Sha256Digest([1; 32])),
            executable: ExecutableRecord::Elf { entry: Text::new("claude".into()).unwrap(), digest: Sha256Digest([2; 32]) },
            pinned_version: Text::new("2.0.0".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 4096, ptys: 16, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000, pty_idle_ms: None, termination_grace_ms: 100 },
            network: NetworkPolicy::Offline, mcp: true, pty_allowed: false, queue_class: QueueClass::Background, mcp_acceptance: None }
    }
    #[test]
    fn publication_generation_pin_refuses_native_policy_replacement_until_restart() {
        let mut publication = InstalledPublication::default();
        let original = Sha256Digest([1; 32]);
        assert!(publication.check_generation(original).is_ok());
        assert!(publication.check_generation(original).is_ok());
        // Any changed signed native artifact has a different release generation.
        assert!(publication.check_generation(Sha256Digest([2; 32])).is_err());
        assert_eq!(publication.generation, Some(original));
        assert!(publication.check_generation(Sha256Digest([0; 32])).is_err());
        assert_eq!(publication.generation, Some(original));
    }
    #[test]
    fn signed_runtime_comparison_rejects_same_profile_version_with_changed_authority() {
        let record = runtime_record();
        let records = [record.clone()];
        let runtime = record.to_runtime();
        assert!(checked_runtime(&records, &runtime).is_ok());
        let mut changed = runtime.clone(); changed.fixed_args.push(Text::new("--other-config".into()).unwrap());
        assert!(checked_runtime(&records, &changed).is_err());
        let mut changed = runtime.clone(); changed.resources.memory_bytes += 1;
        assert!(checked_runtime(&records, &changed).is_err());
        let mut changed = runtime.clone(); changed.timeouts.runtime_ms += 1;
        assert!(checked_runtime(&records, &changed).is_err());
        let mut changed = runtime.clone(); changed.network = NetworkPolicy::Egress(ProfileId(Sha256Digest([9; 32])));
        assert!(checked_runtime(&records, &changed).is_err());
        let mut changed = runtime; changed.executable = Executable::Elf {
            entry: Text::new("claude".into()).unwrap(), digest: Sha256Digest([7; 32]) };
        assert!(checked_runtime(&records, &changed).is_err());
    }
    #[test]
    fn bootstrap_plan_must_match_the_exact_signed_runtime() {
        let runtime = runtime_record().to_runtime();
        let mut plan = crate::bootstrap::BootstrapPlan::from_runtime(RunHandle(duduclaw_isolation::ids::RunId([1; 16])),
            EmployeeId([2; 16]), 70000, 70000, 400, 400, [3; 32], &runtime, false, 0, &[], None).unwrap();
        assert!(check_plan_runtime(&plan, &runtime).is_ok());
        plan.argv.push("--other-config".into());
        assert!(check_plan_runtime(&plan, &runtime).is_err());
        plan.argv.pop(); plan.open_files += 1;
        assert!(check_plan_runtime(&plan, &runtime).is_err());
    }
    #[test]
    fn generation_cache_requires_same_live_signed_identity_and_protocols() {
        use duduclaw_isolation::update::*;
        let version = duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).unwrap();
        let protocols = ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
            updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 };
        let manifest = ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version,
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
            minimum_version: version, serial: 1, created_at_unix: 1, expires_at_unix: 2,
            artifacts: vec![], protocols: protocols.clone(), migrations: vec![] };
        let cached = VerifiedGeneration { digest: Sha256Digest([1; 32]), manifest };
        let live = duduclaw_updated::readiness::GenerationEvidence { generation: cached.digest,
            product: Product::Duduclaw, version, protocols };
        assert!(cache_matches(&cached, &live));
        let mut changed = live.clone(); changed.generation = Sha256Digest([2; 32]);
        assert!(!cache_matches(&cached, &changed));
        let mut changed = live.clone(); changed.product = Product::DuduclawPro;
        assert!(!cache_matches(&cached, &changed));
        let mut changed = live.clone(); changed.protocols.mcp_bridge = 2;
        assert!(!cache_matches(&cached, &changed));
        let mut changed = live; changed.version.patch += 1;
        assert!(!cache_matches(&cached, &changed));
    }
    #[test]
    fn publication_receipt_requires_generation_boot_and_freshness() {
        let r = GatewayPublication { version: 1, generation: Sha256Digest([1; 32]), boot_id: "boot".into(),
            checked_boottime_ms: 50, valid_for_ms: 1000, runtimes: vec![], entitlements: vec![] };
        assert!(r.validate_at("boot", 50).is_ok());
        for (boot, now) in [("other", 50), ("boot", 49), ("boot", 1050)] { assert!(r.validate_at(boot, now).is_err()); }
        assert!(r.policy().is_err());
        let mut r = r; r.generation = Sha256Digest([0; 32]); assert!(r.validate_at("boot", 50).is_err());
    }
    #[test]
    fn publication_records_reject_unknown_fields() {
        let mut value = serde_json::json!({"version":1,"generation":Sha256Digest([1; 32]),"entitlements":[]});
        assert!(serde_json::from_value::<HostPublication>(value.clone()).is_ok());
        value.as_object_mut().unwrap().insert("ready".into(), true.into());
        assert!(serde_json::from_value::<HostPublication>(value).is_err());
        assert!(serde_json::from_str::<InstallReceipt>(r#"{"ready":true}"#).is_err());
    }
    #[test]
    fn mcp_acceptance_binds_exact_runtime_projection_and_child_exec() {
        let registration = duduclaw_mcp_boundary::config::RuntimeRegistration { runtime: RuntimeKind::Claude,
            version: "2.0.0".into(), runtime_digest: Sha256Digest([1; 32]), bridge_digest: Sha256Digest([2; 32]),
            bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
        let projection = duduclaw_mcp_boundary::config::runtime_projection(registration).unwrap();
        let mut acceptance = McpAcceptance { runtime: RuntimeKind::Claude, version: "2.0.0".into(),
            runtime_digest: Sha256Digest([1; 32]), bridge_digest: Sha256Digest([2; 32]),
            projection_digest: projection.digest(), launch_policy_digest: Sha256Digest([4; 32]), inherited_fd: 3,
            initialize_exit: 0, parent_exec_exit: 0, mcp_child_exec_exit: 0, exclusive_config_exit: 0,
            transcript_digest: Sha256Digest([3; 32]) };
        assert!(acceptance.validate(&projection).is_ok());
        acceptance.mcp_child_exec_exit = 1; assert!(acceptance.validate(&projection).is_err());
        acceptance.mcp_child_exec_exit = 0; acceptance.inherited_fd = 4; assert!(acceptance.validate(&projection).is_err());
        acceptance.inherited_fd = 3; acceptance.bridge_digest = Sha256Digest([8; 32]); assert!(acceptance.validate(&projection).is_err());
    }
    #[test]
    #[ignore = "iso_root_: disposable Linux VM; verifies root receipt ownership and mode"]
    fn iso_root_e2e_publication_receipt_mode_and_symlink_refusal() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let directory = std::path::PathBuf::from(format!("/root/iso-publication-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("receipt.json");
        let value = GatewayPublication { version: 1, generation: Sha256Digest([1; 32]), boot_id: "boot".into(),
            checked_boottime_ms: 10, valid_for_ms: 1000, runtimes: vec![], entitlements: vec![] };
        crate::host_state::atomic_json(&path, &value, 0, 0o640).unwrap();
        let read: GatewayPublication = crate::host_state::read_json(&path, 0, 0o640).unwrap();
        assert_eq!(read.generation, value.generation);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(crate::host_state::read_json::<GatewayPublication>(&path, 0, 0o640).is_err());
        fs::remove_file(&path).unwrap();
        symlink("/etc/passwd", &path).unwrap();
        assert!(crate::host_state::read_json::<GatewayPublication>(&path, 0, 0o640).is_err());
        fs::remove_file(path).unwrap(); fs::remove_dir(directory).unwrap();
    }

}

#[cfg(test)]
#[path = "installed_root_tests.rs"]
mod root_acceptance;

#[cfg(test)]
#[path = "installed_account_root_tests.rs"]
mod account_root_acceptance;
