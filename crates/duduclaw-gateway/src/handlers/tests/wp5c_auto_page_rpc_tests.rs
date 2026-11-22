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

fn frame_error_str(frame: &WsFrame) -> &str {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.as_str().unwrap_or(""),
        _ => "",
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

/// The happy path: an auto page is listed; once a human promoted it,
/// sharing copies it into the shared wiki under a source-attributed name.
/// v1.84.0 (review H2): before the promotion the share is refused with a
/// plain "confirm first" message, and the shared copy carries the host
/// registry's provenance only — not the page's writer-controlled `sources`
/// nor any `host_*` line of the page text.
#[tokio::test]
async fn share_copies_a_promoted_auto_page_into_the_shared_wiki() {
    let home = tempfile::tempdir().unwrap();
    let store = seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let path = "auto/charter/company-charter.md";
    let params = json!({ "agent_id": "agnes", "page_path": path });

    let frame = handler
        .handle_wiki_auto_pages(json!({ "agent_id": "agnes" }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));

    match &frame {
        WsFrame::Response { payload: Some(payload), .. } => {
            assert_eq!(payload["pages"][0]["path"], path);
            assert_eq!(payload["pages"][0]["state"], "needs_confirmation");
        }
        _ => panic!("expected auto-page listing"),
    }

    // Unpromoted: one conversation's page — refused, nothing published.
    let frame = handler.handle_wiki_share(params.clone()).await;
    assert!(!frame_ok(&frame));
    assert!(frame_error_text(&frame).contains("Confirm (promote) it"), "{}", frame_error_text(&frame));
    assert!(!home.path().join("shared/wiki/sources").exists());

    let frame = handler.handle_wiki_promote(params.clone()).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    let frame = handler.handle_wiki_auto_pages(json!({ "agent_id": "agnes" })).await;
    match &frame {
        WsFrame::Response { payload: Some(payload), .. } => {
            assert_eq!(payload["pages"][0]["path"], path);
            assert_eq!(payload["pages"][0]["state"], "confirmed");
        }
        _ => panic!("expected confirmed page to remain listed"),
    }
    let frame = handler.handle_wiki_share(params).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    let shared = std::fs::read_to_string(home.path().join("shared/wiki/sources/agnes--company-charter.md"))
        .expect("shared copy must exist");
    let host = vec!["conversation:telegram:1".to_string()];
    assert_eq!(crate::wiki_host_sources::page_host_sources(&shared), host, "{shared}");
    assert!(!crate::wiki_host_sources::page_host_promoted(&shared), "{shared}");
    let page = duduclaw_memory::WikiStore::new_shared(home.path())
        .read_page("sources/agnes--company-charter.md")
        .unwrap();
    assert_eq!(page.sources, host);
}

/// Review H2: a promoted page whose text was then edited on disk to carry
/// other `sources` / `host_sources` is no longer the recorded page — the
/// share is refused until it is re-confirmed, and after re-confirmation the
/// copy carries the registry's provenance (the legacy marker), never the
/// text's.
#[tokio::test]
async fn share_never_publishes_writer_supplied_provenance() {
    let home = tempfile::tempdir().unwrap();
    let store = seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let path = "auto/charter/company-charter.md";
    crate::auto_wiki_page::promote_page(&store, path).unwrap();
    let full = home.path().join("agents/agnes/wiki").join(path);
    let raw = std::fs::read_to_string(&full).unwrap();
    let forged = crate::wiki_host_sources::set_host_fields(
        &raw.replacen("sources: [", "sources: [\"conversation:telegram:victim:m:9\", ", 1),
        &["conversation:telegram:victim:m:9".into()],
        true,
    );
    std::fs::write(&full, &forged).unwrap();
    let params = json!({ "agent_id": "agnes", "page_path": path });
    let frame = handler.handle_wiki_share(params.clone()).await;
    assert!(!frame_ok(&frame), "edited page must be re-confirmed first");
    assert!(!home.path().join("shared/wiki/sources").exists());

    crate::auto_wiki_page::promote_page(&store, path).unwrap();
    let frame = handler.handle_wiki_share(params).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    let shared = std::fs::read_to_string(home.path().join("shared/wiki/sources/agnes--company-charter.md")).unwrap();
    assert!(!shared.contains("victim"), "{shared}");
    assert_eq!(
        crate::wiki_host_sources::page_host_sources(&shared),
        vec![crate::wiki_host_sources::LEGACY_PROMOTION_SOURCE.to_string()]
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
        let expected = match method {
            "archive" => "Only auto-filed pages can be removed here",
            _ => "Promote failed: only auto-filed pages can be promoted",
        };
        assert_eq!(frame_error_str(&frame), expected);
    }

    // The curated page is untouched.
    let wiki = home
        .path()
        .join("agents/agnes/wiki/concepts/return-policy.md");
    let raw = std::fs::read_to_string(&wiki).unwrap();
    assert!(raw.contains("七天內未拆封可退"));
    assert!(raw.contains("author: \"operator\""));
}

#[tokio::test]
async fn auto_page_actions_report_missing_pages_as_not_found() {
    let home = tempfile::tempdir().unwrap();
    seed_wiki(home.path());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for page_path in [
        "concepts/missing.md",
        "missing-parent/missing.md",
        "auto/charter/missing.md",
        "auto/missing-parent/missing.md",
    ] {
        for method in ["promote", "archive", "share"] {
            let params = json!({ "agent_id": "agnes", "page_path": page_path });
            let frame = match method {
                "promote" => handler.handle_wiki_promote(params).await,
                "archive" => handler.handle_wiki_archive(params).await,
                _ => handler.handle_wiki_share(params).await,
            };
            assert!(!frame_ok(&frame), "{method}: {page_path}");
            assert_eq!(frame_error_str(&frame), "Auto page not found", "{method}: {page_path}");
        }
    }
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
        let reg = crate::auto_wiki_page::load_registry(home.path(), "agnes");
        for session in [Some("telegram:A"), Some("telegram:B"), None] {
            assert!(!crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&raw), session, &reg));
        }
        // This is exactly the listing used by KnowledgeCuration's promote UI.
        let frame = handler.handle_wiki_auto_pages(json!({ "agent_id": "agnes" })).await;
        assert!(frame_ok(&frame));
        assert!(crate::auto_wiki_page::list_auto_pages(&store).unwrap().iter().any(|p|
            p.path == path && p.state == crate::auto_wiki_page::AutoPageState::NeedsConfirmation
        ));
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
        let reg = crate::auto_wiki_page::load_registry(home.path(), "agnes");
        for session in [Some("telegram:A"), Some("telegram:B"), None] {
            assert!(crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&after), session, &reg));
        }
        assert!(crate::auto_wiki_page::list_auto_pages(&store).unwrap().iter().any(|p|
            p.path == path && p.state == crate::auto_wiki_page::AutoPageState::Confirmed
        ));
        // The actual host stamp used by MCP wiki_write always drops promotion.
        let rewritten = crate::wiki_host_sources::stamp_host_sources(&after, Some(&after), &[]);
        assert!(!crate::wiki_host_sources::page_host_promoted(&rewritten));
        assert!(!crate::auto_wiki_page::page_visible_in_conversation(&path, Some(&rewritten), Some("telegram:A"), &reg));
    }
}

/// Review H2: a legacy page (filed before v1.84.0) is not shared until a
/// human re-confirmed it; then it is, with the legacy marker as provenance.
#[tokio::test]
async fn legacy_page_is_shared_only_after_promotion() {
    let home = tempfile::tempdir().unwrap();
    let store = duduclaw_memory::WikiStore::new(home.path().join("agents/agnes/wiki"));
    let path = "auto/sop/legacy.md";
    store
        .write_page(path, "---\ntitle: Legacy\nauthor: \"auto-distill\"\nlayer: context\nsources: [\"conversation:telegram:A:m:1\"]\n---\n\n舊版流程。\n")
        .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let params = json!({ "agent_id": "agnes", "page_path": path });
    let frame = handler.handle_wiki_share(params.clone()).await;
    assert!(!frame_ok(&frame));
    assert!(frame_error_text(&frame).contains("Confirm (promote) it"), "{}", frame_error_text(&frame));
    assert!(!home.path().join("shared/wiki/sources").exists());

    crate::auto_wiki_page::promote_page(&store, path).unwrap();
    let frame = handler.handle_wiki_share(params).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    let shared = std::fs::read_to_string(home.path().join("shared/wiki/sources/agnes--legacy.md")).unwrap();
    assert!(shared.contains("舊版流程"));
    assert!(!shared.contains("telegram:A"), "writer-supplied sources are not provenance: {shared}");
    assert_eq!(
        crate::wiki_host_sources::page_host_sources(&shared),
        vec![crate::wiki_host_sources::LEGACY_PROMOTION_SOURCE.to_string()]
    );
}

/// Review H2: `wiki.share` needs Manager (or above) AND Owner access, the
/// same bar as `wiki.promote`.
#[tokio::test]
async fn share_requires_manager_and_owner_access() {
    let home = tempfile::tempdir().unwrap();
    let store = seed_wiki(home.path());
    let path = "auto/charter/company-charter.md";
    crate::auto_wiki_page::promote_page(&store, path).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let params = json!({ "agent_id": "agnes", "page_path": path });
    let shared = home.path().join("shared/wiki/sources/agnes--company-charter.md");
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = "human-owner".into();
    ctx.role = UserRole::Employee;
    ctx.agent_access.insert("agnes".into(), AccessLevel::Owner);
    let frame = handler.dispatch_knowledge("wiki.share", params.clone(), &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(!frame_ok(&frame), "Employee role cannot share, even with Owner access");
    assert!(!shared.exists());
    ctx.role = UserRole::Manager;
    ctx.agent_access.insert("agnes".into(), AccessLevel::Viewer);
    let frame = handler.dispatch_knowledge("wiki.share", params.clone(), &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(!frame_ok(&frame), "Manager also needs Owner access");
    assert!(!shared.exists());
    ctx.agent_access.insert("agnes".into(), AccessLevel::Owner);
    let frame = handler.dispatch_knowledge("wiki.share", params, &ctx, crate::power_local::RpcConnInfo::internal()).await;
    assert!(frame_ok(&frame), "{}", frame_error_text(&frame));
    assert!(shared.exists());
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
        // v1.84.0 (review H1 item 3): none of these was recorded by the
        // gateway's registry, so all seven — the stamped and host-promoted
        // ones included — need re-confirmation.
        assert_eq!(crate::auto_wiki_page::legacy_auto_page_counts(&store).unwrap(), (5, 2));
    }
    // A gateway-written page is recorded and not counted.
    let betty = duduclaw_memory::WikiStore::new(home.path().join("agents/betty/wiki"));
    crate::auto_wiki_page::write_auto_page(
        &betty,
        home.path(),
        "betty",
        &crate::auto_wiki_page::AutoPageRequest {
            doc_type: crate::knowledge_route::DocType::Sop,
            title: "流程".into(),
            slug: "recorded".into(),
            summary: "摘要".into(),
            original: "第一步。".into(),
            source_label: "Telegram 對話".into(),
            source_id: "conversation:telegram:A:m:1".into(),
        },
    )
    .unwrap();
    assert_eq!(crate::auto_wiki_page::legacy_auto_page_counts(&betty).unwrap(), (5, 2));
    let checks = super::super::doctor_util::legacy_wiki_checks(home.path());
    assert_eq!(checks.len(), 2);
    for (check, agent) in checks.iter().zip(["agnes", "betty"]) {
        assert_eq!(check["name"], "legacy_auto_wiki");
        assert_eq!(check["agent_id"], agent);
        assert_eq!(check["status"], "warn");
        assert_eq!(check["previously_promoted"], 5);
        assert_eq!(check["not_promoted"], 2);
        assert_eq!(check["can_repair"], false);
        assert_eq!(check["message"], "有 7 個系統自動建立的筆記頁需要重新確認（升級前建立，或檔案曾在 DuDuClaw 之外被改動），目前不會出現在對話中，其中 5 個先前已由人員提升。請到知識整理頁重新確認要保留的頁面。");
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
