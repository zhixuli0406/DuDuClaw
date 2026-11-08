//! v1.81 P1: Admin break-glass reads of a Personal employee's private data.
use super::*;
use crate::personal_visibility::tests::{PERSONAL, SHARED, home};

fn proof(verified_at: i64) -> MigrationConfirmationSession {
    MigrationConfirmationSession { verified_at: verified_at as u64, csrf: [7; 32] }
}

#[test]
fn p1_admin_read_requires_step_up_and_audits() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    let Ok(Reader::BreakGlass { owner }) =
        visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL) else {
        panic!("an Admin who is not the owner needs break-glass");
    };
    assert_eq!(owner, h.alice);
    let target = Target { method: "memory.search", category: DataCategory::Memory, agent: PERSONAL };

    // Refused without a fresh, session-bound step-up and its CSRF token.
    let refusals: [(Option<MigrationConfirmationSession>, Option<[u8; 32]>); 5] = [
        (None, Some([7; 32])),            // no step-up proof
        (Some(proof(now)), None),         // proof but no token in the request
        (Some(proof(now)), Some([8; 32])), // wrong token
        (Some(proof(now - 301)), Some([7; 32])), // older than five minutes
        (Some(proof(now + 1)), Some([7; 32])),   // from the future
    ];
    for (proof, csrf) in &refusals {
        assert_eq!(
            authorize_break_glass(&h.db, proof.as_ref(), &h.admin, csrf.as_ref(), now, &target, &owner),
            Err(STEP_UP_REQUIRED)
        );
    }
    assert!(h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap().is_empty(),
        "nothing was read, so no read row");
    assert_eq!(h.db.query_audit_log(Some(&h.admin), Some(REFUSED_ACTION), 50).unwrap().len(), refusals.len());

    // Fresh step-up + token: allowed, and the audit row names the Admin, the
    // employee, the owner and the data category.
    assert_eq!(
        authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, Some(&[7; 32]), now, &target, &owner),
        Ok(())
    );
    let rows = h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target.as_deref(), Some(PERSONAL));
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["admin_user_id"], json!(h.admin));
    assert_eq!(detail["employee"], json!(PERSONAL));
    assert_eq!(detail["owner_user_id"], json!(h.alice));
    assert_eq!(detail["category"], json!("memory"));
    assert_eq!(detail["method"], json!("memory.search"));

    // The owner and Shared employees never need a step-up; the owner's
    // manager is refused outright.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    assert_eq!(visibility::classify_reader(h.path(), true, &h.admin, true, SHARED), Ok(Reader::Unrestricted));
    assert_eq!(visibility::classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
}

#[test]
fn p1_gated_methods_and_step_up_field() {
    use DataCategory::*;
    for (method, category) in [
        ("memory.search", Memory), ("memory.browse", Memory), ("memory.graph", Memory), ("memory.forget", Memory),
        ("wiki.pages", Wiki), ("wiki.read", Wiki), ("wiki.search", Wiki),
        ("agent_files.list", AgentFiles), ("agent_files.get", AgentFiles), ("agent_files.history", AgentFiles),
        ("agents.inspect", AgentFiles), ("contract.get", AgentFiles),
        ("chat.sessions.list", Sessions), ("search.query", Sessions), ("runs.list", Sessions),
    ] {
        assert_eq!(agent_scoped(method), Some(category), "{method}");
    }
    for method in ["agents.list", "shared_wiki.read", "system.status", "chat.sessions.history"] {
        assert_eq!(agent_scoped(method), None, "{method}");
    }
    // The token is taken out of the params; a handler never sees it.
    assert_eq!(STEP_UP_FIELD, "step_up_csrf");
    let mut params = json!({"agent_id": PERSONAL, "step_up_csrf": serde_json::to_value([7u8; 32]).unwrap()});
    assert_eq!(take_step_up(&mut params), Some([7; 32]));
    assert!(params.get(STEP_UP_FIELD).is_none());
    assert_eq!(params["agent_id"], json!(PERSONAL));
    let mut malformed = json!({"step_up_csrf": "not-a-token"});
    assert_eq!(take_step_up(&mut malformed), None);
    assert!(malformed.get(STEP_UP_FIELD).is_none());
}
