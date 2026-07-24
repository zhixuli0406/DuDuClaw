//! xAI **Grok Build** CLI runtime — `grok -p <prompt>` (R4).
//!
//! Background: xAI ships the official **"Grok Build"** terminal coding agent
//! (docs at <https://docs.x.ai/build>). Binary `grok`; installed via
//! `curl -fsSL https://x.ai/cli/install.sh | bash` (there is **no** official npm
//! package — `@vibe-kit/grok-cli` etc. are unrelated third-party clients and are
//! deliberately NOT modeled here). It supports Plan Mode, parallel subagents in
//! git worktrees, native MCP, a `-p/--single` headless mode, and ACP.
//!
//! This runtime wires CLI detection + headless spawn. Like the other CLI
//! backends it embeds the system prompt + history *inside* the prompt argument
//! (guaranteed delivery regardless of which context-file convention applies) and
//! captures plain stdout text.
//!
//! ── Verified against docs.x.ai (2026-07-13) ──────────────────────────────────
//!   * Binary `grok`; headless one-shot is `-p, --single <PROMPT>` (value-
//!     consuming) — [docs.x.ai/build/cli/headless-scripting].
//!   * Model flag `-m, --model <MODEL>` — [docs.x.ai/build/cli/reference].
//!   * Config `~/.grok/config.toml` (TOML; `$GROK_HOME` overrides). MCP servers
//!     live under `[mcp_servers.<name>]` with `command`/`args`/`env`/`enabled`
//!     (same shape as Codex, NOT Claude's `.mcp.json`) — [docs.x.ai/build/settings].
//!   * Tool confinement flags `--tools <LIST>` / `--disallowed-tools <LIST>`
//!     (analogues of Claude's `--allowedTools`/`--disallowedTools`) — [.../cli/reference].
//!   * Instruction file family: `AGENTS.md` (also honours `CLAUDE.md`) —
//!     [docs.x.ai/build/features/skills-plugins-marketplaces].
//!   * Auth: `XAI_API_KEY` env var (NOT `GROK_API_KEY`); interactive `grok login`
//!     (OIDC / device-code) — [docs.x.ai/build/enterprise].
//!   * Structured output `--output-format plain|json|streaming-json` exists.
//!
//! ── Residual items to confirm against a live CLI (marked at each site) ────────
//!   1. The `--tools` / `--disallowed-tools` LIST delimiter (comma assumed).
//!   2. Whether Grok discovers a *project-local* `.grok/config.toml` for
//!      `mcp_servers` (we write per-agent config there, cwd-rooted, and also
//!      forward the agent identity via spawn env as a fallback).
//!   3. The `--output-format json` payload schema (so plain stdout + estimated
//!      tokens is used for now, not real usage counts).

use std::path::Path;

use async_trait::async_trait;
use tracing::{info, warn};

use duduclaw_core::types::sandbox_level_for;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Hard backstop on the whole subprocess.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Availability-probe timeout — a bare `grok` starts the interactive TUI, so the
/// probe must never be able to hang the registry.
const PROBE_TIMEOUT_SECS: u64 = 5;
/// Cap the system prompt embedded into the prompt argument (ARG_MAX safety).
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;
/// Wall-clock cap for the single PTY one-shot retry (see [`GrokRuntime::pty_retry`]).
const PTY_RETRY_TIMEOUT_SECS: u64 = 90;

// ── Environment integrity & auth diagnostics ────────────────────
//
// Root cause of the headless "empty stdout, exit 0" symptom on a customer box
// where `grok` *interactive* works but the DuDuClaw-spawned `grok -p` returns
// nothing: under launchd / Docker the gateway process's `$HOME` is frequently
// NOT the user's home (`/var/root`, unset, a service account), so `grok` looks
// in the wrong `~/.grok`, finds no device-auth / SuperGrok credentials, and —
// instead of erroring — prints nothing and exits 0. Same shape as the 2026
// Claude `-p` OAuth-subscription breakage. The fixes below are defensive and
// remotely diagnosable because we cannot reproduce locally (no grok CLI / no
// SuperGrok account on this machine).

/// Resolve the **user's** home directory to hand `grok` so it finds `~/.grok`.
///
/// Precedence (pure, so it is unit-testable without touching the process env):
/// 1. Parent of the DuDuClaw home when it is the canonical `<user>/.duduclaw`
///    layout — this is the home the *gateway* already resolved at startup and
///    the most reliable signal under launchd (operators set `DUDUCLAW_HOME`
///    explicitly in the plist, so its parent is the real user home even when
///    the ambient `$HOME` is wrong/unset).
/// 2. `$HOME` / `%USERPROFILE%` (passed in as `env_home`), when non-empty.
/// 3. The DuDuClaw home itself (never empty) as a last resort.
pub fn resolve_user_home(duduclaw_home: &Path, env_home: Option<&str>) -> std::path::PathBuf {
    if duduclaw_home.file_name().and_then(|n| n.to_str()) == Some(".duduclaw") {
        if let Some(parent) = duduclaw_home.parent() {
            if !parent.as_os_str().is_empty() {
                return parent.to_path_buf();
            }
        }
    }
    if let Some(h) = env_home {
        let h = h.trim();
        if !h.is_empty() {
            return std::path::PathBuf::from(h);
        }
    }
    duduclaw_home.to_path_buf()
}

/// Build the home/credential env pairs stamped onto the `grok` spawn. Explicit
/// `HOME` (Unix) + `USERPROFILE` (Windows) so a launchd/service/Docker parent
/// env with a wrong or missing home can't misdirect `~/.grok` lookup, plus
/// `GROK_HOME` forwarded verbatim when the operator set grok's own config-root
/// override. Pure so the HOME injection is unit-testable without spawning.
pub fn build_home_env(user_home: &Path, grok_home_override: Option<&str>) -> Vec<(String, String)> {
    let home = user_home.to_string_lossy().to_string();
    let mut env = vec![
        ("HOME".to_string(), home.clone()),
        ("USERPROFILE".to_string(), home),
    ];
    if let Some(gh) = grok_home_override {
        let gh = gh.trim();
        if !gh.is_empty() {
            env.push(("GROK_HOME".to_string(), gh.to_string()));
        }
    }
    env
}

/// Known stderr fingerprints for a `grok` CLI that is NOT authenticated (never
/// logged in, credentials expired, or a missing/invalid API key). Matched
/// case-insensitively at word/phrase boundaries (2026-06 review convention #2 —
/// no unanchored `contains` for a routing decision) so a normal answer that
/// merely mentions e.g. "authentication" as a topic never trips the classifier;
/// only these specific operational phrases do. Derived from the docs.x.ai auth
/// notes (device-auth / `grok login`, `XAI_API_KEY`) plus the conventional
/// CLI/HTTP auth-failure vocabulary.
const GROK_AUTH_FAILURE_PATTERNS: &[&str] = &[
    // Verified live (grok 0.2.111, wrong $HOME): "Not signed in. To
    // authenticate without a browser, run:\n  grok login --device-code"
    "not signed in",
    "not signed-in",
    "not logged in",
    "not authenticated",
    "unauthenticated",
    "unauthorized",
    "login required",
    "please log in",
    "please login",
    "you must log in",
    "device auth",
    "device-auth",
    "grok login",
    "authentication failed",
    "authentication required",
    "auth failed",
    "invalid api key",
    "invalid token",
    "invalid credentials",
    "missing api key",
    "missing credentials",
    "no api key",
    "api key not",
    "token expired",
    "expired token",
    "session expired",
    "401",
];

/// True when `stderr` carries one of the known grok auth-failure fingerprints.
/// Empty / normal text → false (誤殺防護：一般回覆不會觸發).
pub fn looks_like_grok_auth_failure(stderr: &str) -> bool {
    GROK_AUTH_FAILURE_PATTERNS
        .iter()
        .any(|p| duduclaw_core::word_contains_ci(stderr, p))
}

/// Build the auth-failure error string. Contains "not logged in" +
/// "authentication" so the gateway's `classify_cli_failure` lands
/// `FailureReason::AuthFailed` (→ the "認證失效" user message), plus a zh-TW
/// operator action so remote debugging is one step.
fn grok_auth_error(stderr_tail: &str) -> String {
    format!(
        "Grok CLI authentication failure (not logged in / 憑證失效): \
         請在執行 gateway 的環境（若為 Docker 需進入容器）執行 `grok login --device-code` \
         重新登入（或設定有效的 XAI_API_KEY）。stderr tail: {stderr_tail}"
    )
}

/// Decide whether to attempt the one-shot PTY retry after a `grok -p` that
/// returned empty stdout with exit 0. Retry ONLY that exact shape, and only when
/// it is not an auth failure (a re-run under a TTY cannot conjure missing
/// credentials) and retries aren't disabled (env kill-switch / native sandbox
/// required). Pure + unit-tested so the decision contract is pinned.
fn should_pty_retry_empty(
    stdout_empty: bool,
    exit_success: bool,
    is_auth_failure: bool,
    retry_disabled: bool,
) -> bool {
    stdout_empty && exit_success && !is_auth_failure && !retry_disabled
}

/// Runtime that delegates to the xAI Grok Build CLI.
pub struct GrokRuntime {
    grok_path: String,
}

impl GrokRuntime {
    pub fn new() -> Self {
        Self {
            grok_path: resolve_grok_path(),
        }
    }
}

impl Default for GrokRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the `grok` binary on `$PATH`, then the usual HOME-rooted install
/// locations. The availability probe ultimately decides whether the runtime
/// registers, so a stale guess here is harmless.
fn resolve_grok_path() -> String {
    duduclaw_core::which_grok().unwrap_or_else(|| "grok".to_string())
}

/// Build the prompt payload: system instructions + history + user message, all
/// embedded as text. Pure function so it is unit-testable without spawning the
/// CLI.
///
/// Grok has a verified `--system-prompt-override <TEXT>` flag, but it *replaces*
/// Grok's own agent scaffolding wholesale; embedding the system prompt in the
/// payload (as the other CLI backends do) delivers it without clobbering that
/// scaffolding.
fn build_prompt(context: &RuntimeContext, user_prompt: &str) -> String {
    // Limit system_prompt to 64KB to avoid ARG_MAX issues. Char-boundary-safe
    // truncation (never raw byte-index slicing — 2026-06 review convention #1).
    let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        warn!(
            agent = %context.agent_id,
            original_len = context.system_prompt.len(),
            "system_prompt truncated to 64KB"
        );
        duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES)
    } else {
        &context.system_prompt
    };

    // Prevent argument injection: a prompt starting with '-' would be parsed as a flag.
    let safe_prompt = if user_prompt.starts_with('-') {
        format!(" {user_prompt}")
    } else {
        user_prompt.to_string()
    };

    let with_history = if context.conversation_history.is_empty() {
        safe_prompt
    } else {
        super::format_history_as_prompt(&context.conversation_history, &safe_prompt)
    };

    if system_prompt.is_empty() {
        with_history
    } else {
        // Escape closing tag in the system prompt to keep the XML frame intact.
        let safe_system =
            system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
        format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{with_history}")
    }
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for GrokRuntime {
    fn name(&self) -> &str {
        "grok"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "GrokRuntime: executing via grok -p");

        // MCP wiring: register the duduclaw MCP server before spawning. Grok is
        // MCP-native; the config format is verified (`[mcp_servers.duduclaw]`
        // TOML) but per-agent project-local discovery is the one residual — so we
        // ALSO forward the agent identity via spawn env below. Warn-not-fatal.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) = Self::ensure_duduclaw_mcp_config(dir, &context.agent_id).await {
                warn!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write grok MCP config — continuing without it"
                );
            }
        }

        let payload = build_prompt(context, prompt);

        // Build the argument vector ONCE so the tokio spawn and the PTY retry run
        // a byte-identical command.
        let caps = context.capabilities.as_ref();
        let mut args: Vec<String> = Vec::new();

        // Model — verified `-m, --model <MODEL>`.
        if !context.model.is_empty() {
            args.push("--model".to_string());
            args.push(context.model.clone());
        }

        // Tool confinement — verified `--tools` / `--disallowed-tools` flags. This
        // is an ADDITIVE best-effort layer on top of the hard `native_sandbox`
        // confinement below (which stays fail-closed). RESIDUAL: the LIST
        // delimiter is assumed comma; if a live CLI wants spaces/repeats this
        // needs adjusting — a wrong delimiter surfaces as visible behavior, never
        // a silent bypass, because native_sandbox remains the hard gate.
        if let Some(c) = caps {
            if !c.allowed_tools.is_empty() {
                args.push("--tools".to_string());
                args.push(c.allowed_tools.join(","));
            }
            if !c.denied_tools.is_empty() {
                args.push("--disallowed-tools".to_string());
                args.push(c.denied_tools.join(","));
            }
            if c.has_tool_restrictions() {
                info!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    level = ?sandbox_level_for(caps),
                    "grok tool confinement applied via --tools/--disallowed-tools (+ native_sandbox if enabled)"
                );
            }
        }

        // Prompt LAST as the value of `-p, --single` (verified value-consuming).
        args.push("-p".to_string());
        args.push(payload.clone());

        // Environment integrity (headless-empty-output root cause): stamp the
        // user's REAL home so grok finds `~/.grok` credentials even under
        // launchd/Docker where the parent env's HOME is wrong/unset. Also forward
        // `GROK_HOME` verbatim when the operator set it. Built once and reused by
        // the PTY retry so both spawns look at the same credential root.
        let user_home = resolve_user_home(&context.home_dir, std::env::var("HOME").ok().as_deref());
        let grok_home_override = std::env::var("GROK_HOME").ok();
        let home_env = build_home_env(&user_home, grok_home_override.as_deref());

        // Auth: forward `XAI_API_KEY` when set (verified var — NOT GROK_API_KEY).
        // Interactive `grok login` sessions are honoured by the CLI itself.
        let api_key = std::env::var("XAI_API_KEY").unwrap_or_default();

        let mut cmd = tokio::process::Command::new(&self.grok_path);
        cmd.args(&args);

        // Working directory (also the root Grok walks for AGENTS.md / project config).
        if let Some(ref dir) = context.agent_dir {
            cmd.current_dir(dir);
        }

        for (k, v) in &home_env {
            cmd.env(k, v);
        }
        if !api_key.is_empty() {
            cmd.env("XAI_API_KEY", &api_key);
        }

        // Forward the duduclaw MCP identity via spawn env so the MCP server child
        // (spawned by grok, inheriting this env) resolves the right agent even if
        // Grok doesn't pick up the per-agent project config (residual #2).
        cmd.env(duduclaw_core::ENV_AGENT_ID, &context.agent_id);
        for (k, v) in duduclaw_core::mcp_forward_env_vars() {
            cmd.env(k, v);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). The hard, fail-closed confinement on this
        // runtime; fail-closed if required but unavailable.
        let native_sandbox_active = caps.map(|c| c.native_sandbox).unwrap_or(false);
        super::apply_native_sandbox(&mut cmd, caps, context.agent_dir.as_deref(), "grok")?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Grok CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn grok: {e}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Auth-aware: a non-zero exit whose stderr matches a known
            // not-logged-in / expired fingerprint → AuthFailed + zh-TW action.
            if looks_like_grok_auth_failure(&stderr) {
                warn!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    code,
                    "grok CLI reported an authentication failure"
                );
                return Err(grok_auth_error(&duduclaw_core::truncate_bytes(
                    stderr.trim(),
                    300,
                )));
            }
            return Err(format!(
                "Grok CLI exited with {code}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let content = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Empty stdout with exit 0 is a FAILURE: an Ok("") would be silently
        // dropped by every channel (empty sends are skipped) and would append an
        // empty assistant turn to the session. Surface it so failover + the
        // classified user message fire — but first (a) detect an auth failure
        // hiding behind the empty output, and (b) attempt ONE PTY retry, which
        // recovers the `grok -p` empty-under-pipe class the same way the Claude
        // OAuth path does.
        if content.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_tail = duduclaw_core::truncate_bytes(stderr.trim(), 300);
            let is_auth = looks_like_grok_auth_failure(&stderr);
            if is_auth {
                warn!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    "grok CLI returned empty output with an auth-failure stderr signature"
                );
                return Err(grok_auth_error(&stderr_tail));
            }

            // PTY one-shot retry (the key defence). Skip when native sandbox is
            // required — we can't replicate that confinement wrap on the oneshot
            // path, so retrying unconfined would break the fail-closed contract.
            let retry_disabled =
                crate::pty_runtime::is_pty_retry_disabled() || native_sandbox_active;
            if should_pty_retry_empty(true, true, is_auth, retry_disabled) {
                match self.pty_retry(&args, &home_env, &api_key, context).await {
                    Ok(text) if !text.trim().is_empty() => {
                        info!(
                            runtime = "grok",
                            agent = %context.agent_id,
                            pty_retry = true,
                            "grok PTY retry recovered a non-empty response"
                        );
                        let recovered = text.trim().to_string();
                        let input_tokens = crate::prompt_compression::estimate_tokens(&payload);
                        let output_tokens = crate::prompt_compression::estimate_tokens(&recovered);
                        return Ok(RuntimeResponse {
                            content: recovered,
                            input_tokens,
                            output_tokens,
                            cache_read_tokens: 0,
                            model_used: context.model.clone(),
                            runtime_name: "grok".to_string(),
                        });
                    }
                    Ok(_) => warn!(
                        runtime = "grok",
                        agent = %context.agent_id,
                        pty_retry = false,
                        "grok PTY retry still returned empty output"
                    ),
                    Err(e) => warn!(
                        runtime = "grok",
                        agent = %context.agent_id,
                        pty_retry = false,
                        error = %e,
                        "grok PTY retry failed"
                    ),
                }
            } else {
                info!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    pty_retry = false,
                    native_sandbox = native_sandbox_active,
                    "grok empty output — PTY retry skipped"
                );
            }

            return Err(format!(
                "Empty response from Grok CLI (exit 0); stderr tail: {stderr_tail}"
            ));
        }

        // Plain stdout is captured; `--output-format json|streaming-json` is
        // verified to exist but its usage-stats schema is unconfirmed (residual
        // #3), so tokens are ESTIMATED with the gateway's CJK-aware heuristic.
        // These feed CostTelemetry as approximations. Switch to real counts once
        // the JSON schema is confirmed.
        let input_tokens = crate::prompt_compression::estimate_tokens(&payload);
        let output_tokens = crate::prompt_compression::estimate_tokens(&content);

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "grok".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        // A bare `grok` opens the TUI, so probe with `--version` under a hard
        // timeout: a hang (or a non-zero exit) ⇒ treat as unavailable.
        let probe = tokio::process::Command::new(&self.grok_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS), probe).await,
            Ok(Ok(status)) if status.success()
        )
    }
}

// ── Streaming ───────────────────────────────────────────────────

impl GrokRuntime {
    /// Execute and return chunks. `grok -p` is request/response, so this wraps the
    /// normal execution into a single `Done` chunk (mirrors the other CLI backends).
    pub async fn execute_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<Vec<super::RuntimeChunk>, String> {
        let response = self.execute(prompt, context).await?;
        Ok(vec![super::RuntimeChunk::Done(response)])
    }
}

// ── PTY one-shot retry ──────────────────────────────────────────

impl GrokRuntime {
    /// One-shot PTY retry of the identical `grok -p` command. Runs the child
    /// under a REAL TTY (portable-pty: ConPTY on Windows, openpty on Unix) via
    /// the generic [`crate::pty_runtime::invoke_oneshot`] — the same primitive
    /// that recovers Claude's OAuth headless breakage — then strips ANSI chrome
    /// and returns the trimmed stdout. Best-effort: any error is a signal to keep
    /// the original empty-response failure. The env mirrors the tokio spawn
    /// (HOME/USERPROFILE/GROK_HOME + XAI_API_KEY + DUDUCLAW_* identity) so the
    /// retry looks at the same `~/.grok` credential root.
    async fn pty_retry(
        &self,
        args: &[String],
        home_env: &[(String, String)],
        api_key: &str,
        context: &RuntimeContext,
    ) -> Result<String, String> {
        let mut env: std::collections::HashMap<String, String> = home_env.iter().cloned().collect();
        if !api_key.is_empty() {
            env.insert("XAI_API_KEY".to_string(), api_key.to_string());
        }
        env.insert(
            duduclaw_core::ENV_AGENT_ID.to_string(),
            context.agent_id.clone(),
        );
        for var in ["DUDUCLAW_HOME", "DUDUCLAW_PORT", "DUDUCLAW_INSTANCE"] {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    env.insert(var.to_string(), v);
                }
            }
        }
        let out = crate::pty_runtime::invoke_oneshot(
            self.grok_path.clone(),
            args.to_vec(),
            env,
            context.agent_dir.clone(),
            std::time::Duration::from_secs(PTY_RETRY_TIMEOUT_SECS),
        )
        .await
        .map_err(|e| format!("grok PTY retry invoke failed: {e}"))?;
        Ok(duduclaw_cli_runtime::strip_ansi(&out.stdout)
            .trim()
            .to_string())
    }
}

// ── MCP config ──────────────────────────────────────────────────

impl GrokRuntime {
    /// Resolve the Grok config file to write. Per-agent isolation: writes to
    /// `<agent_dir>/.grok/config.toml` (cwd-rooted at spawn, mirroring the Codex
    /// per-agent `.codex/config.toml` pattern) so the user's global
    /// `~/.grok/config.toml` (models / auth) is never touched.
    fn agent_config_path(agent_dir: &std::path::Path) -> std::path::PathBuf {
        agent_dir.join(".grok").join("config.toml")
    }

    /// Merge the given MCP servers into `<agent_dir>/.grok/config.toml` under
    /// `[mcp_servers.<name>]` (verified TOML shape: `command`/`args`/`env`/
    /// `enabled`). Preserves every other table and any other server. Idempotent:
    /// returns `Ok(false)` without writing when every requested entry already
    /// matches. Comments in the file are not preserved (this is a duduclaw-owned
    /// per-agent file, not the user's hand-edited global config).
    pub async fn write_mcp_config(
        agent_dir: &std::path::Path,
        servers: &std::collections::HashMap<String, toml::Value>,
    ) -> Result<bool, String> {
        let config_path = Self::agent_config_path(agent_dir);
        let existing = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_default();
        let mut root: toml::Value = if existing.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str(&existing).map_err(|e| format!("malformed grok config.toml: {e}"))?
        };
        let root_tbl = root
            .as_table_mut()
            .ok_or_else(|| "grok config.toml root is not a table".to_string())?;

        let mcp = root_tbl
            .entry("mcp_servers".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if !mcp.is_table() {
            *mcp = toml::Value::Table(toml::map::Map::new());
        }
        let map = mcp
            .as_table_mut()
            .expect("mcp_servers normalized to table above");

        let mut changed = false;
        for (name, def) in servers {
            if map.get(name) != Some(def) {
                map.insert(name.clone(), def.clone());
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }

        if let Some(parent) = config_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        let rendered = toml::to_string_pretty(&root).map_err(|e| e.to_string())?;
        tokio::fs::write(&config_path, rendered)
            .await
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// The duduclaw MCP server as a Grok `[mcp_servers.duduclaw]` TOML table:
    /// absolute binary `command`, `args = ["mcp-server"]`, `enabled = true`, and
    /// an `env` table carrying `DUDUCLAW_AGENT_ID` (+ forwarded home/port/
    /// instance). Returns `None` when the binary can't be resolved to an absolute
    /// path (relative paths aren't safe to persist).
    fn duduclaw_server_toml(agent_id: &str) -> Option<toml::Value> {
        let bin = duduclaw_core::resolve_duduclaw_bin();
        if !bin.is_absolute() {
            return None;
        }
        let mut env = toml::map::Map::new();
        env.insert(
            duduclaw_core::ENV_AGENT_ID.to_string(),
            toml::Value::String(agent_id.to_string()),
        );
        // Shared forward set (home/port/instance + MCP auth). Grok spawns MCP
        // children with ONLY this declared env block — omitting
        // DUDUCLAW_MCP_API_KEY here was the root cause of "grok 查 odoo 不行":
        // the duduclaw mcp-server died at boot (M6 fail-closed) and the agent
        // silently lost its whole tool surface.
        for (k, v) in duduclaw_core::mcp_forward_env_vars() {
            env.insert(k, toml::Value::String(v));
        }
        let mut table = toml::map::Map::new();
        table.insert(
            "command".to_string(),
            toml::Value::String(bin.to_string_lossy().to_string()),
        );
        table.insert(
            "args".to_string(),
            toml::Value::Array(vec![toml::Value::String("mcp-server".to_string())]),
        );
        table.insert("enabled".to_string(), toml::Value::Boolean(true));
        table.insert("env".to_string(), toml::Value::Table(env));
        Some(toml::Value::Table(table))
    }

    /// Ensure the duduclaw MCP server is registered in the agent's
    /// `.grok/config.toml`. Called before every spawn; idempotent.
    pub async fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
    ) -> Result<bool, String> {
        let Some(def) = Self::duduclaw_server_toml(agent_id) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), def);
        Self::write_mcp_config(agent_dir, &servers).await
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ConversationTurn;

    fn ctx(system: &str, model: &str) -> RuntimeContext {
        RuntimeContext {
            agent_dir: None,
            system_prompt: system.to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "test".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
        }
    }

    #[test]
    fn build_prompt_wraps_system_instructions() {
        let c = ctx("You are helpful.", "grok-build-0.1");
        let out = build_prompt(&c, "Hello");
        assert!(out.contains("<system_instructions>"));
        assert!(out.contains("You are helpful."));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn build_prompt_no_system_is_plain() {
        let c = ctx("", "");
        let out = build_prompt(&c, "Just this");
        assert_eq!(out, "Just this");
    }

    #[test]
    fn build_prompt_neutralizes_leading_dash() {
        let c = ctx("", "");
        let out = build_prompt(&c, "--help me");
        assert!(
            out.starts_with(' '),
            "leading dash must be neutralized: {out:?}"
        );
    }

    #[test]
    fn build_prompt_includes_history() {
        let mut c = ctx("sys", "");
        c.conversation_history = vec![ConversationTurn {
            role: "user".to_string(),
            content: "prior".to_string(),
        }];
        let out = build_prompt(&c, "now");
        assert!(out.contains("<conversation_history>"));
        assert!(out.contains("prior"));
        assert!(out.contains("now"));
    }

    #[test]
    fn build_prompt_truncates_oversized_system_on_char_boundary() {
        // 70KB of a 3-byte CJK char — must not panic and must stay valid UTF-8.
        let big = "中".repeat(70_000 / 3);
        let c = ctx(&big, "");
        let out = build_prompt(&c, "x");
        assert!(out.is_char_boundary(out.len()));
        assert!(out.contains("x"));
    }

    #[test]
    fn name_is_grok() {
        assert_eq!(GrokRuntime::new().name(), "grok");
    }

    // ── Environment integrity ───────────────────────────────────

    #[test]
    fn resolve_user_home_prefers_duduclaw_parent() {
        // Canonical `<user>/.duduclaw` layout → parent is the real user home,
        // even when the ambient $HOME says otherwise (the launchd case).
        let home = std::path::Path::new("/Users/sam/.duduclaw");
        let got = resolve_user_home(home, Some("/var/root"));
        assert_eq!(got, std::path::PathBuf::from("/Users/sam"));
    }

    #[test]
    fn resolve_user_home_falls_back_to_env_home() {
        // A custom DUDUCLAW_HOME (not the `.duduclaw` basename) → use $HOME.
        let home = std::path::Path::new("/opt/dudu/state");
        let got = resolve_user_home(home, Some("/Users/sam"));
        assert_eq!(got, std::path::PathBuf::from("/Users/sam"));
    }

    #[test]
    fn resolve_user_home_last_resort_is_duduclaw_home() {
        // No `.duduclaw` basename AND no usable $HOME → never empty: return the
        // duduclaw home itself.
        let home = std::path::Path::new("/opt/dudu/state");
        let got = resolve_user_home(home, None);
        assert_eq!(got, std::path::PathBuf::from("/opt/dudu/state"));
        let got_empty = resolve_user_home(home, Some("   "));
        assert_eq!(got_empty, std::path::PathBuf::from("/opt/dudu/state"));
    }

    #[test]
    fn build_home_env_injects_home_and_userprofile() {
        let env = build_home_env(std::path::Path::new("/Users/sam"), None);
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("HOME").map(String::as_str), Some("/Users/sam"));
        assert_eq!(
            map.get("USERPROFILE").map(String::as_str),
            Some("/Users/sam")
        );
        assert!(!map.contains_key("GROK_HOME"), "no override → no GROK_HOME");
    }

    #[test]
    fn build_home_env_forwards_grok_home_override() {
        let env = build_home_env(std::path::Path::new("/Users/sam"), Some("/data/grok"));
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("GROK_HOME").map(String::as_str), Some("/data/grok"));
        // Blank override is ignored.
        let env2 = build_home_env(std::path::Path::new("/Users/sam"), Some("  "));
        assert!(!env2.iter().any(|(k, _)| k == "GROK_HOME"));
    }

    // ── Auth-failure classification ─────────────────────────────

    #[test]
    fn auth_failure_matches_known_fingerprints() {
        for s in [
            // Real grok 0.2.111 output, captured live 2026-07-23 (wrong $HOME):
            "Not signed in. To authenticate without a browser, run:\n  grok login --device-code",
            "Error: not logged in. Run `grok login`.",
            "HTTP 401 Unauthorized",
            "authentication failed: token expired",
            "device-auth required",
            "Invalid API key provided",
            "Please log in to continue",
            "session expired, re-authenticate",
        ] {
            assert!(
                looks_like_grok_auth_failure(s),
                "should classify as auth failure: {s:?}"
            );
        }
    }

    #[test]
    fn auth_failure_does_not_false_positive_on_normal_output() {
        // 誤殺防護: normal answers / benign stderr must NOT be read as auth
        // failures — including prose that mentions authentication as a topic.
        for s in [
            "",
            "Here is a summary of the quarterly report you asked for.",
            "The function authenticates users via OAuth; here's how it works.",
            "I wrote a login form component with a username field.",
            "grok is a helpful assistant. Model: grok-build-0.1",
            "The word authorization appears here but not as a failure.",
        ] {
            assert!(
                !looks_like_grok_auth_failure(s),
                "should NOT classify as auth failure: {s:?}"
            );
        }
    }

    #[test]
    fn grok_auth_error_lands_authfailed_classification() {
        // The error string must carry tokens the gateway's classifier keys on so
        // it lands FailureReason::AuthFailed (rendered as the "認證失效" message).
        let msg = grok_auth_error("Not logged in");
        let low = msg.to_lowercase();
        assert!(low.contains("not logged in"));
        assert!(low.contains("authentication"));
        assert!(msg.contains("grok login --device-code"));
    }

    // ── PTY retry decision ──────────────────────────────────────

    #[test]
    fn pty_retry_only_on_empty_exit0_non_auth() {
        // The exact shape we retry: empty stdout + exit 0 + not-auth + enabled.
        assert!(should_pty_retry_empty(true, true, false, false));
        // Auth failure → no retry (a TTY can't conjure missing credentials).
        assert!(!should_pty_retry_empty(true, true, true, false));
        // Retries disabled (env kill-switch / native sandbox) → no retry.
        assert!(!should_pty_retry_empty(true, true, false, true));
        // Non-empty stdout → nothing to retry.
        assert!(!should_pty_retry_empty(false, true, false, false));
        // Non-zero exit is handled by the exit-code branch, not the retry.
        assert!(!should_pty_retry_empty(true, false, false, false));
    }

    #[tokio::test]
    async fn unavailable_when_binary_missing() {
        // The registry only inserts GrokRuntime when `is_available()` is true, so
        // a missing binary ⇒ not registered. Point at a path that cannot exist.
        let rt = GrokRuntime {
            grok_path: "/nonexistent/duduclaw-grok-probe-xyzzy".to_string(),
        };
        assert!(!rt.is_available().await);
    }

    #[tokio::test]
    async fn mcp_config_writes_toml_merges_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join(".grok").join("config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        // Pre-existing config: an unrelated table + another MCP server.
        std::fs::write(
            &config_path,
            "[models]\ndefault = \"grok-build\"\n\n\
             [mcp_servers.playwright]\ncommand = \"npx\"\nargs = [\"mcp-playwright\"]\n",
        )
        .unwrap();

        let mut servers = std::collections::HashMap::new();
        let mut duduclaw = toml::map::Map::new();
        duduclaw.insert(
            "command".to_string(),
            toml::Value::String("/usr/local/bin/duduclaw".to_string()),
        );
        duduclaw.insert(
            "args".to_string(),
            toml::Value::Array(vec![toml::Value::String("mcp-server".to_string())]),
        );
        let mut env = toml::map::Map::new();
        env.insert(
            "DUDUCLAW_AGENT_ID".to_string(),
            toml::Value::String("agnes".to_string()),
        );
        duduclaw.insert("env".to_string(), toml::Value::Table(env));
        servers.insert("duduclaw".to_string(), toml::Value::Table(duduclaw));

        assert!(GrokRuntime::write_mcp_config(dir.path(), &servers)
            .await
            .unwrap());
        // Second call: entry already matches → no write.
        assert!(!GrokRuntime::write_mcp_config(dir.path(), &servers)
            .await
            .unwrap());

        let got: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            got["models"]["default"].as_str(),
            Some("grok-build"),
            "unrelated tables preserved"
        );
        assert_eq!(
            got["mcp_servers"]["playwright"]["command"].as_str(),
            Some("npx"),
            "other MCP servers preserved"
        );
        assert_eq!(
            got["mcp_servers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"].as_str(),
            Some("agnes"),
            "duduclaw entry carries the agent identity env"
        );
    }
}
