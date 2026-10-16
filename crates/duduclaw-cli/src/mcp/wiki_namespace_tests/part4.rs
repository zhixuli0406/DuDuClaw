//! Wiki path hardening: one shared validator for every wiki tool, fail-closed
//! containment, no-follow reads, and the `wiki_graph` depth cap.

use super::*;

fn is_err(v: &serde_json::Value) -> bool {
    v["isError"] == true
}

/// String-level forms, checked without touching the disk. Windows forms are
/// refused on every platform.
#[test]
fn shared_validator_refuses_traversal_drive_and_nul_forms() {
    for bad in [
        "",
        "../x.md",
        "a/../../x.md",
        "/etc/x.md",
        "\\x.md",
        "\\\\server\\share\\x.md",
        "\\\\?\\C:\\x.md",
        "C:\\x.md",
        "C:x.md",
        "d:/x.md",
        "a\0b.md",
        "%2e%2e/x.md",
        "a%5Cb.md",
    ] {
        assert!(validate_wiki_page_path(bad).is_err(), "write {bad:?}");
        assert!(validate_wiki_read_path(bad).is_err(), "read {bad:?}");
    }
    assert!(validate_wiki_page_path("entities/a.md").is_ok());
    // Reads may open reserved files; writes may not. Both require `.md`.
    assert!(validate_wiki_read_path("_index.md").is_ok());
    assert!(validate_wiki_page_path("_index.md").is_err());
    assert!(validate_wiki_read_path("notes.txt").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn wiki_read_and_write_refuse_drive_and_nul_paths() {
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    for bad in ["C:x.md", "C:\\x.md", "a\0b.md"] {
        let r = handle_wiki_read(&serde_json::json!({ "page_path": bad }), tmp.path(), "alice").await;
        assert!(is_err(&r), "read {bad:?}: {r}");
        let w = handle_wiki_write(
            &serde_json::json!({ "page_path": bad, "content": "x" }),
            tmp.path(),
            "alice",
        )
        .await;
        assert!(is_err(&w), "write {bad:?}: {w}");
    }
}

/// `wiki_write` used to `create_dir_all` the parent before the escape check:
/// through a symlinked subdirectory it created directories outside the wiki.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn wiki_write_through_symlinked_dir_creates_nothing_outside() {
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    let wiki_dir = resolve_wiki_dir(tmp.path(), "alice").unwrap();
    ensure_wiki_dir(&wiki_dir).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, wiki_dir.join("linked")).unwrap();

    let w = handle_wiki_write(
        &serde_json::json!({ "page_path": "linked/deep/x.md", "content": "---\ntitle: x\n---\n" }),
        tmp.path(),
        "alice",
    )
    .await;
    assert!(is_err(&w), "{w}");
    assert!(!outside.join("deep").exists(), "no directory may be created outside the wiki");
    assert!(!outside.join("deep/x.md").exists());

    // A normal nested write still works.
    let ok = handle_wiki_write(
        &serde_json::json!({ "page_path": "concepts/new/x.md", "content": "---\ntitle: x\n---\n" }),
        tmp.path(),
        "alice",
    )
    .await;
    assert!(!is_err(&ok), "{ok}");
}

/// Reads refuse a page that is a symlink (inside target or outside), a
/// dangling link (canonicalize fails: used to skip the check) and a path
/// through a symlinked directory. Listing skips symlinks.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn wiki_read_and_ls_do_not_follow_symlinks() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    let wiki_dir = resolve_wiki_dir(tmp.path(), "alice").unwrap();
    ensure_wiki_dir(&wiki_dir).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.md"), "---\ntitle: TOPSECRET\n---\nsecret").unwrap();
    symlink(&outside, wiki_dir.join("linked")).unwrap();
    symlink(outside.join("secret.md"), wiki_dir.join("concepts/alias.md")).unwrap();
    symlink(tmp.path().join("nowhere.md"), wiki_dir.join("concepts/dangling.md")).unwrap();

    for p in ["linked/secret.md", "concepts/alias.md", "concepts/dangling.md"] {
        let r = handle_wiki_read(&serde_json::json!({ "page_path": p }), tmp.path(), "alice").await;
        assert!(is_err(&r), "{p}: {r}");
        assert!(!r.to_string().contains("secret"), "{p} leaked: {r}");
    }
    let ls = handle_wiki_ls(&serde_json::json!({}), tmp.path(), "alice").await;
    assert!(!ls.to_string().contains("TOPSECRET"), "{ls}");

    // Reserved files stay readable.
    let idx = handle_wiki_read(&serde_json::json!({ "page_path": "_index.md" }), tmp.path(), "alice").await;
    assert!(!is_err(&idx), "{idx}");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_read_and_delete_refuse_symlinks() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    let shared = resolve_shared_wiki_dir(tmp.path());
    ensure_shared_wiki_dir(&shared).unwrap();
    let outside = tmp.path().join("outside.md");
    fs::write(&outside, "---\nauthor: alice\n---\nouter secret").unwrap();
    symlink(&outside, shared.join("concepts/link.md")).unwrap();

    let r = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "concepts/link.md" }),
        tmp.path(),
        "alice",
    )
    .await;
    assert!(is_err(&r), "{r}");
    assert!(!r.to_string().contains("outer secret"), "{r}");

    let d = handle_shared_wiki_delete(
        &serde_json::json!({ "page_path": "concepts/link.md" }),
        tmp.path(),
        "alice",
    )
    .await;
    assert!(is_err(&d), "{d}");
    assert!(outside.exists(), "the link target must survive");
    assert!(fs::symlink_metadata(shared.join("concepts/link.md")).is_ok());
}

/// `wiki_share` validates before any disk access, refuses reserved files and
/// does not publish what `wiki_read` withholds.
#[tokio::test(flavor = "current_thread")]
async fn wiki_share_refuses_reserved_drive_and_quarantined_pages() {
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    let wiki_dir = resolve_wiki_dir(tmp.path(), "alice").unwrap();
    ensure_wiki_dir(&wiki_dir).unwrap();
    fs::write(
        wiki_dir.join("concepts/q.md"),
        "---\ntitle: Q\ndo_not_inject: true\n---\nquarantined body",
    )
    .unwrap();

    let share = |p: &str| serde_json::json!({ "page_path": p });
    for bad in ["_index.md", "C:x.md", "C:\\x.md", "x\0.md"] {
        let r = handle_wiki_share(&share(bad), tmp.path(), "alice").await;
        assert!(is_err(&r), "{bad:?}: {r}");
        // Validation, not an existence probe.
        assert!(!r.to_string().contains("Page not found"), "{bad:?}: {r}");
    }
    let q = handle_wiki_share(&share("concepts/q.md"), tmp.path(), "alice").await;
    assert!(is_err(&q), "{q}");
    let leaked = fs::read_dir(tmp.path().join("shared/wiki/sources"))
        .map(|d| {
            d.flatten()
                .any(|e| fs::read_to_string(e.path()).is_ok_and(|c| c.contains("quarantined body")))
        })
        .unwrap_or(false);
    assert!(!leaked);
}

/// `depth` near u64::MAX used to keep the BFS spinning with an empty
/// frontier; integral floats (`10.0`) are accepted as integers.
#[tokio::test(flavor = "current_thread")]
async fn wiki_graph_depth_is_capped() {
    let tmp = TempDir::new();
    create_agent_dir(tmp.path(), "alice");
    let wiki_dir = resolve_wiki_dir(tmp.path(), "alice").unwrap();
    ensure_wiki_dir(&wiki_dir).unwrap();
    fs::write(wiki_dir.join("concepts/a.md"), "---\ntitle: A\nrelated: [concepts/b.md]\n---\n").unwrap();
    fs::write(wiki_dir.join("concepts/b.md"), "---\ntitle: B\n---\n").unwrap();

    let started = std::time::Instant::now();
    for depth in [
        serde_json::json!(10_000_000_000_000_000_000u64),
        serde_json::json!(1e19),
        serde_json::json!(10.0),
    ] {
        let r = handle_wiki_graph(
            &serde_json::json!({ "center": "concepts/a.md", "depth": depth }),
            tmp.path(),
            "alice",
        )
        .await;
        assert!(!is_err(&r), "{r}");
        assert!(r.to_string().contains("\\\"B\\\""), "{r}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}
