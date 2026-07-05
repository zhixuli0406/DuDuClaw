//! Google Antigravity CLI runtime — `agy -p <prompt>`.
//!
//! Background: on 2026-06-18 Google retired the personal-tier `gemini` CLI and
//! replaced it with the **Antigravity CLI** (`agy`), a Go single-binary that
//! supersedes `gemini-cli`. It shares the `~/.gemini/` lineage (config now under
//! `~/.gemini/antigravity-cli/`) and exposes Gemini 3.x plus Claude / GPT-OSS
//! models behind one terminal agent. This runtime drives that binary.
//!
//! Differences from [`super::gemini::GeminiRuntime`] (the legacy backend, kept for
//! paid `GEMINI_API_KEY` users whose access continues past the shutdown):
//!   - binary `agy` (installed to `~/.local/bin/agy`), not `gemini`
//!   - model flag `--model <id>`, not `-m <id>`
//!   - permission bypass `--dangerously-skip-permissions`, not `--approval-mode yolo`
//!   - API key env `ANTIGRAVITY_API_KEY`, not `GEMINI_API_KEY`
//!   - MCP config under `~/.gemini/antigravity-cli/settings.json`
//!
//! Verified against `agy --help` + live runs (v1.0.12, 2026-06-25). Confirmed facts:
//!   - `-p` / `--print` takes the prompt as its **value** (not a boolean) and
//!     must be the LAST flag — it consumes the next argv token as the prompt, so
//!     any flag after `-p` is swallowed as the prompt. All other flags go first.
//!   - `--dangerously-skip-permissions` auto-approves all tool permission
//!     requests without prompting — required since a subprocess has no TTY.
//!   - `--model <id>` selects the session model; `--add-dir <path>` adds a
//!     workspace dir (we point it at the agent home so `agy` does not silently
//!     create a default `~/.gemini/antigravity-cli/scratch/` workspace).
//!   - `--print-timeout` bounds print-mode wait (CLI default 5m). We set it
//!     explicitly and keep the wrapper timeout a notch higher as a backstop.
//!   - There is **no** `--output-format`/JSON surface and **no** `--system`
//!     flag, so we capture plain stdout text (token stats are unavailable → 0)
//!     and embed the system prompt + history *inside the prompt argument*,
//!     guaranteeing the model receives them. The 64KB system-prompt cap keeps
//!     the argv well under ARG_MAX.

use async_trait::async_trait;
use tracing::info;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Hard backstop on the whole subprocess. Kept a notch above `PRINT_TIMEOUT`
/// (agy's own print-mode wait) so agy self-bounds first and this only fires if
/// the process truly wedges.
const DEFAULT_TIMEOUT_SECS: u64 = 330;
/// Value passed to `agy --print-timeout` (agy's CLI default is 5m).
const PRINT_TIMEOUT: &str = "300s";
/// Cap the system prompt embedded into the prompt argument (ARG_MAX safety).
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;

/// Runtime that delegates to the Google Antigravity CLI (`agy`).
pub struct AntigravityRuntime {
    agy_path: String,
}

impl AntigravityRuntime {
    pub fn new() -> Self {
        Self {
            agy_path: resolve_agy_path(),
        }
    }
}

impl Default for AntigravityRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the `agy` binary. Prefer a bare `agy` on `$PATH`; fall back to the
/// documented install location `~/.local/bin/agy` so launchd/systemd-launched
/// gateways (which often lack the interactive `PATH`) still discover it. The
/// availability probe ultimately decides whether the runtime registers, so a
/// stale guess here is harmless.
fn resolve_agy_path() -> String {
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local").join("bin").join("agy");
        if local.is_file() {
            return local.to_string_lossy().into_owned();
        }
    }
    "agy".to_string()
}

/// Idempotently add `dir` to agy's `trustedWorkspaces` so that running there does
/// not trigger the interactive "trust this workspace?" prompt (which would hang a
/// headless subprocess). Writes the global `~/.gemini/antigravity-cli/settings.json`
/// under a cross-process lock (multiple agents may share it). Best-effort: any IO
/// error is returned for the caller to log, never to abort the agent call.
fn ensure_workspace_trusted(dir: &std::path::Path) -> std::io::Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let settings_path = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json");
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Canonicalize so the stored path matches what agy compares against.
    let target = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned();

    duduclaw_core::with_file_lock(&settings_path, || {
        let existing =
            std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".to_string());
        let mut settings: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        let mut list: Vec<serde_json::Value> = settings
            .get("trustedWorkspaces")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if list.iter().any(|v| v.as_str() == Some(target.as_str())) {
            return Ok(()); // already trusted — no write
        }
        list.push(serde_json::Value::String(target.clone()));
        settings["trustedWorkspaces"] = serde_json::Value::Array(list);
        let out = serde_json::to_string_pretty(&settings).unwrap_or_default();
        std::fs::write(&settings_path, out)
    })
}

/// Build the prompt payload: system instructions + history + user message, all
/// embedded as text. Pure function so it is unit-testable without spawning `agy`.
fn build_prompt(context: &RuntimeContext, user_prompt: &str) -> String {
    // Limit system_prompt to 64KB to avoid ARG_MAX issues.
    let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        // Walk back to a char boundary so multi-byte CJK/emoji never split.
        let cut = duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES);
        cut
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
        let safe_system = system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
        format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{with_history}")
    }
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for AntigravityRuntime {
    fn name(&self) -> &str {
        "antigravity"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "AntigravityRuntime: executing via agy -p");

        // W1: agy exposes NO sandbox / approval-policy / tool-list flags
        // (verified against `agy --help` v1.0.12 — see module docs), so the
        // agent's CapabilitiesConfig cannot be enforced on this runtime at
        // all. Surface that loudly once per spawn so operators aren't
        // silently unprotected.
        if context.capabilities.is_some() {
            tracing::warn!(
                runtime = "antigravity",
                agent = %context.agent_id,
                "capability enforcement unavailable on this runtime — agy has no \
                 sandbox/approval flags; spawning with --dangerously-skip-permissions"
            );
        }

        // W2 (MCP wiring): register the duduclaw MCP server in the agent's
        // antigravity settings before spawning. Idempotent merge;
        // warn-not-fatal — registration failing must not block the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) = Self::ensure_duduclaw_mcp_config(dir, &context.agent_id).await {
                tracing::warn!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write antigravity MCP settings — continuing without it"
                );
            }
        }

        let payload = build_prompt(context, prompt);

        let mut cmd = tokio::process::Command::new(&self.agy_path);
        // CRITICAL ordering: `-p`/`--print` is NOT a boolean — it consumes the
        // *next argv token* as the prompt value (verified: `agy -p` alone errors
        // "flag needs an argument: -p"). So every other flag MUST come first and
        // `-p <payload>` MUST be last; otherwise `-p` swallows the following flag
        // as the prompt and the real payload is dropped (the cause of agy
        // "answering" about whatever flag followed `-p`).
        //
        // `--dangerously-skip-permissions` auto-approves tool calls (a subprocess
        // has no TTY to confirm at). `--print-timeout` bounds agy's own wait.
        cmd.arg("--dangerously-skip-permissions")
            .arg("--print-timeout")
            .arg(PRINT_TIMEOUT);

        // Set model if specified (agy uses `--model`, not `-m`).
        if !context.model.is_empty() {
            cmd.arg("--model").arg(&context.model);
        }

        // Point agy at the agent home as its workspace so it does not silently
        // spin up a default `~/.gemini/antigravity-cli/scratch/` project.
        //
        // CRITICAL: agy shows an *interactive* "trust this workspace?" prompt for
        // any dir not in `trustedWorkspaces`. In a headless subprocess (no TTY)
        // that prompt blocks forever — `--dangerously-skip-permissions` only
        // auto-approves *tool* calls, not workspace trust. So we pre-seed the
        // agent dir into agy's settings before spawning. Best-effort: a failure
        // here just risks the prompt, it must not abort the call.
        if let Some(ref dir) = context.agent_dir {
            let d = dir.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || ensure_workspace_trusted(&d)).await {
                tracing::warn!(agent = %context.agent_id, error = %e, "ensure_workspace_trusted join failed");
            }
            cmd.arg("--add-dir").arg(dir);
            cmd.current_dir(dir);
        }

        // Prompt LAST, as the value of `-p` (see ordering note above).
        cmd.arg("-p").arg(&payload);

        // Pass API key if available (Antigravity's own env var).
        let api_key = std::env::var("ANTIGRAVITY_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("ANTIGRAVITY_API_KEY", &api_key);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Antigravity CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn agy: {e}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Antigravity CLI exited with {code}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let content = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // agy's print mode exposes no usage stats (no JSON surface), so we
        // estimate with the gateway's shared CJK-aware heuristic. These feed
        // CostTelemetry as approximations — the input estimate is the payload we
        // sent (excludes agy's own injected context) and the output the captured
        // text. If agy ever ships a structured/JSON mode, replace with real counts.
        let input_tokens = crate::prompt_compression::estimate_tokens(&payload);
        let output_tokens = crate::prompt_compression::estimate_tokens(&content);

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "antigravity".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.agy_path)
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

impl AntigravityRuntime {
    /// Execute and return chunks. `agy -p` is request/response, so this wraps the
    /// normal execution into a single `Done` chunk (mirrors the Gemini backend).
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

impl AntigravityRuntime {
    /// Write MCP server configuration to Antigravity settings.
    ///
    /// If `agent_dir` is provided, writes to
    /// `agent_dir/.gemini/antigravity-cli/settings.json` for per-agent isolation.
    /// Otherwise writes to the global `~/.gemini/antigravity-cli/settings.json`.
    ///
    /// Merges per server name (other `mcpServers` entries and unrelated settings —
    /// e.g. `trustedWorkspaces` — are preserved) and is idempotent: returns
    /// `Ok(false)` without writing when every requested entry already matches.
    pub async fn write_mcp_config(
        agent_dir: Option<&std::path::Path>,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let settings_path = if let Some(dir) = agent_dir {
            dir.join(".gemini").join("antigravity-cli").join("settings.json")
        } else {
            dirs::home_dir()
                .ok_or("No home dir")?
                .join(".gemini")
                .join("antigravity-cli")
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
            tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
        }
        tokio::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// W2: ensure the duduclaw MCP server (absolute binary + `mcp-server` arg +
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's antigravity
    /// settings. Called before every spawn; idempotent.
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
        let c = ctx("You are helpful.", "gemini-3-pro");
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
        assert!(out.starts_with(' '), "leading dash must be neutralized: {out:?}");
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

    /// End-to-end against the real `agy` binary. Ignored by default (needs the
    /// CLI installed + authenticated). Run with:
    ///   DUDUCLAW_AGY_E2E=1 cargo test -p duduclaw-gateway --lib \
    ///     antigravity::tests::e2e_real_agy -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a live, authenticated `agy` CLI"]
    async fn e2e_real_agy() {
        if std::env::var("DUDUCLAW_AGY_E2E").as_deref() != Ok("1") {
            eprintln!("set DUDUCLAW_AGY_E2E=1 to run this e2e");
            return;
        }
        let rt = AntigravityRuntime::new();
        assert!(rt.is_available().await, "agy not found on PATH/~/.local/bin");

        let dir = std::env::temp_dir().join("duduclaw-agy-e2e");
        let _ = std::fs::create_dir_all(&dir);
        let c = RuntimeContext {
            agent_dir: Some(dir),
            system_prompt: "You are a terse echo bot. Reply with one word only.".to_string(),
            model: String::new(),
            max_tokens: 256,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "e2e".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
        };
        let resp = rt
            .execute("Reply with exactly: PONG", &c)
            .await
            .expect("agy execute failed");
        eprintln!("agy responded: {:?}", resp.content);
        assert!(!resp.content.is_empty(), "empty response from agy");
        assert_eq!(resp.runtime_name, "antigravity");
    }
}
