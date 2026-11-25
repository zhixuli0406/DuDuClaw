use super::*;

fn bind_fixture(lease: &RunGuard) {
    lease.binding.bind_spawn(duduclaw_core::run_credential::RootProcess::capture(std::process::id()).unwrap()).unwrap();
}

pub(super) fn record() -> RunRecord {
    RunRecord { agent_id: "alice".into(), employee_id: None, incarnation: None,
        principal: Principal::Human { user_id: "user-1".into() }, session_id: Some("session-1".into()),
        turn_id: "turn-1".into(), task_id: None, runtime: "claude".into(), issued_at: 0, issued_at_exact: chrono::Utc::now(), expires_at: 0 }
}

async fn query(registry: &Arc<Registry>, peer: Option<&str>, body: String) -> (u16, Vec<u8>) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let sessions = crate::session::SessionManager::new(file.path()).unwrap();
    let peer = peer.filter(|p| matches!(*p, "127.0.0.1:1234" | "[::1]:1234" | "[::ffff:127.0.0.1]:1234"))
        .and_then(|_| RootProcess::capture(std::process::id()).ok());
    let value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let response = dispatch(registry, &sessions, peer.as_ref(), value).await;
    (200, serde_json::to_vec(&response).unwrap())
}

fn body(credential: &RunCredential) -> String {
    serde_json::json!({"version": 1, "operation": "introspect", "credential": credential.expose()}).to_string()
}

async fn write_ref(registry: &Arc<Registry>, sessions: &Arc<crate::session::SessionManager>, credential: Option<&RunCredential>) -> axum::http::StatusCode {
    let payload = serde_json::json!({
        "version": 1, "operation": "tool-reference",
        "credential": credential.map(RunCredential::expose),
        "DUDUCLAW_SESSION_ID": "other-session", "session_id": "other-session",
        "agent_id": "other-agent", "issued_at": u64::MAX,
        "reference": { "tool_name": "docs_create", "ref_kind": "google_doc", "ref_id": "doc-1", "label": "Document" }
    });
    let peer = RootProcess::capture(std::process::id()).unwrap();
    let response = dispatch(registry, sessions, Some(&peer), payload).await;
    if response["active"] == true { axum::http::StatusCode::NO_CONTENT } else { axum::http::StatusCode::FORBIDDEN }
}

#[tokio::test]
async fn tool_ref_route_uses_only_live_run_session_and_rejects_pre_reset_runs() {
    use axum::http::StatusCode;
    let file = tempfile::NamedTempFile::new().unwrap();
    let sessions = Arc::new(crate::session::SessionManager::new(file.path()).unwrap());
    sessions.get_or_create("session-1", "alice").await.unwrap();
    sessions.get_or_create("other-session", "alice").await.unwrap();
    let registry = Arc::new(Registry::default());
    let lease = registry.issue(record(), 60).unwrap();
    bind_fixture(&lease);
    assert_eq!(write_ref(&registry, &sessions, None).await, StatusCode::FORBIDDEN);
    let unknown = RunCredential::from_bytes(&[42; 32]);
    assert_eq!(write_ref(&registry, &sessions, Some(&unknown)).await, StatusCode::FORBIDDEN);
    assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::NO_CONTENT);
    assert!(!sessions.tool_refs_context("session-1").await.unwrap().is_empty());
    assert!(sessions.tool_refs_context("other-session").await.unwrap().is_empty());
    sessions.clear_for_user("session-1").await.unwrap();
    sessions.get_or_create("session-1", "alice").await.unwrap();
    assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::FORBIDDEN);
    assert!(sessions.tool_refs_context("session-1").await.unwrap().is_empty());
    // Deterministic sub-second reset: comparing integer seconds would allow it.
    let issued = registry.lookup(&lease.credential).unwrap().issued_at_exact;
    let generation = issued + chrono::Duration::nanoseconds(1);
    sessions.acquire().await.execute("UPDATE sessions SET created_at = ?1 WHERE id = 'session-1'",
        [generation.to_rfc3339()]).unwrap();
    assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::FORBIDDEN);
    let fresh = registry.issue(record(), 60).unwrap();
    bind_fixture(&fresh);
    assert_eq!(write_ref(&registry, &sessions, Some(&fresh.credential)).await, StatusCode::NO_CONTENT);
    let proof = RunCredential::parse(fresh.credential.expose().into()).unwrap();
    drop(fresh);
    assert_eq!(write_ref(&registry, &sessions, Some(&proof)).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn both_operations_require_a_kernel_peer_and_protocol_version() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let sessions = crate::session::SessionManager::new(file.path()).unwrap();
    let registry = Arc::new(Registry::default());
    let lease = registry.issue(record(), 60).unwrap();
    bind_fixture(&lease);
    for operation in ["introspect", "tool-reference"] {
        let value = serde_json::json!({"version": 1, "operation": operation, "credential": lease.credential.expose()});
        assert_eq!(dispatch(&registry, &sessions, None, value).await["active"], false);
    }
    let peer = RootProcess::capture(std::process::id()).unwrap();
    let value = serde_json::json!({"credential": lease.credential.expose(), "operation": "introspect"});
    assert_eq!(dispatch(&registry, &sessions, Some(&peer), value).await["active"], false);
    assert!(ipc::endpoint_path("http://127.0.0.1:42/api/internal/identity/introspect").is_none());
}

#[tokio::test]
async fn dedicated_listener_requires_lock_and_publishes_only_local_ipc() {
    #[cfg(unix)]
    let home = tempfile::tempdir_in("/tmp").unwrap();
    #[cfg(windows)]
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::default());
    assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
    assert!(!home.path().join(IDENTITY_ENDPOINT_FILE).exists());
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    let listener = start_listener_with(home.path(), registry.clone()).await.unwrap();
    let url = std::fs::read_to_string(home.path().join(IDENTITY_ENDPOINT_FILE)).unwrap();
    assert!(ipc::endpoint_path(&url).is_some());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(home.path().join("run")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(home.path().join(IDENTITY_ENDPOINT_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        let socket = std::path::Path::new(ipc::endpoint_path(&url).unwrap());
        assert_eq!(std::fs::metadata(socket.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }
    let lease = registry.issue(record(), 30).unwrap();
    bind_fixture(&lease);
    let mut command = tokio::process::Command::new("never-spawned");
    lease.apply(&mut command);
    assert!(command.as_std().get_envs().any(|(key, value)| key == ENV_IDENTITY_ENDPOINT
        && value.and_then(|v| v.to_str()) == Some(url.as_str())));
    assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
    drop(listener);
    assert!(!home.path().join(IDENTITY_ENDPOINT_FILE).exists());
    #[cfg(unix)]
    assert!(!std::path::Path::new(ipc::endpoint_path(&url).unwrap()).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn stale_socket_cleanup_never_follows_planted_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    let discovery = DiscoveryDirectory::open(home.path(), true).unwrap();
    let stale = home.path().join("run/i-1111111111111111");
    std::fs::create_dir(&stale).unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(std::os::unix::net::UnixListener::bind(stale.join("s")).unwrap());
    std::fs::write(outside.path().join("s"), b"untouched").unwrap();
    symlink(outside.path(), home.path().join("run/i-2222222222222222")).unwrap();
    let planted = home.path().join("run/i-3333333333333333");
    std::fs::create_dir(&planted).unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(outside.path().join("s"), planted.join("s")).unwrap();
    let listener = start_listener_with(home.path(), Arc::new(Registry::default())).await.unwrap();
    assert!(!stale.exists());
    assert!(std::fs::symlink_metadata(planted.join("s")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(outside.path().join("s")).unwrap(), b"untouched");
    drop(listener);
    drop(discovery);
}

#[cfg(unix)]
#[tokio::test]
async fn listener_refuses_symlinked_or_group_writable_run_directory() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::default());
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    let run = home.path().join("run");
    symlink(outside.path(), &run).unwrap();
    assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
    assert!(!outside.path().join("identity-endpoint").exists());
    std::fs::remove_file(&run).unwrap();
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o770)).unwrap();
    assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
    assert!(registry.endpoint.lock().unwrap().is_none());
    assert!(!home.path().join(IDENTITY_ENDPOINT_FILE).exists());
}

#[test]
fn protected_branches_do_not_issue_or_pass_run_credentials() {
    // Source-contract guard: protected launch implementations must remain
    // ahead of host command preparation and must not receive its env.
    let channel = include_str!("../channel_reply/cli_env.rs").replace("\r\n", "\n");
    assert!(channel.contains("let _run_identity = if protected { None } else {"));
    let protected = channel.split_once("let mut child = if protected {").unwrap().1
        .split_once("} else {").unwrap().0;
    assert!(!protected.contains("cmd"));
    assert!(!protected.contains("run_identity"));
    assert!(!protected.contains("env"));

    let claude = include_str!("../claude_runner.rs").replace("\r\n", "\n");
    let branch = claude.split_once("if duduclaw_core::execution_gate::isolation_required() {").unwrap().1
        .split_once("let claude = duduclaw_core::which_claude()").unwrap().0;
    assert!(branch.contains("return call_claude_streaming"));
    assert!(!branch.contains("prepare_claude_cmd"));
    assert!(!branch.contains("run_identity::issue"));
    assert!(!branch.contains(".env("));
    for source in [include_str!("../runtime/codex.rs").replace("\r\n", "\n"),
        include_str!("../runtime/grok.rs").replace("\r\n", "\n"),
        include_str!("../runtime/generic_cli.rs").replace("\r\n", "\n")] {
        let execute = source.split_once("async fn execute(").unwrap().1;
        let branch = execute.split_once("if duduclaw_core::execution_gate::isolation_required() {").unwrap().1
            .split_once("\n        }\n").unwrap().0;
        assert!(branch.contains("return "));
        assert!(!branch.contains("run_identity::issue"));
        assert!(!branch.contains(".env("));
        assert!(!branch.contains("prepare_run_command"));
    }
}

#[test]
fn dashboard_does_not_mount_identity_introspection() {
    // Identity is IPC-only; reject accidental legacy HTTP route registration.
    let server = include_str!("../server.rs");
    assert!(!server.contains("run_identity::router"));
    assert!(!server.contains(ROUTE));
}

#[test]
fn junk_and_other_runs_cannot_exhaust_a_valid_runs_bucket() {
    let peer = RootProcess::capture(std::process::id()).unwrap();
    let registry = Arc::new(Registry::default());
    let first = registry.issue(record(), 100).unwrap();
    bind_fixture(&first);
    let second = registry.issue(record(), 100).unwrap();
    bind_fixture(&second);
    let unknown = RunCredential::from_bytes(&[42; 32]);
    for _ in 0..RATE_PER_MINUTE {
        assert!(registry.query(Some(&first.credential), Some(&peer)).is_some());
        assert!(registry.query(Some(&unknown), Some(&peer)).is_none());
    }
    assert!(registry.query(Some(&first.credential), Some(&peer)).is_none());
    assert!(registry.query(None, Some(&peer)).is_none());
    assert!(registry.query(Some(&second.credential), Some(&peer)).is_some());
    assert_eq!(registry.invalid_rate.lock().unwrap().1, RATE_PER_MINUTE);
    assert_eq!(registry.runs.lock().unwrap().get(&second.hash).unwrap().rate.1, 1);
    registry.runs.lock().unwrap().get_mut(&first.hash).unwrap().rate.0 =
        Instant::now() - Duration::from_secs(61);
    assert!(registry.query(Some(&first.credential), Some(&peer)).is_some());
    // Valid buckets live inside the bounded registry, not an unbounded
    // attacker-keyed map. Unknown requests share exactly one bucket.
    assert_eq!(registry.runs.lock().unwrap().len(), 2);
    drop(first);
    assert_eq!(registry.runs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_and_unknown_requests_do_the_same_digest_lookup_work() {
    let registry = Arc::new(Registry::default());
    for raw in ["not json".to_string(), "{}".into(), "{\"credential\":0}".into(),
        "{\"credential\":\"bad\"}".into(), "x".repeat(2048),
        body(&RunCredential::from_bytes(&[42; 32]))] {
        let before = registry.lookups.load(std::sync::atomic::Ordering::Relaxed);
        let response = query(&registry, Some("127.0.0.1:1234"), raw).await;
        assert_eq!(response, (200, br#"{"active":false}"#.to_vec()));
        assert_eq!(registry.lookups.load(std::sync::atomic::Ordering::Relaxed), before + 1);
    }
}

#[test]
fn registry_retains_only_hashes_and_drop_revokes() {
    let registry = Arc::new(Registry::default());
    let lease = registry.issue(record(), 100).unwrap();
    bind_fixture(&lease);
    assert_eq!(format!("{lease:?}"), "RunGuard([REDACTED])");
    let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
    {
        let runs = registry.runs.lock().unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs.contains_key(&digest(&proof)));
        let entry = runs.get(&digest(&proof)).unwrap();
        assert!(!serde_json::to_string(&entry.record).unwrap().contains(proof.expose()));
    }
    assert!(registry.lookup(&proof).is_some());
    drop(lease);
    assert!(registry.lookup(&proof).is_none());
    assert!(registry.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn errors_cancellation_expiry_and_restart_revoke() {
    let registry = Arc::new(Registry::default());
    let lease = registry.issue(record(), u64::MAX).unwrap();
    bind_fixture(&lease);
    let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
    let entry = registry.lookup(&proof).unwrap();
    assert_eq!(entry.expires_at - entry.issued_at, MAX_TTL);
    assert!(Registry::default().lookup(&proof).is_none());
    let result: Result<(), ()> = async move { let _lease = lease; Err(()) }.await;
    assert!(result.is_err());
    assert!(registry.lookup(&proof).is_none());

    let lease = registry.issue(record(), 100).unwrap();
    bind_fixture(&lease);
    let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
    let (ready, observed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _lease = lease;
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    observed.await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(registry.lookup(&proof).is_none());

    let lease = registry.issue(record(), 100).unwrap();
    bind_fixture(&lease);
    registry.runs.lock().unwrap().get_mut(&lease.hash).unwrap().deadline = Instant::now();
    assert!(registry.lookup(&lease.credential).is_none());
}

#[tokio::test]
async fn all_inactive_dispatch_responses_are_identical_and_peer_is_required() {
    let registry = Arc::new(Registry::default());
    let active = registry.issue(record(), 100).unwrap();
    bind_fixture(&active);
    let expired = registry.issue(record(), 100).unwrap();
    bind_fixture(&expired);
    registry.runs.lock().unwrap().get_mut(&expired.hash).unwrap().record.expires_at = 0;
    let revoked = registry.issue(record(), 100).unwrap();
    bind_fixture(&revoked);
    let revoked_body = body(&revoked.credential);
    drop(revoked);
    let baseline = (200, br#"{"active":false}"#.to_vec());
    for raw in [String::new(), "not json".into(), "{}".into(), "{\"credential\":0}".into(),
        "{\"credential\":\"ddc_run_bad\"}".into(), body(&RunCredential::from_bytes(&[42; 32])),
        body(&expired.credential), revoked_body, "x".repeat(2048)] {
        assert_eq!(query(&registry, Some("127.0.0.1:1234"), raw).await, baseline);
    }
    for peer in [None, Some("192.0.2.1:1234"), Some("[2001:db8::1]:1234")] {
        assert_eq!(query(&registry, peer, body(&active.credential)).await, baseline);
    }
    for peer in ["127.0.0.1:1234", "[::1]:1234", "[::ffff:127.0.0.1]:1234"] {
        let (status, bytes) = query(&registry, Some(peer), body(&active.credential)).await;
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["active"], true);
        assert_eq!(value["agent_id"], "alice");
        assert_eq!(value["principal"]["user_id"], "user-1");
        assert!(value.get("credential").is_none());
        assert!(!String::from_utf8(bytes).unwrap().contains(active.credential.expose()));
    }
    registry.runs.lock().unwrap().get_mut(&active.hash).unwrap().rate = (Instant::now(), RATE_PER_MINUTE);
    assert_eq!(query(&registry, Some("127.0.0.1:1234"), body(&active.credential)).await, baseline);
}

#[tokio::test]
async fn gateway_registration_controls_human_and_background_claims() {
    let home = tempfile::tempdir().unwrap();
    for kind in ["cron", "heartbeat", "task"] {
        let run = uuid::Uuid::new_v4().to_string();
        let _registered = crate::knowledge::turn_requesters::register_run("alice", &run, kind).unwrap();
        crate::memory_provenance::DISPATCH_RUN.scope(Some((format!("{kind}:alice"), run)), async {
            let lease = issue(home.path(), "alice", "grok", 30).unwrap();
            bind_fixture(&lease);
            assert_eq!(registry().lookup(&lease.credential).unwrap().principal,
                Principal::Background { kind: kind.into() });
        }).await;
    }
    crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
        RequesterIdentity::DashboardUser { user_id: "verified-user".into() }, async {
            let lease = issue(home.path(), "alice", "codex", 30).unwrap();
            bind_fixture(&lease);
            assert_eq!(registry().lookup(&lease.credential).unwrap().principal,
                Principal::Human { user_id: "verified-user".into() });
        }).await;
}

#[test]
fn capacity_is_bounded_and_expired_entries_are_reclaimed() {
    let registry = Arc::new(Registry::default());
    {
        let mut runs = registry.runs.lock().unwrap();
        for n in 0..MAX_RUNS {
            let hash = Sha256::digest(n.to_be_bytes()).into();
            let mut record = record();
            record.expires_at = u64::MAX;
            runs.insert(hash, Entry { binding: SpawnBinding::pending(Instant::now() + Duration::from_secs(60)), record, deadline: Instant::now() + Duration::from_secs(60), rate: (Instant::now(), 0) });
        }
    }
    assert!(registry.issue(record(), 10).is_err());
    for entry in registry.runs.lock().unwrap().values_mut() { entry.record.expires_at = 0; }
    assert!(registry.issue(record(), 10).is_ok());
}

#[test]
fn runtime_env_and_pty_proofs_are_distinct_and_revoked() {
    let home = tempfile::tempdir().unwrap();
    for runtime in ["claude", "codex", "grok", "copilot", "qwen", "kimi", "opencode", "vibe"] {
        let lease = issue(home.path(), "alice", runtime, 30).unwrap();
        let mut cmd = tokio::process::Command::new("never-spawned");
        lease.apply(&mut cmd);
        let proof = assert_command_identity(&cmd, true);
        bind_fixture(&lease);
        assert_eq!(registry().lookup(&proof).unwrap().runtime, runtime);
        let mut env = HashMap::new();
        lease.apply_pty(&mut env);
        assert!(env.get(ENV_RUN_CREDENTIAL).is_some_and(|value| value == proof.expose()));
        drop(lease);
        assert!(registry().lookup(&proof).is_none());
    }
}

#[tokio::test]
async fn pending_refuses_both_routes_and_double_binding_revokes() {
    let registry = Arc::new(Registry::default());
    let lease = registry.issue(record(), 60).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let sessions = Arc::new(crate::session::SessionManager::new(file.path()).unwrap());
    assert_eq!(query(&registry, Some("127.0.0.1:1234"), body(&lease.credential)).await,
        (200, br#"{"active":false}"#.to_vec()));
    assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, axum::http::StatusCode::FORBIDDEN);
    bind_fixture(&lease);
    assert!(registry.lookup(&lease.credential).is_some());
    assert!(lease.binding.bind_spawn(duduclaw_core::run_credential::RootProcess::capture(std::process::id()).unwrap()).is_err());
    assert!(registry.lookup(&lease.credential).is_none());
}

#[test]
fn direct_api_issuer_uses_distinct_tokens_and_preserves_run_claims() {
    let registry = Arc::new(Registry::default());
    let template = registry.issue(record(), 60).unwrap();
    let template_hash = template.hash;
    let issuer = template.into_issuer().unwrap();
    assert!(!registry.runs.lock().unwrap().contains_key(&template_hash));
    let first = issuer.issue().unwrap();
    let second = issuer.issue().unwrap();
    assert_ne!(first.hash, second.hash);
    assert!(registry.lookup(&first.credential).is_none());
    assert!(registry.lookup(&second.credential).is_none());
    bind_fixture(&first);
    bind_fixture(&second);
    let a = registry.lookup(&first.credential).unwrap();
    let b = registry.lookup(&second.credential).unwrap();
    assert_eq!(a.turn_id, b.turn_id);
    assert_eq!(a.principal, b.principal);
    first.binding.revoke();
    assert!(registry.lookup(&first.credential).is_none());
    assert!(registry.lookup(&second.credential).is_some());
}
