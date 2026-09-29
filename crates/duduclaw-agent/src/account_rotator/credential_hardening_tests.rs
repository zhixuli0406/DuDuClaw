// ── 2026-09 credential-hardening regression tests (D2 / D3 / D4 / D5) ───
//
// The incident these pin down: a setup-token started returning
// `403 oauth_not_allowed_for_organization`; the rotator benched the account
// after three generic errors and a fake health probe (`claude auth status` →
// `loggedIn: true`) resurrected it 60 seconds later, forever — burning one
// scheduled dispatch per resurrection for 18 hours. Separately, an
// `oauth_token_enc` that decrypted to an empty string loaded as a perfectly
// normal account and spawned credential-less children with zero warnings.
mod load_cases;
mod probe_cases;

use super::*;

use crate::credential_probe::CredentialKind;

use std::sync::atomic::{AtomicU64, Ordering};

// ── fixtures ────────────────────────────────────────────────────

/// An Anthropic OAuth account carrying an explicit setup-token — the shape
/// the credential probe can actually authenticate.
fn token_account(id: &str) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority: 1,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "default".to_string(),
        email: String::new(),
        subscription: "max".to_string(),
        label: id.to_string(),
        expires_at: None,
        api_key: String::new(),
        oauth_token: Some(format!("sk-ant-oat01-{id}")),
        credentials_dir: None,
        is_healthy: true,
        consecutive_errors: 0,
        spent_this_month: 0,
        cooldown_until: None,
        last_used: None,
        total_requests: 0,
        credential_state: CredentialState::Unverified,
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    }
}

/// An Anthropic OAuth account with NO explicit token — an OS-keychain
/// session, which has no secret we can present to the API.
fn keychain_account(id: &str) -> Account {
    let mut a = token_account(id);
    a.oauth_token = None;
    a.credentials_dir = Some(PathBuf::from("/tmp/duduclaw-fake-credentials"));
    a
}

async fn snapshot(rotator: &AccountRotator, id: &str) -> Account {
    let accounts = rotator.accounts.read().await;
    accounts
        .iter()
        .find(|a| a.id == id)
        .expect("account present")
        .clone()
}

/// Fixed-response HTTP server that keeps answering until the test drops
/// its handle. Dependency-free on purpose (this crate carries no test
/// HTTP-server dependency and does not need one).
async fn spawn_repeating_server(
    response: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), handle)
}

/// Like [`spawn_repeating_server`], but answers the FIRST request with
/// `first` and every later one with `rest`, and hands back a counter of
/// how many requests actually reached the wire.
///
/// The counter is the only honest way to assert a probe was *skipped*:
/// account state alone cannot tell "we asked and nothing changed" apart
/// from "we never asked". The sequencing lets one account walk from a
/// conclusive rejection into a recovery without rebuilding the rotator
/// (`with_probe_base_url` is a constructor-time builder).
async fn spawn_sequenced_server(
    first: &'static str,
    rest: &'static str,
) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&hits);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            // Counted after the request head is in hand, and before the
            // reply — so a probe that has returned to its caller is
            // always already counted (no flaky ordering).
            let nth = counter.fetch_add(1, Ordering::SeqCst);
            let body = if nth == 0 { first } else { rest };
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), hits, handle)
}

/// Every request gets the same answer; the counter still records them.
async fn spawn_counting_server(
    response: &'static str,
) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    spawn_sequenced_server(response, response).await
}

const RESP_401: &str =
    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_403: &str =
    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_500: &str =
    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

const RESP_200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";

// ── (d) load-time broken-credential detection ───────────────────

static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_home(label: &str) -> tempfile::TempDir {
    let _ = HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    tempfile::Builder::new()
        .prefix(&format!("duduclaw-cred-{label}-"))
        .tempdir()
        .expect("tempdir")
}

