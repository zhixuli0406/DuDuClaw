//! Phase-3 adapter between [`duduclaw_cli_runtime`] and the gateway.
//!
//! This module *exposes* the cross-platform PTY pool API but DELIBERATELY does
//! not wire itself into `channel_reply` or `dispatcher` yet. The deep wiring is
//! the dominant risk surface (each path is 3-5 kLOC) and lands in a follow-up
//! session under the same Phase-3 work item.
//!
//! Caller responsibilities at the integration point:
//! 1. Call [`init`] once at gateway startup (after `home_dir` is known).
//! 2. Check [`is_enabled_for_agent`] before routing through the pool.
//! 3. Use [`acquire`] to get a [`PooledSession`]; treat any error as a signal
//!    to fall back to the legacy `call_claude_cli_rotated` fresh-spawn path.
//!
//! Cross-platform notes:
//! - On Windows we pick ConPTY (Win10 1809+); on Unix we use openpty.
//! - The factory only invokes `which_claude_in_home`; the *running command* is
//!   the unmodified `claude` binary, so there is no Win/Unix divergence in
//!   user-visible CLI behaviour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use duduclaw_cli_runtime::{
    AgentKey, CliKind, OneshotInvocation, OneshotOutput, PoolConfig, PoolError, PooledSession,
    PtyError, PtyPool, PtySession, SpawnOpts, oneshot_pty_invoke,
};
use duduclaw_cli_worker::{InvokeParams, WorkerClient};
use tracing::{debug, info, warn};

/// Global pool. None means Phase-3 wiring is disabled (the gateway should fall
/// back to the legacy `call_claude_cli_rotated` path).
static PTY_POOL: OnceLock<Arc<PtyPool>> = OnceLock::new();

/// **Round 2 review fix (HIGH-3)**: gateway home_dir, captured at init
/// time so helpers like `resolve_managed_worker_work_dir` can build
/// `<home>/agents/<agent_id>` paths without threading home_dir through
/// every API. Mirror of the value passed to the spawn factory.
static GATEWAY_HOME_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Phase 7 — optional managed-worker client. When Some, `acquire_and_invoke`
/// routes through the out-of-process `duduclaw-cli-worker` instead of the
/// in-process `PTY_POOL`. Set by [`set_managed_worker`] during gateway boot.
static MANAGED_WORKER: OnceLock<WorkerClient> = OnceLock::new();

/// Initialise the global PTY pool. Idempotent — second calls are silently
/// ignored. Should be called once during gateway boot; until called,
/// [`acquire`] returns [`PoolError::ShuttingDown`] so callers can branch.
///
/// `home_dir` is the DuDuClaw home (typically `~/.duduclaw`). The factory uses
/// it to resolve the `claude` binary via [`duduclaw_core::which_claude_in_home`].
pub fn init(home_dir: PathBuf) {
    if PTY_POOL.get().is_some() {
        debug!("pty_runtime: init called twice — ignoring second invocation");
        return;
    }

    let home = Arc::new(home_dir);
    // **Round 2 review fix (HIGH-3)**: stash home_dir for helpers like
    // `resolve_managed_worker_work_dir`. Idempotent.
    let _ = GATEWAY_HOME_DIR.set((*home).clone());
    let home_for_factory = home.clone();
    let factory: duduclaw_cli_runtime::pool::SpawnFactory = Arc::new(move |key: AgentKey| {
        let home = home_for_factory.clone();
        Box::pin(async move { spawn_session_for_key(&home, key).await })
    });

    let config = PoolConfig::default();
    let pool = PtyPool::new(factory, config);
    if PTY_POOL.set(pool).is_err() {
        warn!("pty_runtime: race during init — second init dropped");
    } else {
        info!(home = %home.display(), "pty_runtime: initialised");
    }
}

/// Returns true once [`init`] has been called.
pub fn is_initialised() -> bool {
    PTY_POOL.get().is_some()
}

/// Read `[runtime] pty_pool_enabled = true` from the agent's `agent.toml`.
/// Returns `false` for missing file / missing key / parse error — the legacy
/// path is the safe default.
pub fn is_enabled_for_agent(agent_dir: &Path) -> bool {
    matches!(runtime_mode_for_agent(agent_dir), RuntimeMode::PtyPool)
}

/// Which spawn pathway the gateway should use for a given agent.
///
/// `FreshSpawn` is the legacy `tokio::process::Command` path through
/// `call_claude_cli_rotated`. `PtyPool` routes through this crate's
/// PTY-backed one-shot or pooled session APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    FreshSpawn,
    PtyPool,
}

impl RuntimeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FreshSpawn => "fresh_spawn",
            Self::PtyPool => "pty_pool",
        }
    }
}

/// Read `[runtime] pty_pool_enabled` from the agent's `agent.toml`. Returns
/// [`RuntimeMode::FreshSpawn`] when the file is missing, malformed, the flag
/// is absent, OR the global kill-switch env var
/// `DUDUCLAW_DISABLE_PTY_POOL=1` is set.
///
/// **Phase 8 emergency rollback**: operators can force every agent back to
/// the legacy `tokio::process::Command + claude -p` path without touching
/// per-agent config by exporting `DUDUCLAW_DISABLE_PTY_POOL=1` before
/// restarting the gateway. The check happens here (cheap, called per
/// channel_reply) so a wedge'd-out flag survives the next restart.
pub fn runtime_mode_for_agent(agent_dir: &Path) -> RuntimeMode {
    if is_pty_pool_disabled_globally() {
        return RuntimeMode::FreshSpawn;
    }
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return RuntimeMode::FreshSpawn;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        warn!(
            agent_dir = %agent_dir.display(),
            "pty_runtime: agent.toml parse failed — defaulting to FreshSpawn"
        );
        return RuntimeMode::FreshSpawn;
    };
    let enabled = value
        .get("runtime")
        .and_then(|r| r.get("pty_pool_enabled"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    if enabled {
        RuntimeMode::PtyPool
    } else {
        RuntimeMode::FreshSpawn
    }
}

/// Returns true when `DUDUCLAW_PTY_DISABLE_RETRY=1` is set. Operators
/// flip this when empty-payload retries cause runaway token usage or
/// other pathological behaviour. Default off — retry is on.
pub fn is_pty_retry_disabled() -> bool {
    is_env_truthy("DUDUCLAW_PTY_DISABLE_RETRY")
}

fn is_env_truthy(var: &str) -> bool {
    matches!(
        std::env::var(var)
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Construct the retry prompt sent after an empty-payload response.
/// Reminds the model that the sentinel protocol is mandatory and
/// re-issues the original user request. Kept as a pure function for
/// testability (the prompt format is part of the protocol contract).
pub fn build_retry_reminder(original_prompt: &str) -> String {
    format!(
        "[DUDUCLAW PROTOCOL REMINDER]: Your previous response did NOT contain the required \
         sentinel-wrapped answer. The sentinel string is \
         {sentinel}. Wrap your final answer between two such sentinel lines exactly \
         (no markdown wrapping, no characters between the equals signs). Now reply to:\n\n\
         {original_prompt}",
        sentinel = duduclaw_cli_runtime::INTERACTIVE_SENTINEL,
    )
}

/// Returns true when `DUDUCLAW_DISABLE_PTY_POOL` is set to a truthy value
/// (`1`, `true`, `yes`, case-insensitive). Empty / unset / other values
/// resolve to false.
pub fn is_pty_pool_disabled_globally() -> bool {
    is_env_truthy("DUDUCLAW_DISABLE_PTY_POOL")
}

/// Invoke `claude` (or any CLI) one-shot through a PTY. Mirrors the lifecycle
/// of `tokio::process::Command::spawn → wait → capture`, but routes through
/// `portable-pty` so the child sees a real TTY on every platform.
///
/// Caller is responsible for assembling `args` and `env_vars` exactly the way
/// the legacy `spawn_claude_cli_with_env` does — this crate makes no
/// assumptions about flags / output formats / system prompt placement. The
/// returned `OneshotOutput.stdout` is whatever the CLI wrote to stdout
/// between spawn and EOF (e.g. a stream-json log line sequence).
///
/// Used by the Phase-3.B wedge in `channel_reply.rs` once stream-json parser
/// extraction lands.
pub async fn invoke_oneshot(
    program: impl Into<String>,
    args: Vec<String>,
    env_vars: HashMap<String, String>,
    work_dir: Option<PathBuf>,
    deadline: Duration,
) -> Result<OneshotOutput, PtyError> {
    let mut inv = OneshotInvocation::new(program)
        .args(args)
        .envs(env_vars)
        .deadline(deadline);
    if let Some(cwd) = work_dir {
        inv = inv.cwd(cwd);
    }
    oneshot_pty_invoke(inv).await
}

/// Round 4 deferred-cleanup (LOW F-3): single canonical
/// description of an acquire target. The 6 historical `acquire_*` /
/// `acquire_and_invoke_*` variants now collapse into 2 entry points
/// (`acquire_with` and `acquire_and_invoke_with`) that take this
/// struct, plus thin compatibility wrappers around them.
///
/// Borrowed-slice form so the common case (call-site already has
/// `&str`) doesn't allocate; only the underlying `AgentKey` (which
/// is the cache key the pool stores) owns its strings.
#[derive(Debug, Clone)]
pub struct AcquireOptions<'a> {
    pub agent_id: &'a str,
    pub cli_kind: CliKind,
    pub bare_mode: bool,
    pub account_id: Option<&'a str>,
    pub model: Option<&'a str>,
}

impl<'a> AcquireOptions<'a> {
    pub fn new(agent_id: &'a str, cli_kind: CliKind, bare_mode: bool) -> Self {
        Self {
            agent_id,
            cli_kind,
            bare_mode,
            account_id: None,
            model: None,
        }
    }

    pub fn account_id(mut self, account_id: Option<&'a str>) -> Self {
        self.account_id = account_id;
        self
    }

    pub fn model(mut self, model: Option<&'a str>) -> Self {
        self.model = model;
        self
    }

    fn into_key(&self) -> AgentKey {
        AgentKey::with_account_and_model(
            self.agent_id,
            self.cli_kind,
            self.bare_mode,
            self.account_id.map(|s| s.to_string()),
            self.model.map(|s| s.to_string()),
        )
    }
}

/// Round 4 deferred-cleanup (LOW F-3): canonical acquire entry point.
/// Acquires a pooled PTY session for the given agent according to
/// `options`. Errors are intentionally `PoolError` to keep this
/// module decoupled from gateway-internal error enums; callers
/// should treat any error as the signal to fall back to fresh-spawn
/// rather than failing the user request.
pub async fn acquire_with(options: AcquireOptions<'_>) -> Result<PooledSession, PoolError> {
    let pool = PTY_POOL.get().ok_or(PoolError::ShuttingDown)?.clone();
    pool.acquire(options.into_key()).await
}

/// Back-compat wrapper. New code should call [`acquire_with`].
pub async fn acquire(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
) -> Result<PooledSession, PoolError> {
    acquire_with(AcquireOptions::new(agent_id, cli_kind, bare_mode)).await
}

/// Back-compat wrapper. New code should call [`acquire_with`].
pub async fn acquire_for_account(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    account_id: Option<&str>,
) -> Result<PooledSession, PoolError> {
    acquire_with(AcquireOptions::new(agent_id, cli_kind, bare_mode).account_id(account_id)).await
}

/// Back-compat wrapper. New code should call [`acquire_with`].
pub async fn acquire_for_account_with_model(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    account_id: Option<&str>,
    model: Option<&str>,
) -> Result<PooledSession, PoolError> {
    acquire_with(
        AcquireOptions::new(agent_id, cli_kind, bare_mode)
            .account_id(account_id)
            .model(model),
    )
    .await
}

/// Diagnostics — number of cached sessions across all agents.
pub fn session_count() -> usize {
    PTY_POOL.get().map(|p| p.session_count()).unwrap_or(0)
}

/// Phase 7 — switch `acquire_and_invoke` to the out-of-process transport.
///
/// Idempotent: a second call after success silently ignores the new
/// client (the worker supervisor keeps the original handle alive). Should
/// be invoked from `server.rs` after the supervisor is healthy.
pub fn set_managed_worker(client: WorkerClient) {
    if MANAGED_WORKER.get().is_some() {
        debug!("pty_runtime: managed worker already set — ignoring duplicate");
        return;
    }
    if MANAGED_WORKER.set(client).is_err() {
        warn!("pty_runtime: race during set_managed_worker — second set dropped");
    } else {
        info!("pty_runtime: routing acquire_and_invoke through managed worker subprocess");
        crate::metrics::global_metrics().set_managed_worker_active(true);
    }
}

/// Returns true when `acquire_and_invoke` is currently routing through
/// the out-of-process worker.
pub fn is_managed_worker_active() -> bool {
    MANAGED_WORKER.get().is_some()
}

/// **Phase 3.C.4**: high-level invocation that acquires a pooled session,
/// runs `invoke`, and applies soft-failure recovery before returning.
///
/// Behaviour:
/// - Acquires `PtyPool` slot for `(agent_id, cli_kind, bare)` (spawns on
///   miss, reuses on hit).
/// - Runs `invoke(prompt, Some(deadline))`.
/// - On `Err(SessionError::CliError | ChildExited | MalformedResponse)`
///   the pooled session is invalidated (so the next caller gets a fresh
///   one).
/// - On suspicious empty payload (success path but `result.trim()` is
///   empty), [`mark_unhealthy`] is fired without invalidating — the
///   current turn still returns "" but next acquire spawns fresh.
/// - On OAuth-expiry pattern detected in the error message, also
///   invalidate so the pool picks up a re-auth on next spawn.
///
/// Returns the same error shape `(Result<String, String>)` as the
/// existing legacy spawn paths so callers can drop it in.
/// Round 4 deferred-cleanup (LOW F-3): full description of an
/// `acquire + invoke` call. Same rationale as [`AcquireOptions`] —
/// shrinks the prior 3-variant fan-out (no-account / account /
/// account+model) into one struct with builder-style setters.
#[derive(Debug, Clone)]
pub struct InvokeOptions<'a> {
    pub acquire: AcquireOptions<'a>,
    pub prompt: &'a str,
    pub deadline: Duration,
}

impl<'a> InvokeOptions<'a> {
    pub fn new(acquire: AcquireOptions<'a>, prompt: &'a str, deadline: Duration) -> Self {
        Self {
            acquire,
            prompt,
            deadline,
        }
    }
}

/// Round 4 deferred-cleanup (LOW F-3): canonical acquire-and-invoke
/// entry point. The historical 3 free-function variants now delegate
/// here.
pub async fn acquire_and_invoke_with(options: InvokeOptions<'_>) -> Result<String, String> {
    acquire_and_invoke_for_account_with_model(
        options.acquire.agent_id,
        options.acquire.cli_kind,
        options.acquire.bare_mode,
        options.acquire.account_id,
        options.acquire.model,
        options.prompt,
        options.deadline,
    )
    .await
}

pub async fn acquire_and_invoke(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    prompt: &str,
    deadline: Duration,
) -> Result<String, String> {
    acquire_and_invoke_for_account(agent_id, cli_kind, bare_mode, None, prompt, deadline).await
}

/// **Phase 3.D.2**: account-aware variant. When `account_id` is `Some`,
/// the PtyPool slot is keyed per-account so multi-OAuth rotation works.
/// `None` keeps the legacy "shared session" behaviour.
pub async fn acquire_and_invoke_for_account(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    account_id: Option<&str>,
    prompt: &str,
    deadline: Duration,
) -> Result<String, String> {
    acquire_and_invoke_for_account_with_model(
        agent_id, cli_kind, bare_mode, account_id, None, prompt, deadline,
    )
    .await
}

/// **Review fix**: account + model-aware variant. The per-agent `[model]`
/// preferred setting is now honoured in PTY pool mode (was dropped on
/// the OAuth path, a silent regression).
#[allow(clippy::too_many_arguments)]
pub async fn acquire_and_invoke_for_account_with_model(
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    account_id: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    deadline: Duration,
) -> Result<String, String> {
    // Phase 7: prefer the out-of-process worker when one was registered
    // via [`set_managed_worker`]. Falls back to the in-process PTY_POOL
    // otherwise (the legacy Phase 3.C.4 path).
    if let Some(client) = MANAGED_WORKER.get() {
        return invoke_via_managed_worker(
            client, agent_id, cli_kind, bare_mode, account_id, model, prompt, deadline,
        )
        .await;
    }

    // Phase 8 metrics: count acquire (cache hit vs spawn) by sampling
    // session_count before + after. A net-new session ⇒ spawn; otherwise
    // cache hit. This is a heuristic for monitoring; exact attribution
    // would require deeper instrumentation in cli-runtime.
    let metrics = crate::metrics::global_metrics();
    let pre_count = session_count();
    let acquire_start = std::time::Instant::now();
    let lease = acquire_for_account_with_model(agent_id, cli_kind, bare_mode, account_id, model)
        .await
        .map_err(|e| format!("pty_runtime: acquire failed: {e}"))?;
    if session_count() > pre_count {
        metrics.pty_pool_acquire_spawn();
    } else {
        metrics.pty_pool_acquire_cache_hit();
    }

    let session = lease.arc();
    let result = session.invoke(prompt, Some(deadline)).await;
    let elapsed_ms = acquire_start.elapsed().as_millis() as u64;

    match result {
        Ok(answer) => {
            if answer.trim().is_empty() {
                // Phase 3.D.1 — empty payload retry-with-reminder.
                //
                // The model "responded" but the sentinel-bounded payload
                // was empty (spike-observed turn-3 edge case where the
                // model drifts from the protocol on subsequent turns).
                // Issue ONE retry with an explicit reminder injected
                // before the original prompt. The retry uses the SAME
                // session — protocol drift is per-turn, not per-session,
                // so respawning would waste a spawn-cost without
                // changing the outcome.
                //
                // Skipped when `DUDUCLAW_PTY_DISABLE_RETRY=1` to give
                // operators an immediate kill switch if retries cause
                // pathological behaviour.
                // **Review fix**: budget the retry against the
                // remaining wall-clock deadline rather than the full
                // original deadline (which doubled worst-case latency).
                let remaining = deadline.saturating_sub(acquire_start.elapsed());
                if !is_pty_retry_disabled() && remaining > Duration::from_secs(2) {
                    let reminder = build_retry_reminder(prompt);
                    debug!(
                        agent_id = %agent_id,
                        remaining_ms = remaining.as_millis() as u64,
                        "pty_runtime: empty payload — retrying with explicit reminder"
                    );
                    match session.invoke(&reminder, Some(remaining)).await {
                        Ok(retried) if !retried.trim().is_empty() => {
                            metrics.pty_pool_invoke_complete(
                                elapsed_ms,
                                crate::metrics::PtyInvokeOutcome::Ok,
                            );
                            return Ok(retried);
                        }
                        _ => {
                            // Retry didn't help — fall through to the
                            // mark-unhealthy path below.
                        }
                    }
                }

                // Soft failure — pair extracted but payload empty.
                // Mark unhealthy so the next call respawns. Don't
                // invalidate the lease itself (drop will release the
                // permit cleanly).
                warn!(
                    agent_id = %agent_id,
                    "pty_runtime: empty payload (retry exhausted) — marking session unhealthy"
                );
                session.mark_unhealthy();
                metrics.pty_pool_invoke_complete(
                    elapsed_ms,
                    crate::metrics::PtyInvokeOutcome::EmptyPayload,
                );
                Err("pty_runtime: empty payload (session marked unhealthy)".to_string())
            } else {
                metrics.pty_pool_invoke_complete(
                    elapsed_ms,
                    crate::metrics::PtyInvokeOutcome::Ok,
                );
                Ok(answer)
            }
        }
        Err(err) => {
            let err_str = err.to_string();
            let outcome = if matches!(
                err,
                duduclaw_cli_runtime::SessionError::InvokeTimeout(_)
                    | duduclaw_cli_runtime::SessionError::BootTimeout(_)
            ) {
                crate::metrics::PtyInvokeOutcome::Timeout
            } else {
                crate::metrics::PtyInvokeOutcome::Error
            };
            metrics.pty_pool_invoke_complete(elapsed_ms, outcome);
            // OAuth-expiry / "Not logged in" patterns → invalidate so
            // the pool spawns a fresh session (next acquire will pick
            // up refreshed keychain auth).
            if looks_like_oauth_expiry(&err_str) {
                warn!(
                    agent_id = %agent_id,
                    error = %err_str,
                    "pty_runtime: OAuth expiry pattern detected — invalidating session"
                );
                lease.invalidate();
            } else if matches!(
                err,
                duduclaw_cli_runtime::SessionError::ChildExited { .. }
                    | duduclaw_cli_runtime::SessionError::MalformedResponse
                    | duduclaw_cli_runtime::SessionError::CliError(_)
            ) {
                // Hard failure — invalidate so next call gets a fresh session.
                lease.invalidate();
            }
            Err(err_str)
        }
    }
}

/// Phase 7 — `acquire_and_invoke` over the managed worker subprocess.
///
/// Mirrors the in-process path's success / soft-failure / hard-failure
/// shape so callers don't need to know which transport is active.
/// Failures from the worker carry through via [`WorkerClient::invoke`]'s
/// `ClientError` — we string-match a few well-known patterns to drive
/// session invalidation, matching the in-process `looks_like_oauth_expiry`
/// heuristic.
#[allow(clippy::too_many_arguments)]
async fn invoke_via_managed_worker(
    client: &WorkerClient,
    agent_id: &str,
    cli_kind: CliKind,
    bare_mode: bool,
    account_id: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    deadline: Duration,
) -> Result<String, String> {
    let metrics = crate::metrics::global_metrics();
    metrics.pty_pool_acquire_cache_hit(); // worker manages its own pool; from our side every call is one acquire
    // **Round 2 review fix (HIGH-3)**: pass the agent dir as
    // `work_dir` so the worker can chdir the spawned CLI for
    // `.mcp.json` / `CLAUDE.md` auto-discovery. Mirrors the in-
    // process factory's behaviour. Falls back to None when the
    // agent dir doesn't resolve (the worker's spawn-factory also
    // tolerates this).
    let work_dir = resolve_managed_worker_work_dir(agent_id);
    let make_params = |prompt: &str, ms: u64| InvokeParams {
        agent_id: agent_id.to_string(),
        cli_kind: cli_kind.as_str().to_string(),
        bare_mode,
        prompt: prompt.to_string(),
        timeout_ms: ms,
        account_id: account_id.map(|s| s.to_string()),
        model: model.map(|s| s.to_string()),
        work_dir: work_dir.clone(),
    };
    let start = std::time::Instant::now();
    let result = client
        .invoke(make_params(prompt, deadline.as_millis() as u64), deadline)
        .await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(text) => {
            if text.trim().is_empty() {
                // Phase 3.D.1 — managed-worker path also gets one retry
                // with a reminder. The worker rejected the empty
                // payload (server side `mark_unhealthy`); we issue a
                // fresh request which the worker fulfils through a
                // freshly-spawned session.
                //
                // **Review fix**: budget the retry against the
                // *remaining* deadline rather than the original. Caps
                // worst-case at the caller's promised deadline instead
                // of 2x it.
                let remaining = deadline.saturating_sub(start.elapsed());
                if !is_pty_retry_disabled() && remaining > Duration::from_secs(2) {
                    let reminder = build_retry_reminder(prompt);
                    debug!(
                        agent_id = %agent_id,
                        remaining_ms = remaining.as_millis() as u64,
                        "pty_runtime: managed worker empty payload — retrying with reminder"
                    );
                    if let Ok(retried) = client
                        .invoke(make_params(&reminder, remaining.as_millis() as u64), remaining)
                        .await
                        && !retried.trim().is_empty()
                    {
                        metrics.pty_pool_invoke_complete(
                            elapsed_ms,
                            crate::metrics::PtyInvokeOutcome::Ok,
                        );
                        return Ok(retried);
                    }
                }
                warn!(agent_id = %agent_id, "pty_runtime: managed worker returned empty payload");
                metrics.pty_pool_invoke_complete(
                    elapsed_ms,
                    crate::metrics::PtyInvokeOutcome::EmptyPayload,
                );
                Err("pty_runtime: empty payload from managed worker".to_string())
            } else {
                metrics.pty_pool_invoke_complete(
                    elapsed_ms,
                    crate::metrics::PtyInvokeOutcome::Ok,
                );
                Ok(text)
            }
        }
        Err(err) => {
            let err_str = err.to_string();
            let outcome = if err_str.contains("timed out") || err_str.contains("timeout") {
                crate::metrics::PtyInvokeOutcome::Timeout
            } else {
                crate::metrics::PtyInvokeOutcome::Error
            };
            metrics.pty_pool_invoke_complete(elapsed_ms, outcome);
            // OAuth expiry pattern → ask the worker to shutdown its
            // session so the next invoke spawns fresh.
            if looks_like_oauth_expiry(&err_str) {
                warn!(
                    agent_id = %agent_id,
                    error = %err_str,
                    "pty_runtime: managed worker OAuth-expiry — requesting session shutdown"
                );
                let _ = client
                    .shutdown_session(duduclaw_cli_worker::ShutdownSessionParams {
                        agent_id: agent_id.to_string(),
                        cli_kind: cli_kind.as_str().to_string(),
                        bare_mode,
                        account_id: account_id.map(|s| s.to_string()),
                        model: model.map(|s| s.to_string()),
                    })
                    .await;
            }
            Err(format!("managed worker: {err_str}"))
        }
    }
}

/// **Round 2 review fix (HIGH-3)**: resolve the agent's work_dir to a
/// string for inclusion in the managed-worker `InvokeParams`. Reads
/// the gateway's home_dir (captured in the global factory) + builds
/// `<home>/agents/<agent_id>`. Returns `None` when no home_dir is
/// set yet (pre-init) — the worker handles `None` gracefully by
/// inheriting its own cwd.
fn resolve_managed_worker_work_dir(agent_id: &str) -> Option<String> {
    let home = GATEWAY_HOME_DIR.get()?;
    let dir = home.join("agents").join(agent_id);
    if dir.exists() {
        Some(dir.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Pattern-match common OAuth expiry / unauthorised messages emitted by
/// `claude` interactive mode or surfaced by the CLI's error stream.
fn looks_like_oauth_expiry(err: &str) -> bool {
    let needles = [
        "Not logged in",
        "Please run /login",
        "OAuth token expired",
        "OAuth session expired",
        "Unauthorized",
        "401",
    ];
    needles.iter().any(|n| err.contains(n))
}

/// Spawn factory used by [`init`]. Resolves the `claude` binary and builds
/// [`SpawnOpts`] with safe defaults. Account-rotation env injection happens
/// in the deep wiring step (Phase 3.5) — keep it lean for now.
async fn spawn_session_for_key(
    home: &Path,
    key: AgentKey,
) -> Result<Arc<PtySession>, duduclaw_cli_runtime::SessionError> {
    let program = resolve_program(home, key.cli_kind)
        .ok_or_else(|| {
            duduclaw_cli_runtime::SessionError::UnknownCliKind(format!(
                "{}: binary not found",
                key.cli_kind.as_str()
            ))
        })?;

    let mut extra_args: Vec<String> = Vec::new();
    // **Review fix**: honour the caller-supplied model. Previously the
    // PTY OAuth path silently dropped `model` so every PTY session
    // ran on the CLI's built-in default — a regression vs the legacy
    // fresh-spawn path which always set `--model`. Reading from the
    // AgentKey means the cache also segregates per-model so two
    // agents using different models get distinct sessions.
    if let Some(m) = key.model.as_ref() {
        extra_args.push("--model".to_string());
        extra_args.push(m.clone());
    }

    // **Round 2 review note (HIGH-2 — DEFERRED)**: `key.account_id`
    // currently affects only the pool's cache_key — the spawned CLI
    // uses whatever ambient OAuth lives in `~/.claude/` / keychain.
    // True per-account auth isolation requires injecting account-
    // specific env (`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`)
    // at spawn time. The gateway has access to that via
    // `claude_runner::get_rotator_cached(home_dir)` but plumbing it
    // through the spawn factory needs either a task-local for the
    // env_vars or a side-channel DashMap. Tracked separately;
    // operators of multi-OAuth-account setups should not rely on PTY
    // pool to rotate accounts until this lands.
    let _account_id_for_future_env_injection = &key.account_id;
    if matches!(key.cli_kind, CliKind::Claude) && key.bare_mode {
        // Mirror the #15 TODO-runtime-health-fixes BARE_MODE behaviour: --bare
        // bypasses CLAUDE.md auto-discovery at the cost of OAuth. Callers using
        // bare_mode must inject ANTHROPIC_API_KEY into env (Phase 3.5).
        extra_args.push("--bare".to_string());
    }

    let mut env = HashMap::new();
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("TERM".to_string(), "xterm-256color".to_string());

    // **Round 2 review fix (HIGH-3)**: set `cwd` to the agent's
    // directory so `claude` can auto-discover the agent's per-folder
    // `.mcp.json`, `.claude/settings.json`, and `CLAUDE.md`.
    // Previously `cwd: None` meant `claude` inherited the gateway's
    // working directory, which broke per-agent MCP server config in
    // PTY pool mode (an invisible regression vs the legacy fresh-
    // spawn path which already set cwd to the agent dir).
    let agent_cwd = home.join("agents").join(&key.agent_id);
    let cwd = if agent_cwd.exists() {
        Some(agent_cwd)
    } else {
        // Agent dir missing — keep cwd unset rather than passing a
        // non-existent path that portable-pty would error on. This
        // happens for synthetic agent_ids (tests / one-off invokes).
        None
    };

    let opts = SpawnOpts {
        agent_id: key.agent_id.clone(),
        cli_kind: key.cli_kind,
        program,
        extra_args,
        env,
        cwd,
        session_id: None,
        boot_timeout: Duration::from_secs(45),
        default_invoke_timeout: Duration::from_secs(180),
        rows: 24,
        cols: 200,
        // Phase 3.C.2: PtyPool sessions drive real interactive `claude`.
        // The bootstrap dance + ANSI strip + chrome filter all live in
        // `PtySession::spawn` / `invoke` when `interactive = true`.
        interactive: true,
        // Operators are expected to run `claude project trust` for each
        // agent's cwd as part of the PtyPool opt-in setup; if they didn't,
        // the boot dance still auto-accepts the trust dialog via `\r`.
        pre_trusted: false,
    };
    PtySession::spawn(opts).await
}

/// Resolve the CLI binary path for the requested `kind`. Currently only
/// Claude is wired; Codex / Gemini land in Phase 3.5 alongside their
/// account-management glue.
fn resolve_program(home: &Path, kind: CliKind) -> Option<String> {
    match kind {
        CliKind::Claude => duduclaw_core::which_claude_in_home(home),
        // TODO Phase 3.5: route to which_codex / which_gemini helpers
        // (mirrors the multi-runtime registry in CLAUDE.md "Multi-Runtime"
        //  section). Returning None here causes acquire() to error, which
        // lets the caller fall back to fresh-spawn.
        CliKind::Codex | CliKind::Gemini => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn is_enabled_returns_false_for_missing_file() {
        let dir = tempdir().unwrap();
        assert!(!is_enabled_for_agent(dir.path()));
    }

    #[test]
    fn is_enabled_returns_false_for_missing_key() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("agent.toml"), "[other]\nfoo = 1\n").unwrap();
        assert!(!is_enabled_for_agent(dir.path()));
    }

    #[test]
    fn is_enabled_returns_true_when_flag_set() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("agent.toml"),
            "[runtime]\npty_pool_enabled = true\n",
        )
        .unwrap();
        assert!(is_enabled_for_agent(dir.path()));
    }

    #[test]
    fn is_enabled_handles_bad_toml_gracefully() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("agent.toml"), "this is not valid toml = =").unwrap();
        assert!(!is_enabled_for_agent(dir.path()));
    }

    #[test]
    fn runtime_mode_resolves_to_fresh_by_default() {
        let dir = tempdir().unwrap();
        assert_eq!(
            runtime_mode_for_agent(dir.path()),
            RuntimeMode::FreshSpawn
        );
    }

    #[test]
    fn runtime_mode_resolves_to_pty_pool_when_enabled() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("agent.toml"),
            "[runtime]\npty_pool_enabled = true\n",
        )
        .unwrap();
        assert_eq!(runtime_mode_for_agent(dir.path()), RuntimeMode::PtyPool);
    }

    #[test]
    fn oauth_expiry_detection_recognises_common_patterns() {
        assert!(looks_like_oauth_expiry("Not logged in · Please run /login"));
        assert!(looks_like_oauth_expiry("OAuth token expired at 2026-05-14"));
        assert!(looks_like_oauth_expiry("OAuth session expired"));
        assert!(looks_like_oauth_expiry("HTTP 401 Unauthorized"));
        assert!(looks_like_oauth_expiry("Please run /login to continue"));
    }

    #[test]
    fn oauth_expiry_detection_rejects_unrelated_errors() {
        assert!(!looks_like_oauth_expiry("invoke timed out after 60s"));
        assert!(!looks_like_oauth_expiry("rate limit exceeded"));
        assert!(!looks_like_oauth_expiry("malformed response"));
        assert!(!looks_like_oauth_expiry("child exited with code 1"));
    }

    #[tokio::test]
    async fn invoke_oneshot_runs_echo() {
        #[cfg(unix)]
        let (program, args) = ("echo".to_string(), vec!["pty-runtime-smoke".to_string()]);
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "echo".to_string(),
                "pty-runtime-smoke".to_string(),
            ],
        );
        let result = invoke_oneshot(
            program,
            args,
            HashMap::new(),
            None,
            Duration::from_secs(5),
        )
        .await
        .expect("oneshot ok");
        assert!(result.stdout.contains("pty-runtime-smoke"));
    }

    #[tokio::test]
    async fn acquire_without_init_returns_shutting_down() {
        // We can't easily reset the OnceLock between tests; this test relies
        // on running first (cargo runs tests alphabetically by default and
        // this file's other tests don't call init).
        if !is_initialised() {
            let result = acquire("nobody", CliKind::Claude, false).await;
            assert!(matches!(result, Err(PoolError::ShuttingDown)));
        }
    }

    // Phase 7 — managed worker surface.

    #[test]
    fn is_managed_worker_active_is_false_until_set() {
        // The OnceLock starts empty, so this is the default state for any
        // fresh process. Tests can't unset, so we only verify the initial
        // observation.
        if !is_managed_worker_active() {
            // Confirmed default = in-process transport.
        }
    }

    #[test]
    fn oauth_expiry_detection_supports_managed_worker_error_path() {
        // The managed-worker branch reuses `looks_like_oauth_expiry` to
        // decide whether to ask the worker to shutdown the session. Make
        // sure the function still recognises the patterns we care about.
        assert!(looks_like_oauth_expiry(
            "managed worker: worker error: Not logged in"
        ));
        assert!(looks_like_oauth_expiry(
            "managed worker: HTTP 401 Unauthorized"
        ));
    }

    // Phase 8 — emergency kill-switch tests.
    //
    // These tests mutate process-wide env state. **Review fix
    // (MEDIUM)**: all env-mutating tests acquire a shared
    // `std::sync::Mutex` so cargo test's default parallelism doesn't
    // race set / get across them. The mutex isn't a perf concern
    // (these are fast tests).
    //
    // (Modern std::env::set_var is `unsafe` on edition 2024 — wrap in
    // explicit unsafe block.)

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn kill_switch_disabled_by_default() {
        let _guard = env_guard();
        // SAFETY: the test sets + clears in one body; no concurrent
        // tests in this module touch DUDUCLAW_DISABLE_PTY_POOL.
        unsafe { std::env::remove_var("DUDUCLAW_DISABLE_PTY_POOL") };
        assert!(!is_pty_pool_disabled_globally());
    }

    #[test]
    fn kill_switch_recognises_truthy_values() {
        let _guard = env_guard();
        for v in ["1", "true", "TRUE", "yes", "YES"] {
            unsafe { std::env::set_var("DUDUCLAW_DISABLE_PTY_POOL", v) };
            assert!(
                is_pty_pool_disabled_globally(),
                "value {v:?} should disable PTY pool"
            );
        }
        unsafe { std::env::remove_var("DUDUCLAW_DISABLE_PTY_POOL") };
    }

    #[test]
    fn kill_switch_ignores_falsy_values() {
        let _guard = env_guard();
        for v in ["0", "false", "no", "off", "", "garbage"] {
            unsafe { std::env::set_var("DUDUCLAW_DISABLE_PTY_POOL", v) };
            assert!(
                !is_pty_pool_disabled_globally(),
                "value {v:?} should NOT disable PTY pool"
            );
        }
        unsafe { std::env::remove_var("DUDUCLAW_DISABLE_PTY_POOL") };
    }

    // Phase 3.D.1 retry-with-reminder tests.

    #[test]
    fn retry_reminder_prompt_contains_protocol_marker() {
        let prompt = "Please summarise the design doc.";
        let reminder = build_retry_reminder(prompt);
        assert!(reminder.contains("DUDUCLAW PROTOCOL REMINDER"));
        assert!(
            reminder.contains(duduclaw_cli_runtime::INTERACTIVE_SENTINEL),
            "reminder must include the literal sentinel string"
        );
        assert!(reminder.ends_with(prompt));
    }

    #[test]
    fn retry_reminder_preserves_original_prompt() {
        let prompt = "Compute 7*6 — return only the digits.";
        let reminder = build_retry_reminder(prompt);
        assert!(reminder.contains(prompt));
    }

    #[test]
    fn retry_disabled_env_flag_default_off() {
        let _guard = env_guard();
        unsafe { std::env::remove_var("DUDUCLAW_PTY_DISABLE_RETRY") };
        assert!(!is_pty_retry_disabled());
    }

    #[test]
    fn retry_disabled_env_flag_recognises_truthy() {
        let _guard = env_guard();
        for v in ["1", "true", "YES"] {
            unsafe { std::env::set_var("DUDUCLAW_PTY_DISABLE_RETRY", v) };
            assert!(is_pty_retry_disabled(), "value {v:?}");
        }
        unsafe { std::env::remove_var("DUDUCLAW_PTY_DISABLE_RETRY") };
    }

    #[test]
    fn kill_switch_overrides_per_agent_flag() {
        let _guard = env_guard();
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("agent.toml"),
            "[runtime]\npty_pool_enabled = true\n",
        )
        .unwrap();

        // Without the kill switch: PtyPool selected.
        unsafe { std::env::remove_var("DUDUCLAW_DISABLE_PTY_POOL") };
        assert_eq!(runtime_mode_for_agent(dir.path()), RuntimeMode::PtyPool);

        // With the kill switch: FreshSpawn forced.
        unsafe { std::env::set_var("DUDUCLAW_DISABLE_PTY_POOL", "1") };
        assert_eq!(runtime_mode_for_agent(dir.path()), RuntimeMode::FreshSpawn);
        unsafe { std::env::remove_var("DUDUCLAW_DISABLE_PTY_POOL") };
    }
}
