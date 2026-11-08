//! SR2 finding 2 (owner decision 2026-11-08): Personal → Shared delegation
//! needs a single-use owner approval bound to (source employee, target
//! employee, task digest). Real approvals.db and receipt store; the live run
//! association (`delegation_source`) is exercised on Linux by `record_delegation`.
use super::*;
use duduclaw_isolation::{bounded::Text, execution::RunHandle, ids::RunId};

const OWNER: &str = "alice-user-id";
const SOURCE: &str = "alice-personal";
const TARGET: &str = "team";
const SOURCE_ID: EmployeeId = EmployeeId([1; 16]);
const TARGET_ID: EmployeeId = EmployeeId([2; 16]);

fn personal() -> EmployeeBinding { EmployeeBinding::Personal(Text::new(OWNER.into()).unwrap()) }
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new("tenant-1".into()).unwrap()) }

fn material(task: &str) -> Value {
    super::super::material::delegation(SOURCE, TARGET, task, None, None, None, 1, None, None,
        Some(SOURCE), false, 0, false, Some("user:alice-user-id"))
}

fn gate_for(task: &str, target: EmployeeId) -> Gate {
    gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, target, &material(task)).unwrap().expect("personal source")
}

/// The exact delegation receipt `record_delegation` builds for the target.
fn receipt(home: &Path, id: &str, task: &str, target: EmployeeId) -> RecordAuthority {
    let mut value = super::super::tests::receipt(home);
    value.kind = "delegation".into();
    value.id = id.into();
    value.agent = TARGET.into();
    value.employee = target;
    value.approval = None;
    value.caller = Some(SOURCE_ID);
    value.parent_run = Some(RunHandle(RunId([3; 16])));
    value.parent_agent = Some(SOURCE.into());
    value.parent_policy = Some("source-policy".into());
    value.material = store::material_digest(&material(task)).unwrap();
    value
}

async fn cards(home: &Path) -> Vec<ApprovalRecord> {
    ApprovalBroker::open(home).unwrap().list_by_kind(KIND).await.unwrap()
}

async fn pending_card(home: &Path, gate: &Gate) -> crate::approval::ApprovalId {
    cards(home).await.into_iter().find(|card| binds(card, gate) && card.status == ApprovalStatus::Pending)
        .expect("a pending owner card").id
}

fn queued(home: &Path, id: &str) -> bool { store::load(home, "delegation", id).unwrap().is_some() }

#[tokio::test]
async fn personal_to_shared_delegation_needs_owner_approval() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    let task = "請整理本週客戶回覆，並草擬給業務團隊的摘要。".repeat(20);
    let gate = gate_for(&task, TARGET_ID);
    assert_eq!(gate.task_digest, task_digest(&task));
    let first = receipt(home, "msg-1", &task, TARGET_ID);

    // 1. Nothing is queued; one owner card names source, target and the
    //    truncated task, bound to (source id, target id, task digest).
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-1"));
    let filed = cards(home).await;
    assert_eq!(filed.len(), 1);
    let card = &filed[0];
    assert_eq!((card.agent_id.as_str(), card.status), (SOURCE, ApprovalStatus::Pending));
    assert!(card.summary.contains(SOURCE) && card.summary.contains(TARGET), "{}", card.summary);
    assert!(card.summary.contains(&duduclaw_core::truncate_chars(&task, MAX_TASK_CHARS)));
    assert!(!card.summary.contains(&task), "the task text is truncated");
    assert_eq!(card.payload["source_employee"], hex::encode(SOURCE_ID.0));
    assert_eq!(card.payload["target_employee"], hex::encode(TARGET_ID.0));
    assert_eq!(card.payload["task_digest"], task_digest(&task));
    assert_eq!(card.payload["owner_user_id"], OWNER);
    // Retrying while it is pending files no second card.
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::OwnerApprovalPending));
    assert_eq!(cards(home).await.len(), 1);
    // The generic approval centre cannot decide it, not even an Admin.
    let handler = crate::handlers::MethodHandler::new(home.to_path_buf()).await;
    let frame = handler.handle_approvals_decide(json!({"id": card.id.as_str(), "approve": true}),
        &duduclaw_auth::UserContext::admin_fallback()).await;
    assert!(matches!(&frame, crate::protocol::WsFrame::Response { ok: false, error: Some(error), .. }
        if error.as_str() == Some(DECIDE_ELSEWHERE)), "{frame:?}");
    assert_eq!(pending_card(home, &gate).await, card.id);

    // 2. Approve (owner, dashboard) → queued exactly once.
    let broker = ApprovalBroker::open(home).unwrap();
    broker.decide(&card.id, true, &format!("dashboard:{OWNER}")).await.unwrap();
    assert_eq!(admit(home, &gate, &first).await, Ok(()));
    assert!(queued(home, "msg-1"));
    // Replay of the same delegation id: refused, nothing new.
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::Failed(VerificationError::Revoked)));
    // Replay of the approval for a new delegation id: single use, a new card.
    let second = receipt(home, "msg-2", &task, TARGET_ID);
    assert_eq!(admit(home, &gate, &second).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-2"));
    assert_eq!(cards(home).await.len(), 2);

    // 3. Deny (owner) → not queued, and no new card while the denial stands.
    let denied = pending_card(home, &gate).await;
    broker.decide(&denied, false, &format!("dashboard:{OWNER}")).await.unwrap();
    assert_eq!(admit(home, &gate, &second).await, Err(DelegationError::OwnerDenied));
    assert!(!queued(home, "msg-2"));
    assert_eq!(cards(home).await.len(), 2);

    // 4. Expired: an owner approval past the card's TTL never sends.
    let late_task = "another task";
    let late = gate_for(late_task, TARGET_ID);
    let third = receipt(home, "msg-3", late_task, TARGET_ID);
    assert_eq!(admit(home, &late, &third).await, Err(DelegationError::OwnerApprovalPending));
    let id = pending_card(home, &late).await;
    broker.decide(&id, true, &format!("dashboard:{OWNER}")).await.unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    let older = (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339();
    rusqlite::Connection::open(home.join("approvals.db")).unwrap()
        .execute("UPDATE approvals SET created_at=?1, decided_at=?2 WHERE id=?3",
            rusqlite::params![older, old, id.as_str()]).unwrap();
    assert_eq!(admit(home, &late, &third).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-3"));

    // 5. Approved by anyone but the owner (an Admin, a channel): never sends.
    let other_task = "third task";
    let other = gate_for(other_task, TARGET_ID);
    let fourth = receipt(home, "msg-4", other_task, TARGET_ID);
    assert_eq!(admit(home, &other, &fourth).await, Err(DelegationError::OwnerApprovalPending));
    broker.decide(&pending_card(home, &other).await, true, "dashboard:admin-user-id").await.unwrap();
    assert_eq!(admit(home, &other, &fourth).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-4"));

    // 6. The approval is bound to the target employee: another target id
    //    (e.g. the label recreated) cannot use it.
    let moved_task = "fourth task";
    let original = gate_for(moved_task, TARGET_ID);
    let fifth = receipt(home, "msg-5", moved_task, TARGET_ID);
    assert_eq!(admit(home, &original, &fifth).await, Err(DelegationError::OwnerApprovalPending));
    broker.decide(&pending_card(home, &original).await, true, &format!("dashboard:{OWNER}")).await.unwrap();
    let recreated = gate_for(moved_task, EmployeeId([9; 16]));
    let sixth = receipt(home, "msg-6", moved_task, EmployeeId([9; 16]));
    assert_eq!(admit(home, &recreated, &sixth).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-6"));
    // A receipt that does not match the gate is refused outright.
    assert_eq!(admit(home, &original, &sixth).await, Err(DelegationError::Failed(VerificationError::BindingMismatch)));
    // The owner's own approval still works once for the original target.
    assert_eq!(admit(home, &original, &fifth).await, Ok(()));
    assert!(queued(home, "msg-5"));

    assert_eq!(DelegationError::OwnerApprovalPending.user_message(), Some(PENDING_TEXT));
    assert_eq!(DelegationError::OwnerDenied.user_message(), Some(DENIED_TEXT));
}

#[tokio::test]
async fn shared_delegation_unchanged() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    // Shared → anything: no gate, whatever the principal.
    for principal in [OWNER, "someone-else", ""] {
        assert_eq!(gate(&shared(), principal, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &material("x")), Ok(None));
    }
    // The Shared path stores the receipt directly; no owner card is filed.
    let value = receipt(home, "shared-1", "x", TARGET_ID);
    store::save(&value).unwrap();
    assert!(queued(home, "shared-1"));
    assert!(cards(home).await.is_empty());
    // A Personal source whose run principal is not the owner is refused, and
    // a delegation without task text cannot be bound.
    assert_eq!(gate(&personal(), "someone-else", SOURCE, SOURCE_ID, TARGET, TARGET_ID, &material("x")),
        Err(VerificationError::Revoked));
    assert_eq!(gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &json!({})),
        Err(VerificationError::Invalid));
}
