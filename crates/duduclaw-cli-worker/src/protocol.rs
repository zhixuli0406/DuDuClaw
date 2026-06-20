//! Phase 6.1 — JSON-RPC protocol schema for the worker IPC.
//!
//! Wire format: HTTP/1.1 `POST /rpc` with body `{ "method": "...", "params": {...} }`
//! and response body `{ "ok": bool, "data"?: T, "error"?: { ... } }`.
//!
//! - Auth: `Authorization: Bearer <token>` header, validated with
//!   constant-time comparison.
//! - Transport: plain HTTP because the server only listens on `127.0.0.1`.
//!   TLS would be operational overhead with no security benefit on loopback.
//! - Versioning: we don't add `jsonrpc: "2.0"` framing because the worker
//!   is a private peer of the gateway. If we ever expose this externally,
//!   add a version field in the request.

use serde::{Deserialize, Serialize};

/// Endpoint paths the server exposes.
pub const RPC_PATH: &str = "/rpc";
pub const HEALTHZ_PATH: &str = "/healthz";

/// Top-level request envelope. The `method` discriminator selects the
/// concrete params variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Invoke one CLI turn against a pooled session. Returns the model's
    /// final answer as a string.
    Invoke(InvokeParams),
    /// Force-shutdown a pooled session (e.g. after the agent's auth has
    /// rotated). No-op if the session doesn't exist.
    ShutdownSession(ShutdownSessionParams),
    /// Diagnostic: return pool counters.
    Stats,
    /// Liveness ping. Always succeeds when the server is up.
    Health,
}

/// Parameters for [`Request::Invoke`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeParams {
    pub agent_id: String,
    /// Lowercase CLI kind: `"claude"`, `"codex"`, `"gemini"`. Worker
    /// rejects unknown values.
    pub cli_kind: String,
    #[serde(default)]
    pub bare_mode: bool,
    pub prompt: String,
    /// Per-call deadline in milliseconds. The server enforces it on top of
    /// any PtyPool default invoke timeout.
    pub timeout_ms: u64,
    /// Phase 3.D.2 — optional OAuth account scope. When `Some`, the
    /// worker's PtyPool keys sessions per-account so multi-account
    /// rotation gets distinct sessions. `None` keeps the legacy
    /// shared-session behaviour. Schema stays back-compat (older
    /// clients omit the field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// **Review fix (CRITICAL #2)**: per-request model override. When
    /// `Some`, the worker spawns the CLI with `--model <X>`. When
    /// `None`, the worker uses `WorkerServerConfig.default_model`
    /// (which itself may be `None` ⇒ `claude` picks its built-in
    /// default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// **Round 2 review fix (HIGH-3)**: per-request working directory.
    /// When set, the worker chdirs the spawned CLI to this path so
    /// `claude` auto-discovers per-agent `.mcp.json`,
    /// `.claude/settings.json`, and `CLAUDE.md`. The path is treated
    /// as a hint — non-existent values cause spawn to fall back to
    /// the worker's cwd rather than erroring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// **Review fix (HS14)**: per-account environment variables applied
    /// to the spawned CLI child process. The gateway's account rotator
    /// populates this with the resolved account's auth — typically
    /// `CLAUDE_CODE_OAUTH_TOKEN` and/or `CLAUDE_CONFIG_DIR` — so each
    /// pooled session is bound to a *distinct* account instead of all
    /// children silently sharing one ambient `~/.claude` OAuth (which
    /// bypassed per-account billing cooldown + budget caps).
    ///
    /// These env vars only take effect on a cache *miss* (when a fresh
    /// session is spawned). Because `account_id` is part of the pool
    /// cache key, distinct accounts already map to distinct sessions, so
    /// the env applied at spawn time stays consistent for the session's
    /// lifetime.
    ///
    /// Back-compat: older clients omit this field (empty map).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env: std::collections::HashMap<String, String>,
}

/// Parameters for [`Request::ShutdownSession`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownSessionParams {
    pub agent_id: String,
    pub cli_kind: String,
    #[serde(default)]
    pub bare_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Unified response envelope. Either `data` is present (success) or
/// `error` is present (failure); never both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl<T> Response<T> {
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(err: RpcError) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(err),
        }
    }
}

/// Wire shape for an error. `kind` is a short stable token so callers can
/// pattern-match programmatically; `message` is human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub kind: String,
    pub message: String,
}

impl RpcError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }

    pub fn unauthorized() -> Self {
        Self::new("unauthorized", "invalid or missing bearer token")
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }

    pub fn invoke_failed(message: impl Into<String>) -> Self {
        Self::new("invoke_failed", message)
    }
}

/// Convenience alias for any handler return that maps to an [`Response`].
pub type RpcResult<T> = Result<T, RpcError>;

/// Data payload for [`Request::Stats`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsResult {
    pub session_count: usize,
    pub uptime_secs: u64,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_request_round_trips_through_json() {
        let req = Request::Invoke(InvokeParams {
            agent_id: "agnes".into(),
            cli_kind: "claude".into(),
            bare_mode: false,
            prompt: "Say hi".into(),
            timeout_ms: 60_000,
            account_id: None,
            model: None,
            work_dir: None,
            env: std::collections::HashMap::new(),
        });
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        match back {
            Request::Invoke(p) => {
                assert_eq!(p.agent_id, "agnes");
                assert_eq!(p.cli_kind, "claude");
                assert_eq!(p.prompt, "Say hi");
                assert_eq!(p.timeout_ms, 60_000);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn health_request_serialises_without_params() {
        let req = Request::Health;
        let s = serde_json::to_string(&req).unwrap();
        // `Stats` and `Health` are unit variants — params is omitted on the wire.
        assert!(s.contains("\"method\":\"health\""), "got {s}");
    }

    #[test]
    fn response_ok_omits_error() {
        let r: Response<String> = Response::ok("hello".into());
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"data\":\"hello\""));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn response_err_omits_data() {
        let r = Response::<String>::err(RpcError::unauthorized());
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"kind\":\"unauthorized\""));
        assert!(!s.contains("\"data\""));
    }

    #[test]
    fn shutdown_session_request_round_trips() {
        let req = Request::ShutdownSession(ShutdownSessionParams {
            agent_id: "agnes".into(),
            cli_kind: "claude".into(),
            bare_mode: true,
            account_id: Some("alice@example.com".into()),
            model: None,
        });
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Request::ShutdownSession(_)));
    }

    #[test]
    fn invoke_env_round_trips_and_empty_is_skipped() {
        // **Review fix (HS14)**: env must round-trip, and an empty map
        // must be omitted on the wire so older clients stay compatible.
        let mut env = std::collections::HashMap::new();
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "tok-123".to_string());
        let req = Request::Invoke(InvokeParams {
            agent_id: "agnes".into(),
            cli_kind: "claude".into(),
            bare_mode: false,
            prompt: "hi".into(),
            timeout_ms: 1000,
            account_id: Some("alice@example.com".into()),
            model: None,
            work_dir: None,
            env: env.clone(),
        });
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("CLAUDE_CODE_OAUTH_TOKEN"), "got {s}");
        let back: Request = serde_json::from_str(&s).unwrap();
        match back {
            Request::Invoke(p) => assert_eq!(p.env, env),
            other => panic!("expected Invoke, got {other:?}"),
        }

        // Empty map → field omitted on the wire.
        let empty = Request::Invoke(InvokeParams {
            agent_id: "a".into(),
            cli_kind: "claude".into(),
            bare_mode: false,
            prompt: "hi".into(),
            timeout_ms: 1000,
            account_id: None,
            model: None,
            work_dir: None,
            env: std::collections::HashMap::new(),
        });
        let s2 = serde_json::to_string(&empty).unwrap();
        assert!(!s2.contains("\"env\""), "empty env must be omitted: {s2}");
        // Older client payload without `env` still deserializes.
        let legacy = r#"{"method":"invoke","params":{"agent_id":"a","cli_kind":"claude","prompt":"hi","timeout_ms":1000}}"#;
        let parsed: Request = serde_json::from_str(legacy).unwrap();
        match parsed {
            Request::Invoke(p) => assert!(p.env.is_empty()),
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn unknown_method_fails_deserialization() {
        let raw = r#"{"method":"made_up","params":{}}"#;
        let r: Result<Request, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "should not parse unknown method");
    }
}
