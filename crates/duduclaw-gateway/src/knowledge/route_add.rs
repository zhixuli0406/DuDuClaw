//! `add_source` on the internal knowledge route (the `knowledge_add_source`
//! MCP tool): an employee proposes a document for one of the collections on
//! its own side; a person decides in the dashboard
//! ([`super::add_source_approval`]). Nothing is ingested here.

use std::path::Path;

use axum::http::StatusCode;
use duduclaw_knowledge::acl::{agent_side_collections, ids_for_slugs};
use duduclaw_knowledge::requester::agent_ctx;
use duduclaw_knowledge::{KbError, schema};
use serde_json::{Value, json};

use super::add_source_approval::{self as approval, Content, Proposal, Proposed};
use super::route::{Refusal, Request, agent_dir};

/// Longest proposed text (characters) through the tool.
pub const MAX_TOOL_TEXT_CHARS: usize = 200_000;

fn bad(msg: &str) -> Refusal {
    Refusal(StatusCode::BAD_REQUEST, "bad_request", msg.to_string())
}

fn from_kb(e: KbError) -> Refusal {
    match e {
        KbError::PermissionDenied => Refusal(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "permission denied".into(),
        ),
        KbError::Invalid(m) => bad(&m),
        KbError::Rejected { code, message } => Refusal(StatusCode::BAD_REQUEST, code, message),
        other => {
            tracing::error!(error = %other, "knowledge add_source failed");
            Refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "error",
                other.public_message(),
            )
        }
    }
}

/// The collection id for `slug` if it is on the employee's own side.
fn collection_on_agent_side(home: &Path, agent: &str, slug: &str) -> Result<String, Refusal> {
    let (scope, department) =
        duduclaw_core::knowledge_agent::load_agent_knowledge(&agent_dir(home, agent));
    if scope.is_off() {
        return Err(Refusal(
            StatusCode::FORBIDDEN,
            "knowledge_off",
            "這位 AI 員工沒有啟用文件知識庫。".into(),
        ));
    }
    let conn = schema::open_home(home).map_err(from_kb)?;
    let side = agent_side_collections(&conn, &agent_ctx(agent, department.as_deref(), &scope))
        .map_err(from_kb)?;
    let (ids, _) = ids_for_slugs(&conn, &[slug.to_string()]).map_err(from_kb)?;
    ids.into_iter().find(|id| side.contains(id)).ok_or_else(|| {
        Refusal(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "找不到這個知識集合，或這位 AI 員工沒有使用它的權限。".into(),
        )
    })
}

/// Validate and file one proposal.
pub(crate) async fn serve_add_source(
    home: &Path,
    agent: &str,
    request: Request,
) -> Result<Value, Refusal> {
    let Request::AddSource {
        collection,
        path,
        url,
        text,
        title,
        ..
    } = request
    else {
        return Err(bad("知識庫請求格式不正確。"));
    };
    let given = [path.is_some(), url.is_some(), text.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if given != 1 {
        return Err(bad("path、url、text 三者必須剛好提供一個。"));
    }
    let slug = collection.trim().to_string();
    if slug.is_empty() {
        return Err(bad("缺少參數 collection（集合代號）。"));
    }
    let (h, a) = (home.to_path_buf(), agent.to_string());
    let content_path = path.clone();
    let (collection_id, content) = super::blocking(move || {
        Ok(collection_on_agent_side(&h, &a, &slug).map(|cid| {
            let content = content_path.map(|p| {
                approval::attachment_path_allowed(&h, &agent_dir(&h, &a), Path::new(&p)).map(
                    |real| {
                        let name = real
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("file")
                            .to_string();
                        Content::File { path: real, name }
                    },
                )
            });
            (cid, content)
        }))
    })
    .await
    .map_err(from_kb)??;
    let content = match (content, url, text) {
        (Some(Some(c)), _, _) => c,
        (Some(None), _, _) => {
            return Err(bad(
                "path 必須是員工自己的 attachments/ 或共用 attachments/ 目錄裡的檔案。",
            ));
        }
        (None, Some(u), _) => Content::Url(u),
        (None, None, Some(t)) => {
            if t.chars().count() > MAX_TOOL_TEXT_CHARS {
                return Err(bad("text 太長（上限 20 萬字），請改用檔案。"));
            }
            Content::Text(t)
        }
        (None, None, None) => return Err(bad("path、url、text 三者必須剛好提供一個。")),
    };
    let proposal = Proposal {
        agent_id: agent.to_string(),
        collection_id,
        title: title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        content,
        proposed_by: format!("agent:{agent}"),
        via: "mcp",
        origin: None,
    };
    match approval::propose(home, proposal).await.map_err(from_kb)? {
        Proposed::Filed { approval_id, title } => Ok(json!({
            "ok": true,
            "status": "awaiting_approval",
            "approval_id": approval_id,
            "title": title,
        })),
        Proposed::Duplicate { title } => Ok(json!({
            "ok": true,
            "status": "duplicate",
            "title": title,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::test_fixtures::{agent, collection};
    use duduclaw_knowledge::acl::{PrincipalKind, Role};

    fn add(collection: &str, path: Option<String>, text: Option<&str>) -> Request {
        Request::AddSource {
            collection: collection.into(),
            path,
            url: None,
            text: text.map(str::to_string),
            title: None,
            turn_id: None,
            run_id: None,
        }
    }

    #[tokio::test]
    async fn proposals_need_the_collection_on_the_employee_side() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        agent(h, "hr-bot", "[agent]\nname='hr-bot'\n");
        agent(
            h,
            "off-bot",
            "[agent]\nname='off-bot'\n[knowledge]\ncollections=[]\n",
        );
        let (cid, _, _) = collection(
            h,
            "hr",
            "internal",
            &[(PrincipalKind::Agent, "hr-bot", Role::Viewer)],
            "既有。",
        );
        collection(h, "other", "internal", &[], "別的。");
        let v = serve_add_source(h, "hr-bot", add("hr", None, Some("新規定")))
            .await
            .unwrap();
        assert_eq!(v["status"], "awaiting_approval");
        // Nothing was ingested: the card waits.
        let conn = schema::open_home(h).unwrap();
        assert_eq!(
            duduclaw_knowledge::sources::list_sources(&conn, &cid)
                .unwrap()
                .len(),
            1
        );
        let broker = crate::approval::ApprovalBroker::open(h).unwrap();
        assert_eq!(
            broker
                .list_by_kind(approval::ACTION_KIND)
                .await
                .unwrap()
                .len(),
            1
        );
        let denied = serve_add_source(h, "hr-bot", add("other", None, Some("x"))).await;
        assert!(matches!(
            denied,
            Err(Refusal(StatusCode::FORBIDDEN, "permission_denied", _))
        ));
        let off = serve_add_source(h, "off-bot", add("hr", None, Some("x"))).await;
        assert!(matches!(off, Err(Refusal(_, "knowledge_off", _))));
        let outside = h.join("notes.txt");
        std::fs::write(&outside, "x").unwrap();
        let bad_path = serve_add_source(
            h,
            "hr-bot",
            add("hr", Some(outside.display().to_string()), None),
        )
        .await;
        assert!(matches!(
            bad_path,
            Err(Refusal(StatusCode::BAD_REQUEST, _, _))
        ));
        let two = Request::AddSource {
            collection: "hr".into(),
            path: None,
            url: Some("https://example.com".into()),
            text: Some("x".into()),
            title: None,
            turn_id: None,
            run_id: None,
        };
        assert!(serve_add_source(h, "hr-bot", two).await.is_err());
    }
}
