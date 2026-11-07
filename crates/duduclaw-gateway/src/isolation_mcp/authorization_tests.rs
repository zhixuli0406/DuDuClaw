use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    Lane, ResourceGrant}, ids::{RunId, Sha256Digest}};

fn snapshot() -> duduclaw_auth::IsolationAuthoritySnapshot {
    duduclaw_auth::IsolationAuthoritySnapshot { tenant_id: "tenant".into(), acl_generation: 7,
        policy_epoch: 9, human: Some(duduclaw_auth::UserContext {
            user_id: "owner".into(), credential_epoch: 3, email: "owner@example.invalid".into(),
            role: duduclaw_auth::UserRole::Admin, agent_access: Default::default(),
            team_access: Default::default(), must_change_password: false, acl_generation: 7,
        }), personal_owner: None, agent_member_count: 1 }
}
fn issue(home: &Path, current: &duduclaw_auth::IsolationAuthoritySnapshot, expiry: Option<u64>)
    -> Result<super::super::RuntimeAuthorization, VerificationError> {
    human_authorization(home.to_owned(), "alice".into(), EmployeeId([2;16]), "owner".into(), 3,
        Lane::Standard, current, List::new(["tasks_update", "computer_workspace_write",
            "gmail_create_draft", "send_message", "skill_extract", "file_read", "web_extract"]
            .into_iter().map(|name| Text::new(name.into()).unwrap()).collect()).unwrap(),
        super::super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
            turn: "turn".into(), approval_session: None }, egress_turn: None,
            metadata: None, principal_context: None, background: None }, expiry)
}
fn record(auth: &super::super::RuntimeAuthorization) -> CapabilityRecord {
    CapabilityRecord { id: CapabilityId(Sha256Digest([1;32])), employee: EmployeeId([2;16]),
        run: RunId([3;16]), task: auth.grant.task.clone(), principal: auth.grant.principal.clone(),
        employee_binding: auth.grant.binding.clone(), lane: auth.grant.lane,
        tools: auth.grant.tools.clone(), resources: auth.grant.resources.clone(),
        policy_revision: auth.grant.policy_revision, gateway_epoch: [4;16], issued_at: auth.grant.not_before,
        not_before: auth.grant.not_before, expires_at: auth.grant.expires_at, nonce: [5;32], jti: [6;16] }
}
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice/wiki")).unwrap();
    home
}

#[test]
fn isolation_issuer_real_output_preserves_service_and_connector_scope() {
    let home = home();
    let auth = issue(home.path(), &snapshot(), None).unwrap();
    let record = record(&auth);
    record.canonical_bytes().unwrap();
    assert!(record.employee_scope());
    assert!(record.resources.as_slice().iter().all(|grant| matches!(grant,
        ResourceGrant::Employee { .. } | ResourceGrant::EmployeeScope { .. } | ResourceGrant::Confined { .. })));
    let context = duduclaw_mcp_boundary::RequestContext::from_verified_record(record);
    for tool in ["tasks_update", "computer_workspace_write", "gmail_create_draft", "send_message"] {
        assert!(duduclaw_mcp_boundary::tool_policy::eligible(&context, tool), "{tool}");
    }
    // There is no production workspace/URL selector. Do not fabricate these
    // grants to make full File/A/E inventory parity appear to pass.
    for tool in ["file_read", "web_extract", "skill_extract"] {
        assert!(!duduclaw_mcp_boundary::tool_policy::eligible(&context, tool), "{tool}");
    }
}

#[test]
fn isolation_issuer_task_background_is_finite_and_canonical() {
    let home = home();
    let mut auth = issue(home.path(), &snapshot(), None).unwrap();
    auth.limit_to_task("task-a".into()).unwrap();
    let finite = record(&auth);
    finite.canonical_bytes().unwrap();
    assert!(!finite.employee_scope());
    assert_eq!(finite.task.as_ref().unwrap().as_str(), "task-a");
    assert!(finite.resources.as_slice().iter().all(|grant| !matches!(grant,
        ResourceGrant::EmployeeScope { .. } | ResourceGrant::Confined { .. })));
    assert!(finite.resources.as_slice().contains(&ResourceGrant::Task { id: Text::new("task-a".into()).unwrap() }));
    assert!(finite.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::Wiki { .. })));
    for invalid in ["task-b", "", "bad\ntask"] {
        assert!(auth.limit_to_task(invalid.into()).is_err());
        assert_eq!(record(&auth), finite, "failed narrowing must leave the grant unchanged");
    }
    auth.limit_to_task("task-a".into()).unwrap();
    assert_eq!(record(&auth), finite, "same-task narrowing is idempotent");
}

#[test]
fn isolation_issuer_no_wiki_root_means_no_confined_wiki_grant() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    let auth = issue(home.path(), &snapshot(), None).unwrap();
    assert!(!record(&auth).io_resources().any(|grant| matches!(grant, ResourceGrant::Wiki { .. })));
    std::os::unix::fs::symlink(home.path(), home.path().join("agents/alice/wiki")).unwrap();
    assert!(issue(home.path(), &snapshot(), None).is_err());
}

#[test]
fn isolation_issuer_refuses_stale_epoch_wrong_user_and_expiry() {
    let home = home();
    let mut current = snapshot();
    current.human.as_mut().unwrap().credential_epoch += 1;
    assert!(issue(home.path(), &current, None).is_err());
    current = snapshot();
    current.human.as_mut().unwrap().user_id = "other".into();
    assert!(issue(home.path(), &current, None).is_err());
    assert!(issue(home.path(), &snapshot(), Some(1)).is_err());
}
