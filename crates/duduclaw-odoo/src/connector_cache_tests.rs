//! Network fixtures only run when the owner executes the test suite.
use super::*;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct TestHome(PathBuf);
impl TestHome {
    /// The descriptor-pinned cache opens the home itself (O_DIRECTORY |
    /// O_NOFOLLOW), so the home must exist; canonicalize so a symlinked temp
    /// root (macOS /var → /private/var) is never the final component.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("odoo-session-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }
    fn path(&self) -> &Path { &self.0 }
}
impl Drop for TestHome { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

async fn server(auth_result: Value, status: u16) -> (OdooConfig, Arc<AtomicUsize>, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let auth_calls = Arc::new(AtomicUsize::new(0));
    let calls = auth_calls.clone();
    let total_calls = Arc::new(AtomicUsize::new(0));
    let total = total_calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut buf = [0u8; 4096];
                let count = socket.read(&mut buf).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buf[..count]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((header, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = header.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                    }).unwrap();
                    if body.len() >= length { break serde_json::from_str::<Value>(body).unwrap(); }
                }
            };
            total.fetch_add(1, Ordering::SeqCst);
            let method = request["params"]["method"].as_str().unwrap();
            if method == "execute_kw" {
                assert_eq!(request["params"]["args"][1], 7);
                assert!(matches!(request["params"]["args"][2].as_str(), Some("key-one" | "key-two")));
            }
            let result = match method {
                "authenticate" => { calls.fetch_add(1, Ordering::SeqCst); auth_result.clone() }
                "version" => json!({"result":{"server_version":"19.0"}}),
                _ if request["params"]["args"][3] == "ir.module.module" && request["params"]["args"][4] == "search_read" => json!({"result":[]}),
                _ if request["params"]["args"][4] == "fields_get" => json!({"result":{"name":{"type":"char"}}}),
                _ => json!({"result":[]}),
            };
            let body = result.to_string();
            let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (OdooConfig { url, db: "test".into(), username: "tester".into(), ..Default::default() }, auth_calls, total_calls, task)
}

#[tokio::test]
async fn cached_uid_skips_auth_and_edition_but_data_call_sends_current_credential() {
    // The on-disk cache is descriptor-pinned and therefore Unix-only; off Unix
    // it is deliberately disabled (`connection_cache::store`), so every
    // connect authenticates afresh. Expect exactly that on each platform.
    let cached = cfg!(unix);
    let hit = usize::from(!cached);
    let (config, auth, total, task) = server(json!({"result":7}), 200).await;
    let home = TestHome::new();
    let first = OdooConnector::connect_cached(&config, "key-one", Some(home.path()), vec![3]).await.unwrap();
    assert_eq!(first.uid, Some(7));
    assert_eq!(first.status().version, "19.0");
    assert_eq!(auth.load(Ordering::SeqCst), 1);
    let first_total = total.load(Ordering::SeqCst);
    let second = OdooConnector::connect_cached(&config, "key-one", Some(home.path()), vec![3]).await.unwrap();
    assert_eq!(auth.load(Ordering::SeqCst), 1 + hit);
    if cached {
        assert_eq!(total.load(Ordering::SeqCst), first_total, "cache hit must make no authentication, edition or version RPC");
    }
    assert_eq!(first.connection_identity(), second.connection_identity());
    second.execute_kw("crm.lead", "search_read", vec![], json!({})).await.unwrap();
    assert_eq!(auth.load(Ordering::SeqCst), 1 + hit);
    // A different credential or company scope never reuses a cached identity.
    OdooConnector::connect_cached(&config, "key-two", Some(home.path()), vec![3]).await.unwrap();
    assert_eq!(auth.load(Ordering::SeqCst), 2 + hit);
    OdooConnector::connect_cached(&config, "key-two", Some(home.path()), vec![4]).await.unwrap();
    assert_eq!(auth.load(Ordering::SeqCst), 3 + hit);
    task.abort();
}

#[tokio::test]
async fn authentication_denials_are_single_attempt_and_5xx_retries_are_bounded() {
    for result in [json!({"result":false}), json!({"error":{"data":{"name":"odoo.exceptions.AccessDenied"}}})] {
        let (config, calls, _, task) = server(result, 200).await;
        let error = OdooConnector::connect(&config, "bad-key").await.err().unwrap();
        assert!(error.contains("after 1 attempt"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
    for (status, expected) in [(401, 1), (403, 1), (500, 3), (503, 3), (408, 3)] {
        let (config, auth_calls, _, task) = server(json!({"result":false}), status).await;
        assert!(OdooConnector::connect(&config, "key").await.is_err());
        assert_eq!(auth_calls.load(Ordering::SeqCst), expected);
        task.abort();
    }
}
