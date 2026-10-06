//! Second independent review (appendix C): M-1, M-4, M-5, first-round L5,
//! security L8, L-4, L-9 and the "no registry on unused installs" rule.

use std::sync::mpsc;
use std::thread;

use super::concurrency::{parked_write_for, write_with_for};
use super::*;
use crate::computer_workspaces::lock;

fn created(home: &Path, store: &WorkspaceStore, owner: &str) -> String {
    lock::create_workspace(home, store, owner, RUNNER, unix_now(), 3, &|_| {}).unwrap()
}

#[test]
fn m1_a_write_that_lands_after_a_fence_is_recorded_and_blocks_stale_revisions() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let (go, w) = parked_write_for(
        home.path(),
        &lease,
        "r.md",
        "late",
        Some(0),
        WriteStep::AfterRename,
    );
    store.fence(&id, "operator:t", "test").unwrap();
    go.send(()).unwrap();
    assert_eq!(w.join().unwrap(), Err(FileError::LandedAfterFence));
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(
        (row.data_revision, row.files_used, row.bytes_used),
        (1, 1, 4)
    );
    assert!(store.list_intents().unwrap().is_empty());
    assert_eq!(
        store.ids_with_event("write_landed_after_fence").unwrap(),
        vec![id.clone()]
    );
    // A new session holding the old revision cannot overwrite it.
    let lease2 = acquire(&store, &id, "alice", "cu-b", unix_now()).unwrap();
    assert_eq!(
        write_with_for(home.path(), &lease2, "r.md", "stale", Some(0)),
        Err(FileError::RevisionMismatch(1))
    );
    assert_eq!(
        files::read_file(home.path(), &id, "r.md").unwrap().0,
        "late"
    );
}

#[test]
fn m4_reconciliation_rehashes_a_same_size_replacement() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    write(home.path(), &store, &lease, "a.md", "AAAA").unwrap();
    // Same size, different bytes; the process dies before the registry update.
    let r = files::write_file_inner(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "a.md",
            content: "BBBB",
            expected_revision: None,
            now: unix_now(),
        },
        &|_| Ok(()),
        false,
    );
    assert!(r.is_err());
    reconcile_registry(home.path()).unwrap();
    let listing = files::list_files(home.path(), &store, &id).unwrap();
    let (_, sha_b) = files::read_file(home.path(), &id, "a.md").unwrap();
    assert_eq!(
        listing.entries[0].sha256, sha_b,
        "ledger must not keep A's hash"
    );
    assert_eq!(store.get(&id).unwrap().unwrap().data_revision, 2);
    assert_eq!(
        store.ids_with_event("reconciled_after_crash").unwrap(),
        vec![id]
    );
}

#[test]
fn m4_an_unreadable_target_is_listed_with_an_unknown_hash() {
    use std::os::unix::fs::PermissionsExt;
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    write(home.path(), &store, &lease, "a.md", "AAAA").unwrap();
    let r = files::write_file_inner(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "a.md",
            content: "BBBB",
            expected_revision: None,
            now: unix_now(),
        },
        &|_| Ok(()),
        false,
    );
    assert!(r.is_err());
    let file = home
        .path()
        .join("computer_workspaces")
        .join(&id)
        .join("data/a.md");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads anything; the check below only means something otherwise.
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    reconcile_registry(home.path()).unwrap();
    let listing = files::list_files(home.path(), &store, &id).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(
        listing.entries[0].sha256.is_empty(),
        "hash reported unknown"
    );
    assert_eq!(
        store.ids_with_event("write_outcome_unknown").unwrap(),
        vec![id]
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn l5_an_unreadable_data_dir_keeps_usage_ledger_and_intent() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    write(home.path(), &store, &lease, "a.md", "keep").unwrap();
    let r = files::write_file_inner(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "b.md",
            content: "x",
            expected_revision: None,
            now: unix_now(),
        },
        &|_| Ok(()),
        false,
    );
    assert!(r.is_err());
    let ws = home.path().join("computer_workspaces").join(&id);
    std::fs::rename(ws.join("data"), ws.join("moved")).unwrap();
    for _ in 0..2 {
        reconcile_registry(home.path()).unwrap();
    }
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!((row.bytes_used, row.files_used), (4, 1), "usage untouched");
    assert_eq!(store.ledger(&id).unwrap().len(), 1, "ledger untouched");
    assert_eq!(store.list_intents().unwrap().len(), 1, "intent kept");
    assert_eq!(
        store.ids_with_event("data_unreadable").unwrap(),
        vec![id.clone()]
    );
    let n: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM workspace_events WHERE kind = 'data_unreadable'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "noted once a day, not every sweep");
    // Back in place: the next pass settles it.
    std::fs::rename(ws.join("moved"), ws.join("data")).unwrap();
    reconcile_registry(home.path()).unwrap();
    assert!(store.list_intents().unwrap().is_empty());
}

#[test]
fn m5_a_recreated_employee_of_the_same_name_does_not_get_the_workspace() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = created(home.path(), &store, "alice");
    let row = store.get(&id).unwrap().unwrap();
    assert!(row.owner_credential.is_some());
    assert!(owner_cred::matches(home.path(), &row));
    assert!(!orphan_if_owner_removed(home.path(), &store, &row));
    // alice is removed, `_trash` emptied by hand, a new alice is created.
    let dir = home.path().join("agents").join("alice");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "[capabilities]\ncomputer_use = true\n",
    )
    .unwrap();
    assert!(!owner_removed(home.path(), "alice", row.created_at));
    assert!(orphan_if_owner_removed(home.path(), &store, &row));
    let after = store.get(&id).unwrap().unwrap();
    assert_eq!(after.state, WorkspaceState::Orphaned);
    assert_eq!(
        after.state_reason.as_deref(),
        Some("owner_credential_mismatch")
    );
}

#[test]
fn m5_a_row_without_a_credential_or_a_changed_one_fails_closed() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let legacy = ready(home.path(), &store, "alice");
    let row = store.get(&legacy).unwrap().unwrap();
    assert!(row.owner_credential.is_none());
    assert!(!owner_cred::matches(home.path(), &row));
    let id = created(home.path(), &store, "alice");
    owner_cred::record(home.path(), "alice", &id, &owner_cred::new_credential()).unwrap();
    assert!(!owner_cred::matches(
        home.path(),
        &store.get(&id).unwrap().unwrap()
    ));
}

#[test]
fn security_l8_expired_workspaces_do_not_take_a_slot() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let a = created(home.path(), &store, "alice");
    let _b = lock::create_workspace(home.path(), &store, "alice", RUNNER, unix_now(), 2, &|_| {})
        .unwrap();
    assert_eq!(
        lock::create_workspace(home.path(), &store, "alice", RUNNER, unix_now(), 2, &|_| {}),
        Err(StoreError::Quota)
    );
    store
        .transition(
            &a,
            &[WorkspaceState::Ready],
            WorkspaceState::Expired,
            "t",
            "expired",
            None,
        )
        .unwrap();
    assert!(
        lock::create_workspace(home.path(), &store, "alice", RUNNER, unix_now(), 2, &|_| {})
            .is_ok()
    );
}

#[test]
fn l4_renewing_an_expired_workspace_sets_state_and_deadline_together() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    store
        .transition(
            &id,
            &[WorkspaceState::Ready],
            WorkspaceState::Expired,
            "t",
            "expired",
            None,
        )
        .unwrap();
    let now = unix_now();
    let row = store.renew_retention(&id, "operator:t", now, 7).unwrap();
    assert_eq!(row.state, WorkspaceState::Ready);
    assert_eq!(row.expires_at, Some(now + 7 * 86_400));
    assert_eq!(store.get(&id).unwrap().unwrap(), row);
    assert_eq!(
        store.renew_retention("ws-00000000000000000000000000000000", "operator:t", now, 7),
        Err(StoreError::NotFound)
    );
}

#[test]
fn l9_a_halted_session_aborts_before_the_rename_with_its_own_error() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let out = files::write_file(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "a.md",
            content: "x",
            expected_revision: None,
            now: unix_now(),
        },
        &|step| {
            if step == WriteStep::Rename {
                Err(files::write_halted())
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(out, Err(FileError::SessionHalted));
    assert_eq!(
        files::read_file(home.path(), &id, "a.md"),
        Err(FileError::FileNotFound)
    );
}

#[test]
fn reconciliation_never_creates_the_registry_on_an_unused_install() {
    let home = home_with(&["alice"]);
    assert_eq!(reconcile_registry(home.path()).unwrap(), Vec::new());
    assert!(!home.path().join(paths::DB_FILE).exists());
}

#[test]
fn concurrent_creates_never_exceed_max_per_agent() {
    // Two creators race for the last slot: exactly one gets it.
    let home = home_with(&["alice"]);
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for _ in 0..2 {
        let (h, tx) = (home.path().to_path_buf(), tx.clone());
        handles.push(thread::spawn(move || {
            let store = WorkspaceStore::open(&h).unwrap();
            tx.send(lock::create_workspace(
                &h,
                &store,
                "alice",
                RUNNER,
                unix_now(),
                1,
                &|_| {},
            ))
            .unwrap();
        }));
    }
    drop(tx);
    for h in handles {
        h.join().unwrap();
    }
    let results: Vec<_> = rx.iter().collect();
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
}
