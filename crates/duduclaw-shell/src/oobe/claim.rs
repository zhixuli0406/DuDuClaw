// First-run account claim — the `AccountCreate` step's real gateway RPC
// (Shell-S2 round 1, 2026-08-20). Wires `steps::account`'s click handler to
// the gateway's own `GET /api/first-run/status` / `POST /api/first-run/claim`
// endpoints (`duduclaw-gateway/src/server.rs`'s `handle_first_run_status` /
// `handle_first_run_claim` — read-only reference, not modified from here).
//
// ── Why a hand-rolled HTTP/1.1 client ────────────────────────────────────
// This crate's `Cargo.toml` deliberately excludes itself from the parent
// workspace and pulls in exactly `gpui`/`gpui_platform`/`gpui_macros`/
// `duduclaw-native-gui`/`serde`/`serde_json` — no `reqwest`, no `tokio` (see
// that file's own header comment: keeping gpui's dependency tree isolated
// from the gateway build). `duduclaw-native-gui/src/api.rs` gets to use
// `reqwest` because IT pulls in a tokio runtime for its WebSocket chat
// connection anyway; this crate has no such runtime already paid for, and
// this is exactly ONE request pair against a `127.0.0.1` peer — a raw
// `std::net::TcpStream` request/response round trip is a few dozen lines,
// not worth a new dependency (or a tokio runtime) for. `Connection: close`
// on every request sidesteps keep-alive/chunked-encoding entirely: the
// response body is just "everything until the peer closes its write half",
// which is also the fallback this module uses when a response omits (or
// lies about) `Content-Length`.
//
// ── Why loopback is checked BEFORE connecting, not after ────────────────
// `handle_first_run_status`/`handle_first_run_claim` already refuse
// off-loopback callers server-side (`ConnectInfo(addr).ip().is_loopback()`)
// — this module's own `NonLoopback` check is not a security boundary (the
// server enforces that), it is an honesty boundary: dialing a non-loopback
// `DUDUCLAW_SHELL_GATEWAY_URL` would either hang against a firewalled host or
// silently succeed against some OTHER service entirely, and either way the
// claim flow is meaningless off-loopback (there is no operator sitting at
// THIS machine to hand a password to a remote gateway on their behalf). Same
// reasoning `duduclaw-shell` inherits from the gateway's own gate — refuse
// fail-closed before any I/O, not after a confusing failure downstream.
//
// ── Testability split ────────────────────────────────────────────────────
// `create_account` (the public entry point `steps::account` calls) is a
// thin env-reading wrapper around `create_account_at`, which takes the base
// URL as a plain parameter. Every test below calls `create_account_at`
// directly against a local mock `TcpListener`, so almost none of them touch
// `std::env` at all — the ONE test that does (`env_override_is_honored`)
// exercises `create_account` itself and follows the same `ENV_LOCK` mutex
// discipline `oobe/mod.rs`'s own env-mutating tests already establish
// (serialize + always restore the prior value), rather than a naive
// `set_var` race against whatever else `cargo test` is running in parallel.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:18789";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a successful claim attempt settled to — see `create_account_at`'s
/// own doc comment for exactly which server responses map to which variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimOutcome {
    /// This click is the one that set the password (`POST .../claim` ->
    /// 200).
    Claimed,
    /// The instance was already set up — either `GET .../status` already
    /// reported `claimable: false` (no claim attempted at all), or the
    /// claim raced another caller and lost (`POST .../claim` -> 409). Both
    /// land the operator in the exact same place: an admin account already
    /// exists, nothing more to do here.
    AlreadyClaimed,
}

/// Why a claim attempt did NOT resolve to a `ClaimOutcome`. Deliberately
/// keeps `Http`/`Malformed`/`Unreachable`/`NonLoopback` as distinct variants
/// even though `steps::account` collapses all four into one "couldn't reach
/// the local service" message (`AccountClaimFailureKind::Unreachable` in
/// `oobe/mod.rs`) — the fine-grained detail is still worth having for a
/// stderr diagnostic line and for these unit tests, even if the OOBE surface
/// itself has no reason to render four different retry messages for what is,
/// from the operator's chair, the same "try again" action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimError {
    /// Couldn't complete the TCP round trip at all — DNS/connect/read/write
    /// failure, or the connect/read/write timeout tripped. Carries the raw
    /// `std::io::Error` message for the stderr diagnostic; never shown to
    /// the operator verbatim (see `AccountClaimFailureKind`'s own doc
    /// comment in `oobe/mod.rs`).
    Unreachable(String),
    /// The gateway itself rejected the password as too short (`POST
    /// .../claim` -> 400). Mirrors `handle_first_run_claim`'s own `< 8
    /// chars` rule — `steps::account` also checks this client-side before
    /// ever dispatching a request, so reaching this variant in practice
    /// means the two length rules drifted, not that the client-side check
    /// was skipped.
    RejectedTooShort,
    /// An HTTP status this module has no specific handling for (anything
    /// other than 200/400/409 on the claim call, or anything other than 200
    /// on the status call). Carries the raw status code.
    Http(u16),
    /// The response didn't parse as the module expects — not valid
    /// HTTP/1.1 framing, not valid UTF-8 headers, not valid JSON, or JSON
    /// missing an expected field. Carries a short human-readable reason for
    /// the stderr diagnostic.
    Malformed(String),
    /// The configured base URL failed the scheme/host safety gate this
    /// module enforces BEFORE any connection is attempted — see this file's
    /// header comment ("Why loopback is checked BEFORE connecting"). Covers
    /// both "not `http://`" and "host is not one of `127.0.0.1`/`::1`/
    /// `localhost`".
    NonLoopback,
}

/// One parsed HTTP/1.1 response — status line's numeric code + the body,
/// already trimmed to `Content-Length` when the response carried one (see
/// `parse_response`'s own doc comment for the fallback when it doesn't).
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// `steps::account`'s real entry point — reads `DUDUCLAW_SHELL_GATEWAY_URL`
/// (falling back to `DEFAULT_GATEWAY_URL`) and delegates to
/// `create_account_at`, the parameterized/testable core below.
pub(crate) fn create_account(password: &str) -> Result<ClaimOutcome, ClaimError> {
    create_account_at(&gateway_base_url(), password)
}

fn gateway_base_url() -> String {
    std::env::var("DUDUCLAW_SHELL_GATEWAY_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GATEWAY_URL.to_string())
}

/// The testable core: `GET .../first-run/status`, then, only if `claimable`
/// is `true`, `POST .../first-run/claim`. Every test in this module calls
/// this directly against a local mock server rather than exercising
/// `create_account`'s env lookup.
///
/// Outcome mapping (mirrors `handle_first_run_status`/`handle_first_run_claim`
/// in `duduclaw-gateway/src/server.rs`, read there — not duplicated logic,
/// just this module's own honest restatement of the contract it's calling):
///   - status `claimable: false` -> `AlreadyClaimed` (a LOOPBACK caller
///     seeing `false` means this instance was already set up by an earlier
///     run — off-loopback callers always see `false` regardless, but this
///     module already refused to even dial a non-loopback host, so that
///     ambiguity never reaches here).
///   - status `claimable: true`, then claim 200 -> `Claimed`.
///   - claim 409 -> `AlreadyClaimed` (lost a race against another claimer).
///   - claim 400 -> `Err(RejectedTooShort)`.
///   - anything else -> `Err(Http(status))` / `Err(Malformed(..))` /
///     `Err(Unreachable(..))` as appropriate.
pub(crate) fn create_account_at(base_url: &str, password: &str) -> Result<ClaimOutcome, ClaimError> {
    let (host, port) = parse_loopback_base_url(base_url)?;

    let status_resp = http_get(&host, port, "/api/first-run/status")?;
    if status_resp.status != 200 {
        return Err(ClaimError::Http(status_resp.status));
    }
    let status_json: serde_json::Value = serde_json::from_slice(&status_resp.body)
        .map_err(|e| ClaimError::Malformed(format!("first-run status response was not valid JSON: {e}")))?;
    let claimable = status_json
        .get("claimable")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| ClaimError::Malformed("first-run status response missing a boolean `claimable` field".to_string()))?;
    if !claimable {
        return Ok(ClaimOutcome::AlreadyClaimed);
    }

    let body = serde_json::to_vec(&serde_json::json!({ "password": password }))
        .map_err(|e| ClaimError::Malformed(format!("could not encode claim request body: {e}")))?;
    let claim_resp = http_post_json(&host, port, "/api/first-run/claim", &body)?;
    match claim_resp.status {
        200 => Ok(ClaimOutcome::Claimed),
        409 => Ok(ClaimOutcome::AlreadyClaimed),
        400 => Err(ClaimError::RejectedTooShort),
        other => Err(ClaimError::Http(other)),
    }
}

/// Parses `http://host[:port]` (optionally with a trailing path, which is
/// ignored — this crate only ever calls two fixed paths of its own, it has
/// no need for a general-purpose URL parser) and enforces the scheme/host
/// safety gate in one pass, so a caller can never observe a validated host
/// that turns out not to be loopback. IPv6 literals use the standard
/// bracketed form (`http://[::1]:18789`), matching how `connect()` below
/// re-brackets the host for `ToSocketAddrs`.
fn parse_loopback_base_url(url: &str) -> Result<(String, u16), ClaimError> {
    let rest = url.strip_prefix("http://").ok_or(ClaimError::NonLoopback)?;
    let authority = rest.split('/').next().unwrap_or(rest);

    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let mut parts = after_bracket.splitn(2, ']');
        let host = parts.next().ok_or(ClaimError::NonLoopback)?.to_string();
        let port = match parts.next() {
            Some(p) if !p.is_empty() => p.strip_prefix(':').and_then(|d| d.parse::<u16>().ok()).ok_or(ClaimError::NonLoopback)?,
            _ => 80,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| ClaimError::NonLoopback)?),
            None => (authority.to_string(), 80),
        }
    };

    let host_lower = host.to_ascii_lowercase();
    let is_loopback = host_lower == "127.0.0.1" || host_lower == "::1" || host_lower == "localhost";
    if !is_loopback {
        return Err(ClaimError::NonLoopback);
    }
    Ok((host_lower, port))
}

fn connect(host: &str, port: u16) -> Result<TcpStream, ClaimError> {
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut addrs = target.to_socket_addrs().map_err(|e| ClaimError::Unreachable(e.to_string()))?;
    let sock_addr = addrs.next().ok_or_else(|| ClaimError::Unreachable("could not resolve any address".to_string()))?;
    let stream = TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT).map_err(|e| ClaimError::Unreachable(e.to_string()))?;
    // Best-effort: a failure to SET a timeout is not itself a reason to
    // abort the request (the connection already succeeded), so these two
    // are intentionally not propagated as errors.
    let _ = stream.set_read_timeout(Some(READ_WRITE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_WRITE_TIMEOUT));
    Ok(stream)
}

fn http_get(host: &str, port: u16, path: &str) -> Result<HttpResponse, ClaimError> {
    let mut stream = connect(host, port)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n");
    send_and_read(&mut stream, request.as_bytes())
}

fn http_post_json(host: &str, port: u16, path: &str, body: &[u8]) -> Result<HttpResponse, ClaimError> {
    let mut stream = connect(host, port)?;
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    send_and_read(&mut stream, &request)
}

/// Writes the whole request, then reads to EOF — safe because every request
/// this module sends carries `Connection: close`, so the peer closing its
/// write half IS the end of the response, not a hang. `parse_response` below
/// still prefers an explicit `Content-Length` when present (see its own doc
/// comment).
fn send_and_read(stream: &mut TcpStream, request: &[u8]) -> Result<HttpResponse, ClaimError> {
    stream.write_all(request).map_err(|e| ClaimError::Unreachable(e.to_string()))?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| ClaimError::Unreachable(e.to_string()))?;
    parse_response(&raw)
}

/// Status line -> headers -> body. Body length prefers an explicit
/// `Content-Length` header (truncating to it, in case the peer kept the
/// connection open a moment longer than expected before closing); when the
/// header is absent, malformed, or larger than what was actually read, the
/// bytes already read to EOF (see `send_and_read`) stand as the body
/// unmodified — the same fallback this file's header comment already names.
fn parse_response(raw: &[u8]) -> Result<HttpResponse, ClaimError> {
    let split_at =
        raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| ClaimError::Malformed("no header/body separator in HTTP response".to_string()))?;
    let header_text = std::str::from_utf8(&raw[..split_at]).map_err(|_| ClaimError::Malformed("response headers were not valid UTF-8".to_string()))?;
    let mut body = raw[split_at + 4..].to_vec();

    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| ClaimError::Malformed("empty HTTP response".to_string()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ClaimError::Malformed(format!("could not parse HTTP status line: {status_line:?}")))?;

    if let Some(len) = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
    }) {
        if len <= body.len() {
            body.truncate(len);
        }
    }

    Ok(HttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// Binds an ephemeral loopback port and serves `responses` in order, one
    /// per accepted connection (every request this module sends is its own
    /// fresh TCP connection, since every request carries `Connection:
    /// close`) — so a two-request scenario (status then claim) just needs a
    /// two-element `responses` list. Each canned response is raw bytes,
    /// written verbatim (status line, headers, body — the test itself
    /// decides whether to include `Content-Length` or rely on the
    /// close-terminates-body fallback, exercising both paths across the
    /// suite below). Returns the `http://127.0.0.1:<port>` base URL to feed
    /// into `create_account_at`.
    fn start_mock_server(responses: Vec<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut stream, _)) = listener.accept() else { break };
                // Drain (best-effort, small fixed buffer — every request in
                // this suite is well under it) whatever the client already
                // sent before responding; not required for correctness (the
                // canned response doesn't depend on the request), but avoids
                // a spurious RST on some platforms if the client is still
                // mid-write when this thread writes back.
                let mut sink = [0u8; 4096];
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.read(&mut sink);
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn claim_success() {
        let base = start_mock_server(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"claimable\":true}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n{\"ok\":true}\n",
        ]);
        assert_eq!(create_account_at(&base, "a-real-password"), Ok(ClaimOutcome::Claimed));
    }

    #[test]
    fn status_claimable_false_short_circuits_to_already_claimed_with_no_claim_call() {
        // Only ONE canned response — if this module mistakenly issued a
        // second (claim) request, the mock server's second `accept()` would
        // just never resolve and this thread would hang waiting on the
        // client, which the client's own read/connect timeouts would
        // eventually surface as a test failure rather than a silent pass.
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 19\r\n\r\n{\"claimable\":false}"]);
        assert_eq!(create_account_at(&base, "a-real-password"), Ok(ClaimOutcome::AlreadyClaimed));
    }

    #[test]
    fn claim_409_maps_to_already_claimed() {
        let base = start_mock_server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 18\r\n\r\n{\"claimable\":true}",
            "HTTP/1.1 409 Conflict\r\nContent-Length: 9\r\n\r\n{\"x\":1}\n\n",
        ]);
        assert_eq!(create_account_at(&base, "a-real-password"), Ok(ClaimOutcome::AlreadyClaimed));
    }

    #[test]
    fn claim_400_maps_to_rejected_too_short() {
        let base = start_mock_server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 18\r\n\r\n{\"claimable\":true}",
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 7\r\n\r\n{\"x\":1}",
        ]);
        assert_eq!(create_account_at(&base, "short"), Err(ClaimError::RejectedTooShort));
    }

    #[test]
    fn connection_refused_maps_to_unreachable() {
        // Bind, learn the ephemeral port, then drop the listener before
        // anyone connects — the port is very likely to still be refusing
        // connections for the brief moment this test needs.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let base = format!("http://{addr}");
        let result = create_account_at(&base, "a-real-password");
        assert!(matches!(result, Err(ClaimError::Unreachable(_))), "{result:?}");
    }

    #[test]
    fn malformed_json_body_is_reported_not_panicked_on() {
        // No `Content-Length` here on purpose — this exercises the
        // read-to-EOF fallback path (`send_and_read` reads until the mock
        // server closes its write half) rather than the `Content-Length`
        // path the other tests already cover.
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\nthis is not json"]);
        let result = create_account_at(&base, "a-real-password");
        assert!(matches!(result, Err(ClaimError::Malformed(_))), "{result:?}");
    }

    #[test]
    fn non_loopback_url_is_refused_without_any_connection_attempt() {
        // No mock server started at all — if this module dialed out before
        // checking the host, there would be nothing listening and the call
        // would only fail after `CONNECT_TIMEOUT` (3s). Asserting it returns
        // well under that is this test's proof that no connection was
        // attempted, on top of the error variant itself being correct.
        let started = std::time::Instant::now();
        let result = create_account_at("http://example.com:18789", "a-real-password");
        assert_eq!(result, Err(ClaimError::NonLoopback));
        assert!(started.elapsed() < Duration::from_millis(500), "must fail before ever dialing out, elapsed={:?}", started.elapsed());
    }

    #[test]
    fn non_http_scheme_is_also_refused_as_non_loopback() {
        let result = create_account_at("https://127.0.0.1:18789", "a-real-password");
        assert_eq!(result, Err(ClaimError::NonLoopback));
    }

    // `set_var`/`remove_var` are process-global and `unsafe` on this
    // toolchain — same discipline `oobe/mod.rs`'s own `ENV_LOCK`-guarded
    // tests already establish (serialize via a local mutex, always restore
    // the prior value on the way out). This is the ONLY test in this module
    // that exercises the env-reading wrapper (`create_account`) rather than
    // `create_account_at` directly.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_override_is_honored() {
        let _g = ENV_LOCK.lock().unwrap();
        let base = start_mock_server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 18\r\n\r\n{\"claimable\":true}",
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"ok\":true}\n",
        ]);
        let prev = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok();
        unsafe { std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", &base) };

        let result = create_account("a-real-password");

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", v),
                None => std::env::remove_var("DUDUCLAW_SHELL_GATEWAY_URL"),
            }
        }

        assert_eq!(result, Ok(ClaimOutcome::Claimed), "create_account() must have dialed the mock server via the env override, not the real default port");
    }

    /// Live-fire pairing check against a REAL gateway — `#[ignore]`d so
    /// `cargo test` never depends on one being up. Verification playbook
    /// (used for Shell-S2 round 1's acceptance, see the design doc's §14
    /// progress log):
    ///   1. start a gateway with a FRESH (unclaimed) home, e.g.
    ///      `DUDUCLAW_HOME=$(mktemp -d) DUDUCLAW_PORT=28792 duduclaw run`
    ///   2. `DUDUCLAW_SHELL_LIVE_GATEWAY_URL=http://127.0.0.1:28792 \
    ///      cargo test -- --ignored live_first_run_claim --nocapture`
    ///
    /// Asserts the full first-claim -> already-claimed sequence, i.e. the
    /// exact two outcomes `steps::account` renders differently. Without the
    /// env var set it does nothing (still passes) so a bare
    /// `cargo test -- --ignored` on a dev machine can't fail spuriously.
    #[test]
    #[ignore = "requires a live gateway with a fresh (unclaimed) home — see doc comment"]
    fn live_first_run_claim_against_env_gateway() {
        let Some(base) = std::env::var("DUDUCLAW_SHELL_LIVE_GATEWAY_URL").ok().filter(|v| !v.trim().is_empty()) else {
            eprintln!("[live] DUDUCLAW_SHELL_LIVE_GATEWAY_URL not set — skipping");
            return;
        };
        let first = create_account_at(&base, "livetest-oobe-passw0rd");
        assert_eq!(first, Ok(ClaimOutcome::Claimed), "first claim against a fresh home must settle Claimed");
        let second = create_account_at(&base, "livetest-oobe-passw0rd");
        assert_eq!(second, Ok(ClaimOutcome::AlreadyClaimed), "second claim must settle AlreadyClaimed");
        eprintln!("[live] Claimed -> AlreadyClaimed sequence verified against {base}");
    }

    #[test]
    fn parse_loopback_base_url_accepts_all_three_loopback_spellings() {
        assert_eq!(parse_loopback_base_url("http://127.0.0.1:18789"), Ok(("127.0.0.1".to_string(), 18789)));
        assert_eq!(parse_loopback_base_url("http://localhost:18789"), Ok(("localhost".to_string(), 18789)));
        assert_eq!(parse_loopback_base_url("http://LOCALHOST:18789"), Ok(("localhost".to_string(), 18789)), "host match must be case-insensitive");
        assert_eq!(parse_loopback_base_url("http://[::1]:18789"), Ok(("::1".to_string(), 18789)));
    }

    #[test]
    fn parse_loopback_base_url_rejects_other_hosts() {
        assert_eq!(parse_loopback_base_url("http://192.168.1.5:18789"), Err(ClaimError::NonLoopback));
        assert_eq!(parse_loopback_base_url("http://example.com"), Err(ClaimError::NonLoopback));
        assert_eq!(parse_loopback_base_url("not-even-a-url"), Err(ClaimError::NonLoopback));
    }
}
