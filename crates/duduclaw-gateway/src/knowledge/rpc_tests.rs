//! Permission tests for every `knowledge.*` RPC: Employee without a grant,
//! Employee with a viewer / editor grant, Manager, Admin.

use std::collections::HashMap;

use super::*;

fn ctx(id: &str, role: UserRole) -> UserContext {
    UserContext {
        user_id: id.to_string(),
        email: format!("{id}@example.test"),
        role,
        agent_access: HashMap::new(),
        must_change_password: false,
    }
}

async fn call(home: &Path, who: &UserContext, method: &str, params: Value) -> Result<Value, String> {
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
    Fixture { collection: id, ..f }
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
        n(
            call(f.home.path(), &f.stranger, "knowledge.collections.list", json!({}))
                .await
                .unwrap()
        ),
        0
    );
    let v = call(f.home.path(), &f.viewer, "knowledge.collections.list", json!({}))
        .await
        .unwrap();
    assert_eq!(v["collections"][0]["my_role"], "viewer");
    assert_eq!(v["can_create"], false);
    assert_eq!(
        n(call(f.home.path(), &f.admin, "knowledge.collections.list", json!({}))
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
        call(f.home.path(), &f.viewer, "knowledge.sources.add", paste.clone()).await
    ));
    assert!(denied(
        call(f.home.path(), &f.stranger, "knowledge.sources.add", paste.clone()).await
    ));
    let added = call(f.home.path(), &f.editor, "knowledge.sources.add", paste.clone())
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
        ("knowledge.access_log.list", json!({"collection_id": f.collection})),
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
            call(f.home.path(), &f.admin, method, params).await.is_ok() || method == "knowledge.collections.update",
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
    assert!(ops.contains(&"delete_source") && ops.contains(&"add_source"), "{ops:?}");
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
        let Added::New(s) = duduclaw_knowledge::sources::add_source(&mut conn, &new, &KnowledgeConfig::default())?
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
    let found = call(f.home.path(), &f.viewer, "knowledge.search", json!({"query": "特休假"}))
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
    std::fs::write(f.home.path().join("config.toml"), "[knowledge]\nenabled = false\n").unwrap();
    let off = call(f.home.path(), &f.admin, "knowledge.collections.list", json!({})).await;
    assert!(matches!(off, Err(ref m) if m.contains("關閉")));
    assert!(
        handle(f.home.path(), "wiki.list", &json!({}), &f.admin, None)
            .await
            .is_none()
    );
}
