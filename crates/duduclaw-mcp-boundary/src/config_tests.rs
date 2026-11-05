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
