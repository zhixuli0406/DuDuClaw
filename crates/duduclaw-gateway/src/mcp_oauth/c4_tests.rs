//! v1.81 C4 (D2 follow-up): the refresh client never follows a redirect,
//! and a client-config read error is distinguishable from absence.
use super::*;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Read one HTTP/1.1 request completely (headers + Content-Length body) so
/// the response is never mistaken for a connection reset.
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

#[tokio::test]
async fn c4_refresh_client_does_not_follow_redirects() {
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
    for status in ["307 Temporary Redirect", "302 Found", "308 Permanent Redirect"] {
        let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint_port = endpoint.local_addr().unwrap().port();
        let received = tokio::spawn(async move {
            let (mut stream, _) = endpoint.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let response = format!("HTTP/1.1 {status}\r\nLocation: http://127.0.0.1:{target_port}/steal\r\n\
                Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        let config = github_config(format!("http://127.0.0.1:{endpoint_port}/login/oauth/access_token"));
        let result = tokio::time::timeout(std::time::Duration::from_secs(10),
            refresh_token(&config, "fixture-refresh-token")).await.expect("refresh finished");
        let error = result.err().expect("a redirecting token endpoint is a failed refresh");
        assert!(error.contains("redirect refused"), "{error}");
        assert!(!error.contains("fixture-refresh-token") && !error.contains("fixture-client-secret"), "{error}");
        // The grant reached only the configured endpoint.
        let request = String::from_utf8(received.await.unwrap()).unwrap();
        assert!(request.contains("refresh_token=fixture-refresh-token"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect target must never be contacted");
    target_task.abort();
}

#[test]
fn c4_client_config_read_error_fails_closed() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let path = home.path().join(CLIENT_CONFIG_FILE);
    // Absence is distinguishable: Ok(None).
    assert_eq!(read_client_config(home.path(), "github").map(|c| c.is_some()), Ok(false));
    assert!(get_client_config(home.path(), "github").is_none());

    let config = McpOAuthClientConfig {
        provider_id: "github".into(), client_id: "fixture-client".into(), client_secret: "fixture-client-secret".into(),
        auth_url: "https://github.com/login/oauth/authorize".into(),
        token_url: "https://github.com/login/oauth/access_token".into(),
        scopes: vec!["repo".into()], redirect_uri: "http://127.0.0.1/callback".into(),
    };
    upsert_client_config(home.path(), config).unwrap();
    let present = read_client_config(home.path(), "github").unwrap().expect("present");
    assert_eq!(present.client_secret, "fixture-client-secret");
    assert_eq!(read_client_config(home.path(), "notion").map(|c| c.is_some()), Ok(false));

    // Damaged store: Err, never Ok(None).
    for damaged in ["not json", "{\"provider_id\":\"github\"}", "[{\"provider_id\":\"github\",\"client_id\":7}]"] {
        std::fs::write(&path, damaged).unwrap();
        let read = read_client_config(home.path(), "github");
        assert!(read.is_err(), "{damaged}: {read:?}");
        assert!(!read.unwrap_err().contains("fixture-client-secret"));
        assert!(get_client_config(home.path(), "github").is_none());
    }
    // Not a regular file (symlink to a valid copy): Err.
    #[cfg(unix)]
    {
        let elsewhere = home.path().join("elsewhere.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&elsewhere, "[]").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(read_client_config(home.path(), "github").is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "not json").unwrap();
    }

    // The protected GitHub home token source refuses on a read error before
    // any token read or refresh (absence would mean "no refresh possible").
    use crate::isolation_connector_native_home::{GITHUB, HomeTokenSource, OAuthVaultHomeTokens};
    let source = OAuthVaultHomeTokens::new(home.path().to_path_buf()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    assert_eq!(runtime.block_on(source.home_token(GITHUB)).err(),
        Some(duduclaw_mcp_boundary::VerificationError::Unavailable));
}
