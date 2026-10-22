//! Dashboard RPCs added for channel replies:
//!
//! | method | who |
//! |---|---|
//! | `citations.for_session` | the owner of the WebChat conversation (Admin: any) |
//! | `sources.delete` with `also_forget_memories: true` | editor (the forget requests it files are decided by an Admin) |
//! | `proposals.list` / `proposals.decide` | signed in; only cards the caller may decide (Admin, or editor / owner of the card's collection, re-read now) |
//!
//! `proposals.*` exist because the approvals inbox (`approvals.list` /
//! `approvals.decide`) is Manager-and-up, while an Employee can be a
//! collection's editor; it decides the same `knowledge_add_source` cards
//! through the same [`super::add_source_approval::decide`].
//!
//! Stored citations are re-checked against what the caller may read now:
//! a deleted source keeps its title and is marked deleted; a source the
//! caller may no longer read shows neither title nor excerpt.

use std::path::Path;

use duduclaw_knowledge::acl::{self, Narrowing, Role};
use duduclaw_knowledge::citations::{cited_in_session, shown_for_viewer};
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::{KbError, KbResult, schema};
use serde_json::{Value, json};

use super::{blocking, dashboard_human};

/// Most stored citations returned for one conversation.
pub const MAX_SESSION_CITATIONS: usize = 500;

/// Whether `user` may read citations of `session_id`.
pub fn may_view_session(user: &ResolvedUser, session_id: &str) -> bool {
    user.is_admin
        || crate::webchat::owns_webchat_session(
            session_id,
            &crate::webchat::webchat_owner_tag(&user.user_id),
        )
}

/// `knowledge.citations.for_session {session_id}`.
pub async fn citations_for_session(
    home: &Path,
    params: &Value,
    user: ResolvedUser,
) -> KbResult<Value> {
    let session = params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .ok_or_else(|| KbError::Invalid("缺少參數 session_id。".to_string()))?
        .to_string();
    if !may_view_session(&user, &session) {
        return Err(KbError::PermissionDenied);
    }
    let home = home.to_path_buf();
    blocking(move || {
        if !schema::db_path(&home).exists() {
            return Ok(json!({ "citations": [] }));
        }
        let conn = schema::open_home(&home)?;
        let mut rows = cited_in_session(&conn, &session, MAX_SESSION_CITATIONS)?;
        // A called employee's turns run in `<session>#agent:<id>`.
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT session_id FROM turn_citations
              WHERE substr(session_id, 1, length(?1) + 7) = ?1 || '#agent:'",
        )?;
        let called: Vec<String> = stmt
            .query_map([&session], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for s in called {
            rows.extend(cited_in_session(&conn, &s, MAX_SESSION_CITATIONS)?);
        }
        rows.truncate(MAX_SESSION_CITATIONS);
        let human = dashboard_human(&user);
        let allowed = acl::effective_collections(&conn, &human, None, &Narrowing::default())?;
        let shown = shown_for_viewer(&conn, rows, &allowed)?;
        Ok(json!({ "citations": shown }))
    })
    .await
}

/// `knowledge.proposals.list`: pending add-source cards the caller may
/// decide, with who asked and the text preview (binary files: name, type
/// and size only). The waiting files themselves never leave the gateway.
pub async fn proposals_list(home: &Path, user: ResolvedUser) -> KbResult<Value> {
    super::add_source_guard::sweep_unreferenced(home).await;
    use super::add_source_approval::{ACTION_KIND, may_decide};
    let broker = crate::approval::ApprovalBroker::open(home)
        .map_err(|e| KbError::Io(std::io::Error::other(e)))?;
    let now = chrono::Utc::now();
    let pending: Vec<_> = broker
        .list_by_kind(ACTION_KIND)
        .await
        .map_err(|e| KbError::Io(std::io::Error::other(e)))?
        .into_iter()
        .filter(|r| r.status == crate::approval::ApprovalStatus::Pending && !r.is_stale(now))
        .collect();
    let home = home.to_path_buf();
    blocking(move || {
        let items: Vec<Value> = pending
            .iter()
            .filter(|r| may_decide(&home, r, &user).is_ok())
            .map(|r| {
                let p = &r.payload;
                json!({
                    "id": r.id.to_string(),
                    "agent_id": r.agent_id,
                    "created_at": r.created_at,
                    "ttl_seconds": r.ttl_seconds,
                    "summary": r.summary,
                    "collection_id": p["collection_id"],
                    "collection_name": p["collection_name"],
                    "title": p["title"],
                    "kind": p["kind"],
                    "format": p["format"],
                    "bytes": p["bytes"],
                    "url": p["url"],
                    "via": p["via"],
                    "requester_kind": p["requester_kind"],
                    "requester_channel": p["requester_channel"],
                    "preview": p["preview"],
                    "proposed_by": p["proposed_by"],
                })
            })
            .collect();
        Ok(json!({ "proposals": items }))
    })
    .await
}

/// `knowledge.proposals.decide {id, approve}`.
pub async fn proposals_decide(
    home: &Path,
    params: &Value,
    user: ResolvedUser,
) -> Result<Value, String> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("缺少參數 id。")?;
    let approve = params
        .get("approve")
        .and_then(Value::as_bool)
        .ok_or("缺少參數 approve。")?;
    let broker = crate::approval::ApprovalBroker::open(home)?;
    super::add_source_approval::decide(
        home,
        &broker,
        &crate::approval::ApprovalId::from(id.to_string()),
        approve,
        &user,
    )
    .await
}

/// The title of a source the caller may edit (before deleting it).
pub async fn editable_source_title(
    home: &Path,
    source_id: &str,
    user: &ResolvedUser,
) -> KbResult<String> {
    let (home, sid, user) = (home.to_path_buf(), source_id.to_string(), user.clone());
    blocking(move || {
        let conn = schema::open_home(&home)?;
        let src = duduclaw_knowledge::sources::get_source(&conn, &sid)?
            .ok_or(KbError::PermissionDenied)?;
        acl::require_role(
            &conn,
            &src.collection_id,
            &user.user_id,
            user.department.as_deref(),
            user.is_admin,
            Role::Editor,
        )?;
        Ok(src.title)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, admin: bool) -> ResolvedUser {
        ResolvedUser {
            user_id: id.into(),
            department: None,
            is_admin: admin,
        }
    }

    #[test]
    fn only_the_conversation_owner_or_admin_may_view() {
        let tag = crate::webchat::webchat_owner_tag("u1");
        let sid = format!("webchat:webchat:{tag}:abc");
        assert!(may_view_session(&user("u1", false), &sid));
        assert!(!may_view_session(&user("u2", false), &sid));
        assert!(may_view_session(&user("u2", true), &sid));
        assert!(!may_view_session(&user("u1", false), "line:U1"));
    }

    #[tokio::test]
    async fn citations_are_rechecked_for_the_viewer() {
        use crate::knowledge::test_fixtures::collection;
        use duduclaw_knowledge::acl::PrincipalKind;
        use duduclaw_knowledge::citations::{TurnCitation, record_turn};
        let home = tempfile::tempdir().unwrap();
        let (_, src, chunk) = collection(
            home.path(),
            "hr",
            "internal",
            &[(PrincipalKind::User, "u1", Role::Viewer)],
            "特休假七日。",
        );
        let tag = crate::webchat::webchat_owner_tag("u1");
        let sid = format!("webchat:webchat:{tag}:abc");
        let row = |turn: &str, session: &str| TurnCitation {
            turn_id: turn.into(),
            agent_id: "bot".into(),
            session_id: session.into(),
            user_message_key: None,
            assistant_message_id: Some(2),
            n: 1,
            chunk_id: Some(chunk),
            source_id: src.clone(),
            title: "hr.txt".into(),
            locator: "第 1 段".into(),
            excerpt: "特休假七日".into(),
            cited: true,
            at: duduclaw_knowledge::now_rfc3339(),
        };
        let mut conn = schema::open_home(home.path()).unwrap();
        record_turn(
            &mut conn,
            &[row("t1", &sid), row("t2", &format!("{sid}#agent:other"))],
        )
        .unwrap();
        let v = citations_for_session(home.path(), &json!({"session_id": sid}), user("u1", false))
            .await
            .unwrap();
        let list = v["citations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["state"], "available");
        assert_eq!(list[0]["chunk_id"], chunk);
        // Someone else cannot read this conversation at all.
        assert!(matches!(
            citations_for_session(home.path(), &json!({"session_id": sid}), user("u2", false))
                .await,
            Err(KbError::PermissionDenied)
        ));
        // An Admin reads every conversation and every collection.
        let v = citations_for_session(home.path(), &json!({"session_id": sid}), user("adm", true))
            .await
            .unwrap();
        assert_eq!(v["citations"][0]["state"], "available");
    }

    #[tokio::test]
    async fn employee_editors_list_and_decide_proposals() {
        use crate::knowledge::add_source_approval::{Content, NOT_EDITOR, Proposal, propose};
        use crate::knowledge::test_fixtures::{agent, collection};
        use duduclaw_knowledge::acl::PrincipalKind;
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        agent(h, "bot", "[agent]\nname='bot'\n");
        let (cid, _, _) = collection(
            h,
            "hr",
            "internal",
            &[
                (PrincipalKind::User, "ed", Role::Editor),
                (PrincipalKind::User, "vw", Role::Viewer),
            ],
            "既有。",
        );
        propose(
            h,
            Proposal {
                agent_id: "bot".into(),
                collection_id: cid,
                title: Some("規定".into()),
                content: Content::Text("新的規定內容".into()),
                proposed_by: "agent:bot".into(),
                via: "mcp",
                requester_kind: "system".into(),
                requester_channel: None,
                origin: None,
            },
        )
        .await
        .unwrap();
        let listed = proposals_list(h, user("ed", false)).await.unwrap();
        let list = listed["proposals"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["title"], "規定");
        assert_eq!(
            list[0]["preview"], "新的規定內容",
            "the approver reads what they approve"
        );
        assert_eq!(list[0]["requester_kind"], "system");
        assert!(
            proposals_list(h, user("vw", false)).await.unwrap()["proposals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = list[0]["id"].as_str().unwrap().to_string();
        let err = proposals_decide(h, &json!({"id": id, "approve": true}), user("vw", false))
            .await
            .unwrap_err();
        assert_eq!(err, NOT_EDITOR);
        let ok = proposals_decide(h, &json!({"id": id, "approve": true}), user("ed", false))
            .await
            .unwrap();
        assert_eq!(ok["decided"], "approved");
        assert!(
            proposals_decide(h, &json!({"id": id}), user("ed", false))
                .await
                .is_err()
        );
    }
}
