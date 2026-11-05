use super::*;
use duduclaw_isolation::{bounded::Text, ids::RunId};

fn registration(runtime: RuntimeKind) -> RuntimeRegistration {
    #[cfg(not(windows))]
    let path = PathBuf::from("/opt/duduclaw/bin/duduclaw-mcp-bridge");
    #[cfg(windows)]
    let path = PathBuf::from(r"C:\duduclaw\duduclaw-mcp-bridge");
    RuntimeRegistration { runtime, version: "1.2.3".into(), runtime_digest: Sha256Digest([1; 32]),
        bridge_digest: Sha256Digest([2; 32]), bridge_executable: path }
}

#[test]
fn bridge_projection_has_no_bearer_env_or_legacy_server_command() {
    let registration = registration(RuntimeKind::Claude);
    let config = bridge_server_config(&registration.bridge_executable).unwrap();
    assert_eq!(config.as_object().unwrap().len(), 2);
    assert_eq!(config["args"], serde_json::json!([]));
    for command in ["duduclaw-mcp-bridge", "duduclaw", "/tmp/duduclaw", "/tmp/../duduclaw-mcp-bridge"] {
        assert!(bridge_server_config(Path::new(command)).is_err());
    }
}

#[test]
fn claude_selects_the_config_path_visible_inside_the_run() {
    let projection = runtime_projection(registration(RuntimeKind::Claude)).unwrap();
    // B2 installs the detached projection at /config after pivot_root. A host
    // /run path would pass schema validation but fail in the actual employee.
    assert_eq!(projection.arguments, ["--strict-mcp-config", "--mcp-config", "/config/mcp.json"]);
    assert_eq!(projection.files.len(), 1);
    assert_eq!(projection.files[0].destination, ConfigDestination::RunConfig);
    assert_eq!(projection.files[0].relative_target, "mcp.json");
}

#[test]
fn registered_cli_projection_requires_versioned_adapter_and_exact_fixed_protocol() {
    let runtime = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
    let projection = runtime_projection(registration(runtime.clone())).unwrap();
    assert_eq!(registered_cli_name(&runtime), Some("qwen"));
    assert_eq!(projection.arguments, ["--duduclaw-protocol", "generic-cli-v1", "--runtime", "qwen",
        "--mcp-config", "/config/mcp.json"]);
    assert_eq!(projection.files.len(), 1);
    assert_eq!(projection.files[0].destination, ConfigDestination::RunConfig);
    assert_eq!(projection.files[0].relative_target, "mcp.json");
    for name in ["qwen", "generic-cli-v2.qwen", "generic-cli-v1.", "generic-cli-v1../qwen",
        "generic-cli-v1.Qwen", "generic-cli-v1.qwen\n", "xgeneric-cli-v1.qwen"] {
        let runtime = RuntimeKind::Registered(Text::new(name.into()).unwrap());
        assert!(registered_cli_name(&runtime).is_none(), "{name}");
        assert!(runtime_projection(registration(runtime)).is_err(), "{name}");
    }
}

#[test]
fn api_worker_projection_binds_its_exact_protocol_and_native_acceptance() {
    let projection = runtime_projection(registration(RuntimeKind::OpenAiCompat)).unwrap();
    assert_eq!(projection.arguments, ["--duduclaw-protocol", "openai-compat-v1", "--mcp-config", "/config/mcp.json"]);
    assert_eq!(projection.files.len(), 1);
    let run = RunHandle(RunId([3; 16]));
    let employee = EmployeeId([4; 16]);
    assert!(prepare(&Registry { fault: 0 }, &RuntimeKind::OpenAiCompat, run, employee).is_ok());
    for fault in 1..=13 {
        assert!(prepare(&Registry { fault }, &RuntimeKind::OpenAiCompat, run, employee).is_err(), "fault {fault}");
    }
}

#[test]
fn registered_cli_preparation_retains_every_native_acceptance_gate() {
    let kind = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
    let run = RunHandle(RunId([3; 16]));
    let employee = EmployeeId([4; 16]);
    assert!(prepare(&Registry { fault: 0 }, &kind, run, employee).is_ok());
    for fault in 1..=13 {
        assert!(prepare(&Registry { fault }, &kind, run, employee).is_err(), "fault {fault}");
    }
}

#[test]
fn every_native_projection_is_fresh_secret_free_and_bound_to_binary_bytes() {
    for runtime in [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Grok, RuntimeKind::OpenAiCompat] {
        let projection = runtime_projection(registration(runtime)).unwrap();
        assert_eq!(projection.inherited_mcp_fd, 3);
        assert!(!projection.files.is_empty());
        for file in &projection.files {
            assert!(!file.contents.contains("DUDUCLAW_"));
            assert!(!file.contents.contains("mcp-server"));
            assert!(file.contents.contains("duduclaw-mcp-bridge"));
        }
        let mut changed = projection.clone();
        changed.registration.runtime_digest = Sha256Digest([9; 32]);
        assert_ne!(projection.digest(), changed.digest());
        changed = projection.clone();
        changed.files[0].contents.push(' ');
        assert_ne!(projection.digest(), changed.digest());
        changed = projection.clone();
        changed.arguments.push("--unsafe-overlay".into());
        assert_ne!(projection.digest(), changed.digest());
    }
    assert!(runtime_projection(registration(RuntimeKind::Registered(Text::new("unknown".into()).unwrap()))).is_err());
    let mut missing = registration(RuntimeKind::Claude);
    missing.runtime_digest = Sha256Digest([0; 32]);
    assert!(runtime_projection(missing).is_err());
}

struct Registry { fault: u8 }
impl RuntimeBridgeRegistry for Registry {
    fn registration(&self, runtime: &RuntimeKind) -> Result<RuntimeRegistration, VerificationError> {
        if self.fault == 1 { return Err(VerificationError::Unavailable); }
        Ok(registration(runtime.clone()))
    }
    fn publish_for_run(&self, run: RunHandle, employee: EmployeeId, projection: &RuntimeProjection)
        -> Result<ProjectionReceipt, VerificationError> {
        let mut receipt = ProjectionReceipt {
            run, employee, projection_digest: projection.digest(), recursive_readonly: true,
            root_owned_ancestors: true, inheritance: FdInheritanceReceipt {
                registration: projection.registration.clone(), projection_digest: projection.digest(),
                inherited_fd: 3, initializes_mcp: true, survives_parent_exec: true,
                survives_mcp_child_exec: true, config_is_exclusive: true,
            },
        };
        match self.fault {
            2 => receipt.run = RunHandle(RunId([8; 16])),
            3 => receipt.employee = EmployeeId([8; 16]),
            4 => receipt.projection_digest = Sha256Digest([8; 32]),
            5 => receipt.recursive_readonly = false,
            6 => receipt.root_owned_ancestors = false,
            7 => receipt.inheritance.registration.runtime_digest = Sha256Digest([8; 32]),
            8 => receipt.inheritance.inherited_fd = 4,
            9 => receipt.inheritance.initializes_mcp = false,
            10 => receipt.inheritance.survives_parent_exec = false,
            11 => receipt.inheritance.survives_mcp_child_exec = false,
            12 => receipt.inheritance.config_is_exclusive = false,
            13 => receipt.inheritance.projection_digest = Sha256Digest([8; 32]),
            _ => {},
        }
        Ok(receipt)
    }
}

#[test]
fn runtime_preparation_refuses_every_missing_or_mismatched_readback() {
    let run = RunHandle(RunId([3; 16]));
    let employee = EmployeeId([4; 16]);
    let prepared = prepare(&Registry { fault: 0 }, &RuntimeKind::Claude, run, employee).unwrap();
    assert_eq!(prepared.receipt().run, run);
    for fault in 1..=13 {
        assert!(prepare(&Registry { fault }, &RuntimeKind::Claude, run, employee).is_err(), "fault {fault}");
    }
    #[cfg(not(target_os = "linux"))]
    assert!(prepare_runtime_bridge(&Registry { fault: 0 }, &RuntimeKind::Claude, run, employee).is_err());
}

#[test]
fn native_cli_projections_have_closed_names_and_exact_adapter_protocols() {
    for (kind, protocol, names) in [
        (RuntimeKind::Codex, "codex-v1", vec!["codex-home.toml"]),
        (RuntimeKind::Grok, "grok-v1", vec!["grok-home.toml", "grok-project.toml", "project-mcp.json"]),
    ] {
        let projection = runtime_projection(registration(kind)).unwrap();
        assert_eq!(projection.arguments, ["--duduclaw-protocol",protocol]);
        assert_eq!(projection.files.iter().map(|f| projection_name(f).unwrap()).collect::<Vec<_>>(), names);
        let mut bad = projection.files[0].clone();
        bad.relative_target = "../config.toml";
        assert!(projection_name(&bad).is_err());
        bad.relative_target = "mcp.json";
        assert!(projection_name(&bad).is_err());
    }
}
