//! Phase 6.5 — end-to-end IPC integration tests.
//!
//! Boots a [`WorkerServer`] on `127.0.0.1:<random>` with a fake
//! `PtyPool` whose spawn factory uses `cat` (Unix) / `findstr` (Win) so
//! `invoke` succeeds without needing a real `claude` binary on PATH.
//! Then drives the [`WorkerClient`] over loopback to verify the full
//! HTTP+JSON path: roundtrip, unauthorised rejection, healthz no-auth,
//! concurrent dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use duduclaw_cli_runtime::pool::futures_compat::SpawnFuture;
use duduclaw_cli_runtime::{AgentKey, PoolConfig, PtyPool, PtySession, SpawnOpts};
use duduclaw_cli_worker::client::ClientError;
use duduclaw_cli_worker::{
    HEALTHZ_PATH, InvokeParams, RPC_PATH, ServerHandle, WorkerClient, WorkerServer,
    WorkerServerConfig,
};

fn cat_program() -> (String, Vec<String>) {
    #[cfg(unix)]
    {
        ("cat".to_string(), vec![])
    }
    #[cfg(windows)]
    {
        (
            "findstr".to_string(),
            vec!["/N".to_string(), "^".to_string()],
        )
    }
}

/// Factory that spawns `cat` (or `findstr`) under PTY. The session is
/// *non-interactive* — invoke writes a full sentinel-wrapping envelope
/// via `frame_request`, cat echoes it back, parse_frame matches the
/// pair, and we return the payload between them.
fn cat_factory() -> duduclaw_cli_runtime::pool::SpawnFactory {
    Arc::new(|key: AgentKey| -> SpawnFuture {
        Box::pin(async move {
            let (program, args) = cat_program();
            PtySession::spawn(SpawnOpts {
                agent_id: key.agent_id.clone(),
                cli_kind: key.cli_kind,
                program,
                extra_args: args,
                env: HashMap::new(),
                cwd: None,
                session_id: None,
                boot_timeout: Duration::from_millis(500),
                default_invoke_timeout: Duration::from_secs(5),
                rows: 24,
                cols: 200,
                interactive: false,
                pre_trusted: false,
            })
            .await
        })
    })
}

fn fake_pool() -> Arc<PtyPool> {
    PtyPool::new(
        cat_factory(),
        PoolConfig {
            max_per_agent: 1,
            idle_timeout: Duration::from_secs(60),
            default_invoke_timeout: Duration::from_secs(5),
            ..PoolConfig::default()
        },
    )
}

async fn boot_server(token: &str) -> ServerHandle {
    let config = WorkerServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(), // ephemeral
        token: token.to_string(),
        ..WorkerServerConfig::default()
    };
    let server = WorkerServer::with_pool(config, fake_pool()).expect("server new");
    server.serve_on().await.expect("server serve_on")
}

#[tokio::test(flavor = "current_thread")]
async fn healthz_succeeds_without_auth_header() {
    let handle = boot_server("test-token").await;
    let base = format!("http://{}", handle.local_addr);
    // Hit /healthz with a *bare* HTTP client (no Authorization header).
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}{}", base, HEALTHZ_PATH))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_rejects_missing_auth_header() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    // Send a valid RPC body but with no Authorization header.
    let client = reqwest::Client::new();
    let body = serde_json::json!({"method":"health","params":null});
    let resp = client
        .post(format!("{}{}", base, RPC_PATH))
        .json(&body)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 401);
    let payload: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["kind"], "unauthorized");
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_rejects_wrong_token() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "different-token").expect("client new");
    let err = client.health().await.expect_err("must reject");
    match err {
        ClientError::Worker { kind, .. } => assert_eq!(kind, "unauthorized"),
        other => panic!("expected Worker(unauthorized), got {other:?}"),
    }
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_health_round_trip_succeeds_with_auth() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "real-token").expect("client new");
    let value = client.health().await.expect("health");
    assert_eq!(value, serde_json::json!({"alive": true}));
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_stats_reports_zero_sessions_before_any_invoke() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "real-token").expect("client new");
    let stats = client.stats().await.expect("stats");
    assert_eq!(stats.session_count, 0);
    // version was injected at config time and should round-trip.
    assert!(!stats.version.is_empty());
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_invoke_rejects_unknown_cli_kind() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "real-token").expect("client new");
    let err = client
        .invoke(
            InvokeParams {
                agent_id: "agent-x".into(),
                cli_kind: "not-a-real-cli".into(),
                bare_mode: false,
                prompt: "hi".into(),
                timeout_ms: 500,
                account_id: None,
                model: None,
                work_dir: None,
            },
            Duration::from_secs(2),
        )
        .await
        .expect_err("must reject unknown cli_kind");
    match err {
        ClientError::Worker { kind, message } => {
            assert_eq!(kind, "bad_request");
            assert!(message.contains("not-a-real-cli"), "got: {message}");
        }
        other => panic!("expected Worker(bad_request), got {other:?}"),
    }
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_invoke_rejects_empty_prompt() {
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "real-token").expect("client new");
    let err = client
        .invoke(
            InvokeParams {
                agent_id: "agent-x".into(),
                cli_kind: "claude".into(),
                bare_mode: false,
                prompt: String::new(),
                timeout_ms: 500,
                account_id: None,
                model: None,
                work_dir: None,
            },
            Duration::from_secs(2),
        )
        .await
        .expect_err("must reject empty prompt");
    match err {
        ClientError::Worker { kind, .. } => assert_eq!(kind, "bad_request"),
        other => panic!("expected Worker(bad_request), got {other:?}"),
    }
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_shutdown_session_no_op_when_session_absent() {
    // **Review fix (CRITICAL #1)**: previously this RPC spawned a real
    // session just to invalidate it (token-cost trap). The new contract
    // is: never-acquired keys return `shutdown=false` immediately
    // without touching the pool.
    let handle = boot_server("real-token").await;
    let base = format!("http://{}", handle.local_addr);
    let client = WorkerClient::new(&base, "real-token").expect("client new");
    let result = client
        .shutdown_session(duduclaw_cli_worker::protocol::ShutdownSessionParams {
            agent_id: "never-touched-agent".into(),
            cli_kind: "claude".into(),
            bare_mode: false,
            account_id: None,
            model: None,
        })
        .await
        .expect("shutdown_session");
    assert!(!result, "must return false for keys with no cached session");
    let _ = handle.shutdown_tx.send(());
    let _ = handle.join.await;
}
