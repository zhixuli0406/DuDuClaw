//! v1.81 E3: the home OAuth exchange never follows a redirect; a dead GitHub
//! home grant fences the SharedHome github routes; a route revoked during a
//! GitHub home refresh discards the new token. Owned loopback only; not
//! compiled or run here (Linux builder).
use super::*;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::isolation_connector_events::{Target, e3_test_support::{company, personal, selection}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};

/// Read one HTTP/1.1 request completely (headers + Content-Length body).
async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await.unwrap();
        if read == 0 { return buffer; }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let headers = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
        let length = headers.lines().find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok()).unwrap_or(0);
        if buffer.len() >= end + 4 + length { return buffer; }
    }
}

/// One owned loopback token endpoint: runs `on_request` after the request
/// arrived and before answering `status` + JSON `body`.
async fn token_endpoint(status: &'static str, body: serde_json::Value, on_request: impl FnOnce() + Send + 'static)
    -> (u16, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        on_request();
        let body = serde_json::to_vec(&body).unwrap();
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len());
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    (port, handle)
}

fn github_config(token_url: String) -> McpOAuthConfig {
    McpOAuthConfig {
        provider_id: "github".into(),
        client_id: "fixture-client".into(),
        client_secret: "fixture-client-secret".into(),
        auth_url: "http://127.0.0.1/auth".into(),
        token_url,
        scopes: vec!["repo".into()],
        redirect_uri: "http://127.0.0.1/callback".into(),
    }
}

/// A connected GitHub home OAuth whose access token has expired.
fn expired_github_home(home: &Path, token_url: String) {
    let config = github_config(token_url);
    upsert_client_config(home, McpOAuthClientConfig {
        provider_id: config.provider_id, client_id: config.client_id, client_secret: config.client_secret,
        auth_url: config.auth_url, token_url: config.token_url, scopes: config.scopes,
        redirect_uri: config.redirect_uri,
    }).unwrap();
    // Legacy plaintext row (read transparently, like the other home fixtures).
    std::fs::write(home.join(TOKEN_FILE), serde_json::to_vec(&serde_json::json!([{
        "provider_id": "github", "access_token": "fixture-old-access", "refresh_token": "fixture-old-refresh",
        "expires_at": (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(), "scopes": ["repo"],
    }])).unwrap()).unwrap();
    assert_eq!(stored_github(home), ("fixture-old-access".into(), Some("fixture-old-refresh".into())));
}
fn stored_github(home: &Path) -> (String, Option<String>) {
    let token = load_tokens(home).into_iter().find(|t| t.provider_id == "github").expect("github token kept");
    (token.access_token, token.refresh_token)
}

#[tokio::test]
async fn e3_exchange_code_does_not_follow_redirects() {
    // The redirect target counts every connection it receives.
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_port = target.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let target_task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = target.accept().await else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = read_request(&mut stream).await;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 37\r\nConnection: close\r\n\r\n{\"access_token\":\"stolen-new-token-x\"}").await;
        }
    });
    for status in ["307 Temporary Redirect", "302 Found", "308 Permanent Redirect", "303 See Other"] {
        let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint_port = endpoint.local_addr().unwrap().port();
        let received = tokio::spawn(async move {
            let (mut stream, _) = endpoint.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            // A JSON body that would parse as tokens if a 3xx were accepted.
            let body = "{\"access_token\":\"redirect-body-token\",\"refresh_token\":\"redirect-body-refresh\"}";
            let response = format!("HTTP/1.1 {status}\r\nLocation: http://127.0.0.1:{target_port}/steal\r\n\
                Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        let config = github_config(format!("http://127.0.0.1:{endpoint_port}/login/oauth/access_token"));
        let result = tokio::time::timeout(std::time::Duration::from_secs(10),
            exchange_code(&config, "fixture-code", "fixture-verifier")).await.expect("exchange finished");
        let error = result.err().expect("a redirecting token endpoint is a failed exchange");
        assert!(error.contains("redirect refused"), "{error}");
        for secret in ["fixture-code", "fixture-verifier", "fixture-client-secret", "redirect-body"] {
            assert!(!error.contains(secret), "{error}");
        }
        // The code reached only the configured endpoint.
        let request = String::from_utf8(received.await.unwrap()).unwrap();
        assert!(request.contains("code=fixture-code"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect target must never be contacted");
    target_task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn e3_github_home_invalid_grant_revokes_shared_home() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let catalog = ConnectorCatalog::open(home.path()).unwrap();
    let github = catalog.register("home github", "github", ConnectorKind::SharedHome, &company()).unwrap();
    let notion = catalog.register("home notion", "notion", ConnectorKind::SharedHome, &company()).unwrap();
    let alice = catalog.register("alice github", "github", ConnectorKind::Personal, &personal("alice")).unwrap();
    let target = Target::home("github").unwrap();
    // GitHub reports a dead refresh token with HTTP 200 and an error body.
    let (port, served) = token_endpoint("200 OK", serde_json::json!({"error":"bad_refresh_token",
        "error_description":"fixture-provider-description"}), || {}).await;
    expired_github_home(home.path(), format!("http://127.0.0.1:{port}/login/oauth/access_token"));

    assert!(crate::github_workspace::get_valid_github_token(home.path()).await.is_err());
    assert!(served.await.unwrap().contains("refresh_token=fixture-old-refresh"));
    assert!(!catalog.route(&github).unwrap().unwrap().active);
    assert!(catalog.route(&notion).unwrap().unwrap().active);
    assert!(catalog.route(&alice).unwrap().unwrap().active);
    assert_eq!(catalog.pending_relinks(target.scope()).unwrap(), 1);
    // The stored material is not replaced by the failed refresh.
    assert_eq!(stored_github(home.path()), ("fixture-old-access".into(), Some("fixture-old-refresh".into())));
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("home_oauth_consent_revoked"));
    assert!(!audit.contains("fixture-old") && !audit.contains("fixture-provider-description"));

    // The typed classifier: invalid_grant (RFC 6749) is a dead grant too;
    // other failures are not.
    let (port, served) = token_endpoint("400 Bad Request", serde_json::json!({"error":"invalid_grant"}), || {}).await;
    let config = github_config(format!("http://127.0.0.1:{port}/login/oauth/access_token"));
    assert!(matches!(refresh_token_checked(&config, "r").await, Err(RefreshFailure::InvalidGrant(_))));
    served.await.unwrap();
    let (port, served) = token_endpoint("503 Service Unavailable", serde_json::json!({"error":"temporarily_unavailable"}), || {}).await;
    let config = github_config(format!("http://127.0.0.1:{port}/login/oauth/access_token"));
    assert!(matches!(refresh_token_checked(&config, "r").await, Err(RefreshFailure::Failed(_))));
    served.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn e3_revoke_during_github_refresh_discards_new_token() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let catalog = ConnectorCatalog::open(home.path()).unwrap();
    let account = catalog.register("home github", "github", ConnectorKind::SharedHome, &company()).unwrap();
    let selected = selection(home.path(), &account, "alice", "github_search_issues", true);
    // The route is revoked while GitHub is answering the refresh.
    let path = home.path().to_path_buf();
    let revoked = account.clone();
    let (port, served) = token_endpoint("200 OK", serde_json::json!({"access_token":"fixture-new-access",
        "refresh_token":"fixture-new-refresh","token_type":"bearer","expires_in":28800}), move || {
        ConnectorCatalog::open(&path).unwrap().revoke(&revoked).unwrap();
    }).await;
    expired_github_home(home.path(), format!("http://127.0.0.1:{port}/login/oauth/access_token"));

    let result = duduclaw_mcp_boundary::io_authority::scope(selected.io_context(),
        crate::github_workspace::get_valid_github_token(home.path())).await;
    let error = result.err().expect("refused after the route moved").to_string();
    assert!(!error.contains("fixture-new"), "{error}");
    served.await.unwrap();
    // The new token was discarded: neither stored nor returned.
    assert_eq!(stored_github(home.path()), ("fixture-old-access".into(), Some("fixture-old-refresh".into())));
    assert!(selected.recheck().is_err());
    // A revoke is not a dead grant: no consent-revoke event.
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(!audit.contains("home_oauth_consent_revoked"));
}
