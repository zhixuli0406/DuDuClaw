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
