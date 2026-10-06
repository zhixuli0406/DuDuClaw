//! Regression locks for the 2026-09-28 audit of `mcp.rs`'s raw
//! `home_dir.join("agents").join(...)` sites. A team role member is an
//! `eph-*` agent living at `<home>/agents/.ephemeral/<id>/`, NOT at
//! `<home>/agents/<id>` — every path derived from the CALLER's identity
//! has to say so.

use super::*;
use crate::mcp::caller_shims::handle_tasks_list;
use std::fs;

const EPH: &str = "eph-agnes-r1-executor-ab12";

fn tmp_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Scaffold a role member exactly where `ephemeral::scaffold_role_member`
/// puts one.
fn mk_scaffold(home: &Path, id: &str, agent_toml: &str) -> std::path::PathBuf {
    let dir = home
        .join("agents")
        .join(duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME)
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("agent.toml"), agent_toml).unwrap();
    dir
}

/// Compare two paths that may differ only in symlink spelling (macOS
/// tempdirs are `/var/…` → `/private/var/…`). The leaf need not exist, so
/// canonicalize the parent and re-attach the file name.
fn same_path(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> std::path::PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(leaf)) => match parent.canonicalize() {
                Ok(c) => c.join(leaf),
                Err(_) => p.to_path_buf(),
            },
            _ => p.to_path_buf(),
        }
    }
    norm(a) == norm(b)
}

#[test]
fn caller_agent_dir_and_agent_dir_for_id_both_see_the_scaffold() {
    let home = tmp_home();
    let scaffold = mk_scaffold(home.path(), EPH, "[team_member]\nrole = \"executor\"\n");
    assert!(same_path(&caller_agent_dir(home.path(), EPH), &scaffold));
    assert!(same_path(&agent_dir_for_id(home.path(), EPH), &scaffold));
    // An ordinary registry agent is byte-identical to the bare join.
    assert_eq!(
        agent_dir_for_id(home.path(), "agnes"),
        home.path().join("agents").join("agnes")
    );
}

/// `wiki_*` derives its directory from the caller. Before the fix a role
/// member's pages landed in a freshly *created* `agents/eph-…/` registry
/// directory — outside its own scaffold, and outliving the scaffold's GC.
#[test]
fn eph_caller_wiki_dir_lands_inside_the_scaffold_and_mints_nothing() {
    let home = tmp_home();
    let scaffold = mk_scaffold(home.path(), EPH, "[team_member]\nrole = \"planner\"\n");

    let wiki = resolve_wiki_dir(home.path(), EPH).expect("a live scaffold must resolve");
    assert!(
        same_path(&wiki, &scaffold.join("wiki")),
        "wiki dir must sit inside the scaffold, got {}",
        wiki.display()
    );
    assert!(
        !home.path().join("agents").join(EPH).exists(),
        "a registry directory must never be minted for an ephemeral id"
    );
}

/// Fail-closed companion: an `eph-` id with no live scaffold is refused
/// rather than materialised as a registry directory.
#[test]
fn eph_caller_without_a_scaffold_is_refused_not_materialised() {
    let home = tmp_home();
    let ghost = "eph-deadbeef1234";
    let err = resolve_wiki_dir(home.path(), ghost).expect_err("must refuse");
    assert!(err.contains("no live scaffold"), "{err}");
    assert!(!home.path().join("agents").join(ghost).exists());
}

/// The ordinary-agent path keeps BUG-QA-003's auto-create behavior.
#[test]
fn ordinary_agent_wiki_dir_is_still_auto_created() {
    let home = tmp_home();
    let wiki = resolve_wiki_dir(home.path(), "claude-desktop").expect("must resolve");
    assert_eq!(
        wiki,
        home.path()
            .join("agents")
            .join("claude-desktop")
            .join("wiki")
    );
    assert!(home.path().join("agents").join("claude-desktop").is_dir());
}

/// Wiki ACLs are read for the page's OWNER — same resolution, so owner and
/// ACL come from one directory instead of two.
#[test]
fn eph_target_wiki_visibility_reads_the_scaffold_agent_toml() {
    let home = tmp_home();
    mk_scaffold(
        home.path(),
        EPH,
        "[team_member]\nrole = \"planner\"\n\n[capabilities]\nwiki_visible_to = [\"agnes\"]\n",
    );
    assert!(
        check_wiki_visibility(home.path(), EPH, "agnes").unwrap(),
        "listed reader must be allowed"
    );
    assert!(
        !check_wiki_visibility(home.path(), EPH, "mallory").unwrap(),
        "unlisted reader must be denied — before the fix the scaffold's \
             agent.toml was invisible and everything defaulted to open"
    );
}

/// Every `resolve_agent_department` call site passes the CALLER.
#[test]
fn eph_caller_department_resolves_from_the_scaffold() {
    let home = tmp_home();
    mk_scaffold(
        home.path(),
        EPH,
        "[agent]\ndepartment = \"engineering\"\n\n[team_member]\nrole = \"executor\"\n",
    );
    assert_eq!(
        resolve_agent_department(home.path(), EPH),
        Some("engineering".to_string())
    );
}

/// `working_state_*` is the assignment's named caller-path family. The
/// store already understood `.ephemeral/`; this pins it end-to-end through
/// the MCP handlers so a future refactor of either half cannot drift.
#[tokio::test]
async fn eph_caller_working_state_round_trips_inside_the_scaffold() {
    let home = tmp_home();
    let scaffold = mk_scaffold(home.path(), EPH, "[team_member]\nrole = \"executor\"\n");

    let set = handle_working_state_set(
        &serde_json::json!({
            "key": "stop_loss",
            "value": "3 lines",
            "reason": "round 1 decision",
        }),
        home.path(),
        EPH,
    )
    .await;
    assert_ne!(set["isError"], serde_json::json!(true), "{set}");

    assert!(
        scaffold.join("state").join("working_state.json").is_file(),
        "state must be written inside the scaffold"
    );
    assert!(
        !home.path().join("agents").join(EPH).exists(),
        "no registry directory may appear"
    );

    let got = handle_working_state_get(&serde_json::json!({}), home.path(), EPH).await;
    let text = got["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("stop_loss"), "{text}");
}

/// `tasks_*` is keyed by agent **id** in a home-level SQLite store, so
/// there is no directory to resolve — an `eph-*` caller reaches its own
/// queue with no scaffold on disk at all. Pinned so nobody "fixes" it into
/// a path join.
#[tokio::test]
async fn eph_caller_tasks_list_is_id_keyed_not_directory_keyed() {
    let home = tmp_home();
    let out = handle_tasks_list(&serde_json::json!({}), home.path(), EPH).await;
    assert_ne!(out["isError"], serde_json::json!(true), "{out}");
    let text = out["content"][0]["text"].as_str().unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(text).expect("json payload");
    assert_eq!(parsed["filtered_by_agent"], serde_json::json!(EPH));
    assert!(
        !home.path().join("agents").join(EPH).exists(),
        "tasks_list must not touch the agent registry at all"
    );
}
