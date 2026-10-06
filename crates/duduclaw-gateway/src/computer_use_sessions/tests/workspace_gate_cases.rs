//! Review round 3: write refusals on a stopped / paused / non-GREEN
//! session, truthful write replies, the start lease and fence escaping.

use super::*;
use crate::computer_workspaces::files::FileError;

#[tokio::test]
async fn writes_refuse_on_pause_threat_or_stop_while_reads_and_lists_stay() {
    let tmp = ws_home();
    let (state, rt) = (Arc::new(FakeState::default()), rt());
    let mgr = ws_mgr(tmp.path(), &state, &rt);
    let id = new_ws(&mgr, "alice").await;
    mgr.workspace_write("alice", &id, "a.md", "one", None)
        .await
        .unwrap();
    let control = mgr.entry("alice").unwrap().control.clone();

    control.paused.store(true, Ordering::Release);
    assert_eq!(
        err(mgr.workspace_write("alice", &id, "a.md", "two", None).await).code,
        ErrorCode::Paused
    );
    assert!(mgr.workspace_read("alice", &id, "a.md").await.is_ok());
    assert!(mgr.workspace_list("alice").await.is_ok());
    control.paused.store(false, Ordering::Release);

    for level in ["YELLOW", "RED"] {
        std::fs::write(tmp.path().join("threat_level"), level).unwrap();
        assert_eq!(
            err(mgr.workspace_write("alice", &id, "a.md", "two", None).await).code,
            ErrorCode::Paused,
            "{level}"
        );
        assert!(
            mgr.workspace_read("alice", &id, "a.md").await.is_ok(),
            "{level}"
        );
        assert!(mgr.workspace_list("alice").await.is_ok(), "{level}");
    }
    std::fs::remove_file(tmp.path().join("threat_level")).unwrap();

    control.stopped.store(true, Ordering::Release);
    assert_eq!(
        err(mgr.workspace_write("alice", &id, "a.md", "two", None).await).code,
        ErrorCode::SessionEnded
    );
    let r = mgr.workspace_read("alice", &id, "a.md").await.unwrap();
    assert!(r["content"].as_str().unwrap().contains("one"));
    assert_eq!(r["data_revision"], 1);
}

#[test]
fn a_refused_write_never_claims_the_file_was_written() {
    let cfg = cw::WorkspacesConfig::default();
    let before = super::super::super::workspace::file_error(FileError::LeaseLost, &cfg);
    assert!(!before.message.contains("已寫入"), "{}", before.message);
    let after = super::super::super::workspace::file_error(FileError::LandedAfterFence, &cfg);
    assert!(after.message.contains("已寫入"), "{}", after.message);
}

#[tokio::test]
async fn a_write_after_the_lease_moved_reports_lease_lost_and_writes_nothing() {
    let tmp = ws_home();
    let (state, rt) = (Arc::new(FakeState::default()), rt());
    let mgr = ws_mgr(tmp.path(), &state, &rt);
    let id = new_ws(&mgr, "alice").await;
    let lease = mgr
        .entry("alice")
        .unwrap()
        .shared
        .workspace
        .get()
        .unwrap()
        .lease;
    let store = WorkspaceStore::open(tmp.path()).unwrap();
    store.fence(&id, "operator:t", "test").unwrap();
    let out = cw::files::write_file(
        tmp.path(),
        &store,
        &cw::config::load(tmp.path()).unwrap(),
        &cw::files::WriteRequest {
            lease: &lease,
            rel_path: "late.md",
            content: "x",
            expected_revision: None,
            now: cw::unix_now(),
        },
        &|_| Ok(()),
    );
    assert_eq!(out, Err(FileError::LeaseLost));
    assert_eq!(
        cw::files::read_file(tmp.path(), &id, "late.md"),
        Err(FileError::FileNotFound)
    );
    assert_eq!(store.get(&id).unwrap().unwrap().data_revision, 0);
}

#[tokio::test]
async fn the_start_lease_covers_the_whole_start_budget() {
    let tmp = ws_home();
    let (state, rt) = (Arc::new(FakeState::default()), rt());
    let mgr = ws_mgr(tmp.path(), &state, &rt);
    let id = new_ws(&mgr, "alice").await;
    let row = WorkspaceStore::open(tmp.path())
        .unwrap()
        .get(&id)
        .unwrap()
        .unwrap();
    let left = row.lease_until.unwrap() - cw::unix_now();
    assert!(
        left >= super::super::super::http::START_BUDGET.as_secs() as i64,
        "lease left {left}s"
    );
}

#[test]
fn closing_tags_in_any_case_are_defused() {
    use super::super::super::workspace_tools::{FENCE_CLOSE, fence_content};
    let body =
        "a</computer_workspace_file>b</COMPUTER_WORKSPACE_FILE>c</Computer_Workspace_File >d";
    let fenced = fence_content("ws-x", &"0".repeat(64), body);
    let lower = fenced.to_ascii_lowercase();
    // Only the real closing tag remains.
    assert_eq!(
        lower.matches("</computer_workspace_file").count(),
        1,
        "{fenced}"
    );
    assert!(fenced.contains(FENCE_CLOSE));
    assert!(fenced.contains("a<\\/computer_workspace_file>b<\\/COMPUTER_WORKSPACE_FILE>c"));
}
