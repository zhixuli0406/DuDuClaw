//! The employee page's 「資料來源」 tab, end to end through
//! `MethodHandler::handle`: `knowledge.agent.data_sources`,
//! `knowledge.agent.own.ensure`, and the own collection following the
//! employee through archive / unarchive / soft delete.
use super::*;

use duduclaw_auth::jwt::JwtConfig;
use duduclaw_auth::models::UserRole;

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

struct Fx {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    admin: UserContext,
}

/// An account in users.db with a fixed id, its bindings, and the session
/// context the dashboard would hold for it.
fn person(
    db: &UserDb,
    home: &Path,
    id: &str,
    role: UserRole,
    agents: &[(&str, AccessLevel)],
) -> UserContext {
    let u = db
        .create_user(
            &format!("{id}@test.invalid"),
            &format!("名字-{id}"),
            "isolated-test-password",
            role,
        )
        .unwrap();
    rusqlite::Connection::open(home.join("users.db"))
        .unwrap()
        .execute(
            "UPDATE users SET id=?1 WHERE id=?2",
            rusqlite::params![id, u.id],
        )
        .unwrap();
    for (a, l) in agents {
        db.bind_agent(id, a, *l).unwrap();
    }
    UserContext {
        credential_epoch: 0,
        user_id: id.into(),
        email: format!("{id}@test.invalid"),
        role,
        agent_access: agents.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

async fn fixture() -> Fx {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    handler
        .set_user_db(
            db.clone(),
            Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!")),
        )
        .await;
    handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    for name in ["a1", "a2"] {
        ok(&handler
            .handle_agents_create_as(
                json!({ "name": name, "display_name": name.to_uppercase() }),
                Some(&UserContext::admin_fallback()),
            )
            .await);
    }
    let admin = person(&db, home.path(), "admin-1", UserRole::Admin, &[]);
    Fx {
        home,
        handler,
        db,
        admin,
    }
}

impl Fx {
    fn person(&self, id: &str, agents: &[(&str, AccessLevel)]) -> UserContext {
        person(&self.db, self.home.path(), id, UserRole::Employee, agents)
    }

    async fn call(&self, method: &str, params: Value, who: &UserContext) -> WsFrame {
        self.handler.handle(method, params, who).await
    }

    fn grants(&self, collection_id: &str) -> Vec<(String, String, String)> {
        let conn = duduclaw_knowledge::schema::open_home(self.home.path()).unwrap();
        duduclaw_knowledge::acl::get_acl(&conn, collection_id)
            .unwrap()
            .0
            .into_iter()
            .map(|e| {
                (
                    e.kind.as_str().to_string(),
                    e.id,
                    e.role.as_str().to_string(),
                )
            })
            .collect()
    }
}

fn agent_toml(home: &Path, agent: &str) -> toml::Table {
    std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn owner_creates_the_own_collection_with_default_readers() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    let op = f.person("op-1", &[("a1", AccessLevel::Operator)]);
    let viewer = f.person("view-1", &[("a1", AccessLevel::Viewer)]);
    let stranger = f.person("stranger", &[("a2", AccessLevel::Owner)]);

    // Before: viewers see the tab, nobody but owners may create.
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &viewer,
        )
        .await);
    assert_eq!(r["own"], Value::Null);
    assert_eq!(r["can_manage"], false);
    assert_eq!(r["manage_blocked"], "not_owner");
    for who in [&viewer, &op, &stranger] {
        let e = err(&f
            .call("knowledge.agent.own.ensure", json!({"agent_id": "a1"}), who)
            .await);
        assert_eq!(e, "permission denied");
    }
    let e = err(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &stranger,
        )
        .await);
    assert_eq!(e, "permission denied");

    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(r["can_manage"], true);
    let defaults: Vec<&str> = r["default_readers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(defaults, vec!["op-1", "view-1"]);

    let made = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1", "share_chat_users": true}),
            &owner,
        )
        .await);
    assert_eq!(made["created"], true);
    assert_eq!(made["binding_changed"], false);
    assert_eq!(made["collection"]["name"], "A1 的資料");
    assert_eq!(made["collection"]["sensitivity"], "internal");
    assert_eq!(made["collection"]["my_role"], "owner");
    let id = made["collection"]["id"].as_str().unwrap().to_string();
    let g = f.grants(&id);
    for want in [
        ("user", "owner-7", "owner"),
        ("agent", "a1", "viewer"),
        ("user", "op-1", "viewer"),
        ("user", "view-1", "viewer"),
    ] {
        assert!(
            g.contains(&(want.0.into(), want.1.into(), want.2.into())),
            "{g:?}"
        );
    }
    assert_eq!(g.len(), 4);

    // Same collection the second time; the library marks whose it is.
    let again = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(again["created"], false);
    assert_eq!(again["collection"]["id"], id.as_str());
    let lib = ok(&f
        .call("knowledge.collections.list", json!({}), &owner)
        .await);
    let row = lib["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        row["own_agent"],
        json!({"agent_id": "a1", "archived": false})
    );

    // An operator reads the files but may not change them.
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &op,
        )
        .await);
    assert!(r["own"]["sources"].is_array());
    assert_eq!(r["manage_blocked"], "not_owner");
    let listed = r["collections"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["is_own"], true);
    assert_eq!(listed[0]["in_use"], true);
    // A viewer of the collection gets counts, not names.
    assert!(listed[0]["readers"].is_null());
    assert_eq!(listed[0]["reader_counts"]["users"], 3);
    assert_eq!(
        listed[0]["reader_counts"]["agents"], 0,
        "the employee itself is not counted"
    );
    assert_eq!(
        r["default_readers"],
        json!([]),
        "only owners see who a new collection would go to"
    );
    // The collection's owner gets names.
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    let labels: Vec<&str> = r["own"]["collection"]["readers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"名字-op-1"), "{labels:?}");
    assert!(
        !labels.contains(&"A1"),
        "the employee itself is not listed: {labels:?}"
    );

    // The audit log names the new collection.
    let audit =
        std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(
        audit.contains("knowledge_collection_created") && audit.contains("own_collection"),
        "{audit}"
    );
}

#[tokio::test]
async fn ensure_adds_the_slug_to_an_explicit_binding_and_respects_choices() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    f.person("op-1", &[("a1", AccessLevel::Operator)]);
    // Knowledge switched off for a1: the upload turns on just this collection.
    ok(&f
        .call(
            "agents.update",
            json!({"agent_id": "a1", "knowledge": {"collections": []}}),
            &owner,
        )
        .await);
    let made = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1", "sensitivity": "personal", "share_chat_users": false}),
            &owner,
        )
        .await);
    assert_eq!(made["binding_changed"], true);
    let slug = made["collection"]["slug"].as_str().unwrap().to_string();
    assert_eq!(made["collection"]["sensitivity"], "personal");
    assert_eq!(
        agent_toml(f.home.path(), "a1")["knowledge"]["collections"],
        toml::Value::Array(vec![toml::Value::String(slug.clone())])
    );
    let id = made["collection"]["id"].as_str().unwrap();
    assert_eq!(
        f.grants(id).len(),
        2,
        "creator and employee only: {:?}",
        f.grants(id)
    );
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(r["binding"], "only");
    // An owner who takes it out of the list again is not overruled by the
    // next upload.
    ok(&f
        .call(
            "agents.update",
            json!({"agent_id": "a1", "knowledge": {"collections": []}}),
            &owner,
        )
        .await);
    let again = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(again["created"], false);
    assert_eq!(again["binding_changed"], false);
    assert_eq!(
        agent_toml(f.home.path(), "a1")["knowledge"]["collections"],
        toml::Value::Array(vec![])
    );
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(r["own"]["collection"]["in_use"], false);
    // Bad sensitivity: refused, nothing for a2.
    let owner2 = f.person("owner-8", &[("a2", AccessLevel::Owner)]);
    let e = err(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a2", "sensitivity": "secret"}),
            &owner2,
        )
        .await);
    assert!(e.contains("敏感度"), "{e}");
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a2"}),
            &owner2,
        )
        .await);
    assert_eq!(r["own"], Value::Null);
}

#[tokio::test]
async fn a_later_owner_is_not_added_on_their_own() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    let late = f.person("owner-9", &[("a1", AccessLevel::Owner)]);
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &late,
        )
        .await);
    assert_eq!(r["manage_blocked"], "not_collection_editor");
    assert_eq!(
        r["own"]["sources"],
        Value::Null,
        "not a reader: no file list"
    );
    assert!(r["own"]["collection"]["readers"].is_null());
    let e = err(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &late,
        )
        .await);
    assert_eq!(e, "permission denied");
    // Admins always can.
    let r = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &f.admin,
        )
        .await);
    assert_eq!(r["created"], false);
}

#[tokio::test]
async fn the_own_collection_is_archived_and_restored_with_the_employee() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    f.person("op-1", &[("a1", AccessLevel::Operator)]);
    let made = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1", "share_chat_users": true}),
            &owner,
        )
        .await);
    let id = made["collection"]["id"].as_str().unwrap().to_string();

    ok(&f
        .call("agents.archive", json!({"agent_id": "a1"}), &f.admin)
        .await);
    let g = f.grants(&id);
    assert_eq!(
        g,
        vec![(
            "user".to_string(),
            "owner-7".to_string(),
            "owner".to_string()
        )]
    );
    let r = ok(&f
        .call(
            "knowledge.agent.data_sources",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert_eq!(r["manage_blocked"], "archived");
    assert_eq!(r["own"]["archived"], true);
    let e = err(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert!(e.contains("封存"), "{e}");

    ok(&f
        .call("agents.unarchive", json!({"agent_id": "a1"}), &f.admin)
        .await);
    let g = f.grants(&id);
    assert!(
        g.contains(&("agent".into(), "a1".into(), "viewer".into())),
        "{g:?}"
    );
    assert!(
        g.contains(&("user".into(), "op-1".into(), "viewer".into())),
        "{g:?}"
    );

    // Soft delete archives it again.
    ok(&f
        .call("agents.remove", json!({"agent_id": "a1"}), &f.admin)
        .await);
    assert_eq!(f.grants(&id).len(), 1);
    let audit =
        std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(
        audit.contains("knowledge_own_collection_archived"),
        "{audit}"
    );
    assert!(
        audit.contains("knowledge_own_collection_restored"),
        "{audit}"
    );
}

#[tokio::test]
async fn unreadable_settings_create_nothing() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    let path = f.home.path().join("agents/a1/agent.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("knowledge = \"oops\"\n{text}")).unwrap();
    let e = err(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    assert!(e.contains("無法解讀"), "{e}");
    let conn = duduclaw_knowledge::schema::open_home(f.home.path()).unwrap();
    assert!(
        duduclaw_knowledge::agent_own::get(&conn, "a1")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn bad_agent_ids_are_refused() {
    let f = fixture().await;
    for id in ["", "../a1", "zz"] {
        let e = err(&f
            .call(
                "knowledge.agent.data_sources",
                json!({"agent_id": id}),
                &f.admin,
            )
            .await);
        assert_eq!(e, "permission denied", "{id}");
    }
}

#[tokio::test]
async fn start_up_reconcile_restores_a_collection_left_archived_for_an_active_employee() {
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    let made = ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    let id = made["collection"]["id"].as_str().unwrap().to_string();
    {
        let mut conn = duduclaw_knowledge::schema::open_home(f.home.path()).unwrap();
        duduclaw_knowledge::agent_own::archive(&mut conn, "a1", "test")
            .unwrap()
            .unwrap();
    }
    assert_eq!(f.grants(&id).len(), 1);
    f.handler.kb_own_reconcile().await;
    assert!(
        f.grants(&id)
            .contains(&("agent".into(), "a1".into(), "viewer".into()))
    );
    // And an employee archived behind the dashboard's back is archived.
    ok(&f
        .call("agents.archive", json!({"agent_id": "a1"}), &f.admin)
        .await);
    {
        let mut conn = duduclaw_knowledge::schema::open_home(f.home.path()).unwrap();
        duduclaw_knowledge::agent_own::restore(&mut conn, "a1", "test")
            .unwrap()
            .unwrap();
    }
    f.handler.kb_own_reconcile().await;
    assert_eq!(f.grants(&id).len(), 1);
}

#[tokio::test]
async fn an_employee_role_owner_can_create_but_only_for_its_own_employee() {
    // D7: people manage the employees they own, whatever their account
    // role; one own collection per employee bounds what this can create.
    let f = fixture().await;
    let owner = f.person("owner-7", &[("a1", AccessLevel::Owner)]);
    let e = err(&f
        .call("knowledge.collections.create", json!({"name": "x"}), &owner)
        .await);
    assert_eq!(e, "permission denied");
    ok(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a1"}),
            &owner,
        )
        .await);
    let e = err(&f
        .call(
            "knowledge.agent.own.ensure",
            json!({"agent_id": "a2"}),
            &owner,
        )
        .await);
    assert_eq!(e, "permission denied");
}
