//! Secret-free native runtime projections and the A/B publication seam.
use std::path::{Component, Path, PathBuf};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind}, ids::{EmployeeId, Sha256Digest}};
use sha2::{Digest, Sha256};
use crate::VerificationError;

pub const MCP_FD: i32 = 3;

/// The root registry verifies this executable against the release manifest.
/// This helper checks syntax only; `prepare_runtime_bridge` also requires a
/// matching publication and runtime inheritance receipt from the registry.
pub fn bridge_server_config(registered_executable: &Path) -> Result<serde_json::Value, VerificationError> {
    let command = registered_executable.to_str().ok_or(VerificationError::Invalid)?;
    if !registered_executable.is_absolute() || command.len() > 4096
        || command.chars().any(char::is_control)
        || registered_executable.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || registered_executable.file_name().and_then(|name| name.to_str()) != Some("duduclaw-mcp-bridge") {
        return Err(VerificationError::Invalid);
    }
    Ok(serde_json::json!({"command": command, "args": []}))
}

/// Host-derived, exact manifest identity. Never deserialize this from MCP data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRegistration {
    pub runtime: RuntimeKind,
    pub version: String,
    pub runtime_digest: Sha256Digest,
    pub bridge_digest: Sha256Digest,
    pub bridge_executable: PathBuf,
}

/// The relative target is fixed by D; B publishes each file under a root-owned
/// projection and mounts it read-only at the runtime's corresponding config
/// location. No merge with employee-controlled configuration is permitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionFile {
    pub destination: ConfigDestination,
    pub relative_target: &'static str,
    pub contents: String,
}

/// Resolve these locations from B's root registry, never from tool arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigDestination { RunConfig, RuntimeHome, WorkingDirectory }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeProjection {
    pub registration: RuntimeRegistration,
    pub files: Vec<ProjectionFile>,
    /// Fixed runtime arguments, checked by A and independently required in B's
    /// signed runtime registry; StartRun carries no argv. These never contain
    /// tokens, identity, host directories or caller-provided args.
    pub arguments: Vec<String>,
    pub inherited_mcp_fd: i32,
}
impl RuntimeProjection {
    /// Digest covers exact bytes, targets, argv, versions and both binaries.
    /// Local seam encoding only; this does not change the isolation wire format.
    pub fn digest(&self) -> Sha256Digest {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "domain": "duduclaw-runtime-mcp-projection-v1",
            "runtime": self.registration.runtime,
            "version": self.registration.version,
            "runtime_digest": self.registration.runtime_digest,
            "bridge_digest": self.registration.bridge_digest,
            "bridge_executable": self.registration.bridge_executable,
            "files": self.files.iter().map(|file| serde_json::json!({
                "destination": format!("{:?}", file.destination),
                "target": file.relative_target, "contents": file.contents,
            })).collect::<Vec<_>>(),
            "arguments": self.arguments, "mcp_fd": self.inherited_mcp_fd,
        })).expect("projection has only JSON-compatible fields");
        Sha256Digest(Sha256::digest(bytes).into())
    }
}

/// Acceptance must exercise the exact runtime's real MCP-child spawn, including
/// its sandbox/profile. A runtime version is not accepted merely because the
/// bridge works when directly launched. Root owns these records; no defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FdInheritanceReceipt {
    pub registration: RuntimeRegistration,
    pub projection_digest: Sha256Digest,
    pub inherited_fd: i32,
    pub initializes_mcp: bool,
    pub survives_parent_exec: bool,
    pub survives_mcp_child_exec: bool,
    pub config_is_exclusive: bool,
}

/// Publication readback must come from B's authenticated root-owned registry.
/// It binds the readonly projection and descriptor plan to this particular run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionReceipt {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub projection_digest: Sha256Digest,
    pub recursive_readonly: bool,
    pub root_owned_ancestors: bool,
    pub inheritance: FdInheritanceReceipt,
}

/// A/B integration seam. Implementations verify manifests, publish atomically,
/// read back mounts and fd plan, and load exact-version native CLI acceptance.
/// An unavailable, unknown or untested runtime must return Unavailable. No
/// registry supplied by default and no gateway-uid or legacy MCP fallback.
pub trait RuntimeBridgeRegistry: Send + Sync {
    fn registration(&self, runtime: &RuntimeKind) -> Result<RuntimeRegistration, VerificationError>;
    fn publish_for_run(&self, run: RunHandle, employee: EmployeeId,
        projection: &RuntimeProjection) -> Result<ProjectionReceipt, VerificationError>;
}

/// Prepared value has no public constructor: only a fully matched post-start
/// root readback permits D to activate the run's actual MCP endpoint.
pub struct PreparedRuntimeBridge { projection: RuntimeProjection, receipt: ProjectionReceipt }
impl PreparedRuntimeBridge {
    pub fn projection(&self) -> &RuntimeProjection { &self.projection }
    pub fn receipt(&self) -> &ProjectionReceipt { &self.receipt }
}

pub fn prepare_runtime_bridge(registry: &dyn RuntimeBridgeRegistry, runtime: &RuntimeKind,
    run: RunHandle, employee: EmployeeId) -> Result<PreparedRuntimeBridge, VerificationError> {
    if !cfg!(target_os = "linux") { return Err(VerificationError::Unavailable); }
    prepare(registry, runtime, run, employee)
}

fn prepare(registry: &dyn RuntimeBridgeRegistry, runtime: &RuntimeKind,
    run: RunHandle, employee: EmployeeId) -> Result<PreparedRuntimeBridge, VerificationError> {
    let registration = registry.registration(runtime)?;
    if &registration.runtime != runtime { return Err(VerificationError::BindingMismatch); }
    let projection = runtime_projection(registration)?;
    let receipt = registry.publish_for_run(run, employee, &projection)?;
    let inheritance = &receipt.inheritance;
    if receipt.run != run || receipt.employee != employee
        || receipt.projection_digest != projection.digest()
        || !receipt.recursive_readonly || !receipt.root_owned_ancestors
        || inheritance.registration != projection.registration
        || inheritance.projection_digest != projection.digest()
        || inheritance.inherited_fd != MCP_FD || !inheritance.initializes_mcp
        || !inheritance.survives_parent_exec || !inheritance.survives_mcp_child_exec
        || !inheritance.config_is_exclusive {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(PreparedRuntimeBridge { projection, receipt })
}

/// Builds complete fresh files. B maps relative targets under the immutable
/// config projection and A selects that projection as the runtime config root.
/// In particular Grok must have no writable/global config overlay, and Codex
/// must not load a project config on top. The receipt proves those conditions.
pub fn runtime_projection(registration: RuntimeRegistration) -> Result<RuntimeProjection, VerificationError> {
    if registration.version.is_empty() || registration.version.len() > 128
        || registration.version.chars().any(char::is_control)
        || registration.runtime_digest.0 == [0; 32] || registration.bridge_digest.0 == [0; 32] {
        return Err(VerificationError::Invalid);
    }
    let server = bridge_server_config(&registration.bridge_executable)?;
    let command = serde_json::to_string(server["command"].as_str().ok_or(VerificationError::Invalid)?)
        .map_err(|_| VerificationError::Invalid)?;
    let (files, arguments) = match registration.runtime {
        RuntimeKind::Claude => (vec![ProjectionFile {
            destination: ConfigDestination::RunConfig, relative_target: "mcp.json",
            contents: serde_json::json!({"mcpServers": {"duduclaw": server}}).to_string(),
        }], vec!["--strict-mcp-config".into(), "--mcp-config".into(),
            "/config/mcp.json".into()]),
        RuntimeKind::Codex => (vec![ProjectionFile {
            destination: ConfigDestination::RuntimeHome, relative_target: ".codex/config.toml",
            contents: format!("[mcp_servers.duduclaw]\ncommand = {command}\nargs = []\nenv_vars = []\ndefault_tools_approval_mode = \"auto\"\n"),
        }], vec!["--dangerously-bypass-approvals-and-sandbox".into()]),
        RuntimeKind::Grok => {
            let contents = format!("[mcp_servers.duduclaw]\ncommand = {command}\nargs = []\nenabled = true\nenv = {{}}\n");
            (vec![ProjectionFile {
                destination: ConfigDestination::RuntimeHome, relative_target: ".grok/config.toml", contents: contents.clone(),
            }, ProjectionFile {
                destination: ConfigDestination::WorkingDirectory, relative_target: ".grok/config.toml", contents,
            }], Vec::new())
        },
        // A's API tool loop starts the bridge itself with the explicit fd handoff.
        RuntimeKind::OpenAiCompat => (vec![ProjectionFile {
            destination: ConfigDestination::RunConfig, relative_target: "mcp.json",
            contents: serde_json::json!({"mcpServers": {"duduclaw": server}}).to_string(),
        }], Vec::new()),
        RuntimeKind::Registered(_) => return Err(VerificationError::Unavailable),
    };
    Ok(RuntimeProjection { registration, files, arguments, inherited_mcp_fd: MCP_FD })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(target_os = "linux")]
#[path = "config_fd.rs"]
pub mod fd;
