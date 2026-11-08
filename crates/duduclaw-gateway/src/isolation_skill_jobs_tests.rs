use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, ResourceGrant}, execution::RunHandle, ids::{EmployeeId, RunId, Sha256Digest},
    recording_layout};
use duduclaw_mcp_boundary::{execution_jobs::{handle_text, SqliteExecutionJobs}, resources::{test_broker::TestWorkBroker,
    EMPLOYEE_WORK_ROOT}, tool_policy::{authorize, ResourceResolver, TestRecordingJobs, ToolPolicy}};
use serde_json::Value;
use sha2::{Digest, Sha256};

const RUN: RunId = RunId([3; 16]);
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
const OTHER: EmployeeId = EmployeeId([8; 16]);

struct Live;
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct Fixed(Vec<ResourceGrant>);
impl ResourceResolver for Fixed {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}
fn work() -> ResourceGrant { ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() } }
fn context(employee: EmployeeId) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run: RUN, task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("skill_from_recording".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: employee }, ResourceGrant::Employee { id: employee },
            work()]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live))
}
/// Write the helper's three artifacts at the shared layout; return its result.
fn recorded(root: &std::path::Path, id: &str, actions: &[u8]) -> Value {
    let artifacts: Vec<Value> = recording_layout::ARTIFACTS.iter().map(|name| {
        let bytes: Vec<u8> = if *name == "actions.json" { actions.to_vec() } else { name.as_bytes().to_vec() };
        let path = root.join(recording_layout::artifact_path(id, name).unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        json!({"name":name,"bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes))})
    }).collect();
    json!({"id":id,"status":"recorded","reason":"stop","actions":1,"artifacts":artifacts,"navigation":"completed"})
}
struct Fixture { _home: tempfile::TempDir, work: tempfile::TempDir, jobs: Arc<SqliteExecutionJobs>,
    roots: ResourceRoots, broker: Arc<TestWorkBroker> }
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    let mut roots = ResourceRoots::default();
    roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).unwrap();
    Fixture { _home: home, work, jobs, roots, broker }
}
fn call(context: &RequestContext, id: &str) -> Result<AuthorizedToolCall, VerificationError> {
    authorize(context, "skill_from_recording", &json!({"id": id}),
        &Fixed(vec![ResourceGrant::Employee { id: context.record().employee }, work()]))
}

#[test]
fn h12d_skill_from_recording_reads_own_finished_recording() {
    let fixture = fixture();
    let _jobs = TestRecordingJobs::install(Some(Arc::new(ExecutionJobRecordings::new(fixture.jobs.clone()))));
    let owner = context(EMPLOYEE);
    let run = RunHandle(RunId([0x4d; 16]));
    let id = handle_text(run);
    let actions = br#"{"actions":[{"kind":"click","tag":"button"}]}"#;
    fixture.jobs.insert(&owner, "browser_record_start", run).unwrap();
    fixture.jobs.finish(run, recorded(fixture.work.path(), &id, actions)).unwrap();
    let call = call(&owner, &id).unwrap();
    let (manifest, input) = recording_summary_input(&call, &fixture.roots).unwrap();
    assert_eq!(manifest.id, id);
    let input: Value = serde_json::from_slice(&input).unwrap();
    // The artifact text is passed through as data; the host did not parse it.
    assert_eq!(input["actions"].as_str().unwrap().as_bytes(), actions);
    assert_eq!(input["events"], "");
    assert_eq!(fixture.broker.calls(), vec![(RUN, EMPLOYEE, format!("recordings/{id}/actions.json"))]);
}

#[test]
fn h12d_skill_from_recording_refuses_other_employee_job() {
    let fixture = fixture();
    let _jobs = TestRecordingJobs::install(Some(Arc::new(ExecutionJobRecordings::new(fixture.jobs.clone()))));
    let owner = context(EMPLOYEE);
    let thief = context(OTHER);
    let mine = RunHandle(RunId([0x5e; 16]));
    let theirs = RunHandle(RunId([0x6f; 16]));
    fixture.jobs.insert(&owner, "browser_record_start", mine).unwrap();
    fixture.jobs.finish(mine, recorded(fixture.work.path(), &handle_text(mine), b"{\"actions\":[]}")).unwrap();
    fixture.jobs.insert(&thief, "browser_record_start", theirs).unwrap();
    fixture.jobs.finish(theirs, recorded(fixture.work.path(), &handle_text(theirs), b"{\"actions\":[]}")).unwrap();
    // Another employee's finished recording is not this employee's job.
    let call = call(&owner, &handle_text(theirs)).unwrap();
    assert!(recording_summary_input(&call, &fixture.roots).is_err());
    // The other employee cannot read this employee's recording either: its
    // job read fails, and the brokered root belongs to this employee.
    let foreign = call_for_other(&thief, &handle_text(mine));
    assert!(recording_summary_input(&foreign, &fixture.roots).is_err());
    assert!(fixture.broker.calls().is_empty(), "no foreign job reaches root");
    // Unfinished, cancelled-early and non-recording jobs refuse.
    let pending = RunHandle(RunId([0x70; 16]));
    fixture.jobs.insert(&owner, "browser_record_start", pending).unwrap();
    assert!(recording_summary_input(&call_for_other(&owner, &handle_text(pending)), &fixture.roots).is_err());
    let early = RunHandle(RunId([0x71; 16]));
    fixture.jobs.insert(&owner, "browser_record_start", early).unwrap();
    fixture.jobs.finish(early, json!({"id":handle_text(early),"status":"not_recorded","reason":"stop","actions":0,
        "artifacts":[],"navigation":"not_attempted"})).unwrap();
    assert!(recording_summary_input(&call_for_other(&owner, &handle_text(early)), &fixture.roots).is_err());
    let program = RunHandle(RunId([0x72; 16]));
    fixture.jobs.insert(&owner, "execute_program", program).unwrap();
    fixture.jobs.finish(program, recorded(fixture.work.path(), &handle_text(program), b"{}")).unwrap();
    assert!(recording_summary_input(&call_for_other(&owner, &handle_text(program)), &fixture.roots).is_err());
}

/// Authorize as `context` (whichever employee it names).
fn call_for_other(context: &RequestContext, id: &str) -> AuthorizedToolCall { call(context, id).unwrap() }

#[test]
fn h12d_skill_from_recording_refused_without_job_source() {
    let fixture = fixture();
    let _jobs = TestRecordingJobs::install(None);
    let owner = context(EMPLOYEE);
    // Not eligible, so never authorized (and listed nowhere) until HX6.
    assert!(!duduclaw_mcp_boundary::tool_policy::eligible(&owner, "skill_from_recording"));
    assert!(call(&owner, &"ab".repeat(16)).is_err());
    assert!(fixture.broker.calls().is_empty());
}
