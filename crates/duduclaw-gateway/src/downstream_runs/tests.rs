use super::*;

#[tokio::test]
async fn downstream_guard_drop_panic_and_exact_two_hop_stop() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let a = register(h, Some("s"), Some("t"), Some("user:alice")).unwrap().unwrap();
    let b = register(h, Some("s"), Some("t"), Some(" user:alice ")).unwrap().unwrap();
    let foreign = register(h, Some("s"), Some("t"), Some("user:bob")).unwrap().unwrap();
    let other_turn = register(h, Some("s"), Some("other"), None).unwrap().unwrap();
    let other_session = register(h, Some("other"), Some("t"), None).unwrap().unwrap();
    assert_eq!(count(h, "s", "t", "user:alice"), 2);
    let first = tokio::spawn(async move { a.run(std::future::pending::<()>()).await });
    let second = tokio::spawn(async move { b.run(std::future::pending::<()>()).await });
    let result = stop(h, "s", "t", "user:alice", Duration::from_secs(1), "test").await;
    assert_eq!(result, StopResult { stopped: 2, still_stopping: 0 });
    assert!(first.await.unwrap().is_err());
    assert!(second.await.unwrap().is_err());
    assert!(!foreign.token.is_cancelled());
    assert!(!other_turn.token.is_cancelled());
    assert!(!other_session.token.is_cancelled());
    drop(foreign);
    assert_eq!(count(h, "s", "t", "user:alice"), 0);
    let panic_guard = register(h, Some("s"), Some("panic"), None).unwrap().unwrap();
    let panicked = tokio::spawn(async move { let _guard = panic_guard; panic!("fixture"); });
    assert!(panicked.await.unwrap_err().is_panic());
    assert_eq!(count(h, "s", "panic", "user:alice"), 0);
}

#[tokio::test]
async fn downstream_cleanup_must_confirm_before_stopped() {
    let home = tempfile::tempdir().unwrap();
    for confirmed in [true, false] {
        let guard = register(home.path(), Some("s"), Some("t"), None).unwrap().unwrap();
        let completion = guard.completion.clone();
        let hold = COMPLETION.scope(completion.clone(), async { cleanup_hold().unwrap() }).await;
        let task = tokio::spawn(async move { guard.run(std::future::pending::<()>()).await });
        let finish = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            hold.finish(confirmed);
        });
        let result = stop(home.path(), "s", "t", "user:a", Duration::from_millis(100), "test").await;
        assert_eq!(result.stopped, usize::from(confirmed));
        assert_eq!(result.still_stopping, usize::from(!confirmed));
        task.await.unwrap().unwrap_err();
        finish.await.unwrap();
        assert!(completion.dropped.load(Ordering::Acquire));
    }
}

#[test]
fn downstream_registry_bounded_and_half_pair_absent() {
    let home = tempfile::tempdir().unwrap();
    let registry = Box::leak(Box::new(Registry::default()));
    assert!(register_in(registry, home.path(), Some("s"), None, None).unwrap().is_none());
    let mut guards = Vec::new();
    for _ in 0..MAX_RUNS {
        match register_in(registry, home.path(), Some("s"), Some("t"), None) {
            Ok(Some(guard)) => guards.push(guard),
            Err(_) => break,
            _ => unreachable!(),
        }
    }
    assert!(register_in(registry, home.path(), Some("s"), Some("t"), None).is_err());
    drop(guards);
    assert!(registry.entries.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn downstream_stop_kills_real_process_tree() {
    let home = tempfile::tempdir().unwrap();
    let guard = register(home.path(), Some("s"), Some("t"), None).unwrap().unwrap();
    let path = home.path().to_path_buf();
    let task = tokio::spawn(async move {
        guard.run(async {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.env_clear().current_dir(path).args(["-c", "/bin/sleep 30 & echo $! > pid; wait"]);
            crate::process_group::output(&mut command, None, Duration::from_secs(60)).await
        }).await
    });
    let mut pid = None;
    for _ in 0..100 {
        pid = std::fs::read_to_string(home.path().join("pid")).ok().and_then(|s| s.trim().parse::<u32>().ok());
        if pid.is_some() { break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = pid.expect("fixture started");
    assert!(duduclaw_core::platform::process_alive(pid));
    let result = stop(home.path(), "s", "t", "user:a", Duration::from_secs(2), "test").await;
    assert_eq!(result.stopped, 1);
    task.await.unwrap().unwrap_err();
    for _ in 0..100 {
        if !duduclaw_core::platform::process_alive(pid) { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("grandchild survived stop");
}

#[tokio::test]
async fn downstream_unknown_principal_stops_but_foreign_human_is_excluded() {
    let home = tempfile::tempdir().unwrap();
    let unknown = register(home.path(), Some("s"), Some("t"), Some("garbage")).unwrap().unwrap();
    let foreign = register(home.path(), Some("s"), Some("t"), Some("user:bob")).unwrap().unwrap();
    assert_eq!(count(home.path(), "s", "t", "user:alice"), 1);
    assert_eq!(stop(home.path(), "s", "t", "user:alice", Duration::ZERO, "person").await, StopResult { stopped: 0, still_stopping: 1 });
    assert!(unknown.token.is_cancelled());
    assert!(!foreign.token.is_cancelled());
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = audit.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mismatch = rows.iter().find(|r| r["cause"] == "principal_mismatch").expect("mismatch audit");
    assert_eq!(mismatch["stopped"], 0);
    assert_eq!(mismatch["still_stopping"], 0);
    assert_eq!(mismatch["excluded"], 1);
    assert_eq!(mismatch["count"], 1);
}

#[tokio::test]
async fn downstream_unavailable_lineage_matches_only_background_and_unknown() {
    let home = tempfile::tempdir().unwrap();
    let background = register(home.path(), Some("s"), Some("t"), None).unwrap().unwrap();
    let unknown = register(home.path(), Some("s"), Some("t"), Some("malformed")).unwrap().unwrap();
    let human = register(home.path(), Some("s"), Some("t"), Some("user:alice")).unwrap().unwrap();
    assert_eq!(count(home.path(), "s", "t", "lineage-unavailable"), 2);
    assert_eq!(stop(home.path(), "s", "t", "lineage-unavailable", Duration::ZERO, "test").await,
        StopResult { stopped: 0, still_stopping: 2 });
    assert!(background.token.is_cancelled());
    assert!(unknown.token.is_cancelled());
    assert!(!human.token.is_cancelled());
}
