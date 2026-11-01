//! Exercise the real queue -> runner -> CLI path, with only the CLI replaced.
//! Re-exec isolates environment/caches and contains a stack-overflow abort.
use super::*;

#[test]
fn dispatch_future_sizes() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(RwLock::new(AgentRegistry::new(home.path().join("agents"))));
    let queue = Arc::new(crate::message_queue::MessageQueue::open(home.path()).unwrap());
    let delegation = DelegationEnv {
        credential_source: "size-test".into(), depth: 0,
        origin: String::new(), sender: String::new(), hop_depth: 0,
    };
    // Before boxing (macOS arm64, debug, no default features): outcome
    // 21,104 B, JSONL 4,952 B, ephemeral 16,112 B, SQLite 43,968 B.
    // JSONL spawns each run separately; its poll future is already small.
    let sizes = [
        ("dispatch.outcome", std::mem::size_of_val(&dispatch_to_agent_outcome(
            home.path(), &registry, "prod-shaped", "wake", &delegation, None,
        )), 24 * 1024),
        ("dispatch.jsonl", std::mem::size_of_val(&poll_and_dispatch(
            home.path(), &registry, None, None,
        )), 8 * 1024),
        ("dispatch.ephemeral", std::mem::size_of_val(&crate::ephemeral::dispatch(
            home.path(), &registry, "eph-test", "wake",
        )), 20 * 1024),
        ("dispatch.sqlite", std::mem::size_of_val(&poll_and_dispatch_sqlite(
            &queue, home.path(), &registry, None, None,
        )), 16 * 1024),
    ];
    for (name, size, limit) in sizes {
        eprintln!("{name}: {size} bytes");
        assert!(size <= limit, "{name}: {size} > {limit}; box before adding dispatch scopes");
    }
}

#[cfg(unix)]
#[test]
fn queued_wake_on_two_mib_stack() {
    const CHILD: &str = "DUDUCLAW_STACK_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .name("queued-wake-2mib".into())
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                    .block_on(run_queued_wakes());
            }).unwrap().join().unwrap();
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let os_home = root.path().join("os-home");
    std::fs::create_dir(&os_home).unwrap();
    let home = root.path().join("live");
    let setup = std::process::Command::new("/bin/bash")
        .args(["-c", include_str!("../../../scripts/live-test/make-home.sh"), "make-home", home.to_str().unwrap()])
        .env("HOME", &os_home).output().unwrap();
    assert!(setup.status.success(), "{}", String::from_utf8_lossy(&setup.stderr));
    // Keep all real gates; a fresh home has no provider credentials.
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().append(true).open(home.join("config.toml")).unwrap(),
        "\n[account_loading]\ninherit_host_credentials = false\n[dispatch]\nenabled = true").unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let cli = bin.join("claude");
    std::fs::write(&cli, r#"#!/bin/sh
case "$1" in
  --version) echo '99.0.0'; exit 0 ;;
  auth) echo '{"loggedIn":false}'; exit 0 ;;
esac
# Prove that real command preparation retained the wildcard and deny gate.
case "$*" in *mcp__duduclaw__\**) ;; *) echo missing-wildcard >&2; exit 2 ;; esac
case "$*" in *--disallowedTools*WebFetch*) ;; *) echo missing-deny >&2; exit 2 ;; esac
[ -n "$DUDUCLAW_TASK_ID" ] || { echo missing-task >&2; exit 2; }
[ -n "$DUDUCLAW_PRINCIPAL_CONTEXT" ] || { echo missing-context >&2; exit 2; }
printf '%s\n' "$DUDUCLAW_TASK_ID" >> .stack-cli-invoked
echo '{"type":"result","is_error":true,"result":"stack-test-cli-reached"}'
exit 1
"#).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "dispatcher::dispatch_stack_tests::queued_wake_on_two_mib_stack", "--nocapture"])
        .env_clear().env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &os_home).env("DUDUCLAW_HOME", &home).env(CHILD, "1")
        .spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "queue path failed on a 2 MiB stack: {status}");
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("queue path did not terminate (possible recursion)");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Also check in the parent: a renamed --exact filter must not pass by
    // running zero tests in the child.
    let calls = std::fs::read_to_string(home.join("agents/prod-shaped/.stack-cli-invoked")).unwrap();
    assert_eq!(calls, "heartbeat-stall\nheartbeat-pull\n");
}

#[cfg(unix)]
async fn run_queued_wakes() {
    use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
    use crate::task_store::{TaskRow, TaskStore};
    let home = duduclaw_core::platform::duduclaw_home();
    let mut registry = AgentRegistry::new(home.join("agents"));
    registry.scan().await.unwrap();
    assert!(registry.get("prod-shaped").is_some());
    let registry = Arc::new(RwLock::new(registry));
    {
        let reg = registry.read().await;
        let agent = reg.get("prod-shaped").unwrap();
        let size = std::mem::size_of_val(&crate::claude_runner::call_claude_for_agent_preloaded_with(
            &home, &registry, agent, "wake", crate::cost_telemetry::RequestType::Dispatch,
            crate::claude_runner::DispatchOverrides::default(),
        ));
        eprintln!("runner.preloaded: {size} bytes");
        assert!(size <= 8 * 1024, "preloaded runtime must stay boxed: {size}");
    }
    let store = TaskStore::open(&home).unwrap();
    let queue = Arc::new(MessageQueue::open(&home).unwrap());
    let dispatcher = start_agent_dispatcher_with_crypto(
        home.clone(), registry.clone(), None, Some(queue.clone()), None, None,
    );
    for marker in ["heartbeat-stall", "heartbeat-pull"] {
        let mut task = TaskRow::new(marker.into(), marker.into(), "work".into(), "medium".into(), "prod-shaped".into(), "test".into());
        task.status = "in_progress".into();
        task.updated_at = (Utc::now() - chrono::Duration::minutes(30)).to_rfc3339();
        store.insert_task(&task).await.unwrap();
        let msg = QueueMessage {
            id: marker.into(), sender: crate::responsibility::HEARTBEAT_SENDER.into(),
            target: "prod-shaped".into(), payload: format!("[{marker} task_id={marker}] continue"),
            status: MessageStatus::Pending, retry_count: 0, delegation_depth: 0,
            origin_agent: None, sender_agent: None, error: None, response: None,
            created_at: Utc::now().to_rfc3339(), acked_at: None, completed_at: None,
            reply_channel: None, turn_id: None, session_id: None, upstream_unknown: false,
            lane: None, on_behalf_of: None,
        };
        queue.enqueue(&msg).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let row = queue.get_by_id(marker).await.unwrap().unwrap();
                if matches!(row.status, MessageStatus::Failed | MessageStatus::Done) { break row; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("dispatcher must finish the queued wake-up");
        assert_eq!(result.status, MessageStatus::Failed);
        assert!(result.error.as_deref().unwrap_or_default().contains("stack-test-cli-reached"), "{result:?}");
    }
    dispatcher.abort();
    assert!(dispatcher.await.unwrap_err().is_cancelled());
    let calls = std::fs::read_to_string(home.join("agents/prod-shaped/.stack-cli-invoked")).unwrap();
    assert_eq!(calls, "heartbeat-stall\nheartbeat-pull\n");
}
