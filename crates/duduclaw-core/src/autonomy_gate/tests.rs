use super::*;
use crate::GuardDecision;

#[test]
fn config_refusal_classification_excludes_pauses_and_unrelated_reasons() {
    let path = AutonomyPath::GoalLoop;
    for reason in ["agent_missing", "agent_status_unreadable: invalid agent config"] {
        assert!(denied(path, reason.into(), None, None).is_agent_config_refusal());
    }
    for reason in [
        "agent_pause", "global_pause", "state_unreadable", "agent_not_operational",
        "ephemeral_parent_invalid", "invalid_agent_id",
        "prefix agent_status_unreadable: invalid agent config", "agent_status_unreadable_other",
    ] {
        assert!(!denied(path, reason.into(), None, None).is_agent_config_refusal());
    }
    let entry = PauseEntry { reason: "agent_status_unreadable: user text".into(),
        who: "operator".into(), at: chrono::Utc::now().to_rfc3339() };
    assert!(!denied(path, "agent_missing".into(), Some(entry), None).is_agent_config_refusal());
    assert!(!denied(path, "agent_missing".into(), None, Some("unreadable".into()))
        .is_agent_config_refusal());
}

#[cfg(unix)]
#[test]
fn existing_autonomy_files_are_tightened_before_use() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    set_pause(dir.path(), None, "maintenance", "operator").unwrap();
    resume(dir.path(), None).unwrap();
    for name in [STATE_FILE, INIT_FILE, ADMISSION_LOCK_FILE] {
        std::fs::set_permissions(
            dir.path().join(name),
            std::fs::Permissions::from_mode(0o666),
        ).unwrap();
    }
    assert!(status(dir.path()).state_error.is_none());
    let guard = lock_admission(dir.path(), false, ADMISSION_WAIT).unwrap();
    for name in [STATE_FILE, INIT_FILE, ADMISSION_LOCK_FILE] {
        assert_eq!(
            std::fs::metadata(dir.path().join(name)).unwrap().mode() & 0o777,
            0o600,
            "{name}",
        );
    }
    drop(guard);
}

#[cfg(unix)]
#[test]
fn multiply_linked_autonomy_files_are_refused_without_chmod() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for name in [STATE_FILE, INIT_FILE, ADMISSION_LOCK_FILE] {
        let dir = tempfile::tempdir().unwrap();
        set_pause(dir.path(), None, "maintenance", "operator").unwrap();
        resume(dir.path(), None).unwrap();
        let path = dir.path().join(name);
        let alias = dir.path().join("foreign-alias");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(open_regular(&path, false).is_err(), "{name}");
        assert_eq!(std::fs::metadata(&alias).unwrap().mode() & 0o777, 0o644);
        if name == ADMISSION_LOCK_FILE {
            assert!(lock_admission(dir.path(), false, ADMISSION_WAIT).is_err());
        } else {
            assert!(status(dir.path()).state_error.is_some(), "{name}");
        }
        assert!(set_pause(dir.path(), None, "blocked", "operator").is_err());
    }
}

#[cfg(unix)]
#[test]
fn atomic_state_replacement_reopens_detached_reader_at_each_fstat() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for replace_at in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        resume(dir.path(), None).unwrap();
        let path = dir.path().join(STATE_FILE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut inspections = 0;
        let file = open_regular_with_metadata(&path, false, |file| {
            inspections += 1;
            if inspections == replace_at {
                // Exercise the real writer between open/fchmod and fstat.
                set_pause(dir.path(), None, "replacement", "operator").unwrap();
                assert_eq!(file.metadata()?.nlink(), 0, "rename detaches the old inode");
            }
            file.metadata()
        }).unwrap();
        let state: DiskState = serde_json::from_reader(&file).unwrap();
        assert_eq!(state.global.unwrap().reason, "replacement");
        assert_eq!(file.metadata().unwrap().nlink(), 1);
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(inspections, replace_at + 2, "must validate a newly opened fd");
    }
}

#[cfg(unix)]
#[test]
fn detached_state_retry_still_refuses_hardlinks_and_missing_state() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for hardlink in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        resume(dir.path(), None).unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut replaced = false;
        let result = open_regular_with_metadata(&path, false, |file| {
            if !replaced {
                replaced = true;
                set_pause(dir.path(), None, "replacement", "operator").unwrap();
                assert_eq!(file.metadata()?.nlink(), 0);
                if hardlink {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
                    std::fs::hard_link(&path, dir.path().join("alias")).unwrap();
                } else {
                    std::fs::remove_file(&path).unwrap();
                }
            }
            file.metadata()
        });
        assert!(result.is_err());
        assert!(status(dir.path()).state_error.is_some());
        if hardlink {
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        }
    }
}

#[cfg(unix)]
#[test]
fn detached_state_retry_is_bounded_and_never_accepts_stale_state() {
    let dir = tempfile::tempdir().unwrap();
    resume(dir.path(), None).unwrap();
    let mut inspections = 0;
    let error = open_regular_with_metadata(&dir.path().join(STATE_FILE), false, |file| {
        inspections += 1;
        set_pause(dir.path(), None, "replacement", "operator").unwrap();
        file.metadata()
    }).unwrap_err();
    assert_eq!(inspections, 8);
    assert!(error.to_string().contains("changed repeatedly"));
}

#[cfg(unix)]
#[test]
fn foreign_autonomy_owner_is_refused_by_metadata_validation() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let file = open_regular(&dir.path().join(STATE_FILE), true).unwrap();
    let metadata = file.metadata().unwrap();
    // Exercise the ownership predicate without requiring root or chowning a
    // fixture away from the test runner. Production always passes geteuid().
    assert!(validate_unix_owner_and_links(&metadata, metadata.uid().wrapping_add(1)).is_err());
    assert!(validate_unix_owner_and_links(&metadata, metadata.uid()).is_ok());
}

fn agent(home: &Path, id: &str, status: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nstatus = \"{status}\"\n"),
    )
    .unwrap();
}

#[test]
fn autonomy_pause_resume_scopes_and_restart_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    agent(home, "alice", "active");
    agent(home, "bob", "active");
    let check = |id| super::check(home, id, AutonomyPath::Background);
    assert!(check("alice").is_ok());
    set_pause(home, Some("alice"), "維護🛠", "operator-1").unwrap();
    assert!(check("alice").is_err());
    assert!(check("bob").is_ok());
    // Reopen through the disk reader; no in-memory state participates.
    let reopened = read_state(home).unwrap();
    assert_eq!(reopened.agents["alice"].who, "operator-1");
    assert_eq!(reopened.agents["alice"].reason, "維護🛠");
    set_pause(home, None, "全員暫停", "admin").unwrap();
    resume(home, Some("alice")).unwrap();
    assert!(check("alice").is_err());
    assert!(check("bob").is_err());
    resume(home, None).unwrap();
    assert!(check("alice").is_ok());
    assert!(check("bob").is_ok());
}

#[test]
fn autonomy_corrupt_missing_and_unreadable_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    agent(home, "alice", "active");
    for bad in [
        "{",
        "{}",
        "null",
        r#"{"version":2,"global":null,"agents":{}}"#,
    ] {
        std::fs::write(home.join(STATE_FILE), bad).unwrap();
        assert!(status(home).state_error.is_some());
        assert!(check(home, "alice", AutonomyPath::Cron).is_err());
        assert!(check_global(home, AutonomyPath::Autopilot).is_err());
        assert!(resume(home, None).is_err());
        assert_eq!(std::fs::read_to_string(home.join(STATE_FILE)).unwrap(), bad);
    }
    std::fs::remove_file(home.join(STATE_FILE)).unwrap();
    std::fs::create_dir(home.join(STATE_FILE)).unwrap();
    assert!(status(home).state_error.is_some());
    std::fs::remove_dir(home.join(STATE_FILE)).unwrap();
    set_pause(home, None, "", "admin").unwrap();
    std::fs::remove_file(home.join(STATE_FILE)).unwrap();
    assert!(status(home).state_error.is_some());
}

#[test]
fn autonomy_status_is_live_and_resolved_config_only_narrows() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    for state in ["paused", "terminated", "archived", "deleted", "unknown"] {
        agent(home, "alice", state);
        assert!(check(home, "alice", AutonomyPath::Heartbeat).is_err());
    }
    agent(home, "alice", "active");
    assert!(check(home, "alice", AutonomyPath::Heartbeat).is_ok());
    let resolved = crate::preset::agent_resolved_path(home, "alice");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    std::fs::write(&resolved, "[agent]\nstatus='paused'\n").unwrap();
    assert!(check(home, "alice", AutonomyPath::Heartbeat).is_err());
    std::fs::write(&resolved, "[agent]\nstatus='active'\n").unwrap();
    agent(home, "alice", "paused");
    assert!(check(home, "alice", AutonomyPath::Heartbeat).is_err());
    assert!(check(home, "../alice", AutonomyPath::Heartbeat).is_err());
}

#[test]
fn autonomy_unicode_limit_and_concurrent_updates() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::thread::scope(|scope| {
        for n in 0..16 {
            scope.spawn(move || {
                set_pause(
                    home,
                    Some(&format!("agent-{n}")),
                    &"貓🐈".repeat(1000),
                    "admin",
                )
                .unwrap();
            });
        }
    });
    let state = status(home);
    assert_eq!(state.agents.len(), 16);
    assert!(
        state
            .agents
            .values()
            .all(|e| e.reason.chars().count() == MAX_REASON_CHARS)
    );
}

#[test]
fn autonomy_files_are_guarded_from_employee_writes() {
    use crate::org_field_guard::*;
    let dir = tempfile::tempdir().unwrap();
    for name in [STATE_FILE, INIT_FILE, ADMISSION_LOCK_FILE, "autonomy_pause.json.lock"] {
        assert_eq!(
            classify_identity_surface(&dir.path().join(name), dir.path()),
            Some(ProtectedSurface::AutonomyPause)
        );
        assert!(matches!(
            check_identity_surface_write_as(
                &dir.path().join(name),
                dir.path(),
                &HookCaller::Absent,
                None,
                Some("{}")
            ),
            GuardDecision::BlockedIdentitySurface { .. }
        ));
        assert!(matches!(
            check_bash_protected_write(&format!("rm {name}"), dir.path(), &HookCaller::Absent),
            GuardDecision::BlockedBashProtectedWrite { .. }
        ));
    }
}

#[test]
fn autonomy_error_does_not_disclose_malformed_content() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(STATE_FILE),
        r#"{"version":"private reason","agents":{}}"#,
    )
    .unwrap();
    let error = status(dir.path()).state_error.unwrap();
    assert!(!error.contains("private reason"));
}

#[cfg(unix)]
#[test]
fn autonomy_special_files_fail_closed_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    symlink("missing", dir.path().join(STATE_FILE)).unwrap();
    assert!(status(dir.path()).state_error.is_some());
    std::fs::remove_file(dir.path().join(STATE_FILE)).unwrap();
    let fifo_path = dir.path().join(STATE_FILE);
    let fifo_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo_path is a valid null-terminated path, and mkfifo does not retain the pointer.
    let rc = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    assert!(status(dir.path()).state_error.is_some());
    assert!(set_pause(dir.path(), None, "", "admin").is_err());
}

#[tokio::test]
async fn pause_waits_for_admission_and_next_admission_is_denied() {
    let dir = tempfile::tempdir().unwrap();
    agent(dir.path(), "alice", "active");
    let guard = admit(dir.path(), "alice", AutonomyPath::Background).await.unwrap();
    fn assert_send<T: Send>(_: &T) {}
    assert_send(&guard);
    let home = dir.path().to_path_buf();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut writer = tokio::task::spawn_blocking(move || {
        started_tx.send(()).unwrap();
        set_pause(&home, Some("alice"), "pause", "operator")
    });
    started_rx.await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut writer).await.is_err());
    assert!(status(dir.path()).agents.is_empty());
    drop(guard);
    writer.await.unwrap().unwrap();
    assert!(admit(dir.path(), "alice", AutonomyPath::Background).await.is_err());
}

#[test]
fn admission_wait_timeouts_fail_closed_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    agent(dir.path(), "alice", "active");
    let wait = std::time::Duration::from_millis(10);
    let shared = lock_admission(dir.path(), false, wait).unwrap();
    let outcome = mutate_with_durability(dir.path(), None, None, wait, || Ok(()));
    assert!(matches!(outcome, MutationOutcome::NotApplied(ref error)
        if error.kind() == io::ErrorKind::TimedOut && error.to_string().starts_with("admission_wait_timeout")));
    assert!(!dir.path().join(STATE_FILE).exists());
    drop(shared);
    let exclusive = lock_admission(dir.path(), true, wait).unwrap();
    let error = lock_admission(dir.path(), false, wait).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    drop(exclusive);
}

#[test]
fn rename_is_applied_even_when_directory_sync_or_readback_fails() {
    for failure in ["directory fsync failed", "readback failed"] {
        let dir = tempfile::tempdir().unwrap();
        agent(dir.path(), "alice", "active");
        let entry = PauseEntry {
            reason: "maintenance".into(),
            who: "operator".into(),
            at: chrono::Utc::now().to_rfc3339(),
        };
        let outcome = mutate_with_durability(
            dir.path(), Some("alice"), Some(entry), ADMISSION_WAIT,
            || Err(io::Error::other(failure)),
        );
        match outcome {
            MutationOutcome::AppliedButDurabilityUnknown(state, error) => {
                assert_eq!(state.agents["alice"].reason, "maintenance");
                assert_eq!(error.to_string(), failure);
            }
            other => panic!("rename must report applied: {other:?}"),
        }
        assert!(check(dir.path(), "alice", AutonomyPath::Background).is_err());
        // Resume has the same commit point and cannot be reported as not applied.
        let resumed = mutate_with_durability(dir.path(), Some("alice"), None, ADMISSION_WAIT,
            || Err(io::Error::other(failure)));
        assert!(matches!(resumed, MutationOutcome::AppliedButDurabilityUnknown(_, _)));
        assert!(check(dir.path(), "alice", AutonomyPath::Background).is_ok());
    }
}

#[tokio::test]
async fn timeout_reports_only_local_holder_counts_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    agent(dir.path(), "alice", "active");
    let guard = admit(dir.path(), "alice", AutonomyPath::Workflow).await.unwrap();
    let message = admission_timeout(dir.path()).to_string();
    assert!(message.starts_with("admission_wait_timeout:"));
    assert!(message.contains("local_holders=1"));
    assert!(message.contains("Workflow"));
    assert!(message.contains("other_process_holders=unknown"));
    assert!(!message.contains("alice"));
    drop(guard);
    assert!(admission_timeout(dir.path()).to_string().contains("local_holders=0"));
}

#[tokio::test]
async fn autonomy_receipt_cap_prunes_oldest_completed_and_refuses_all_pending() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let mut state = DiskState::default();
    for _ in 0..MAX_PENDING_CHANGE_RECEIPTS {
        state.receipts.push(ChangeReceipt::new(None, None, "admin").unwrap());
    }
    // Put the oldest completed receipt after a newer one in vector order.
    state.receipts[5].completion_recorded = true;
    state.receipts[5].applied_at = "2026-01-02T00:00:00Z".into();
    state.receipts[20].completion_recorded = true;
    state.receipts[20].applied_at = "2026-01-01T00:00:00Z".into();
    let old = state.receipts[20].change_id.clone();
    let newer = state.receipts[5].change_id.clone();
    let pending = state.receipts[0].change_id.clone();
    std::fs::write(home.join(STATE_FILE), serde_json::to_vec(&state).unwrap()).unwrap();
    let receipt = ChangeReceipt::new(None, None, "admin").unwrap();
    assert!(matches!(mutate_tracked_async(home, None, receipt).await, MutationOutcome::Applied(_)));
    let receipts = change_receipts(home).unwrap();
    assert_eq!(receipts.len(), MAX_PENDING_CHANGE_RECEIPTS);
    assert!(!receipts.iter().any(|r| r.change_id == old));
    assert!(receipts.iter().any(|r| r.change_id == newer));
    assert!(receipts.iter().any(|r| r.change_id == pending));

    // No completed receipt is available: neither pause nor resume may land.
    let mut state = read_state(home).unwrap();
    for receipt in &mut state.receipts { receipt.completion_recorded = false; }
    std::fs::write(home.join(STATE_FILE), serde_json::to_vec(&state).unwrap()).unwrap();
    let before = std::fs::read(home.join(STATE_FILE)).unwrap();
    for pause in [true, false] {
        let entry = pause.then(|| PauseEntry { reason: "test".into(), who: "admin".into(),
            at: chrono::Utc::now().to_rfc3339() });
        let receipt = ChangeReceipt::new(None, entry.as_ref(), "admin").unwrap();
        assert!(matches!(mutate_tracked_async(home, entry, receipt).await,
            MutationOutcome::NotApplied(_)));
        assert_eq!(std::fs::read(home.join(STATE_FILE)).unwrap(), before);
    }
}

#[test]
fn autonomy_invalid_receipts_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = serde_json::to_value(ChangeReceipt::new(None, None, "admin").unwrap()).unwrap();
    let mut invalid_receipts = Vec::new();
    for (field, value) in [
        ("change_id", serde_json::json!("x".repeat(500))),
        ("method", serde_json::json!("delete")),
        ("scope", serde_json::json!({"agent": "x".repeat(129)})),
        ("scope", serde_json::json!({"agent": "../sales"})),
        ("actor", serde_json::json!("x".repeat(257))),
        ("actor", serde_json::json!("")),
        ("actor", serde_json::json!("admin\nforged")),
        ("request_digest", serde_json::json!("f".repeat(63))),
        ("request_digest", serde_json::json!("g".repeat(64))),
        ("applied_at", serde_json::json!("not a date")),
        ("applied_at", serde_json::json!("1969-01-01T00:00:00Z")),
        ("completion_recorded", serde_json::json!("false")),
    ] {
        let mut bad = receipt.clone();
        bad[field] = value;
        invalid_receipts.push(serde_json::json!([bad]));
    }
    invalid_receipts.push(serde_json::json!([receipt.clone(), receipt]));
    for receipts in invalid_receipts {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1, "global": null, "agents": {}, "receipts": receipts
        })).unwrap();
        std::fs::write(dir.path().join(STATE_FILE), &bytes).unwrap();
        assert!(status(dir.path()).state_error.is_some());
        assert!(resume(dir.path(), None).is_err());
        assert_eq!(std::fs::read(dir.path().join(STATE_FILE)).unwrap(), bytes);
    }
}

#[test]
fn autonomy_receipt_schema_accepts_no_receipts_but_rejects_unmerged_token_format() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(STATE_FILE),
        r#"{"version":1,"global":null,"agents":{}}"#).unwrap();
    assert!(change_receipts(dir.path()).unwrap().is_empty());
    // applied_change_ids existed only in this unmerged feature. It cannot be
    // migrated safely without trusting audit rows; reject instead of guessing.
    std::fs::write(dir.path().join(STATE_FILE),
        r#"{"version":1,"global":null,"agents":{},"applied_change_ids":[]}"#).unwrap();
    assert!(status(dir.path()).state_error.is_some());
    assert!(resume(dir.path(), None).is_err());
}

#[tokio::test]
async fn autonomy_receipt_digest_binds_effective_request_and_scope() {
    let dir = tempfile::tempdir().unwrap();
    let entry = PauseEntry { reason: "maintenance".into(), who: "admin".into(),
        at: chrono::Utc::now().to_rfc3339() };
    let receipt = ChangeReceipt::new(Some("all"), Some(&entry), "admin").unwrap();
    assert_eq!(receipt.request_digest,
        ChangeReceipt::new(Some("all"), Some(&entry), "admin").unwrap().request_digest);
    assert_ne!(receipt.request_digest,
        ChangeReceipt::new(None, Some(&entry), "admin").unwrap().request_digest);
    let changed = PauseEntry { reason: "different".into(), ..entry.clone() };
    assert!(matches!(mutate_tracked_async(dir.path(), Some(changed), receipt.clone()).await,
        MutationOutcome::NotApplied(_)));
    assert!(!dir.path().join(STATE_FILE).exists());
    assert!(matches!(mutate_tracked_async(dir.path(), Some(entry), receipt.clone()).await,
        MutationOutcome::Applied(_)));
    let before = status(dir.path());
    mark_completion_recorded(dir.path(), &receipt.change_id).unwrap();
    assert_eq!(status(dir.path()).agents, before.agents);
    assert!(change_receipts(dir.path()).unwrap()[0].completion_recorded);
    // A duplicate token never commits a second mutation.
    assert!(matches!(mutate_tracked_async(dir.path(), None, receipt).await,
        MutationOutcome::NotApplied(_)));
}

#[tokio::test]
async fn autonomy_completion_sync_failure_restores_pending_without_changing_pause() {
    let dir = tempfile::tempdir().unwrap();
    let entry = PauseEntry { reason: "test".into(), who: "admin".into(),
        at: chrono::Utc::now().to_rfc3339() };
    let receipt = ChangeReceipt::new(None, Some(&entry), "admin").unwrap();
    assert!(matches!(mutate_tracked_async(dir.path(), Some(entry), receipt.clone()).await,
        MutationOutcome::Applied(_)));
    let before = status(dir.path());
    assert!(mark_completion_with_durability(dir.path(), &receipt.change_id,
        || Err(io::Error::other("injected post-rename sync failure"))).is_err());
    assert!(!change_receipts(dir.path()).unwrap()[0].completion_recorded);
    assert_eq!(status(dir.path()).global, before.global);
    mark_completion_recorded(dir.path(), &receipt.change_id).unwrap();
    assert!(change_receipts(dir.path()).unwrap()[0].completion_recorded);
}

fn ephemeral_agent(home: &Path, id: &str, parent: &str) -> PathBuf {
    let dir = home.join("agents/.ephemeral").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nstatus = 'active'\nreports_to = '{parent}'\n"),
    ).unwrap();
    std::fs::write(
        dir.join("ephemeral.toml"),
        format!(
            "parent = '{parent}'\ntier = 'standard'\ncreated_at = '2026-10-29T00:00:00Z'\nexpires_at = '2026-10-30T00:00:00Z'\n"
        ),
    ).unwrap();
    crate::org_store::upsert(home, id, crate::org_store::OrgEntry::new(parent, "")).unwrap();
    dir
}

#[tokio::test]
async fn autonomy_ephemeral_chain_inherits_pause_and_admission_lease() {
    let home = tempfile::tempdir().unwrap();
    agent(home.path(), "parent", "active");
    ephemeral_agent(home.path(), "eph-child", "parent");
    ephemeral_agent(home.path(), "eph-grandchild", "eph-child");
    let path = AutonomyPath::Background;
    assert_eq!(
        effective_identities(home.path(), "eph-grandchild", path).unwrap(),
        vec!["eph-grandchild", "eph-child", "parent"],
    );
    for paused in ["parent", "eph-child", "eph-grandchild"] {
        set_pause(home.path(), Some(paused), "test", "operator").unwrap();
        assert_eq!(check(home.path(), "eph-grandchild", path).unwrap_err().reason, "agent_pause");
        assert_eq!(admit(home.path(), "eph-grandchild", path).await.unwrap_err().reason, "agent_pause");
        resume(home.path(), Some(paused)).unwrap();
    }
    let guard = admit(home.path(), "eph-grandchild", path).await.unwrap();
    assert!(lock_admission(home.path(), true, std::time::Duration::ZERO).is_err());
    drop(guard);
    set_pause(home.path(), Some("parent"), "test", "operator").unwrap();
    assert!(admit(home.path(), "eph-grandchild", path).await.is_err());
}

#[test]
fn autonomy_ephemeral_missing_and_corrupt_parent_metadata_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    agent(home.path(), "parent", "active");
    let dir = ephemeral_agent(home.path(), "eph-child", "parent");
    let path = AutonomyPath::Background;
    let metadata_path = dir.join("ephemeral.toml");
    let metadata = std::fs::read_to_string(&metadata_path).unwrap();
    for invalid in ["", "parent = 'parent'", "parent = 42", "invalid = ["] {
        std::fs::write(&metadata_path, invalid).unwrap();
        assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    }
    std::fs::remove_file(&metadata_path).unwrap();
    assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    std::fs::write(&metadata_path, metadata).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = 42\n").unwrap();
    assert!(check(home.path(), "eph-child", path).unwrap_err().reason.starts_with("agent_status_unreadable"));
    std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = 'active'\n").unwrap();
    std::fs::remove_file(home.path().join("agents/parent/agent.toml")).unwrap();
    assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "agent_missing");
}

#[test]
fn autonomy_ephemeral_requires_host_authority_and_refuses_cycles_and_depth() {
    let home = tempfile::tempdir().unwrap();
    agent(home.path(), "parent", "active");
    ephemeral_agent(home.path(), "eph-child", "parent");
    let path = AutonomyPath::Background;
    let org_path = home.path().join(crate::org_store::ORG_STORE_FILE);
    let org = std::fs::read_to_string(&org_path).unwrap();
    for invalid in ["", "invalid = ["] {
        std::fs::write(&org_path, invalid).unwrap();
        assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    }
    std::fs::remove_file(&org_path).unwrap();
    assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    std::fs::write(&org_path, org).unwrap();
    crate::org_store::upsert(home.path(), "eph-child", crate::org_store::OrgEntry::new("another", "")).unwrap();
    assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    ephemeral_agent(home.path(), "eph-child", "eph-other");
    ephemeral_agent(home.path(), "eph-other", "eph-child");
    assert_eq!(check(home.path(), "eph-child", path).unwrap_err().reason, "ephemeral_parent_invalid");
    for index in 0..32 {
        let parent = if index == 31 { "parent".to_string() } else { format!("eph-depth-{}", index + 1) };
        ephemeral_agent(home.path(), &format!("eph-depth-{index}"), &parent);
    }
    assert!(check(home.path(), "eph-depth-1", path).is_ok(), "32 total identities are allowed");
    assert_eq!(check(home.path(), "eph-depth-0", path).unwrap_err().reason, "ephemeral_parent_invalid");
}

#[test]
fn autonomy_regular_employee_does_not_inherit_org_parent_pause() {
    let home = tempfile::tempdir().unwrap();
    agent(home.path(), "parent", "active");
    agent(home.path(), "employee", "active");
    crate::org_store::upsert(home.path(), "employee", crate::org_store::OrgEntry::new("parent", "")).unwrap();
    set_pause(home.path(), Some("parent"), "test", "operator").unwrap();
    assert_eq!(effective_identities(home.path(), "employee", AutonomyPath::Background).unwrap(), vec!["employee"]);
    assert!(check(home.path(), "employee", AutonomyPath::Background).is_ok());
}

#[test]
fn autonomy_ephemeral_lifecycle_uses_scaffold_not_regular_shadow() {
    let home = tempfile::tempdir().unwrap();
    agent(home.path(), "parent", "active");
    let dir = ephemeral_agent(home.path(), "eph-child", "parent");
    agent(home.path(), "eph-child", "active");
    std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = 'paused'\n").unwrap();
    assert_eq!(
        check(home.path(), "eph-child", AutonomyPath::Background).unwrap_err().reason,
        "agent_not_operational",
    );
    std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = 'active'\n").unwrap();
    assert!(check(home.path(), "eph-child", AutonomyPath::Background).is_ok());
}

#[tokio::test]
async fn lifecycle_barrier_drains_admissions_before_status_or_removal() {
    for status in ["paused", "archived", "deleted", "terminated", "remove"] {
        let dir = tempfile::tempdir().unwrap();
        agent(dir.path(), "alice", "active");
        let config = dir.path().join("agents/alice/agent.toml");
        let original = std::fs::read(&config).unwrap();
        let guard = admit(dir.path(), "alice", AutonomyPath::Background).await.unwrap();
        let home = dir.path().to_path_buf();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut writer = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            with_exclusive_barrier(&home, || {
                if status == "remove" {
                    std::fs::remove_dir_all(home.join("agents/alice"))
                } else {
                    std::fs::write(home.join("agents/alice/agent.toml"),
                        format!("[agent]\nstatus = \"{status}\"\n"))
                }
            })
        });
        started_rx.await.unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut writer).await.is_err());
        assert_eq!(std::fs::read(&config).unwrap(), original);
        drop(guard);
        writer.await.unwrap().unwrap();
        assert!(admit(dir.path(), "alice", AutonomyPath::Background).await.is_err());
        if status == "remove" { assert!(!config.exists()); }
    }
}

#[test]
fn lifecycle_barrier_timeout_leaves_config_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    agent(dir.path(), "alice", "active");
    let config = dir.path().join("agents/alice/agent.toml");
    let original = std::fs::read(&config).unwrap();
    let shared = admit_blocking(dir.path(), "alice", AutonomyPath::Background).unwrap();
    let result = (|| -> io::Result<()> {
        let _lease = exclusive_barrier_with_wait(dir.path(), std::time::Duration::from_millis(10))?;
        std::fs::write(&config, "[agent]\nstatus = \"paused\"\n")
    })();
    let error = result.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("admission_wait_timeout"));
    assert_eq!(std::fs::read(&config).unwrap(), original);
    drop(shared);
}

#[test]
fn lifecycle_detection_does_not_gate_resume_or_non_status_edits() {
    let active = "[agent]\nstatus = 'active'\n";
    let paused = "[agent]\nstatus = 'paused'\n";
    assert!(lifecycle_barrier_required(active, paused));
    assert!(lifecycle_barrier_required(active, "[agent]\n"));
    assert!(!lifecycle_barrier_required(paused, active));
    assert!(!lifecycle_barrier_required(active, "[agent]\nstatus = 'active'\nicon = 'x'\n"));
    assert!(!lifecycle_barrier_required(paused, "[agent]\nstatus = 'paused'\nicon = 'x'\n"));
}
