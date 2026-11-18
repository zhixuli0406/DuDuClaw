//! R8r-4: `duduclaw-isolation-policy accept`, the MCP acceptance harness
//! (feature `policy-tool`, never a release build; README "MCP acceptance
//! harness"), and `verify --evidence-dir`.
//!
//! `accept` produces the `mcp_acceptance` record of ONE `open_ai_compat`
//! runtime (the api worker) by running it, never by asserting it: the policy
//! is generated exactly as `build` does ([`build_policy_bytes`]), the run is
//! launched through the production launch pieces on a disposable host
//! ([`harness`]), the harness plays the gateway end of descriptor 3
//! ([`Responder`]) and observes the process facts itself ([`Observations`]).
//! Only when every fact was positively observed ([`decide`]) does it write the
//! evidence and the output source; a record never carries a non-zero exit.
//!
//! This file holds the parts that need no privilege (gates, the responder,
//! the decision, the evidence encoding and checks, the output source); the
//! root launch is [`harness`].
use super::*;
use serde_json::{json, Value};
use std::time::Duration;

#[path = "installed_policy_tool_harness.rs"]
mod harness;

/// The convention of every root test: set only on disposable hosts.
pub(super) const DISPOSABLE_ENV: &str = "DUDUCLAW_ISO_DISPOSABLE";
pub(super) const EVIDENCE_DOMAIN: &str = "duduclaw-runtime-mcp-acceptance-evidence-v1";
pub(super) const EVIDENCE_FORMAT: u64 = 1;
pub(super) const MANIFEST_FILE: &str = "acceptance-manifest.json";
pub(super) const TRANSCRIPT_FILE: &str = "transcript.jsonl";
/// Hard limits of the conversation on descriptor 3 (both directions).
pub(super) const MAX_FRAMES: usize = 64;
pub(super) const MAX_TRANSCRIPT_BYTES: usize = 256 * 1024;
pub(super) const CONVERSATION_LIMIT: Duration = Duration::from_secs(30);
/// Transcript (≤ 256 KiB of payload, base64 plus line overhead) and manifest.
const MAX_EVIDENCE_FILE_BYTES: u64 = 1024 * 1024;
pub(super) const BRIDGE_EXECUTABLE: &str = "/runtime/duduclaw-mcp-bridge";
/// The only MCP configuration the api worker accepts (duduclaw-api-worker
/// `verify_config`). spawnd does not depend on that crate; the harness
/// re-checks the mounted projection against this value.
pub(super) const API_WORKER_CONFIG: &str = r#"{"mcpServers":{"duduclaw":{"command":"/runtime/duduclaw-mcp-bridge","args":[]}}}"#;

/// `duduclaw_core::isolation_install::FACTS`. spawnd's production graph does
/// not depend on duduclaw-core (and this package adds no dependency); the
/// equality is tested (`install_facts_are_duduclaw_core_facts`).
pub(super) const INSTALL_FACTS: &[&str] = &[
    duduclaw_isolation::ids::SPAWND_SOCKET,
    "/run/duduclaw-parse",
    "/usr/lib/tmpfiles.d/duduclaw-parse.conf",
    "/etc/systemd/system/duduclaw-spawnd.socket",
    "/etc/systemd/system/duduclaw-spawnd.service",
    "/etc/systemd/system/duduclaw-isolation-ready.service",
    "/etc/systemd/system/duduclaw-isolation-selftest.service",
    "/etc/systemd/system/duduclaw-isolation-probe@.service",
    "/etc/systemd/system/duduclaw-employees.slice",
    "/etc/systemd/system/duduclaw-egress.socket",
    "/etc/systemd/system/duduclaw-egress.service",
    "/etc/systemd/system/duduclaw-dns-broker.socket",
    "/etc/systemd/system/duduclaw-dns-broker.service",
    "/etc/systemd/system/duduclaw-parse.socket",
    "/etc/systemd/system/duduclaw-parse@.service",
    "/etc/systemd/system/duduclaw-updated.service",
    "/etc/systemd/system/duduclaw-import@.service",
    "/etc/systemd/system/duduclaw-migrate-expire@.service",
    "/etc/systemd/system/duduclaw-migrate-expire@.timer",
    "/usr/lib/duduclaw",
    "/usr/lib/duduclaw/parse-root",
    "/usr/lib/duduclaw/parse-root-tree",
    "/usr/lib/duduclaw/current",
    "/usr/libexec/duduclaw",
    "/var/lib/duduclaw-spawnd",
    "/var/lib/duduclaw-spawnd/etc",
    "/etc/duduclaw-updated",
    "/var/lib/duduclaw-updated",
    "/opt/duduclaw-isolated",
    "/opt/duduclaw-isolated/generations",
    "/opt/duduclaw-isolated/slots",
    "/opt/duduclaw-isolated/slots/a",
    "/opt/duduclaw-isolated/slots/b",
    "/var/lib/duduclaw-migrate",
    "/opt/duduclaw-isolated/current",
    "/var/lib/duduclaw-updated/state.json",
    "/etc/duduclaw/migration-import.json",
    "/usr/lib/duduclaw/duduclaw-spawnd",
    "/usr/lib/duduclaw/duduclaw-run",
    "/usr/lib/duduclaw/duduclaw-run-selftest",
    "/usr/lib/duduclaw/duduclaw-isolation-probe",
    "/opt/duduclaw/bin/duduclaw-egress",
    "/usr/libexec/duduclaw/duduclaw-parse-worker",
    "/usr/lib/duduclaw/current/duduclaw-import",
    "/usr/lib/duduclaw/current/duduclaw-migrate",
    "/usr/lib/duduclaw/current/duduclaw-mcp-bridge",
    "/usr/lib/duduclaw/current/duduclaw",
    "/usr/libexec/duduclaw-updated-launch",
];
/// `duduclaw_core::protected_mode::MODE_MARKER` (equality tested).
pub(super) const PROTECTED_MODE_MARKER: &str = "/etc/duduclaw/protected-mode";

/// Fixed refusals (exit 1), each before any side effect.
pub(super) mod refusal {
    pub const NOT_LINUX: &str = "accept: not supported on this host: Linux only";
    pub const NOT_ROOT: &str = "accept: refused: run as root (effective uid 0 and effective gid 0)";
    pub const NOT_DISPOSABLE: &str = "accept: refused: DUDUCLAW_ISO_DISPOSABLE=1 is required (disposable hosts only)";
    pub const INSTALLED: &str = "accept: refused: this host has an isolation installation or the protected-mode marker; \
        run accept only on a disposable host without either";
    pub const WRONG_ARCH: &str = "accept: refused: --arch is not this host's architecture";
    pub const EVIDENCE_DIR: &str = "accept: refused: --evidence-dir must not exist or must be an empty directory (not a symbolic link)";
    pub const OUT_SOURCE: &str = "accept: refused: --out-source must not exist and its directory must exist";
    pub const OUT_IS_SOURCE: &str = "accept: refused: --out-source must not be --source (the input is never modified)";
    pub const TRAMPOLINE: &str = "accept: refused: --trampoline must be a regular file (the duduclaw-run binary)";
    pub const LABEL_MISSING: &str = "accept: refused: no runtime with this --label in the source";
    pub const NOT_API_WORKER: &str = "accept: refused: the labelled runtime is not open_ai_compat with mcp: true \
        (this harness covers only the api worker)";
    pub const ALREADY_ACCEPTED: &str = "accept: refused: the labelled runtime already has mcp_acceptance";
    pub const LAUNCHER: &str = "accept: not supported on this host: a runtime with an in-run helper launcher \
        (the harness plans no helper table)";
}

// ── host facts and gates ────────────────────────────────────────────────

/// What the gates decide on; tests inject their own.
#[derive(Clone)]
pub(super) struct HostFacts {
    pub linux: bool,
    pub euid: u32,
    pub egid: u32,
    pub disposable: bool,
    pub installed: bool,
    pub protected_marker: bool,
    pub arch: Option<Arch>,
}
impl HostFacts {
    pub(super) fn current() -> Self {
        Self {
            linux: cfg!(target_os = "linux"),
            euid: unsafe { libc::geteuid() },
            egid: unsafe { libc::getegid() },
            disposable: std::env::var_os(DISPOSABLE_ENV).is_some_and(|value| value.as_os_str() == std::ffi::OsStr::new("1")),
            installed: installed_under(Path::new("/")),
            protected_marker: present(Path::new(PROTECTED_MODE_MARKER)),
            arch: Arch::native().ok(),
        }
    }
}

/// Only NotFound proves absence (as `isolation_install::present`).
fn present(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

/// `isolation_install::installed_under`: any fact present below `root`.
pub(super) fn installed_under(root: &Path) -> bool {
    INSTALL_FACTS.iter().any(|fact| present(&root.join(fact.trim_start_matches('/'))))
}

pub(super) struct Request {
    pub source: PathBuf,
    pub artifacts: PathBuf,
    pub arch: Arch,
    pub label: String,
    pub trampoline: PathBuf,
    pub evidence: PathBuf,
    pub out_source: PathBuf,
}

fn parse_request(args: &[String]) -> Checked<Request> {
    let values = options(args, &["--source", "--artifacts-dir", "--arch", "--label", "--trampoline", "--evidence-dir", "--out-source"])?;
    let get = |key: &str| values.get(key).cloned().ok_or_else(|| format!("accept needs {key}"));
    let request = Request { source: get("--source")?.into(), artifacts: get("--artifacts-dir")?.into(),
        arch: parse_arch(&get("--arch")?)?, label: get("--label")?, trampoline: get("--trampoline")?.into(),
        evidence: get("--evidence-dir")?.into(), out_source: get("--out-source")?.into() };
    check_label("--label", &request.label)?;
    Ok(request)
}

/// Gate 0 in the documented order; no side effect.
pub(super) fn host_gate(facts: &HostFacts, request: &Request) -> std::result::Result<(), &'static str> {
    if !facts.linux { return Err(refusal::NOT_LINUX); }
    if facts.euid != 0 || facts.egid != 0 { return Err(refusal::NOT_ROOT); }
    if !facts.disposable { return Err(refusal::NOT_DISPOSABLE); }
    if facts.installed || facts.protected_marker { return Err(refusal::INSTALLED); }
    if facts.arch != Some(request.arch) { return Err(refusal::WRONG_ARCH); }
    match fs::symlink_metadata(&request.evidence) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.is_dir() => {
            let empty = fs::read_dir(&request.evidence).map(|mut entries| entries.next().is_none()).unwrap_or(false);
            if !empty { return Err(refusal::EVIDENCE_DIR); }
        }
        _ => return Err(refusal::EVIDENCE_DIR),
    }
    if !matches!(fs::symlink_metadata(&request.out_source), Err(error) if error.kind() == io::ErrorKind::NotFound) {
        return Err(refusal::OUT_SOURCE);
    }
    let out_parent = parent_directory(&request.out_source);
    if !fs::metadata(&out_parent).is_ok_and(|metadata| metadata.is_dir()) { return Err(refusal::OUT_SOURCE); }
    if request.out_source == request.source { return Err(refusal::OUT_IS_SOURCE); }
    if !fs::symlink_metadata(&request.trampoline).is_ok_and(|metadata| metadata.is_file()) { return Err(refusal::TRAMPOLINE); }
    Ok(())
}

/// The labelled runtime exists, is the api worker with MCP, and has no record yet.
pub(super) fn label_gate(source: &PolicySource, label: &str) -> std::result::Result<usize, &'static str> {
    let index = source.runtimes.iter().position(|runtime| runtime.label == label).ok_or(refusal::LABEL_MISSING)?;
    let runtime = &source.runtimes[index];
    if runtime.runtime != RuntimeKind::OpenAiCompat || !runtime.mcp { return Err(refusal::NOT_API_WORKER); }
    if runtime.mcp_acceptance.is_some() { return Err(refusal::ALREADY_ACCEPTED); }
    Ok(index)
}

fn parent_directory(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

// ── generation (the same code path as `build`) ──────────────────────────

/// Everything the launch and the record need, generated without side effects.
pub(super) struct Prepared {
    pub label: String,
    pub arch: Arch,
    pub artifacts: PathBuf,
    pub policy: SignedPolicy,
    pub manifest: ReleaseManifest,
    /// The generated policy bytes (no acceptance yet).
    pub generated: Vec<u8>,
    pub index: usize,
    pub native_index: usize,
    pub runtime_digest: Sha256Digest,
    pub bridge_digest: Sha256Digest,
    pub projection: duduclaw_mcp_boundary::config::RuntimeProjection,
    pub launch_policy_digest: Sha256Digest,
}
impl Prepared {
    pub(super) fn record(&self) -> &RuntimeRecord { &self.policy.runtimes[self.index] }
    pub(super) fn native(&self) -> &NativeRecord { &self.policy.native[self.native_index] }
}

/// Generate exactly as `build` does, then derive the record's digests: the
/// runtime and bridge digests from the artifact FILES the launch copies (equal
/// to the generated record and the manifest), the projection from the same
/// `runtime_projection` spawnd and the gateway use, and the launch-policy
/// digest from `McpAcceptance::launch_policy_digest` (what `validate_launch`
/// compares).
pub(super) fn prepare(request: &Request, raw: &[u8]) -> Checked<Prepared> {
    let (generated, policy) = build_policy_bytes(raw, &request.source, &request.artifacts, request.arch, None)?;
    let profile = profile_id(&request.label);
    let index = policy.runtimes.iter().position(|record| record.profile == profile).ok_or_else(|| refusal::LABEL_MISSING.to_owned())?;
    let native_index = policy.native.iter().position(|native| native.profile == profile)
        .ok_or_else(|| "accept: refused: no native record for the labelled runtime".to_owned())?;
    let manifest = load_manifest(&request.artifacts, None, request.arch, false)?;
    let (runtime_digest, bridge_digest, projection, launch_policy_digest) = {
        let record = &policy.runtimes[index];
        let native = &policy.native[native_index];
        if record.runtime != RuntimeKind::OpenAiCompat || !record.mcp { return Err(refusal::NOT_API_WORKER.into()); }
        if record.mcp_acceptance.is_some() { return Err(refusal::ALREADY_ACCEPTED.into()); }
        if native.launcher.is_some() { return Err(refusal::LAUNCHER.into()); }
        let ExecutableRecord::Elf { entry, digest } = &record.executable else {
            return Err("accept: refused: the labelled runtime is not an ELF entry".into());
        };
        let mismatch = |what: &str| format!("accept: refused: {what} file differs from the generated record / manifest digest");
        let (file, _) = file_digest(&request.artifacts.join(entry.as_str()))?;
        let signed = manifest.artifacts.iter().find(|a| artifact_name(&a.install_target).is_ok_and(|name| name == entry.as_str()))
            .ok_or_else(|| mismatch("the runtime"))?;
        if file != *digest || signed.sha256 != *digest { return Err(mismatch("the runtime")); }
        let bridge = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge)
            .ok_or_else(|| "accept: refused: --artifacts-dir has no duduclaw-mcp-bridge".to_owned())?;
        let bridge_name = artifact_name(&InstallTarget::McpBridge).map_err(|_| "bridge entry".to_owned())?;
        let (bridge_file, _) = file_digest(&request.artifacts.join(&bridge_name))?;
        if bridge_file != bridge.sha256 { return Err(mismatch("the MCP bridge")); }
        if let Some(forwarder) = &native.forwarder {
            let (forwarder_file, _) = file_digest(&request.artifacts.join(&forwarder.entry))?;
            if forwarder_file != forwarder.executable_digest { return Err(mismatch("the egress forwarder")); }
        }
        let projection = duduclaw_mcp_boundary::config::runtime_projection(duduclaw_mcp_boundary::config::RuntimeRegistration {
            runtime: record.runtime.clone(), version: record.pinned_version.as_str().into(), runtime_digest: *digest,
            bridge_digest: bridge.sha256, bridge_executable: BRIDGE_EXECUTABLE.into() })
            .map_err(|_| "accept: refused: no MCP projection for the labelled runtime".to_owned())?;
        if projection.inherited_mcp_fd != duduclaw_mcp_boundary::config::MCP_FD {
            return Err("accept: refused: the projection does not inherit descriptor 3".into());
        }
        let launch = McpAcceptance::launch_policy_digest(record, native)
            .map_err(|_| "accept: refused: launch-policy digest unavailable".to_owned())?;
        (*digest, bridge.sha256, projection, launch)
    };
    Ok(Prepared { label: request.label.clone(), arch: request.arch, artifacts: request.artifacts.clone(), policy, manifest,
        generated: generated.bytes, index, native_index, runtime_digest, bridge_digest, projection, launch_policy_digest })
}

// ── the gateway end of descriptor 3 ─────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction { RuntimeToHarness, HarnessToRuntime }
impl Direction {
    pub(super) fn name(self) -> &'static str {
        match self { Self::RuntimeToHarness => "runtime_to_harness", Self::HarnessToRuntime => "harness_to_runtime" }
    }
}
/// One newline-delimited JSON-RPC frame (payload without its `\n`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Frame { pub direction: Direction, pub payload: Vec<u8> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase { AwaitInitialize, InitializeHeld, AwaitInitialized, AwaitToolsList, Complete }

pub(super) const LIMIT_FRAMES: &str = "the MCP conversation exceeded 64 frames";
pub(super) const LIMIT_BYTES: &str = "the MCP conversation exceeded 256 KiB";

/// What the bridge needs and nothing else: `initialize` (reply held until the
/// process facts are observed), `notifications/initialized`, `tools/list`
/// (a fixed EMPTY inventory: `provider_tools` accepts it, so no tool is ever
/// offered and no `tools/call` can be valid). Every other message is refused
/// (a JSON-RPC error when it has an id) and counted. The bridge is a byte pump
/// (`duduclaw-mcp-bridge`); the api worker frames newline-delimited JSON.
pub(super) struct Responder {
    buffer: Vec<u8>,
    frames: Vec<Frame>,
    bytes: usize,
    phase: Phase,
    held: Option<Value>,
    initialize_requests: u32,
    refused: u32,
    observed_before_reply: bool,
    initialized_after_reply: bool,
    tools_list_answered: bool,
}
impl Default for Responder {
    fn default() -> Self {
        Self { buffer: Vec::new(), frames: Vec::new(), bytes: 0, phase: Phase::AwaitInitialize, held: None,
            initialize_requests: 0, refused: 0, observed_before_reply: false, initialized_after_reply: false,
            tools_list_answered: false }
    }
}
fn rpc_result(id: Value, result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result})).unwrap_or_default()
}
fn rpc_error(id: Value, code: i64, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})).unwrap_or_default()
}
impl Responder {
    /// Bytes from the runtime end; returns the bytes to write back now.
    pub(super) fn feed(&mut self, bytes: &[u8]) -> std::result::Result<Vec<u8>, &'static str> {
        if self.bytes + self.buffer.len() + bytes.len() > MAX_TRANSCRIPT_BYTES { return Err(LIMIT_BYTES); }
        self.buffer.extend_from_slice(bytes);
        self.drain()
    }
    /// Whether an `initialize` request waits for [`Self::release_initialize`].
    pub(super) fn initialize_held(&self) -> bool { self.phase == Phase::InitializeHeld }
    /// Reply to the held `initialize` (`observed`: the process facts were
    /// observed while it was held), then answer whatever followed it.
    pub(super) fn release_initialize(&mut self, observed: bool) -> std::result::Result<Vec<u8>, &'static str> {
        if self.phase != Phase::InitializeHeld { return Err("no initialize request is held"); }
        let id = self.held.take().ok_or("no initialize request is held")?;
        self.observed_before_reply = observed;
        let reply = rpc_result(id, json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
            "serverInfo": {"name": "duduclaw-mcp-acceptance-harness", "version": "1"}}));
        self.record(Direction::HarnessToRuntime, reply.clone())?;
        self.phase = Phase::AwaitInitialized;
        let mut out = reply;
        out.push(b'\n');
        out.extend(self.drain()?);
        Ok(out)
    }
    /// `tools/list` was answered: the conversation the record needs is over.
    pub(super) fn complete(&self) -> bool { self.phase == Phase::Complete }
    pub(super) fn frames(&self) -> &[Frame] { &self.frames }
    pub(super) fn facts(&self) -> InitializeFacts {
        InitializeFacts { initialize_requests: self.initialize_requests, observed_before_reply: self.observed_before_reply,
            initialized_after_reply: self.initialized_after_reply, tools_list_answered: self.tools_list_answered,
            refused_frames: self.refused, frames: self.frames.len() as u32 }
    }
    fn drain(&mut self) -> std::result::Result<Vec<u8>, &'static str> {
        let mut out = Vec::new();
        while self.phase != Phase::InitializeHeld {
            let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') else { break };
            let mut payload: Vec<u8> = self.buffer.drain(..=end).collect();
            payload.pop();
            self.record(Direction::RuntimeToHarness, payload.clone())?;
            if let Some(reply) = self.answer(&payload) {
                self.record(Direction::HarnessToRuntime, reply.clone())?;
                out.extend(reply);
                out.push(b'\n');
            }
        }
        Ok(out)
    }
    fn record(&mut self, direction: Direction, payload: Vec<u8>) -> std::result::Result<(), &'static str> {
        if self.frames.len() >= MAX_FRAMES { return Err(LIMIT_FRAMES); }
        self.bytes += payload.len() + 1;
        if self.bytes > MAX_TRANSCRIPT_BYTES { return Err(LIMIT_BYTES); }
        self.frames.push(Frame { direction, payload });
        Ok(())
    }
    fn answer(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let message = serde_json::from_slice::<Value>(payload).ok()
            .filter(|value| value.is_object() && value.get("jsonrpc").and_then(Value::as_str) == Some("2.0"));
        let Some(message) = message else {
            self.refused += 1;
            return Some(rpc_error(Value::Null, -32700, "not a JSON-RPC 2.0 message"));
        };
        let id = message.get("id").filter(|id| id.is_number() || id.is_string()).cloned();
        match (message.get("method").and_then(Value::as_str), id, self.phase) {
            (Some("initialize"), Some(id), Phase::AwaitInitialize) => {
                self.initialize_requests += 1;
                self.held = Some(id);
                self.phase = Phase::InitializeHeld;
                None
            }
            (Some("initialize"), id, _) => {
                self.initialize_requests += 1;
                self.refused += 1;
                id.map(|id| rpc_error(id, -32600, "initialize refused by the acceptance harness"))
            }
            (Some("notifications/initialized"), None, Phase::AwaitInitialized) if message.get("id").is_none() => {
                self.initialized_after_reply = true;
                self.phase = Phase::AwaitToolsList;
                None
            }
            (Some("tools/list"), Some(id), Phase::AwaitToolsList) => {
                self.tools_list_answered = true;
                self.phase = Phase::Complete;
                Some(rpc_result(id, json!({"tools": []})))
            }
            (_, id, _) => {
                self.refused += 1;
                id.map(|id| rpc_error(id, -32601, "refused by the acceptance harness"))
            }
        }
    }
}

// ── observations and the decision ───────────────────────────────────────

/// A socket by its inode and its (sockfs) mount id, as `fd::observe` reports
/// the runtime-side endpoint before the launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SocketId { pub inode: u64, pub mount_id: u64 }
/// The runtime process's descriptor 3: the expected socket, absent, or anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fd3 { ExpectedSocket, Absent, Other }
impl Fd3 {
    fn name(self) -> &'static str {
        match self { Self::ExpectedSocket => "expected_socket", Self::Absent => "absent", Self::Other => "other" }
    }
}
/// One process, observed while pinned by a pidfd.
#[derive(Clone)]
pub(super) struct ProcessFacts {
    pub pid: u32,
    /// Below the unit's main process (runtime) or below the runtime (bridge).
    pub depth: u32,
    pub uids: [u32; 4],
    pub exe_sha256: Sha256Digest,
    pub cgroup_member: bool,
    /// Every descriptor of the process whose target is the recorded socket.
    pub socket_fds: Vec<i32>,
    /// The pidfd still showed the same live process after every read.
    pub alive_through_observation: bool,
}
#[derive(Clone, Default)]
pub(super) struct InitializeFacts {
    pub initialize_requests: u32,
    pub observed_before_reply: bool,
    pub initialized_after_reply: bool,
    pub tools_list_answered: bool,
    pub refused_frames: u32,
    pub frames: u32,
}
#[derive(Clone, Default)]
pub(super) struct ExclusiveFacts {
    /// The projection file is byte-exact the projection's `mcp.json`.
    pub projection_bytes_exact: bool,
    /// ... and equals the only configuration the api worker accepts.
    pub projection_is_api_worker_config: bool,
    /// `verify_mcp_projection`'s mount id of the projection handed to B2.
    pub projection_mount_id: u64,
    pub decoys_planted: u32,
    pub decoy_markers_present: u32,
    /// Distinct processes with the bridge digest seen in the run cgroup.
    pub bridge_processes_seen: u32,
}
#[derive(Clone)]
pub(super) struct Observations {
    pub uid: u32,
    pub socket: SocketId,
    pub runtime_digest: Sha256Digest,
    pub bridge_digest: Sha256Digest,
    pub trampoline_digest: Sha256Digest,
    /// sha256 of the unit main process's executable (the trampoline copy).
    pub launcher_exe_sha256: Option<Sha256Digest>,
    pub runtime: Option<ProcessFacts>,
    pub runtime_fd3: Fd3,
    pub bridge: Option<ProcessFacts>,
    pub initialize: InitializeFacts,
    pub exclusive: ExclusiveFacts,
    /// How the run ended (after the conversation): "exited 1", "killed by the harness", ...
    pub run_end: String,
}

/// The four facts of the record. `Ok` only when every fact was positively
/// observed (then all four exits are 0); any missing or false fact is `Err`
/// naming it, and no record is written.
pub(super) fn decide(o: &Observations) -> std::result::Result<(), &'static str> {
    let uid = [o.uid; 4];
    if o.socket.inode == 0 || o.socket.mount_id == 0 || o.runtime_digest == o.bridge_digest
        || !(duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX).contains(&o.uid) {
        return Err("the run's expected identities (socket, digests, uid) were not recorded before the launch");
    }
    // parent exec
    if o.launcher_exe_sha256 != Some(o.trampoline_digest) {
        return Err("parent exec: the unit's main process is not the harness's trampoline copy");
    }
    let runtime = o.runtime.as_ref().ok_or("parent exec: no single process with the runtime digest below the trampoline")?;
    if runtime.exe_sha256 != o.runtime_digest || runtime.uids != uid || !runtime.cgroup_member || runtime.depth == 0
        || !runtime.alive_through_observation {
        return Err("parent exec: the runtime process is not the signed runtime, the employee uid, in the run cgroup and pinned");
    }
    if o.runtime_fd3 == Fd3::Other { return Err("parent exec: runtime descriptor 3 is neither absent nor the MCP socket"); }
    let bridge = o.bridge.as_ref().ok_or("MCP child exec: no single process with the bridge digest below the runtime")?;
    if runtime.socket_fds.is_empty() && bridge.socket_fds.is_empty() {
        return Err("parent exec: the runtime's process tree does not hold the MCP socket");
    }
    // MCP child exec
    if bridge.exe_sha256 != o.bridge_digest || bridge.uids != uid || !bridge.cgroup_member || bridge.depth == 0
        || !bridge.alive_through_observation || bridge.socket_fds.is_empty() {
        return Err("MCP child exec: the bridge is not the signed bridge, a descendant of the runtime in the run cgroup holding the socket");
    }
    // initialize
    let i = &o.initialize;
    if i.initialize_requests != 1 || !i.observed_before_reply || !i.initialized_after_reply || !i.tools_list_answered
        || i.refused_frames != 0 {
        return Err("initialize: not exactly one initialize, observed before its reply, then notifications/initialized and tools/list");
    }
    // exclusive config
    let e = &o.exclusive;
    if !e.projection_bytes_exact || !e.projection_is_api_worker_config || e.projection_mount_id == 0 {
        return Err("exclusive config: the mounted projection is not the byte-exact api worker configuration");
    }
    if e.decoys_planted != 2 || e.decoy_markers_present != 0 || e.bridge_processes_seen != 1 || i.initialize_requests != 1 {
        return Err("exclusive config: a decoy was not planted, a decoy marker exists, or not exactly one bridge process / initialize");
    }
    Ok(())
}

// ── evidence ────────────────────────────────────────────────────────────

/// Canonical JSON: object keys sorted (byte order), no whitespace, serde_json
/// scalars. Independent of serde_json's map feature.
pub(super) fn canonical(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}
fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 { out.push(b','); }
                out.extend(serde_json::to_vec(key).unwrap_or_default());
                out.push(b':');
                if let Some(item) = map.get(key) { write_canonical(item, out); }
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 { out.push(b','); }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        scalar => out.extend(serde_json::to_vec(scalar).unwrap_or_default()),
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub(super) fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(char::from(BASE64[(word >> 18) as usize & 63]));
        out.push(char::from(BASE64[(word >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 { char::from(BASE64[(word >> 6) as usize & 63]) } else { '=' });
        out.push(if chunk.len() > 2 { char::from(BASE64[word as usize & 63]) } else { '=' });
    }
    out
}
/// Strict inverse of [`base64`] (padding required, no other characters).
pub(super) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 { return None; }
    let value = |byte: u8| BASE64.iter().position(|c| *c == byte).map(|p| p as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let pad = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if pad > 2 || (pad > 0 && !last) { return None; }
        let mut word = 0u32;
        for (position, byte) in chunk.iter().enumerate() {
            let digit = if position >= 4 - pad { 0 } else { value(*byte)? };
            word = (word << 6) | digit;
        }
        out.push((word >> 16) as u8);
        if pad < 2 { out.push((word >> 8) as u8); }
        if pad < 1 { out.push(word as u8); }
    }
    if base64(&out) != text { return None; }
    Some(out)
}

fn sha(bytes: &[u8]) -> Sha256Digest { Sha256Digest(Sha256::digest(bytes).into()) }

/// `transcript.jsonl`: one canonical line per frame, in order. Only the MCP
/// conversation crosses this socket (no provider key: the key is in the
/// employee home, read by the worker, and never written to descriptor 3).
pub(super) fn transcript_jsonl(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        out.extend(canonical(&json!({"seq": index as u64 + 1, "direction": frame.direction.name(),
            "sha256": hex(&sha(&frame.payload).0), "length": frame.payload.len() as u64,
            "payload_base64": base64(&frame.payload)})));
        out.push(b'\n');
    }
    out
}

fn process_json(process: &Option<ProcessFacts>) -> Value {
    match process {
        None => Value::Null,
        Some(p) => json!({"pid": p.pid, "depth": p.depth, "uids": p.uids.to_vec(), "exe_sha256": hex(&p.exe_sha256.0),
            "cgroup_member": p.cgroup_member, "socket_fds": p.socket_fds, "alive_through_observation": p.alive_through_observation}),
    }
}

/// The bindings of one acceptance run (all digests lowercase hex in the manifest).
pub(super) struct EvidenceSummary<'a> {
    pub arch: Arch,
    pub harness_sha256: Sha256Digest,
    pub trampoline_sha256: Sha256Digest,
    pub runtime_sha256: Sha256Digest,
    pub bridge_sha256: Sha256Digest,
    pub projection_sha256: Sha256Digest,
    pub launch_policy_sha256: Sha256Digest,
    pub transcript_sha256: Sha256Digest,
    pub observations: &'a Observations,
    pub kernel: &'a str,
    pub finished_unix: u64,
}

/// `acceptance-manifest.json` (canonical JSON). Its SHA-256 is the record's
/// `transcript_digest`, so the signed record binds the transcript digest and
/// the harness, trampoline, architecture and format without a new field.
pub(super) fn evidence_manifest(s: &EvidenceSummary<'_>) -> Vec<u8> {
    let o = s.observations;
    let i = &o.initialize;
    let e = &o.exclusive;
    canonical(&json!({
        "domain": EVIDENCE_DOMAIN, "format": EVIDENCE_FORMAT, "arch": arch_name(s.arch),
        "harness_sha256": hex(&s.harness_sha256.0), "trampoline_sha256": hex(&s.trampoline_sha256.0),
        "runtime_sha256": hex(&s.runtime_sha256.0), "bridge_sha256": hex(&s.bridge_sha256.0),
        "projection_sha256": hex(&s.projection_sha256.0), "launch_policy_sha256": hex(&s.launch_policy_sha256.0),
        "transcript_sha256": hex(&s.transcript_sha256.0),
        "observations": {
            "parent_exec": {"uid": o.uid, "socket": {"inode": o.socket.inode, "mount_id": o.socket.mount_id},
                "launcher_exe_sha256": o.launcher_exe_sha256.map(|d| hex(&d.0)), "runtime": process_json(&o.runtime),
                "fd3": o.runtime_fd3.name()},
            "mcp_child_exec": {"bridge": process_json(&o.bridge)},
            "initialize": {"initialize_requests": i.initialize_requests, "observed_before_reply": i.observed_before_reply,
                "initialized_after_reply": i.initialized_after_reply, "tools_list_answered": i.tools_list_answered,
                "refused_frames": i.refused_frames, "frames": i.frames},
            "exclusive_config": {"projection_bytes_exact": e.projection_bytes_exact,
                "projection_is_api_worker_config": e.projection_is_api_worker_config,
                "projection_mount_id": e.projection_mount_id, "decoys_planted": e.decoys_planted,
                "decoy_markers_present": e.decoy_markers_present, "bridge_processes_seen": e.bridge_processes_seen,
                "initialize_requests": i.initialize_requests},
            "run_end": o.run_end,
        },
        "kernel": s.kernel, "finished_unix": s.finished_unix,
    }))
}

/// The record of a run whose facts [`decide`] accepted: every exit 0, fd 3,
/// `transcript_digest` = SHA-256 of the manifest bytes.
pub(super) fn acceptance_record(prepared: &Prepared, manifest: &[u8]) -> McpAcceptance {
    let record = prepared.record();
    McpAcceptance { runtime: record.runtime.clone(), version: record.pinned_version.as_str().into(),
        runtime_digest: prepared.runtime_digest, bridge_digest: prepared.bridge_digest,
        projection_digest: prepared.projection.digest(), launch_policy_digest: prepared.launch_policy_digest,
        inherited_fd: duduclaw_mcp_boundary::config::MCP_FD, initialize_exit: 0, parent_exec_exit: 0,
        mcp_child_exec_exit: 0, exclusive_config_exit: 0, transcript_digest: sha(manifest) }
}

// ── the output source ───────────────────────────────────────────────────

/// Relative syscall-list paths of a `PerArch` object made absolute against `base`.
fn absolutize(value: &mut Value, base: &Path) -> Checked<()> {
    let Some(object) = value.as_object_mut() else { return Ok(()) };
    for item in object.values_mut() {
        if let Some(text) = item.as_str() {
            if !Path::new(text).is_absolute() {
                let joined = base.join(text);
                *item = Value::String(joined.to_str().ok_or("accept: a syscall-list path is not UTF-8")?.to_owned());
            }
        }
    }
    Ok(())
}

/// The input source with the labelled runtime's `mcp_acceptance` filled in
/// (same schema; everything else unchanged except that, when `--out-source`
/// is in another directory than `--source`, relative syscall-list paths are
/// made absolute against the source's directory so `build --source <out>`
/// reads the same files).
pub(super) fn accepted_source(raw: &[u8], source: &Path, out: &Path, label: &str, acceptance: &McpAcceptance) -> Checked<Vec<u8>> {
    let mut document: Value = serde_json::from_slice(raw).map_err(|e| format!("accept: policy source: {e}"))?;
    let source_dir = fs::canonicalize(parent_directory(source)).map_err(|e| format!("accept: --source directory: {e}"))?;
    let out_dir = fs::canonicalize(parent_directory(out)).map_err(|e| format!("accept: --out-source directory: {e}"))?;
    let rewrite = source_dir != out_dir;
    if rewrite {
        if let Some(forwarder) = document.get_mut("forwarder_seccomp") { absolutize(forwarder, &source_dir)?; }
    }
    let record = serde_json::to_value(acceptance).map_err(|e| format!("accept: record: {e}"))?;
    let runtimes = document.get_mut("runtimes").and_then(Value::as_array_mut).ok_or("accept: policy source has no runtimes")?;
    let mut matched = 0;
    for runtime in runtimes.iter_mut() {
        let object = runtime.as_object_mut().ok_or("accept: a runtime entry is not an object")?;
        if rewrite {
            if let Some(seccomp) = object.get_mut("native").and_then(|native| native.get_mut("seccomp")) {
                absolutize(seccomp, &source_dir)?;
            }
        }
        if object.get("label").and_then(Value::as_str) == Some(label) {
            if object.get("mcp_acceptance").is_some_and(|value| !value.is_null()) { return Err(refusal::ALREADY_ACCEPTED.into()); }
            object.insert("mcp_acceptance".into(), record.clone());
            matched += 1;
        }
    }
    if matched != 1 { return Err(refusal::LABEL_MISSING.into()); }
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|e| format!("accept: output source: {e}"))?;
    bytes.push(b'\n');
    serde_json::from_slice::<PolicySource>(&bytes).map_err(|e| format!("accept: the output source is not a policy source: {e}"))?;
    Ok(bytes)
}

/// The policy generated from the output source equals the one generated
/// before, except that runtime `index` carries `acceptance`.
pub(super) fn only_record_added(before: &[u8], after: &[u8], index: usize, acceptance: &McpAcceptance) -> Checked<()> {
    let mut expected: Value = serde_json::from_slice(before).map_err(|e| format!("accept: policy: {e}"))?;
    let slot = expected.get_mut("runtimes").and_then(Value::as_array_mut).and_then(|runtimes| runtimes.get_mut(index))
        .and_then(Value::as_object_mut).ok_or("accept: policy runtime missing")?;
    slot.insert("mcp_acceptance".into(), serde_json::to_value(acceptance).map_err(|e| format!("accept: record: {e}"))?);
    let actual: Value = serde_json::from_slice(after).map_err(|e| format!("accept: policy: {e}"))?;
    if expected != actual {
        return Err("accept: the output source changes more than the labelled runtime's mcp_acceptance".into());
    }
    Ok(())
}

/// A new file (never over an existing one), fsynced; removed again on failure.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Checked<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)
        .map_err(|e| format!("accept: {}: {e}", path.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)))
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(format!("accept: {}: {error}", path.display()));
    }
    Ok(())
}

/// `--evidence-dir` (created 0700 when missing, still empty otherwise), root
/// owned, holding `transcript.jsonl` and `acceptance-manifest.json` (0600).
fn write_evidence(directory: &Path, transcript: &[u8], manifest: &[u8]) -> Checked<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound =>
            fs::DirBuilder::new().mode(0o700).create(directory).map_err(|e| format!("accept: --evidence-dir: {e}"))?,
        Ok(metadata) if metadata.is_dir() && fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none()) => {}
        _ => return Err(refusal::EVIDENCE_DIR.into()),
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|e| format!("accept: --evidence-dir: {e}"))?;
    std::os::unix::fs::chown(directory, Some(0), Some(0)).map_err(|e| format!("accept: --evidence-dir: {e}"))?;
    write_new(&directory.join(TRANSCRIPT_FILE), transcript, 0o600)?;
    write_new(&directory.join(MANIFEST_FILE), manifest, 0o600)?;
    File::open(directory).and_then(|file| file.sync_all()).map_err(|e| format!("accept: --evidence-dir: {e}"))
}

// ── the command ─────────────────────────────────────────────────────────

/// `accept` (without the program name and command word).
pub(super) fn command(args: &[String], facts: &HostFacts) -> std::result::Result<String, (i32, String)> {
    let request = parse_request(args).map_err(|message| (2, format!("{message}\n{USAGE}")))?;
    host_gate(facts, &request).map_err(|message| (1, message.to_owned()))?;
    let raw = read_limited(&request.source, MAX_SOURCE_BYTES, "policy source").map_err(|e| (1, format!("accept: refused: {e}")))?;
    let source: PolicySource = serde_json::from_slice(&raw).map_err(|e| (1, format!("accept: refused: policy source: {e}")))?;
    label_gate(&source, &request.label).map_err(|message| (1, message.to_owned()))?;
    drop(source);
    let prepared = prepare(&request, &raw).map_err(|message| (1, message))?;
    let outcome = harness::run(&prepared, &request.trampoline).map_err(|message| (1, message))?;
    finish(&request, &raw, &prepared, &outcome).map_err(|message| (1, message))
}

/// Decide, encode the evidence, build and check the record and the output
/// source, and only then write the evidence and the output source.
fn finish(request: &Request, raw: &[u8], prepared: &Prepared, outcome: &harness::Outcome) -> Checked<String> {
    decide(&outcome.observations).map_err(|reason| format!("accept: no record written: {reason}"))?;
    let transcript = transcript_jsonl(&outcome.frames);
    let finished_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let manifest = evidence_manifest(&EvidenceSummary { arch: prepared.arch, harness_sha256: harness::self_digest()?,
        trampoline_sha256: outcome.trampoline_sha256, runtime_sha256: prepared.runtime_digest,
        bridge_sha256: prepared.bridge_digest, projection_sha256: prepared.projection.digest(),
        launch_policy_sha256: prepared.launch_policy_digest, transcript_sha256: sha(&transcript),
        observations: &outcome.observations, kernel: &outcome.kernel, finished_unix });
    let acceptance = acceptance_record(prepared, &manifest);
    // The same validation the policy check uses, then check_policy itself on
    // a policy generated from the output source.
    acceptance.validate(&prepared.projection).map_err(|_| "accept: no record written: McpAcceptance::validate refused it".to_owned())?;
    acceptance.validate_launch(prepared.record(), prepared.native())
        .map_err(|_| "accept: no record written: McpAcceptance::validate_launch refused it".to_owned())?;
    let out = accepted_source(raw, &request.source, &request.out_source, &request.label, &acceptance)?;
    let (generated, _) = build_policy_bytes(&out, &request.out_source, &request.artifacts, request.arch, None)
        .map_err(|e| format!("accept: no record written: the output source fails the production checks: {e}"))?;
    only_record_added(&prepared.generated, &generated.bytes, prepared.index, &acceptance)?;
    write_evidence(&request.evidence, &transcript, &manifest)?;
    write_new(&request.out_source, &out, 0o644)?;
    Ok(format!("accepted runtime {label}: runtime {runtime}… bridge {bridge}… projection {projection}… launch policy {launch}…\n\
        record transcript_digest {digest}… = sha256({MANIFEST_FILE}); evidence in {evidence}\nwrote {out}\n",
        label = prepared.label, runtime = short(&prepared.runtime_digest), bridge = short(&prepared.bridge_digest),
        projection = short(&acceptance.projection_digest), launch = short(&prepared.launch_policy_digest),
        digest = short(&acceptance.transcript_digest), evidence = request.evidence.display(), out = request.out_source.display()))
}

// ── verify --evidence-dir ───────────────────────────────────────────────

fn hex_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> { value.get(key).and_then(Value::as_str) }

/// One record against its evidence: the manifest bytes hash to the record's
/// `transcript_digest` and are canonical, name this format, architecture and
/// the record's digests, the runtime and bridge digests are the release
/// manifest's artifacts, and the transcript file is the one the manifest names
/// (each line canonical, in sequence, its payload matching its digest/length).
pub(super) fn check_evidence(manifest_bytes: &[u8], transcript: &[u8], acceptance: &McpAcceptance, record: &RuntimeRecord,
    release: &ReleaseManifest, arch: Arch) -> std::result::Result<(), &'static str> {
    if sha(manifest_bytes) != acceptance.transcript_digest { return Err("acceptance-manifest.json is not the record's transcript_digest"); }
    let value: Value = serde_json::from_slice(manifest_bytes).map_err(|_| "acceptance-manifest.json is not JSON")?;
    if canonical(&value) != manifest_bytes { return Err("acceptance-manifest.json is not canonical JSON"); }
    if value.get("domain").and_then(Value::as_str) != Some(EVIDENCE_DOMAIN) || value.get("format").and_then(Value::as_u64) != Some(EVIDENCE_FORMAT)
        || value.get("arch").and_then(Value::as_str) != Some(arch_name(arch)) {
        return Err("acceptance-manifest.json has another domain, format or architecture");
    }
    let ExecutableRecord::Elf { entry, digest } = &record.executable else { return Err("the accepted runtime is not an ELF entry") };
    let runtime_artifact = release.artifacts.iter().find(|a| artifact_name(&a.install_target).is_ok_and(|name| name == entry.as_str()))
        .ok_or("the release manifest has no artifact for the accepted runtime")?;
    let bridge_artifact = release.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge)
        .ok_or("the release manifest has no duduclaw-mcp-bridge")?;
    let pairs = [("runtime_sha256", acceptance.runtime_digest), ("bridge_sha256", acceptance.bridge_digest),
        ("projection_sha256", acceptance.projection_digest), ("launch_policy_sha256", acceptance.launch_policy_digest)];
    for (key, expected) in pairs {
        if hex_field(&value, key) != Some(hex(&expected.0).as_str()) { return Err("acceptance-manifest.json digests differ from the record"); }
    }
    if acceptance.runtime_digest != *digest || runtime_artifact.sha256 != *digest || bridge_artifact.sha256 != acceptance.bridge_digest {
        return Err("the record's runtime/bridge digests differ from the artifacts");
    }
    if hex_field(&value, "transcript_sha256") != Some(hex(&sha(transcript).0).as_str()) {
        return Err("transcript.jsonl is not the transcript the manifest names");
    }
    let mut lines = 0u64;
    for line in transcript.split(|byte| *byte == b'\n') {
        if line.is_empty() { continue; }
        lines += 1;
        let entry: Value = serde_json::from_slice(line).map_err(|_| "transcript.jsonl has a line that is not JSON")?;
        if canonical(&entry) != line || entry.get("seq").and_then(Value::as_u64) != Some(lines)
            || !matches!(entry.get("direction").and_then(Value::as_str), Some("runtime_to_harness" | "harness_to_runtime")) {
            return Err("transcript.jsonl lines are not canonical, in sequence and directed");
        }
        let payload = entry.get("payload_base64").and_then(Value::as_str).and_then(base64_decode)
            .ok_or("transcript.jsonl has a payload that is not base64")?;
        if entry.get("length").and_then(Value::as_u64) != Some(payload.len() as u64)
            || hex_field(&entry, "sha256") != Some(hex(&sha(&payload).0).as_str()) {
            return Err("transcript.jsonl has a frame whose digest or length is wrong");
        }
    }
    if !transcript.is_empty() && !transcript.ends_with(b"\n") { return Err("transcript.jsonl is truncated"); }
    if lines == 0 || lines > MAX_FRAMES as u64 { return Err("transcript.jsonl has no frames or more than 64"); }
    Ok(())
}

/// `verify --evidence-dir <dir>`: every runtime with an acceptance record must
/// have an `acceptance-manifest.json` (in `<dir>` or one of its immediate
/// subdirectories, e.g. `<dir>/<label>/`) whose SHA-256 is the record's
/// `transcript_digest`, checked by [`check_evidence`] with the
/// `transcript.jsonl` beside it.
pub(super) fn verify_evidence(policy: &SignedPolicy, release: &ReleaseManifest, arch: Arch, directory: &Path)
    -> std::result::Result<String, String> {
    let refuse = |what: &str| format!("refused: --evidence-dir {}: {what}", directory.display());
    if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir()) { return Err(refuse("not a directory")); }
    let mut candidates = vec![directory.to_path_buf()];
    for entry in fs::read_dir(directory).map_err(|e| refuse(e.to_string().as_str()))? {
        let entry = entry.map_err(|e| refuse(e.to_string().as_str()))?;
        if candidates.len() > 256 { return Err(refuse("more than 256 entries")); }
        if fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()) { candidates.push(entry.path()); }
    }
    let mut by_digest: BTreeMap<[u8; 32], PathBuf> = BTreeMap::new();
    for candidate in candidates {
        let path = candidate.join(MANIFEST_FILE);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(refuse("an acceptance-manifest.json is not a regular file")),
        }
        let bytes = read_limited(&path, MAX_EVIDENCE_FILE_BYTES, "acceptance-manifest.json").map_err(|e| refuse(e.as_str()))?;
        if by_digest.insert(sha(&bytes).0, candidate).is_some() { return Err(refuse("two identical acceptance manifests")); }
    }
    let mut checked = 0;
    for record in &policy.runtimes {
        let Some(acceptance) = &record.mcp_acceptance else { continue };
        let found = by_digest.get(&acceptance.transcript_digest.0)
            .ok_or_else(|| refuse("no acceptance-manifest.json whose sha256 is a record's transcript_digest"))?;
        let manifest = read_limited(&found.join(MANIFEST_FILE), MAX_EVIDENCE_FILE_BYTES, "acceptance-manifest.json").map_err(|e| refuse(e.as_str()))?;
        let transcript = read_limited(&found.join(TRANSCRIPT_FILE), MAX_EVIDENCE_FILE_BYTES, "transcript.jsonl").map_err(|e| refuse(e.as_str()))?;
        check_evidence(&manifest, &transcript, acceptance, record, release, arch).map_err(refuse)?;
        checked += 1;
    }
    Ok(format!("evidence: {checked} acceptance record(s) backed by harness evidence in {}\n", directory.display()))
}

#[cfg(test)]
#[path = "installed_policy_tool_accept_tests.rs"]
mod tests;
