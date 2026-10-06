//! One writer per workspace, across processes (review H1), size limits and
//! unprocessable entries (review H3), retention and event cap (M4, L9).
//!
//! Every interleaving is forced with channels at the write's own stop
//! points (`WriteStep`, `CreateStep`); nothing sleeps. Each thread opens its
//! own registry connection, as a second process would.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::computer_workspaces::lock::{self, CreateStep};

fn write_with(
    home: &Path,
    lease: &Lease,
    path: &str,
    body: &str,
    expected: Option<i64>,
    cfg: &WorkspacesConfig,
    fault: &dyn Fn(WriteStep) -> std::io::Result<()>,
) -> Result<files::WriteOutcome, FileError> {
    let store = WorkspaceStore::open(home).unwrap();
    files::write_file(
        home,
        &store,
        cfg,
        &WriteRequest {
            lease,
            rel_path: path,
            content: body,
            expected_revision: expected,
            now: unix_now(),
        },
        fault,
    )
}

/// Start a write on its own thread that stops at `at` until released.
/// Returns once the writer is parked there (holding the workspace lock).
fn parked_write(
    home: &Path,
    lease: &Lease,
    path: &'static str,
    body: &'static str,
    expected: Option<i64>,
    cfg: WorkspacesConfig,
    at: WriteStep,
) -> (
    mpsc::Sender<()>,
    thread::JoinHandle<Result<files::WriteOutcome, FileError>>,
) {
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (home, lease) = (home.to_path_buf(), lease.clone());
    let handle = thread::spawn(move || {
        let fault = |step: WriteStep| {
            if step == at {
                reached_tx.send(()).unwrap();
                go_rx.recv().unwrap();
            }
            Ok(())
        };
        write_with(&home, &lease, path, body, expected, &cfg, &fault)
    });
    reached_rx.recv().unwrap();
    (go_tx, handle)
}

#[test]
fn two_writes_with_the_same_expected_revision_one_wins_nothing_lost() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let (go_a, a) = parked_write(
        home.path(),
        &lease,
        "r.md",
        "A",
        Some(0),
        cfg(),
        WriteStep::Write,
    );
    // A holds the workspace lock: nobody else can take it now.
    assert!(
        lock::lock_workspace(home.path(), &id, Duration::ZERO)
            .unwrap()
            .is_none()
    );
    let (h, l) = (home.path().to_path_buf(), lease.clone());
    let b = thread::spawn(move || write_with(&h, &l, "r.md", "B", Some(0), &cfg(), &|_| Ok(())));
    go_a.send(()).unwrap();
    let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
    assert_eq!(ra.as_ref().map(|o| o.data_revision), Ok(1), "{ra:?}");
    assert_eq!(rb, Err(FileError::RevisionMismatch(1)));
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(
        (row.data_revision, row.files_used, row.bytes_used),
        (1, 1, 1)
    );
    assert_eq!(files::read_file(home.path(), &id, "r.md").unwrap().0, "A");
    assert!(store.list_intents().unwrap().is_empty());
}

#[test]
fn concurrent_writes_cannot_exceed_the_quota() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let tight = WorkspacesConfig {
        max_files: 1,
        ..cfg()
    };
    let (go_a, a) = parked_write(
        home.path(),
        &lease,
        "a.md",
        "A",
        None,
        tight.clone(),
        WriteStep::Sync,
    );
    let (h, l, t) = (home.path().to_path_buf(), lease.clone(), tight.clone());
    let b = thread::spawn(move || write_with(&h, &l, "b.md", "B", None, &t, &|_| Ok(())));
    go_a.send(()).unwrap();
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(FileError::Quota { .. })))
            .count(),
        1
    );
    let listing = files::list_files(home.path(), &store, &id).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(store.get(&id).unwrap().unwrap().files_used, 1);
}

#[test]
fn reconciliation_at_every_write_stage_leaves_the_write_intact() {
    for at in [
        WriteStep::Write,
        WriteStep::Sync,
        WriteStep::Rename,
        WriteStep::AfterRename,
    ] {
        let home = home_with(&["alice"]);
        let store = WorkspaceStore::open(home.path()).unwrap();
        let id = ready(home.path(), &store, "alice");
        let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
        let (go, w) = parked_write(home.path(), &lease, "d/f.md", "body", Some(0), cfg(), at);
        reconcile_registry(home.path()).unwrap();
        // The live writer's intent is left alone.
        assert_eq!(store.list_intents().unwrap().len(), 1, "{at:?}");
        go.send(()).unwrap();
        let out = w.join().unwrap();
        assert_eq!(out.map(|o| o.data_revision), Ok(1), "{at:?}");
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(
            (row.data_revision, row.files_used, row.bytes_used),
            (1, 1, 4),
            "{at:?}"
        );
        assert_eq!(
            files::read_file(home.path(), &id, "d/f.md").unwrap().0,
            "body"
        );
        assert!(store.list_intents().unwrap().is_empty());
        for kind in [
            "write_abandoned_after_crash",
            "write_outcome_unknown",
            "reconciled_after_crash",
        ] {
            assert!(
                store.ids_with_event(kind).unwrap().is_empty(),
                "{at:?} {kind}"
            );
        }
    }
}

#[test]
fn a_write_whose_intent_was_settled_elsewhere_never_bumps_twice() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let (go, w) = parked_write(
        home.path(),
        &lease,
        "f.md",
        "x",
        None,
        cfg(),
        WriteStep::AfterRename,
    );
    // Someone settles the intent behind the writer's back.
    let intent = store.list_intents().unwrap().remove(0);
    store
        .reconcile_intent(&intent, (store::EMPTY_MANIFEST, 1, 1), true, "test_settled")
        .unwrap();
    go.send(()).unwrap();
    assert_eq!(w.join().unwrap(), Err(FileError::Unavailable));
    assert_eq!(store.get(&id).unwrap().unwrap().data_revision, 1);
}

#[test]
fn a_workspace_being_created_is_never_removed_by_reconciliation() {
    for pause in [CreateStep::AfterInsert, CreateStep::AfterMkdir] {
        let home = home_with(&["alice"]);
        let (reached_tx, reached_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let h = home.path().to_path_buf();
        let creator = thread::spawn(move || {
            let store = WorkspaceStore::open(&h).unwrap();
            let step = |s: CreateStep| {
                if s == pause {
                    reached_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                }
            };
            lock::create_workspace(&h, &store, "alice", RUNNER, unix_now(), 3, &step)
        });
        reached_rx.recv().unwrap();
        reconcile_registry(home.path()).unwrap();
        let store = WorkspaceStore::open(home.path()).unwrap();
        let row = store.list_all().unwrap().remove(0);
        assert_eq!(row.state, WorkspaceState::Creating, "{pause:?}");
        go_tx.send(()).unwrap();
        let id = creator.join().unwrap().unwrap();
        assert_eq!(id, row.workspace_id);
        assert_eq!(
            store.get(&id).unwrap().unwrap().state,
            WorkspaceState::Ready
        );
        assert!(paths::open_data(home.path(), &id).unwrap().is_some());
        assert!(store.ids_with_event("failed_create").unwrap().is_empty());
    }
}

#[test]
fn an_abandoned_creating_row_is_cleaned_once_its_lock_is_free() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    // A creator that died after the insert: no lock held any more.
    let id = store.create("alice", RUNNER, unix_now(), 3).unwrap();
    paths::create_workspace_dirs(home.path(), &id).unwrap();
    reconcile_registry(home.path()).unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::FailedCreate
    );
    assert!(paths::open_data(home.path(), &id).unwrap().is_none());
}

#[test]
fn unprocessable_entries_are_counted_never_read_and_never_fail_the_list() {
    use std::os::unix::fs::symlink;
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    write(home.path(), &store, &lease, "ok.md", "fine").unwrap();
    let data = home
        .path()
        .join("computer_workspaces")
        .join(&id)
        .join("data");
    std::fs::write(data.join("big.md"), vec![b'x'; files::MAX_FILE_BYTES + 1]).unwrap();
    std::fs::write(data.join("orig.md"), "o").unwrap();
    std::fs::hard_link(data.join("orig.md"), data.join("twin.md")).unwrap();
    symlink("/etc/passwd", data.join("link.md")).unwrap();
    std::fs::write(data.join("back\\slash.md"), "b").unwrap();
    std::fs::write(data.join("colon:name.md"), "c").unwrap();
    let listing = files::list_files(home.path(), &store, &id).unwrap();
    let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(names, vec!["ok.md"]);
    // big, orig + twin (2 links each), link, backslash, colon.
    assert_eq!(listing.unprocessable, 6);
    assert_eq!(
        files::read_file(home.path(), &id, "twin.md"),
        Err(FileError::Unprocessable)
    );
    assert_eq!(
        files::read_file(home.path(), &id, "big.md"),
        Err(FileError::TooLarge)
    );
    assert!(files::read_file(home.path(), &id, "link.md").is_err());
    // A write onto a hard-linked name is refused, the other link untouched.
    assert_eq!(
        write(home.path(), &store, &lease, "twin.md", "new"),
        Err(FileError::Unprocessable)
    );
    assert_eq!(std::fs::read_to_string(data.join("orig.md")).unwrap(), "o");
}

#[test]
fn list_and_write_with_1000_files_do_not_rehash_the_tree() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let wide = WorkspacesConfig {
        max_files: 2000,
        ..cfg()
    };
    let body = "x".repeat(2048);
    fn req<'a>(lease: &'a Lease, p: &'a str, body: &'a str) -> WriteRequest<'a> {
        WriteRequest {
            lease,
            rel_path: p,
            content: body,
            expected_revision: None,
            now: unix_now(),
        }
    }
    let filling = Instant::now();
    for i in 0..1000 {
        let p = format!("d{}/f{i}.md", i % 10);
        files::write_file(home.path(), &store, &wide, &req(&lease, &p, &body), &|_| {
            Ok(())
        })
        .unwrap();
    }
    let fill = filling.elapsed();
    let t = Instant::now();
    let listing = files::list_files(home.path(), &store, &id).unwrap();
    let list = t.elapsed();
    assert_eq!(listing.entries.len(), files::LIST_LIMIT);
    assert!(listing.more);
    assert_eq!(listing.unprocessable, 0);
    let t = Instant::now();
    files::write_file(
        home.path(),
        &store,
        &wide,
        &req(&lease, "d0/extra.md", &body),
        &|_| Ok(()),
    )
    .unwrap();
    let one = t.elapsed();
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!((row.files_used, row.data_revision), (1001, 1001));
    println!(
        "MEASURE 1000-files fill={fill:?} per_write_avg={:?} list={list:?} write_1001st={one:?}",
        fill / 1000
    );
}

#[test]
fn retention_never_expires_a_workspace_with_a_live_lease() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let now = unix_now();
    let _lease = acquire(&store, &id, "alice", "cu-a", now).unwrap();
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE workspaces SET expires_at = ?2 WHERE workspace_id = ?1",
            rusqlite::params![id, now - 10],
        )
        .unwrap();
    assert!(store.expire_retention(now).unwrap().is_empty());
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Ready
    );
    // Once the lease lapsed it expires.
    assert_eq!(
        store
            .expire_retention(now + LEASE_TTL_SECS + 1)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Expired
    );
}

#[test]
fn the_event_table_is_capped() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    for _ in 0..30 {
        store
            .note(&id, "test_note", "system:test", serde_json::json!({}))
            .unwrap();
    }
    store.prune_events_with(unix_now(), 86_400, 10).unwrap();
    let n: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM workspace_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 10);
    // Age: everything older than the window goes.
    store.prune_events_with(unix_now() + 10, 5, 1000).unwrap();
    let n: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM workspace_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

/// [`parked_write`] with the default test config (used by `review2`).
pub(super) fn parked_write_for(
    home: &Path,
    lease: &Lease,
    path: &'static str,
    body: &'static str,
    expected: Option<i64>,
    at: WriteStep,
) -> (
    mpsc::Sender<()>,
    thread::JoinHandle<Result<files::WriteOutcome, FileError>>,
) {
    parked_write(home, lease, path, body, expected, cfg(), at)
}

/// [`write_with`] with the default test config and no fault.
pub(super) fn write_with_for(
    home: &Path,
    lease: &Lease,
    path: &str,
    body: &str,
    expected: Option<i64>,
) -> Result<files::WriteOutcome, FileError> {
    write_with(home, lease, path, body, expected, &cfg(), &|_| Ok(()))
}
