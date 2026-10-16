//! `POST /api/internal/sysd`: how the separate `duduclaw mcp-server`
//! process (the `os_*` MCP tools) reaches the root `duduclaw-sysd` daemon
//! on an appliance — through the gateway, never directly.
//!
//! Why: once sysd runs with `--require-main-pid-of duduclaw-gateway.service`
//! it answers only the gateway's main process, so a tool running in an
//! `mcp-server` child must ask the gateway. Following the project rule for
//! facts the live gateway owns, the request goes over a signed loopback
//! route; the gateway then calls sysd with its own socket client.
//!
//! What the route checks, all fail closed:
//! - authentication exactly as the computer-use route does
//!   (`computer_use_sessions::auth`): loopback peer, the employee id, a
//!   timestamp within the skew window, a fresh nonce and a signature keyed
//!   by the gateway-internal MCP key over the employee's identity token and
//!   the body hash; per-employee rate limit; replay guard;
//! - the body names this route (`"route": "sysd"`), so a signed body made
//!   for another internal route cannot be replayed here;
//! - the verb is on [`forwardable`], the closed set the `os_*` tools use
//!   (power, update status, verified update install, boot assessment, rollback, factory
//!   reset). SSH, network, hostname, timezone, local-model and Wi-Fi-only
//!   verbs are refused even with a valid signature;
//!
//! None of these checks stops a same-uid agent that forges the signature
//! (see "Known limit"), so the allowlist holds only verbs that cannot hand
//! back root on their own: `update_rollback` stays only because sysd itself
//! refuses a target older than `--min-rollback-version` whatever the caller;
//! `sysupdate_stage_apply` installs only a release root has verified itself
//! (signature, hashes, newer than running, at least the floor); and
//! `clear_exhausted_update_target` only deletes an exhausted, non-running
//! entry. Plain `sysupdate_apply` is not forwarded: on an image whose
//! sysupdate reads the gateway's download directory it installs whatever a
//! same-uid process put there. The rest cost availability or data (reboot, power off, factory
//! reset), not privilege.
//! - the host is an appliance with a sysd socket.
//!
//! The `mcp-server` side keeps every gate it has (scopes, `[permissions]`,
//! confirmation, the factory-reset approval) and only then forwards.
//!
//! Known limit (the identity model's own): a process of the gateway's OS
//! user, which on DuDuClaw OS includes an AI employee with a shell, can read
//! `identity.key` and the internal key and sign requests here too. The route
//! narrows what such a process can ask for to the verbs above; protection
//! against regaining root is sysd's rollback floor, not this route.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use duduclaw_sysd::{SysdClientError, SysdOpOutput, SysdRequest};
use serde::{Deserialize, Serialize};

use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};

/// The route path.
pub const ROUTE: &str = "/api/internal/sysd";
/// Largest accepted request body (every sysd request is well under 1 KiB).
pub const MAX_BODY_BYTES: usize = 4 * 1024;
/// The body's route tag.
const ROUTE_TAG: &str = "sysd";
/// Client timeout: covers sysd's own longest verb (update apply), like
/// `SysdClient`'s default.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(620);

/// The request body.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteBody {
    pub route: String,
    pub request: SysdRequest,
}

/// The answer: sysd's own output or error, or the route's refusal.
#[derive(Debug, Serialize, Deserialize)]
pub struct RouteAnswer {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<SysdOpOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RouteError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteError {
    pub kind: String,
    pub message: String,
}

/// The verbs the `os_*` MCP tools need. A closed list: anything else is
/// refused by the route whatever the signature.
pub fn forwardable(req: &SysdRequest) -> bool {
    matches!(
        req,
        SysdRequest::Reboot
            | SysdRequest::Poweroff
            | SysdRequest::SysupdateStatus
            | SysdRequest::SysupdateStageApply
            | SysdRequest::BootAssessmentStatus
            | SysdRequest::UpdateRollback
            | SysdRequest::ClearExhaustedUpdateTarget { .. }
            | SysdRequest::FactoryResetV2 { .. }
    )
}

// ── process-wide switch (mcp-server side) ───────────────────────────────

static FORWARD_HOME: OnceLock<PathBuf> = OnceLock::new();

/// Called once by `duduclaw mcp-server` at start: on an appliance, every
/// sysd call this process makes goes through the gateway route of `home`.
pub fn forward_sysd_through_gateway(home: &Path) {
    let _ = FORWARD_HOME.set(home.to_path_buf());
}

/// The home whose gateway route this process forwards to, if switched on.
pub fn forward_home() -> Option<&'static Path> {
    FORWARD_HOME.get().map(PathBuf::as_path)
}

// ── server ───────────────────────────────────────────────────────────────

struct RouteState {
    home: PathBuf,
    replay: ReplayGuard,
    rate: RateLimiter,
}

/// The router, merged into the gateway app (served with connect info).
pub fn router(home: PathBuf) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(Arc::new(RouteState { home, replay: ReplayGuard::default(), rate: RateLimiter::default() }))
}

fn answer(status: StatusCode, kind: &str, message: &str) -> Response {
    let body = RouteAnswer {
        ok: false,
        data: None,
        error: Some(RouteError { kind: kind.into(), message: message.into() }),
    };
    (status, Json(body)).into_response()
}

fn unauthorized() -> Response {
    answer(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "拒絕存取：這個端點只接受 DuDuClaw gateway 啟動的 MCP 程序在本機呼叫，並需通過員工身分驗證。",
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What the route decides for an authenticated body (pure, unit-tested).
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Forward,
    BadRequest,
    VerbNotAllowed,
    NotAppliance,
}

pub fn decide(body: &[u8], is_appliance: bool, socket_exists: bool) -> (Decision, Option<SysdRequest>) {
    let Ok(parsed) = serde_json::from_slice::<RouteBody>(body) else {
        return (Decision::BadRequest, None);
    };
    if parsed.route != ROUTE_TAG {
        return (Decision::BadRequest, None);
    }
    if !forwardable(&parsed.request) {
        return (Decision::VerbNotAllowed, None);
    }
    if !(is_appliance && socket_exists) {
        return (Decision::NotAppliance, None);
    }
    (Decision::Forward, Some(parsed.request))
}

async fn handle(
    State(state): State<Arc<RouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() {
        return unauthorized();
    }
    let Ok(body) = body else {
        return answer(StatusCode::PAYLOAD_TOO_LARGE, "bad_request", "請求內容太大或無法讀取。");
    };
    let caller = match authenticate(&state.home, peer, &headers, &body, unix_now()) {
        Ok(caller) => caller,
        Err(failure) => {
            tracing::debug!(reason = failure.as_str(), "sysd route refused");
            return unauthorized();
        }
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) {
        return answer(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "裝置操作請求太頻繁，請稍後再試。");
    }
    if !state.replay.record(&caller.nonce, now) {
        return unauthorized();
    }
    let socket = duduclaw_sysd::resolve_socket_path();
    let (decision, request) = decide(&body, duduclaw_core::is_appliance(), socket.exists());
    let request = match (decision, request) {
        (Decision::Forward, Some(request)) => request,
        (Decision::BadRequest, _) => {
            return answer(StatusCode::BAD_REQUEST, "bad_request", "請求格式不正確。");
        }
        (Decision::VerbNotAllowed, _) => {
            tracing::warn!(agent = %caller.agent_id, "sysd route: verb not forwardable");
            return answer(StatusCode::FORBIDDEN, "forbidden", "這項裝置操作不能透過 MCP 工具執行。");
        }
        _ => {
            return answer(
                StatusCode::CONFLICT,
                "unavailable",
                "這台裝置不是 DuDuClaw OS 裝置，或系統服務沒有啟動。",
            );
        }
    };
    tracing::info!(agent = %caller.agent_id, verb = request.verb_name(), "sysd route: forwarding");
    match duduclaw_sysd::SysdClient::new(socket).call(&request).await {
        Ok(out) => (StatusCode::OK, Json(RouteAnswer { ok: true, data: Some(out), error: None })).into_response(),
        // sysd's own refusal travels as-is (e.g. an older sysd's unknown
        // verb), so the caller can tell it apart from a transport problem.
        Err(SysdClientError::Rejected { kind, message }) => {
            let body = RouteAnswer { ok: false, data: None, error: Some(RouteError { kind, message }) };
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => answer(StatusCode::OK, "unavailable", &e.to_string()),
    }
}

// ── client (mcp-server side) ─────────────────────────────────────────────

/// The four authentication headers for `body` (same scheme as the
/// computer-use client).
pub fn signed_headers(
    internal_key: &str,
    agent_id: &str,
    agent_token: &str,
    timestamp: u64,
    nonce: &[u8; 16],
    body: &[u8],
) -> [(&'static str, String); 4] {
    let timestamp = timestamp.to_string();
    let nonce = hex::encode(nonce);
    let signature =
        duduclaw_core::internal_request_signature(internal_key.as_bytes(), agent_id, agent_token, &timestamp, &nonce, body);
    [
        ("X-Duduclaw-Agent-Id", agent_id.to_string()),
        ("X-Duduclaw-Timestamp", timestamp),
        ("X-Duduclaw-Nonce", nonce),
        ("X-Duduclaw-Signature", signature),
    ]
}

fn refused(kind: &str, message: &str) -> SysdClientError {
    SysdClientError::Rejected { kind: kind.into(), message: message.into() }
}

/// Turn the route's HTTP answer into sysd's result shape.
pub fn interpret(status: u16, body: &[u8]) -> Result<SysdOpOutput, SysdClientError> {
    let parsed: RouteAnswer = serde_json::from_slice(body)
        .map_err(|_| SysdClientError::Decode(format!("gateway sysd route answered HTTP {status} with an unreadable body")))?;
    match (parsed.ok, parsed.data, parsed.error) {
        (true, Some(data), _) => Ok(data),
        (true, None, _) => Err(SysdClientError::MissingData),
        (false, _, Some(e)) => Err(refused(&e.kind, &e.message)),
        (false, _, None) => Err(SysdClientError::Decode(format!("gateway sysd route refused (HTTP {status})"))),
    }
}

/// Send `req` to the gateway route of `home`, signed with this process's
/// employee identity (`DUDUCLAW_AGENT_ID`, `DUDUCLAW_AGENT_TOKEN`) and the
/// gateway-internal key (`DUDUCLAW_MCP_API_KEY`). Missing credentials or an
/// unreachable gateway is an error; there is no direct sysd fallback.
pub async fn call_via_gateway(home: &Path, req: &SysdRequest) -> Result<SysdOpOutput, SysdClientError> {
    let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let (Some(key), Some(agent_id), Some(token)) =
        (env(duduclaw_core::ENV_MCP_API_KEY), env(duduclaw_core::ENV_AGENT_ID), env(duduclaw_core::ENV_AGENT_TOKEN))
    else {
        return Err(refused(
            "no_identity",
            "裝置操作工具無法使用：這個 MCP 程序沒有 gateway 內部金鑰或員工身分，只有由 DuDuClaw gateway 啟動的 AI 員工能使用。",
        ));
    };
    let body = serde_json::to_vec(&RouteBody { route: ROUTE_TAG.into(), request: req.clone() })
        .map_err(|e| SysdClientError::Decode(e.to_string()))?;
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let url = format!("http://127.0.0.1:{port}{ROUTE}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(CLIENT_TIMEOUT)
        .build()
        .map_err(|e| SysdClientError::Decode(e.to_string()))?;
    let timestamp = unix_now();
    let nonce: [u8; 16] = rand::random();
    let mut request = client.post(&url).header("Content-Type", "application/json");
    for (name, value) in signed_headers(&key, &agent_id, &token, timestamp, &nonce, &body) {
        request = request.header(name, value);
    }
    let resp = request.body(body).send().await.map_err(|e| {
        if e.is_timeout() {
            SysdClientError::Timeout(CLIENT_TIMEOUT)
        } else {
            SysdClientError::Connect { path: url.clone(), source: std::io::Error::other(e.to_string()) }
        }
    })?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| SysdClientError::Decode(e.to_string()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(SysdClientError::Decode("gateway sysd route answer too large".into()));
    }
    interpret(status, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(req: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"route": "sysd", "request": req})).unwrap()
    }

    #[test]
    fn only_the_os_tool_verbs_are_forwardable() {
        for req in [
            SysdRequest::Reboot,
            SysdRequest::Poweroff,
            SysdRequest::SysupdateStatus,
            SysdRequest::SysupdateStageApply,
            SysdRequest::BootAssessmentStatus,
            SysdRequest::UpdateRollback,
            SysdRequest::ClearExhaustedUpdateTarget { version: "0.4.0".into() },
            SysdRequest::FactoryResetV2 { clear_network: true },
        ] {
            assert!(forwardable(&req), "{req:?}");
        }
        for req in [
            SysdRequest::SysupdateApply,
            SysdRequest::SshServiceStart,
            SysdRequest::SshServiceStop,
            // The maintenance SSH verbs are gateway-only (design §8.1): an AI
            // employee must never open a login path or read its audit view.
            SysdRequest::SshMaintOpen {
                public_key: "ssh-ed25519 AAAA".into(),
                ttl_seconds: 600,
                window_id: "w".into(),
            },
            SysdRequest::SshMaintStatus,
            SysdRequest::FactoryReset,
            SysdRequest::ClearNetworkCredentials,
            SysdRequest::Hostname { set: "x".into() },
            SysdRequest::SetTimezone { timezone: "UTC".into() },
            SysdRequest::SetNtp { enabled: true },
            SysdRequest::LocalModelService { action: duduclaw_sysd::LocalModelAction::Stop },
            SysdRequest::NetworkWiredConfig {
                interface: "eth0".into(),
                mode: "dhcp".into(),
                address: None,
                gateway: None,
                dns: vec![],
            },
        ] {
            assert!(!forwardable(&req), "{req:?}");
        }
    }

    #[test]
    fn the_route_decision_fails_closed() {
        let reboot = body(serde_json::json!({"verb": "reboot"}));
        assert_eq!(decide(&reboot, true, true).0, Decision::Forward);
        assert_eq!(decide(&reboot, false, true).0, Decision::NotAppliance);
        assert_eq!(decide(&reboot, true, false).0, Decision::NotAppliance);
        let ssh = body(serde_json::json!({"verb": "ssh_service_start"}));
        assert_eq!(decide(&ssh, true, true).0, Decision::VerbNotAllowed);
        let maint = body(serde_json::json!({"verb": "ssh_maint_status"}));
        assert_eq!(decide(&maint, true, true).0, Decision::VerbNotAllowed);
        let open = body(serde_json::json!({"verb": "ssh_maint_open", "params": {"public_key": "k", "ttl_seconds": 600, "window_id": "w"}}));
        assert_eq!(decide(&open, true, true).0, Decision::VerbNotAllowed);
        // A body made for another route, or with extra fields, is refused.
        assert_eq!(decide(br#"{"op":"status"}"#, true, true).0, Decision::BadRequest);
        let other = serde_json::to_vec(&serde_json::json!({"route": "computer-use", "request": {"verb": "reboot"}})).unwrap();
        assert_eq!(decide(&other, true, true).0, Decision::BadRequest);
        let extra = serde_json::to_vec(&serde_json::json!({"route": "sysd", "request": {"verb": "reboot"}, "x": 1})).unwrap();
        assert_eq!(decide(&extra, true, true).0, Decision::BadRequest);
        assert_eq!(decide(b"not json", true, true).0, Decision::BadRequest);
    }

    #[test]
    fn answers_map_back_to_sysd_results() {
        let ok = br#"{"ok":true,"data":{"success":true,"stdout":"x","stderr":""}}"#;
        assert_eq!(interpret(200, ok).unwrap().stdout, "x");
        let old = br#"{"ok":false,"error":{"kind":"bad_request","message":"malformed request: unknown variant `factory_reset_v2`"}}"#;
        match interpret(200, old) {
            Err(SysdClientError::Rejected { kind, message }) => {
                assert_eq!(kind, "bad_request");
                assert!(message.contains("unknown variant"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(interpret(401, br#"{"ok":false,"error":{"kind":"unauthorized","message":"x"}}"#),
            Err(SysdClientError::Rejected { kind, .. }) if kind == "unauthorized"));
        assert!(matches!(interpret(502, b"<html>"), Err(SysdClientError::Decode(_))));
    }

    /// The route accepts exactly what the client signs (end-to-end through
    /// the shared authenticator), and nothing signed with another key, over
    /// another body or from a non-loopback peer.
    #[test]
    fn signatures_from_the_client_verify_on_the_route() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER};
        const KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
        let home = tempfile::tempdir().unwrap();
        let now_rfc = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[mcp_keys.\"{KEY}\"]\nclient_id = \"gateway-internal\"\nis_external = false\n\
                 created_at = \"{now_rfc}\"\nscopes = [\"admin\"]\n"
            ),
        )
        .unwrap();
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let token = duduclaw_core::mint_identity_token(&identity, "worker");
        let req = body(serde_json::json!({"verb": "boot_assessment_status"}));
        let now = unix_now();
        let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let headers_for = |k: &str, b: &[u8]| {
            let [a, t, n, sig] = signed_headers(k, "worker", &token, now, &[7u8; 16], b);
            let mut map = HeaderMap::new();
            map.insert(AGENT_ID_HEADER, a.1.parse().unwrap());
            map.insert(TIMESTAMP_HEADER, t.1.parse().unwrap());
            map.insert(NONCE_HEADER, n.1.parse().unwrap());
            map.insert(SIGNATURE_HEADER, sig.1.parse().unwrap());
            map
        };
        assert!(authenticate(home.path(), peer, &headers_for(KEY, &req), &req, now).is_ok());
        assert!(authenticate(home.path(), peer, &headers_for("wrong-key", &req), &req, now).is_err());
        let swapped = body(serde_json::json!({"verb": "update_rollback"}));
        assert!(authenticate(home.path(), peer, &headers_for(KEY, &req), &swapped, now).is_err());
        let remote: SocketAddr = "192.168.1.5:50000".parse().unwrap();
        assert!(authenticate(home.path(), remote, &headers_for(KEY, &req), &req, now).is_err());
    }
}
