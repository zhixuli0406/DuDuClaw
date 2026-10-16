//! Native Unix socket integration: real gateway lifecycle + SysdClient, with
//! an isolated fake daemon. No environment overrides or real sysd are used.

use std::sync::Mutex as StdMutex;

use duduclaw_sysd::protocol::{SysdError, SysdResponse};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::*;
use crate::maintenance::lifecycle::{EnableRequest, MaintEnv, ReauthVerifier};
use crate::maintenance::notify::NoNotifier;
use crate::maintenance::{MaintenanceStore, disable, enable, error_code, status_json};

const KEY: &str = include_str!("../../../duduclaw-sysd/tests/fixtures/ssh_keys/ed25519.pub");

struct SocketProvider(Arc<SocketMaintSysd>);

impl SysdProvider for SocketProvider {
    fn access(&self) -> SysdAccess {
        SysdAccess::Available(self.0.clone())
    }
}

struct Verifier;

impl ReauthVerifier for Verifier {
    fn verify(&self, _: &str, password: &str) -> bool {
        password == "socket-test-password"
    }
}

#[derive(Clone, Copy)]
enum Behavior {
    Current,
    Busy,
    Old,
}

struct FakeDaemon {
    requests: Arc<StdMutex<Vec<SysdRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn success(body: Value) -> SysdResponse {
    SysdResponse::ok(
        "socket-test-audit",
        SysdOpOutput {
            success: true,
            stdout: body.to_string(),
            stderr: String::new(),
        },
    )
}

fn fake_daemon(home: &std::path::Path, behavior: Behavior) -> (MaintEnv, FakeDaemon) {
    let socket = home.join("sysd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        let mut window: Option<(String, String)> = None;
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut line = String::new();
            BufReader::new(reader).read_line(&mut line).await.unwrap();
            let request: SysdRequest = serde_json::from_str(&line).unwrap();
            captured.lock().unwrap().push(request.clone());
            let reply = match request {
                SysdRequest::SshMaintOpen {
                    public_key,
                    ttl_seconds,
                    window_id,
                } => {
                    assert_eq!(ttl_seconds, 3600);
                    let key = duduclaw_sysd::ssh_key::parse(KEY).unwrap();
                    assert_eq!(
                        public_key,
                        key.wire(),
                        "only the canonical key goes over UDS"
                    );
                    match behavior {
                        Behavior::Current => {
                            window = Some((window_id.clone(), key.fingerprint.clone()));
                            success(
                                json!({"window_id": window_id, "key_type": key.key_type.name(),
                                "key_fingerprint": key.fingerprint, "host_key_fingerprints": ["SHA256:socket-host"]}),
                            )
                        }
                        Behavior::Busy => {
                            SysdResponse::err("busy-audit", SysdError::new("busy", "audit pending"))
                        }
                        Behavior::Old => SysdResponse::err(
                            "old-audit",
                            SysdError::bad_request(
                                "malformed request: unknown variant `ssh_maint_open`, expected one of ...",
                            ),
                        ),
                    }
                }
                SysdRequest::SshMaintStatus => success(json!({
                    "ready": true,
                    "window_id": window.as_ref().map(|w| &w.0),
                    "key_fingerprint": window.as_ref().map(|w| &w.1),
                    "key_installed": window.is_some(),
                    "ssh_active": window.is_some(),
                    "host_key_fingerprints": ["SHA256:socket-host"],
                    "pending_audit": matches!(behavior, Behavior::Busy),
                })),
                SysdRequest::SshServiceStop => {
                    window = None;
                    success(json!({"closed": true}))
                }
                other => panic!("unexpected wire request: {other:?}"),
            };
            let mut response = serde_json::to_vec(&reply).unwrap();
            response.push(b'\n');
            writer.write_all(&response).await.unwrap();
        }
    });
    let env = MaintEnv {
        home_dir: home.to_path_buf(),
        sysd: Arc::new(SocketProvider(Arc::new(SocketMaintSysd {
            client: SysdClient::new(socket),
        }))),
        notifier: Arc::new(NoNotifier),
        reconcile_retry: Duration::from_millis(5),
        reconcile_deadline: Duration::from_millis(100),
        tick_interval: Duration::from_secs(30),
    };
    (env, FakeDaemon { requests, task })
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("isolated fake sysd must answer promptly")
}

fn request(caps: &[String]) -> EnableRequest<'_> {
    EnableRequest {
        user_id: "socket-admin",
        email: "socket-admin@local",
        ttl_hours: 1,
        sub_capabilities: caps,
        ssh_public_key: Some(KEY),
        reauth_password: "socket-test-password",
    }
}

#[tokio::test]
async fn uds_enable_status_disable_uses_real_client_and_parses_replies() {
    let home = tempfile::tempdir().unwrap();
    let (env, daemon) = fake_daemon(home.path(), Behavior::Current);
    let caps = vec!["ssh".to_string()];
    let window = bounded(enable(&env, &request(&caps), &Verifier))
        .await
        .unwrap();
    let status = bounded(status_json(&env, None)).await.unwrap();
    assert_eq!(status["active"], true);
    assert_eq!(status["ssh"]["window_id"], window.id);
    assert_eq!(
        status["ssh"]["key_fingerprint"],
        window.ssh_key_fingerprint.unwrap()
    );
    assert_eq!(window.host_key_fingerprints, vec!["SHA256:socket-host"]);
    assert!(
        bounded(disable(&env, "socket-admin", None))
            .await
            .unwrap()
            .unwrap()
            .close_error
            .is_none()
    );
    let closed = bounded(status_json(&env, None)).await.unwrap();
    assert_eq!(closed["active"], false);
    assert_eq!(closed["ssh_observed_active"], false);
    let requests = daemon.requests.lock().unwrap();
    assert!(
        matches!(&requests[0], SysdRequest::SshMaintOpen { window_id, .. } if window_id == &window.id)
    );
    assert_eq!(
        &requests[1..],
        &[
            SysdRequest::SshMaintStatus,
            SysdRequest::SshServiceStop,
            SysdRequest::SshMaintStatus,
        ]
    );
}

#[tokio::test]
async fn uds_busy_does_not_send_a_rollback_stop() {
    let home = tempfile::tempdir().unwrap();
    let (env, daemon) = fake_daemon(home.path(), Behavior::Busy);
    let error = bounded(enable(&env, &request(&["ssh".to_string()]), &Verifier))
        .await
        .unwrap_err();
    assert_eq!(error.code, error_code::SSH_BUSY);
    assert_eq!(error.extra["reason"], "audit_pending");
    let requests = daemon.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(matches!(requests[0], SysdRequest::SshMaintOpen { .. }));
    assert_eq!(requests[1], SysdRequest::SshMaintStatus);
}

#[tokio::test]
async fn uds_unknown_verb_refuses_ssh_without_using_legacy_start() {
    let home = tempfile::tempdir().unwrap();
    let (env, daemon) = fake_daemon(home.path(), Behavior::Old);
    let error = bounded(enable(&env, &request(&["ssh".to_string()]), &Verifier))
        .await
        .unwrap_err();
    assert_eq!(error.code, error_code::SSH_UNSUPPORTED);
    assert!(
        MaintenanceStore::open(home.path())
            .unwrap()
            .active_window()
            .await
            .unwrap()
            .is_none()
    );
    let requests = daemon.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0], SysdRequest::SshMaintOpen { .. }));
}
