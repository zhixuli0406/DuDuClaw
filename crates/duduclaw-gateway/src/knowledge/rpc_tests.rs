//! Permission tests for every `knowledge.*` RPC: Employee without a grant,
//! Employee with a viewer / editor grant, Manager, Admin.

use std::collections::HashMap;

use super::*;

fn ctx(id: &str, role: UserRole) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: id.to_string(),
        email: format!("{id}@example.test"),
        role,
        agent_access: HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

async fn call(
    home: &Path,
    who: &UserContext,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    handle(home, method, &params, who, None)
        .await
        .expect("knowledge method")
}

fn denied(r: Result<Value, String>) -> bool {
    matches!(r, Err(ref m) if m == "permission denied")
}

struct Fixture {
    home: tempfile::TempDir,
    admin: UserContext,
    manager: UserContext,
    viewer: UserContext,
    editor: UserContext,
    stranger: UserContext,
    collection: String,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let f = Fixture {
        admin: ctx("admin", UserRole::Admin),
        manager: ctx("mgr", UserRole::Manager),
        viewer: ctx("viewer", UserRole::Employee),
        editor: ctx("editor", UserRole::Employee),
        stranger: ctx("stranger", UserRole::Employee),
        collection: String::new(),
        home,
    };
    let created = call(
        f.home.path(),
        &f.manager,
        "knowledge.collections.create",
        json!({"name": "人資手冊", "slug": "hr"}),
    )
    .await
    .unwrap();
    let id = created["collection"]["id"].as_str().unwrap().to_string();
    let acl = call(
        f.home.path(),
        &f.manager,
        "knowledge.acl.get",
        json!({"collection_id": id}),
    )
    .await
    .unwrap();
    call(
        f.home.path(),
        &f.manager,
        "knowledge.acl.set",
        json!({"collection_id": id, "expected_revision": acl["revision"], "entries": [
            {"kind": "user", "id": "mgr", "role": "owner"},
            {"kind": "user", "id": "viewer", "role": "viewer"},
            {"kind": "user", "id": "editor", "role": "editor"},
        ]}),
    )
    .await
    .unwrap();
    Fixture {
        collection: id,
        ..f
    }
}

#[tokio::test]
async fn only_manager_and_admin_create() {
    let f = fixture().await;
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.collections.create",
            json!({"name": "x"})
        )
        .await
    ));
    assert!(
        call(
            f.home.path(),
            &f.admin,
            "knowledge.collections.create",
            json!({"name": "y"})
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn listing_follows_grants() {
    let f = fixture().await;
    let n = |v: Value| v["collections"].as_array().unwrap().len();
    assert_eq!(
        n(call(
            f.home.path(),
            &f.stranger,
            "knowledge.collections.list",
            json!({})
        )
        .await
        .unwrap()),
        0
    );
    let v = call(
        f.home.path(),
        &f.viewer,
        "knowledge.collections.list",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(v["collections"][0]["my_role"], "viewer");
    assert_eq!(v["can_create"], false);
    assert_eq!(
        n(call(
            f.home.path(),
            &f.admin,
            "knowledge.collections.list",
            json!({})
        )
        .await
        .unwrap()),
        1
    );
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.collections.get",
            json!({"id": f.collection})
        )
        .await
    ));
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.collections.get",
            json!({"id": "col_missing"})
        )
        .await
    ));
}

#[tokio::test]
async fn editing_needs_editor_and_settings_need_owner() {
    let f = fixture().await;
    let paste = json!({"collection_id": f.collection, "kind": "paste", "title": "請假規定", "text": "員工請假需提前三日申請。"});
    assert!(denied(
        call(
            f.home.path(),
            &f.viewer,
            "knowledge.sources.add",
            paste.clone()
        )
        .await
    ));
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.sources.add",
            paste.clone()
        )
        .await
    ));
    let added = call(
        f.home.path(),
        &f.editor,
        "knowledge.sources.add",
        paste.clone(),
    )
    .await
    .unwrap();
    assert_eq!(added["duplicate"], false);
    let again = call(f.home.path(), &f.editor, "knowledge.sources.add", paste)
        .await
        .unwrap();
    assert_eq!(again["duplicate"], true);
    let sid = added["source"]["id"].as_str().unwrap().to_string();
    assert!(
        call(
            f.home.path(),
            &f.viewer,
            "knowledge.sources.get",
            json!({"source_id": sid})
        )
        .await
        .is_ok()
    );
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.sources.get",
            json!({"source_id": sid})
        )
        .await
    ));
    assert!(denied(
        call(
            f.home.path(),
            &f.viewer,
            "knowledge.sources.delete",
            json!({"source_id": sid})
        )
        .await
    ));
    for (method, params) in [
        ("knowledge.acl.get", json!({"collection_id": f.collection})),
        (
            "knowledge.access_log.list",
            json!({"collection_id": f.collection}),
        ),
        (
            "knowledge.collections.update",
            json!({"id": f.collection, "expected_updated_at": "x", "description": "d"}),
        ),
        (
            "knowledge.collections.delete",
            json!({"id": f.collection, "confirm_name": "人資手冊"}),
        ),
    ] {
        assert!(
            denied(call(f.home.path(), &f.editor, method, params.clone()).await),
            "{method}"
        );
        assert!(
            call(f.home.path(), &f.admin, method, params).await.is_ok()
                || method == "knowledge.collections.update",
            "{method}"
        );
    }
}

#[tokio::test]
async fn delete_source_removes_unreferenced_blob() {
    let f = fixture().await;
    let added = call(
        f.home.path(),
        &f.editor,
        "knowledge.sources.add",
        json!({"collection_id": f.collection, "kind": "paste", "text": "加班費依勞基法計算。"}),
    )
    .await
    .unwrap();
    let sha = added["source"]["blob_sha256"].as_str().unwrap().to_string();
    let path = BlobStore::for_home(f.home.path()).path_for(&sha).unwrap();
    assert!(path.exists());
    // A file inside the GC grace period is kept (an upload of the same bytes
    // may be in flight); an older one goes with its last source.
    let f2 = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f2.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let sid = added["source"]["id"].as_str().unwrap();
    call(
        f.home.path(),
        &f.editor,
        "knowledge.sources.delete",
        json!({"source_id": sid}),
    )
    .await
    .unwrap();
    assert!(!path.exists());
    let log = call(
        f.home.path(),
        &f.manager,
        "knowledge.access_log.list",
        json!({"collection_id": f.collection}),
    )
    .await
    .unwrap();
    let ops: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["op"].as_str())
        .collect();
    assert!(
        ops.contains(&"delete_source") && ops.contains(&"add_source"),
        "{ops:?}"
    );
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("knowledge_source_deleted") && audit.contains("knowledge_acl_changed"));
}

#[tokio::test]
async fn search_and_passage_use_the_human_side_only() {
    let f = fixture().await;
    let home = f.home.path().to_path_buf();
    let cid = f.collection.clone();
    blocking(move || {
        let mut conn = schema::open_home(&home)?;
        let new = duduclaw_knowledge::sources::NewSource {
            collection_id: cid.clone(),
            kind: SourceKind::Paste,
            title: "手冊".into(),
            original_name: None,
            url: None,
            format: Format::Txt,
            blob_sha256: "b".repeat(64),
            bytes: 1,
            origin: json!({}),
            created_by: "mgr".into(),
        };
        let Added::New(s) =
            duduclaw_knowledge::sources::add_source(&mut conn, &new, &KnowledgeConfig::default())?
        else {
            panic!()
        };
        let raw = vec![duduclaw_knowledge::extract::RawSegment::new(
            duduclaw_knowledge::extract::SegmentKind::Text,
            "特休假依年資計算，滿一年者給予七日。".into(),
        )];
        let prepared = duduclaw_knowledge::chunk::prepare(Format::Txt, raw);
        duduclaw_knowledge::index::write_index(
            &mut conn,
            &s.id,
            &prepared,
            &[],
            "t",
            &|_| false,
            &KnowledgeConfig::default(),
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let found = call(
        f.home.path(),
        &f.viewer,
        "knowledge.search",
        json!({"query": "特休假"}),
    )
    .await
    .unwrap();
    let chunk_id = found["hits"][0]["chunk_id"].as_i64().unwrap();
    assert_eq!(found["mode"], "keyword_only");
    let none = call(
        f.home.path(),
        &f.stranger,
        "knowledge.search",
        json!({"query": "特休假"}),
    )
    .await
    .unwrap();
    assert!(none["hits"].as_array().unwrap().is_empty());
    let pg = call(
        f.home.path(),
        &f.viewer,
        "knowledge.passage.get",
        json!({"chunk_id": chunk_id}),
    )
    .await
    .unwrap();
    assert!(pg["match"].as_str().unwrap().contains("七日"));
    assert!(denied(
        call(
            f.home.path(),
            &f.stranger,
            "knowledge.passage.get",
            json!({"chunk_id": chunk_id})
        )
        .await
    ));
}

#[tokio::test]
async fn acl_conflicts_and_disabled_switch() {
    let f = fixture().await;
    let stale = call(
        f.home.path(),
        &f.manager,
        "knowledge.acl.set",
        json!({"collection_id": f.collection, "expected_revision": 0, "entries": [{"kind": "user", "id": "mgr", "role": "owner"}]}),
    )
    .await;
    assert!(matches!(stale, Err(ref m) if m.contains("重新載入")));
    std::fs::write(
        f.home.path().join("config.toml"),
        "[knowledge]\nenabled = false\n",
    )
    .unwrap();
    let off = call(
        f.home.path(),
        &f.admin,
        "knowledge.collections.list",
        json!({}),
    )
    .await;
    assert!(matches!(off, Err(ref m) if m.contains("關閉")));
    assert!(
        handle(f.home.path(), "wiki.list", &json!({}), &f.admin, None)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn old_own_collection_acl_is_dynamically_hidden_and_restored() {
    use crate::knowledge::test_fixtures::{agent, collection};
    use duduclaw_core::preset;
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    crate::team_edition::record(h, duduclaw_core::EditionProfile::Enterprise);
    agent(h, "sales", "[agent]\nname='sales'\n[knowledge]\ncollections=['own-sales']\n");
    let viewer = ctx("viewer", UserRole::Employee);
    let admin = ctx("admin", UserRole::Admin);
    let (cid, source, chunk) = collection(h, "own-sales", "internal",
        &[(PrincipalKind::User, "viewer", Role::Owner), (PrincipalKind::Agent, "sales", Role::Viewer)], "特休假七日。");
    let conn = schema::open_home(h).unwrap();
    conn.execute("INSERT INTO agent_own_collections (agent_id, collection_id, created_by, created_at) VALUES ('sales', ?1, 'admin', 't')", [&cid]).unwrap();
    let acl_before = acl::get_acl(&conn, &cid).unwrap();
    let employee = crate::knowledge::principals::AgentRef {
        id: "sales".into(), display_name: "Sales".into(), department: None,
    };
    for restricted in [false, true, false] {
        if restricted {
            preset::install_system_operator_preset(h, false).unwrap();
            preset::bind(h, "sales", &h.join("agents/sales"), "system-operator", "test", "test").unwrap();
        } else {
            preset::unbind(h, "sales", "test", "test").unwrap();
        }
        for who in [&viewer, &admin] {
            let visible = !restricted || who.is_admin();
            let list = call(h, who, "knowledge.collections.list", json!({})).await.unwrap();
            assert_eq!(!list["collections"].as_array().unwrap().is_empty(), visible);
            assert_eq!(call(h, who, "knowledge.collections.get", json!({"id":cid})).await.is_ok(), visible);
            let found = call(h, who, "knowledge.search", json!({"query":"特休假"})).await.unwrap();
            assert_eq!(!found["hits"].as_array().unwrap().is_empty(), visible);
            assert_eq!(call(h, who, "knowledge.sources.list", json!({"collection_id":cid})).await.is_ok(), visible);
            assert_eq!(call(h, who, "knowledge.sources.get", json!({"source_id":source})).await.is_ok(), visible);
            assert_eq!(call(h, who, "knowledge.passage.get", json!({"chunk_id":chunk})).await.is_ok(), visible);
            assert_eq!(call(h, who, "knowledge.acl.get", json!({"collection_id":cid})).await.is_ok(), visible);
            let metadata = crate::knowledge::agent_binding::describe(h, &employee, &h.join("agents/sales"), who.is_admin()).unwrap();
            assert_eq!(!metadata["granted"].as_array().unwrap().is_empty(), visible);
            assert_eq!(!metadata["bound"].as_array().unwrap().is_empty(), visible);
        }
        assert_eq!(acl::get_acl(&conn, &cid).unwrap(), acl_before);
    }
    // A failed own-collection lookup never falls back to the old ACL.
    conn.execute("DROP TABLE agent_own_collections", []).unwrap();
    assert!(!super::super::principals::collection_visible(h, &conn, &cid, false));
}

#[test]
fn restricted_own_collection_keeps_unattended_internal_reads() {
    use crate::knowledge::test_fixtures::{agent, collection};
    use duduclaw_core::preset;
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "sales", "[agent]\nname='sales'\n");
    let (cid, _, _) = collection(h, "own-sales", "internal",
        &[(PrincipalKind::User, "viewer", Role::Owner), (PrincipalKind::Agent, "sales", Role::Viewer)], "Internal knowledge.");
    let conn = schema::open_home(h).unwrap();
    conn.execute("INSERT INTO agent_own_collections (agent_id, collection_id, created_by, created_at) VALUES ('sales', ?1, 'admin', 't')", [&cid]).unwrap();
    preset::install_system_operator_preset(h, false).unwrap();
    preset::bind(h, "sales", &h.join("agents/sales"), "system-operator", "test", "test").unwrap();
    let employee = acl::AgentCtx { agent_id: "sales".into(), department: None, narrowing: acl::AgentNarrowing::All };
    let full = acl::HumanCtx::dashboard("viewer", None, false);
    let internal = acl::HumanCtx { mode: acl::HumanMode::InternalOnly, ..acl::HumanCtx::nobody() };
    for (human, keeps) in [(&internal, true), (&full, false)] {
        let allowed = acl::effective_collections(&conn, human, Some(&employee), &acl::Narrowing::default()).unwrap();
        assert!(allowed.contains(&cid));
        assert_eq!(super::super::principals::filter_readable(h, &conn, human, &allowed).contains(&cid), keeps);
    }
}
