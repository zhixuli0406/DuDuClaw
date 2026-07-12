//! xAI Grok CLI runtime — `grok -p <prompt>` (R4).
//!
//! Background: xAI shipped the official **"Grok Build"** terminal coding agent
//! (beta, 2026-05) — Plan Mode, up to 8 parallel subagents (each in its own git
//! worktree), **native MCP support**, a **`-p` headless mode**, and ACP (Agent
//! Client Protocol). It drives `grok-build-0.1` behind a SuperGrok ($30/mo) / X
//! Premium+ subscription; login is OAuth against accounts.x.ai. A separate
//! third-party CLI (`superagent-ai/grok-cli`) exists as an API-key-only client.
//!
//! This runtime wires **CLI detection + headless spawn** (R4 phase 1). It drives
//! the binary the same way the [`super::antigravity`] backend drives `agy`:
//! embed the system prompt + history *inside* the prompt argument (so the model
//! receives them regardless of which context-file convention the CLI reads) and
//! capture plain stdout text.
//!
//! ── UNVERIFIED assumptions (R4, 2026-07-12) ──────────────────────────────────
//! This code was written WITHOUT a live Grok CLI to verify against. Each of the
//! following is a documented assumption, flagged so a maintainer with the real
//! CLI can confirm or correct it:
//!   1. **Binary name** — official `grok` (fallback third-party `grok-cli`).
//!      See [`duduclaw_core::which_grok`].
//!   2. **Headless flag** — `-p <prompt>` (confirmed by xAI docs to exist as a
//!      "headless" mode; the exact argv shape — value-consuming vs. boolean +
//!      positional — is unconfirmed, so `-p` is emitted LAST with the payload as
//!      the following token, which is safe under both interpretations).
//!   3. **Model flag** — `--model <id>` (common CLI convention; `-m` unconfirmed).
//!   4. **System prompt** — no verified `--system` flag or context-file name, so
//!      the system prompt is embedded in the prompt payload (guaranteed delivery).
//!   5. **Output format** — no verified JSON/stream surface, so plain stdout is
//!      captured and token usage is *estimated* (like the `agy` backend).
//!   6. **MCP config** — Grok is MCP-native, but the on-disk config path is
//!      unconfirmed; the duduclaw MCP server is written to
//!      `<agent_dir>/.grok/settings.json` `mcpServers` (mirroring Gemini's
//!      `settings.json` shape) as the most likely location. Warn-not-fatal.
//!   7. **API key env** — `XAI_API_KEY` (xAI's standard var) is forwarded when
//!      set, for the third-party api-key CLI. SuperGrok OAuth is phase 2.
//!   8. **Sandbox flags** — no verified CLI confinement flag. Rather than
//!      fabricate one, capability enforcement is logged as best-effort and the
//!      only hard confinement is the opt-in native OS sandbox (`native_sandbox`).

use async_trait::async_trait;
use tracing::{info, warn};

use duduclaw_core::types::sandbox_level_for;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Hard backstop on the whole subprocess.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Cap the system prompt embedded into the prompt argument (ARG_MAX safety).
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;

/// Runtime that delegates to the xAI Grok CLI.
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

/// Resolve the `grok` binary. Prefers the official `grok` (fallback third-party
/// `grok-cli`) on `$PATH`, then the usual HOME-rooted install locations. The
/// availability probe ultimately decides whether the runtime registers, so a
/// stale guess here is harmless.
///
/// UNVERIFIED (R4): official "Grok Build" binary name assumed to be `grok`.
fn resolve_grok_path() -> String {
    duduclaw_core::which_grok().unwrap_or_else(|| "grok".to_string())
}

/// Build the prompt payload: system instructions + history + user message, all
/// embedded as text. Pure function so it is unit-testable without spawning the
/// CLI. Mirrors the antigravity backend (no verified `--system` flag).
fn build_prompt(context: &RuntimeContext, user_prompt: &str) -> String {
    // Limit system_prompt to 64KB to avoid ARG_MAX issues.
    // Char-boundary-safe truncation (never raw byte-index slicing on
    // potentially CJK/emoji content — 2026-06 review convention #1).
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

        // Capability enforcement is best-effort on this runtime: there is no
        // verified CLI sandbox flag to translate `CapabilitiesConfig` into
        // (UNVERIFIED #8). Log the derived level so operators can see the gap;
        // the only hard confinement is the opt-in native OS sandbox below.
        let caps = context.capabilities.as_ref();
        if let Some(c) = caps {
            if c.has_tool_restrictions() {
                warn!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    level = ?sandbox_level_for(caps),
                    "capability enforcement is best-effort on the Grok runtime — \
                     no verified CLI sandbox flag; relying on native_sandbox (if enabled)"
                );
            }
        }

        // MCP wiring: register the duduclaw MCP server before spawning. Grok is
        // MCP-native, but the config path is UNVERIFIED (#6) — we write the most
        // likely location. Warn-not-fatal: registration failing must not block
        // the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) = Self::ensure_duduclaw_mcp_config(dir, &context.agent_id).await {
                warn!(
                    runtime = "grok",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write grok MCP settings — continuing without it"
                );
            }
        }

        let payload = build_prompt(context, prompt);

        let mut cmd = tokio::process::Command::new(&self.grok_path);

        // Model first (UNVERIFIED #3: `--model`, not `-m`).
        if !context.model.is_empty() {
            cmd.arg("--model").arg(&context.model);
        }

        // Working directory.
        if let Some(ref dir) = context.agent_dir {
            cmd.current_dir(dir);
        }

        // Prompt LAST as the value/positional of `-p` (UNVERIFIED #2 — safe under
        // both value-consuming and boolean+positional interpretations).
        cmd.arg("-p").arg(&payload);

        // Forward the xAI API key when set (UNVERIFIED #7 — for the api-key CLI;
        // SuperGrok OAuth is phase 2).
        let api_key = std::env::var("XAI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("XAI_API_KEY", &api_key);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). This is the only enforceable confinement on
        // this runtime; fail-closed if required but unavailable.
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

        // Grok's headless mode exposes no verified usage stats (UNVERIFIED #5), so
        // estimate with the gateway's shared CJK-aware heuristic. These feed
        // CostTelemetry as approximations. If Grok ships a structured/JSON mode,
        // replace with real counts.
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
        tokio::process::Command::new(&self.grok_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
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
    /// Write MCP server configuration to Grok settings.
    ///
    /// If `agent_dir` is provided, writes to `agent_dir/.grok/settings.json` for
    /// per-agent isolation. Otherwise writes to the global `~/.grok/settings.json`.
    ///
    /// UNVERIFIED (R4 #6): the real Grok CLI MCP config path is unconfirmed; this
    /// mirrors Gemini's `settings.json` `mcpServers` shape as the most likely
    /// location. Merges per server name (other entries preserved) and is
    /// idempotent: returns `Ok(false)` without writing when every requested entry
    /// already matches.
    pub async fn write_mcp_config(
        agent_dir: Option<&std::path::Path>,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let settings_path = if let Some(dir) = agent_dir {
            dir.join(".grok").join("settings.json")
        } else {
            dirs::home_dir()
                .ok_or("No home dir")?
                .join(".grok")
                .join("settings.json")
        };
        let existing = tokio::fs::read_to_string(&settings_path)
            .await
            .unwrap_or_else(|_| "{}".to_string());
        let mut settings: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or(serde_json::json!({}));
        if !settings.is_object() {
            settings = serde_json::json!({});
        }
        let mcp = settings
            .as_object_mut()
            .expect("settings is an object — normalized above")
            .entry("mcpServers")
            .or_insert(serde_json::json!({}));
        if !mcp.is_object() {
            *mcp = serde_json::json!({});
        }
        let map = mcp.as_object_mut().expect("mcpServers normalized to object");
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
        if let Some(parent) = settings_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Ensure the duduclaw MCP server (absolute binary + `mcp-server` arg +
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's `.grok/settings.json`.
    /// Called before every spawn; idempotent.
    pub async fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
    ) -> Result<bool, String> {
        let Some(def) = super::duduclaw_mcp_server_json(agent_id) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), def);
        Self::write_mcp_config(Some(agent_dir), &servers).await
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
    async fn mcp_config_write_merges_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join(".grok").join("settings.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        // Pre-existing user settings with an unrelated key + another MCP server.
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "theme": "dark",
                "mcpServers": {
                    "playwright": { "command": "npx", "args": ["mcp-playwright"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "duduclaw".to_string(),
            serde_json::json!({
                "command": "/usr/local/bin/duduclaw",
                "args": ["mcp-server"],
                "env": { "DUDUCLAW_AGENT_ID": "agnes" },
            }),
        );
        assert!(GrokRuntime::write_mcp_config(Some(dir.path()), &servers)
            .await
            .unwrap());
        // Second call: entry already matches → no write.
        assert!(!GrokRuntime::write_mcp_config(Some(dir.path()), &servers)
            .await
            .unwrap());

        let got: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(got["theme"], "dark", "unrelated settings preserved");
        assert_eq!(
            got["mcpServers"]["playwright"]["command"], "npx",
            "other MCP servers preserved"
        );
        assert_eq!(
            got["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"], "agnes",
            "duduclaw entry carries the agent identity env"
        );
    }
}
