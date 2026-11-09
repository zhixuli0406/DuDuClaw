use super::*;
use crate::tool_policy::{authorize, eligible, policy, ResourceKind};
use duduclaw_isolation::{bounded::List, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    Principal, PublicOrigin}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn scope() -> ResourceGrant { ResourceGrant::EmployeeScope { id: EmployeeId([2; 16]) } }
fn employee() -> ResourceGrant { ResourceGrant::Employee { id: EmployeeId([2; 16]) } }
fn other_employee() -> ResourceGrant { ResourceGrant::Employee { id: EmployeeId([8; 16]) } }
fn work() -> ResourceGrant { ResourceGrant::Workspace { handle: text("employee-work") } }
fn web() -> ResourceGrant { ResourceGrant::PublicOrigins { origins: List::new(vec![PublicOrigin::any()]).unwrap() } }
fn context(tool: &str, resources: Vec<ResourceGrant>) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: text("owner"), generation: 1 },
        employee_binding: EmployeeBinding::Personal(text("owner")), lane: Lane::Standard,
        tools: List::new(vec![text(tool)]).unwrap(), resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [0; 32], jti: [0; 16],
    })
}
struct Fixed(Vec<ResourceGrant>);
impl ResourceResolver for Fixed {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}
struct NoJobs;
impl RecordingJobs for NoJobs {
    fn recorded(&self, _: &RequestContext, _: &str) -> Result<RecordingManifest, VerificationError> {
        Err(VerificationError::Invalid)
    }
}

#[test]
fn h12b_hub_install_routed_with_own_employee_and_public_origins() {
    assert_eq!(policy("skill_hub_install").unwrap().required, &[ResourceKind::Employee]);
    let ctx = context("skill_hub_install", vec![scope(), employee(), web()]);
    ctx.record().canonical_bytes().unwrap();
    assert!(eligible(&ctx, "skill_hub_install"));
    let args = json!({"hub":"clawhub","skill_name":"notes"});
    let call = authorize(&ctx, "skill_hub_install", &args, &Fixed(vec![employee()])).unwrap();
    // PublicOrigins comes from the signed record, not the resolver.
    assert_eq!(call.resources(), &[employee(), web()]);
    // Install target is always the caller's own SKILLS: no scope/owner/identity.
    for extra in [json!({"scope":"global"}), json!({"scope":"other-agent"}), json!({"owner":"someone"}),
        json!({"agent_id":"main"}), json!({"target_agent":"other"}), json!({"url":"https://evil.example/x.zip"}),
        json!({"workspace":"employee-work"})] {
        let mut forged = args.clone();
        forged.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(authorize(&ctx, "skill_hub_install", &forged, &Fixed(vec![employee()])).is_err(), "{forged}");
    }
    for bad in [json!({"hub":"clawhub"}), json!({"hub":"../x","skill_name":"notes"}),
        json!({"hub":"clawhub","skill_name":"a/b"}), json!({"hub":"clawhub","skill_name":"x".repeat(65)})] {
        assert!(authorize(&ctx, "skill_hub_install", &bad, &Fixed(vec![employee()])).is_err(), "{bad}");
    }
}

#[test]
fn h12b_hub_install_refused_without_grant_or_for_another_employee() {
    let args = json!({"hub":"clawhub","skill_name":"notes"});
    // Without the admin-enabled protected public web grant: not listed, not callable.
    let ctx = context("skill_hub_install", vec![scope(), employee()]);
    assert!(!eligible(&ctx, "skill_hub_install"));
    assert!(authorize(&ctx, "skill_hub_install", &args, &Fixed(vec![employee()])).is_err());
    // Another employee's grant is never "own".
    let ctx = context("skill_hub_install", vec![scope(), other_employee(), web()]);
    assert!(!eligible(&ctx, "skill_hub_install"));
    // The resolver cannot substitute another employee or add a workspace.
    let ctx = context("skill_hub_install", vec![scope(), employee(), web(), work()]);
    for resolved in [vec![other_employee()], vec![employee(), work()], vec![], vec![employee(), web()]] {
        assert!(authorize(&ctx, "skill_hub_install", &args, &Fixed(resolved.clone())).is_err(), "{resolved:?}");
    }
    // A task-limited record (no EmployeeScope) never lists it.
    assert!(!eligible(&context("skill_hub_install", vec![employee(), work()]), "skill_hub_install"));
}

#[test]
fn h12c_synthesis_routed_with_own_employee_only() {
    assert_eq!(policy("skill_synthesis_run").unwrap().required, &[ResourceKind::Employee]);
    let ctx = context("skill_synthesis_run", vec![scope(), employee()]);
    assert!(eligible(&ctx, "skill_synthesis_run"));
    for args in [json!({}), json!({"dry_run":false,"lookback_days":7}), json!({"agent_id":"main","dry_run":true})] {
        let call = authorize(&ctx, "skill_synthesis_run", &args, &Fixed(vec![employee()])).unwrap();
        assert_eq!(call.resources(), &[employee()], "{args}");
    }
    for bad in [json!({"dry_run":"false"}), json!({"lookback_days":0}), json!({"lookback_days":31}),
        json!({"agent_id":"../x"}), json!({"target_agent":"other"}), json!({"scope":"global"}), json!({"run_id":"x"})] {
        assert!(authorize(&ctx, "skill_synthesis_run", &bad, &Fixed(vec![employee()])).is_err(), "{bad}");
    }
    // No Workspace grant is part of a synthesis call, even when the record has one.
    let with_work = context("skill_synthesis_run", vec![scope(), employee(), work()]);
    assert!(authorize(&with_work, "skill_synthesis_run", &json!({}), &Fixed(vec![employee(), work()])).is_err());
    assert!(!eligible(&context("skill_synthesis_run", vec![scope(), other_employee()]), "skill_synthesis_run"));
    assert!(!eligible(&context("skill_synthesis_run", vec![employee()]), "skill_synthesis_run"));
}

#[test]
fn h12d_recording_refused_until_job_source_installed() {
    let ctx = context("skill_from_recording", vec![scope(), employee(), work()]);
    let args = json!({"id":"ab".repeat(16)});
    let _none = TestRecordingJobs::install(None);
    assert!(recording_jobs().is_none());
    assert_eq!(own_skill_job_selector(&ctx, "skill_from_recording"), Err(VerificationError::Unavailable));
    assert!(!eligible(&ctx, "skill_from_recording"));
    assert!(authorize(&ctx, "skill_from_recording", &args, &Fixed(vec![employee(), work()])).is_err());
}

#[test]
fn h12d_recording_routed_with_brokered_workspace_and_job_source() {
    let _jobs = TestRecordingJobs::install(Some(Arc::new(NoJobs)));
    let ctx = context("skill_from_recording", vec![scope(), employee(), work()]);
    assert!(eligible(&ctx, "skill_from_recording"));
    for args in [json!({"id":"ab".repeat(16)}), json!({"id":"ab".repeat(16),"name":"填寫表單"})] {
        let call = authorize(&ctx, "skill_from_recording", &args, &Fixed(vec![employee(), work()])).unwrap();
        assert_eq!(call.resources(), &[employee(), work()]);
    }
    for bad in [json!({"id":"AB".repeat(16)}), json!({"id":"../x"}), json!({"id":"ab".repeat(16),"path":"recordings/x"}),
        json!({"id":"ab".repeat(16),"workspace":"other"}), json!({"id":"ab".repeat(16),"name":"a\nb"}),
        json!({"id":"ab".repeat(16),"agent_id":"other"})] {
        assert!(authorize(&ctx, "skill_from_recording", &bad, &Fixed(vec![employee(), work()])).is_err(), "{bad}");
    }
    let args = json!({"id":"ab".repeat(16)});
    for resolved in [vec![employee()], vec![other_employee(), work()], vec![employee(), work(),
        ResourceGrant::Workspace { handle: text("other") }]] {
        assert!(authorize(&ctx, "skill_from_recording", &args, &Fixed(resolved.clone())).is_err(), "{resolved:?}");
    }
    // Without the brokered workspace, with a confined or a different handle,
    // or for another employee: not eligible.
    for resources in [vec![scope(), employee()], vec![scope(), employee(), ResourceGrant::Confined { resource: Box::new(work()) }],
        vec![scope(), employee(), ResourceGrant::Workspace { handle: text("work") }], vec![scope(), other_employee(), work()],
        vec![employee(), work()]] {
        assert!(!eligible(&context("skill_from_recording", resources.clone()), "skill_from_recording"), "{resources:?}");
    }
}

#[test]
fn h12d_recording_manifest_binds_exact_artifacts() {
    let id = "cd".repeat(16);
    let artifacts = |names: [&str; 3]| names.map(|name| json!({"name":name,"bytes":5,"sha256":"0a".repeat(32)}));
    let good = json!({"id":id,"status":"recorded","reason":"stop","actions":1,"navigation":"completed",
        "artifacts":artifacts(["trace.zip","actions.json","network.json"])});
    let manifest = RecordingManifest::from_result(&id, &good).unwrap();
    assert_eq!(manifest.artifact("actions.json").unwrap().bytes, 5);
    assert_eq!(manifest.artifact("actions.json").unwrap().sha256, [0x0a; 32]);
    assert!(manifest.artifact("meta.json").is_none());
    let mut cases = vec![];
    let mut other = good.clone(); other["id"] = json!("ef".repeat(16)); cases.push(other);
    let mut early = good.clone(); early["status"] = json!("not_recorded"); cases.push(early);
    let mut order = good.clone(); order["artifacts"] = json!(artifacts(["actions.json","trace.zip","network.json"])); cases.push(order);
    let mut empty = good.clone(); empty["artifacts"][1]["bytes"] = json!(0); cases.push(empty);
    let mut large = good.clone(); large["artifacts"][1]["bytes"] = json!(recording_layout::MAX_ARTIFACT_BYTES + 1); cases.push(large);
    let mut upper = good.clone(); upper["artifacts"][1]["sha256"] = json!("0A".repeat(32)); cases.push(upper);
    let mut extra = good.clone(); extra["artifacts"][1]["path"] = json!("/etc/passwd"); cases.push(extra);
    let mut short = good.clone(); short["artifacts"] = json!([]); cases.push(short);
    for case in cases { assert!(RecordingManifest::from_result(&id, &case).is_err(), "{case}"); }
    assert!(RecordingManifest::from_result("../x", &good).is_err());
}
