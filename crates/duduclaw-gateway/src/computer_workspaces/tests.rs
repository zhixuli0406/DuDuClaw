//! Registry and file tests (design §10.1, fake-backend parts of HT1–HT5).
#![cfg(unix)]

use std::os::unix::fs::symlink;

use super::files::{self, FileError, WriteRequest};
use super::store::{AcquireRequest, StoreError};
use super::*;
use crate::fs_safe::WriteStep;

const RUNNER: &str = "local-docker:0123456789abcdef0123456789abcdef";

fn home_with(agents: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for a in agents {
        let dir = tmp.path().join("agents").join(a);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\ncomputer_use = true\n",
        )
        .unwrap();
    }
    tmp
}

fn cfg() -> WorkspacesConfig {
    WorkspacesConfig {
        enabled: true,
        min_free_bytes: 0,
        ..WorkspacesConfig::default()
    }
}

fn ready(home: &Path, store: &WorkspaceStore, owner: &str) -> String {
    let id = store.create(owner, RUNNER, unix_now(), 3).unwrap();
    paths::create_workspace_dirs(home, &id).unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Creating],
            WorkspaceState::Ready,
            "system:test",
            "created",
            None,
        )
        .unwrap();
    id
}

fn acquire(
    store: &WorkspaceStore,
    id: &str,
    caller: &str,
    holder: &str,
    now: i64,
) -> Result<Lease, StoreError> {
    store
        .acquire(&AcquireRequest {
            workspace_id: id,
            caller,
            runner_id: RUNNER,
            holder,
            instance: "inst",
            now,
            ttl_secs: LEASE_TTL_SECS,
            retention_days: 30,
        })
        .map(|(l, _)| l)
}

fn write(
    home: &Path,
    store: &WorkspaceStore,
    lease: &Lease,
    path: &str,
    body: &str,
) -> Result<files::WriteOutcome, FileError> {
    files::write_file(
        home,
        store,
        &cfg(),
        &WriteRequest {
            lease,
            rel_path: path,
            content: body,
            expected_revision: None,
            now: unix_now(),
        },
        &|_| Ok(()),
    )
}

#[test]
fn lease_cas_release_by_an_old_holder_changes_nothing() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let now = unix_now();
    let a = acquire(&store, &id, "alice", "cu-a", now).unwrap();
    assert_eq!(
        acquire(&store, &id, "alice", "cu-b", now),
        Err(StoreError::Busy)
    );
    assert!(store.renew(&a, now, LEASE_TTL_SECS).unwrap());
    assert!(store.release(&a).unwrap());
    // B takes over; A's late release / renew must not touch B's lease.
    let b = acquire(&store, &id, "alice", "cu-b", now).unwrap();
    assert!(b.epoch > a.epoch);
    assert!(!store.release(&a).unwrap());
    assert!(!store.renew(&a, now, LEASE_TTL_SECS).unwrap());
    assert!(store.lease_current(&b, now).unwrap());
}

#[test]
fn other_owner_gets_not_found_and_runner_mismatch_never_rebinds() {
    let home = home_with(&["alice", "bob"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let now = unix_now();
    assert_eq!(
        acquire(&store, &id, "bob", "cu-b", now),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        acquire(
            &store,
            "ws-0123456789abcdef0123456789abcdef",
            "bob",
            "cu-b",
            now
        ),
        Err(StoreError::NotFound)
    );
    let mismatch = store.acquire(&AcquireRequest {
        workspace_id: &id,
        caller: "alice",
        runner_id: "local-docker:ffffffffffffffffffffffffffffffff",
        holder: "cu-a",
        instance: "i",
        now,
        ttl_secs: LEASE_TTL_SECS,
        retention_days: 30,
    });
    assert_eq!(mismatch.map(|_| ()), Err(StoreError::RunnerMismatch));
    assert_eq!(store.get(&id).unwrap().unwrap().runner_id, RUNNER);
}

#[test]
fn two_registries_on_one_home_admit_exactly_one_holder() {
    let home = home_with(&["alice"]);
    let s1 = WorkspaceStore::open(home.path()).unwrap();
    let s2 = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &s1, "alice");
    let now = unix_now();
    let r1 = acquire(&s1, &id, "alice", "cu-1", now);
    let r2 = acquire(&s2, &id, "alice", "cu-2", now);
    assert_eq!([r1.is_ok(), r2.is_ok()].iter().filter(|x| **x).count(), 1);
}

#[test]
fn fence_revoke_and_expiry_end_the_lease() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let now = unix_now();
    let a = acquire(&store, &id, "alice", "cu-a", now).unwrap();
    store.fence(&id, "operator:t", "test").unwrap();
    assert!(!store.lease_current(&a, now).unwrap());
    let b = acquire(&store, &id, "alice", "cu-b", now).unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Ready],
            WorkspaceState::Revoked,
            "operator:t",
            "revoked",
            None,
        )
        .unwrap();
    assert!(!store.lease_current(&b, now).unwrap());
    assert_eq!(
        acquire(&store, &id, "alice", "cu-c", now),
        Err(StoreError::State(WorkspaceState::Revoked))
    );
    let row = store
        .transition(
            &id,
            &[WorkspaceState::Revoked],
            WorkspaceState::Ready,
            "operator:t",
            "regranted",
            None,
        )
        .unwrap();
    assert!(row.permission_revision > b.permission_revision);
    // An expired lease is reclaimed by the sweep and the next acquire works.
    let c = acquire(&store, &id, "alice", "cu-c", now - 1000).unwrap();
    assert!(!store.lease_current(&c, now).unwrap());
    assert_eq!(store.expire_stale_leases(now).unwrap(), 1);
    assert!(acquire(&store, &id, "alice", "cu-d", now).is_ok());
}

#[test]
fn per_agent_quota_counts_live_workspaces() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    for _ in 0..2 {
        store.create("alice", RUNNER, unix_now(), 2).unwrap();
    }
    assert_eq!(
        store.create("alice", RUNNER, unix_now(), 2),
        Err(StoreError::Quota)
    );
}

#[test]
fn write_read_round_trip_and_revisions() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let out = write(home.path(), &store, &lease, "reports/週報 (1).md", "內容").unwrap();
    assert_eq!(out.data_revision, 1);
    let (text, sha) = files::read_file(home.path(), &id, "reports/週報 (1).md").unwrap();
    assert_eq!(text, "內容");
    assert_eq!(sha, out.sha256);
    // `expected_revision` mismatch refuses without touching the file.
    let stale = files::write_file(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "reports/週報 (1).md",
            content: "x",
            expected_revision: Some(0),
            now: unix_now(),
        },
        &|_| Ok(()),
    );
    assert_eq!(stale.map(|_| ()), Err(FileError::RevisionMismatch(1)));
    assert_eq!(
        files::read_file(home.path(), &id, "reports/週報 (1).md")
            .unwrap()
            .0,
        "內容"
    );
}

#[test]
fn bad_paths_are_refused_and_nothing_lands_outside_data() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    for bad in [
        "../x",
        "/etc/x",
        "a/../../x",
        "a\0b",
        ".hidden",
        "a/b/c/d/e",
        "",
        "a//b",
        "c:x",
        "a\\b",
        " a",
        "a\nb",
    ] {
        assert_eq!(
            write(home.path(), &store, &lease, bad, "x").map(|_| ()),
            Err(FileError::InvalidPath),
            "{bad:?}"
        );
    }
    let ws = home.path().join("computer_workspaces").join(&id);
    let names: Vec<_> = std::fs::read_dir(&ws)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("data")]);
    assert!(
        files::list_files(home.path(), &store, &id)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn a_planted_symlink_in_data_is_never_followed() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), "KEEP").unwrap();
    let data = home
        .path()
        .canonicalize()
        .unwrap()
        .join("computer_workspaces")
        .join(&id)
        .join("data");
    symlink(outside.path().join("victim"), data.join("f")).unwrap();
    symlink(outside.path(), data.join("dir")).unwrap();
    assert!(write(home.path(), &store, &lease, "f", "x").is_err());
    assert!(write(home.path(), &store, &lease, "dir/g", "x").is_err());
    assert!(files::read_file(home.path(), &id, "f").is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("victim")).unwrap(),
        "KEEP"
    );
    assert!(!outside.path().join("g").exists());
}

#[test]
fn injected_failures_keep_old_bytes_and_revision() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    write(home.path(), &store, &lease, "a.txt", "old").unwrap();
    for step in [WriteStep::Write, WriteStep::Sync, WriteStep::Rename] {
        let fault = move |s: WriteStep| {
            if s == step {
                Err(std::io::Error::from_raw_os_error(28))
            } else {
                Ok(())
            }
        };
        let r = files::write_file(
            home.path(),
            &store,
            &cfg(),
            &WriteRequest {
                lease: &lease,
                rel_path: "a.txt",
                content: "new",
                expected_revision: None,
                now: unix_now(),
            },
            &fault,
        );
        assert_eq!(r.map(|_| ()), Err(FileError::DiskFull), "{step:?}");
        assert_eq!(
            files::read_file(home.path(), &id, "a.txt").unwrap().0,
            "old"
        );
        assert_eq!(store.get(&id).unwrap().unwrap().data_revision, 1);
        assert!(store.list_intents().unwrap().is_empty());
    }
}

#[test]
fn a_crash_after_rename_is_reconciled_at_boot() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let r = files::write_file_inner(
        home.path(),
        &store,
        &cfg(),
        &WriteRequest {
            lease: &lease,
            rel_path: "a.txt",
            content: "landed",
            expected_revision: None,
            now: unix_now(),
        },
        &|_| Ok(()),
        false,
    );
    assert!(r.is_err());
    assert_eq!(store.get(&id).unwrap().unwrap().data_revision, 0);
    assert_eq!(store.list_intents().unwrap().len(), 1);
    drop(store);
    reconcile_registry(home.path()).unwrap();
    let store = WorkspaceStore::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(row.data_revision, 1);
    assert_eq!(row.files_used, 1);
    assert!(store.list_intents().unwrap().is_empty());
    assert_eq!(
        store.ids_with_event("reconciled_after_crash").unwrap(),
        vec![id]
    );
}

#[test]
fn quota_refuses_without_deleting_anything() {
    let home = home_with(&["alice"]);
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let lease = acquire(&store, &id, "alice", "cu-a", unix_now()).unwrap();
    let tight = WorkspacesConfig {
        max_files: 1,
        ..cfg()
    };
    let req = |p: &'static str| WriteRequest {
        lease: &lease,
        rel_path: p,
        content: "x",
        expected_revision: None,
        now: unix_now(),
    };
    files::write_file(home.path(), &store, &tight, &req("a"), &|_| Ok(())).unwrap();
    // Overwriting the same file stays within quota; a second file does not.
    files::write_file(home.path(), &store, &tight, &req("a"), &|_| Ok(())).unwrap();
    assert!(matches!(
        files::write_file(home.path(), &store, &tight, &req("b"), &|_| Ok(())),
        Err(FileError::Quota { .. })
    ));
    assert_eq!(
        files::list_files(home.path(), &store, &id)
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[test]
fn retention_expiry_keeps_files_and_blocks_attach() {
    let home = home_with(&["alice"]);
    std::fs::write(
        home.path().join("config.toml"),
        "[computer_use.workspaces]\nenabled = true\nretention_days = 1\n",
    )
    .unwrap();
    let store = WorkspaceStore::open(home.path()).unwrap();
    let id = ready(home.path(), &store, "alice");
    let past = unix_now() - 3 * 86_400;
    let (lease, _) = store
        .acquire(&AcquireRequest {
            workspace_id: &id,
            caller: "alice",
            runner_id: RUNNER,
            holder: "cu-a",
            instance: "i",
            now: past,
            ttl_secs: LEASE_TTL_SECS,
            retention_days: 1,
        })
        .unwrap();
    std::fs::write(
        home.path()
            .canonicalize()
            .unwrap()
            .join("computer_workspaces")
            .join(&id)
            .join("data/keep.txt"),
        "k",
    )
    .unwrap();
    store.release(&lease).unwrap();
    let expired = reconcile_registry(home.path()).unwrap();
    assert_eq!(expired, vec![(id.clone(), "alice".to_string())]);
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Expired
    );
    assert_eq!(
        acquire(&store, &id, "alice", "cu-b", unix_now()),
        Err(StoreError::State(WorkspaceState::Expired))
    );
    assert!(paths::verify_mount_source(home.path(), &id).is_ok());
    assert_eq!(
        files::read_file(home.path(), &id, "keep.txt").unwrap().0,
        "k"
    );
}

#[test]
fn owner_removal_is_detected_and_fails_closed() {
    let home = home_with(&["alice"]);
    let now = unix_now();
    assert!(!owner_removed(home.path(), "alice", now));
    let trash = home.path().join("agents").join("_trash");
    std::fs::create_dir_all(&trash).unwrap();
    // Removed before the workspace was made: a previous incarnation.
    std::fs::create_dir(trash.join("alice_20200101000000")).unwrap();
    assert!(!owner_removed(home.path(), "alice", now));
    let later = chrono::DateTime::from_timestamp(now + 5, 0)
        .unwrap()
        .format("%Y%m%d%H%M%S")
        .to_string();
    std::fs::create_dir(trash.join(format!("alice_{later}"))).unwrap();
    assert!(owner_removed(home.path(), "alice", now));
    assert!(owner_removed(home.path(), "nobody", now));
}

#[test]
fn runner_id_is_stable_and_scoped() {
    let a = runner_id_from("home1", "daemon");
    assert_eq!(a, runner_id_from("home1", "daemon"));
    assert_ne!(a, runner_id_from("home2", "daemon"));
    assert!(a.starts_with("local-docker:") && a.len() == "local-docker:".len() + 32);
}

#[test]
fn doctor_verdicts() {
    use super::doctor::{Facts, verdict};
    use duduclaw_core::types::CheckStatus;
    let base = || Facts {
        config: Ok(WorkspacesConfig {
            enabled: true,
            ..WorkspacesConfig::default()
        }),
        unix: true,
        root_problem: None,
        registry: Ok(Vec::new()),
        attention: Vec::new(),
        runner_known: true,
        free_bytes: Some(u64::MAX),
    };
    assert_eq!(verdict(&base()).0, CheckStatus::Pass);
    let off = Facts {
        config: Ok(WorkspacesConfig::default()),
        ..base()
    };
    assert!(verdict(&off).1.contains("未啟用"));
    assert_eq!(
        verdict(&Facts {
            config: Err("bad".into()),
            ..base()
        })
        .0,
        CheckStatus::Fail
    );
    assert_eq!(
        verdict(&Facts {
            unix: false,
            ..base()
        })
        .0,
        CheckStatus::Fail
    );
    assert_eq!(
        verdict(&Facts {
            root_problem: Some(Some("x")),
            ..base()
        })
        .0,
        CheckStatus::Fail
    );
    assert_eq!(
        verdict(&Facts {
            registry: Err("x".into()),
            ..base()
        })
        .0,
        CheckStatus::Fail
    );
    assert_eq!(
        verdict(&Facts {
            runner_known: false,
            ..base()
        })
        .0,
        CheckStatus::Warn
    );
    assert_eq!(
        verdict(&Facts {
            free_bytes: Some(1),
            ..base()
        })
        .0,
        CheckStatus::Warn
    );
    let att = Facts {
        attention: vec![("ws-1".into(), "刪除未完成")],
        ..base()
    };
    assert!(verdict(&att).1.contains("ws-1"));
}

/// Preparing an existing registry never opens and closes the file, so a
/// POSIX lock this process holds on it (SQLite's WAL locks) survives; an
/// unwritable existing file is only chmod-ed, never opened.
#[test]
fn preparing_an_existing_registry_keeps_this_process_posix_locks() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("computer_workspaces.db");
    std::fs::write(&path, b"").unwrap();
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let write_lock = || {
        // SAFETY: `flock` is plain data; all-zero is a valid value.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as libc::c_short;
        fl.l_whence = libc::SEEK_SET as libc::c_short;
        fl.l_start = 0;
        fl.l_len = 1;
        fl
    };
    let fl = write_lock();
    // SAFETY: valid fd and a valid `flock`.
    assert_eq!(
        unsafe { libc::fcntl(holder.as_raw_fd(), libc::F_SETLK, &fl) },
        0
    );

    super::store::prepare_db_file(&path).unwrap();

    // Another process sees whether the lock is still held. The child only
    // makes async-signal-safe calls before `_exit`.
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut probe = write_lock();
    // SAFETY: fork in a test; the child calls only open/fcntl/_exit.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            let fd = libc::open(c_path.as_ptr(), libc::O_RDWR);
            let held = fd >= 0
                && libc::fcntl(fd, libc::F_GETLK, &mut probe) == 0
                && probe.l_type != libc::F_UNLCK as libc::c_short;
            libc::_exit(if held { 0 } else { 1 });
        }
    }
    assert!(pid > 0, "fork failed");
    let mut status = 0;
    // SAFETY: waiting for our own child.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "the POSIX lock was dropped by preparing the registry (status {status})"
    );
    drop(holder);

    // An existing file nobody may open is only chmod-ed back to 0600.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    super::store::prepare_db_file(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[path = "concurrency_tests.rs"]
mod concurrency;

#[path = "review2_tests.rs"]
mod review2;
