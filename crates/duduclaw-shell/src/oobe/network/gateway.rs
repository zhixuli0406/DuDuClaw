// Real Wi-Fi backend — the LOCAL gateway process's pre-auth
// `/api/first-run/network/*` HTTP endpoints. D4a-5 (2026-08-23), the fix
// for the D4a §1.2/§5.4 ship-blocker (see `mod.rs`'s own header comment for
// the full story): the appliance image ships iwd, driven only by the
// gateway over D-Bus (`commercial/docs/DESIGN-network-settings-2026-08.md`
// §2/§3) — the shell itself never joins the `netdev` group and never talks
// D-Bus at all (§3.3: keeping the kiosk shell, the machine's largest attack
// surface, out of direct Wi-Fi control). This module is the shell-side half
// of that split; the gateway-side `network/iwd.rs` + `network.*` RPC +
// `/api/first-run/network/*` handlers are implemented elsewhere
// (`duduclaw-gateway`, out of this crate's scope — see this repo's own
// parallel-work notice for D4a-5).
//
// ── Why a hand-rolled HTTP/1.1 client, duplicated from `oobe::claim` ──────
// Same reasoning `oobe/claim.rs`'s own header comment gives in full (read
// there first) — this crate deliberately excludes itself from the parent
// workspace and has no `reqwest`/`tokio` HTTP client available cheaply. The
// client below is a near-verbatim copy of `claim.rs`'s own GET/POST/
// parse-response machinery, not a shared helper: `claim.rs` is explicitly
// OUT of this task's edit scope (a parallel agent may be touching
// gateway/appliance code that `claim.rs` itself documents), and `claim`'s
// helper functions are private to that module anyway (no `pub`), so sharing
// would require widening `claim.rs`'s own API surface for a single second
// caller. A few dozen duplicated lines is the smaller, safer change.
//
// ── Why loopback is checked BEFORE connecting, why `DUDUCLAW_SHELL_
// GATEWAY_URL` is honored ──────────────────────────────────────────────────
// Identical reasoning to `claim.rs`'s own header comment: the gateway's
// `/api/first-run/*` handlers already refuse off-loopback callers
// server-side, so this module's own loopback check is an honesty boundary
// (dialing a non-loopback configured URL is meaningless off-loopback — there
// is no operator sitting at THIS machine to act on the far end's Wi-Fi),
// not a second security layer. `DUDUCLAW_SHELL_GATEWAY_URL` is the SAME env
// var `claim.rs` reads (both talk to the same local gateway process on the
// same port), so a dev/test override that redirects account-claim traffic
// redirects Wi-Fi traffic too, consistently.
//
// ── Testability split ────────────────────────────────────────────────────
// Every public operation below has an `_at(base_url, ..)` pure/testable
// core (mirroring `claim::create_account_at`) that `GatewayNetworkBackend`'s
// trait methods just call with `self.base_url`. Every test in this module's
// own `tests` mod calls the `_at` functions directly against a local mock
// `TcpListener`, same pattern `claim.rs`'s own test module already
// establishes.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

use super::{
    AccessPoint, ConnStatus, InternetState, NetError, NetworkBackend, NetworkStatus,
    WifiFailureCode,
};

const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:18789";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
// More generous than `claim.rs`'s 5s: unlike an account claim, the gateway
// answers `network/scan`/`network/connect` only AFTER it has finished
// talking to iwd itself (a real Wi-Fi scan takes several seconds; a real
// join + DHCP round trip can take longer still — the design doc's own
// `nm.rs`-era `CONNECT_TIMEOUT` budgeted 15s for the equivalent D-Bus poll).
// This is OUR read timeout on the gateway's HTTP response, so it has to
// exceed the gateway's own worst-case work, with margin.
const READ_WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// One HTTP/1.1 response — status line's numeric code + the body. Mirrors
/// `claim.rs`'s own `HttpResponse`, not reused from there (see this file's
/// header comment).
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Internal HTTP-transport failure — mirrors `oobe::claim::ClaimError`'s
/// shape, minus that type's `Http(u16)` variant: `claim.rs` only ever makes
/// ONE HTTP call per outcome path, so a non-200/400/409 status is genuinely
/// "some other status, unspecified". This module's requests all funnel
/// through `decode_envelope` instead, which inspects `HttpResponse.status`
/// directly (403 gets its own specific `NetError::Unavailable` message,
/// anything else non-200 gets a status-carrying one) — so there is no
/// separate construction site that would ever need an `HttpError::Http`
/// variant of its own. Never surfaced directly to `steps::network`: every
/// public fn below maps this into `NetError` via `to_net_error`, so the
/// rest of this crate only ever sees the one error type the
/// `NetworkBackend` trait already defines.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HttpError {
    Unreachable(String),
    Malformed(String),
    NonLoopback,
}

fn to_net_error(e: HttpError) -> NetError {
    match e {
        HttpError::Unreachable(reason) => {
            NetError::Unavailable(format!("could not reach the local gateway: {reason}"))
        }
        HttpError::Malformed(reason) => {
            NetError::Unavailable(format!("gateway response was malformed: {reason}"))
        }
        HttpError::NonLoopback => NetError::Unavailable(
            "configured gateway URL is not loopback — refusing to dial it".to_string(),
        ),
    }
}

fn gateway_base_url() -> String {
    std::env::var("DUDUCLAW_SHELL_GATEWAY_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GATEWAY_URL.to_string())
}

/// Parses `http://host[:port]` and enforces the scheme/host safety gate in
/// one pass — verbatim logic to `claim.rs`'s own `parse_loopback_base_url`
/// (see that fn's own doc comment), duplicated per this file's header
/// comment.
fn parse_loopback_base_url(url: &str) -> Result<(String, u16), HttpError> {
    let rest = url.strip_prefix("http://").ok_or(HttpError::NonLoopback)?;
    let authority = rest.split('/').next().unwrap_or(rest);

    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let mut parts = after_bracket.splitn(2, ']');
        let host = parts.next().ok_or(HttpError::NonLoopback)?.to_string();
        let port = match parts.next() {
            Some(p) if !p.is_empty() => p
                .strip_prefix(':')
                .and_then(|d| d.parse::<u16>().ok())
                .ok_or(HttpError::NonLoopback)?,
            _ => 80,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| HttpError::NonLoopback)?,
            ),
            None => (authority.to_string(), 80),
        }
    };

    let host_lower = host.to_ascii_lowercase();
    let is_loopback = host_lower == "127.0.0.1" || host_lower == "::1" || host_lower == "localhost";
    if !is_loopback {
        return Err(HttpError::NonLoopback);
    }
    Ok((host_lower, port))
}

fn connect(host: &str, port: u16) -> Result<TcpStream, HttpError> {
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut addrs = target
        .to_socket_addrs()
        .map_err(|e| HttpError::Unreachable(e.to_string()))?;
    let sock_addr = addrs
        .next()
        .ok_or_else(|| HttpError::Unreachable("could not resolve any address".to_string()))?;
    let stream = TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT)
        .map_err(|e| HttpError::Unreachable(e.to_string()))?;
    // Best-effort, same as `claim.rs`: a failure to SET a timeout is not
    // itself a reason to abort a request the connection already succeeded
    // for.
    let _ = stream.set_read_timeout(Some(READ_WRITE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_WRITE_TIMEOUT));
    Ok(stream)
}

fn http_get(host: &str, port: u16, path: &str) -> Result<HttpResponse, HttpError> {
    let mut stream = connect(host, port)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n");
    send_and_read(&mut stream, request.as_bytes())
}

fn http_post_json(
    host: &str,
    port: u16,
    path: &str,
    body: &[u8],
) -> Result<HttpResponse, HttpError> {
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
/// this module sends carries `Connection: close`. See `claim.rs`'s own
/// `send_and_read` doc comment.
fn send_and_read(stream: &mut TcpStream, request: &[u8]) -> Result<HttpResponse, HttpError> {
    stream
        .write_all(request)
        .map_err(|e| HttpError::Unreachable(e.to_string()))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| HttpError::Unreachable(e.to_string()))?;
    parse_response(&raw)
}

/// Status line -> headers -> body. Verbatim logic to `claim.rs`'s own
/// `parse_response` (see that fn's own doc comment for the `Content-Length`
/// vs. read-to-EOF fallback reasoning).
fn parse_response(raw: &[u8]) -> Result<HttpResponse, HttpError> {
    let split_at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| {
            HttpError::Malformed("no header/body separator in HTTP response".to_string())
        })?;
    let header_text = std::str::from_utf8(&raw[..split_at])
        .map_err(|_| HttpError::Malformed("response headers were not valid UTF-8".to_string()))?;
    let mut body = raw[split_at + 4..].to_vec();

    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| HttpError::Malformed("empty HTTP response".to_string()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            HttpError::Malformed(format!("could not parse HTTP status line: {status_line:?}"))
        })?;

    if let Some(len) = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    }) {
        if len <= body.len() {
            body.truncate(len);
        }
    }

    Ok(HttpResponse { status, body })
}

/// Decodes the `{"ok": bool, ...}` envelope D4a §5.2 specifies for every
/// endpoint below. `Ok(result)` is the `"result"` object on success;
/// `Err(NetError::Classified(code))` is a well-formed `ok:false` response
/// (HTTP 200 — see this fn's own 403 special-case for the ONE non-200
/// status the design doc gives explicit meaning to); any transport failure,
/// unexpected HTTP status, or unparseable body collapses to `NetError::
/// Unavailable` via `to_net_error`/the explicit mappings below — never a
/// panic, never a silently-assumed success.
fn decode_envelope(resp: HttpResponse) -> Result<Value, NetError> {
    if resp.status == 403 {
        // D4a §5.2: 403 = the gateway's own first-run gate refused this
        // caller (not loopback, first run already completed, or not
        // running on an appliance). In practice this path should never be
        // reached (this module already refuses non-loopback URLs itself,
        // and OOBE only runs before first-run completes) — if it IS
        // reached, something upstream disagrees about state, and the
        // honest answer is "the network service isn't available", not a
        // guess at which of the three gate conditions tripped.
        return Err(NetError::Unavailable("gateway refused this caller (HTTP 403 — not loopback, first run already completed, or not an appliance)".to_string()));
    }
    if resp.status != 200 {
        return Err(NetError::Unavailable(format!(
            "gateway returned unexpected HTTP {}",
            resp.status
        )));
    }
    let json: Value = serde_json::from_slice(&resp.body)
        .map_err(|e| NetError::Unavailable(format!("gateway response was not valid JSON: {e}")))?;
    let ok = json.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let code = json
            .get("code")
            .and_then(Value::as_str)
            .map(WifiFailureCode::parse)
            .unwrap_or(WifiFailureCode::Unknown);
        return Err(NetError::Classified(code));
    }
    json.get("result")
        .cloned()
        .ok_or_else(|| NetError::Unavailable("gateway response missing `result`".to_string()))
}

fn fetch_status_result_at(base_url: &str) -> Result<Value, NetError> {
    let (host, port) = parse_loopback_base_url(base_url).map_err(to_net_error)?;
    let resp = http_get(&host, port, "/api/first-run/network/status").map_err(to_net_error)?;
    decode_envelope(resp)
}

/// Typed `GET /api/first-run/network/status` — D4a §5.2.
pub(crate) fn fetch_status_at(base_url: &str) -> Result<NetworkStatus, NetError> {
    fetch_status_result_at(base_url).map(|result| parse_network_status(&result))
}

/// The SAME status endpoint, read for `NetworkBackend::status()`'s narrower
/// `ConnStatus` shape (`wifi.state`/`wifi.ssid` only) — kept as a separate
/// fn rather than deriving `ConnStatus` from `NetworkStatus` because the
/// gateway's `wifi.state` has a THIRD live value (`"connecting"`) that
/// `NetworkStatus`/`InternetState` (link+internet layer) has no equivalent
/// for; re-parsing the same `result` JSON twice on one round trip is
/// cheaper than inventing a combined type only one caller (`kick_off_scan`)
/// would ever destructure. Best-effort: any failure honestly reports
/// `Disconnected` rather than panicking or guessing — `status()`'s own
/// signature has no `Result` to propagate one through.
fn wifi_conn_status_at(base_url: &str) -> ConnStatus {
    match fetch_status_result_at(base_url) {
        Ok(result) => parse_wifi_conn_status(&result),
        Err(_) => ConnStatus::Disconnected,
    }
}

fn parse_wifi_conn_status(result: &Value) -> ConnStatus {
    let wifi = result.get("wifi");
    let state = wifi
        .and_then(|w| w.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("disconnected");
    let ssid = wifi
        .and_then(|w| w.get("ssid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match (state, ssid) {
        ("connected", Some(ssid)) => ConnStatus::Connected { ssid },
        ("connecting", Some(ssid)) => ConnStatus::Connecting { ssid },
        ("connecting", None) => ConnStatus::Connecting {
            ssid: String::new(),
        },
        _ => ConnStatus::Disconnected,
    }
}

fn parse_network_status(result: &Value) -> NetworkStatus {
    let wifi_ssid = result
        .get("wifi")
        .and_then(|w| w.get("ssid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let has_ip = result
        .get("ip")
        .and_then(|ip| ip.get("addresses"))
        .and_then(Value::as_array)
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);
    let internet = result
        .get("internet")
        .and_then(Value::as_str)
        .map(InternetState::parse)
        .unwrap_or_default();
    let portal_url = result
        .get("portal_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    NetworkStatus {
        internet,
        has_ip,
        wifi_ssid,
        portal_url,
    }
}

/// `POST /api/first-run/network/scan` — D4a §5.2. Always requests a fresh
/// scan (`{"rescan": true}`): both of `steps::network`'s two click targets
/// ("掃描 Wi-Fi" and "重新整理") are the operator explicitly asking for one,
/// and the trait's `scan()` signature has no parameter to distinguish them
/// anyway.
pub(crate) fn scan_at(base_url: &str) -> Result<Vec<AccessPoint>, NetError> {
    let (host, port) = parse_loopback_base_url(base_url).map_err(to_net_error)?;
    let body = serde_json::to_vec(&serde_json::json!({ "rescan": true }))
        .map_err(|e| NetError::Unavailable(format!("could not encode scan request body: {e}")))?;
    let resp =
        http_post_json(&host, port, "/api/first-run/network/scan", &body).map_err(to_net_error)?;
    let result = decode_envelope(resp)?;
    Ok(parse_networks(&result))
}

/// Parses the `"networks"` array — skips (never panics on) any entry
/// missing a usable `ssid`, and skips `hidden: true` entries the same way
/// `nm::NmNetworkBackend::scan` already skips a blank SSID (no name to show
/// is no row to render, same UX every consumer Wi-Fi picker takes).
fn parse_networks(result: &Value) -> Vec<AccessPoint> {
    let Some(list) = result.get("networks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut aps = Vec::new();
    for entry in list {
        let Some(ssid) = entry.get("ssid").and_then(Value::as_str) else {
            continue;
        };
        if ssid.is_empty() {
            continue;
        }
        let hidden = entry
            .get("hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if hidden {
            continue;
        }
        let signal_bars = entry
            .get("signal_bars")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 4) as u8)
            .unwrap_or(1);
        let security = entry
            .get("security")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let known = entry.get("known").and_then(Value::as_bool).unwrap_or(false);
        aps.push(AccessPoint {
            ssid: ssid.to_string(),
            signal_bars,
            security,
            known,
        });
    }
    aps
}

/// `POST /api/first-run/network/connect` — D4a §5.2. `psk: None` omits the
/// field entirely (open network / rely on a saved credential), matching the
/// gateway's own "`psk` 省略 = open network 或使用已存憑證" contract.
pub(crate) fn connect_at(base_url: &str, ssid: &str, psk: Option<&str>) -> Result<(), NetError> {
    let (host, port) = parse_loopback_base_url(base_url).map_err(to_net_error)?;
    let mut payload = serde_json::Map::new();
    payload.insert("ssid".to_string(), Value::String(ssid.to_string()));
    if let Some(p) = psk {
        payload.insert("psk".to_string(), Value::String(p.to_string()));
    }
    let body = serde_json::to_vec(&Value::Object(payload)).map_err(|e| {
        NetError::Unavailable(format!("could not encode connect request body: {e}"))
    })?;
    let resp = http_post_json(&host, port, "/api/first-run/network/connect", &body)
        .map_err(to_net_error)?;
    decode_envelope(resp)?;
    Ok(())
}

/// The `NetworkBackend` impl `select_backend` (`mod.rs`) hands out on an
/// appliance. Holds only a base URL — no persistent connection, same "fresh
/// TCP connection per call" simplicity `oobe::claim`'s own client already
/// established (D-Bus-era `nm.rs`'s "fresh `Connection` per instance, not a
/// cached pool" reasoning applies here too, just one layer up the stack).
pub(crate) struct GatewayNetworkBackend {
    base_url: String,
}

impl GatewayNetworkBackend {
    /// Constructs the backend AND checks reachability in one call —
    /// `select_backend`'s own doc comment explains why this returns `(Self,
    /// Result<(), NetError>)` rather than `Result<Self, NetError>` the way
    /// `nm::NmNetworkBackend::probe` used to: D4a §5.4's ship-blocker fix
    /// requires that an appliance ALWAYS gets a real (if currently failing)
    /// backend, never `FakeNetworkBackend` — so the caller needs `Self`
    /// unconditionally; only the reachability VERDICT (which decides
    /// `NetBackendKind::Real` vs. `Unavailable`) is fallible. The
    /// reachability check itself is just a `GET .../network/status` call —
    /// no separate lightweight probe endpoint exists, and this one is cheap
    /// (a single JSON GET) and doubles as the very first data `steps::
    /// network` would want anyway.
    pub(crate) fn probe() -> (Self, Result<(), NetError>) {
        let backend = Self {
            base_url: gateway_base_url(),
        };
        let probe_result = fetch_status_at(&backend.base_url).map(|_| ());
        (backend, probe_result)
    }
}

impl NetworkBackend for GatewayNetworkBackend {
    fn scan(&self) -> Result<Vec<AccessPoint>, NetError> {
        scan_at(&self.base_url)
    }

    fn connect(&self, ssid: &str, psk: Option<&str>) -> Result<(), NetError> {
        connect_at(&self.base_url, ssid, psk)
    }

    fn status(&self) -> ConnStatus {
        wifi_conn_status_at(&self.base_url)
    }

    /// D4a §5.1: "沒有 forget 的 pre-auth 端點（刻意；OOBE 沒有忘記網路的
    /// UI）" — the gateway's own pre-auth network API deliberately has no
    /// `forget` endpoint. This is not a missed implementation; there is
    /// simply nothing to call. `NetworkBackend::forget`'s own doc comment
    /// explains why `steps::network` has no UI entry point for this anyway
    /// (managing previously-saved profiles is out of scope for a first-run
    /// wizard) — this override just makes that honest for THIS backend
    /// instead of silently inheriting a default that would claim success.
    fn forget(&self, _ssid: &str) -> Result<(), NetError> {
        Err(NetError::Unavailable(
            "forget is not exposed on the first-run path".to_string(),
        ))
    }

    fn network_status(&self) -> Result<NetworkStatus, NetError> {
        fetch_status_at(&self.base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Binds an ephemeral loopback port and serves `responses` in order, one
    /// per accepted connection — same shape `claim.rs`'s own `start_mock_
    /// server` establishes (every request here carries `Connection: close`,
    /// so each is its own fresh TCP connection). Duplicated rather than
    /// shared for the same reason the whole HTTP client above is (this
    /// file's header comment).
    fn start_mock_server(responses: Vec<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut sink = [0u8; 4096];
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.read(&mut sink);
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        format!("http://{addr}")
    }

    // ── scan ─────────────────────────────────────────────────────────────

    #[test]
    fn scan_success_parses_networks_and_skips_hidden_and_empty_ssid_entries() {
        let base = start_mock_server(vec![concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n",
            r#"{"ok":true,"result":{"networks":["#,
            r#"{"ssid":"DuDu-Office","signal_bars":4,"security":"psk","connected":false,"known":true,"hidden":false},"#,
            r#"{"ssid":"DuDu-Guest","signal_bars":2,"security":"open","connected":false,"known":false,"hidden":false},"#,
            r#"{"ssid":"secret","signal_bars":3,"security":"psk","connected":false,"known":false,"hidden":true},"#,
            r#"{"ssid":"","signal_bars":1,"security":"open","connected":false,"known":false,"hidden":false}"#,
            r#"],"scanning":false}}"#,
        )]);
        let aps = scan_at(&base).expect("scan must succeed");
        assert_eq!(
            aps.len(),
            2,
            "hidden and empty-ssid entries must be skipped: {aps:?}"
        );
        assert_eq!(
            aps[0],
            AccessPoint {
                ssid: "DuDu-Office".to_string(),
                signal_bars: 4,
                security: "psk".to_string(),
                known: true
            }
        );
        assert_eq!(
            aps[1],
            AccessPoint {
                ssid: "DuDu-Guest".to_string(),
                signal_bars: 2,
                security: "open".to_string(),
                known: false
            }
        );
    }

    #[test]
    fn scan_clamps_out_of_range_signal_bars_and_defaults_missing_security() {
        let base = start_mock_server(vec![concat!(
            "HTTP/1.1 200 OK\r\n\r\n",
            r#"{"ok":true,"result":{"networks":[{"ssid":"Weird","signal_bars":99,"connected":false,"known":false,"hidden":false}],"scanning":false}}"#,
        )]);
        let aps = scan_at(&base).unwrap();
        assert_eq!(aps.len(), 1);
        assert_eq!(
            aps[0].signal_bars, 4,
            "signal_bars must clamp into 1..=4, never pass an out-of-spec value through"
        );
        assert_eq!(
            aps[0].security, "unknown",
            "a missing security field must default, never panic"
        );
    }

    // ── connect ──────────────────────────────────────────────────────────

    #[test]
    fn connect_success() {
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"result\":{\"state\":\"connected\",\"ssid\":\"DuDu-Office\"}}"]);
        assert_eq!(
            connect_at(&base, "DuDu-Office", Some("a-real-password")),
            Ok(())
        );
    }

    #[test]
    fn connect_open_network_omits_psk_field_from_the_request_body() {
        // No mock-server assertion on the wire body itself (this suite has
        // no request-capturing mock), but this at least proves the `None`
        // path builds a valid request and parses a normal success reply —
        // the actual omission is covered structurally (`payload.insert`
        // only runs inside `if let Some(p) = psk`).
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"result\":{\"state\":\"connected\",\"ssid\":\"DuDu-Guest\"}}"]);
        assert_eq!(connect_at(&base, "DuDu-Guest", None), Ok(()));
    }

    // ── the nine classified failure codes + Unknown ────────────────────

    #[test]
    fn connect_failure_parses_each_of_the_nine_classified_codes() {
        let codes: &[(&str, WifiFailureCode)] = &[
            ("wrong_password", WifiFailureCode::WrongPassword),
            ("not_found", WifiFailureCode::NotFound),
            ("out_of_range", WifiFailureCode::OutOfRange),
            ("no_adapter", WifiFailureCode::NoAdapter),
            ("driver_missing", WifiFailureCode::DriverMissing),
            ("no_ip", WifiFailureCode::NoIp),
            ("portal", WifiFailureCode::Portal),
            ("backend_unavailable", WifiFailureCode::BackendUnavailable),
            ("unsupported_security", WifiFailureCode::UnsupportedSecurity),
        ];
        for (code, expected) in codes {
            let body = format!(
                r#"{{"ok":false,"code":"{code}","message":"irrelevant, this module never renders the server-supplied message"}}"#
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let base = start_mock_server(vec![Box::leak(resp.into_boxed_str())]);
            let result = connect_at(&base, "SomeSsid", Some("whatever-12345"));
            assert_eq!(result, Err(NetError::Classified(*expected)), "code={code}");
        }
    }

    #[test]
    fn connect_failure_with_an_unrecognized_code_falls_back_to_unknown_not_a_panic_not_a_success() {
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\n{\"ok\":false,\"code\":\"a_future_code_this_build_predates\",\"message\":\"x\"}"]);
        assert_eq!(
            connect_at(&base, "SomeSsid", None),
            Err(NetError::Classified(WifiFailureCode::Unknown))
        );
    }

    #[test]
    fn connect_failure_missing_a_code_field_entirely_still_falls_back_to_unknown() {
        let base = start_mock_server(vec![
            "HTTP/1.1 200 OK\r\n\r\n{\"ok\":false,\"message\":\"no code at all\"}",
        ]);
        assert_eq!(
            connect_at(&base, "SomeSsid", None),
            Err(NetError::Classified(WifiFailureCode::Unknown))
        );
    }

    // ── HTTP-layer failure modes ─────────────────────────────────────────

    #[test]
    fn http_403_maps_to_an_unavailable_error_not_a_classified_code() {
        let base = start_mock_server(vec!["HTTP/1.1 403 Forbidden\r\n\r\n{}"]);
        assert!(matches!(scan_at(&base), Err(NetError::Unavailable(_))));
    }

    #[test]
    fn http_400_maps_to_an_unavailable_error() {
        let base = start_mock_server(vec![
            "HTTP/1.1 400 Bad Request\r\n\r\n{\"error\":\"malformed body\"}",
        ]);
        assert!(matches!(
            connect_at(&base, "x", None),
            Err(NetError::Unavailable(_))
        ));
    }

    #[test]
    fn malformed_json_body_is_reported_not_panicked_on() {
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\nthis is not json at all"]);
        assert!(matches!(scan_at(&base), Err(NetError::Unavailable(_))));
    }

    #[test]
    fn a_response_missing_result_on_ok_true_is_reported_not_panicked_on() {
        let base = start_mock_server(vec!["HTTP/1.1 200 OK\r\n\r\n{\"ok\":true}"]);
        assert!(matches!(scan_at(&base), Err(NetError::Unavailable(_))));
    }

    #[test]
    fn non_loopback_url_is_refused_without_any_connection_attempt() {
        let started = std::time::Instant::now();
        let result = scan_at("http://example.com:18789");
        assert!(matches!(result, Err(NetError::Unavailable(_))));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "must fail before ever dialing out, elapsed={:?}",
            started.elapsed()
        );
    }

    #[test]
    fn non_http_scheme_is_also_refused_as_non_loopback() {
        assert!(matches!(
            connect_at("https://127.0.0.1:18789", "x", None),
            Err(NetError::Unavailable(_))
        ));
    }

    #[test]
    fn connection_refused_maps_to_unavailable_not_a_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let base = format!("http://{addr}");
        assert!(matches!(scan_at(&base), Err(NetError::Unavailable(_))));
    }

    // ── status ───────────────────────────────────────────────────────────

    #[test]
    fn fetch_status_parses_online_with_ip_and_ssid() {
        let base = start_mock_server(vec![concat!(
            "HTTP/1.1 200 OK\r\n\r\n",
            r#"{"ok":true,"result":{"#,
            r#""wifi":{"state":"connected","ssid":"DuDu-Office","signal_bars":4,"security":"psk","frequency":2412},"#,
            r#""ip":{"interface":"wlan0","addresses":["192.168.1.23/24"],"gateway":"192.168.1.1","dns":["192.168.1.1"],"source":"dhcp"},"#,
            r#""internet":"online","portal_url":null,"interfaces":[]}}"#,
        )]);
        let status = fetch_status_at(&base).expect("status must succeed");
        assert_eq!(status.internet, InternetState::Online);
        assert!(status.has_ip);
        assert_eq!(status.wifi_ssid.as_deref(), Some("DuDu-Office"));
        assert_eq!(status.portal_url, None);
    }

    #[test]
    fn fetch_status_parses_portal_with_its_url() {
        let base = start_mock_server(vec![concat!(
            "HTTP/1.1 200 OK\r\n\r\n",
            r#"{"ok":true,"result":{"#,
            r#""wifi":{"state":"connected","ssid":"Cafe-Guest","signal_bars":2,"security":"open","frequency":2437},"#,
            r#""ip":{"interface":"wlan0","addresses":["10.0.0.5/24"],"gateway":"10.0.0.1","dns":[],"source":"dhcp"},"#,
            r#""internet":"portal","portal_url":"http://10.0.0.1/login","interfaces":[]}}"#,
        )]);
        let status = fetch_status_at(&base).unwrap();
        assert_eq!(status.internet, InternetState::Portal);
        assert!(status.internet.counts_as_connected());
        assert_eq!(status.portal_url.as_deref(), Some("http://10.0.0.1/login"));
    }

    #[test]
    fn fetch_status_with_no_ip_addresses_reports_has_ip_false() {
        let base = start_mock_server(vec![concat!(
            "HTTP/1.1 200 OK\r\n\r\n",
            r#"{"ok":true,"result":{"#,
            r#""wifi":{"state":"connecting","ssid":"DuDu-Office","signal_bars":3,"security":"psk","frequency":2412},"#,
            r#""ip":{"interface":"wlan0","addresses":[],"gateway":null,"dns":[],"source":"unknown"},"#,
            r#""internet":"unknown","portal_url":null,"interfaces":[]}}"#,
        )]);
        let status = fetch_status_at(&base).unwrap();
        assert!(!status.has_ip);
        assert_eq!(status.internet, InternetState::Unknown);
        assert!(!status.internet.counts_as_connected());
    }
}
