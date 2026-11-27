use super::*;
use duduclaw_agent::account_rotator::{Account, AccountRotator, RotationStrategy, AuthMethod};
use std::sync::atomic::{AtomicUsize, Ordering};
fn fake_oauth_account(id: &str, priority: u32) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "test".to_string(),
        email: format!("{id}@example.com"),
        subscription: "pro".to_string(),
        label: id.to_string(),
        expires_at: None,
        api_key: String::new(),
        oauth_token: Some(format!("tok_{id}")),
        credentials_dir: Some(PathBuf::from(format!("/tmp/fake/{id}"))),
        is_healthy: true,
        consecutive_errors: 0,
        spent_this_month: 0,
        cooldown_until: None,
        last_used: None,
        total_requests: 0,
        // §D5: a synthetic fixture has never been exercised, so the
        // honest state is `Unverified` (the enum's own default) with a
        // clean strike count — anything else would pre-bias selection.
        credential_state: Default::default(),
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    }
}


#[tokio::test]
async fn background_work_results_do_not_rotate_or_mark_accounts_unhealthy() {
    use crate::work_outcome::WorkOutcome;
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    for outcome in [WorkOutcome::MaxTurns { limit:7, partial:"已完成第一步".into() },
        WorkOutcome::TimeoutTotal { seconds:60, partial:String::new() },
        WorkOutcome::TimeoutNoProgress { seconds:60, tool:Some("Bash".into()), partial:String::new() }] {
        let rotator = Arc::new(AccountRotator::new(RotationStrategy::Priority, 120));
        rotator.push_account_for_test(fake_oauth_account("first", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("second", 2)).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let error = outcome.encode();
        TEST_WORK_ROTATOR.scope(rotator.clone(), TEST_WORK_ERROR.scope((error.clone(), calls.clone()), async {
            for _ in 0..4 {
                assert_eq!(call_with_rotation(home.path(), "worker", "task", "claude-sonnet-4-6", "system",
                    crate::cost_telemetry::RequestType::Dispatch, None, Some(&dir), false, &[], None,
                    SpawnIdentity::default()).await.unwrap_err(), error);
            }
        })).await;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(rotator.status().await.iter().all(|s| s.is_available && s.is_healthy && s.total_requests == 0));
    }
}

#[tokio::test]
async fn only_registered_entry_refusals_stop_account_rotation() {
    for genuine in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"),
            "[agent]\nname='worker'\nstatus='active'\n[model]\npreferred='claude-sonnet-4-6'\n").unwrap();
        let error = if genuine {
            crate::entry_gate::background_refusal(&crate::entry_gate::EntryRefusal::AgentNotFound)
        } else {
            "ENTRY_REFUSED:agent_not_found:00000000000000000000000000000000: 500 internal server error".into()
        };
        assert_eq!(is_admission_refusal(&error), genuine);
        assert_eq!(is_chain_failover(&error), !genuine);
        let rotator = Arc::new(AccountRotator::new(RotationStrategy::Priority, 120));
        rotator.push_account_for_test(fake_oauth_account("first", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("second", 2)).await;
        let calls = Arc::new(AtomicUsize::new(0));
        TEST_WORK_ROTATOR.scope(rotator.clone(), TEST_WORK_ERROR.scope((error.clone(), calls.clone()), async {
            for _ in 0..3 {
                let actual = call_with_rotation(home.path(), "worker", "task", "claude-sonnet-4-6", "system",
                    crate::cost_telemetry::RequestType::Dispatch, None, Some(&dir), false, &[], None,
                    SpawnIdentity::default()).await.unwrap_err();
                assert_eq!(actual, if genuine { error.clone() }
                    else { format!("All accounts exhausted. Last error: {error}") });
            }
        })).await;
        assert_eq!(calls.load(Ordering::SeqCst), if genuine { 3 } else { 6 });
        // Three ordinary errors count against both accounts and trigger cooldown.
        for status in rotator.status().await {
            assert_eq!(status.is_healthy, genuine);
            assert_eq!(status.is_available, genuine);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn background_stream_parser_preserves_partial_and_empty_or_missing_result() {
    for terminal in [r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":""}"#,
                     r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#] {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("stream.jsonl");
        std::fs::write(&fixture, format!("{}\n{terminal}\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"已完成第一步"}]}}"#)).unwrap();
        let mut cmd = tokio::process::Command::new("/bin/cat");
        cmd.arg(&fixture);
        let error = call_claude_streaming(&mut cmd, None, None, 300, 60, 7, None, None).await.err().expect("terminal work result");
        assert_eq!(crate::work_outcome::WorkOutcome::decode(&error), Some(crate::work_outcome::WorkOutcome::MaxTurns {
            limit:7, partial:"已完成第一步".into() }));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn background_idle_stops_pending_tool_and_keepalive_does_not_reset_it() {
    let started = Arc::new(tokio::sync::Notify::new());
    let entered = started.clone();
    let keepalives = Arc::new(AtomicUsize::new(0));
    let seen = keepalives.clone();
    let keepalive_seen = Arc::new(tokio::sync::Notify::new());
    let heartbeat = keepalive_seen.clone();
    let callback: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        match event {
            crate::channel_reply::ProgressEvent::ToolUse { .. } => entered.notify_one(),
            crate::channel_reply::ProgressEvent::Keepalive => {
                seen.fetch_add(1, Ordering::SeqCst);
                heartbeat.notify_one();
            }
            _ => {}
        }
    });
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", r#"printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"one","name":"Bash"}]}}'; /bin/sleep 300"#]);
    let task = tokio::spawn(async move { call_claude_streaming(&mut cmd, Some(&callback), None, 300, 120, 7, None, None).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified()).await.expect("tool event must arrive");
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(90)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), keepalive_seen.notified()).await
        .expect("keepalive must arrive before the idle deadline");
    assert!(!task.is_finished());
    assert!(keepalives.load(Ordering::SeqCst) > 0);
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let error = task.await.unwrap().err().expect("idle timeout");
    assert_eq!(crate::work_outcome::WorkOutcome::decode(&error), Some(crate::work_outcome::WorkOutcome::TimeoutNoProgress {
        seconds:120, tool:Some("Bash".into()), partial:String::new() }));
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_result_survives_a_hung_process() {
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", r#"printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"完成","usage":{"input_tokens":2,"output_tokens":3}}'; exec /bin/sleep 300"#]);
    let response = tokio::time::timeout(std::time::Duration::from_secs(10),
        call_claude_streaming(&mut cmd, None, None, 300, 1, 7, None, None)).await
        .expect("terminal cleanup must be bounded").expect("terminal result must survive cleanup");
    assert_eq!(response.text, "完成");
}
