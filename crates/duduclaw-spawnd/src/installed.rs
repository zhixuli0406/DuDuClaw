//! Consumers of F's signed policy artifact and the root publications derived
//! from it. Neither gateway IPC nor employee files can publish these inputs.
//! R7c: no installer receipt is read any more (SSH denial: `crate::ssh_evidence`;
//! external NSS: refused, `crate::accounts::nss_supported`).
use std::{ffi::CString, fs::{self, File}, io, os::fd::AsFd, path::Path, sync::{Arc, Mutex}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{bounded::Text, egress::{DnsRoute, EgressProfile, ProfileId}, execution::{RunHandle, DiskQuota, NetworkPolicy, ResourceLimits, RunTimeouts, RuntimeKind},
    ids::{EmployeeId, Sha256Digest}, ipc::{ErrorCode, QueueClass}, update::{InstallTarget, ReleaseManifest}};
use duduclaw_updated::linux::{files::Directory, service::{artifact_name, verified_committed_generation}, INSTALL_ROOT};
use crate::{Result, policy::{EmployeeEntitlement, Executable, PublishedRuntime, RootPolicy, TurnLimit}};
const ROOT: &str = "/var/lib/duduclaw-spawnd";
/// Per-run /etc projection state (the etc-state parent of `RootPublicationAnchors`).
pub const ETC_STATE_DIRECTORY: &str = "/var/lib/duduclaw-spawnd/etc-state";
/// Per-employee MCP projection files (the projection root of `RootPublicationAnchors`).
pub const PROJECTIONS_DIRECTORY: &str = "/var/lib/duduclaw-spawnd/projections";
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
/// Also the generation input of the R7a self-test key (`crate::selftest`).
pub(crate) fn committed_generation() -> io::Result<(Sha256Digest, ReleaseManifest)> {
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
    /// R6 policy v3 only. Omitted when absent, so v1/v2 records (and their
    /// MCP launch-policy digests) are byte-identical to before; a ranged v3
    /// record's digest covers the range, so its acceptance is regenerated.
    /// In a signed policy the key's presence is checked on the raw bytes
    /// (`turn_limit_keys`): refused before v3, and `null` refused in v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_limit: Option<TurnLimit>,
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
            mcp: self.mcp, pty_allowed: self.pty_allowed, queue_class: self.queue_class, turn_limit: self.turn_limit }
    }
}
/// Root-owned host entitlement. `max_turns` (R6) can only narrow the signed
/// step range; it is omitted when absent so existing files keep their shape.
/// It is written only by root: R8d's `Entitle` (and the removal by `Disable`
/// / `Retire`) on the allocation worker, and the generation rollover of
/// [`InstalledPublication::load`] (`crate::entitlement`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementRecord { pub employee: EmployeeId, pub profiles: Vec<ProfileId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u16> }
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
        if self.launch_policy_digest != Self::launch_policy_digest(runtime, native)? { return Err(denied()); }
        Ok(())
    }
    /// The digest [`Self::validate_launch`] compares (R8r-4: the policy
    /// tool's acceptance harness computes a record's value with this same
    /// function, never a copy of it).
    fn launch_policy_digest(runtime: &RuntimeRecord, native: &NativeRecord) -> io::Result<Sha256Digest> {
        let mut runtime = runtime.clone();
        runtime.mcp_acceptance = None;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "domain": "duduclaw-runtime-mcp-acceptance-launch-v1", "runtime": runtime, "native": native,
        })).map_err(|_| denied())?;
        Ok(Sha256Digest(Sha256::digest(bytes).into()))
    }
    pub fn validate(&self, projection: &duduclaw_mcp_boundary::config::RuntimeProjection) -> io::Result<()> {
        self.validate_check(projection).map_err(|_| denied())
    }
    /// R8z-3: [`Self::validate`] naming the first check that refused (the
    /// former single condition chain, in its order).
    pub fn validate_check(&self, projection: &duduclaw_mcp_boundary::config::RuntimeProjection)
        -> std::result::Result<(), duduclaw_mcp_boundary::config::AcceptanceCheck> {
        use duduclaw_mcp_boundary::config::AcceptanceCheck as C;
        let r = &projection.registration;
        let checks = [(self.runtime == r.runtime, C::Runtime), (self.version == r.version, C::Version),
            (self.runtime_digest == r.runtime_digest, C::RuntimeDigest), (self.bridge_digest == r.bridge_digest, C::BridgeDigest),
            (self.projection_digest == projection.digest(), C::ProjectionDigest), (self.inherited_fd == 3, C::InheritedFd),
            (self.initialize_exit == 0, C::InitializeExit), (self.parent_exec_exit == 0, C::ParentExecExit),
            (self.mcp_child_exec_exit == 0, C::McpChildExecExit), (self.exclusive_config_exit == 0, C::ExclusiveConfigExit),
            (nonzero(self.transcript_digest), C::TranscriptDigest)];
        match checks.into_iter().find(|(passed, _)| !passed) {
            Some((_, check)) => Err(check),
            None => Ok(()),
        }
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
/// `/var/lib/duduclaw-spawnd/publication.json` (root:root 0600). The
/// installer writes it once with no entitlement; afterwards only root writes
/// it (R8d, `crate::entitlement`), always through [`write_host`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPublication { pub(crate) version: u16, pub(crate) generation: Sha256Digest,
    pub(crate) entitlements: Vec<EntitlementRecord> }
const HOST_PUBLICATION: &str = "publication.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vendor_digest: Option<Sha256Digest>,
    profile: ProfileId, runtime: duduclaw_run::seccomp::RuntimeProfile, runtime_digest: Sha256Digest,
    forwarder: Option<ForwarderRecord>, tmpfs_bytes: u64, tmpfs_inodes: u64, proc_subset_pid: bool,
    /// HX helper allowlist of this parent profile (policy v2 only). Omitted when
    /// absent, so v1-shaped records keep their MCP launch-policy digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launcher: Option<crate::helper_policy::LauncherRecord>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwarderRecord { entry: String, executable_digest: Sha256Digest, argv: Vec<String>,
    seccomp: duduclaw_run::seccomp::RuntimeProfile, seccomp_digest: Sha256Digest }
/// Verified in-memory policy; `version` records which signed schema produced it.
/// `dns_probe_name` (R7b) exists from v4 on; `None` for v1–v3.
struct SignedPolicy { version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord>, dns_probe_name: Option<String> }
/// Exact v1 document. A v2 document always carries `domain`, so this strict
/// reader (and every deployed v1 reader) refuses it instead of reinterpreting it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedPolicyV1 { version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord> }
/// v2: `domain` and `version` are inside the artifact bytes that the signed
/// release manifest binds by sha256 [F17].
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedPolicyV2 { domain: String, version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord> }
const POLICY_DOMAIN_V2: &str = "duduclaw-isolation-policy-v2";
/// v3 (R6): the v2 shape under its own signed domain; runtime records may carry
/// `turn_limit`. v1/v2 documents carrying `turn_limit` are refused.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedPolicyV3 { domain: String, version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord> }
const POLICY_DOMAIN_V3: &str = "duduclaw-isolation-policy-v3";
/// v4 (R7b): the v3 shape under its own signed domain plus the required
/// `dns_probe_name` (`duduclaw_isolation::egress::validate_dns_probe_name`):
/// a hostname AI employees need anyway, chosen by the vendor at signing time,
/// that spawnd asks egress to resolve as its DNS route evidence.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedPolicyV4 { domain: String, version: u16, runtimes: Vec<RuntimeRecord>, native: Vec<NativeRecord>, dns_probe_name: String }
const POLICY_DOMAIN_V4: &str = "duduclaw-isolation-policy-v4";
#[derive(Deserialize)]
struct PolicyVersion { version: u16 }
/// v4 reader. A v1 document is accepted as "no launcher" (helper tools then
/// report `helper_launcher_unavailable`); a v1 document naming a launcher, a
/// v2, v3 or v4 document without its own domain, a v1/v2 document with a step
/// range, a v1–v3 document with a `dns_probe_name`, a v4 document without a
/// valid one, a v3/v4 document with any invalid step range (the whole
/// policy), or any other version is refused.
fn parse_policy(bytes: &[u8]) -> io::Result<SignedPolicy> {
    let probe: PolicyVersion = serde_json::from_slice(bytes).map_err(|_| denied())?;
    let policy = match probe.version {
        1 => {
            let v1: SignedPolicyV1 = serde_json::from_slice(bytes).map_err(|_| denied())?;
            if v1.version != 1 || v1.native.iter().any(|n| n.launcher.is_some()) { return Err(denied()); }
            SignedPolicy { version: 1, runtimes: v1.runtimes, native: v1.native, dns_probe_name: None }
        }
        2 => {
            let v2: SignedPolicyV2 = serde_json::from_slice(bytes).map_err(|_| denied())?;
            if v2.version != 2 || v2.domain != POLICY_DOMAIN_V2 { return Err(denied()); }
            SignedPolicy { version: 2, runtimes: v2.runtimes, native: v2.native, dns_probe_name: None }
        }
        3 => {
            let v3: SignedPolicyV3 = serde_json::from_slice(bytes).map_err(|_| denied())?;
            if v3.version != 3 || v3.domain != POLICY_DOMAIN_V3 { return Err(denied()); }
            SignedPolicy { version: 3, runtimes: v3.runtimes, native: v3.native, dns_probe_name: None }
        }
        4 => {
            let v4: SignedPolicyV4 = serde_json::from_slice(bytes).map_err(|_| denied())?;
            if v4.version != 4 || v4.domain != POLICY_DOMAIN_V4 { return Err(denied()); }
            duduclaw_isolation::egress::validate_dns_probe_name(&v4.dns_probe_name).map_err(|_| denied())?;
            SignedPolicy { version: 4, runtimes: v4.runtimes, native: v4.native, dns_probe_name: Some(v4.dns_probe_name) }
        }
        _ => return Err(denied()),
    };
    dns_probe_name_key(bytes, policy.version)?;
    turn_limit_keys(bytes, policy.version)?;
    validate_turn_limits(&policy)?;
    Ok(policy)
}
/// R7b, decided on the raw document like `turn_limit_keys`: the key (also as
/// an explicit `null`) is refused before v4. The strict v1–v3 readers refuse
/// it as an unknown field too; this keeps the rule explicit.
fn dns_probe_name_key(bytes: &[u8], version: u16) -> io::Result<()> {
    let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| denied())?;
    if version < 4 && document.get("dns_probe_name").is_some() { return Err(denied()); }
    Ok(())
}
/// A signed policy document through spawnd's strict reader plus the
/// runtime/native pairing `bundle` requires (installer-side readers).
fn signed_policy(bytes: &[u8]) -> io::Result<SignedPolicy> {
    if bytes.len() > 1024 * 1024 { return Err(denied()); }
    let policy = parse_policy(bytes)?;
    if policy.native.len() != policy.runtimes.len() { return Err(denied()); }
    for runtime in &policy.runtimes {
        if !policy.native.iter().any(|native| native.profile == runtime.profile) { return Err(denied()); }
    }
    Ok(policy)
}
/// The egress profile ids the signed isolation policy names (`network:
/// {kind: egress, profile}` of each runtime record), in first-seen order and
/// without duplicates, read through the same strict reader spawnd uses at
/// start (`parse_policy`: version/domain, step ranges, canonical Claude+MCP
/// argument shapes) plus the runtime/native pairing `bundle` requires. The
/// root installer (R3 S5) writes `/etc/duduclaw/egress.json` from exactly
/// these ids; a policy spawnd would refuse is refused here as well.
pub fn signed_policy_egress_profiles(bytes: &[u8]) -> io::Result<Vec<ProfileId>> {
    let policy = signed_policy(bytes)?;
    let mut profiles = Vec::new();
    for runtime in &policy.runtimes {
        if let NetworkPolicy::Egress(profile) = &runtime.network {
            if !profiles.contains(profile) { profiles.push(*profile); }
        }
    }
    Ok(profiles)
}
/// R7b: the signed policy's `dns_probe_name` through the same strict reader;
/// `None` for a policy older than v4 (spawnd then never reports the DNS route
/// as verified).
pub fn signed_policy_dns_probe_name(bytes: &[u8]) -> io::Result<Option<String>> {
    Ok(signed_policy(bytes)?.dns_probe_name)
}
/// R8d (installer re-check): whether `entitlements` — the records of an
/// existing host publication, as untyped JSON — are records spawnd reads
/// (strict `EntitlementRecord`) and pass the production publication check
/// (`RootPolicy::from_verified_publication`: one record per employee, 1..=128
/// distinct profiles each in this signed policy, `max_turns` 1..=1000) against
/// the policy behind `bytes`, read through the same strict reader as above.
pub fn signed_policy_admits_entitlements(bytes: &[u8], entitlements: &[serde_json::Value]) -> io::Result<()> {
    let policy = signed_policy(bytes)?;
    let records = entitlements.iter().map(|value| EntitlementRecord::deserialize(value).map_err(|_| denied()))
        .collect::<io::Result<Vec<_>>>()?;
    io(crate::entitlement::admitted(&policy.runtimes, &records)).map(|_| ())
}
/// R7b: the probe name of the committed generation's verified policy (the
/// full `bundle` checks). Blocking (updater probe): DNS evidence worker only.
pub(crate) fn installed_dns_probe_name() -> io::Result<Option<String>> {
    Ok(bundle()?.policy.dns_probe_name)
}
/// R7b: spawnd's view of the installer's `/etc/duduclaw/egress.json` (the
/// document egress reads): the profiles, each in its strict shape. The other
/// members are egress's to validate; egress refuses to start on them.
#[derive(Deserialize)]
struct EgressConfiguration { profiles: Vec<EgressProfile> }
const EGRESS_CONFIGURATION_DIRECTORY: &str = "/etc/duduclaw";
/// The DNS route this host is configured for: the one route of every profile
/// in the root-owned egress configuration (read with the same root
/// ownership, no-symlink and single-link checks as the other installed
/// documents). The installer writes it from the signed policy's profile ids
/// and the route it selected; spawnd has no other source for the route.
pub(crate) fn configured_dns_route() -> io::Result<DnsRoute> {
    let configuration: EgressConfiguration = read(EGRESS_CONFIGURATION_DIRECTORY, "egress.json")?;
    single_route(&configuration.profiles)
}
fn single_route(profiles: &[EgressProfile]) -> io::Result<DnsRoute> {
    let first = profiles.first().ok_or_else(denied)?;
    if profiles.iter().any(|profile| profile.validate().is_err() || profile.dns_route != first.dns_route) { return Err(denied()); }
    Ok(first.dns_route)
}
/// R6 review round, finding 3: decided on the raw document, because the typed
/// record reads both an absent key and `"turn_limit": null` as `None`. A
/// v1/v2 runtime record must not carry the key at all; a v3 record either
/// omits it (no range: the fixed `--max-turns 50` shape) or carries a range
/// object. An explicit `null` is refused in every version.
fn turn_limit_keys(bytes: &[u8], version: u16) -> io::Result<()> {
    let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| denied())?;
    let runtimes = document.get("runtimes").and_then(serde_json::Value::as_array).ok_or_else(denied)?;
    for record in runtimes {
        let record = record.as_object().ok_or_else(denied)?;
        if record.get("turn_limit").is_some_and(|limit| version < 3 || limit.is_null()) { return Err(denied()); }
    }
    Ok(())
}
/// R6: no step range before v3; every v3 range valid; every Claude MCP record
/// with exactly the canonical fixed-50 or ranged argument shape. Otherwise
/// the policy is refused as a whole.
fn validate_turn_limits(policy: &SignedPolicy) -> io::Result<()> {
    for record in &policy.runtimes {
        if record.turn_limit.is_some() && policy.version < 3 { return Err(denied()); }
        io(crate::policy::validate_turn_limit(&record.to_runtime()))?;
    }
    Ok(())
}
/// HX publication checks 1–7 against the verified runtime set (§2.3), plus the
/// `/helper-tmp` budget, which must fit inside the parent's tmpfs budget.
/// For the native architecture (the unit tests' entry; `bundle` calls
/// `validate_launchers_for` through `verified_policy`).
#[cfg(test)]
fn validate_launchers(policy: &SignedPolicy) -> io::Result<()> {
    validate_launchers_for(policy, io(duduclaw_run::seccomp::Architecture::native())?)
}
/// `validate_launchers` for an explicit architecture (spawnd: the native one;
/// the R7d policy tool: the generation's `--arch`).
fn validate_launchers_for(policy: &SignedPolicy, arch: duduclaw_run::seccomp::Architecture) -> io::Result<()> {
    let runtimes: Vec<_> = policy.runtimes.iter().map(RuntimeRecord::to_runtime).collect();
    for native in &policy.native {
        let Some(launcher) = &native.launcher else { continue };
        if policy.version < 2 || launcher.helper_tmpfs_bytes == 0 || launcher.helper_tmpfs_bytes > native.tmpfs_bytes { return Err(denied()); }
        let parent = runtimes.iter().find(|r| r.profile == native.profile).ok_or_else(denied)?;
        io(crate::helper_policy::validate_launcher(arch, parent, &native.runtime, launcher, &runtimes))?;
    }
    Ok(())
}
fn launchers(policy: &SignedPolicy) -> Vec<(ProfileId, duduclaw_run::seccomp::RuntimeProfile, crate::helper_policy::LauncherRecord)> {
    policy.native.iter().filter_map(|n| n.launcher.as_ref().map(|l| (n.profile, n.runtime.clone(), l.clone()))).collect()
}

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
            self.entitlements.iter().map(|e| EmployeeEntitlement { employee: e.employee, profiles: e.profiles.clone(),
                max_turns: e.max_turns }).collect())
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
    let bundle = unchecked_bundle()?;
    if bundle.host.generation != bundle.generation { return Err(denied()); }
    Ok(bundle)
}
/// Every check of [`bundle`] except that the host publication names the
/// committed generation (it must still parse strictly and be version 1).
/// Only [`InstalledPublication::load`] uses it directly, to roll a host
/// publication of another generation over (R8d B).
fn unchecked_bundle() -> io::Result<Bundle> {
    let (generation, manifest) = committed_generation()?;
    let host: HostPublication = read(ROOT, HOST_PUBLICATION)?;
    if host.version != 1 { return Err(denied()); }
    let target = InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() };
    let artifact = policy_artifact(&manifest)?;
    let hex: String = generation.0.iter().map(|byte| format!("{byte:02x}")).collect();
    let directory = Path::new(INSTALL_ROOT).join("generations").join(hex);
    let bytes = io(io(Directory::open(&directory))?.bytes(&io(artifact_name(&target))?, 1024 * 1024))?;
    policy_bytes_match(artifact, &bytes)?;
    let policy = verified_policy(&bytes, &manifest, io(duduclaw_run::seccomp::Architecture::native())?)?;
    Ok(Bundle { generation, manifest, directory, host, policy })
}
/// The signed policy artifact of a generation manifest: the one
/// `isolation-policy-v1` runtime entry, mode 0444, at most 1 MiB.
fn policy_artifact(manifest: &ReleaseManifest) -> io::Result<&duduclaw_isolation::update::ReleaseArtifact> {
    let target = InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() };
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == target).ok_or_else(denied)?;
    if artifact.size_bytes > 1024 * 1024 || artifact.mode != 0o444 { return Err(denied()); }
    Ok(artifact)
}
/// The policy bytes are exactly the ones the manifest binds (size and sha256).
fn policy_bytes_match(artifact: &duduclaw_isolation::update::ReleaseArtifact, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() as u64 != artifact.size_bytes || Sha256Digest(Sha256::digest(bytes).into()) != artifact.sha256 { return Err(denied()); }
    Ok(())
}
/// Every check `bundle` applies to the policy bytes of a generation, against
/// that generation's manifest, for `arch` (spawnd: the native architecture;
/// the R7d policy tool runs exactly this on its own output for `--arch`):
/// the strict reader, one native record per runtime, the runtime artifact
/// cross-check, unique native profiles each with its runtime, each runtime
/// seccomp filter against its signed digest, and the helper launchers.
fn verified_policy(bytes: &[u8], manifest: &ReleaseManifest, arch: duduclaw_run::seccomp::Architecture) -> io::Result<SignedPolicy> {
    let policy = parse_policy(bytes)?;
    if policy.native.len() != policy.runtimes.len() { return Err(denied()); }
    validate_runtime_artifacts(&policy, manifest)?;
    let mut ids = std::collections::BTreeSet::new();
    for profile in &policy.native {
        if !ids.insert((profile.profile.0).0) || !policy.runtimes.iter().any(|r| r.profile == profile.profile) { return Err(denied()); }
        io(duduclaw_run::seccomp::runtime(arch, &profile.runtime, profile.runtime_digest))?;
    }
    validate_launchers_for(&policy, arch)?;
    Ok(policy)
}
/// A native acceptance binds the exact vendor ELF as part of its launch-policy
/// digest. Replacing either adapter or vendor requires new signed acceptance.
fn validate_runtime_artifacts(policy: &SignedPolicy, manifest: &ReleaseManifest) -> io::Result<()> {
    for runtime in &policy.runtimes {
        let native = policy.native.iter().find(|n| n.profile == runtime.profile).ok_or_else(denied)?;
        let (entry, digest) = match &runtime.executable {
            ExecutableRecord::Elf { entry, digest } => (entry.as_str(), *digest),
            ExecutableRecord::Script { .. } => continue,
        };
        let artifact = manifest.artifacts.iter().find(|a| artifact_name(&a.install_target).is_ok_and(|name| name == entry))
            .ok_or_else(denied)?;
        if artifact.sha256 != digest || artifact.mode != 0o755 { return Err(denied()); }
        let vendor = match runtime.runtime { RuntimeKind::Codex => Some("vendor-codex"),
            RuntimeKind::Grok => Some("vendor-grok"), _ => None };
        if let Some(vendor) = vendor {
            if entry != "runtime-native-adapter-v1" { return Err(denied()); }
            let artifact = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::RuntimeEntry { registry_id: vendor.into() })
                .ok_or_else(denied)?;
            if artifact.mode != 0o755 || native.vendor_digest != Some(artifact.sha256) { return Err(denied()); }
        } else if native.vendor_digest.is_some() { return Err(denied()); }
        if runtime.runtime == RuntimeKind::OpenAiCompat && entry != "runtime-api-worker-v1" { return Err(denied()); }
    }
    Ok(())
}
/// A signed forwarder record names the generation's egress forwarder by its
/// generation entry and digest, with 1–128 arguments (`native_profile`).
fn forwarder_artifact_matches(manifest: &ReleaseManifest, f: &ForwarderRecord) -> bool {
    !f.argv.is_empty() && f.argv.len() <= 128 && manifest.artifacts.iter().any(|a|
        a.install_target == InstallTarget::EgressForwarder && a.sha256 == f.executable_digest
        && artifact_name(&a.install_target).is_ok_and(|name| name == f.entry))
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
    fn checked_bundle(&mut self) -> Result<Bundle> { self.pinned(os(bundle())?) }
    fn pinned(&mut self, bundle: Bundle) -> Result<Bundle> {
        // A verified policy/native artifact is immutable within a generation.
        // Never mix old plans or detached mounts with a later generation. F's
        // activation restarts B1; live policy swapping is deliberately refused.
        self.check_generation(bundle.generation)?;
        Ok(bundle)
    }
    /// R8d B, the one place a host publication is migrated: `load` (root's
    /// lifecycle thread only; never the gateway-side reader). A publication
    /// of another generation is rolled over to the pinned committed one
    /// (`entitlement::rolled_over`), fitted to the same in-force check as
    /// `Entitle` (trailing records dropped while only capacity fails;
    /// anything else refuses) and written atomically before use; a
    /// failed write refuses (fail closed). Structural garbage never gets
    /// here: [`unchecked_bundle`] refuses it like [`bundle`] does.
    fn rolled_bundle(&mut self) -> Result<Bundle> {
        let mut bundle = self.pinned(os(unchecked_bundle())?)?;
        if bundle.host.generation != bundle.generation {
            bundle.host = crate::entitlement::roll_over(&mut InstalledEntitlements, &bundle.host, bundle.generation,
                &bundle.policy.runtimes).map_err(|_| ErrorCode::IsolationUnavailable)?;
        }
        Ok(bundle)
    }
}
/// The only writer of the host publication after install (R8d): the existing
/// root writer (`host_state::atomic_json`: root:root 0600, temp file, fsync,
/// rename, directory fsync). A failure is `Unavailable` only when a re-read
/// proves `previous` is still in force; otherwise the on-disk state is
/// unknown (`Unknown`, the allocation journal's poison rule).
fn write_host(previous: &HostPublication, next: &HostPublication) -> std::result::Result<(), crate::entitlement::Refusal> {
    use crate::entitlement::Refusal;
    match crate::host_state::atomic_json(&Path::new(ROOT).join(HOST_PUBLICATION), next, 0, 0o600) {
        Ok(()) => Ok(()),
        Err(_) => match read::<HostPublication>(ROOT, HOST_PUBLICATION) {
            Ok(current) if current == *previous => Err(Refusal::Unavailable),
            _ => Err(Refusal::Unknown),
        },
    }
}
/// R8d: the allocation worker's view of the installed host publication.
pub(crate) struct InstalledEntitlements;
impl crate::entitlement::Entitlements for InstalledEntitlements {
    /// The committed generation's verified signed runtimes (every `bundle`
    /// check; a host publication not yet rolled over refuses).
    fn signed(&mut self) -> io::Result<(Sha256Digest, Vec<RuntimeRecord>)> {
        let bundle = bundle()?;
        Ok((bundle.generation, bundle.policy.runtimes))
    }
    fn read(&mut self) -> io::Result<HostPublication> { read(ROOT, HOST_PUBLICATION) }
    fn write(&mut self, previous: &HostPublication, next: &HostPublication) -> std::result::Result<(), crate::entitlement::Refusal> {
        write_host(previous, next)
    }
}
fn checked_runtime<'a>(records: &'a [RuntimeRecord], runtime: &PublishedRuntime) -> Result<&'a RuntimeRecord> {
    let mut matching = records.iter().filter(|record| record.profile == runtime.profile);
    let record = matching.next().ok_or(ErrorCode::RuntimeDenied)?;
    if matching.next().is_some() || record.to_runtime() != *runtime { return Err(ErrorCode::RuntimeDenied); }
    Ok(record)
}
/// Recompute the plan from the signed record. `from_runtime` re-checks the
/// step limit against the record's signed range (R6 item 4); the employee's
/// entitlement narrowing was applied by `RootPolicy::max_turns` at start.
fn check_plan_runtime(plan: &crate::bootstrap::BootstrapPlan, runtime: &PublishedRuntime) -> Result<()> {
    let expected = crate::bootstrap::BootstrapPlan::from_runtime(plan.run, plan.employee, plan.uid, plan.gid,
        plan.supervisor_uid, plan.supervisor_gid, plan.nonce, runtime, plan.pty, plan.prompt_bytes, &plan.input_fds, plan.proxy_port,
        plan.max_turns)?;
    if expected.encode()? != plan.encode()? { return Err(ErrorCode::RuntimeDenied); }
    Ok(())
}
impl crate::host::Publication for InstalledPublication {
    fn load(&mut self) -> Result<RootPolicy> {
        let started = os(crate::host_state::boottime_ms())?;
        let b = self.rolled_bundle()?;
        let launchers = launchers(&b.policy);
        let report = GatewayPublication { version: 1, generation: b.generation,
            boot_id: os(fs::read_to_string("/proc/sys/kernel/random/boot_id"))?.trim().into(), checked_boottime_ms: started, valid_for_ms: 1000,
            runtimes: b.policy.runtimes, entitlements: b.host.entitlements };
        let policy = report.policy()?.with_launchers(os(duduclaw_run::seccomp::Architecture::native())?, launchers)?;
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
        os(crate::bootstrap_mounts::RootPublicationAnchors::new(b.directory, Path::new(ETC_STATE_DIRECTORY).into(), Path::new(PROJECTIONS_DIRECTORY).into()))
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
        let anchor = Path::new(PROJECTIONS_DIRECTORY).to_path_buf();
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
        let names: Vec<_> = projection.files.iter().map(duduclaw_mcp_boundary::config::projection_name)
            .collect::<std::result::Result<_, _>>().map_err(|_| ErrorCode::RuntimeDenied)?;
        for name in ["mcp.json", "codex-home.toml", "grok-home.toml", "grok-project.toml", "project-mcp.json"] {
            if names.contains(&name) { continue; }
            match directory.remove(name) {
                Ok(()) => {}, Err(duduclaw_updated::Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {},
                Err(_) => return Err(ErrorCode::IsolationUnavailable),
            }
        }
        for file in projection.files {
            let name = os(duduclaw_mcp_boundary::config::projection_name(&file))?;
            let temporary = format!("{name}.next");
            match directory.remove(&temporary) {
                Ok(()) => {}, Err(duduclaw_updated::Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {},
                Err(_) => return Err(ErrorCode::IsolationUnavailable),
            }
            let mut output = os(directory.create(&temporary))?;
            os(output.write_all(file.contents.as_bytes()))?;
            os(output.set_permissions(fs::Permissions::from_mode(0o444)))?;
            os(output.sync_all())?;
            os(directory.rename(&temporary, &directory, name, true))?;
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
            if !forwarder_artifact_matches(&b.manifest, &f) { return Err(ErrorCode::RuntimeDenied); }
            let argv = f.argv.into_iter().map(|arg| os(CString::new(arg))).collect::<Result<Vec<_>>>()?;
            Ok(crate::native_trampoline::ForwarderProfile { entry: f.entry, executable_digest: f.executable_digest,
                argv, seccomp: f.seccomp, seccomp_digest: f.seccomp_digest })
        }).transpose()?;
        Ok(crate::native_trampoline::NativeProfile { runtime: native.runtime, runtime_digest: native.runtime_digest,
            forwarder, snapshot, tmpfs_bytes: native.tmpfs_bytes, tmpfs_inodes: native.tmpfs_inodes, proc_subset_pid: native.proc_subset_pid,
            // The host attaches the per-employee helper table (RootPolicy::helper_table).
            launcher: None })
    }
}

pub fn install(components: &mut crate::host::Components) {
    components.publication = Box::new(InstalledPublication::default());
    // R7c: SSH denial from spawnd's own probe unit through PID 1, not a receipt.
    components.ssh = Arc::new(crate::ssh_evidence::installed());
    // R7b: DNS route evidence from spawnd's own probe through egress, over the
    // same (single) control connection the run registrations use.
    let client = crate::host::SharedEgressClient::default();
    // The second layer from PID 1's own view of the egress unit's IP rules.
    components.egress = Box::new(crate::host::LinuxEgress::with_client(
        Box::new(crate::dns_evidence::installed(client.clone())), client)
        .with_second_layer(Box::new(crate::egress_layer::installed())));
    // R7a: trampoline readiness comes from spawnd's own self-test, not the receipt.
    components.trampoline = Box::new(crate::native_trampoline::NativeTrampoline::new(
        Box::new(crate::selftest::SelfTestInstallation::default())));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime_record() -> RuntimeRecord {
        RuntimeRecord { runtime: RuntimeKind::Claude, profile: ProfileId(Sha256Digest([1; 32])),
            executable: ExecutableRecord::Elf { entry: Text::new("claude".into()).unwrap(), digest: Sha256Digest([2; 32]) },
            pinned_version: Text::new("2.0.0".into()).unwrap(),
            // The canonical v1/v2 Claude MCP shape (fixed `--max-turns 50`).
            fixed_args: crate::gateway_plan::claude_arguments("model").unwrap().into_iter().map(|arg| Text::new(arg).unwrap()).collect(),
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 4096, ptys: 16, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000, pty_idle_ms: None, termination_grace_ms: 100 },
            network: NetworkPolicy::Offline, mcp: true, pty_allowed: false, queue_class: QueueClass::Background, mcp_acceptance: None,
            turn_limit: None }
    }
    #[test]
    fn native_runtime_publication_requires_signed_adapter_and_matching_vendor_digest() {
        use duduclaw_isolation::update::*;
        let version = duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).unwrap();
        let artifact = |id: &str, digest| ReleaseArtifact { id: id.into(), sha256: digest, size_bytes: 7, mode: 0o755,
            install_target: InstallTarget::RuntimeEntry { registry_id: id.into() } };
        for kind in [RuntimeKind::Codex, RuntimeKind::Grok] {
            let vendor = if kind == RuntimeKind::Codex { "vendor-codex" } else { "vendor-grok" };
            let mut record = runtime_record();
            record.runtime = kind;
            record.executable = ExecutableRecord::Elf { entry: Text::new("runtime-native-adapter-v1".into()).unwrap(),digest: Sha256Digest([2;32]) };
            let mut policy = SignedPolicy { version: 1, runtimes: vec![record.clone()], native: vec![NativeRecord {
                vendor_digest: Some(Sha256Digest([3;32])), profile: record.profile,
                runtime: duduclaw_run::seccomp::RuntimeProfile { allowed_syscalls: vec![0] },
                runtime_digest: Sha256Digest([4;32]), forwarder: None, tmpfs_bytes: 4096,
                tmpfs_inodes: 32, proc_subset_pid: true, launcher: None }], dns_probe_name: None };
            let mut manifest = ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version,
                channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
                minimum_version: version, serial: 1, created_at_unix: 1, expires_at_unix: 2,
                artifacts: vec![artifact("native-adapter-v1",Sha256Digest([2;32])),artifact(vendor,Sha256Digest([3;32]))],
                protocols: ProtocolCompatibility { gateway: 1,spawnd: 1,trampoline: 1,egress: 1,updated: 1,mcp_bridge: 1,parse_worker: 1,importer: 1 },migrations: vec![] };
            validate_runtime_artifacts(&policy,&manifest).unwrap();
            policy.native[0].vendor_digest = None;
            assert!(validate_runtime_artifacts(&policy,&manifest).is_err());
            policy.native[0].vendor_digest = Some(Sha256Digest([3;32]));
            manifest.artifacts[1].sha256 = Sha256Digest([9;32]);
            assert!(validate_runtime_artifacts(&policy,&manifest).is_err());
            manifest.artifacts[1].sha256 = Sha256Digest([3;32]);
            manifest.artifacts[1].mode = 0o444;
            assert!(validate_runtime_artifacts(&policy,&manifest).is_err());
            manifest.artifacts.pop();
            assert!(validate_runtime_artifacts(&policy,&manifest).is_err());
        }
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
            EmployeeId([2; 16]), 70000, 70000, 400, 400, [3; 32], &runtime, false, 0, &[], None, None).unwrap();
        assert!(check_plan_runtime(&plan, &runtime).is_ok());
        plan.argv.push("--other-config".into());
        assert!(check_plan_runtime(&plan, &runtime).is_err());
        plan.argv.pop(); plan.open_files += 1;
        assert!(check_plan_runtime(&plan, &runtime).is_err());
        // An unranged record never accepts a step limit.
        plan.open_files -= 1; plan.max_turns = Some(50);
        assert!(check_plan_runtime(&plan, &runtime).is_err());
    }
    #[test]
    fn ranged_plan_recheck_binds_the_limit_and_its_argv_position() {
        let record = ranged_record();
        let runtime = record.to_runtime();
        let build = |max_turns| crate::bootstrap::BootstrapPlan::from_runtime(RunHandle(duduclaw_isolation::ids::RunId([1; 16])),
            EmployeeId([2; 16]), 70000, 70000, 400, 400, [3; 32], &runtime, false, 0, &[], None, max_turns);
        let plan = build(Some(80)).unwrap();
        assert!(check_plan_runtime(&plan, &runtime).is_ok());
        // Changing the value in argv only, or in the field only, is refused.
        let mut changed = plan.clone();
        let index = changed.argv.iter().position(|arg| arg == "--max-turns").unwrap();
        changed.argv[index + 1] = "200".into();
        assert!(check_plan_runtime(&changed, &runtime).is_err());
        let mut changed = plan.clone(); changed.max_turns = Some(200);
        assert!(check_plan_runtime(&changed, &runtime).is_err());
        // A value outside the signed range can never be re-derived.
        let mut changed = plan.clone(); changed.max_turns = Some(201);
        assert!(check_plan_runtime(&changed, &runtime).is_err());
        let mut changed = plan; changed.max_turns = None;
        assert!(check_plan_runtime(&changed, &runtime).is_err());
        // The MCP suffix check of `record_mcp_projection` is unchanged: the
        // signed arguments still end with the projection's exact suffix.
        let arguments: Vec<_> = record.fixed_args.iter().map(|arg| arg.as_str()).collect();
        let expected = duduclaw_mcp_boundary::config::claude_mcp_arguments();
        let expected: Vec<_> = expected.iter().map(String::as_str).collect();
        assert!(arguments.ends_with(&expected));
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
    }
    /// R8d C: the installer's re-check of an existing publication accepts
    /// exactly the entitlements the production check admits for this policy.
    #[test]
    fn installer_entitlement_check_is_the_production_check() {
        let bytes = serde_json::to_vec(&v1_document()).unwrap();
        let profile = runtime_record().profile;
        let employee = EmployeeId([5; 16]);
        let good = serde_json::json!({ "employee": employee, "profiles": [profile] });
        assert!(signed_policy_admits_entitlements(&bytes, &[]).is_ok());
        assert!(signed_policy_admits_entitlements(&bytes, &[good.clone()]).is_ok());
        for bad in [
            serde_json::json!({ "employee": employee, "profiles": [ProfileId(Sha256Digest([9; 32]))] }),
            serde_json::json!({ "employee": employee, "profiles": [] }),
            serde_json::json!({ "employee": employee, "profiles": [profile, profile] }),
            serde_json::json!({ "employee": employee, "profiles": [profile], "max_turns": 0 }),
            serde_json::json!({ "employee": employee, "profiles": [profile], "ready": true }),
            serde_json::json!({ "employee": "00".repeat(16), "profiles": [profile] }),
        ] {
            assert!(signed_policy_admits_entitlements(&bytes, &[bad.clone()]).is_err(), "{bad}");
        }
        assert!(signed_policy_admits_entitlements(&bytes, &[good.clone(), good]).is_err(), "one record per employee");
        assert!(signed_policy_admits_entitlements(b"{}", &[]).is_err(), "an unreadable policy admits nothing");
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
    /// R8z-3: each acceptance check that refuses is named, in the order of
    /// the former condition chain; `validate` refuses exactly then.
    #[test]
    fn r8z3_mcp_acceptance_names_the_first_refusing_check() {
        use duduclaw_mcp_boundary::config::AcceptanceCheck as C;
        let registration = duduclaw_mcp_boundary::config::RuntimeRegistration { runtime: RuntimeKind::OpenAiCompat,
            version: "api-worker-v1".into(), runtime_digest: Sha256Digest([1; 32]), bridge_digest: Sha256Digest([2; 32]),
            bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
        let projection = duduclaw_mcp_boundary::config::runtime_projection(registration).unwrap();
        let good = McpAcceptance { runtime: RuntimeKind::OpenAiCompat, version: "api-worker-v1".into(),
            runtime_digest: Sha256Digest([1; 32]), bridge_digest: Sha256Digest([2; 32]),
            projection_digest: projection.digest(), launch_policy_digest: Sha256Digest([4; 32]), inherited_fd: 3,
            initialize_exit: 0, parent_exec_exit: 0, mcp_child_exec_exit: 0, exclusive_config_exit: 0,
            transcript_digest: Sha256Digest([3; 32]) };
        assert_eq!(good.validate_check(&projection), Ok(()));
        let edits: [(C, fn(&mut McpAcceptance)); 11] = [
            (C::Runtime, |a| a.runtime = RuntimeKind::Claude),
            (C::Version, |a| a.version = "v-api-worker-v1".into()),
            (C::RuntimeDigest, |a| a.runtime_digest = Sha256Digest([9; 32])),
            (C::BridgeDigest, |a| a.bridge_digest = Sha256Digest([9; 32])),
            (C::ProjectionDigest, |a| a.projection_digest = Sha256Digest([9; 32])),
            (C::InheritedFd, |a| a.inherited_fd = 4),
            (C::InitializeExit, |a| a.initialize_exit = 1),
            (C::ParentExecExit, |a| a.parent_exec_exit = 1),
            (C::McpChildExecExit, |a| a.mcp_child_exec_exit = 1),
            (C::ExclusiveConfigExit, |a| a.exclusive_config_exit = 1),
            (C::TranscriptDigest, |a| a.transcript_digest = Sha256Digest([0; 32])),
        ];
        for (check, edit) in edits {
            let mut bad = good.clone();
            edit(&mut bad);
            assert_eq!(bad.validate_check(&projection), Err(check), "{check:?}");
            assert!(bad.validate(&projection).is_err(), "{check:?}");
        }
        // The launch-policy digest is `validate_launch`'s, not `validate`'s.
        let mut other = good.clone();
        other.launch_policy_digest = Sha256Digest([9; 32]);
        assert_eq!(other.validate_check(&projection), Ok(()));
        // The first failing check in order wins.
        let mut both = good;
        both.inherited_fd = 4; both.version = "other".into();
        assert_eq!(both.validate_check(&projection), Err(C::Version));
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

    fn native_record(record: &RuntimeRecord) -> NativeRecord {
        NativeRecord { vendor_digest: None, profile: record.profile,
            runtime: duduclaw_run::seccomp::RuntimeProfile { allowed_syscalls: vec![0] }, runtime_digest: Sha256Digest([4; 32]),
            forwarder: None, tmpfs_bytes: 4096, tmpfs_inodes: 32, proc_subset_pid: true, launcher: None }
    }
    fn v1_document() -> serde_json::Value {
        let record = runtime_record();
        serde_json::json!({ "version": 1, "runtimes": [record.clone()], "native": [native_record(&record)] })
    }
    fn launcher_value() -> serde_json::Value {
        serde_json::json!({ "reserve": { "memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1 }, "helpers": [],
            "helper_tmpfs_bytes": 1 })
    }
    #[test]
    fn policy_v1_reads_as_no_launcher() {
        let policy = parse_policy(&serde_json::to_vec(&v1_document()).unwrap()).unwrap();
        assert_eq!(policy.version, 1);
        assert!(policy.native.iter().all(|native| native.launcher.is_none()));
        assert!(launchers(&policy).is_empty());
        // A v1 document cannot carry a launcher or the v2 domain.
        let mut smuggled = v1_document(); smuggled["native"][0]["launcher"] = launcher_value();
        assert!(parse_policy(&serde_json::to_vec(&smuggled).unwrap()).is_err());
        let mut relabelled = v1_document(); relabelled["domain"] = POLICY_DOMAIN_V2.into();
        assert!(parse_policy(&serde_json::to_vec(&relabelled).unwrap()).is_err());
        for version in [0, 3, 4, 65535] {
            let mut other = v1_document(); other["version"] = version.into();
            assert!(parse_policy(&serde_json::to_vec(&other).unwrap()).is_err());
        }
    }
    #[test]
    fn v2_reader_v1_policy_means_no_launcher() {
        let policy = parse_policy(&serde_json::to_vec(&v1_document()).unwrap()).unwrap();
        validate_launchers(&policy).unwrap();
        let record = runtime_record();
        let employee = EmployeeId([5; 16]);
        let root = RootPolicy::from_verified_publication(vec![record.to_runtime()],
            vec![EmployeeEntitlement { employee, profiles: vec![record.profile], max_turns: None }]).unwrap()
            .with_launchers(duduclaw_run::seccomp::Architecture::native().unwrap(), launchers(&policy)).unwrap();
        // No launcher: the helper table is empty, so every helper call is refused.
        assert!(root.helper_table(employee, &record.to_runtime()).is_empty());
        assert!(root.helper_tmpfs_bytes(&record.to_runtime()).is_none());
        assert!(root.derive_helper(employee, &record.to_runtime(), &RuntimeKind::Registered(
            Text::new("duduclaw-program-v1".into()).unwrap())).is_err());
        // The MCP launch-policy digest input of a launcher-less record is unchanged.
        let value = serde_json::to_value(native_record(&record)).unwrap();
        assert!(value.get("launcher").is_none());
    }
    #[test]
    fn v2_bytes_not_valid_as_v1() {
        let record = runtime_record();
        let v2 = SignedPolicyV2 { domain: POLICY_DOMAIN_V2.into(), version: 2, runtimes: vec![record.clone()],
            native: vec![native_record(&record)] };
        let bytes = serde_json::to_vec(&v2).unwrap();
        // The deployed v1 reader (deny_unknown_fields) refuses v2 bytes outright.
        assert!(serde_json::from_slice::<SignedPolicyV1>(&bytes).is_err());
        assert_eq!(parse_policy(&bytes).unwrap().version, 2);
        // Relabelling the version keeps the domain, so it is still not a v1 document.
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["version"] = 1.into();
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
        // v2 requires its exact domain.
        for domain in [serde_json::Value::Null, "duduclaw-isolation-policy-v1".into(), "".into()] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if domain.is_null() { value.as_object_mut().unwrap().remove("domain"); } else { value["domain"] = domain; }
            assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        // A v2 launcher must still pass checks 1–7: an empty table fails closed.
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["native"][0]["launcher"] = launcher_value();
        let policy = parse_policy(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(validate_launchers(&policy).is_err());
    }

    /// A v3 Claude record: signed arguments without `--max-turns`, the fixed
    /// MCP suffix last, and the Q12 working-default range (pending owner).
    fn ranged_record() -> RuntimeRecord {
        let mut record = runtime_record();
        record.fixed_args = crate::gateway_plan::claude_ranged_arguments("model").unwrap().into_iter()
            .map(|arg| Text::new(arg).unwrap()).collect();
        record.turn_limit = Some(crate::policy::WORKING_DEFAULT_TURN_LIMIT);
        record
    }
    fn v3_document() -> serde_json::Value {
        let record = ranged_record();
        serde_json::json!({ "domain": POLICY_DOMAIN_V3, "version": 3, "runtimes": [record.clone()],
            "native": [native_record(&record)] })
    }
    #[test]
    fn policy_v3_carries_the_signed_range_under_its_own_domain() {
        let policy = parse_policy(&serde_json::to_vec(&v3_document()).unwrap()).unwrap();
        assert_eq!(policy.version, 3);
        assert_eq!(policy.runtimes[0].turn_limit, Some(crate::policy::WORKING_DEFAULT_TURN_LIMIT));
        validate_launchers(&policy).unwrap();
        // The range reaches the gateway projection and the root policy unchanged.
        let report = GatewayPublication { version: 1, generation: Sha256Digest([1; 32]), boot_id: "boot".into(),
            checked_boottime_ms: 50, valid_for_ms: 1000, runtimes: policy.runtimes,
            entitlements: vec![EntitlementRecord { employee: EmployeeId([5; 16]), profiles: vec![ProfileId(Sha256Digest([1; 32]))],
                max_turns: Some(30) }] };
        let projected: GatewayPublication = serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
        let root = projected.policy().unwrap();
        assert_eq!(root.turn_cap(EmployeeId([5; 16])), Some(30));
        // v3 needs its exact domain; relabelling as v1/v2 is refused.
        for (version, domain) in [(3, serde_json::Value::Null), (3, POLICY_DOMAIN_V2.into()), (2, POLICY_DOMAIN_V3.into()),
            (1, POLICY_DOMAIN_V3.into())] {
            let mut value = v3_document();
            value["version"] = version.into();
            if domain.is_null() { value.as_object_mut().unwrap().remove("domain"); } else { value["domain"] = domain; }
            assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err(), "{version}");
        }
        // The deployed v1 and v2 readers refuse v3 bytes outright.
        let bytes = serde_json::to_vec(&v3_document()).unwrap();
        assert!(serde_json::from_slice::<SignedPolicyV1>(&bytes).is_err());
        // An unknown field inside the range is refused.
        let mut value = v3_document(); value["runtimes"][0]["turn_limit"]["step"] = 1.into();
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn policy_v3_with_any_invalid_range_is_refused_as_a_whole() {
        // A second, valid record does not rescue the document.
        let mut second = runtime_record();
        second.runtime = RuntimeKind::Codex; second.profile = ProfileId(Sha256Digest([9; 32]));
        let bads: Vec<serde_json::Value> = vec![
            serde_json::json!({"min": 0, "max": 200, "default": 50}),
            serde_json::json!({"min": 60, "max": 200, "default": 50}),
            serde_json::json!({"min": 1, "max": 40, "default": 50}),
            serde_json::json!({"min": 1, "max": 1001, "default": 50}),
            serde_json::json!({"min": 1, "max": 200}),
            serde_json::json!({"min": -1, "max": 200, "default": 50}),
            serde_json::json!({"min": 1, "max": 70000, "default": 50}),
            serde_json::json!("1..200"),
        ];
        for bad in bads {
            let mut value = v3_document();
            value["runtimes"][0]["turn_limit"] = bad.clone();
            value["runtimes"].as_array_mut().unwrap().push(serde_json::to_value(&second).unwrap());
            value["native"].as_array_mut().unwrap().push(serde_json::to_value(native_record(&second)).unwrap());
            assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err(), "{bad}");
        }
        // A ranged record must not also carry a fixed `--max-turns`.
        let mut value = v3_document();
        let arguments = value["runtimes"][0]["fixed_args"].as_array_mut().unwrap();
        arguments.insert(0, serde_json::Value::from("500"));
        arguments.insert(0, serde_json::Value::from("--max-turns"));
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
        // A range on a record that is not a Claude MCP runtime.
        let mut value = v3_document(); value["runtimes"][0]["runtime"] = serde_json::to_value(RuntimeKind::Codex).unwrap();
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn policy_v1_v2_unaffected_and_cannot_carry_a_range() {
        // v1/v2 keep parsing exactly as before; no record has a range.
        let v1 = parse_policy(&serde_json::to_vec(&v1_document()).unwrap()).unwrap();
        assert!(v1.runtimes.iter().all(|record| record.turn_limit.is_none()));
        let record = runtime_record();
        let v2 = SignedPolicyV2 { domain: POLICY_DOMAIN_V2.into(), version: 2, runtimes: vec![record.clone()],
            native: vec![native_record(&record)] };
        let v2_bytes = serde_json::to_vec(&v2).unwrap();
        assert!(parse_policy(&v2_bytes).unwrap().runtimes.iter().all(|record| record.turn_limit.is_none()));
        // The serialized record (input of the MCP launch-policy digest) has no
        // new key, so deployed v1/v2 acceptances keep their digests.
        assert!(serde_json::to_value(&record).unwrap().get("turn_limit").is_none());
        // A range in a v1 or v2 document is refused as a whole.
        let range = serde_json::to_value(crate::policy::WORKING_DEFAULT_TURN_LIMIT).unwrap();
        let mut value = v1_document(); value["runtimes"][0]["turn_limit"] = range.clone();
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&v2_bytes).unwrap();
        value["runtimes"][0]["turn_limit"] = range;
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err());
        // R6 review round, finding 3: the key itself is refused before v3,
        // also as an explicit `null` (which the typed record reads as absent).
        let mut value = v1_document(); value["runtimes"][0]["turn_limit"] = serde_json::Value::Null;
        assert!(serde_json::from_slice::<SignedPolicyV1>(&serde_json::to_vec(&value).unwrap()).unwrap().runtimes[0].turn_limit.is_none());
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err(), "v1 null");
        let mut value: serde_json::Value = serde_json::from_slice(&v2_bytes).unwrap();
        value["runtimes"][0]["turn_limit"] = serde_json::Value::Null;
        assert!(parse_policy(&serde_json::to_vec(&value).unwrap()).is_err(), "v2 null");
        // Host entitlements without `max_turns` keep their exact shape.
        let entitlement: EntitlementRecord = serde_json::from_value(serde_json::json!({
            "employee": EmployeeId([1; 16]), "profiles": [] })).unwrap();
        assert!(entitlement.max_turns.is_none());
        assert!(serde_json::to_value(&entitlement).unwrap().get("max_turns").is_none());
        assert!(serde_json::from_value::<EntitlementRecord>(serde_json::json!({
            "employee": EmployeeId([1; 16]), "profiles": [], "max_turns": 30, "ready": true })).is_err());
    }

    /// R6 review round, findings 1–3: every signed Claude MCP record carries
    /// exactly one enforceable step limit, in every policy version; a v3
    /// `"turn_limit": null` is refused (absent is the only "no range").
    #[test]
    fn policy_claude_mcp_step_limit_shape_is_refused_as_a_whole() {
        let mut second = runtime_record();
        second.runtime = RuntimeKind::Codex; second.profile = ProfileId(Sha256Digest([9; 32])); second.fixed_args.clear();
        let parses = |mut value: serde_json::Value| {
            value["runtimes"].as_array_mut().unwrap().push(serde_json::to_value(&second).unwrap());
            value["native"].as_array_mut().unwrap().push(serde_json::to_value(native_record(&second)).unwrap());
            parse_policy(&serde_json::to_vec(&value).unwrap()).is_ok()
        };
        let legacy = serde_json::to_value(crate::gateway_plan::claude_arguments("model").unwrap()).unwrap();
        let ranged = serde_json::to_value(crate::gateway_plan::claude_ranged_arguments("model").unwrap()).unwrap();
        let unranged = |mut value: serde_json::Value| {
            value["runtimes"][0].as_object_mut().unwrap().remove("turn_limit");
            value
        };
        assert!(parses(v3_document()));
        assert!(parses(v1_document()));
        // v3 without a range keeps the canonical fixed 50.
        let mut value = unranged(v3_document()); value["runtimes"][0]["fixed_args"] = legacy.clone();
        assert!(parses(value));
        // v3: an explicit null is not "no range".
        let mut value = v3_document(); value["runtimes"][0]["turn_limit"] = serde_json::Value::Null;
        assert!(!parses(value), "v3 null");
        // Neither a range nor the fixed flag (would launch with no step limit).
        assert!(!parses(unranged(v3_document())), "v3 neither");
        let mut value = v1_document(); value["runtimes"][0]["fixed_args"] = ranged.clone();
        assert!(!parses(value), "v1 neither");
        // Both.
        let mut value = v3_document(); value["runtimes"][0]["fixed_args"] = legacy.clone();
        assert!(!parses(value), "v3 both");
        // The fixed flag with another value, duplicated or in `=` spelling.
        let position = crate::policy::CLAUDE_FIXED_PREFIX.len();
        for (index, replacement) in [(position + 1, "500"), (position, "--max-turns=50")] {
            let mut value = v1_document(); value["runtimes"][0]["fixed_args"][index] = replacement.into();
            assert!(!parses(value), "{replacement}");
        }
        let mut value = v1_document();
        let arguments = value["runtimes"][0]["fixed_args"].as_array_mut().unwrap();
        arguments.insert(position, "50".into()); arguments.insert(position, "--max-turns".into());
        assert!(!parses(value), "duplicated");
        // A bare `--` before the suffix, or the insertion point after a
        // value-taking option.
        for (index, smuggled) in [(14, "--"), (1, "--"), (14, "--model")] {
            let mut value = v3_document();
            value["runtimes"][0]["fixed_args"].as_array_mut().unwrap().insert(index, smuggled.into());
            assert!(!parses(value), "{index} {smuggled}");
        }
    }

    /// R7b: v4 = the v3 shape under its own domain plus `dns_probe_name`.
    fn v4_document() -> serde_json::Value {
        let mut value = v3_document();
        value["domain"] = POLICY_DOMAIN_V4.into();
        value["version"] = 4.into();
        value["dns_probe_name"] = "api.example.com".into();
        value
    }
    fn parses(value: &serde_json::Value) -> bool { parse_policy(&serde_json::to_vec(value).unwrap()).is_ok() }
    #[test]
    fn policy_v4_carries_the_probe_name_and_older_versions_have_none() {
        let bytes = serde_json::to_vec(&v4_document()).unwrap();
        let policy = parse_policy(&bytes).unwrap();
        assert_eq!((policy.version, policy.dns_probe_name.as_deref()), (4, Some("api.example.com")));
        // Everything v3 carries still reaches the policy.
        assert_eq!(policy.runtimes[0].turn_limit, Some(crate::policy::WORKING_DEFAULT_TURN_LIMIT));
        validate_launchers(&policy).unwrap();
        assert_eq!(signed_policy_dns_probe_name(&bytes).unwrap().as_deref(), Some("api.example.com"));
        assert_eq!(signed_policy_egress_profiles(&bytes).unwrap(), Vec::<ProfileId>::new());
        // v1–v3: no probe name (spawnd then never reports the DNS route as verified).
        let record = runtime_record();
        let v2 = serde_json::to_value(SignedPolicyV2 { domain: POLICY_DOMAIN_V2.into(), version: 2,
            runtimes: vec![record.clone()], native: vec![native_record(&record)] }).unwrap();
        for older in [v1_document(), v2, v3_document()] {
            let bytes = serde_json::to_vec(&older).unwrap();
            assert_eq!(parse_policy(&bytes).unwrap().dns_probe_name, None);
            assert_eq!(signed_policy_dns_probe_name(&bytes).unwrap(), None);
        }
        // The deployed v1–v3 readers refuse v4 bytes outright.
        assert!(serde_json::from_slice::<SignedPolicyV1>(&bytes).is_err());
        assert!(serde_json::from_slice::<SignedPolicyV3>(&bytes).is_err());
    }
    #[test]
    fn policy_v4_refusals() {
        // Missing, null or non-string probe name.
        let mut value = v4_document(); value.as_object_mut().unwrap().remove("dns_probe_name");
        assert!(!parses(&value), "missing");
        for bad in [serde_json::Value::Null, 7.into(), serde_json::json!(["api.example.com"])] {
            let mut value = v4_document(); value["dns_probe_name"] = bad.clone();
            assert!(!parses(&value), "{bad}");
        }
        // Each part of the name rule.
        let long_label = format!("{}.com", "a".repeat(64));
        let long_name = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        for name in ["", "com", "localhost", "API.example.com", "api.example.com.", "-api.example.com", "api-.example.com",
            "api_x.example.com", "api..example.com", "192.0.2.1", "printer.local", "db.internal", "app.localhost",
            "\u{e4}pi.example.com", long_label.as_str(), long_name.as_str()] {
            let mut value = v4_document(); value["dns_probe_name"] = name.into();
            assert!(!parses(&value), "{name}");
        }
        // v4 needs its exact domain; relabelling either way is refused.
        for (version, domain) in [(4, serde_json::Value::Null), (4, POLICY_DOMAIN_V3.into()), (3, POLICY_DOMAIN_V4.into())] {
            let mut value = v4_document(); value["version"] = version.into();
            if domain.is_null() { value.as_object_mut().unwrap().remove("domain"); } else { value["domain"] = domain; }
            assert!(!parses(&value), "{version}");
        }
        // An unknown top-level member is refused as before.
        let mut value = v4_document(); value["dns_route"] = "resolved_varlink".into();
        assert!(!parses(&value));
        // v1–v3 refuse the key, also as an explicit null.
        let record = runtime_record();
        let v2 = serde_json::to_value(SignedPolicyV2 { domain: POLICY_DOMAIN_V2.into(), version: 2,
            runtimes: vec![record.clone()], native: vec![native_record(&record)] }).unwrap();
        for older in [v1_document(), v2, v3_document()] {
            assert!(parses(&older));
            for key in [serde_json::Value::from("api.example.com"), serde_json::Value::Null] {
                let mut value = older.clone(); value["dns_probe_name"] = key.clone();
                assert!(!parses(&value), "{} {key}", older["version"]);
            }
        }
        // The v3 turn-limit rules still apply to v4 as a whole.
        let mut value = v4_document(); value["runtimes"][0]["turn_limit"] = serde_json::Value::Null;
        assert!(!parses(&value));
    }
    #[test]
    fn configured_dns_route_is_the_single_route_of_valid_profiles() {
        use std::num::{NonZeroU16, NonZeroU32};
        let limits = duduclaw_isolation::egress::EgressLimits { max_frame_bytes: NonZeroU32::new(65536).unwrap(),
            max_tunnels: NonZeroU16::new(16).unwrap(), frames_per_second: NonZeroU32::new(100).unwrap(),
            bytes_per_second: NonZeroU32::new(1024).unwrap(), connect_timeout_ms: NonZeroU32::new(5000).unwrap(),
            idle_timeout_ms: NonZeroU32::new(30000).unwrap(), max_header_bytes: NonZeroU32::new(8192).unwrap(),
            max_headers: NonZeroU16::new(32).unwrap() };
        let profile = |seed: u8, dns_route| EgressProfile { id: ProfileId(Sha256Digest([seed; 32])),
            allowed_connect_ports: duduclaw_isolation::bounded::List::new(vec![NonZeroU16::new(443).unwrap()]).unwrap(),
            dns_route, limits: limits.clone() };
        assert_eq!(single_route(&[profile(1, DnsRoute::DedicatedDnsBroker), profile(2, DnsRoute::DedicatedDnsBroker)]).unwrap(),
            DnsRoute::DedicatedDnsBroker);
        assert!(single_route(&[]).is_err());
        assert!(single_route(&[profile(1, DnsRoute::ResolvedVarlink), profile(2, DnsRoute::DedicatedDnsBroker)]).is_err());
        let mut invalid = profile(1, DnsRoute::ResolvedVarlink);
        invalid.allowed_connect_ports = duduclaw_isolation::bounded::List::new(vec![]).unwrap();
        assert!(single_route(&[invalid]).is_err());
        // The installer's document shape: extra top-level members are egress's;
        // a profile's shape is strict, and the route spelling is the product's.
        let document = serde_json::json!({ "profiles": [profile(1, DnsRoute::DedicatedDnsBroker)], "run_caps": {}, "max_runs": 1 });
        assert_eq!(document["profiles"][0]["dns_route"], "dedicated_dns_broker");
        let configuration: EgressConfiguration = serde_json::from_value(document.clone()).unwrap();
        assert_eq!(single_route(&configuration.profiles).unwrap(), DnsRoute::DedicatedDnsBroker);
        let mut legacy = document.clone(); legacy["profiles"][0]["dns_route"] = "dedicated_broker".into();
        assert!(serde_json::from_value::<EgressConfiguration>(legacy).is_err());
        let mut extra = document; extra["profiles"][0]["ready"] = true.into();
        assert!(serde_json::from_value::<EgressConfiguration>(extra).is_err());
    }

    /// R6 on a real installed host: for every employee entitled to a ranged
    /// (policy v3) Claude runtime in the live root publication, nothing the
    /// gateway can ask for exceeds the signed maximum or the employee's root
    /// ceiling, and a request above either is refused rather than clamped.
    /// The real run chain (`tests/claude_chain.rs`) passes the same value.
    #[test]
    #[ignore = "requires root native Linux installed protected host with a policy v3 Claude runtime"]
    fn iso_root_e2e_max_turns_entitlement_narrows_only() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let (_, gid) = crate::server::gateway_identity().unwrap();
        let publication = GatewayPublication::read(gid).expect("fresh installed signed root publication");
        let policy = publication.policy().unwrap();
        let mut checked = 0;
        for entitlement in &publication.entitlements {
            let Ok((claude, _)) = policy.derive(entitlement.employee, &RuntimeKind::Claude, false) else { continue };
            let Some(limit) = claude.turn_limit else { continue };
            let employee = entitlement.employee;
            let cap = entitlement.max_turns.unwrap_or(limit.max).min(limit.max);
            for desired in [u64::MAX, u64::from(limit.max) + 1, u64::from(limit.default), 50] {
                let request = crate::policy::narrowed_request(limit, entitlement.max_turns, desired);
                match policy.max_turns(employee, claude, Some(request)) {
                    Ok(value) => assert!(value == Some(request) && request <= cap),
                    Err(_) => assert!(request < limit.min),
                }
            }
            assert!(policy.max_turns(employee, claude, Some(limit.max.saturating_add(1))).is_err());
            if cap < limit.max { assert!(policy.max_turns(employee, claude, Some(cap + 1)).is_err()); }
            match policy.max_turns(employee, claude, None) {
                Ok(Some(value)) => assert!(limit.min <= value && value <= cap),
                Ok(None) => panic!("a ranged record always yields a value"),
                Err(_) => assert!(cap < limit.min),
            }
            checked += 1;
        }
        assert!(checked > 0, "no employee is entitled to a policy v3 ranged Claude runtime on this host");
    }

}

#[cfg(test)]
#[path = "installed_root_tests.rs"]
mod root_acceptance;

#[cfg(test)]
#[path = "installed_account_root_tests.rs"]
mod account_root_acceptance;

/// R7d: the isolation policy generator behind `duduclaw-isolation-policy`
/// (feature `policy-tool`, never a release binary). A child module so it runs
/// this module's own readers and checks on its output, never copies of them.
#[cfg(any(feature = "policy-tool", test))]
#[path = "installed_policy_tool.rs"]
pub mod policy_tool;
