//! Night Engine live LLM adapter — wires the N1/N2 [`NightLlm`] seam to a real
//! model.
//!
//! The scaffold in `night_engine.rs` drives everything (budgets, prompts,
//! caching, circuit breaker) behind the [`NightLlm`] trait; this module is the
//! production implementation. Call path mirrors the channel-reply chain:
//!
//! 1. **Rotated Claude CLI** — [`crate::channel_reply::call_claude_cli_rotated`]
//!    (AccountRotator selects OAuth/API-key accounts, cooldowns, failure
//!    classification).
//! 2. **Direct Anthropic API** — [`crate::direct_api::call_direct_api`] with the
//!    stored API key, used only when the CLI path fails.
//!
//! The model is the agent's *utility* model (`agent.toml [model] utility`,
//! default `claude-haiku-4-5`) — night compute always runs on the cheapest
//! suitable tier, never the agent's preferred conversational model.
//!
//! ## Gating (fail-safe, default OFF)
//!
//! Live night LLM calls require **both**:
//! - the agent's `[night_engine] enabled = true` (existing per-agent gate — the
//!   scheduler only reaches this module for enabled agents), and
//! - the operator-level knob in `<home>/config.toml`:
//!
//!   ```toml
//!   [night]
//!   llm_enabled = true   # default false
//!   ```
//!
//! When the knob is absent/false/malformed, [`build_night_llm`] returns `None`
//! and the scheduler passes `None` to the orchestrator — byte-identical to the
//! pre-wiring scaffold behaviour (N3/N4 still run; N1/N2 no-op with a note).
//! A missing or unreadable `config.toml` is treated as disabled, never an error.
//!
//! Per-call spend accounting happens in the orchestrator: every call is checked
//! against the persistent [`crate::night_engine::DailyCircuitBreaker`] before
//! spawn and its estimated cost is recorded after.

use std::path::{Path, PathBuf};

use duduclaw_core::truncate_chars;
use tracing::{debug, warn};

use crate::night_engine::{NightInference, NightLlm};

/// Read the operator-level `[night] llm_enabled` knob from `<home>/config.toml`.
///
/// Lenient read (same pattern as `runtime_config::read_global_config`): a
/// missing file, unparseable TOML, missing table/key, or non-bool value all
/// mean **disabled** — the knob can only ever turn the feature on explicitly.
pub fn night_llm_enabled(home_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return false;
    };
    let Ok(v) = text.parse::<toml::Value>() else {
        return false;
    };
    v.get("night")
        .and_then(|n| n.get("llm_enabled"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
}

/// Build the live N1/N2 adapter for one agent, or `None` when live calls are
/// not permitted (knob off, or neither LLM sub-pass is enabled in the agent's
/// night config). `None` keeps the orchestrator on the scaffold no-op path.
pub fn build_night_llm(
    home_dir: &Path,
    agent_id: &str,
    cfg: &duduclaw_core::types::NightEngineConfig,
) -> Option<RotatedNightLlm> {
    if !cfg.sleep_time && !cfg.prefetch {
        return None; // no LLM sub-pass wants a model
    }
    if !night_llm_enabled(home_dir) {
        return None; // operator knob off (default) → scaffold behaviour
    }
    let agent_dir = home_dir.join("agents").join(agent_id);
    let model = crate::runtime_config::agent_utility_model(&agent_dir);
    Some(RotatedNightLlm::new(
        home_dir.to_path_buf(),
        agent_id.to_string(),
        model,
    ))
}

// ── Cost estimation (pure) ────────────────────────────────────

// Haiku-class pricing in true millicents per MTok ($1/MTok in, $5/MTok out).
// Night compute always targets the utility (haiku-class) tier, so these rates
// are the right ceiling; if an operator points `[model] utility` at a pricier
// model the daily circuit breaker still bounds total spend.
const INPUT_MILLICENTS_PER_MTOK: u64 = 100_000;
const OUTPUT_MILLICENTS_PER_MTOK: u64 = 500_000;

/// Estimate the cost of one night call in true millicents (1/1000 cent) from
/// token counts. Rounds up; never returns 0 so every real call charges the
/// budget at least a sliver.
pub fn night_cost_from_tokens(input_tokens: u64, output_tokens: u64) -> u64 {
    let raw = input_tokens.saturating_mul(INPUT_MILLICENTS_PER_MTOK)
        + output_tokens.saturating_mul(OUTPUT_MILLICENTS_PER_MTOK);
    (raw.saturating_add(999_999) / 1_000_000).max(1)
}

/// Estimate the cost of one night call from raw text (CLI path — the CLI does
/// not return token usage here). CJK-aware token heuristic.
pub fn estimate_night_cost_millicents(input_text: &str, output_text: &str) -> u64 {
    let in_tok = crate::cost_telemetry::estimate_tokens(input_text);
    let out_tok = crate::cost_telemetry::estimate_tokens(output_text);
    night_cost_from_tokens(in_tok, out_tok)
}

// ── Night capabilities (locked down) ──────────────────────────

/// Locked-down [`CapabilitiesConfig`] for night compute.
///
/// N1/N2 are pure reasoning over pre-gathered memory snippets — they need
/// **zero tools**. The CLI spawn path runs with
/// `--dangerously-skip-permissions` and the full MCP surface, and
/// `capabilities: None` used to mean *default* caps (no `--allowedTools`
/// restriction at all). `allowed_tools()` treats an empty vec as "no
/// allowlist", so we pin the allowlist to a single inert sentinel that
/// matches no real tool: Claude CLI enters allowlist mode and every actual
/// tool falls outside it. Defense in depth: the write/exec/network surface is
/// also bare-denied, and `computer` stays denied via the default
/// `computer_use = false` flag.
pub fn night_capabilities() -> duduclaw_core::types::CapabilitiesConfig {
    duduclaw_core::types::CapabilitiesConfig {
        // Allowlist mode with a sentinel that names no real tool ⇒ no tools.
        allowed_tools: vec!["duduclaw_night_no_tools".to_string()],
        // Belt-and-suspenders denylist for runtimes that only honour denies.
        denied_tools: vec![
            "Bash".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
            "MultiEdit".to_string(),
            "NotebookEdit".to_string(),
            "WebFetch".to_string(),
            "WebSearch".to_string(),
            "Task".to_string(),
        ],
        ..Default::default()
    }
}

// ── Production adapter ────────────────────────────────────────

/// [`NightLlm`] implementation over the gateway's existing rotated Claude CLI
/// path with Direct-API fallback. Holds no connection state — each `infer`
/// goes through the shared cached rotator / HTTP client singletons.
pub struct RotatedNightLlm {
    home_dir: PathBuf,
    agent_id: String,
    model: String,
}

impl RotatedNightLlm {
    pub fn new(home_dir: PathBuf, agent_id: String, model: String) -> Self {
        Self {
            home_dir,
            agent_id,
            model,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[async_trait::async_trait]
impl NightLlm for RotatedNightLlm {
    async fn infer(&self, system: &str, user: &str) -> Result<NightInference, String> {
        // Night compute is pure reasoning: spawn with an explicit zero-tools
        // allowlist. `None` here would mean DEFAULT caps — i.e. the full tool
        // + MCP surface under --dangerously-skip-permissions.
        let caps = night_capabilities();
        // ── 1) Rotated Claude CLI (subscription quota first) ──
        let cli_err = match crate::channel_reply::call_claude_cli_rotated(
            user,
            &self.model,
            system,
            &self.home_dir,
            None,        // work_dir — night passes never touch a workspace
            None,        // on_progress — background task, no live progress surface
            Some(&caps), // capabilities — locked down: night needs NO tools
            None,        // session_id — single-shot, no multi-turn session
            &[],         // conversation_history — prompts are self-contained
        )
        .await
        {
            Ok(text) if !text.trim().is_empty() => {
                let cost = estimate_night_cost_millicents(&format!("{system}\n{user}"), &text);
                debug!(
                    agent = %self.agent_id,
                    model = %self.model,
                    cost_millicents = cost,
                    "night LLM call succeeded via rotated CLI"
                );
                return Ok(NightInference {
                    text,
                    cost_millicents: cost,
                });
            }
            Ok(_) => "empty CLI response".to_string(),
            Err(e) => truncate_chars(&e, 200),
        };

        // ── 2) Direct API fallback (paid, only if an API key is stored) ──
        let api_key = crate::claude_runner::get_api_key_from_home(&self.home_dir).await;
        if api_key.is_empty() {
            return Err(format!(
                "night LLM: CLI path failed ({cli_err}); no API key for direct fallback"
            ));
        }
        warn!(
            agent = %self.agent_id,
            error = %cli_err,
            "night LLM: rotated CLI failed, falling back to Direct API"
        );
        match crate::direct_api::call_direct_api(&api_key, &self.model, system, user, &[]).await {
            Ok(resp) if !resp.text.trim().is_empty() => {
                let cost = match &resp.usage {
                    Some(u) => night_cost_from_tokens(
                        u.input_tokens + u.cache_read_tokens + u.cache_creation_tokens,
                        u.output_tokens,
                    ),
                    None => {
                        estimate_night_cost_millicents(&format!("{system}\n{user}"), &resp.text)
                    }
                };
                Ok(NightInference {
                    text: resp.text,
                    cost_millicents: cost,
                })
            }
            Ok(_) => Err(format!(
                "night LLM: CLI failed ({cli_err}); direct API returned empty response"
            )),
            Err(e) => Err(format!(
                "night LLM: CLI failed ({cli_err}); direct API failed ({})",
                truncate_chars(&e, 200)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::NightEngineConfig;

    fn write_config(dir: &Path, body: &str) {
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    // ── knob (fail-safe: only an explicit true enables) ──
    #[test]
    fn knob_missing_file_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!night_llm_enabled(dir.path()));
    }

    #[test]
    fn knob_explicit_true_enables() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night]\nllm_enabled = true\n");
        assert!(night_llm_enabled(dir.path()));
    }

    #[test]
    fn knob_explicit_false_disables() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night]\nllm_enabled = false\n");
        assert!(!night_llm_enabled(dir.path()));
    }

    #[test]
    fn knob_malformed_toml_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night\nllm_enabled = true");
        assert!(!night_llm_enabled(dir.path()));
    }

    #[test]
    fn knob_wrong_type_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night]\nllm_enabled = \"yes\"\n");
        assert!(!night_llm_enabled(dir.path()));
    }

    // ── builder gating ──
    #[test]
    fn build_returns_none_when_knob_off() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NightEngineConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(build_night_llm(dir.path(), "a", &cfg).is_none());
    }

    #[test]
    fn build_returns_none_when_no_llm_subpass() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night]\nllm_enabled = true\n");
        let cfg = NightEngineConfig {
            enabled: true,
            sleep_time: false,
            prefetch: false,
            ..Default::default()
        };
        assert!(build_night_llm(dir.path(), "a", &cfg).is_none());
    }

    #[test]
    fn build_returns_adapter_with_utility_model_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[night]\nllm_enabled = true\n");
        let cfg = NightEngineConfig {
            enabled: true,
            ..Default::default()
        };
        let adapter = build_night_llm(dir.path(), "a", &cfg).expect("adapter built");
        // No agent.toml → utility-model default (haiku-class).
        assert_eq!(adapter.model(), "claude-haiku-4-5");
    }

    // ── HIGH-E: locked-down night capabilities ──
    #[test]
    fn night_caps_engage_allowlist_mode_with_no_real_tools() {
        let caps = night_capabilities();
        // Allowlist mode must be engaged (empty vec would mean "no allowlist"
        // → full default surface), and it must name no real tool.
        let allowed = caps.allowed_tools();
        assert!(!allowed.is_empty(), "allowlist mode must be engaged");
        assert_eq!(allowed, vec!["duduclaw_night_no_tools".to_string()]);
        // Write/exec tools are unavailable both via the allowlist and the deny.
        assert!(!caps.write_tools_allowed(), "night must not have write tools");
        let denied = caps.disallowed_tools();
        for t in ["Bash", "Write", "Edit", "WebFetch", "WebSearch", "Task", "computer"] {
            assert!(
                denied.iter().any(|d| d == t),
                "{t} must be in the night denylist: {denied:?}"
            );
        }
    }

    // ── cost estimation ──
    #[test]
    fn cost_from_tokens_haiku_rates() {
        // 1M input + 1M output = 100_000 + 500_000 millicents = $6.00.
        assert_eq!(night_cost_from_tokens(1_000_000, 1_000_000), 600_000);
        // Tiny call still charges at least 1 millicent.
        assert_eq!(night_cost_from_tokens(0, 0), 1);
        assert!(night_cost_from_tokens(10, 10) >= 1);
    }

    #[test]
    fn cost_estimate_is_cjk_aware_and_nonzero() {
        let cheap = estimate_night_cost_millicents("hi", "ok");
        let cjk = estimate_night_cost_millicents(&"記憶整理".repeat(2000), &"好".repeat(2000));
        assert!(cheap >= 1);
        assert!(
            cjk > cheap,
            "CJK-heavy call must cost more: {cjk} vs {cheap}"
        );
    }

    // ── LIVE verification (ignored by default; run manually) ──
    //
    //   cargo test -p duduclaw-gateway --lib night_llm::tests::live_haiku_call \
    //     -- --ignored --nocapture
    //
    // Requires the real `claude` CLI + a configured account under ~/.duduclaw
    // (or ambient CLI auth). Performs ONE tiny haiku-class call through the
    // exact production adapter path.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live network + claude CLI — run manually"]
    async fn live_haiku_call_via_adapter() {
        let home = PathBuf::from(std::env::var("HOME").expect("HOME set")).join(".duduclaw");
        let llm = RotatedNightLlm::new(home, "night-live-test".to_string(), "haiku".to_string());
        let inf = llm
            .infer(
                "You are a terse test probe. Answer in one word.",
                "Reply with exactly the word: pong",
            )
            .await
            .expect("live haiku call should succeed");
        println!(
            "LIVE night LLM output: {:?} (cost {} millicents)",
            inf.text, inf.cost_millicents
        );
        assert!(!inf.text.trim().is_empty(), "live call must return text");
        assert!(inf.cost_millicents >= 1, "live call must charge something");
    }
}
