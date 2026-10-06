//! Tests of `duduclaw ops computer-workspaces` (review H2, L5).

use super::*;
use duduclaw_core::HookCaller;
use duduclaw_gateway::computer_workspaces::{WorkspaceState, WorkspaceStore, paths};

#[test]
fn refuses_when_any_session_variable_is_present() {
    assert!(agent_session_refusal(|_| None).is_none());
    assert!(agent_session_refusal(|_| Some("  ".into())).is_none());
    for key in SESSION_ENV_VARS {
        let k = key.to_string();
        assert!(
            agent_session_refusal(|q| (q == k).then(|| "x".to_string())).is_some(),
            "{key}"
        );
    }
}

#[test]
fn the_terminal_note_never_claims_an_immediate_stop() {
    assert!(!NO_BARRIER_NOTE.contains("立即停止"));
    assert!(NO_BARRIER_NOTE.contains("跑完") && NO_BARRIER_NOTE.contains("15 秒"));
}

#[test]
fn terminal_answers_carry_no_barrier_time() {
    let v = without_barrier(serde_json::json!({"ok": true, "barrier_at": 5}));
    assert!(v.get("barrier_at").is_none());
    assert_eq!(v["ok"], true);
}

#[test]
fn bash_rule_blocks_employees_and_unverified_callers() {
    let positives = [
        "duduclaw ops computer-workspaces delete ws-1 --confirm",
        "/usr/local/bin/duduclaw ops computer-workspaces regrant ws-1",
        "duduclaw-pro --home /tmp/x ops computer-workspaces renew ws-1",
        "cd /tmp && 'duduclaw' ops computer-workspaces list",
        "echo hi; DUDUCLAW.EXE ops computer-workspaces fence ws-1",
    ];
    for cmd in positives {
        for caller in [
            HookCaller::Agent("alice".into()),
            HookCaller::Untrusted("alice".into()),
        ] {
            let d = bash_workspace_ops_decision(cmd, &caller);
            assert!(
                matches!(
                    d,
                    Some(duduclaw_core::GuardDecision::BlockedOperatorCommand { ref caller, .. })
                        if caller == "alice"
                ),
                "{cmd}"
            );
        }
    }
    let negatives = [
        "duduclaw ops tunnel",
        "duduclaw agent create bob",
        "echo duduclaw ops",
        "grep computer-workspaces README.md",
        "duduclawx ops computer-workspaces list",
    ];
    for cmd in negatives {
        assert!(
            bash_workspace_ops_decision(cmd, &HookCaller::Agent("alice".into())).is_none(),
            "{cmd}"
        );
    }
    // The operator (no claimed identity) is not judged here.
    assert!(
        bash_workspace_ops_decision(
            "duduclaw ops computer-workspaces delete ws-1 --confirm",
            &HookCaller::Absent
        )
        .is_none()
    );
}

fn revoked(home: &std::path::Path) -> String {
    std::fs::create_dir_all(home.join("agents/alice")).unwrap();
    std::fs::write(
        home.join("agents/alice/agent.toml"),
        "[agent]\nname = \"alice\"\n",
    )
    .unwrap();
    let store = WorkspaceStore::open(home).unwrap();
    let id = store.create("alice", "local-docker:x", 1, 3).unwrap();
    paths::create_workspace_dirs(home, &id).unwrap();
    for (from, to) in [
        (WorkspaceState::Creating, WorkspaceState::Ready),
        (WorkspaceState::Ready, WorkspaceState::Revoked),
    ] {
        store.transition(&id, &[from], to, "t", "t", None).unwrap();
    }
    id
}

#[tokio::test]
// The workspace registry is unix-only (Windows refuses every entry point), so
// these store-backed cases run on unix only.
#[cfg_attr(not(unix), ignore)]
async fn regrant_runs_only_after_a_dashboard_approval() {
    let home = tempfile::tempdir().unwrap();
    let id = revoked(home.path());
    let cmd = || ComputerWorkspaceCommands::Regrant {
        workspace_id: id.clone(),
    };
    let refused = run_with_env(home.path(), cmd(), |_| None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains(GO_TO_DASHBOARD), "{refused}");
    let store = WorkspaceStore::open(home.path()).unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Revoked
    );

    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home.path()).unwrap();
    let rec = broker
        .list_by_kind(cli_approval::ACTION_KIND)
        .await
        .unwrap()
        .remove(0);
    broker
        .decide(&rec.id, true, "dashboard:admin-1")
        .await
        .unwrap();
    run_with_env(home.path(), cmd(), |_| None).await.unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Ready
    );
}

/// Approve the single pending `computer_workspace_admin` request as an Admin.
async fn approve_pending(home: &std::path::Path) {
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home).unwrap();
    let pending: Vec<_> = broker
        .list_by_kind(cli_approval::ACTION_KIND)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.status == duduclaw_gateway::approval::ApprovalStatus::Pending)
        .collect();
    assert_eq!(pending.len(), 1, "{pending:?}");
    broker
        .decide(&pending[0].id, true, "dashboard:admin-1")
        .await
        .unwrap();
}

fn cli_audit_phases(home: &std::path::Path) -> Vec<(String, String)> {
    duduclaw_security::audit::read_recent_events(home, 100)
        .into_iter()
        .filter(|e| e.event_type == "computer_workspace_cli_action")
        .map(|e| {
            (
                e.details["action"].as_str().unwrap_or("").to_string(),
                e.details["phase"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
// The workspace registry is unix-only (Windows refuses every entry point), so
// these store-backed cases run on unix only.
#[cfg_attr(not(unix), ignore)]
async fn revoke_from_the_terminal_waits_for_a_dashboard_approval() {
    let home = tempfile::tempdir().unwrap();
    let id = revoked(home.path());
    let store = WorkspaceStore::open(home.path()).unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Revoked],
            WorkspaceState::Ready,
            "t",
            "t",
            None,
        )
        .unwrap();
    let cmd = || ComputerWorkspaceCommands::Revoke {
        workspace_id: id.clone(),
    };
    let refused = run_with_env(home.path(), cmd(), |_| None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains(GO_TO_DASHBOARD), "{refused}");
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Ready,
        "not applied"
    );
    approve_pending(home.path()).await;
    run_with_env(home.path(), cmd(), |_| None).await.unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Revoked
    );
    let conn = rusqlite::Connection::open(home.path().join(paths::DB_FILE)).unwrap();
    let actor: String = conn
        .query_row(
            "SELECT actor FROM workspace_events WHERE workspace_id = ?1 AND kind = 'revoked' \
             ORDER BY id DESC LIMIT 1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actor, UNVERIFIED_ACTOR);
    let phases = cli_audit_phases(home.path());
    assert!(
        phases.contains(&("revoke".into(), "requested".into())),
        "{phases:?}"
    );
    assert!(
        phases.contains(&("revoke".into(), "applied".into())),
        "{phases:?}"
    );
}

#[tokio::test]
// The workspace registry is unix-only (Windows refuses every entry point), so
// these store-backed cases run on unix only.
#[cfg_attr(not(unix), ignore)]
async fn fence_from_the_terminal_waits_for_a_dashboard_approval() {
    let home = tempfile::tempdir().unwrap();
    let id = revoked(home.path());
    let store = WorkspaceStore::open(home.path()).unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Revoked],
            WorkspaceState::Ready,
            "t",
            "t",
            None,
        )
        .unwrap();
    let epoch = store.get(&id).unwrap().unwrap().lease_epoch;
    let cmd = || ComputerWorkspaceCommands::Fence {
        workspace_id: id.clone(),
        reason: "operator_fence".into(),
    };
    let refused = run_with_env(home.path(), cmd(), |_| None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains(GO_TO_DASHBOARD), "{refused}");
    assert_eq!(
        store.get(&id).unwrap().unwrap().lease_epoch,
        epoch,
        "not applied"
    );
    approve_pending(home.path()).await;
    run_with_env(home.path(), cmd(), |_| None).await.unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().lease_epoch,
        epoch + 1,
        "fenced"
    );
    let phases = cli_audit_phases(home.path());
    assert!(
        phases.contains(&("fence".into(), "requested".into())),
        "{phases:?}"
    );
    assert!(
        phases.contains(&("fence".into(), "applied".into())),
        "{phases:?}"
    );
}

#[tokio::test]
// The workspace registry is unix-only (Windows refuses every entry point), so
// these store-backed cases run on unix only.
#[cfg_attr(not(unix), ignore)]
async fn a_refused_terminal_action_is_audited() {
    let home = tempfile::tempdir().unwrap();
    let id = revoked(home.path());
    let err = run_with_env(
        home.path(),
        ComputerWorkspaceCommands::Fence {
            workspace_id: id.clone(),
            reason: "x".into(),
        },
        |k| (k == "DUDUCLAW_AGENT_ID").then(|| "alice".to_string()),
    )
    .await;
    assert!(err.is_err());
    assert!(cli_audit_phases(home.path()).contains(&("fence".into(), "refused".into())));
    // `list` changes nothing and needs no approval.
    run_with_env(
        home.path(),
        ComputerWorkspaceCommands::List { owner: None },
        |_| None,
    )
    .await
    .unwrap();
}
