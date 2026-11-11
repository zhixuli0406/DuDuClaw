//! H13b: the ephemeral child's receipt shape, owner rule and tool narrowing.
use super::*;
use duduclaw_isolation::{capability::EmployeeBinding, execution::RunHandle, ids::{EmployeeId, RunId}};
use crate::isolation_background::same_owner;
use crate::isolation_ephemeral::{EphemeralRequest, ParentRun};

fn t(value: &str) -> Text<128> { Text::new(value.into()).unwrap() }
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new("tenant-1".into()).unwrap()) }
fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(owner.into()).unwrap()) }
const RUN: RunHandle = RunHandle(RunId([5; 16]));

/// The parent of `super::super::tests::receipt`: label `alice`, employee
/// `[1; 16]`, incarnation 0, requester `original-user`.
fn parent(binding: EmployeeBinding, requester: &str) -> ParentRun {
    ParentRun { agent: "alice".into(), employee: EmployeeId([1; 16]), incarnation: 0, binding, run: RUN,
        lane: Lane::Standard, requester: requester.into(), task: None }
}
fn request() -> EphemeralRequest {
    EphemeralRequest { instruction: "You review invoices.".into(), context: "Check the March batch.".into(),
        tier: "cheap".into(), tools: vec!["wiki_read".into(), "memory_search".into()] }
}
/// The receipt `record_child` captures for `job` (self-delegation).
fn child_receipt(job: &EphemeralJob) -> RecordAuthority {
    let mut receipt = super::super::tests::receipt(&super::super::tests::trusted_home());
    receipt.kind = "delegation".into();
    receipt.id = message_id(&job.id);
    receipt.binding = Some(job.parent.binding.clone());
    receipt.approval = None;
    receipt.caller = Some(receipt.employee);
    receipt.parent_run = Some(job.parent.run);
    receipt.parent_agent = Some(receipt.agent.clone());
    receipt.parent_policy = Some("original-policy".into());
    receipt
}
fn snapshot(human: &str, owner: Option<&str>, agent: &str) -> duduclaw_auth::IsolationAuthoritySnapshot {
    let mut user = duduclaw_auth::UserContext::admin_fallback();
    user.user_id = human.into();
    user.agent_access.insert(agent.into(), duduclaw_auth::AccessLevel::Owner);
    duduclaw_auth::IsolationAuthoritySnapshot { tenant_id: "tenant-1".into(), acl_generation: 1, policy_epoch: 1,
        human: Some(user), personal_owner: owner.map(Into::into), agent_member_count: 1 }
}

#[test]
fn h13b_personal_spawn_ephemeral_requires_owner_initiated_parent() {
    let owned = personal("original-user");
    // The job: a Personal parent's child exists only for its owner's request.
    assert_eq!(EphemeralJob::new(parent(owned.clone(), "intruder"), request(), 1), Err(VerificationError::BindingMismatch));
    let job = EphemeralJob::new(parent(owned.clone(), "original-user"), request(), 1).unwrap();
    assert_eq!(owner_principal(&job).unwrap(), "user:original-user");
    // Capture (`record_child` → `record_delegation_inner`): the verified
    // requester must be exactly the lifecycle owner …
    assert_eq!(same_owner::personal_target_allowed(&owned, &owned, "original-user"), Ok(()));
    assert_eq!(same_owner::personal_target_allowed(&owned, &owned, "intruder"), Err(VerificationError::BindingMismatch));
    // … on a fresh users.db read that shows that owner (capture and recheck).
    assert_eq!(same_owner::check_binding_facts(&owned, "alice", &snapshot("original-user", Some("original-user"), "alice")), Ok(()));
    assert_eq!(same_owner::check_binding_facts(&owned, "alice", &snapshot("intruder", Some("original-user"), "alice")),
        Err(VerificationError::Revoked));
    // The receipt is the one Personal background shape B1 admits, stored and
    // issued only for that owner.
    let mut receipt = child_receipt(&job);
    assert!(same_owner::personal_receipt_allowed(&receipt));
    assert!(same_owner::personal_record_allowed(&receipt, "original-user"));
    assert!(!same_owner::personal_record_allowed(&receipt, "someone-else"));
    assert_eq!(super::super::store::validate(&receipt), Ok(()));
    assert_eq!(ephemeral_job_of(&receipt), Some(job.id.as_str()));
    // Requested by someone else (e.g. an Admin who may assign the employee):
    // never valid, never issued.
    receipt.owner = "user:intruder".into();
    assert!(!same_owner::personal_receipt_allowed(&receipt));
    assert_eq!(super::super::store::validate(&receipt), Err(VerificationError::Invalid));
    // Any other Personal background record stays refused (B1).
    let mut cron = child_receipt(&job);
    cron.kind = "cron".into();
    assert!(!same_owner::personal_receipt_allowed(&cron));
    assert_eq!(super::super::store::validate(&cron), Err(VerificationError::Invalid));
}

/// A same-owner delegated background receipt: `bob` (Personal, same owner)
/// delegated to `alice` from the owner's live run.
fn delegated_receipt(owner: &str) -> RecordAuthority {
    let mut receipt = super::super::tests::receipt(&super::super::tests::trusted_home());
    receipt.kind = "delegation".into();
    receipt.id = "message-1".into();
    receipt.binding = Some(personal(owner));
    receipt.approval = None;
    receipt.caller = Some(EmployeeId([2; 16]));
    receipt.parent_run = Some(RUN);
    receipt.parent_agent = Some("bob".into());
    receipt.parent_policy = Some("original-policy".into());
    receipt
}

#[test]
fn h13c_background_source_never_reaches_personal_target() {
    let owned = personal("original-user");
    let job = EphemeralJob::new(parent(owned.clone(), "original-user"), request(), 1).unwrap();
    // Both runs below are issued for `Principal::Human { original-user }`, yet
    // no owner is present: their `DelegationSource.parent` is a receipt.
    let background = delegated_receipt("original-user");
    let child = child_receipt(&job);
    assert!(same_owner::personal_receipt_allowed(&background) && same_owner::personal_receipt_allowed(&child));
    for parent in [&background, &child] {
        // spawn_agent / delegation to a same-owner Personal employee: refused …
        assert_eq!(same_owner::personal_target_from_run(&owned, &owned, "original-user", Some(parent)),
            Err(VerificationError::BindingMismatch));
        // … and spawn_ephemeral of the Personal employee itself: refused.
        assert_eq!(same_owner::ephemeral_parent_live(&owned, Some(parent)), Err(VerificationError::BindingMismatch));
    }
    // The live owner session (no parent receipt): allowed, no card.
    assert_eq!(same_owner::personal_target_from_run(&owned, &owned, "original-user", None), Ok(()));
    assert_eq!(same_owner::ephemeral_parent_live(&owned, None), Ok(()));
    // A live session of someone else is still refused (H13b rule kept).
    assert_eq!(same_owner::personal_target_from_run(&owned, &owned, "intruder", None), Err(VerificationError::BindingMismatch));
    // Shared targets and Shared parents are unchanged (a Personal source's
    // Shared target keeps the owner card path).
    for parent in [None, Some(&background)] {
        assert_eq!(same_owner::personal_target_from_run(&owned, &shared(), "original-user", parent), Ok(()));
        assert_eq!(same_owner::personal_target_from_run(&shared(), &shared(), "anyone", parent), Ok(()));
        assert_eq!(same_owner::ephemeral_parent_live(&shared(), parent), Ok(()));
    }
}

#[test]
fn h13c_spawn_tools_not_offered_to_personal_background_runs() {
    let tools = || List::new(vec![t("spawn_agent"), t("wiki_read"), t("spawn_ephemeral"), t("memory_search")]).unwrap();
    let names = |list: List<Text<128>, 256>| list.as_slice().iter().map(|tool| tool.as_str().to_owned()).collect::<Vec<_>>();
    // A record-backed run of a Personal employee: neither spawn tool is signed,
    // so neither is discovered nor eligible.
    assert_eq!(names(same_owner::record_run_tools(&personal("original-user"), true, tools()).unwrap()),
        vec!["wiki_read".to_owned(), "memory_search".to_owned()]);
    // The owner's live session, and any Shared run, keep their snapshot.
    for (binding, record) in [(personal("original-user"), false), (shared(), true), (shared(), false)] {
        assert_eq!(names(same_owner::record_run_tools(&binding, record, tools()).unwrap()), names(tools()));
    }
}

#[test]
fn h13c_child_never_holds_spawn_tools() {
    let job = EphemeralJob { tools: vec!["spawn_agent".into(), "spawn_ephemeral".into(), "wiki_read".into()],
        ..EphemeralJob::new(parent(shared(), "original-user"), request(), 1).unwrap() };
    let granted = [t("spawn_agent"), t("spawn_ephemeral"), t("wiki_read")];
    assert_eq!(narrowed_tools(&granted, &job.tools).unwrap().as_slice(), &[t("wiki_read")]);
    // A job naming only spawn tools grants nothing: the child run is refused.
    assert_eq!(narrowed_tools(&granted, &["spawn_agent".into(), "spawn_ephemeral".into()]).err(),
        Some(VerificationError::Invalid));
    let receipt = child_receipt(&job);
    assert_eq!(child_tools(&receipt, &granted, |_| Ok(Some(job.clone()))).unwrap().unwrap().as_slice(), &[t("wiki_read")]);
}

#[test]
fn h13b_child_tools_narrowed_to_job() {
    let job = EphemeralJob::new(parent(shared(), "original-user"), request(), 1).unwrap();
    let receipt = child_receipt(&job);
    let granted = [t("web_extract"), t("memory_search"), t("spawn_ephemeral"), t("wiki_read")];
    fn narrowed(receipt: &RecordAuthority, granted: &[Text<128>], found: Result<Option<EphemeralJob>, VerificationError>)
        -> Result<Option<Vec<Text<128>>>, VerificationError> {
        let expected = receipt.id.clone();
        child_tools(receipt, granted, move |id| { assert_eq!(message_id(id), expected); found })
            .map(|tools| tools.map(|tools| tools.as_slice().to_vec()))
    }
    // Grant order, only the job's tools: never more than the job …
    assert_eq!(narrowed(&receipt, &granted, Ok(Some(job.clone()))), Ok(Some(vec![t("memory_search"), t("wiki_read")])));
    // … nor than the child's own live snapshot (the parent may have lost one).
    assert_eq!(narrowed(&receipt, &[t("wiki_read"), t("web_extract")], Ok(Some(job.clone()))), Ok(Some(vec![t("wiki_read")])));
    assert_eq!(narrowed(&receipt, &[t("web_extract")], Ok(Some(job.clone()))), Err(VerificationError::Invalid));
    // Not released (pending, dropped, unknown) or unreadable: the run is refused.
    assert_eq!(narrowed(&receipt, &granted, Ok(None)), Err(VerificationError::Revoked));
    assert_eq!(narrowed(&receipt, &granted, Err(VerificationError::Unavailable)), Err(VerificationError::Unavailable));
    // A job that does not match the receipt exactly is refused.
    for changed in [ParentRun { incarnation: 1, ..job.parent.clone() }, ParentRun { run: RunHandle(RunId([6; 16])), ..job.parent.clone() },
        ParentRun { requester: "someone-else".into(), ..job.parent.clone() }, ParentRun { employee: EmployeeId([9; 16]), ..job.parent.clone() }] {
        let other = EphemeralJob { parent: changed, ..job.clone() };
        assert_eq!(narrowed(&receipt, &granted, Ok(Some(other))), Err(VerificationError::BindingMismatch));
    }
    // A self-delegation under any other id never runs; an ordinary delegation
    // to another employee is not narrowed here.
    let mut forged = receipt.clone();
    forged.id = "message-1".into();
    assert_eq!(child_tools(&forged, &granted, |_| unreachable!()).err(), Some(VerificationError::BindingMismatch));
    let mut other_target = receipt.clone();
    other_target.parent_agent = Some("bob".into());
    assert!(child_tools(&other_target, &granted, |_| unreachable!()).unwrap().is_none());
    // Message ids are exactly the prefix + a canonical uuid.
    assert_eq!(job_id_of(&message_id(&job.id)), Some(job.id.as_str()));
    let upper = format!("ephemeral-{}", job.id.to_uppercase());
    for id in ["ephemeral-", "ephemeral-../x", upper.as_str(), job.id.as_str()] {
        assert_eq!(job_id_of(id), None, "{id}");
    }
}
