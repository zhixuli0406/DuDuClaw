//! Knowledge base dashboard RPCs that need the live registry, end to end
//! through `MethodHandler::handle`: `knowledge.agent.collections`,
//! `agents.update {knowledge}`, `knowledge.principals.search`, the grant
//! check in front of `knowledge.acl.set`, and `knowledge.access_log.denied`.
use super::*;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn admin() -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = "admin-1".into();
    c
}

fn user(id: &str, agents: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        user_id: id.into(),
        email: format!("{id}@example.com"),
        role: duduclaw_auth::models::UserRole::Employee,
        agent_access: agents.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
    }
}

/// `agents.update` checks the caller's Owner binding live against
/// `users.db` (agent-file editing), not the context snapshot, so a test
/// owner must exist there with the same id and binding.
fn register_live_owner(home: &std::path::Path, id: &str, agents: &[(&str, AccessLevel)]) {
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let u = db
        .create_user(
            &format!("{id}@example.com"),
            id,
            "isolated-test-password",
            duduclaw_auth::models::UserRole::Employee,
        )
        .unwrap();
    rusqlite::Connection::open(home.join("users.db"))
        .unwrap()
        .execute("UPDATE users SET id=?1 WHERE id=?2", rusqlite::params![id, u.id])
        .unwrap();
    for (a, l) in agents {
        db.bind_agent(id, a, *l).unwrap();
    }
}

struct Fx {
    home: tempfile::TempDir,
    handler: MethodHandler,
    hr: String,
}

/// Agents `a1` and `a2`; collections `hr` (granted to `a1`) and `faq`
/// (granted to nobody).
async fn fixture() -> Fx {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for name in ["a1", "a2"] {
        ok(&handler
            .handle_agents_create_as(
                json!({ "name": name, "display_name": name.to_uppercase() }),
                Some(&admin()),
            )
            .await);
    }
    let mut ids = Vec::new();
    for slug in ["hr", "faq"] {
        let c = ok(&handler
            .handle(
                "knowledge.collections.create",
                json!({ "name": slug, "slug": slug }),
                &admin(),
            )
            .await);
        ids.push(c["collection"]["id"].as_str().unwrap().to_string());
    }
    ok(&handler
        .handle(
            "knowledge.acl.set",
            json!({ "collection_id": ids[0], "expected_revision": 0, "entries": [
                { "kind": "user", "id": "admin-1", "role": "owner" },
                { "kind": "agent", "id": "a1", "role": "viewer" },
            ]}),
            &admin(),
        )
        .await);
    Fx {
        home,
        handler,
        hr: ids[0].clone(),
    }
}

fn agent_toml(home: &Path, agent: &str) -> toml::Table {
    std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn agent_collections_lists_grants_for_owners_only() {
    let f = fixture().await;
    let owner = user("owner-7", &[("a1", AccessLevel::Owner)]);
    let r = ok(&f
        .handler
        .handle(
            "knowledge.agent.collections",
            json!({ "agent_id": "a1" }),
            &owner,
        )
        .await);
    let slugs: Vec<&str> = r["granted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["hr"]);
    assert_eq!(r["bound"], Value::Null);
    assert_eq!(r["unreadable"], false);

    let operator = user("op-1", &[("a1", AccessLevel::Operator)]);
    let e = err(&f
        .handler
        .handle(
            "knowledge.agent.collections",
            json!({ "agent_id": "a1" }),
            &operator,
        )
        .await);
    assert_eq!(e, "permission denied");
    let e = err(&f
        .handler
        .handle(
            "knowledge.agent.collections",
            json!({ "agent_id": "zz" }),
            &admin(),
        )
        .await);
    assert_eq!(e, "permission denied");
}

#[tokio::test]
async fn agents_update_binds_only_granted_collections() {
    let f = fixture().await;
    register_live_owner(f.home.path(), "owner-7", &[("a1", AccessLevel::Owner)]);
    let owner = user("owner-7", &[("a1", AccessLevel::Owner)]);
    ok(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": ["hr"] } }),
            &owner,
        )
        .await);
    let t = agent_toml(f.home.path(), "a1");
    assert_eq!(
        t["knowledge"]["collections"],
        toml::Value::Array(vec![toml::Value::String("hr".into())])
    );
    let r = ok(&f
        .handler
        .handle(
            "knowledge.agent.collections",
            json!({ "agent_id": "a1" }),
            &owner,
        )
        .await);
    assert_eq!(r["bound"], json!(["hr"]));

    // A collection not granted to the employee is refused, nothing written.
    let e = err(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": ["hr", "faq"] } }),
            &owner,
        )
        .await);
    assert!(e.contains("faq"), "{e}");
    let e = err(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": ["Bad Slug"] } }),
            &owner,
        )
        .await);
    assert!(e.contains("格式"), "{e}");
    assert_eq!(
        agent_toml(f.home.path(), "a1")["knowledge"]["collections"],
        toml::Value::Array(vec![toml::Value::String("hr".into())])
    );

    // [] turns knowledge off; null removes the key again.
    ok(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": [] } }),
            &owner,
        )
        .await);
    assert_eq!(
        agent_toml(f.home.path(), "a1")["knowledge"]["collections"],
        toml::Value::Array(vec![])
    );
    ok(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": null } }),
            &owner,
        )
        .await);
    assert!(agent_toml(f.home.path(), "a1").get("knowledge").is_none());

    // Only the employee's owner (or an admin) may write it.
    let other = user("someone", &[("a2", AccessLevel::Owner)]);
    let e = err(&f
        .handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "knowledge": { "collections": [] } }),
            &other,
        )
        .await);
    assert!(!e.is_empty());
    // Malformed shapes are refused.
    for bad in [
        json!("hr"),
        json!({}),
        json!({ "collections": "hr" }),
        json!({ "collections": [1] }),
    ] {
        let e = err(&f
            .handler
            .handle(
                "agents.update",
                json!({ "agent_id": "a1", "knowledge": bad }),
                &owner,
            )
            .await);
        assert!(!e.is_empty());
    }
}

#[tokio::test]
async fn grant_check_runs_before_acl_set_through_dispatch() {
    let f = fixture().await;
    let mut owner = user("owner-7", &[("a1", AccessLevel::Owner)]);
    owner.role = duduclaw_auth::models::UserRole::Manager;
    let c = ok(&f
        .handler
        .handle(
            "knowledge.collections.create",
            json!({ "name": "Team notes", "slug": "team" }),
            &owner,
        )
        .await);
    let team = c["collection"]["id"].as_str().unwrap().to_string();
    let r = ok(&f
        .handler
        .handle(
            "knowledge.principals.search",
            json!({ "collection_id": team }),
            &owner,
        )
        .await);
    let agents: Vec<&str> = r["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        agents,
        vec!["a1"],
        "only the owner's own employees are offered"
    );
    // Someone else's collection: no picker.
    let e = err(&f
        .handler
        .handle(
            "knowledge.principals.search",
            json!({ "collection_id": f.hr }),
            &owner,
        )
        .await);
    assert_eq!(e, "permission denied");

    // a2 is not the owner's employee: the write is refused too.
    let e = err(&f
        .handler
        .handle(
            "knowledge.acl.set",
            json!({ "collection_id": team, "expected_revision": 0, "entries": [
                { "kind": "user", "id": "owner-7", "role": "owner" },
                { "kind": "agent", "id": "a2", "role": "viewer" },
            ]}),
            &owner,
        )
        .await);
    assert!(e.contains("a2"), "{e}");
    // Own employee plus a chat channel is fine.
    ok(&f
        .handler
        .handle(
            "knowledge.acl.set",
            json!({ "collection_id": team, "expected_revision": 0, "entries": [
                { "kind": "user", "id": "owner-7", "role": "owner" },
                { "kind": "agent", "id": "a1", "role": "viewer" },
                { "kind": "channel", "id": "line", "role": "viewer" },
            ]}),
            &owner,
        )
        .await);
    // Without an account database no new person can be granted.
    let e = err(&f
        .handler
        .handle(
            "knowledge.acl.set",
            json!({ "collection_id": team, "expected_revision": 1, "entries": [
                { "kind": "user", "id": "owner-7", "role": "owner" },
                { "kind": "user", "id": "ghost", "role": "viewer" },
            ]}),
            &owner,
        )
        .await);
    assert!(e.contains("ghost") || e.contains("個人版"), "{e}");
}

#[tokio::test]
async fn denied_reads_are_admin_only() {
    let f = fixture().await;
    let stranger = user("stranger", &[]);
    // A refused passage read lands in the access log as `denied`.
    let _ = err(&f
        .handler
        .handle("knowledge.passage.get", json!({ "chunk_id": 1 }), &stranger)
        .await);
    let e = err(&f
        .handler
        .handle("knowledge.access_log.denied", json!({}), &stranger)
        .await);
    assert_eq!(e, "permission denied");
    let r = ok(&f
        .handler
        .handle("knowledge.access_log.denied", json!({}), &admin())
        .await);
    let rows = r["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["op"], "get_passage");
    assert_eq!(rows[0]["requester_id"], "stranger");
}

#[tokio::test]
async fn viewer_access_to_an_employee_cannot_share_with_it() {
    let f = fixture().await;
    let mut viewer = user("view-1", &[("a1", AccessLevel::Viewer)]);
    viewer.role = duduclaw_auth::models::UserRole::Manager;
    let c = ok(&f
        .handler
        .handle(
            "knowledge.collections.create",
            json!({ "name": "Mine", "slug": "mine" }),
            &viewer,
        )
        .await);
    let mine = c["collection"]["id"].as_str().unwrap().to_string();
    let r = ok(&f
        .handler
        .handle(
            "knowledge.principals.search",
            json!({ "collection_id": mine }),
            &viewer,
        )
        .await);
    assert!(r["agents"].as_array().unwrap().is_empty());
    let e = err(&f
        .handler
        .handle(
            "knowledge.acl.set",
            json!({ "collection_id": mine, "expected_revision": 0, "entries": [
                { "kind": "user", "id": "view-1", "role": "owner" },
                { "kind": "agent", "id": "a1", "role": "viewer" },
            ]}),
            &viewer,
        )
        .await);
    assert!(e.contains("a1"), "{e}");
}

#[tokio::test]
async fn limits_and_trimmed_agent_collections() {
    let f = fixture().await;
    let someone = user("anyone", &[]);
    let r = ok(&f
        .handler
        .handle("knowledge.limits", json!({}), &someone)
        .await);
    assert!(r["max_source_bytes"].as_u64().unwrap() > 0);
    assert!(
        r["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "pdf")
    );
    let r = ok(&f
        .handler
        .handle(
            "knowledge.agent.collections",
            json!({ "agent_id": "a1" }),
            &admin(),
        )
        .await);
    let keys: Vec<&String> = r["granted"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["id", "name", "sensitivity", "slug"]);
}
