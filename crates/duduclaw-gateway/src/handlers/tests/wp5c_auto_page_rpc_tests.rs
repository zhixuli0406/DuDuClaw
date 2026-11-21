//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP5c curation-station RPCs. The three state-changing actions
//! (`wiki.promote` / `wiki.archive` / `wiki.share`) all live on the
//! auto-filing audit tab and must agree on one gate: **an auto-filed page,
//! and only an auto-filed page**. A gap in any one of them turns that tab
//! into a general write primitive over the agent's whole wiki.
use super::*;

fn frame_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

fn frame_error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

/// Seed an agent wiki with one auto-filed page and one hand-curated page.
fn seed_wiki(home: &std::path::Path) -> duduclaw_memory::WikiStore {
    let wiki_dir = home.join("agents").join("agnes").join("wiki");
    std::fs::create_dir_all(&wiki_dir).unwrap();
    let store = duduclaw_memory::WikiStore::new(wiki_dir);

    crate::auto_wiki_page::write_auto_page(
        &store,
        home,
        "agnes",
        &crate::auto_wiki_page::AutoPageRequest {
            doc_type: crate::knowledge_route::DocType::Charter,
            title: "公司章程".into(),
            slug: "company-charter".into(),
            summary: "公司的組織章程。".into(),
            original: "第一條　本公司依法設立。".into(),
            source_label: "Telegram 對話".into(),
            source_id: "conversation:telegram:1".into(),
        },
    )
    .expect("seed auto page");

    store
        .write_page(
            "concepts/return-policy.md",
            "---\ntitle: \"退貨政策\"\nauthor: \"operator\"\nlayer: core\ntrust: 0.900\n---\n\n七天內未拆封可退。",
        )
        .unwrap();
    store
}

/// The happy path: an auto page is listed, and sharing it copies it into
/// the shared wiki under a source-attributed name.
#[tokio::test]
async fn share_copies_an_auto_page_into_the_shared_wiki() {
    let home = tempfile::tempdir().unwrap();
    seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_wiki_auto_pages(json!({ "agent_id": "agnes" }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));

    let frame = handler
        .handle_wiki_share(json!({
            "agent_id": "agnes",
            "page_path": "auto/charter/company-charter.md",
        }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    assert!(
        home.path()
            .join("shared/wiki/sources/agnes--company-charter.md")
            .exists(),
        "shared copy must exist"
    );
}

/// M1: a hand-curated page must NOT be shareable through this RPC. It is
/// outside `auto/`, so the audit tab's provenance guarantees do not hold
/// for it and the shared-wiki safeguards (`.scope.toml`, department
/// visibility, secret scan) are not applied on this path.
#[tokio::test]
async fn share_refuses_a_hand_curated_page() {
    let home = tempfile::tempdir().unwrap();
    seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_wiki_share(json!({
            "agent_id": "agnes",
            "page_path": "concepts/return-policy.md",
        }))
        .await;
    assert!(!frame_ok(&frame), "curated page must be refused");
    assert!(
        frame_error_text(&frame).contains("auto-filed"),
        "got: {}",
        frame_error_text(&frame)
    );
    assert!(
        !home.path().join("shared/wiki/sources").exists(),
        "nothing may be written to the shared wiki"
    );
}

/// A page that merely SITS under `auto/` but was written by a human is
/// also refused — the path check and the author check must both hold, or a
/// user who files something by hand into `auto/` loses the protection.
#[tokio::test]
async fn share_refuses_a_human_authored_page_inside_auto() {
    let home = tempfile::tempdir().unwrap();
    let store = seed_wiki(home.path());
    store
        .write_page(
            "auto/charter/handmade.md",
            "---\ntitle: \"手寫\"\nauthor: \"operator\"\nlayer: core\ntrust: 0.900\n---\n\nbody",
        )
        .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_wiki_share(json!({
            "agent_id": "agnes",
            "page_path": "auto/charter/handmade.md",
        }))
        .await;
    assert!(!frame_ok(&frame), "human-authored page must be refused");
}

/// `wiki.archive` and `wiki.promote` enforce the same gate — asserted here
/// so the three actions can never drift apart silently.
#[tokio::test]
async fn archive_and_promote_share_the_same_gate() {
    let home = tempfile::tempdir().unwrap();
    seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for method in ["archive", "promote"] {
        let params = json!({
            "agent_id": "agnes",
            "page_path": "concepts/return-policy.md",
        });
        let frame = match method {
            "archive" => handler.handle_wiki_archive(params).await,
            _ => handler.handle_wiki_promote(params).await,
        };
        assert!(!frame_ok(&frame), "{method} must refuse a curated page");
    }

    // The curated page is untouched.
    let wiki = home
        .path()
        .join("agents/agnes/wiki/concepts/return-policy.md");
    let raw = std::fs::read_to_string(&wiki).unwrap();
    assert!(raw.contains("七天內未拆封可退"));
    assert!(raw.contains("author: \"operator\""));
}

/// The dashboard's existing promotion action can re-confirm v1.83 pages,
/// including ones whose earlier human promotion changed author/layer.
#[tokio::test]
async fn dashboard_repromotes_legacy_pages_without_changing_content() {
    let home = tempfile::tempdir().unwrap();
    let wiki = home.path().join("agents/agnes/wiki");
    let store = duduclaw_memory::WikiStore::new(wiki);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    for (name, author, layer) in [("promoted", "operator", "core"), ("unpromoted", "auto-distill", "context")] {
        let path = format!("auto/sop/{name}.md");
        let raw = format!("---\ntitle: Legacy\nauthor: \"{author}\"\nlayer: {layer}\ntrust: 0.3\nsources: [\"conversation:telegram:A:m:1\"]\n---\n\n原文及人工修訂。\n");
        store.write_page(&path, &raw).unwrap();
        for session in [Some("telegram:A"), Some("telegram:B"), None] {
            assert!(!crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&raw), session));
        }
        // This is exactly the listing used by KnowledgeCuration's promote UI.
        let frame = handler.handle_wiki_auto_pages(json!({ "agent_id": "agnes" })).await;
        assert!(frame_ok(&frame));
        assert!(crate::auto_wiki_page::list_auto_pages(&store).unwrap().iter().any(|p| p.path == path));
        let frame = handler.handle("wiki.promote", json!({
            "agent_id": "agnes", "page_path": path,
        }), &ctx).await;
        assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
        let after = store.read_raw(&path).unwrap();
        assert!(crate::wiki_host_sources::page_host_promoted(&after));
        let entries = crate::wiki_host_sources::page_host_sources(&after);
        assert_eq!(entries, vec![crate::wiki_host_sources::LEGACY_PROMOTION_SOURCE.to_string()]);
        assert!(!crate::auto_wiki_page::source_belongs_to(&entries[0], "telegram:A"));
        assert_eq!(after, crate::wiki_host_sources::set_host_fields(&raw, &entries, true));
        for session in [Some("telegram:A"), Some("telegram:B"), None] {
            assert!(crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&after), session));
        }
        assert!(!crate::auto_wiki_page::list_auto_pages(&store).unwrap().iter().any(|p| p.path == path));
        // The actual host stamp used by MCP wiki_write always drops promotion.
        let rewritten = crate::wiki_host_sources::stamp_host_sources(&after, Some(&after), &[]);
        assert!(!crate::wiki_host_sources::page_host_promoted(&rewritten));
        assert!(!crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&rewritten), Some("telegram:A")));
    }
}

#[test]
fn doctor_counts_legacy_auto_pages_without_failing() {
    let home = tempfile::tempdir().unwrap();
    for agent in ["agnes", "betty"] {
        let store = duduclaw_memory::WikiStore::new(home.path().join("agents").join(agent).join("wiki"));
        for (name, author, layer) in [
            ("promoted", "operator", "core"),
            ("layer-only", "auto-distill", "core"),
            ("author-only", "operator", "context"),
            ("unpromoted", "auto-distill", "context"),
            ("stamped", "auto-distill", "context"),
            ("host-promoted", "operator", "core"),
            ("malformed-stamp", "operator", "core"),
        ] {
            let raw = format!("---\ntitle: Legacy\nauthor: \"{author}\"\nlayer: {layer}\n---\n\nbody\n");
            let raw = match name {
                "stamped" => crate::wiki_host_sources::set_host_fields(&raw, &["conversation:telegram:A:m:1".into()], false),
                "host-promoted" => crate::wiki_host_sources::set_host_fields(&raw, &[], true),
                "malformed-stamp" => raw.replacen("title: Legacy", "title: Legacy\nhost_sources: invalid", 1),
                _ => raw,
            };
            store.write_page(&format!("auto/sop/{name}.md"), &raw).unwrap();
        }
        store.write_page("concepts/manual.md", "---\ntitle: Manual\n---\nbody").unwrap();
        assert_eq!(crate::auto_wiki_page::legacy_auto_page_counts(&store).unwrap(), (3, 1));
    }
    let checks = super::super::doctor_util::legacy_wiki_checks(home.path());
    assert_eq!(checks.len(), 2);
    for (check, agent) in checks.iter().zip(["agnes", "betty"]) {
        assert_eq!(check["name"], "legacy_auto_wiki");
        assert_eq!(check["agent_id"], agent);
        assert_eq!(check["status"], "warn");
        assert_eq!(check["previously_promoted"], 3);
        assert_eq!(check["not_promoted"], 1);
        assert_eq!(check["can_repair"], false);
        assert_eq!(check["message"], "有 4 個升級前由系統自動建立的筆記頁目前不會出現在對話中，其中 3 個先前已由人員提升。請到知識整理頁重新確認要保留的頁面。");
    }
    assert_eq!(crate::os_ops::doctor_summary(&checks)["fail"], 0);
}

#[tokio::test]
async fn legacy_promotion_requires_manager_and_owner_access() {
    let home = tempfile::tempdir().unwrap();
    let store = duduclaw_memory::WikiStore::new(home.path().join("agents/agnes/wiki"));
    let path = "auto/sop/legacy.md";
    let raw = "---\ntitle: Legacy\nauthor: operator\nlayer: core\n---\n\nbody\n";
    store.write_page(path, raw).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let params = json!({ "agent_id": "agnes", "page_path": path });
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = "human-manager".into();
    ctx.role = UserRole::Employee;
    ctx.agent_access.insert("agnes".into(), AccessLevel::Owner);
    let frame = handler.dispatch_knowledge("wiki.promote", params.clone(), &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(!frame_ok(&frame), "Employee role cannot promote, even with Owner access");
    assert_eq!(store.read_raw(path).unwrap(), raw);
    ctx.role = UserRole::Manager;
    ctx.agent_access.insert("agnes".into(), AccessLevel::Viewer);
    let frame = handler.dispatch_knowledge("wiki.promote", params.clone(), &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(!frame_ok(&frame), "Manager also needs Owner access");
    assert_eq!(store.read_raw(path).unwrap(), raw);
    ctx.agent_access.insert("agnes".into(), AccessLevel::Owner);
    let frame = handler.dispatch_knowledge("wiki.promote", params, &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    assert!(crate::wiki_host_sources::page_host_promoted(&store.read_raw(path).unwrap()));
}
