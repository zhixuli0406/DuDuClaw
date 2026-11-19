//! R8z: the protected OpenAI-compatible (API worker) path builds its plan with
//! `requested_max_turns: None` (`duduclaw-spawnd` `gateway_plan::other_runtime_plan`)
//! and the API request itself carries no step limit; the product default
//! (`[runtime] default_max_turns`) applies only to Claude. This pins, with
//! spawnd's own rule functions (not a copy), that `None` is the only value
//! spawnd accepts at StartRun for the published `open_ai_compat` record: a
//! signed range exists only on Claude MCP records, and without a range any
//! `Some(_)` is refused (`runtime_denied`).
use duduclaw_isolation::{bounded::Text, egress::ProfileId, execution::*, ids::{EmployeeId, Sha256Digest}, ipc::QueueClass};
use duduclaw_spawnd::policy::{effective_max_turns, validate_turn_limit, EmployeeEntitlement, Executable,
    PublishedRuntime, RootPolicy, WORKING_DEFAULT_TURN_LIMIT};

/// The R8z host's published record (io limits are not in the evidence; the
/// spawnd test fixture's values are used).
fn api_record() -> PublishedRuntime {
    PublishedRuntime { runtime: RuntimeKind::OpenAiCompat, profile: ProfileId(Sha256Digest([2; 32])),
        executable: Executable::Elf { entry: Text::new("runtime-api-worker-v1".into()).unwrap(), digest: Sha256Digest([3; 32]) },
        pinned_version: Text::new("test-only".into()).unwrap(),
        fixed_args: duduclaw_mcp_boundary::config::api_worker_arguments().into_iter()
            .map(|arg| Text::new(arg).unwrap()).collect(),
        resources: ResourceLimits { memory_bytes: 1_073_741_824, cpu_quota_percent: 100, tasks: 256, open_files: 4096,
            ptys: 1, disk: DiskQuota::LoopFilesystem { bytes: 1_073_741_824, inodes: 65_536 },
            io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10, io_write_ops_per_sec: 10,
            output_buffer_bytes: 65_536, output_total_bytes: 16_777_216, memfd_total_bytes: 8_388_608 },
        timeouts: RunTimeouts { queue_ms: 60_000, startup_ms: 30_000, runtime_ms: 600_000, pty_idle_ms: None,
            termination_grace_ms: 10_000 },
        network: NetworkPolicy::Egress(ProfileId(Sha256Digest([4; 32]))),
        mcp: true, pty_allowed: false, queue_class: QueueClass::Background, turn_limit: None }
}

fn root(record: &PublishedRuntime, employee: EmployeeId, cap: Option<u16>) -> RootPolicy {
    RootPolicy::from_verified_publication(vec![record.clone()], vec![EmployeeEntitlement {
        employee, profiles: vec![record.profile], max_turns: cap }]).unwrap()
}

#[test]
fn api_worker_record_without_turn_limit_accepts_only_an_absent_step_limit() {
    let employee = EmployeeId([1; 16]);
    let record = api_record();
    assert!(validate_turn_limit(&record).is_ok());
    // A signed range on a non-Claude record invalidates the whole
    // publication, so an API worker record never has one.
    let mut ranged = record.clone();
    ranged.turn_limit = Some(WORKING_DEFAULT_TURN_LIMIT);
    assert!(validate_turn_limit(&ranged).is_err());
    for cap in [None, Some(30)] {
        let policy = root(&record, employee, cap);
        let (runtime, _) = policy.derive(employee, &RuntimeKind::OpenAiCompat, false).unwrap();
        assert!(runtime.turn_limit.is_none());
        // spawnd's StartRun rule: absent ⇒ accepted, the signed args apply.
        assert_eq!(policy.max_turns(employee, runtime, None), Ok(None));
        // Any value — the product default 50 included — is refused, never clamped.
        for value in [1u16, 50, 200, 1000] {
            assert!(policy.max_turns(employee, runtime, Some(value)).is_err(), "{value}");
        }
        // The plan copies these limits; StartRun needs them non-zero.
        assert!(runtime.resources.output_total_bytes > 0 && runtime.resources.output_buffer_bytes > 0
            && runtime.resources.memfd_total_bytes > 0);
    }
    assert_eq!(effective_max_turns(None, Some(30), None), Ok(None));
    assert!(effective_max_turns(None, None, Some(50)).is_err());
}
