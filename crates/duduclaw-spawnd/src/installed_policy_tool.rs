//! R7d isolation policy generator: `duduclaw-isolation-policy build|verify`
//! (feature `policy-tool`; README "Isolation policy generator"), and R8r-4's
//! MCP acceptance harness `accept` / `verify --evidence-dir` ([`accept`],
//! README "MCP acceptance harness").
//!
//! `build` turns a small human-authored SOURCE document into a signed-policy
//! v4 document (the `runtime-isolation-policy-v1` artifact): every digest is
//! computed from the real files of `--artifacts-dir`, every profile id is
//! derived from a label ([`profile_id`]), and the result goes through
//! [`check_policy`] — spawnd's own reader and publication checks (this is a
//! child module of `installed`, so they are called, never copied) — before it
//! is written (mode 0444, never over an existing file). `verify` runs the same
//! checks on an existing policy and prints a short summary.
//!
//! Inputs are named by their installed generation file names
//! (`duduclaw_updated::publication::artifact_name`): `runtime-<registry_id>`
//! for runtime entries, `duduclaw-egress-forwarder`, `duduclaw-mcp-bridge`.
//! Without `--manifest` the cross-check runs against a manifest synthesised
//! from those files (runtime ELF entries as the packager signs them, 0755);
//! with `--manifest` against that (signed) manifest, and every file of
//! `--artifacts-dir` it also lists must have the same digest and size.
use super::*;
use std::{collections::BTreeMap, ffi::OsString, io::Read as _, path::PathBuf};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use duduclaw_isolation::update::ReleaseArtifact;
use duduclaw_run::seccomp::{Architecture as Arch, RuntimeProfile};

type Checked<T> = std::result::Result<T, String>;

/// `schema` of every source document this tool reads.
pub const SOURCE_SCHEMA: &str = "duduclaw-isolation-policy-source-v1";
/// Domain of [`profile_id`]. Nothing in the product defines how profile ids
/// are chosen (they are opaque signed values); this tool derives them as
/// `sha256(PROFILE_DOMAIN ++ label)` so the same label always yields the same id.
pub const PROFILE_DOMAIN: &[u8] = b"duduclaw-isolation-profile-v1\0";
const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_SYSCALL_FILE_BYTES: u64 = 64 * 1024;
const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

const USAGE: &str = "usage:
  duduclaw-isolation-policy build  --source <policy-source.json> --artifacts-dir <dir> --arch <x86_64|aarch64>
                                   --out <new policy.json> [--manifest <manifest.json>]
  duduclaw-isolation-policy verify --policy <policy.json> --artifacts-dir <dir>
                                   [--manifest <manifest.json>] [--arch <x86_64|aarch64>]
                                   [--evidence-dir <dir> (requires --manifest) [--harness-sha256 <hex>]]
  sudo env DUDUCLAW_ISO_DISPOSABLE=1 duduclaw-isolation-policy accept
                                   --source <policy-source.json> --artifacts-dir <dir> --arch <x86_64|aarch64>
                                   --label <runtime label> --trampoline <duduclaw-run>
                                   --trampoline-sha256 <hex of the duduclaw-run the release manifest names>
                                   --evidence-dir <new empty dir> --out-source <new file>
                                   (root, disposable host without an isolation installation only)";

// ── source document ─────────────────────────────────────────────────────

/// Human-authored input of `build` (README "Source format").
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicySource {
    schema: String,
    /// Free text for reviewers (never copied into the policy).
    #[serde(default, rename = "comment")]
    _comment: Option<String>,
    dns_probe_name: String,
    /// Syscall allow-list of the egress forwarder (one per architecture);
    /// required exactly when a runtime uses `network: egress`.
    #[serde(default)]
    forwarder_seccomp: Option<PerArch>,
    runtimes: Vec<RuntimeSource>,
}

/// Paths of syscall allow-list files, relative to the source document's directory.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PerArch {
    #[serde(default)]
    x86_64: Option<String>,
    #[serde(default)]
    aarch64: Option<String>,
}

/// `"default"` (the gateway's required main-run arguments for the kind) or an explicit list.
#[derive(Deserialize)]
#[serde(untagged)]
enum FixedArgsSource { Keyword(String), Explicit(Vec<String>) }

/// The policy's network shape with an egress profile LABEL instead of an id.
#[derive(Deserialize)]
#[serde(tag = "kind", content = "profile", rename_all = "snake_case", deny_unknown_fields)]
enum NetworkSource { Offline, Egress(String) }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSource {
    seccomp: PerArch,
    tmpfs_bytes: u64,
    tmpfs_inodes: u64,
    proc_subset_pid: bool,
    /// Copied verbatim into the native record (helper bindings name profile
    /// ids: use `profile_id(<label>)`, printed by `build`).
    #[serde(default)]
    launcher: Option<crate::helper_policy::LauncherRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSource {
    label: String,
    /// Free text for reviewers (never copied into the policy).
    #[serde(default, rename = "comment")]
    _comment: Option<String>,
    runtime: RuntimeKind,
    /// Generation file name of the ELF in `--artifacts-dir`, e.g. `runtime-api-worker-v1`.
    executable: String,
    pinned_version: String,
    fixed_args: FixedArgsSource,
    /// Claude with `fixed_args: "default"` only: the `--model` value.
    #[serde(default)]
    model: Option<String>,
    #[serde(with = "ResourceRecord")]
    resources: ResourceLimits,
    #[serde(with = "TimeoutRecord")]
    timeouts: RunTimeouts,
    network: NetworkSource,
    mcp: bool,
    pty_allowed: bool,
    queue_class: QueueClass,
    #[serde(default)]
    turn_limit: Option<TurnLimit>,
    native: NativeSource,
    /// Copied verbatim from an acceptance record. Without it the policy loads,
    /// but spawnd refuses every MCP run of this runtime at MCP preparation.
    #[serde(default)]
    mcp_acceptance: Option<McpAcceptance>,
}

// ── pure parts ──────────────────────────────────────────────────────────

/// `sha256("duduclaw-isolation-profile-v1\0" ++ label)`, for runtime and egress profile labels.
pub fn profile_id(label: &str) -> ProfileId {
    let mut hasher = Sha256::new();
    hasher.update(PROFILE_DOMAIN);
    hasher.update(label.as_bytes());
    ProfileId(Sha256Digest(hasher.finalize().into()))
}

fn check_label(what: &str, label: &str) -> Checked<()> {
    let ok = !label.is_empty() && label.len() <= 64 && !label.starts_with('-') && !label.ends_with('-')
        && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else {
        Err(format!("{what} {label:?}: rule label charset: 1-64 characters of [a-z0-9-], not starting or ending with '-'"))
    }
}

fn bounded<const N: usize>(value: String, label: &str, what: &str) -> Checked<Text<N>> {
    Text::new(value).map_err(|_| format!("runtime {label}: {what} is longer than {N} bytes"))
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn short(digest: &Sha256Digest) -> String { hex(&digest.0[..6]) }

fn parse_arch(value: &str) -> Checked<Arch> {
    match value { "x86_64" => Ok(Arch::X86_64), "aarch64" => Ok(Arch::Aarch64),
        _ => Err(format!("--arch must be x86_64 or aarch64, not {value:?}")) }
}
fn arch_name(arch: Arch) -> &'static str { match arch { Arch::X86_64 => "x86_64", Arch::Aarch64 => "aarch64" } }
fn manifest_arch(arch: Arch) -> duduclaw_isolation::update::Architecture {
    match arch { Arch::X86_64 => duduclaw_isolation::update::Architecture::X86_64,
        Arch::Aarch64 => duduclaw_isolation::update::Architecture::Aarch64 }
}
fn kind_name(kind: &RuntimeKind) -> String {
    match kind { RuntimeKind::Claude => "claude".into(), RuntimeKind::Codex => "codex".into(), RuntimeKind::Grok => "grok".into(),
        RuntimeKind::OpenAiCompat => "open_ai_compat".into(), RuntimeKind::Registered(name) => format!("registered:{}", name.as_str()) }
}

/// One syscall NUMBER (decimal) or an inclusive range `N-M` per line; `#`
/// starts a comment; blank lines are ignored. Names are not accepted.
fn parse_syscalls(text: &str) -> Checked<Vec<u32>> {
    let number = |value: &str| if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) { value.parse::<u32>().ok() } else { None };
    let mut calls = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let (first, last) = match line.split_once('-') {
            Some((first, last)) => (number(first.trim()), number(last.trim())),
            None => (number(line), number(line)),
        };
        let (Some(first), Some(last)) = (first, last) else {
            return Err(format!("line {}: {line:?} is not a decimal syscall number or an inclusive range N-M (syscall names are not accepted)", index + 1));
        };
        if first > last || last - first >= 512 {
            return Err(format!("line {}: range {line:?} is empty or longer than 512 syscalls", index + 1));
        }
        calls.extend(first..=last);
        if calls.len() > 512 { return Err("more than 512 syscalls (RuntimeProfile limit)".into()); }
    }
    calls.sort_unstable();
    Ok(calls)
}

fn read_limited(path: &Path, limit: u64, what: &str) -> Checked<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{what} {}: {e}", path.display()))?;
    if !metadata.file_type().is_file() { return Err(format!("{what} {}: not a regular file (symbolic links are refused)", path.display())); }
    if metadata.len() > limit { return Err(format!("{what} {}: larger than {limit} bytes", path.display())); }
    let mut bytes = Vec::new();
    File::open(path).and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("{what} {}: {e}", path.display()))?;
    if bytes.len() as u64 > limit { return Err(format!("{what} {}: larger than {limit} bytes", path.display())); }
    Ok(bytes)
}

/// (sha256, size) of a regular file's real bytes; symbolic links are refused.
fn file_digest(path: &Path) -> Checked<(Sha256Digest, u64)> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.file_type().is_file() { return Err(format!("{}: not a regular file (symbolic links are refused)", path.display())); }
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    if size != metadata.len() || size == 0 { return Err(format!("{}: empty or changed while it was hashed", path.display())); }
    Ok((Sha256Digest(hasher.finalize().into()), size))
}

/// The install target whose generation file name is `name`, if any this tool hashes.
fn generation_target(name: &str) -> Option<InstallTarget> {
    let target = match name {
        "duduclaw-egress-forwarder" => InstallTarget::EgressForwarder,
        "duduclaw-mcp-bridge" => InstallTarget::McpBridge,
        _ => InstallTarget::RuntimeEntry { registry_id: name.strip_prefix("runtime-")?.into() },
    };
    artifact_name(&target).is_ok_and(|generation| generation == name).then_some(target)
}

/// Manifest artifacts for the files of `directory` this tool knows (others
/// are ignored); the policy entry only with `include_policy`.
fn scan_artifacts(directory: &Path, include_policy: bool) -> Checked<Vec<ReleaseArtifact>> {
    let policy_target = InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() };
    let entries = fs::read_dir(directory).map_err(|e| format!("--artifacts-dir {}: {e}", directory.display()))?;
    let mut artifacts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("--artifacts-dir {}: {e}", directory.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some(target) = generation_target(&name) else { continue };
        let policy = target == policy_target;
        if policy && !include_policy { continue; }
        let (sha256, size_bytes) = file_digest(&entry.path())?;
        artifacts.push(ReleaseArtifact { id: name, sha256, size_bytes, mode: if policy { 0o444 } else { 0o755 }, install_target: target });
    }
    artifacts.sort_by(|a, b| a.id.cmp(&b.id));
    if artifacts.len() > duduclaw_isolation::update::MAX_ARTIFACTS { return Err("--artifacts-dir holds more than 128 artifacts".into()); }
    Ok(artifacts)
}

fn synthetic_manifest(arch: Arch, artifacts: Vec<ReleaseArtifact>) -> Checked<ReleaseManifest> {
    use duduclaw_isolation::update::{OperatingSystem, Product, ProtocolCompatibility, ReleaseChannel};
    let version = duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).map_err(|_| "workspace version".to_owned())?;
    Ok(ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version, channel: ReleaseChannel::Stable,
        os: OperatingSystem::Linux, arch: manifest_arch(arch), minimum_version: version, serial: 1, created_at_unix: 1,
        expires_at_unix: 2, artifacts, protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
            updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] })
}

/// The manifest the cross-check runs against (module docs).
fn load_manifest(directory: &Path, manifest: Option<&Path>, arch: Arch, include_policy: bool) -> Checked<ReleaseManifest> {
    let scanned = scan_artifacts(directory, include_policy)?;
    let Some(path) = manifest else { return synthetic_manifest(arch, scanned) };
    let manifest: ReleaseManifest = serde_json::from_slice(&read_limited(path, MAX_MANIFEST_BYTES, "--manifest")?)
        .map_err(|e| format!("--manifest {}: not a release manifest: {e}", path.display()))?;
    manifest.validate().map_err(|e| format!("--manifest {}: ReleaseManifest::validate refused it: {e}", path.display()))?;
    if manifest.arch != manifest_arch(arch) { return Err(format!("--manifest {} is not a {} manifest", path.display(), arch_name(arch))); }
    for file in &scanned {
        if let Some(signed) = manifest.artifacts.iter().find(|a| a.install_target == file.install_target) {
            if signed.sha256 != file.sha256 || signed.size_bytes != file.size_bytes {
                return Err(format!("--artifacts-dir {} differs from the manifest's artifact {}", file.id, signed.id));
            }
        }
    }
    Ok(manifest)
}

fn arch_file<'a>(files: &'a PerArch, arch: Arch) -> Option<&'a String> {
    match arch { Arch::X86_64 => files.x86_64.as_ref(), Arch::Aarch64 => files.aarch64.as_ref() }
}

/// The runtime seccomp profile of an allow-list file and its digest for `arch`
/// (`duduclaw_run::seccomp::RuntimeProfile::digest`, the digest spawnd checks).
fn seccomp_profile(base: &Path, files: &PerArch, arch: Arch, what: &str) -> Checked<(RuntimeProfile, Sha256Digest)> {
    let file = arch_file(files, arch).ok_or_else(|| format!("{what}: no seccomp allow-list file for {}", arch_name(arch)))?;
    let path = base.join(file);
    let bytes = read_limited(&path, MAX_SYSCALL_FILE_BYTES, what)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{what} {}: not UTF-8", path.display()))?;
    let profile = RuntimeProfile { allowed_syscalls: parse_syscalls(text).map_err(|e| format!("{what} {}: {e}", path.display()))? };
    let digest = profile.digest(arch).map_err(|e| format!("{what} {}: refused by RuntimeProfile::canonical_bytes \
        (1-512 unique syscall numbers below 0x40000000): {e}", path.display()))?;
    Ok((profile, digest))
}

/// `"default"`: exactly the arguments the gateway requires for the kind
/// (`gateway_plan`: Claude, or `duduclaw_mcp_boundary::config`).
fn source_fixed_args(runtime: &RuntimeSource) -> Checked<Vec<String>> {
    let label = &runtime.label;
    if runtime.model.is_some() && (runtime.runtime != RuntimeKind::Claude || !matches!(&runtime.fixed_args, FixedArgsSource::Keyword(_))) {
        return Err(format!("runtime {label}: `model` only selects the --model of the Claude default arguments (fixed_args \"default\")"));
    }
    match &runtime.fixed_args {
        FixedArgsSource::Explicit(arguments) => Ok(arguments.clone()),
        FixedArgsSource::Keyword(word) if word == "default" => {
            if !runtime.mcp {
                return Err(format!("runtime {label}: fixed_args \"default\" is the gateway's MCP main-run shape; \
                    a runtime with mcp: false must list its fixed_args"));
            }
            match &runtime.runtime {
                RuntimeKind::Claude => {
                    let model = runtime.model.as_deref().unwrap_or("");
                    let arguments = if runtime.turn_limit.is_some() { crate::gateway_plan::claude_ranged_arguments(model) }
                        else { crate::gateway_plan::claude_arguments(model) };
                    arguments.map_err(|_| format!("runtime {label}: model {model:?} is not a plain value (gateway_plan::claude_arguments)"))
                }
                RuntimeKind::OpenAiCompat => Ok(duduclaw_mcp_boundary::config::api_worker_arguments()),
                RuntimeKind::Codex | RuntimeKind::Grok => duduclaw_mcp_boundary::config::native_cli_arguments(&runtime.runtime)
                    .map_err(|_| format!("runtime {label}: no native adapter arguments")),
                RuntimeKind::Registered(_) => duduclaw_mcp_boundary::config::registered_cli_arguments(&runtime.runtime)
                    .map_err(|_| format!("runtime {label}: no default fixed_args for this registered runtime \
                        (only generic-cli-v1.<id> has one); list them")),
            }
        }
        FixedArgsSource::Keyword(other) => Err(format!("runtime {label}: fixed_args must be \"default\" or a list of strings, not {other:?}")),
    }
}

/// Output of [`generate`]: the policy bytes and the label → id mapping.
struct Generated { bytes: Vec<u8>, runtimes: Vec<(String, ProfileId)>, egress: Vec<(String, ProfileId)> }

/// The policy v4 document of `source` for `arch` (not yet checked: see [`check_policy`]).
fn generate(source: PolicySource, base: &Path, artifacts: &Path, arch: Arch) -> Checked<Generated> {
    if source.schema != SOURCE_SCHEMA { return Err(format!("schema must be {SOURCE_SCHEMA:?}")); }
    duduclaw_isolation::egress::validate_dns_probe_name(&source.dns_probe_name).map_err(|_| format!(
        "dns_probe_name {:?}: refused by egress::validate_dns_probe_name (lower-case ASCII hostname, at least two labels, \
         no trailing dot, not an IP address, not localhost/.local/.internal/.localhost)", source.dns_probe_name))?;
    if source.runtimes.is_empty() || source.runtimes.len() > 128 { return Err("runtimes: 1 to 128 entries".into()); }
    let mut runtime_labels = Vec::new();
    let mut egress_labels: Vec<String> = Vec::new();
    for runtime in &source.runtimes {
        check_label("runtime label", &runtime.label)?;
        if runtime_labels.contains(&runtime.label) { return Err(format!("runtime label {:?}: rule unique runtime labels", runtime.label)); }
        runtime_labels.push(runtime.label.clone());
        if let NetworkSource::Egress(label) = &runtime.network {
            check_label("egress profile label", label)?;
            if !egress_labels.contains(label) { egress_labels.push(label.clone()); }
        }
    }
    if let Some(label) = egress_labels.iter().find(|label| runtime_labels.contains(label)) {
        return Err(format!("egress profile label {label:?} is also a runtime label (rule: runtime and egress labels are disjoint)"));
    }
    let forwarder = match (&source.forwarder_seccomp, egress_labels.is_empty()) {
        (None, true) => None,
        (Some(_), true) => return Err("forwarder_seccomp is given but no runtime uses network egress".into()),
        (None, false) => return Err("forwarder_seccomp is required: a runtime with network egress needs the forwarder record".into()),
        (Some(files), false) => {
            let (seccomp, seccomp_digest) = seccomp_profile(base, files, arch, "forwarder_seccomp")?;
            let entry = artifact_name(&InstallTarget::EgressForwarder).map_err(|_| "forwarder entry".to_owned())?;
            let (executable_digest, _) = file_digest(&artifacts.join(&entry))?;
            Some((entry, executable_digest, seccomp, seccomp_digest))
        }
    };
    let mut runtimes = Vec::new();
    let mut native = Vec::new();
    let mut labels = Vec::new();
    for runtime in source.runtimes {
        let label = runtime.label.clone();
        let arguments = source_fixed_args(&runtime)?;
        let name = runtime.executable.as_str();
        if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
            return Err(format!("runtime {label}: executable must be a generation file name in --artifacts-dir, not {name:?}"));
        }
        let (digest, _) = file_digest(&artifacts.join(name))?;
        let vendor_digest = match runtime.runtime {
            RuntimeKind::Codex => Some(file_digest(&artifacts.join("runtime-vendor-codex"))?.0),
            RuntimeKind::Grok => Some(file_digest(&artifacts.join("runtime-vendor-grok"))?.0),
            _ => None,
        };
        let profile = profile_id(&label);
        let network = match &runtime.network {
            NetworkSource::Offline => NetworkPolicy::Offline,
            NetworkSource::Egress(egress) => NetworkPolicy::Egress(profile_id(egress)),
        };
        let (seccomp, runtime_digest) = seccomp_profile(base, &runtime.native.seccomp, arch, &format!("runtime {label} seccomp"))?;
        let forwarder_record = match (&network, &forwarder) {
            (NetworkPolicy::Egress(_), Some((entry, executable_digest, forwarder_seccomp, seccomp_digest))) => Some(ForwarderRecord {
                entry: entry.clone(), executable_digest: *executable_digest,
                argv: vec![crate::native_trampoline::forwarder_argv0(entry), "forwarder".into(), crate::policy::RUNTIME_PROXY_PORT.to_string()],
                seccomp: forwarder_seccomp.clone(), seccomp_digest: *seccomp_digest }),
            _ => None,
        };
        let entry: Text<128> = bounded(name.to_owned(), &label, "executable")?;
        let pinned_version: Text<64> = bounded(runtime.pinned_version, &label, "pinned_version")?;
        let fixed_args: Vec<Text<4096>> = arguments.into_iter().map(|arg| bounded(arg, &label, "a fixed argument")).collect::<Checked<_>>()?;
        runtimes.push(RuntimeRecord { runtime: runtime.runtime, profile, executable: ExecutableRecord::Elf { entry, digest },
            pinned_version, fixed_args,
            resources: runtime.resources, timeouts: runtime.timeouts, network, mcp: runtime.mcp, pty_allowed: runtime.pty_allowed,
            queue_class: runtime.queue_class, mcp_acceptance: runtime.mcp_acceptance, turn_limit: runtime.turn_limit });
        native.push(NativeRecord { vendor_digest, profile, runtime: seccomp, runtime_digest, forwarder: forwarder_record,
            tmpfs_bytes: runtime.native.tmpfs_bytes, tmpfs_inodes: runtime.native.tmpfs_inodes,
            proc_subset_pid: runtime.native.proc_subset_pid, launcher: runtime.native.launcher });
        labels.push((label, profile));
    }
    let document = SignedPolicyV4 { domain: POLICY_DOMAIN_V4.into(), version: 4, runtimes, native, dns_probe_name: source.dns_probe_name };
    let bytes = serde_json::to_vec(&document).map_err(|e| format!("cannot serialize the policy: {e}"))?;
    Ok(Generated { bytes, runtimes: labels, egress: egress_labels.into_iter().map(|label| { let id = profile_id(&label); (label, id) }).collect() })
}

fn placeholder_employee(index: usize) -> EmployeeId {
    let mut id = [0xee; 16];
    id[14] = (index >> 8) as u8;
    id[15] = index as u8;
    EmployeeId(id)
}

fn describe(index: usize, record: &RuntimeRecord) -> String {
    format!("runtime #{index} ({}, profile {})", kind_name(&record.runtime), short(&record.profile.0))
}

/// spawnd's and the installer's checks, in order, each failure naming its
/// rule. All are the production functions: the installer reader
/// (`signed_policy`), per runtime `systemd::UnitPolicy::new`,
/// `policy::validate_turn_limit` and `RootPolicy::from_verified_publication`
/// (to name the failing runtime), `validate_runtime_artifacts`, each native
/// record's `seccomp::runtime`, `validate_launchers`, then the complete
/// `bundle` sequence (`verified_policy`), `load`'s publication
/// (`GatewayPublication::policy` + `with_launchers`), `RootPolicy::derive` per
/// runtime (with PTY when allowed), the installer's S5 readers
/// (`signed_policy_egress_profiles`: at least one egress profile;
/// `signed_policy_dns_probe_name`), the forwarder rules of `native_profile`,
/// the trampoline (`forwarder_argv0`, presence iff egress), egress `forwarder`
/// argv and the runner's `seccomp::runtime`, an MCP acceptance's
/// `validate_launch` and `validate` against the manifest's bridge, and with
/// `bind_policy` (verify) the manifest's own policy entry (`policy_artifact`,
/// `policy_bytes_match`) when it has one — required with `require_bound`.
fn check_policy(bytes: &[u8], manifest: &ReleaseManifest, arch: Arch, bind_policy: bool, require_bound: bool) -> Checked<SignedPolicy> {
    if bytes.len() as u64 > MAX_POLICY_BYTES { return Err("the policy exceeds 1 MiB (signed_policy limit)".into()); }
    let policy = parse_policy(bytes).map_err(|_| "the strict policy reader (installed::parse_policy) refused it: version and \
        domain, dns_probe_name, turn_limit keys and ranges, the Claude MCP argument shape, unknown fields".to_owned())?;
    signed_policy(bytes).map_err(|_| "the installer reader (installed::signed_policy) refused it: one native record per runtime profile".to_owned())?;
    for (index, record) in policy.runtimes.iter().enumerate() {
        let name = describe(index, record);
        let runtime = record.to_runtime();
        crate::systemd::UnitPolicy::new(duduclaw_isolation::ids::RunId([0; 16]), EmployeeId([0; 16]), &runtime.resources,
            &runtime.timeouts, 1, 1).map_err(|_| format!("{name}: resources/timeouts refused by systemd::UnitPolicy::new \
            (every resource limit and the startup/runtime/termination timeouts non-zero, ptys >= 1, open_files >= 1184, \
            memfd_total_bytes <= 64 MiB)"))?;
        crate::policy::validate_turn_limit(&runtime).map_err(|_| format!("{name}: refused by policy::validate_turn_limit \
            (turn_limit only on a Claude MCP record, 1 <= min <= default <= max <= 1000, canonical Claude arguments)"))?;
        RootPolicy::from_verified_publication(vec![runtime], vec![]).map_err(|_| format!("{name}: refused by \
            RootPolicy::from_verified_publication (pinned_version non-empty and not latest/*, at most 128 fixed_args \
            without NUL, executable entry of [A-Za-z0-9._-])"))?;
    }
    // R8n-1 review F1: one signed record per in-run helper kind. spawnd's
    // launcher rule refuses duplicate kinds, the gateway's entitlement leaves
    // an ambiguous kind out and its plan construction refuses the helper set.
    let mut helper_kinds = Vec::new();
    for (index, record) in policy.runtimes.iter().enumerate() {
        if !crate::helper_policy::is_helper_kind(&record.runtime) { continue; }
        if helper_kinds.contains(&&record.runtime) {
            return Err(format!("{}: the helper kind is signed twice (rule: at most one record per in-run helper kind; \
                the gateway refuses the whole helper set as invalid_helper_runtime)", describe(index, record)));
        }
        helper_kinds.push(&record.runtime);
    }
    validate_runtime_artifacts(&policy, manifest).map_err(|_| "installed::validate_runtime_artifacts refused it: every ELF \
        entry must be a manifest artifact's generation name with the same sha256 and mode 0755; open_ai_compat runs \
        runtime-api-worker-v1; codex/grok run runtime-native-adapter-v1 with vendor_digest = the runtime-vendor-codex/-grok \
        artifact; other kinds carry no vendor_digest".to_owned())?;
    for native in &policy.native {
        duduclaw_run::seccomp::runtime(arch, &native.runtime, native.runtime_digest).map_err(|e| format!("profile {}: runtime \
            seccomp refused by seccomp::runtime ({e}): runtime_digest must be the {} digest of the list, which must allow \
            execveat ({}) and exit_group ({})", short(&native.profile.0), arch_name(arch), arch.execveat(), arch.exit_group()))?;
    }
    validate_launchers_for(&policy, arch).map_err(|_| "installed::validate_launchers refused a helper launcher (policy v2+, \
        0 < helper_tmpfs_bytes <= tmpfs_bytes, helper_policy::validate_launcher checks 1-7)".to_owned())?;
    let policy = verified_policy(bytes, manifest, arch).map_err(|_| "the complete bundle checks (installed::verified_policy) \
        refused it: native profile ids must be unique".to_owned())?;
    let entitlements = policy.runtimes.iter().enumerate().map(|(index, record)| EntitlementRecord {
        employee: placeholder_employee(index), profiles: vec![record.profile], max_turns: None }).collect();
    let publication = GatewayPublication { version: 1, generation: Sha256Digest([1; 32]), boot_id: String::new(),
        checked_boottime_ms: 0, valid_for_ms: 1000, runtimes: policy.runtimes.clone(), entitlements };
    let root = publication.policy().map_err(|_| "RootPolicy::from_verified_publication refused the runtime set \
        (1-128 runtimes, unique runtime profile ids)".to_owned())?
        .with_launchers(arch, launchers(&policy)).map_err(|_| "RootPolicy::with_launchers refused a launcher".to_owned())?;
    for (index, record) in policy.runtimes.iter().enumerate() {
        let name = describe(index, record);
        root.derive(placeholder_employee(index), &record.runtime, false).map_err(|_| format!("{name}: RootPolicy::derive refused \
            a run (LaunchPolicy::validate: queue_ms 1..=3600000, startup_ms 1..=300000, runtime_ms 1..=86400000, ptys <= 64)"))?;
        if record.pty_allowed {
            root.derive(placeholder_employee(index), &record.runtime, true).map_err(|_| format!("{name}: pty_allowed, but \
                RootPolicy::derive refused a PTY run (needs pty_idle_ms in 1..=runtime_ms)"))?;
        }
    }
    let profiles = signed_policy_egress_profiles(bytes).map_err(|_| "installed::signed_policy_egress_profiles refused it".to_owned())?;
    if profiles.is_empty() { return Err("installer step S5 (egress_document) needs at least one runtime with network egress".into()); }
    if signed_policy_dns_probe_name(bytes).ok().flatten().is_none() {
        return Err("no dns_probe_name (policy older than v4): spawnd never reports the DNS route as verified".into());
    }
    for (index, record) in policy.runtimes.iter().enumerate() {
        let name = describe(index, record);
        let native = policy.native.iter().find(|n| n.profile == record.profile).ok_or_else(|| format!("{name}: no native record"))?;
        // The trampoline's root configuration of every run (native_trampoline prepare).
        duduclaw_run::linux::mounts::RootConfig { employee_uid: duduclaw_isolation::ids::EMPLOYEE_UID_MIN,
            employee_gid: duduclaw_isolation::ids::EMPLOYEE_UID_MIN, pty: false, pty_limit: record.resources.ptys.max(1),
            work_noexec: true, tmp_noexec: true, tmpfs_bytes: native.tmpfs_bytes, tmpfs_inodes: native.tmpfs_inodes }
            .validate().map_err(|_| format!("{name}: refused by duduclaw-run RootConfig::validate (tmpfs_bytes 1..=1 GiB, \
            tmpfs_inodes 1..=1000000, ptys <= 256)"))?;
        let egress = matches!(record.network, NetworkPolicy::Egress(_));
        if native.forwarder.is_some() != egress {
            return Err(format!("{name}: a forwarder record is required exactly for network egress (native_trampoline prepare)"));
        }
        if let Some(f) = &native.forwarder {
            if !forwarder_artifact_matches(manifest, f) {
                return Err(format!("{name}: the forwarder must name the manifest's egress_forwarder artifact by entry \
                    duduclaw-egress-forwarder and sha256, with 1-128 arguments (installed native_profile)"));
            }
            if f.argv.first() != Some(&crate::native_trampoline::forwarder_argv0(&f.entry)) {
                return Err(format!("{name}: forwarder argv[0] must be /runtime/{} (native_trampoline prepare)", f.entry));
            }
            let port = crate::policy::RUNTIME_PROXY_PORT.to_string();
            if f.argv.len() > 4 || f.argv.get(1).map(String::as_str) != Some("forwarder") || f.argv.get(2) != Some(&port) {
                return Err(format!("{name}: forwarder argv must be [/runtime/{}, forwarder, {port}] (policy::RUNTIME_PROXY_PORT, the \
                    port spawnd plans as HTTP_PROXY) plus at most an EgressLimits JSON (duduclaw-egress forwarder mode)", f.entry));
            }
            if let Some(json) = f.argv.get(3) {
                let limits: duduclaw_isolation::egress::EgressLimits = serde_json::from_str(json).ok().filter(|_| json.len() <= 2048)
                    .ok_or_else(|| format!("{name}: forwarder limits argument is not an EgressLimits JSON of at most 2048 bytes"))?;
                limits.validate().map_err(|_| format!("{name}: forwarder limits refused by EgressLimits::validate"))?;
            }
            duduclaw_run::seccomp::runtime(arch, &f.seccomp, f.seccomp_digest).map_err(|e| format!("{name}: forwarder seccomp \
                refused by seccomp::runtime ({e}, the runner installs it before exec): seccomp_digest must be the {} digest of \
                the list, which must allow execveat ({}) and exit_group ({})", arch_name(arch), arch.execveat(), arch.exit_group()))?;
        }
        if let Some(acceptance) = &record.mcp_acceptance {
            acceptance.validate_launch(record, native).map_err(|_| format!("{name}: mcp_acceptance.launch_policy_digest does not \
                bind this exact runtime and native record (McpAcceptance::validate_launch)"))?;
            let bridge = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge)
                .ok_or_else(|| format!("{name}: mcp_acceptance needs duduclaw-mcp-bridge in the manifest / --artifacts-dir"))?;
            let runtime_digest = match &record.executable {
                ExecutableRecord::Elf { digest, .. } => *digest, ExecutableRecord::Script { script_digest, .. } => *script_digest,
            };
            let projection = duduclaw_mcp_boundary::config::runtime_projection(duduclaw_mcp_boundary::config::RuntimeRegistration {
                runtime: record.runtime.clone(), version: record.pinned_version.as_str().into(), runtime_digest,
                bridge_digest: bridge.sha256, bridge_executable: "/runtime/duduclaw-mcp-bridge".into() })
                .map_err(|_| format!("{name}: no MCP projection for this runtime"))?;
            acceptance.validate(&projection).map_err(|_| format!("{name}: mcp_acceptance refused by McpAcceptance::validate \
                (runtime, version, runtime/bridge/projection digests, inherited fd 3, zero exits, transcript digest)"))?;
        }
    }
    if bind_policy {
        let target = InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() };
        if manifest.artifacts.iter().any(|a| a.install_target == target) {
            let artifact = policy_artifact(manifest).map_err(|_| "the manifest's isolation-policy-v1 entry must be mode 0444 and \
                at most 1 MiB (installed::bundle)".to_owned())?;
            policy_bytes_match(artifact, bytes).map_err(|_| "the policy bytes are not the manifest's isolation-policy-v1 artifact \
                (size/sha256, installed::bundle)".to_owned())?;
        } else if require_bound {
            return Err("the manifest has no isolation-policy-v1 runtime entry; spawnd refuses such a generation (installed::bundle)".into());
        }
    }
    Ok(policy)
}

fn summary(policy: &SignedPolicy, bytes: &[u8], arch: Arch) -> String {
    let mut out = format!("isolation policy v{} for {}, sha256 {}, {} bytes\n", policy.version, arch_name(arch),
        hex(&Sha256::digest(bytes)), bytes.len());
    out += &format!("dns_probe_name: {}\n", policy.dns_probe_name.as_deref().unwrap_or("(none)"));
    for (index, record) in policy.runtimes.iter().enumerate() {
        let native = policy.native.iter().find(|n| n.profile == record.profile);
        let (entry, digest) = match &record.executable {
            ExecutableRecord::Elf { entry, digest } => (entry.as_str().to_owned(), short(digest)),
            ExecutableRecord::Script { script, script_digest, .. } => (format!("script {}", script.as_str()), short(script_digest)),
        };
        let network = match &record.network { NetworkPolicy::Offline => "offline".to_owned(),
            NetworkPolicy::Egress(profile) => format!("egress {}", short(&profile.0)) };
        out += &format!("runtime #{index}: {} profile {} entry {entry} sha256 {digest}… version {} network {network} mcp {} pty {}\n",
            kind_name(&record.runtime), short(&record.profile.0), record.pinned_version.as_str(),
            if record.mcp { "yes" } else { "no" }, if record.pty_allowed { "allowed" } else { "no" });
        if let Some(native) = native {
            out += &format!("  seccomp {} syscalls ({}…), forwarder {}, launcher {}, vendor_digest {}\n",
                native.runtime.allowed_syscalls.len(), short(&native.runtime_digest),
                native.forwarder.as_ref().map_or("none".to_owned(), |f| format!("{} {}…", f.argv.join(" "), short(&f.executable_digest))),
                if native.launcher.is_some() { "yes" } else { "none" },
                native.vendor_digest.as_ref().map_or("none".to_owned(), |d| format!("{}…", short(d))));
        }
        out += &format!("  mcp_acceptance: {}\n", match (&record.mcp_acceptance, record.mcp) {
            (Some(_), _) => "present (checked)",
            (None, true) => "ABSENT: the policy loads, but every MCP run of this runtime is refused at MCP preparation",
            (None, false) => "absent (not an MCP runtime)",
        });
    }
    out
}

// ── command line ────────────────────────────────────────────────────────

fn options(args: &[String], allowed: &[&str]) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if !allowed.contains(&flag.as_str()) { return Err(format!("unknown option {flag}")); }
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if values.insert(flag.clone(), value.clone()).is_some() { return Err(format!("{flag} is given twice")); }
    }
    Ok(values)
}

/// Read, generate and check (no output written).
fn build_policy(source_path: &Path, artifacts: &Path, arch: Arch, manifest: Option<&Path>) -> Checked<(Generated, SignedPolicy)> {
    let raw = read_limited(source_path, MAX_SOURCE_BYTES, "policy source")?;
    build_policy_bytes(&raw, source_path, artifacts, arch, manifest)
}

/// [`build_policy`] on source bytes already read; relative syscall-list paths
/// resolve against `source_path`'s directory (R8r-4: `accept` reads the source
/// once and checks its output source before writing it).
fn build_policy_bytes(raw: &[u8], source_path: &Path, artifacts: &Path, arch: Arch, manifest: Option<&Path>)
    -> Checked<(Generated, SignedPolicy)> {
    let source: PolicySource = serde_json::from_slice(raw).map_err(|e| format!("policy source {}: {e}", source_path.display()))?;
    let base = source_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let generated = generate(source, &base, artifacts, arch)?;
    let manifest = load_manifest(artifacts, manifest, arch, false)?;
    let policy = check_policy(&generated.bytes, &manifest, arch, false, false)?;
    Ok((generated, policy))
}

fn write_policy(out: &Path, bytes: &[u8]) -> Checked<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o444).open(out)
        .map_err(|e| format!("--out {}: {e} (it must not exist yet)", out.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o444)))
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(out);
        return Err(format!("--out {}: {error}", out.display()));
    }
    Ok(())
}

/// The command line (without the program name): `Ok` is the text for stdout,
/// `Err((exit code, message))` with 2 for usage errors and 1 for refusals.
pub fn run(args: &[String]) -> std::result::Result<String, (i32, String)> {
    let usage = |message: String| (2, format!("{message}\n{USAGE}"));
    let (command, rest) = args.split_first().ok_or_else(|| usage("missing command".into()))?;
    let refused = |message: String| (1, format!("refused: {message}"));
    match command.as_str() {
        "build" => {
            let values = options(rest, &["--source", "--artifacts-dir", "--arch", "--out", "--manifest"]).map_err(usage)?;
            let get = |key: &str| values.get(key).ok_or_else(|| usage(format!("build needs {key}")));
            let (source, artifacts, arch, out) = (get("--source")?, get("--artifacts-dir")?, get("--arch")?, get("--out")?);
            let arch = parse_arch(arch).map_err(usage)?;
            let out = Path::new(out);
            if fs::symlink_metadata(out).is_ok() { return Err(refused(format!("--out {} already exists", out.display()))); }
            let (generated, policy) = build_policy(Path::new(source), Path::new(artifacts), arch,
                values.get("--manifest").map(Path::new)).map_err(refused)?;
            write_policy(out, &generated.bytes).map_err(refused)?;
            let mut text = format!("wrote {} (mode 0444) after every production check passed\n", out.display());
            for (label, id) in &generated.runtimes { text += &format!("runtime label {label} -> profile {}\n", hex(&id.0.0)); }
            for (label, id) in &generated.egress { text += &format!("egress label {label} -> profile {}\n", hex(&id.0.0)); }
            Ok(text + &summary(&policy, &generated.bytes, arch))
        }
        "verify" => {
            let values = options(rest, &["--policy", "--artifacts-dir", "--manifest", "--arch", "--evidence-dir", "--harness-sha256"])
                .map_err(usage)?;
            let get = |key: &str| values.get(key).ok_or_else(|| usage(format!("verify needs {key}")));
            let (policy_path, artifacts) = (get("--policy")?, get("--artifacts-dir")?);
            let manifest = values.get("--manifest").map(Path::new);
            // R8r-4 review F1: evidence is only ever checked against a release
            // manifest (its trampoline, runtime and bridge entries).
            if values.contains_key("--evidence-dir") && manifest.is_none() {
                return Err((1, accept::EVIDENCE_NEEDS_MANIFEST.to_owned()));
            }
            let harness_pin = match values.get("--harness-sha256") {
                Some(_) if !values.contains_key("--evidence-dir") => return Err(usage("--harness-sha256 needs --evidence-dir".into())),
                Some(text) => Some(accept::parse_digest_hex(text)
                    .ok_or_else(|| usage("--harness-sha256 must be 64 lowercase hex characters".into()))?),
                None => None,
            };
            let arch = match values.get("--arch") {
                Some(value) => parse_arch(value).map_err(usage)?,
                None => match manifest {
                    Some(path) => {
                        let value: serde_json::Value = serde_json::from_slice(&read_limited(path, MAX_MANIFEST_BYTES, "--manifest")
                            .map_err(refused)?).map_err(|e| refused(format!("--manifest: {e}")))?;
                        parse_arch(value.get("arch").and_then(serde_json::Value::as_str).unwrap_or("")).map_err(refused)?
                    }
                    None => Arch::native().map_err(|_| usage("unsupported host architecture: pass --arch".into()))?,
                },
            };
            let bytes = read_limited(Path::new(policy_path), MAX_POLICY_BYTES, "--policy").map_err(refused)?;
            let manifest_document = load_manifest(Path::new(artifacts), manifest, arch, true).map_err(refused)?;
            let policy = check_policy(&bytes, &manifest_document, arch, true, manifest.is_some()).map_err(refused)?;
            // R8r-4: with --evidence-dir, every acceptance record must be backed by
            // the harness evidence it names. Without the flag nothing changes.
            let evidence = match values.get("--evidence-dir") {
                Some(directory) => accept::verify_evidence(&policy, &manifest_document, arch, Path::new(directory), harness_pin)
                    .map_err(|message| (1, message))?,
                None => String::new(),
            };
            Ok(format!("OK: every production check passed\n{}{evidence}", summary(&policy, &bytes, arch)))
        }
        "accept" => accept::command(rest, &accept::HostFacts::current()),
        "-h" | "--help" | "help" => Ok(format!("{USAGE}\n")),
        other => Err(usage(format!("unknown command {other:?}"))),
    }
}

/// Entry point of the `duduclaw-isolation-policy` binary; returns the exit code.
pub fn main(args: &[OsString]) -> i32 {
    let Some(args) = args.iter().map(|arg| arg.to_str().map(str::to_owned)).collect::<Option<Vec<_>>>() else {
        eprintln!("duduclaw-isolation-policy: arguments must be UTF-8\n{USAGE}");
        return 2;
    };
    match run(&args) {
        Ok(text) => { print!("{text}"); 0 }
        Err((code, message)) => { eprintln!("duduclaw-isolation-policy: {message}"); code }
    }
}

/// R8r-4: the MCP acceptance harness (`accept`) and `verify --evidence-dir`.
#[path = "installed_policy_tool_accept.rs"]
mod accept;

#[cfg(test)]
#[path = "installed_policy_tool_tests.rs"]
mod tests;
