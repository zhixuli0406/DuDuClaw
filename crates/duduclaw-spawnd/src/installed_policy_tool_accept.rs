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
//! the classification of the run cgroup, the decision, the tear-down
//! selection, the evidence encoding and checks, the output source); the root
//! launch is [`harness`].
//!
//! Security review round 1 (R8r-4): the trampoline is bound to the shipped
//! one (`--trampoline-sha256`, checked again by `verify --manifest
//! --evidence-dir` against the manifest's `InstallTarget::Trampoline`, F1);
//! every observation is complete or there is no record ([`classify`],
//! [`Census`], [`incomplete`], F2); the tear-down signals only members of the
//! run cgroup ([`teardown_selection`], F3); `verify --evidence-dir` re-parses,
//! replays and re-decides the evidence ([`check_evidence`], F4); the harness
//! directory is registered for removal right after its mkdir
//! ([`make_registered_dir`], F5).
use super::*;
use serde::{Deserialize, Serialize};
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
    pub const TRAMPOLINE_DIGEST: &str = "accept: refused: the SHA-256 of --trampoline is not --trampoline-sha256 \
        (the duduclaw-run the release manifest will name)";
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
    /// F1: the SHA-256 of the `duduclaw-run` the release manifest will name
    /// (`InstallTarget::Trampoline`), computed from the same archive snapshot
    /// the trampoline file was staged from. `verify --manifest --evidence-dir`
    /// proves the equality with the signed manifest afterwards.
    pub trampoline_sha256: Sha256Digest,
    pub evidence: PathBuf,
    pub out_source: PathBuf,
}

/// 64 lowercase hex characters.
pub(super) fn parse_digest_hex(text: &str) -> Option<Sha256Digest> {
    if text.len() != 64 || !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) { return None; }
    let mut out = [0u8; 32];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(Sha256Digest(out))
}

fn parse_request(args: &[String]) -> Checked<Request> {
    let values = options(args, &["--source", "--artifacts-dir", "--arch", "--label", "--trampoline", "--trampoline-sha256",
        "--evidence-dir", "--out-source"])?;
    let get = |key: &str| values.get(key).cloned().ok_or_else(|| format!("accept needs {key}"));
    let trampoline_sha256 = parse_digest_hex(&get("--trampoline-sha256")?)
        .ok_or_else(|| "--trampoline-sha256 must be 64 lowercase hex characters".to_owned())?;
    let request = Request { source: get("--source")?.into(), artifacts: get("--artifacts-dir")?.into(),
        arch: parse_arch(&get("--arch")?)?, label: get("--label")?, trampoline: get("--trampoline")?.into(),
        trampoline_sha256, evidence: get("--evidence-dir")?.into(), out_source: get("--out-source")?.into() };
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
    // F1: hashing is not a side effect; the root copy is checked again.
    match file_digest(&request.trampoline) {
        Ok((digest, _)) if digest == request.trampoline_sha256 => Ok(()),
        Ok(_) => Err(refusal::TRAMPOLINE_DIGEST),
        Err(_) => Err(refusal::TRAMPOLINE),
    }
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
    fn from_name(name: &str) -> Option<Self> {
        match name { "expected_socket" => Some(Self::ExpectedSocket), "absent" => Some(Self::Absent), "other" => Some(Self::Other),
            _ => None }
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
    /// F2: why the observation is not complete ([`incomplete`]); `Some` means
    /// no record. Never written to the evidence (a record has none).
    pub incomplete: Option<&'static str>,
}

// ── the run cgroup: complete enumeration and classification (F2) ────────
//
// Which processes legitimately live in the run unit's cgroup (duduclaw-run
// `runner.rs`; the harness refuses runtimes with a helper launcher, so the HX
// launcher is never forked), each bound by its `/proc/<pid>/exe` SHA-256:
//
// | process | parent | exe digest bound to |
// |---|---|---|
// | supervisor = the unit's `MainPID` | PID 1 (systemd) | the trampoline (`--trampoline-sha256` = manifest `Trampoline`) |
// | init (PID 1 of the run's PID namespace; a clone, never exec'd) | supervisor | the trampoline |
// | forwarder (only `network: egress`) | init | `native.forwarder.executable_digest` = manifest `EgressForwarder` |
// | runtime (the api worker) | init | the record's runtime digest = manifest `RuntimeEntry` |
// | bridge (`/runtime/duduclaw-mcp-bridge`) | runtime (init once the runtime is gone) | the record's bridge digest = manifest `McpBridge` |
//
// plus the transient pre-exec windows of the same processes: a fork of init
// still shows the trampoline digest before it executes the forwarder or the
// runtime, a fork of the runtime still shows the runtime digest before it
// executes the bridge. A process keeps its `(pid, start)` across exec, so the
// census allows exactly the transitions trampoline → forwarder, trampoline →
// runtime and runtime → bridge. Threads are not cgroup.procs entries. Any
// other digest, parent or transition is `unexpected_process`: refused by
// default. Nothing else is placed in the unit by systemd (`Type=exec`, one
// main process) and nothing inside the run reaches cgroupfs.

/// Fixed reasons an observation is incomplete; any of them means no record.
pub(super) mod incomplete {
    pub const CGROUP_UNREADABLE: &str = "cgroup_unreadable";
    pub const CGROUP_UNSTABLE: &str = "cgroup_unstable";
    pub const CGROUP_TREE_CAP: &str = "cgroup_tree_cap";
    pub const NODE_CAP: &str = "tree_node_cap";
    pub const DEPTH_CAP: &str = "tree_depth_cap";
    pub const CENSUS_CAP: &str = "census_cap";
    pub const UNREADABLE_PROCESS: &str = "unreadable_process";
    pub const HASH_ERROR: &str = "hash_error";
    pub const UNCONFIRMED_EXIT: &str = "unconfirmed_exit";
    pub const UNEXPECTED_PROCESS: &str = "unexpected_process";
    pub const LAUNCHER_MISSING: &str = "launcher_missing";
    pub const PROCESS_FACTS: &str = "process_facts_unreadable";
}

/// The fixed refusal of [`decide`] for an incomplete observation.
pub(super) fn incomplete_message(reason: &str) -> &'static str {
    use incomplete::*;
    match reason {
        CGROUP_UNREADABLE => "incomplete observation (cgroup_unreadable): the run cgroup could not be enumerated",
        CGROUP_UNSTABLE => "incomplete observation (cgroup_unstable): no two consecutive enumerations of the run cgroup agreed",
        CGROUP_TREE_CAP => "incomplete observation (cgroup_tree_cap): the run cgroup has more sub-cgroups than the harness walks",
        NODE_CAP => "incomplete observation (tree_node_cap): the run cgroup listed more processes than its task limit",
        DEPTH_CAP => "incomplete observation (tree_depth_cap): a process chain is deeper than the run's task limit",
        CENSUS_CAP => "incomplete observation (census_cap): more distinct processes than the census holds",
        UNREADABLE_PROCESS => "incomplete observation (unreadable_process): a live process in the run cgroup could not be read",
        HASH_ERROR => "incomplete observation (hash_error): the executable of a process in the run cgroup could not be hashed",
        UNCONFIRMED_EXIT => "incomplete observation (unconfirmed_exit): a process vanished without its pidfd confirming the exit",
        UNEXPECTED_PROCESS => "incomplete observation (unexpected_process): a process in the run cgroup is not the trampoline, \
            the forwarder, the runtime or the bridge in its documented place",
        LAUNCHER_MISSING => "incomplete observation (launcher_missing): the unit's main process was not in the run cgroup \
            before the conversation completed",
        PROCESS_FACTS => "incomplete observation (process_facts_unreadable): the runtime's or the bridge's facts could not be read",
        _ => "incomplete observation (unknown reason)",
    }
}

/// One cgroup member as one inspection saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MemberRead {
    /// Pinned by a pidfd between two reads of the same start time; `exe` is
    /// the SHA-256 of its executable at that time.
    Live { start: u64, ppid: u32, exe: Sha256Digest },
    /// Its `/proc` entry was gone; `confirmed`: the pidfd reported the exit
    /// (or `pidfd_open` reported ESRCH).
    Exited { confirmed: bool },
    /// Alive, but its stat or executable could not be read.
    Unreadable,
    /// Its executable could not be hashed.
    HashError,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Member { pub pid: u32, pub read: MemberRead }
/// One enumeration of the whole unit cgroup (its `cgroup.procs` and those of
/// every sub-cgroup, re-read until two consecutive reads around the
/// inspection agree), or why there is none.
#[derive(Clone, Debug, Default)]
pub(super) struct CgroupSnapshot { pub members: Vec<Member>, pub incomplete: Option<&'static str> }

/// Who the run's processes may be, by digest (table above).
#[derive(Clone, Copy, Debug)]
pub(super) struct Identities {
    /// `(pid, start)` of the unit's main process, pinned at start.
    pub launcher: (u32, u64),
    pub trampoline: Sha256Digest,
    pub runtime: Sha256Digest,
    pub bridge: Sha256Digest,
    pub forwarder: Option<Sha256Digest>,
}

/// The distinct processes the census keeps. A legitimate run has at most five
/// (supervisor, init, forwarder, runtime, bridge), so this is never reached
/// by a legitimate run.
pub(super) const CENSUS_MAX: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WalkLimits { pub max_nodes: usize, pub max_depth: u32, pub max_census: usize }
impl WalkLimits {
    /// The run unit's `TasksMax` is the record's `resources.tasks`
    /// (`UnitPolicy::new`): the pids controller never lets the unit hold more
    /// processes, so neither the member count nor a parent chain (at most the
    /// member count) can legitimately exceed it. The walk is never truncated:
    /// reaching a limit is an incomplete observation.
    pub(super) fn for_tasks(tasks: u32) -> Self {
        Self { max_nodes: tasks as usize, max_depth: tasks, max_census: CENSUS_MAX }
    }
}

/// A live member with its depth below the unit's main process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LiveMember { pub pid: u32, pub start: u64, pub ppid: u32, pub exe: Sha256Digest, pub depth: u32 }
#[derive(Clone, Debug, Default)]
pub(super) struct Classified {
    pub live: Vec<LiveMember>,
    /// `(pid, depth below the main process)` of the only runtime-digest member.
    pub runtime: Option<(u32, u32)>,
    /// `(pid, distance below the runtime)` of the only bridge-digest member.
    pub bridge: Option<(u32, u32)>,
}

/// Classify one complete snapshot: every member must be live and in its
/// documented place (table above), else the fixed reason.
pub(super) fn classify(snapshot: &CgroupSnapshot, ids: &Identities, limits: &WalkLimits)
    -> std::result::Result<Classified, &'static str> {
    use incomplete::*;
    if let Some(reason) = snapshot.incomplete { return Err(reason); }
    if snapshot.members.len() > limits.max_nodes { return Err(NODE_CAP); }
    let mut live: BTreeMap<u32, (u64, u32, Sha256Digest)> = BTreeMap::new();
    for member in &snapshot.members {
        match &member.read {
            MemberRead::Live { start, ppid, exe } => {
                if live.insert(member.pid, (*start, *ppid, *exe)).is_some() { return Err(CGROUP_UNSTABLE); }
            }
            MemberRead::Exited { confirmed: true } => {}
            MemberRead::Exited { confirmed: false } => return Err(UNCONFIRMED_EXIT),
            MemberRead::Unreadable => return Err(UNREADABLE_PROCESS),
            MemberRead::HashError => return Err(HASH_ERROR),
        }
    }
    let (launcher_pid, launcher_start) = ids.launcher;
    match live.get(&launcher_pid) {
        Some((start, _, _)) if *start == launcher_start => {}
        Some(_) => return Err(UNEXPECTED_PROCESS),
        None => return Err(LAUNCHER_MISSING),
    }
    // Every member descends from the main process inside the cgroup.
    let mut members = Vec::with_capacity(live.len());
    for (&pid, &(start, ppid, exe)) in &live {
        let mut depth = 0u32;
        let mut current = pid;
        while current != launcher_pid {
            let parent = live.get(&current).map(|(_, parent, _)| *parent).ok_or(UNEXPECTED_PROCESS)?;
            if !live.contains_key(&parent) { return Err(UNEXPECTED_PROCESS); }
            depth += 1;
            if depth > limits.max_depth { return Err(DEPTH_CAP); }
            if depth as usize > live.len() { return Err(UNEXPECTED_PROCESS); }
            current = parent;
        }
        members.push(LiveMember { pid, start, ppid, exe, depth });
    }
    let depth_of: BTreeMap<u32, u32> = members.iter().map(|member| (member.pid, member.depth)).collect();
    let digest_of = |pid: u32| live.get(&pid).map(|(_, _, exe)| *exe);
    let (mut spawned_runtimes, mut forwarders, mut inits, mut init_forks) = (0usize, 0usize, 0usize, 0usize);
    for member in &members {
        let parent = digest_of(member.ppid);
        let from_init = parent == Some(ids.trampoline) && depth_of.get(&member.ppid) == Some(&1);
        let placed = if member.pid == launcher_pid {
            member.exe == ids.trampoline
        } else if member.exe == ids.trampoline {
            // init (depth 1) and the pre-exec forks of init (depth 2: the
            // forwarder and the runtime before their exec).
            if member.depth == 1 { inits += 1; } else { init_forks += 1; }
            parent == Some(ids.trampoline) && member.depth <= 2
        } else if Some(member.exe) == ids.forwarder {
            forwarders += 1;
            from_init
        } else if member.exe == ids.runtime {
            if from_init { spawned_runtimes += 1; true } else { parent == Some(ids.runtime) }
        } else if member.exe == ids.bridge {
            parent == Some(ids.runtime) || from_init
        } else {
            false
        };
        if !placed { return Err(UNEXPECTED_PROCESS); }
    }
    if spawned_runtimes > 1 || forwarders > 1 || inits > 1 || init_forks > 2 { return Err(UNEXPECTED_PROCESS); }
    let runtimes: Vec<&LiveMember> = members.iter().filter(|member| member.exe == ids.runtime).collect();
    let bridges: Vec<&LiveMember> = members.iter().filter(|member| member.exe == ids.bridge).collect();
    let runtime = match runtimes.as_slice() { [only] => Some((only.pid, only.depth)), _ => None };
    let ancestor = |mut pid: u32, steps: u32| -> Option<u32> {
        for _ in 0..steps { pid = live.get(&pid).map(|(_, parent, _)| *parent)?; }
        Some(pid)
    };
    let bridge = match (runtime, bridges.as_slice()) {
        (Some((runtime_pid, runtime_depth)), [only]) if only.depth > runtime_depth
            && ancestor(only.pid, only.depth - runtime_depth) == Some(runtime_pid) => Some((only.pid, only.depth - runtime_depth)),
        _ => None,
    };
    Ok(Classified { live: members, runtime, bridge })
}

/// The digests one `(pid, start)` had, in order: constant, or one documented exec.
fn allowed_sequence(digests: &[Sha256Digest], ids: &Identities) -> bool {
    let known = |digest: &Sha256Digest| *digest == ids.trampoline || *digest == ids.runtime || *digest == ids.bridge
        || Some(*digest) == ids.forwarder;
    match digests {
        [one] => known(one),
        [first, second] => (*first == ids.trampoline && (*second == ids.runtime || Some(*second) == ids.forwarder))
            || (*first == ids.runtime && *second == ids.bridge),
        _ => false,
    }
}

/// Every process ever seen in the run cgroup by `(pid, start)` with the
/// digests it had, from complete snapshots taken on every turn of the
/// conversation loop; sticky `incomplete`.
#[derive(Default)]
pub(super) struct Census { pub seen: BTreeMap<(u32, u64), Vec<Sha256Digest>>, pub incomplete: Option<&'static str> }
impl Census {
    pub(super) fn fail(&mut self, reason: &'static str) {
        if self.incomplete.is_none() { self.incomplete = Some(reason); }
    }
    /// `launcher_exited`: the main process's pidfd reported its exit (read
    /// after the snapshot). The supervisor exits only after init, whose exit
    /// SIGKILLs everything in the run's PID namespace, so a later snapshot
    /// shows only processes being torn down and is not classified; before the
    /// conversation completed that is itself a refusal.
    pub(super) fn record(&mut self, snapshot: &CgroupSnapshot, ids: &Identities, limits: &WalkLimits, launcher_exited: bool,
        after_conversation: bool) {
        if self.incomplete.is_some() { return; }
        if launcher_exited {
            if !after_conversation { self.fail(incomplete::LAUNCHER_MISSING); }
            return;
        }
        let classified = match classify(snapshot, ids, limits) {
            Ok(classified) => classified,
            Err(reason) => { self.fail(reason); return; }
        };
        for member in &classified.live {
            let digests = self.seen.entry((member.pid, member.start)).or_default();
            if digests.last() != Some(&member.exe) { digests.push(member.exe); }
        }
        if self.seen.len() > limits.max_census { self.fail(incomplete::CENSUS_CAP); return; }
        if self.seen.values().any(|digests| !allowed_sequence(digests, ids)) { self.fail(incomplete::UNEXPECTED_PROCESS); }
    }
    /// The census's verdict: incomplete, or more than one runtime (a runtime-
    /// digest process that never became the bridge) or forwarder ever.
    pub(super) fn finish(&self, ids: &Identities) -> Option<&'static str> {
        if self.incomplete.is_some() { return self.incomplete; }
        let runtimes = self.seen.values().filter(|d| d.contains(&ids.runtime) && !d.contains(&ids.bridge)).count();
        let forwarders = self.seen.values().filter(|d| ids.forwarder.is_some_and(|f| d.contains(&f))).count();
        if runtimes > 1 || forwarders > 1 { return Some(incomplete::UNEXPECTED_PROCESS); }
        None
    }
    /// Distinct processes that ever had the bridge digest.
    pub(super) fn bridges(&self, ids: &Identities) -> u32 {
        self.seen.values().filter(|digests| digests.contains(&ids.bridge)).count() as u32
    }
}

// ── tear-down selection (F3) ────────────────────────────────────────────

/// A process with the temporary uid (any uid or gid field), as the
/// tear-down's scan saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UidProcess { pub pid: u32, pub start: u64, pub cgroup: Option<String> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TeardownSelection {
    /// Members of the run cgroups: signalled (pidfd, start time re-checked).
    pub kill: Vec<(u32, u64)>,
    /// Same uid OUTSIDE them: never signalled, reported `foreign_uid_process`.
    pub foreign: Vec<(u32, u64)>,
}
pub(super) const FOREIGN_UID_PROCESS: &str = "foreign_uid_process";
/// Only processes proven to belong to this run may be signalled.
pub(super) fn teardown_selection(processes: &[UidProcess], run_cgroups: &[&str]) -> TeardownSelection {
    let mut selection = TeardownSelection::default();
    for process in processes {
        let inside = process.pid > 1 && process.cgroup.as_deref().is_some_and(|actual|
            run_cgroups.iter().any(|allowed| crate::accounts::cgroup_member(actual, allowed)));
        if inside { selection.kill.push((process.pid, process.start)); } else { selection.foreign.push((process.pid, process.start)); }
    }
    selection
}
/// The run unit's cgroup and, when it is its parent, the harness's own leaf
/// employee slice.
pub(super) fn run_cgroups(unit_cgroup: &str, slice: &str) -> Vec<String> {
    let mut groups = vec![unit_cgroup.to_owned()];
    if let Some((parent, _)) = unit_cgroup.rsplit_once('/') {
        if !parent.is_empty() && parent.rsplit('/').next() == Some(slice) { groups.push(parent.to_owned()); }
    }
    groups
}

// ── harness directory (F5) ──────────────────────────────────────────────

/// Create `path`, hand it to `register` (the tear-down guard) right after the
/// mkdir succeeded, then `secure` it (chmod/chown): a failing `secure` leaves
/// a registered directory the guard removes.
pub(super) fn make_registered_dir(path: &Path, mode: u32, register: impl FnOnce(&Path),
    secure: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(mode).create(path)?;
    register(path);
    secure(path)
}

/// The four facts of the record. `Ok` only when every fact was positively
/// observed (then all four exits are 0); any missing or false fact is `Err`
/// naming it, and no record is written.
pub(super) fn decide(o: &Observations) -> std::result::Result<(), &'static str> {
    if let Some(reason) = o.incomplete { return Err(incomplete_message(reason)); }
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
        out.extend(canonical_of(&TranscriptLine { seq: index as u64 + 1, direction: frame.direction.name().into(),
            sha256: hex(&sha(&frame.payload).0), length: frame.payload.len() as u64, payload_base64: base64(&frame.payload) }));
        out.push(b'\n');
    }
    out
}

/// Canonical JSON of a typed value (its fields are plain JSON scalars,
/// arrays and objects, so the conversion cannot fail).
fn canonical_of<T: Serialize>(value: &T) -> Vec<u8> { canonical(&serde_json::to_value(value).unwrap_or(Value::Null)) }

/// One line of `transcript.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TranscriptLine { pub seq: u64, pub direction: String, pub sha256: String, pub length: u64, pub payload_base64: String }

/// `acceptance-manifest.json`, typed: the writer encodes exactly this and
/// `verify --evidence-dir` accepts exactly this (unknown fields refused,
/// canonical re-encoding byte-identical to the file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceDocument {
    pub domain: String,
    pub format: u64,
    pub arch: String,
    /// The policy tool that ran `accept` (`/proc/self/exe`). The tool is not
    /// shipped, so no manifest names it: the signing ceremony pins it out of
    /// band (`verify --harness-sha256`).
    pub harness_sha256: String,
    pub trampoline_sha256: String,
    pub runtime_sha256: String,
    pub bridge_sha256: String,
    pub projection_sha256: String,
    pub launch_policy_sha256: String,
    pub transcript_sha256: String,
    pub observations: ObservationsDocument,
    pub kernel: String,
    pub finished_unix: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservationsDocument {
    pub parent_exec: ParentExecDocument,
    pub mcp_child_exec: McpChildExecDocument,
    pub initialize: InitializeDocument,
    pub exclusive_config: ExclusiveDocument,
    pub run_end: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParentExecDocument {
    pub uid: u32,
    pub socket: SocketDocument,
    pub launcher_exe_sha256: Option<String>,
    pub runtime: Option<ProcessDocument>,
    pub fd3: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SocketDocument { pub inode: u64, pub mount_id: u64 }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessDocument {
    pub pid: u32,
    pub depth: u32,
    pub uids: [u32; 4],
    pub exe_sha256: String,
    pub cgroup_member: bool,
    pub socket_fds: Vec<i32>,
    pub alive_through_observation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpChildExecDocument { pub bridge: Option<ProcessDocument> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitializeDocument {
    pub initialize_requests: u32,
    pub observed_before_reply: bool,
    pub initialized_after_reply: bool,
    pub tools_list_answered: bool,
    pub refused_frames: u32,
    pub frames: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExclusiveDocument {
    pub projection_bytes_exact: bool,
    pub projection_is_api_worker_config: bool,
    pub projection_mount_id: u64,
    pub decoys_planted: u32,
    pub decoy_markers_present: u32,
    pub bridge_processes_seen: u32,
    pub initialize_requests: u32,
}

fn process_document(process: &Option<ProcessFacts>) -> Option<ProcessDocument> {
    process.as_ref().map(|p| ProcessDocument { pid: p.pid, depth: p.depth, uids: p.uids, exe_sha256: hex(&p.exe_sha256.0),
        cgroup_member: p.cgroup_member, socket_fds: p.socket_fds.clone(), alive_through_observation: p.alive_through_observation })
}
fn process_facts_of(document: &Option<ProcessDocument>) -> std::result::Result<Option<ProcessFacts>, &'static str> {
    document.as_ref().map(|p| -> std::result::Result<ProcessFacts, &'static str> {
        Ok(ProcessFacts { pid: p.pid, depth: p.depth, uids: p.uids,
            exe_sha256: parse_digest_hex(&p.exe_sha256).ok_or(evidence_refusal::DIGEST_SYNTAX)?, cgroup_member: p.cgroup_member,
            socket_fds: p.socket_fds.clone(), alive_through_observation: p.alive_through_observation })
    }).transpose()
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
pub(super) fn evidence_manifest(s: &EvidenceSummary<'_>) -> Vec<u8> { encode_evidence(&evidence_document(s)) }

/// The canonical bytes of an evidence document.
pub(super) fn encode_evidence(document: &EvidenceDocument) -> Vec<u8> { canonical_of(document) }

pub(super) fn evidence_document(s: &EvidenceSummary<'_>) -> EvidenceDocument {
    let o = s.observations;
    let i = &o.initialize;
    let e = &o.exclusive;
    EvidenceDocument {
        domain: EVIDENCE_DOMAIN.into(), format: EVIDENCE_FORMAT, arch: arch_name(s.arch).into(),
        harness_sha256: hex(&s.harness_sha256.0), trampoline_sha256: hex(&s.trampoline_sha256.0),
        runtime_sha256: hex(&s.runtime_sha256.0), bridge_sha256: hex(&s.bridge_sha256.0),
        projection_sha256: hex(&s.projection_sha256.0), launch_policy_sha256: hex(&s.launch_policy_sha256.0),
        transcript_sha256: hex(&s.transcript_sha256.0),
        observations: ObservationsDocument {
            parent_exec: ParentExecDocument { uid: o.uid, socket: SocketDocument { inode: o.socket.inode, mount_id: o.socket.mount_id },
                launcher_exe_sha256: o.launcher_exe_sha256.map(|d| hex(&d.0)), runtime: process_document(&o.runtime),
                fd3: o.runtime_fd3.name().into() },
            mcp_child_exec: McpChildExecDocument { bridge: process_document(&o.bridge) },
            initialize: InitializeDocument { initialize_requests: i.initialize_requests, observed_before_reply: i.observed_before_reply,
                initialized_after_reply: i.initialized_after_reply, tools_list_answered: i.tools_list_answered,
                refused_frames: i.refused_frames, frames: i.frames },
            exclusive_config: ExclusiveDocument { projection_bytes_exact: e.projection_bytes_exact,
                projection_is_api_worker_config: e.projection_is_api_worker_config, projection_mount_id: e.projection_mount_id,
                decoys_planted: e.decoys_planted, decoy_markers_present: e.decoy_markers_present,
                bridge_processes_seen: e.bridge_processes_seen, initialize_requests: i.initialize_requests },
            run_end: o.run_end.clone(),
        },
        kernel: s.kernel.into(), finished_unix: s.finished_unix,
    }
}

/// The observations an evidence document states, with the digests the
/// checks already bound (record = artifacts = release manifest).
fn observations_of(d: &EvidenceDocument, runtime: Sha256Digest, bridge: Sha256Digest, trampoline: Sha256Digest)
    -> std::result::Result<Observations, &'static str> {
    let p = &d.observations.parent_exec;
    let i = &d.observations.initialize;
    let e = &d.observations.exclusive_config;
    let launcher = p.launcher_exe_sha256.as_deref()
        .map(|text| parse_digest_hex(text).ok_or(evidence_refusal::DIGEST_SYNTAX)).transpose()?;
    Ok(Observations { uid: p.uid, socket: SocketId { inode: p.socket.inode, mount_id: p.socket.mount_id },
        runtime_digest: runtime, bridge_digest: bridge, trampoline_digest: trampoline, launcher_exe_sha256: launcher,
        runtime: process_facts_of(&p.runtime)?, runtime_fd3: Fd3::from_name(&p.fd3).ok_or(evidence_refusal::SCHEMA)?,
        bridge: process_facts_of(&d.observations.mcp_child_exec.bridge)?,
        initialize: InitializeFacts { initialize_requests: i.initialize_requests, observed_before_reply: i.observed_before_reply,
            initialized_after_reply: i.initialized_after_reply, tools_list_answered: i.tools_list_answered,
            refused_frames: i.refused_frames, frames: i.frames },
        exclusive: ExclusiveFacts { projection_bytes_exact: e.projection_bytes_exact,
            projection_is_api_worker_config: e.projection_is_api_worker_config, projection_mount_id: e.projection_mount_id,
            decoys_planted: e.decoys_planted, decoy_markers_present: e.decoy_markers_present,
            bridge_processes_seen: e.bridge_processes_seen },
        run_end: d.observations.run_end.clone(), incomplete: None })
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
    let outcome = harness::run(&prepared, &request.trampoline, request.trampoline_sha256).map_err(|message| (1, message))?;
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

/// `verify --evidence-dir` without `--manifest` (F1): the evidence's
/// trampoline can only be compared with a release manifest.
pub(super) const EVIDENCE_NEEDS_MANIFEST: &str = "refused: --evidence-dir requires --manifest (the release manifest whose \
    trampoline, runtime and bridge the evidence must name)";

/// Fixed refusals of [`check_evidence`].
pub(super) mod evidence_refusal {
    pub const NOT_RECORD: &str = "acceptance-manifest.json is not the record's transcript_digest";
    pub const SCHEMA: &str = "acceptance-manifest.json does not match the evidence schema (a missing or unknown field, or a wrong type)";
    pub const CANONICAL: &str = "acceptance-manifest.json is not the canonical encoding of the evidence schema";
    pub const DOMAIN: &str = "acceptance-manifest.json has another domain";
    pub const FORMAT: &str = "acceptance-manifest.json has another format than this tool writes";
    pub const ARCH: &str = "acceptance-manifest.json is for another architecture than the policy";
    pub const DIGEST_SYNTAX: &str = "acceptance-manifest.json has a digest that is not 64 lowercase hex characters";
    pub const HARNESS_PIN: &str = "acceptance-manifest.json harness_sha256 is not the pinned --harness-sha256";
    pub const RECORD_DIGESTS: &str = "acceptance-manifest.json digests differ from the record";
    pub const RECORD_EXITS: &str = "the record is not descriptor 3 with all four exits 0";
    pub const NOT_ELF: &str = "the accepted runtime is not an ELF entry";
    pub const ARTIFACTS: &str = "the record's runtime/bridge digests differ from the release manifest's artifacts";
    pub const NO_TRAMPOLINE: &str = "the release manifest has no single trampoline (duduclaw-run) artifact";
    pub const TRAMPOLINE: &str = "acceptance-manifest.json trampoline_sha256 is not the release manifest's trampoline";
    pub const TRANSCRIPT_FILE: &str = "transcript.jsonl is not the transcript the manifest names";
    pub const TRANSCRIPT_TRUNCATED: &str = "transcript.jsonl is empty or truncated";
    pub const TRANSCRIPT_LINES: &str = "transcript.jsonl lines are not canonical, in sequence and directed";
    pub const TRANSCRIPT_PAYLOAD: &str = "transcript.jsonl has a payload that is not base64";
    pub const TRANSCRIPT_FRAME: &str = "transcript.jsonl has a frame whose digest or length is wrong";
    pub const TRANSCRIPT_LIMIT: &str = "transcript.jsonl exceeds the conversation limits (64 frames, 256 KiB)";
    pub const TRANSCRIPT_ORDER: &str = "transcript.jsonl is not, in order, initialize, its reply, notifications/initialized, \
        tools/list and its reply as the harness responder answers them";
    pub const SECOND_INITIALIZE: &str = "transcript.jsonl has more than one initialize";
    pub const REFUSED_FRAME: &str = "transcript.jsonl has a frame the harness responder refused";
    pub const TRANSCRIPT_FACTS: &str = "acceptance-manifest.json initialize observations are not those of the replayed transcript";
    pub const DECISION: &str = "acceptance-manifest.json observations fail the harness decision (not all four exits 0)";
}

/// `transcript.jsonl` back into frames: typed lines, each canonical, in
/// sequence, its payload matching its digest and length.
pub(super) fn transcript_frames(transcript: &[u8]) -> std::result::Result<Vec<Frame>, &'static str> {
    use evidence_refusal as refuse;
    if transcript.is_empty() || !transcript.ends_with(b"\n") { return Err(refuse::TRANSCRIPT_TRUNCATED); }
    let mut frames = Vec::new();
    for line in transcript[..transcript.len() - 1].split(|byte| *byte == b'\n') {
        if frames.len() >= MAX_FRAMES { return Err(refuse::TRANSCRIPT_LIMIT); }
        let entry: TranscriptLine = serde_json::from_slice(line).map_err(|_| refuse::TRANSCRIPT_LINES)?;
        if canonical_of(&entry) != line || entry.seq != frames.len() as u64 + 1 { return Err(refuse::TRANSCRIPT_LINES); }
        let direction = match entry.direction.as_str() {
            "runtime_to_harness" => Direction::RuntimeToHarness,
            "harness_to_runtime" => Direction::HarnessToRuntime,
            _ => return Err(refuse::TRANSCRIPT_LINES),
        };
        let payload = base64_decode(&entry.payload_base64).ok_or(refuse::TRANSCRIPT_PAYLOAD)?;
        if entry.length != payload.len() as u64 || entry.sha256 != hex(&sha(&payload).0) { return Err(refuse::TRANSCRIPT_FRAME); }
        frames.push(Frame { direction, payload });
    }
    Ok(frames)
}

/// Replay the runtime's frames through the SAME [`Responder`] the harness
/// ran (releasing the held `initialize` as the harness does): every frame of
/// the transcript, both directions, must be exactly what it produces, and
/// the conversation must be the complete five-frame one.
pub(super) fn replay(frames: &[Frame]) -> std::result::Result<InitializeFacts, &'static str> {
    use evidence_refusal as refuse;
    let mut responder = Responder::default();
    for frame in frames.iter().filter(|frame| frame.direction == Direction::RuntimeToHarness) {
        let mut bytes = frame.payload.clone();
        bytes.push(b'\n');
        responder.feed(&bytes).map_err(|_| refuse::TRANSCRIPT_LIMIT)?;
        if responder.initialize_held() { responder.release_initialize(true).map_err(|_| refuse::TRANSCRIPT_LIMIT)?; }
    }
    if responder.frames() != frames { return Err(refuse::TRANSCRIPT_ORDER); }
    let facts = responder.facts();
    if facts.initialize_requests > 1 { return Err(refuse::SECOND_INITIALIZE); }
    if facts.refused_frames > 0 { return Err(refuse::REFUSED_FRAME); }
    if facts.initialize_requests != 1 || !responder.complete() || facts.frames != 5 { return Err(refuse::TRANSCRIPT_ORDER); }
    Ok(facts)
}

/// One record against its evidence (F4): the manifest bytes hash to the
/// record's `transcript_digest`, parse as the typed [`EvidenceDocument`]
/// (unknown fields refused) whose canonical encoding is byte-identical,
/// name this domain, format and architecture, carry well-formed digests (the
/// harness one equal to `harness_pin` when given) equal to the record's, the
/// release manifest's runtime/bridge artifacts and its trampoline (F1); the
/// transcript is the one named, replays through the [`Responder`] and yields
/// the stated initialize facts; and the stated observations pass [`decide`].
pub(super) fn check_evidence(manifest_bytes: &[u8], transcript: &[u8], acceptance: &McpAcceptance, record: &RuntimeRecord,
    release: &ReleaseManifest, arch: Arch, harness_pin: Option<Sha256Digest>) -> std::result::Result<(), &'static str> {
    use evidence_refusal as refuse;
    if sha(manifest_bytes) != acceptance.transcript_digest { return Err(refuse::NOT_RECORD); }
    let document: EvidenceDocument = serde_json::from_slice(manifest_bytes).map_err(|_| refuse::SCHEMA)?;
    if encode_evidence(&document) != manifest_bytes { return Err(refuse::CANONICAL); }
    if document.domain != EVIDENCE_DOMAIN { return Err(refuse::DOMAIN); }
    if document.format != EVIDENCE_FORMAT { return Err(refuse::FORMAT); }
    if document.arch != arch_name(arch) { return Err(refuse::ARCH); }
    let digest = |text: &str| parse_digest_hex(text).ok_or(refuse::DIGEST_SYNTAX);
    let harness = digest(&document.harness_sha256)?;
    let trampoline = digest(&document.trampoline_sha256)?;
    let runtime = digest(&document.runtime_sha256)?;
    let bridge = digest(&document.bridge_sha256)?;
    let projection = digest(&document.projection_sha256)?;
    let launch = digest(&document.launch_policy_sha256)?;
    let transcript_digest = digest(&document.transcript_sha256)?;
    if harness_pin.is_some_and(|pin| pin != harness) { return Err(refuse::HARNESS_PIN); }
    if runtime != acceptance.runtime_digest || bridge != acceptance.bridge_digest || projection != acceptance.projection_digest
        || launch != acceptance.launch_policy_digest {
        return Err(refuse::RECORD_DIGESTS);
    }
    if acceptance.inherited_fd != duduclaw_mcp_boundary::config::MCP_FD || acceptance.initialize_exit != 0
        || acceptance.parent_exec_exit != 0 || acceptance.mcp_child_exec_exit != 0 || acceptance.exclusive_config_exit != 0 {
        return Err(refuse::RECORD_EXITS);
    }
    let ExecutableRecord::Elf { entry, digest: record_digest } = &record.executable else { return Err(refuse::NOT_ELF) };
    let runtime_artifact = release.artifacts.iter().find(|a| artifact_name(&a.install_target).is_ok_and(|name| name == entry.as_str()))
        .ok_or(refuse::ARTIFACTS)?;
    let bridge_artifact = release.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge).ok_or(refuse::ARTIFACTS)?;
    if runtime != *record_digest || runtime_artifact.sha256 != runtime || bridge_artifact.sha256 != bridge {
        return Err(refuse::ARTIFACTS);
    }
    let shipped: Vec<_> = release.artifacts.iter().filter(|a| a.install_target == InstallTarget::Trampoline).collect();
    let [shipped] = shipped.as_slice() else { return Err(refuse::NO_TRAMPOLINE) };
    if shipped.sha256 != trampoline { return Err(refuse::TRAMPOLINE); }
    if sha(transcript) != transcript_digest { return Err(refuse::TRANSCRIPT_FILE); }
    let replayed = replay(&transcript_frames(transcript)?)?;
    let observations = observations_of(&document, runtime, bridge, trampoline)?;
    let stated = &observations.initialize;
    if (stated.initialize_requests, stated.initialized_after_reply, stated.tools_list_answered, stated.refused_frames, stated.frames)
        != (replayed.initialize_requests, replayed.initialized_after_reply, replayed.tools_list_answered, replayed.refused_frames,
            replayed.frames)
        || document.observations.exclusive_config.initialize_requests != stated.initialize_requests {
        return Err(refuse::TRANSCRIPT_FACTS);
    }
    decide(&observations).map_err(|_| refuse::DECISION)
}

/// `verify --evidence-dir <dir>`: every runtime with an acceptance record must
/// have an `acceptance-manifest.json` (in `<dir>` or one of its immediate
/// subdirectories, e.g. `<dir>/<label>/`) whose SHA-256 is the record's
/// `transcript_digest`, checked by [`check_evidence`] with the
/// `transcript.jsonl` beside it. `release` is the `--manifest` document
/// (required, F1); `harness_pin` is `--harness-sha256` when given.
pub(super) fn verify_evidence(policy: &SignedPolicy, release: &ReleaseManifest, arch: Arch, directory: &Path,
    harness_pin: Option<Sha256Digest>) -> std::result::Result<String, String> {
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
        check_evidence(&manifest, &transcript, acceptance, record, release, arch, harness_pin).map_err(refuse)?;
        checked += 1;
    }
    let pin = if harness_pin.is_some() { "the pinned harness" } else { "an UNPINNED harness (no --harness-sha256)" };
    Ok(format!("evidence: {checked} acceptance record(s) backed by harness evidence in {} ({pin}, the manifest's trampoline)\n",
        directory.display()))
}

#[cfg(test)]
#[path = "installed_policy_tool_accept_tests.rs"]
mod tests;
