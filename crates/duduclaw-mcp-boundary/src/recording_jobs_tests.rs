//! HX6 recording store: `(run, call)` keying, owner binding, stored digests.
use super::*;
use duduclaw_isolation::ids::RunId;

fn sha(bytes: &[u8]) -> String { Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect() }
pub(crate) fn recorded(id: &str, files: [&[u8]; 3]) -> Value {
    let artifacts: Vec<Value> = ARTIFACT_NAMES.iter().zip(files).map(|(name, bytes)|
        json!({"name":name, "bytes":bytes.len(), "sha256":sha(bytes)})).collect();
    json!({"id":id, "status":"recorded", "reason":"stop", "actions":0, "artifacts":artifacts, "navigation":"completed"})
}

#[test]
fn artifact_hash_mismatch_refused() {
    let result = recorded(&"07".repeat(16), [b"trace", b"{\"actions\":[]}", b"{\"responses\":[]}"]);
    let digests = artifacts_from_result(&result).unwrap();
    assert_eq!(digests.iter().map(|d| d.name).collect::<Vec<_>>(), ARTIFACT_NAMES);
    let verified = verify_artifact(&digests, "trace.zip", b"trace".to_vec()).unwrap();
    assert_eq!((verified.name(), verified.bytes()), ("trace.zip", &b"trace"[..]));
    assert_eq!(verified.into_bytes(), b"trace");
    // Rewritten by the runtime (same size or not), truncated, or replaced.
    for forged in [&b"TRACE"[..], b"trac", b"", b"trace\n"] {
        let error = verify_artifact(&digests, "trace.zip", forged.to_vec()).unwrap_err();
        assert_eq!((error, error.code()), (ArtifactError::Modified, "artifact_modified"));
    }
    assert_eq!(verify_artifact(&digests, "../trace.zip", b"trace".to_vec()).err(), Some(ArtifactError::Unknown));
    // Only a strictly shaped `recorded` result has digests.
    for bad in [json!({"status":"uncertain","reason":"run_killed"}),
        json!({"status":"not_recorded","artifacts":[]}),
        { let mut v = result.clone(); v["artifacts"][0]["sha256"] = json!("A".repeat(64)); v },
        { let mut v = result.clone(); v["artifacts"][1]["name"] = json!("trace.zip"); v },
        { let mut v = result.clone(); v["artifacts"][2]["bytes"] = json!(MAX_ARTIFACT_BYTES + 1); v }] {
        assert_eq!(artifacts_from_result(&bad), Err(ArtifactError::Unknown), "{bad}");
    }
}

#[test]
fn recording_ids_are_exact_call_ids() {
    let call = parse_id(&"0a".repeat(16)).unwrap();
    assert_eq!(call.hex(), "0a".repeat(16));
    for bad in ["0".repeat(32), "0A".repeat(16), "0a".repeat(15), "../recording".into(), format!("{}x", "0a".repeat(16))] {
        assert!(parse_id(&bad).is_err(), "{bad}");
    }
}

#[cfg(feature = "binding-store")]
mod sqlite {
    use super::*;
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding,
        Lane, Principal, ResourceGrant}, ids::Sha256Digest};

    fn context() -> RequestContext {
        RequestContext::with_live_verifier(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![Text::new("browser_record_start".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
                ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap(),
            policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX - 1,
            nonce: [5; 32], jti: [6; 16],
        }, std::sync::Arc::new(AlwaysLive))
    }
    fn key(call: u8) -> RecordingKey {
        RecordingKey { run: RunHandle(context().record().run), call: HelperCallId::new([call; 16]).unwrap() }
    }

    #[test]
    fn recording_rows_are_owned_keyed_by_run_and_call_and_single_finish() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteRecordingJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        // In-run only: a row for another run is refused at insert.
        let foreign = RecordingKey { run: RunHandle(RunId([9; 16])), call: HelperCallId::new([1; 16]).unwrap() };
        assert_eq!(jobs.insert(&owner, foreign), Err(VerificationError::BindingMismatch));
        jobs.insert(&owner, key(1)).unwrap();
        assert!(jobs.insert(&owner, key(1)).is_err(), "call ids are unique");
        let row = jobs.read(&owner, key(1).call).unwrap();
        assert_eq!((row.key, row.state, row.result.clone()), (key(1), RecordingState::Recording, None));
        // Another employee / principal / generation / binding never sees it.
        let mut employee = owner.record().clone(); employee.employee = EmployeeId([8; 16]);
        let mut principal = owner.record().clone();
        principal.principal = Principal::Human { user_id: Text::new("other".into()).unwrap(), generation: 1 };
        let mut generation = owner.record().clone();
        generation.principal = Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 2 };
        let mut binding = owner.record().clone();
        binding.employee_binding = EmployeeBinding::Shared(Text::new("owner".into()).unwrap());
        for record in [employee, principal, generation, binding] {
            let thief = RequestContext::with_live_verifier(record, std::sync::Arc::new(AlwaysLive));
            assert!(jobs.read(&thief, key(1).call).is_err());
            assert!(jobs.request_stop(&thief, key(1)).is_err());
        }
        assert_eq!(jobs.stop_by(key(1)).unwrap(), None);
        jobs.request_stop(&owner, key(1)).unwrap();
        jobs.mark_stop(key(1), StopBy::Revoked).unwrap();
        assert_eq!(jobs.stop_by(key(1)).unwrap(), Some(StopBy::Stop), "the first stop reason wins");
        assert_eq!(jobs.read(&owner, key(1).call).unwrap().state, RecordingState::StopRequested);
        let result = recorded(&key(1).id(), [b"a", b"b", b"c"]);
        jobs.finish(key(1), result.clone()).unwrap();
        assert!(jobs.finish(key(1), json!({"status":"recorded"})).is_err(), "single use");
        let done = jobs.read(&owner, key(1).call).unwrap();
        assert_eq!((done.state, done.result), (RecordingState::Complete, Some(result)));
        // A later run of the same actor reads the stored result.
        let mut later = owner.record().clone(); later.run = RunId([7; 16]);
        let later = RequestContext::with_live_verifier(later, std::sync::Arc::new(AlwaysLive));
        assert_eq!(jobs.read(&later, key(1).call).unwrap().state, RecordingState::Complete);
        assert_eq!(jobs.artifacts(owner.record().employee, key(1).call).unwrap().len(), 3);
        assert!(jobs.artifacts(EmployeeId([8; 16]), key(1).call).is_err());
    }

    #[test]
    fn recording_restart_marks_live_rows_uncertain() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("jobs.db");
        { let jobs = SqliteRecordingJobs::open(&path).unwrap(); jobs.insert(&context(), key(2)).unwrap(); }
        let jobs = SqliteRecordingJobs::open(&path).unwrap();
        let row = jobs.read(&context(), key(2).call).unwrap();
        assert_eq!(row.state, RecordingState::Uncertain);
        assert_eq!(row.result.unwrap()["reason"], "gateway_restarted");
        assert!(jobs.finish(key(2), json!({"status":"recorded"})).is_err());
        // Shares the file with the legacy job table without touching it.
        assert!(crate::execution_jobs::SqliteExecutionJobs::open(&path).is_ok());
    }

    struct AlwaysLive;
    impl duduclaw_isolation::capability::LiveRequestVerifier for AlwaysLive {
        fn verify_live(&self, _: &duduclaw_isolation::capability::CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
}
