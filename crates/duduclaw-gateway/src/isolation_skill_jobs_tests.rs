use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, ResourceGrant}, execution::RunHandle, ids::{EmployeeId, RunId, Sha256Digest},
    ipc::HelperCallId, recording_layout};
use duduclaw_mcp_boundary::{recording_jobs::{verify_artifact, RecordingJobs as _, RecordingKey, SqliteRecordingJobs},
    resources::{test_broker::TestWorkBroker, EMPLOYEE_WORK_ROOT},
    tool_policy::{authorize, ResourceResolver, TestRecordingJobs, ToolPolicy}};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const RUN: RunId = RunId([3; 16]);
const EARLIER_RUN: RunId = RunId([4; 16]);
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
const OTHER: EmployeeId = EmployeeId([8; 16]);

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
fn work() -> ResourceGrant { ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() } }
fn context_as(employee: EmployeeId, run: RunId, user: &str, generation: u64) -> (RequestContext, Arc<AtomicBool>) {
    let flag = Arc::new(AtomicBool::new(true));
    (RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run, task: None,
        principal: Principal::Human { user_id: Text::new(user.into()).unwrap(), generation },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("skill_from_recording".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: employee }, ResourceGrant::Employee { id: employee },
            work()]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Toggle(flag.clone()))), flag)
}
fn context(employee: EmployeeId) -> RequestContext { context_as(employee, RUN, "owner", 1).0 }
fn call_id(byte: u8) -> HelperCallId { HelperCallId::new([byte; 16]).unwrap() }

/// Write the helper's three artifacts at the shared layout and return the
/// helper's strict `recorded` result for them.
fn publish(work: &Path, id: &str, trace: &[u8]) -> Value {
    let artifacts: Vec<Value> = recording_layout::ARTIFACTS.iter().map(|name| {
        let bytes: Vec<u8> = match *name {
            "trace.zip" => trace.to_vec(),
            "actions.json" => br#"{"actions":[{"kind":"click","tag":"button"}]}"#.to_vec(),
            _ => br#"{"responses":[]}"#.to_vec(),
        };
        let path = work.join(recording_layout::artifact_path(id, name).unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        json!({"name":name,"bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes))})
    }).collect();
    json!({"id":id,"status":"recorded","reason":"stop","actions":1,"artifacts":artifacts,"navigation":"completed"})
}
/// The collector's stored row: the helper result plus `ended_by`.
fn stored(mut result: Value, ended_by: &str) -> Value { result["ended_by"] = json!(ended_by); result }

struct Fixture { _home: tempfile::TempDir, work: tempfile::TempDir, store: Arc<SqliteRecordingJobs>,
    roots: ResourceRoots, broker: Arc<TestWorkBroker> }
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = SqliteRecordingJobs::open(&home.path().join("isolation-execution-jobs.db")).unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    let mut roots = ResourceRoots::default();
    roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).unwrap();
    Fixture { _home: home, work, store, roots, broker }
}
impl Fixture {
    fn source(&self) -> TestRecordingJobs {
        TestRecordingJobs::install(Some(Arc::new(StoredRecordings::new(self.store.clone()))))
    }
    /// A finished recording `call` made by `owner` in `owner`'s own run.
    fn finished(&self, owner: &RequestContext, call: HelperCallId, result: Value) {
        let key = RecordingKey { run: RunHandle(owner.record().run), call };
        self.store.insert(owner, key).unwrap();
        self.store.finish(key, result).unwrap();
    }
}
fn authorized(context: &RequestContext, id: &str) -> AuthorizedToolCall {
    authorize(context, "skill_from_recording", &json!({"id": id}),
        &Fixed(vec![ResourceGrant::Employee { id: context.record().employee }, work()])).unwrap()
}

#[test]
fn sfr_own_finished_job_only() {
    let f = fixture();
    let _source = f.source();
    let owner = context(EMPLOYEE);
    // Recorded in an earlier run of the same employee (flushed at run end),
    // distilled now: the stored row is the evidence, not the run.
    let (earlier, _) = context_as(EMPLOYEE, EARLIER_RUN, "owner", 1);
    let id = call_id(0x4d).hex();
    f.finished(&earlier, call_id(0x4d), stored(publish(f.work.path(), &id, b"PK\x03\x04trace"), "run_end"));
    let (manifest, recording) = recording_artifacts(&authorized(&owner, &id), &f.roots).unwrap();
    assert_eq!((manifest.id.as_str(), recording.id()), (id.as_str(), id.as_str()));
    // Exactly the three layout paths, through root's broker for this run and employee.
    assert_eq!(f.broker.calls(), recording_layout::ARTIFACTS.iter()
        .map(|name| (RUN, EMPLOYEE, format!("recordings/{id}/{name}"))).collect::<Vec<_>>());

    // Not (yet) a finished recording: live, stop requested, uncertain, not recorded.
    let live = call_id(0x50);
    f.store.insert(&owner, RecordingKey { run: RunHandle(RUN), call: live }).unwrap();
    let uncertain = call_id(0x51);
    f.finished(&owner, uncertain, json!({"status":"uncertain","reason":"run_killed"}));
    let early = call_id(0x52);
    f.finished(&owner, early, stored(json!({"id":early.hex(),"status":"not_recorded","reason":"stop","actions":0,
        "artifacts":[],"navigation":"not_attempted"}), "stop"));
    let consulted = f.broker.calls().len();
    for call in [live, uncertain, early, call_id(0x53)] {
        assert!(recording_artifacts(&authorized(&owner, &call.hex()), &f.roots).is_err(), "{}", call.hex());
    }
    assert_eq!(f.broker.calls().len(), consulted, "no unfinished or unknown job reaches root");
}

#[test]
fn sfr_foreign_job_refused() {
    let f = fixture();
    let _source = f.source();
    let owner = context(EMPLOYEE);
    let thief = context(OTHER);
    let (mine, theirs) = (call_id(0x5e), call_id(0x6f));
    f.finished(&owner, mine, stored(publish(f.work.path(), &mine.hex(), b"PK\x03\x04mine"), "stop"));
    f.finished(&thief, theirs, stored(publish(f.work.path(), &theirs.hex(), b"PK\x03\x04theirs"), "stop"));
    // Another employee's finished recording is not this employee's job.
    assert!(recording_artifacts(&authorized(&owner, &theirs.hex()), &f.roots).is_err());
    // …and the other employee cannot name this one's.
    assert!(recording_artifacts(&authorized(&thief, &mine.hex()), &f.roots).is_err());
    // Same employee, another person (or the same person's older credential
    // generation): the owner read keys principal + binding too.
    for (user, generation) in [("intruder", 1), ("owner", 2)] {
        let (other_person, _) = context_as(EMPLOYEE, RUN, user, generation);
        assert!(recording_artifacts(&authorized(&other_person, &mine.hex()), &f.roots).is_err(), "{user} {generation}");
    }
    assert!(f.broker.calls().is_empty(), "no foreign job reaches root");
    // Without an installed source nothing is eligible or read.
    let _none = TestRecordingJobs::install(None);
    assert!(!duduclaw_mcp_boundary::tool_policy::eligible(&owner, "skill_from_recording"));
    assert!(authorize(&owner, "skill_from_recording", &json!({"id": mine.hex()}),
        &Fixed(vec![ResourceGrant::Employee { id: EMPLOYEE }, work()])).is_err());
}

#[test]
fn sfr_unverified_or_hash_mismatch_artifact_refused() {
    let f = fixture();
    let _source = f.source();
    let owner = context(EMPLOYEE);
    let call = call_id(0x7a);
    let id = call.hex();
    let result = publish(f.work.path(), &id, b"PK\x03\x04trace-bytes");
    f.finished(&owner, call, stored(result.clone(), "stop"));
    // The same-run runtime rewrote trace.zip after the flush (same size).
    let trace = f.work.path().join(recording_layout::artifact_path(&id, "trace.zip").unwrap());
    std::fs::write(&trace, b"PK\x03\x04trace-bytEs").unwrap();
    let refusal = recording_artifacts(&authorized(&owner, &id), &f.roots).err().unwrap();
    assert_eq!(refusal.artifact(), Some(ArtifactError::Modified));
    // A stored row that is not the collector's exact shape is no manifest.
    for (byte, row) in [(0x7b, result.clone()), (0x7c, stored(result.clone(), "whenever")),
        (0x7d, { let mut extra = stored(result.clone(), "stop"); extra["path"] = json!("/etc/passwd"); extra })] {
        let forged = call_id(byte);
        let mut row = row;
        row["id"] = json!(forged.hex());
        f.finished(&owner, forged, row);
        assert!(StoredRecordings::new(f.store.clone()).recorded(&owner, &forged.hex()).is_err(), "{byte:#x}");
    }
    // Verified artifacts out of layout order never form a recording.
    let digests = recording_jobs::artifacts_from_result(&result).unwrap();
    let bytes = |name: &str| std::fs::read(f.work.path().join(recording_layout::artifact_path(&id, name).unwrap())).unwrap();
    let actions = verify_artifact(&digests, "actions.json", bytes("actions.json")).unwrap();
    let network = verify_artifact(&digests, "network.json", bytes("network.json")).unwrap();
    let again = verify_artifact(&digests, "network.json", bytes("network.json")).unwrap();
    assert_eq!(VerifiedRecording::new(&id, [actions, network, again]).err(), Some(ArtifactError::Unknown));
}

#[tokio::test]
async fn sfr_publish_into_own_skills_only() {
    let home = tempfile::tempdir().unwrap();
    let f = fixture();
    let _source = f.source();
    let (owner, flag) = context_as(EMPLOYEE, RUN, "owner", 1);
    let id = call_id(0x11).hex();
    let call = authorized(&owner, &id);
    let skill = "---\nname: order-entry\ndescription: Enter an order\n---\n## 步驟\n1. Open the form.";
    write_owned(home.path(), &call, "main", "order-entry", skill).await.unwrap();
    let own = home.path().join("agents/main/SKILLS/order-entry.md");
    assert_eq!(std::fs::read_to_string(&own).unwrap(), skill);
    // Never an overwrite of an existing own skill.
    assert!(write_owned(home.path(), &call, "main", "order-entry", "---\nname: x\n---\nother").await.is_err());
    assert_eq!(std::fs::read_to_string(&own).unwrap(), skill);
    // No path, no reserved label, no other tool's call, no oversize, no revoked run.
    for (agent, slug) in [("main", "../escape"), ("main", "Notes"), ("main", "a/b"), ("", "notes"), ("../main", "notes")] {
        assert!(write_owned(home.path(), &call, agent, slug, skill).await.is_err(), "{agent} {slug}");
    }
    assert!(write_owned(home.path(), &call, "main", "big", &"x".repeat(MAX_SKILL_BYTES + 1)).await.is_err());
    flag.store(false, Ordering::Release);
    assert!(write_owned(home.path(), &call, "main", "revoked", skill).await.is_err());
    flag.store(true, Ordering::Release);
    let entries: Vec<_> = std::fs::read_dir(home.path().join("agents/main/SKILLS")).unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(entries, ["order-entry.md"]);
    assert!(!home.path().join("skills").exists(), "never the global bank");
    assert_eq!(std::fs::read_dir(home.path().join("agents")).unwrap().count(), 1, "only the caller's own label");
    // The full path scans these exact bytes in E first; without a worker it
    // fails closed and writes nothing.
    assert!(publish_owned(home.path(), &call, "main", "scanned", skill).await.is_err());
    assert!(!home.path().join("agents/main/SKILLS/scanned.md").exists());
}
