//! Phase 5.2 + 6.2 — axum-backed JSON-RPC server.
//!
//! Wires together:
//! - `POST /rpc` — bearer-authenticated dispatch on [`crate::protocol::Request`].
//! - `GET /healthz` — no-auth liveness ping.
//!
//! Carries an in-memory `Arc<PtyPool>` and a token string. The server can
//! be run as a long-lived task ([`WorkerServer::serve`]) or in-process for
//! integration tests ([`WorkerServer::serve_on`]).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use duduclaw_cli_runtime::{AgentKey, CliKind, PoolConfig, PtyPool, PtySession, SpawnOpts};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::auth::verify_token;
use crate::protocol::{
    HEALTHZ_PATH, InvokeParams, RPC_PATH, Request, Response, RpcError, ShutdownSessionParams,
    StatsResult,
};

/// Configuration for the worker server.
#[derive(Debug, Clone)]
pub struct WorkerServerConfig {
    /// Bind address. **MUST** be a loopback (127.0.0.1 / ::1).
    pub bind: SocketAddr,
    /// Bearer token clients must present on `/rpc`.
    pub token: String,
    /// Default per-agent pool capacity (1 for CLIs without re-entrancy).
    pub max_per_agent: usize,
    /// Default idle timeout before a session is evicted.
    pub idle_timeout: Duration,
    /// Default invoke timeout when a request doesn't supply one.
    pub default_invoke_timeout: Duration,
    /// Build identifier reported via `Stats` / `Health`.
    pub version: String,
    /// **Review fix (CRITICAL #2)**: default model used when an
    /// `Invoke` request doesn't carry a `model` (per-request override).
    /// `None` lets `claude` pick its built-in default.
    pub default_model: Option<String>,
    /// **Round 3 security fix (HIGH-H2)**: DuDuClaw home dir used to
    /// constrain `InvokeParams.work_dir` to `<home>/agents/`. When
    /// `None`, the worker rejects any non-None `work_dir` (defensive
    /// default — operators that don't supply home cannot accept
    /// arbitrary cwds via RPC).
    pub home_dir: Option<std::path::PathBuf>,
}

impl Default for WorkerServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:9876".parse().expect("static literal must parse"),
            token: String::new(),
            max_per_agent: 1,
            idle_timeout: Duration::from_secs(10 * 60),
            default_invoke_timeout: Duration::from_secs(300),
            version: env!("CARGO_PKG_VERSION").to_string(),
            default_model: None,
            home_dir: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("bind {bind} is not a loopback address — server refuses to start")]
    NonLoopbackBind { bind: SocketAddr },
    #[error("server token is empty — refusing to start without auth")]
    EmptyToken,
    #[error("tokio listener bind: {0}")]
    Bind(#[from] std::io::Error),
}

/// Server-side state owned by the axum router.
struct AppState {
    pool: Arc<PtyPool>,
    token: String,
    started_at: Instant,
    version: String,
    /// Optional home dir for path-traversal validation. See
    /// `validate_work_dir`.
    home_dir: Option<std::path::PathBuf>,
}

// **Round 2 review fix (HIGH-3)**: per-invoke work_dir hint scoped via
// tokio task-local. Lets `handle_invoke` pass the caller's requested cwd
// down to the `PtyPool` spawn factory without polluting the `AgentKey`
// (cache keys should be identity, not invocation context).
tokio::task_local! {
    static SPAWN_WORK_DIR_HINT: Option<std::path::PathBuf>;
}

/// Long-lived worker server.
pub struct WorkerServer {
    pool: Arc<PtyPool>,
    config: WorkerServerConfig,
}

/// Returned by [`WorkerServer::serve_on`]; holds the listener address +
/// shutdown trigger so tests can stop the server cleanly.
pub struct ServerHandle {
    pub local_addr: SocketAddr,
    pub shutdown_tx: oneshot::Sender<()>,
    pub join: tokio::task::JoinHandle<()>,
}

impl WorkerServer {
    /// Build a server. The factory used by the embedded [`PtyPool`] is a
    /// generic Claude-only resolver: it expects the `claude` binary on
    /// PATH and spawns interactive sessions with the standard sentinel
    /// protocol.
    pub fn new(config: WorkerServerConfig) -> Result<Self, ServerError> {
        if !is_loopback(config.bind) {
            return Err(ServerError::NonLoopbackBind { bind: config.bind });
        }
        if config.token.is_empty() {
            return Err(ServerError::EmptyToken);
        }
        let pool = build_default_pool(&config);
        Ok(Self { pool, config })
    }

    /// Inject a custom [`PtyPool`] (for tests + cases where the factory
    /// must use a different spawn strategy, e.g. echo-server in
    /// integration tests).
    pub fn with_pool(config: WorkerServerConfig, pool: Arc<PtyPool>) -> Result<Self, ServerError> {
        if !is_loopback(config.bind) {
            return Err(ServerError::NonLoopbackBind { bind: config.bind });
        }
        if config.token.is_empty() {
            return Err(ServerError::EmptyToken);
        }
        Ok(Self { pool, config })
    }

    /// Bind + serve until `shutdown_signal` resolves.
    pub async fn serve(
        self,
        shutdown_signal: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), ServerError> {
        let listener = TcpListener::bind(self.config.bind).await?;
        let addr = listener.local_addr()?;
        info!(bind = %addr, "worker: serving");
        let state = Arc::new(AppState {
            pool: self.pool.clone(),
            token: self.config.token.clone(),
            started_at: Instant::now(),
            version: self.config.version.clone(),
            home_dir: self.config.home_dir.clone(),
        });
        let app = router(state);
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal)
            .await
            .map_err(ServerError::Bind)?;
        // Drain pool on shutdown.
        self.pool.shutdown().await;
        info!("worker: pool drained, exiting");
        Ok(())
    }

    /// Spawn the server in the background on the configured bind. Useful
    /// for tests that want to issue requests synchronously without
    /// driving the main async runtime.
    pub async fn serve_on(self) -> Result<ServerHandle, ServerError> {
        let listener = TcpListener::bind(self.config.bind).await?;
        let local_addr = listener.local_addr()?;
        info!(bind = %local_addr, "worker (background): serving");
        let state = Arc::new(AppState {
            pool: self.pool.clone(),
            token: self.config.token.clone(),
            started_at: Instant::now(),
            version: self.config.version.clone(),
            home_dir: self.config.home_dir.clone(),
        });
        let app = router(state);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let pool = self.pool.clone();
        let join = tokio::spawn(async move {
            let serve_result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(e) = serve_result {
                warn!(error = %e, "worker: serve loop exited with error");
            }
            pool.shutdown().await;
        });
        Ok(ServerHandle {
            local_addr,
            shutdown_tx,
            join,
        })
    }
}

fn is_loopback(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// **Round 3 security helper (HIGH-H1)**: allowlist a model name.
///
/// Model names are interpolated directly into the spawned CLI's
/// arg list as `--model <value>`. portable-pty's `CommandBuilder`
/// uses `execvp` / `CreateProcessW` (not shell), so traditional
/// shell-injection isn't possible, but an attacker could still
/// inject a SEPARATE CLI flag by supplying `--append-system-prompt`
/// or similar as the model. Allow only the printable subset that
/// real model identifiers use: alphanumeric, `-`, `.`, `_`, up to
/// 128 chars, not starting with `-`.
pub(crate) fn validate_model_name(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 128
        && !m.starts_with('-')
        && m
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// **Round 3 security helper (HIGH-H2)**: validate that `work_dir`
/// canonicalises inside `<home>/agents/`. Returns `false` for
/// non-existent paths, paths outside the constrained root, or
/// when `home` is `None` (caller should reject the request).
///
/// Kept as a thin bool-returning wrapper around
/// [`canonicalize_work_dir`] so existing Round-3 tests stay
/// readable; production code calls `canonicalize_work_dir` directly
/// (Round 4 HIGH-1) to also obtain the canonical PathBuf.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_work_dir(work_dir: &str, home: Option<&std::path::Path>) -> bool {
    canonicalize_work_dir(work_dir, home).is_some()
}

/// **Round 4 security fix (HIGH-1)**: return the *canonicalised*
/// PathBuf so the caller passes the post-canonicalize path to the
/// spawn factory. The previous code accepted `params.work_dir` as a
/// raw string, validated it through `canonicalize`, then handed the
/// raw (pre-canonicalize) path to `PtySession::spawn`. Between those
/// two reads an attacker who could rename `<home>/agents/<id>` could
/// substitute a symlink to an arbitrary directory — a TOCTOU window.
/// Returning the canonical PathBuf eliminates the second filesystem
/// resolution.
pub(crate) fn canonicalize_work_dir(
    work_dir: &str,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let home = home?;
    let candidate = std::path::Path::new(work_dir).canonicalize().ok()?;
    let root = home.join("agents").canonicalize().ok()?;
    if candidate.starts_with(&root) {
        Some(candidate)
    } else {
        None
    }
}

fn build_default_pool(config: &WorkerServerConfig) -> Arc<PtyPool> {
    let pool_config = PoolConfig {
        max_per_agent: config.max_per_agent,
        idle_timeout: config.idle_timeout,
        default_invoke_timeout: config.default_invoke_timeout,
        ..PoolConfig::default()
    };

    // Capture the worker's default_model so the factory can fall back
    // when a key doesn't carry an explicit model.
    let fallback_model = config.default_model.clone();
    let factory: duduclaw_cli_runtime::pool::SpawnFactory = Arc::new(move |key: AgentKey| {
        let fallback = fallback_model.clone();
        Box::pin(async move { spawn_session_default(key, fallback).await })
    });
    PtyPool::new(factory, pool_config)
}

async fn spawn_session_default(
    key: AgentKey,
    fallback_model: Option<String>,
) -> Result<Arc<PtySession>, duduclaw_cli_runtime::SessionError> {
    let program = match key.cli_kind {
        CliKind::Claude => duduclaw_core::which_claude()
            .ok_or_else(|| {
                duduclaw_cli_runtime::SessionError::UnknownCliKind(
                    "claude binary not found on PATH".to_string(),
                )
            })?,
        _ => {
            return Err(duduclaw_cli_runtime::SessionError::UnknownCliKind(
                format!("unsupported cli_kind {:?}", key.cli_kind),
            ));
        }
    };

    let mut opts = SpawnOpts::claude_interactive(key.agent_id.clone(), &program);
    // **Round 2 review note (HIGH-2 — DEFERRED)**: `key.account_id`
    // affects only the pool's cache_key here — the spawned CLI uses
    // whatever ambient OAuth lives in `~/.claude/` / keychain. True
    // per-account auth isolation in the worker requires accepting
    // env_vars in `InvokeParams` and applying them to `opts.env`
    // (the gateway has those env_vars from its rotator; passing them
    // across the JSON-RPC boundary is a future schema change).
    // Operators of multi-OAuth-account setups should not rely on the
    // managed worker for account rotation until this lands.
    let _account_id_for_future_env_injection = &key.account_id;
    // **Review fix (CRITICAL #2)**: model precedence is
    // (1) key.model (per-Invoke override) →
    // (2) WorkerServerConfig.default_model (worker-wide default) →
    // (3) no `--model` arg (claude picks its built-in default).
    let chosen_model = key.model.clone().or(fallback_model);
    let mut args: Vec<String> = Vec::new();
    if let Some(m) = chosen_model {
        args.push("--model".to_string());
        args.push(m);
    }
    if key.bare_mode {
        args.push("--bare".to_string());
    }
    opts.extra_args = args;

    // **Round 2 review fix (HIGH-3)**: pick up the per-invoke
    // work_dir hint that the handler scoped via task-local. Existing
    // path → cwd; non-existent / unset → leave as default (worker's
    // own cwd) so spawn doesn't fail.
    let hint = SPAWN_WORK_DIR_HINT
        .try_with(|opt| opt.clone())
        .ok()
        .flatten();
    if let Some(path) = hint {
        if path.exists() {
            opts.cwd = Some(path);
        } else {
            tracing::warn!(
                requested = %path.display(),
                "worker: spawn work_dir hint does not exist — falling back to worker cwd"
            );
        }
    }
    PtySession::spawn(opts).await
}

fn router(state: Arc<AppState>) -> Router {
    // **Round 4 security fix (MED-4)**: bound request body size.
    // Worker prompts realistically never exceed a few KB; axum's
    // 2 MB default lets a caller force the worker to allocate and
    // parse multi-MB JSON bodies repeatedly. 512 KB is comfortably
    // larger than any production prompt seen in DuDuClaw traces.
    const MAX_RPC_BODY_BYTES: usize = 512 * 1024;
    Router::new()
        .route(RPC_PATH, post(rpc_handler))
        .route(HEALTHZ_PATH, get(healthz_handler))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_RPC_BODY_BYTES))
        .with_state(state)
}

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn rpc_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<Request>,
) -> impl IntoResponse {
    if let Err(resp) = authorise(&state, &headers) {
        return resp;
    }

    let result = match req {
        Request::Health => Ok(serde_json::json!({"alive": true})),
        Request::Stats => handle_stats(&state).map(|s| serde_json::to_value(s).unwrap()),
        Request::Invoke(params) => handle_invoke(&state, params).await,
        Request::ShutdownSession(params) => handle_shutdown_session(&state, params).await,
    };

    match result {
        Ok(data) => (StatusCode::OK, Json(Response::ok(data))).into_response(),
        Err(err) => {
            let status = status_for(&err);
            (status, Json(Response::<Value>::err(err))).into_response()
        }
    }
}

fn authorise(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match supplied {
        Some(token) if verify_token(&state.token, token) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(Response::<Value>::err(RpcError::unauthorized())),
        )
            .into_response()),
    }
}

fn status_for(err: &RpcError) -> StatusCode {
    match err.kind.as_str() {
        "unauthorized" => StatusCode::UNAUTHORIZED,
        "bad_request" => StatusCode::BAD_REQUEST,
        "invoke_failed" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn handle_stats(state: &AppState) -> Result<StatsResult, RpcError> {
    Ok(StatsResult {
        session_count: state.pool.session_count(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        version: state.version.clone(),
    })
}

/// Round 4 deferred-cleanup: emit at most one warning per unique
/// `(requested, actual)` pair to keep log volume bounded when the
/// same divergent pair re-fires for every invoke on a cached session.
fn warn_work_dir_divergence(requested: &std::path::Path, actual: &std::path::Path) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<(std::path::PathBuf, std::path::PathBuf)>>> =
        OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let already = {
        let mut guard = seen.lock().unwrap_or_else(|p| p.into_inner());
        !guard.insert((requested.to_path_buf(), actual.to_path_buf()))
    };
    if !already {
        warn!(
            requested = %requested.display(),
            actual = %actual.display(),
            "worker: cached session was spawned in a different cwd than the current invoke's work_dir hint — the session will continue using its original cwd. \
This warning is one-shot per unique pair."
        );
    }
}

async fn handle_invoke(
    state: &AppState,
    params: InvokeParams,
) -> Result<Value, RpcError> {
    let cli_kind = CliKind::parse(&params.cli_kind)
        .map_err(|_| RpcError::bad_request(format!("unknown cli_kind: {}", params.cli_kind)))?;
    if params.prompt.is_empty() {
        return Err(RpcError::bad_request("prompt must not be empty"));
    }
    // **Round 4 security fix (HIGH-4)**: bound `agent_id` and
    // `account_id` lengths. Each unique value creates a new entry in
    // `PtyPool::semaphores` (DashMap with no eviction); without a cap
    // a caller with a valid bearer token could exhaust gateway memory
    // by sending many distinct ids in a tight loop.
    const MAX_AGENT_ID_LEN: usize = 128;
    const MAX_ACCOUNT_ID_LEN: usize = 256;
    if params.agent_id.len() > MAX_AGENT_ID_LEN {
        return Err(RpcError::bad_request(format!(
            "agent_id exceeds {MAX_AGENT_ID_LEN} bytes"
        )));
    }
    if let Some(acc) = params.account_id.as_deref() {
        if acc.len() > MAX_ACCOUNT_ID_LEN {
            return Err(RpcError::bad_request(format!(
                "account_id exceeds {MAX_ACCOUNT_ID_LEN} bytes"
            )));
        }
    }
    // **Round 3 security fix (HIGH-H1)**: `model` is interpolated
    // directly into the CLI arg list as `--model <value>`. Reject any
    // value that could escape to a separate flag (leading `-`, control
    // chars, NUL). Allowlist: alphanumeric + `-` `.` `_`, ≤ 128 chars.
    if let Some(m) = &params.model {
        if !validate_model_name(m) {
            return Err(RpcError::bad_request(format!(
                "invalid model name (must match [A-Za-z0-9._-]{{1,128}} and not start with `-`): {m:?}"
            )));
        }
    }
    // **Round 3 security fix (HIGH-H2) + Round 4 (HIGH-1)**:
    // canonicalise `work_dir` once. The resulting PathBuf is what
    // gets passed to the spawn factory, eliminating the TOCTOU
    // window where an attacker could swap `<home>/agents/<id>` for
    // a symlink between validation and the actual spawn.
    let canonical_work_dir: Option<std::path::PathBuf> = match &params.work_dir {
        Some(wd) => match state.home_dir.as_deref() {
            None => {
                return Err(RpcError::bad_request(
                    "work_dir requires the worker to be configured with --home-dir",
                ));
            }
            Some(home) => match canonicalize_work_dir(wd, Some(home)) {
                Some(p) => Some(p),
                None => {
                    return Err(RpcError::bad_request(format!(
                        "work_dir must canonicalize inside <home>/agents/: {wd}"
                    )));
                }
            },
        },
        None => None,
    };

    // **Round 4 security fix (HIGH-2)**: cap the per-invoke timeout
    // so a caller can't pin an agent's PTY slot forever by passing
    // `timeout_ms = u64::MAX`. 10 minutes is comfortably longer than
    // any legitimate single-prompt completion; the default invoke
    // timeout in `PoolConfig` is 5 minutes.
    const MAX_INVOKE_TIMEOUT_MS: u64 = 10 * 60 * 1000;
    let timeout = Duration::from_millis(
        params.timeout_ms.max(1).min(MAX_INVOKE_TIMEOUT_MS),
    );

    let key = AgentKey::with_account_and_model(
        &params.agent_id,
        cli_kind,
        params.bare_mode,
        params.account_id.clone(),
        params.model.clone(),
    );
    // **Round 2 review fix (HIGH-3)** + **Round 4 (HIGH-1)**: scope
    // the *canonical* PathBuf into the task-local so the spawn
    // factory doesn't re-resolve the path (TOCTOU-free).
    let canonical_work_dir_for_check = canonical_work_dir.clone();
    let lease = SPAWN_WORK_DIR_HINT
        .scope(canonical_work_dir, state.pool.acquire(key))
        .await
        .map_err(|e| RpcError::invoke_failed(format!("pool acquire: {e}")))?;
    let session = lease.arc();
    // Round 4 deferred-cleanup: warn (once-per-divergent-pair) when
    // the cached session's cwd differs from the caller's
    // canonicalised work_dir. The spawn factory only consults the
    // task-local hint on a cache miss; on a cache hit, the cached
    // session keeps whatever cwd it was originally spawned with —
    // which produces silent divergence if the same agent_id later
    // submits an invoke with a different work_dir (an unusual but
    // operationally valid setup, e.g. an agent whose root moved
    // mid-process). Logging it makes the divergence diagnosable
    // without changing the cache semantics.
    if let (Some(requested), Some(actual)) =
        (canonical_work_dir_for_check.as_ref(), session.spawn_cwd())
    {
        if requested.as_path() != actual {
            warn_work_dir_divergence(requested, actual);
        }
    }
    let result = session.invoke(&params.prompt, Some(timeout)).await;
    match result {
        Ok(text) => {
            if text.trim().is_empty() {
                session.mark_unhealthy();
                Err(RpcError::invoke_failed(
                    "empty payload (session marked unhealthy)",
                ))
            } else {
                Ok(serde_json::json!({"text": text}))
            }
        }
        Err(e) => {
            // Hard failures: invalidate so next acquire spawns fresh.
            if matches!(
                e,
                duduclaw_cli_runtime::SessionError::ChildExited { .. }
                    | duduclaw_cli_runtime::SessionError::MalformedResponse
                    | duduclaw_cli_runtime::SessionError::CliError(_)
            ) {
                lease.invalidate();
            }
            Err(RpcError::invoke_failed(e.to_string()))
        }
    }
}

async fn handle_shutdown_session(
    state: &AppState,
    params: ShutdownSessionParams,
) -> Result<Value, RpcError> {
    let cli_kind = CliKind::parse(&params.cli_kind)
        .map_err(|_| RpcError::bad_request(format!("unknown cli_kind: {}", params.cli_kind)))?;
    let key = AgentKey::with_account_and_model(
        &params.agent_id,
        cli_kind,
        params.bare_mode,
        params.account_id.clone(),
        params.model.clone(),
    );
    // **Round 2 review fix (HIGH-1)**: use the atomic
    // `remove_if_present` API to close the TOCTOU race between an
    // existence check and an acquire. The previous "contains_key
    // then acquire" sequence could spawn a fresh session in the
    // window between the two when the background eviction tick
    // ran concurrently. `remove_if_present` is a single DashMap
    // remove + cooperative shutdown — no spawn.
    let shutdown = state.pool.remove_if_present(&key).await;
    if shutdown {
        debug!(
            agent_id = %params.agent_id,
            "worker: shutdown_session evicted cached session"
        );
    }
    Ok(serde_json::json!({"shutdown": shutdown}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn is_loopback_accepts_ipv4_127() {
        let addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
        assert!(is_loopback(addr));
    }

    #[test]
    fn is_loopback_accepts_ipv6_one() {
        let addr: SocketAddr = "[::1]:0".parse().unwrap();
        assert!(is_loopback(addr));
    }

    #[test]
    fn is_loopback_rejects_zero_zero_zero_zero() {
        let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        assert!(!is_loopback(addr));
    }

    #[test]
    fn is_loopback_rejects_public_ip() {
        let addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
        assert!(!is_loopback(addr));
    }

    #[test]
    fn new_rejects_non_loopback() {
        let cfg = WorkerServerConfig {
            bind: "0.0.0.0:9876".parse().unwrap(),
            token: "secret".into(),
            ..WorkerServerConfig::default()
        };
        let err = WorkerServer::new(cfg).err().expect("should reject");
        assert!(matches!(err, ServerError::NonLoopbackBind { .. }));
    }

    #[test]
    fn new_rejects_empty_token() {
        let cfg = WorkerServerConfig {
            bind: "127.0.0.1:9876".parse().unwrap(),
            token: String::new(),
            ..WorkerServerConfig::default()
        };
        let err = WorkerServer::new(cfg).err().expect("should reject");
        assert!(matches!(err, ServerError::EmptyToken));
    }

    #[test]
    fn status_mapping_unauthorized() {
        let s = status_for(&RpcError::unauthorized());
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn status_mapping_bad_request() {
        let s = status_for(&RpcError::bad_request("x"));
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn status_mapping_invoke_failed() {
        let s = status_for(&RpcError::invoke_failed("x"));
        assert_eq!(s, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn status_mapping_internal_default() {
        let s = status_for(&RpcError::internal("x"));
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // **Round 3 security tests (HIGH-H1)** — model name allowlist.

    #[test]
    fn validate_model_accepts_realistic_names() {
        for m in [
            "claude-haiku-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-3-7-sonnet",
            "gpt-4o",
            "claude.haiku",
            "model_with_underscores",
            "abc123",
        ] {
            assert!(validate_model_name(m), "expected accept: {m:?}");
        }
    }

    #[test]
    fn validate_model_rejects_injection_attempts() {
        for m in [
            "",                          // empty
            "--append-system-prompt",   // leading dash → injects as flag
            "-x",
            "model with spaces",        // shell-like separator
            "model;rm -rf",
            "model$(whoami)",
            "model\nnewline",
            "model\x00null",
            "model\"quoted\"",
            "model'sq'",
            "model/slash",
            "model\\backslash",
        ] {
            assert!(!validate_model_name(m), "expected reject: {m:?}");
        }
    }

    #[test]
    fn validate_model_rejects_oversize() {
        let big = "a".repeat(129);
        assert!(!validate_model_name(&big));
        let max = "a".repeat(128);
        assert!(validate_model_name(&max));
    }

    // **Round 3 security tests (HIGH-H2)** — work_dir traversal.

    #[test]
    fn validate_work_dir_requires_home() {
        // No home configured → all paths rejected.
        assert!(!validate_work_dir("/tmp", None));
        assert!(!validate_work_dir("/some/path", None));
    }

    #[test]
    fn validate_work_dir_accepts_path_inside_agents() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let agent_dir = agents.join("agnes");
        fs::create_dir_all(&agent_dir).unwrap();
        let agent_dir_str = agent_dir.to_string_lossy().to_string();
        assert!(validate_work_dir(&agent_dir_str, Some(tmp.path())));
    }

    #[test]
    fn validate_work_dir_rejects_path_outside_agents() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("agents")).unwrap();
        // /etc exists but is NOT inside <home>/agents/.
        assert!(!validate_work_dir("/etc", Some(tmp.path())));
        // A sibling of agents/ (e.g. <home>/secrets) also rejected.
        let secrets = tmp.path().join("secrets");
        fs::create_dir(&secrets).unwrap();
        assert!(!validate_work_dir(
            &secrets.to_string_lossy(),
            Some(tmp.path())
        ));
    }

    #[test]
    fn validate_work_dir_rejects_nonexistent_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents")).unwrap();
        assert!(!validate_work_dir(
            "/path/that/does/not/exist/anywhere",
            Some(tmp.path())
        ));
    }

    #[test]
    fn validate_work_dir_rejects_traversal_via_dotdot() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        fs::create_dir_all(&agents).unwrap();
        let secret = tmp.path().join("secret");
        fs::create_dir(&secret).unwrap();
        // canonicalize resolves `..` so this becomes <home>/secret which
        // is OUTSIDE <home>/agents/.
        let traversal = agents.join("..").join("secret");
        assert!(!validate_work_dir(
            &traversal.to_string_lossy(),
            Some(tmp.path())
        ));
    }

    // **Round 4 security tests (HIGH-1)** — canonicalize_work_dir
    // must return the canonicalised PathBuf, not the raw input.

    #[test]
    fn canonicalize_work_dir_returns_canonical_pathbuf() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let agent_dir = agents.join("a1");
        fs::create_dir_all(&agent_dir).unwrap();
        let raw = agents.join("a1");
        let got = canonicalize_work_dir(&raw.to_string_lossy(), Some(tmp.path()))
            .expect("must accept legit agent dir");
        assert_eq!(got, agent_dir.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_work_dir_resolves_dot_segments() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let agent_dir = agents.join("b2");
        fs::create_dir_all(&agent_dir).unwrap();
        // Caller passes a path with `.` segments — canonicalize_work_dir
        // must collapse them and return the underlying canonical path
        // so spawn doesn't re-walk a different filesystem state.
        let messy = agents.join(".").join("b2").join(".");
        let canon = canonicalize_work_dir(&messy.to_string_lossy(), Some(tmp.path()))
            .expect("must accept");
        assert_eq!(canon, agent_dir.canonicalize().unwrap());
        assert!(!canon.to_string_lossy().contains("/./"));
    }

    #[test]
    fn canonicalize_work_dir_returns_none_outside_root() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("agents")).unwrap();
        let outside = tmp.path().join("not-agents");
        fs::create_dir(&outside).unwrap();
        assert!(canonicalize_work_dir(&outside.to_string_lossy(), Some(tmp.path())).is_none());
    }
}
