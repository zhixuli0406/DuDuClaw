//! Re-exec the native test binary, so all three CI platforms need no shell/network.
use super::*;
use tokio::io::AsyncWriteExt;

const FIXTURE: &str = "run_identity::ipc_tests::process_tree_fixture";
const ROLE: &str = "DUDUCLAW_IDENTITY_TEST_ROLE";
const RESULTS: &str = "DUDUCLAW_IDENTITY_TEST_RESULTS";

fn command(role: &str, endpoint: &str, token: &str, results: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", FIXTURE, "--nocapture"])
        .env(ROLE, role).env(RESULTS, results).env(ENV_IDENTITY_ENDPOINT, endpoint)
        .env(ENV_RUN_CREDENTIAL, token).env_remove(ENV_IDENTITY_URL)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit()).kill_on_drop(true);
    cmd
}

#[test]
fn process_tree_fixture() {
    let Ok(role) = std::env::var(ROLE) else { return; };
    if role == "root" {
        let mut go = String::new();
        assert!(std::io::stdin().read_line(&mut go).unwrap() > 0);
    }
    let endpoint = std::env::var(ENV_IDENTITY_ENDPOINT).unwrap();
    let token = std::env::var(ENV_RUN_CREDENTIAL).unwrap();
    let results = std::path::PathBuf::from(std::env::var_os(RESULTS).unwrap());
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let identity = ipc::request(&endpoint, &serde_json::json!({
            "version": 1, "operation": "introspect", "credential": token,
        })).await.unwrap();
        let reference = ipc::request(&endpoint, &serde_json::json!({
            "version": 1, "operation": "tool-reference", "credential": token,
            "reference": {"tool_name": "docs_create", "ref_kind": "google_doc",
                "ref_id": format!("doc-{role}"), "label": role},
        })).await.unwrap();
        std::fs::write(results.join(&role), serde_json::to_vec(&serde_json::json!({
            "identity": identity["active"], "reference": reference["active"],
        })).unwrap()).unwrap();
        let next = match role.as_str() { "root" => Some("child"), "child" => Some("grandchild"), _ => None };
        if let Some(next) = next {
            let mut child = command(next, &endpoint, &token, &results).spawn().unwrap();
            assert!(tokio::time::timeout(Duration::from_secs(15), child.wait()).await.unwrap().unwrap().success());
        }
    });
    if role == "root" {
        std::fs::write(results.join("ready"), b"ready").unwrap();
        // Keep the bound root alive while the unrelated token holder is tested.
        let mut finish = String::new();
        assert!(std::io::stdin().read_line(&mut finish).unwrap() > 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tree_allows_root_child_grandchild_and_denies_same_user_token_thief() {
    // A short home also fits macOS's 104-byte sockaddr_un path budget.
    #[cfg(unix)]
    let home = tempfile::tempdir_in("/tmp").unwrap();
    #[cfg(windows)]
    let home = tempfile::tempdir().unwrap();
    let results = tempfile::tempdir().unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    let registry = Arc::new(Registry::default());
    let listener = start_listener_with(home.path(), registry.clone()).await.unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    sessions.get_or_create("session-1", "alice").await.unwrap();
    let lease = registry.issue(super::tests::record(), 60).unwrap();
    let endpoint = lease.identity_endpoint.as_ref().unwrap();
    let mut root = command("root", endpoint, lease.credential.expose(), results.path()).spawn().unwrap();
    lease.binding.bind_spawn(RootProcess::capture(root.id().unwrap()).unwrap()).unwrap();
    root.stdin.as_mut().unwrap().write_all(b"go\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !results.path().join("ready").exists() {
            assert!(root.try_wait().unwrap().is_none(), "fixture root exited early");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    for role in ["root", "child", "grandchild"] {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(results.path().join(role)).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"identity": true, "reference": true}), "{role}");
    }
    // Spawned by the test parent, not by root. Same account, exact same token/endpoint.
    let mut thief = command("outsider", endpoint, lease.credential.expose(), results.path()).spawn().unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(10), thief.wait()).await.unwrap().unwrap().success());
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(results.path().join("outsider")).unwrap()).unwrap();
    assert_eq!(value, serde_json::json!({"identity": false, "reference": false}));
    let references = sessions.tool_refs_context("session-1").await.unwrap();
    assert!(!references.contains("doc-outsider"));
    assert!(references.contains("doc-grandchild"));
    root.stdin.as_mut().unwrap().write_all(b"finish\n").await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(5), root.wait()).await.unwrap().unwrap().success());
    assert!(lease.binding.root().is_none());
    drop(listener);
}

#[cfg(unix)]
type ClientStream = tokio::net::UnixStream;
#[cfg(windows)]
type ClientStream = tokio::net::windows::named_pipe::NamedPipeClient;

async fn connect(endpoint: &str) -> ClientStream {
    let path = ipc::endpoint_path(endpoint).unwrap();
    #[cfg(unix)]
    { tokio::net::UnixStream::connect(path).await.unwrap() }
    #[cfg(windows)]
    {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                    Ok(stream) => break stream,
                    Err(e) if e.raw_os_error() == Some(231) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(e) => panic!("local pipe connect failed: {e}"),
                }
            }
        }).await.unwrap()
    }
}

#[tokio::test]
async fn ipc_rejects_http_oversize_and_slow_frames_and_closes_after_one_request() {
    use tokio::io::AsyncReadExt;
    #[cfg(unix)]
    let home = tempfile::tempdir_in("/tmp").unwrap();
    #[cfg(windows)]
    let home = tempfile::tempdir().unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    let registry = Arc::new(Registry::default());
    let _listener = start_listener_with(home.path(), registry.clone()).await.unwrap();
    let lease = registry.issue(super::tests::record(), 60).unwrap();
    lease.binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).unwrap();
    let endpoint = lease.identity_endpoint.as_ref().unwrap();
    for header in [((ipc::MAX_REQUEST + 1) as u32).to_be_bytes(), *b"POST"] {
        let mut stream = connect(endpoint).await;
        stream.write_all(&header).await.unwrap();
        let response = ipc::read_frame(&mut stream, ipc::MAX_RESPONSE).await.unwrap();
        assert_eq!(response, serde_json::json!({"active": false}));
        stream.write_u8(1).await.unwrap();
    }
    let mut stream = connect(endpoint).await;
    let request = serde_json::json!({"version": 1, "operation": "introspect", "credential": lease.credential.expose()});
    ipc::write_frame(&mut stream, &request, ipc::MAX_REQUEST).await.unwrap();
    assert_eq!(ipc::read_frame(&mut stream, ipc::MAX_RESPONSE).await.unwrap()["active"], true);
    stream.write_u8(1).await.unwrap();
    let _ = ipc::write_frame(&mut stream, &request, ipc::MAX_REQUEST).await;
    assert!(tokio::time::timeout(Duration::from_secs(5), ipc::read_frame(&mut stream, ipc::MAX_RESPONSE))
        .await.unwrap().is_err());
    let mut slow = connect(endpoint).await;
    slow.write_u32(100).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(5), slow.read_u8()).await.unwrap().is_err());
}
