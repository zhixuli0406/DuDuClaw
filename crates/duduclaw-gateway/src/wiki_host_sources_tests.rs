//! Tests for host-recorded wiki page sources (P2-B H-4).

use super::*;

fn sel(session: &str, messages: &[&str], upto_seq: Option<i64>) -> SelectorView {
    SelectorView {
        session: session.to_string(),
        messages: messages.iter().map(|m| m.to_string()).collect(),
        upto_seq,
        upto_time: Some(chrono::Utc::now()),
    }
}

#[test]
fn stamping_adds_keeps_and_owns_the_key() {
    let page = "---\ntitle: SOP\nhost_sources: [\"conversation:forged:m:1\"]\n---\n\nbody\n";
    let out = stamp_host_sources(page, None, &["conversation:telegram:1:m:5".into()]);
    assert_eq!(
        page_host_sources(&out),
        vec!["conversation:telegram:1:m:5".to_string()]
    );
    assert!(
        out.contains("title: SOP") && out.ends_with("body\n"),
        "{out}"
    );

    // A later rewrite without the key keeps the earlier entry.
    let again = stamp_host_sources(
        "---\ntitle: SOP\n---\nnew body\n",
        Some(&out),
        &["conversation:telegram:2:m:9".into()],
    );
    assert_eq!(
        page_host_sources(&again),
        vec![
            "conversation:telegram:1:m:5".to_string(),
            "conversation:telegram:2:m:9".to_string()
        ]
    );

    // No frontmatter: one is added; nothing to record: unchanged.
    let plain = stamp_host_sources("# Hi\n", None, &["conversation:a:m:1".into()]);
    assert!(plain.starts_with("---\nhost_sources: "), "{plain}");
    assert!(plain.ends_with("# Hi\n"));
    assert_eq!(stamp_host_sources("# Hi\n", None, &[]), "# Hi\n");
}

/// v1.84.0 (review H1): the promotion mark is host-owned — a writer cannot
/// supply it through the MCP stamp, and a rewrite through the stamp drops
/// it; only `set_host_fields` (the host paths) sets it.
#[test]
fn promotion_mark_cannot_be_supplied_or_carried_by_a_stamped_write() {
    let forged = "---\ntitle: SOP\nhost_promoted: true\nhost_promoted : true\n---\n\nbody\n";
    let out = stamp_host_sources(forged, None, &[]);
    assert!(!page_host_promoted(&out), "{out}");
    assert!(!out.contains(HOST_PROMOTED_KEY), "{out}");
    let out = stamp_host_sources(forged, None, &["conversation:telegram:1:m:5".into()]);
    assert!(!page_host_promoted(&out), "{out}");

    let promoted = set_host_fields(
        "---\ntitle: SOP\n---\n\nbody\n",
        &["conversation:telegram:1:m:5".into()],
        true,
    );
    assert!(page_host_promoted(&promoted), "{promoted}");
    assert_eq!(page_host_sources(&promoted), vec!["conversation:telegram:1:m:5".to_string()]);
    // A later stamped rewrite keeps the host sources but not the mark.
    let rewritten = stamp_host_sources("---\ntitle: SOP\n---\n\nnew\n", Some(&promoted), &[]);
    assert!(!page_host_promoted(&rewritten), "{rewritten}");
    assert_eq!(page_host_sources(&rewritten), vec!["conversation:telegram:1:m:5".to_string()]);
    // `set_host_fields` replaces whatever host lines the content carried.
    let replaced = set_host_fields(forged, &[], false);
    assert!(!replaced.contains("host_"), "{replaced}");
    assert!(!page_host_promoted("---\nhost_promoted: yes\n---\n"));
}

#[test]
fn entries_skip_external_calls_and_cap_at_the_limit() {
    let at = chrono::Utc::now();
    let ext = SourceRef::other(SourceKind::McpExternal, "mcp:c", "call:1", at);
    let msg = SourceRef::channel_message("telegram:1", 4, at, None);
    assert_eq!(
        entries_for(&[ext, msg]),
        vec!["conversation:telegram:1:m:4".to_string()]
    );

    let many: Vec<String> = (0..30).map(|i| format!("conversation:s:m:{i}")).collect();
    let out = stamp_host_sources("---\nt: 1\n---\n", None, &many);
    let got = page_host_sources(&out);
    assert_eq!(got.len(), HOST_SOURCES_MAX);
    assert_eq!(got.last().unwrap(), "conversation:s:m:29");
}

#[test]
fn review_listing_finds_employee_pages_but_not_auto_or_other_sessions() {
    let home = tempfile::tempdir().unwrap();
    let wiki = home.path().join("agents/agnes/wiki");
    std::fs::create_dir_all(wiki.join("concepts")).unwrap();
    std::fs::create_dir_all(wiki.join("auto/sop")).unwrap();
    std::fs::create_dir_all(home.path().join("shared/wiki")).unwrap();
    let page = |e: &str| stamp_host_sources("---\ntitle: x\n---\nb\n", None, &[e.to_string()]);
    std::fs::write(
        wiki.join("concepts/a.md"),
        page("conversation:telegram:1:m:5"),
    )
    .unwrap();
    std::fs::write(
        wiki.join("concepts/b.md"),
        page("conversation:telegram:2:m:5"),
    )
    .unwrap();
    std::fs::write(
        wiki.join("auto/sop/c.md"),
        page("conversation:telegram:1:m:5"),
    )
    .unwrap();
    std::fs::write(
        home.path().join("shared/wiki/d.md"),
        page("conversation:telegram:1:m:5"),
    )
    .unwrap();
    let got = pages_needing_review(home.path(), "agnes", &sel("telegram:1", &["m:5"], None));
    assert_eq!(
        got,
        vec!["shared/d.md".to_string(), "wiki/concepts/a.md".to_string()]
    );
    assert!(
        pages_needing_review(home.path(), "agnes", &sel("telegram:1", &["m:6"], None)).is_empty()
    );
}
