use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal as RunPrincipal}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::RequestContext;

fn isolated_context() -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: RunPrincipal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
    })
}

#[tokio::test]
#[cfg(unix)]
async fn discovery_isolation_completed_views_never_invoke_artifact_verifier() {
    let (dir, spec, broker) = fixture();
    let home = dir.path().canonicalize().unwrap();
    let key = duduclaw_core::ensure_identity_key(&home).unwrap();
    let token = duduclaw_core::mint_identity_token(&key, "worker");
    let signed = TrustedCaller::from_signed_agent(&home, "worker", Some(&token)).unwrap();
    let tasks = TaskStore::open(&home).unwrap();
    let created = create(&home, &tasks, &broker, &signed, "worker", "Completed fixture", "Explore", spec.clone()).await.unwrap();
    // Model a completed host-owned record without starting an employee process.
    rusqlite::Connection::open(home.join("tasks.db")).unwrap().execute(
        "UPDATE tasks SET status='done',discovery_approval_id=NULL WHERE id=?1",
        rusqlite::params![created.task_id]).unwrap();
    let store = DiscoveryStore::open(&home).unwrap();
    store.create_run(&created.run_id, "Explore", "worker", "score", &"a".repeat(64), Direction::Max, &spec.budget).unwrap();
    store.attach_run_identity(&super::super::super::online::RunIdentity {
        run_id: created.run_id.clone(), task_id: Some(created.task_id.clone()), creator_id: "agent:worker".into(),
        creator_origin: "mcp".into(), approved_root_id: Some(spec.approved_root_id.clone()),
    }).unwrap();
    store.record_verified_artifact(&created.run_id, "r1-b0-a0", &"b".repeat(64)).unwrap();
    store.finish_run(&created.run_id, "complete", Some("r1-b0-a0")).unwrap();
    let artifact_parent = home.join("discovery/artifacts").join(&created.run_id).join("r1-b0-a0");
    workspace::create_private_directory(&artifact_parent).unwrap();
    let poison = home.join("private-content");
    std::fs::create_dir(&poison).unwrap();
    std::fs::write(poison.join("secret"), "MUST_NOT_BE_READ").unwrap();
    std::os::unix::fs::symlink(&poison, artifact_parent.join("ws")).unwrap();
    let before = super::super::queries::artifact_check_count();
    crate::isolation_egress::scope(&home, "worker", &isolated_context(), None, async {
        let caller = TrustedCaller::from_isolated_agent(&home, "worker").unwrap();
        let listed = list(&home, &caller, None, 20).await.unwrap();
        assert_eq!(listed["runs"].as_array().unwrap().len(), 1);
        let tree = tree(&home, &caller, &created.run_id).await.unwrap();
        for run in [&listed["runs"][0], &tree["run"]] {
            assert_eq!(run["status"], "complete");
            assert_eq!(run["artifact_verified"], Value::Null);
            assert_eq!(run["artifact_verification"], "unavailable");
            assert!(!run.to_string().contains("MUST_NOT_BE_READ"));
        }
        assert!(artifact(&home, &caller, &created.run_id, None).await.is_err());
        assert_eq!(super::super::queries::artifact_check_count(), before,
            "list/tree must not even invoke the pathname-based artifact verifier");
    }).await.unwrap();
    let ordinary = tree(&home, &signed, &created.run_id).await.unwrap();
    assert_eq!(ordinary["run"]["artifact_verified"], false);
    assert_eq!(ordinary["run"]["artifact_verification"], "checked");
    assert_eq!(super::super::queries::artifact_check_count(), before + 1,
        "the control query must exercise the verifier instrumentation");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn discovery_isolation_artifact_reads_published_manifest_and_exact_file_digest() {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use super::super::super::artifact_manifest::ArtifactManifest;
    let (dir, spec, broker) = fixture();
    let home = dir.path().canonicalize().unwrap();
    let key = duduclaw_core::ensure_identity_key(&home).unwrap();
    let token = duduclaw_core::mint_identity_token(&key, "worker");
    let signed = TrustedCaller::from_signed_agent(&home, "worker", Some(&token)).unwrap();
    let tasks = TaskStore::open(&home).unwrap();
    let created = create(&home, &tasks, &broker, &signed, "worker", "Artifact fixture", "Explore", spec.clone()).await.unwrap();
    let store = DiscoveryStore::open(&home).unwrap();
    store.create_run(&created.run_id, "Explore", "worker", "score", &"a".repeat(64), Direction::Max, &spec.budget).unwrap();
    store.attach_run_identity(&super::super::super::online::RunIdentity {
        run_id: created.run_id.clone(), task_id: Some(created.task_id.clone()), creator_id: "agent:worker".into(),
        creator_origin: "mcp".into(), approved_root_id: Some(spec.approved_root_id.clone()),
    }).unwrap();
    let root = home.join("discovery/artifacts").join(&created.run_id).join("r1-b0-a0/ws");
    workspace::create_private_directory(&root).unwrap();
    std::fs::write(root.join("result.txt"), "checkpoint").unwrap();
    std::fs::set_permissions(root.join("result.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let manifest = ArtifactManifest::from_checkpoint(BTreeMap::from([
        (PathBuf::from("result.txt"), format!("{:x}", Sha256::digest(b"checkpoint"))),
    ])).unwrap();
    store.record_artifact_manifest(&created.run_id, "r1-b0-a0", &manifest).unwrap();
    assert_eq!(store.load_artifact_manifest(&created.run_id, "r1-b0-a0").unwrap().unwrap().digest(), manifest.digest());
    store.finish_run(&created.run_id, "complete", Some("r1-b0-a0")).unwrap();
    let before = super::super::queries::artifact_check_count();
    crate::isolation_egress::scope(&home, "worker", &isolated_context(), None, async {
        let caller = TrustedCaller::from_isolated_agent(&home, "worker").unwrap();
        let listed = artifact(&home, &caller, &created.run_id, None).await.unwrap();
        assert_eq!(listed["manifest_verified"], true);
        assert_eq!(listed["verified"], false, "listing must not claim current content verification");
        let file = listed["files"][0]["file_id"].as_str().unwrap();
        let downloaded = artifact(&home, &caller, &created.run_id, Some(file)).await.unwrap();
        assert_eq!(downloaded["verified"], true);
        assert_eq!(downloaded["content_base64"], "Y2hlY2twb2ludA==");
        std::fs::write(root.join("result.txt"), "changed").unwrap();
        assert!(artifact(&home, &caller, &created.run_id, Some(file)).await.is_err());
        rusqlite::Connection::open(home.join("tasks.db")).unwrap().execute(
            "UPDATE tasks SET created_by='agent:other' WHERE id=?1", rusqlite::params![created.task_id]).unwrap();
        let refused = artifact(&home, &caller, &created.run_id, None).await.unwrap_err();
        assert_eq!(refused, "permission denied");
        assert_eq!(super::super::queries::artifact_check_count(), before);
    }).await.unwrap();
}
