use super::*;
use crate::{resources::{test_broker::TestWorkBroker, WorkBrokerError}, tool_policy::{authorize, ResourceResolver,
    TestRecordingJobs, ToolPolicy, RecordingArtifact, RecordingJobs}, RequestContext};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::{json, Value};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

const RUN: RunId = RunId([3; 16]);
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);

struct Toggle(Arc<AtomicBool>);
impl LiveRequestVerifier for Toggle {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::Acquire) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
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
fn employee(id: EmployeeId) -> ResourceGrant { ResourceGrant::Employee { id } }
fn work() -> ResourceGrant { ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() } }
fn live(employee_id: EmployeeId) -> (RequestContext, Arc<AtomicBool>) {
    let flag = Arc::new(AtomicBool::new(true));
    let record = CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: employee_id, run: RUN, task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("skill_from_recording".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: employee_id }, employee(employee_id), work()]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    };
    (RequestContext::with_live_verifier(record, Arc::new(Toggle(flag.clone()))), flag)
}
/// Publish fixture artifacts exactly where the recording helper does.
fn publish(root: &std::path::Path, id: &str) -> RecordingManifest {
    let mut artifacts = vec![];
    for name in recording_layout::ARTIFACTS {
        let relative = recording_layout::artifact_path(id, name).unwrap();
        let path = root.join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = format!("{{\"{name}\":[]}}").into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        artifacts.push(RecordingArtifact { name, bytes: bytes.len() as u64, sha256: Sha256::digest(&bytes).into() });
    }
    RecordingManifest { id: id.to_owned(), artifacts }
}
fn call(context: &RequestContext, id: &str) -> AuthorizedToolCall {
    authorize(context, "skill_from_recording", &json!({"id": id}), &Fixed(vec![employee(context.record().employee), work()])).unwrap()
}

/// H12d regression (reader half): the reader asks root's broker for exactly the
/// helper's `recordings/<id>/<artifact>` under `employee-work`, never the old
/// host-side `<home>/recordings/<id>/{meta.json,done.json,session.har}`.
#[test]
fn h12d_recording_reader_uses_published_layout() {
    let _jobs = TestRecordingJobs::install(Some(Arc::new(NoJobs)));
    let dir = tempfile::tempdir().unwrap();
    let id = "5a".repeat(16);
    let manifest = publish(dir.path(), &id);
    let broker = Arc::new(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
    let mut roots = ResourceRoots::default();
    roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).unwrap();
    let (context, _) = live(EMPLOYEE);
    let call = call(&context, &id);
    for name in ["actions.json", "network.json"] {
        let bytes = roots.read_recording_artifact(&call, &manifest, name).unwrap();
        assert_eq!(bytes, format!("{{\"{name}\":[]}}").into_bytes());
    }
    assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, format!("recordings/{id}/actions.json")),
        (RUN, EMPLOYEE, format!("recordings/{id}/network.json"))]);
    for legacy in ["meta.json", "done.json", "session.har"] {
        assert!(roots.read_recording_artifact(&call, &manifest, legacy).is_err(), "{legacy}");
    }
    assert_eq!(broker.calls().len(), 2, "names outside the layout never reach root");
    // Files only at the pre-H12d host layout are not found under the work root.
    let legacy_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(legacy_dir.path().join("recordings").join(&id)).unwrap();
    std::fs::write(legacy_dir.path().join("recordings").join(&id).join("session.har"), b"{}").unwrap();
    let legacy_broker = Arc::new(TestWorkBroker::new(legacy_dir.path().to_owned(), RUN, EMPLOYEE));
    let mut legacy_roots = ResourceRoots::default();
    legacy_roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, legacy_broker).unwrap();
    assert_eq!(legacy_roots.read_recording_artifact(&call, &manifest, "actions.json").err(),
        Some(FileRefusal::Broker(WorkBrokerError::NotFound)));
}

#[test]
fn h12d_recording_reader_refuses_tampered_foreign_or_ended() {
    let _jobs = TestRecordingJobs::install(Some(Arc::new(NoJobs)));
    let dir = tempfile::tempdir().unwrap();
    let id = "6b".repeat(16);
    let manifest = publish(dir.path(), &id);
    let broker = Arc::new(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
    let mut roots = ResourceRoots::default();
    roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).unwrap();
    let (context, flag) = live(EMPLOYEE);
    let call = call(&context, &id);
    // The same-run employee rewrote the artifact after the helper finished.
    std::fs::write(dir.path().join(recording_layout::artifact_path(&id, "actions.json").unwrap()), b"{\"actions\":[1]}").unwrap();
    assert_eq!(roots.read_recording_artifact(&call, &manifest, "actions.json").err(),
        Some(FileRefusal::Verification(VerificationError::BindingMismatch)));
    // A manifest of another recording cannot be paired with this call.
    let other = RecordingManifest { id: "7c".repeat(16), ..manifest.clone() };
    assert!(roots.read_recording_artifact(&call, &other, "network.json").is_err());
    // A root registered for another employee never serves this call.
    let mut foreign_roots = ResourceRoots::default();
    foreign_roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EmployeeId([9; 16]), broker.clone()).unwrap();
    assert!(foreign_roots.read_recording_artifact(&call, &manifest, "network.json").is_err());
    // The run ended: root refuses (typed), and a revoked capability never reaches root.
    broker.end_run();
    assert_eq!(roots.read_recording_artifact(&call, &manifest, "network.json").err(),
        Some(FileRefusal::Broker(WorkBrokerError::NotLive)));
    let consulted = broker.calls().len();
    flag.store(false, Ordering::Release);
    assert!(roots.read_recording_artifact(&call, &manifest, "network.json").is_err());
    assert_eq!(broker.calls().len(), consulted);
}
