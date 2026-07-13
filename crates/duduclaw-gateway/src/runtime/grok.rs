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

        let mut cmd = tokio::process::Command::new(&self.grok_path);

        // Model — verified `-m, --model <MODEL>`.
        if !context.model.is_empty() {
            cmd.arg("--model").arg(&context.model);
        }

        // Tool confinement — verified `--tools` / `--disallowed-tools` flags. This
        // is an ADDITIVE best-effort layer on top of the hard `native_sandbox`
        // confinement below (which stays fail-closed). RESIDUAL: the LIST
        // delimiter is assumed comma; if a live CLI wants spaces/repeats this
        // needs adjusting — a wrong delimiter surfaces as visible behavior, never
        // a silent bypass, because native_sandbox remains the hard gate.
        let caps = context.capabilities.as_ref();
        if let Some(c) = caps {
            if !c.allowed_tools.is_empty() {
                cmd.arg("--tools").arg(c.allowed_tools.join(","));
            }
            if !c.denied_tools.is_empty() {
                cmd.arg("--disallowed-tools").arg(c.denied_tools.join(","));
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

        // Working directory (also the root Grok walks for AGENTS.md / project config).
        if let Some(ref dir) = context.agent_dir {
            cmd.current_dir(dir);
        }

        // Prompt LAST as the value of `-p, --single` (verified value-consuming).
        cmd.arg("-p").arg(&payload);

        // Auth: forward `XAI_API_KEY` when set (verified var — NOT GROK_API_KEY).
        // Interactive `grok login` sessions are honoured by the CLI itself.
        let api_key = std::env::var("XAI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("XAI_API_KEY", &api_key);
        }

        // Forward the duduclaw MCP identity via spawn env so the MCP server child
        // (spawned by grok, inheriting this env) resolves the right agent even if
        // Grok doesn't pick up the per-agent project config (residual #2).
        cmd.env(duduclaw_core::ENV_AGENT_ID, &context.agent_id);
        for var in ["DUDUCLAW_HOME", "DUDUCLAW_PORT", "DUDUCLAW_INSTANCE"] {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    cmd.env(var, v);
                }
            }
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). The hard, fail-closed confinement on this
        // runtime; fail-closed if required but unavailable.
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
            return Err(format!(
                "Grok CLI exited with {code}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let content = String::from_utf8_lossy(&output.stdout).trim().to_string();

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
        for var in ["DUDUCLAW_HOME", "DUDUCLAW_PORT", "DUDUCLAW_INSTANCE"] {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    env.insert(var.to_string(), toml::Value::String(v));
                }
            }
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
