//! Periodic auto-run scheduler for the Rollout-to-Skill synthesis pipeline (W19-P1).
//!
//! Makes "conversation → skill" extraction *autonomous*: instead of waiting for
//! an operator to invoke the `skill_synthesis_run` MCP tool, the gateway runs
//! the pipeline on a fixed interval (default daily).
//!
//! ## Safety posture (W19 conservative rollout)
//! - **Off by default.** The scheduler only runs the pipeline when
//!   `config.toml [skill_synthesis] auto_run = true`.
//! - **Dry-run by default even when enabled.** Set `dry_run = false` to allow
//!   real Skill Bank writes. Mirrors [`PipelineConfig`]'s safe default.
//! - **Hot config.** The enable flag and parameters are re-read every poll, so
//!   an operator can flip `auto_run` without restarting the gateway.
//! - **Non-blocking.** Pipeline errors are captured into the run summary and
//!   logged; they never abort the scheduler loop.
//!
//! ## Config (`config.toml`)
//! ```toml
//! [skill_synthesis]
//! auto_run = true        # master switch (default: false)
//! dry_run = true         # score+log only, no Skill Bank writes (default: true)
//! interval_hours = 24    # how often to run (default: 24, min: 1)
//! lookback_days = 1      # days of EvolutionEvents JSONL to scan (default: 1, max: 30)
//! target_agent = "dudu"  # owner of synthesized skills (default: [general] default_agent)
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

use super::pipeline::{self, PipelineConfig};

/// Re-evaluate config (and thus pick up `auto_run` / `interval_hours` changes)
/// at most this often, even when the configured interval is longer.
const POLL: Duration = Duration::from_secs(1800); // 30 min

/// Let bots / registry settle before the first scan after a gateway start.
const STARTUP_SETTLE: Duration = Duration::from_secs(120);

// ── Config ───────────────────────────────────────────────────────────────────

/// Parsed `[skill_synthesis]` scheduler settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SynthesisScheduleConfig {
    /// Master switch — when `false`, the scheduler never runs the pipeline.
    pub auto_run: bool,
    /// When `true`, score+log only (no Skill Bank writes).
    pub dry_run: bool,
    /// Interval between runs, in hours (>= 1).
    pub interval_hours: u64,
    /// Days of EvolutionEvents JSONL to scan per run (1..=30).
    pub lookback_days: u32,
    /// Agent that owns synthesized skills. `None` → resolve `default_agent`.
    pub target_agent: Option<String>,
}

impl Default for SynthesisScheduleConfig {
    fn default() -> Self {
        Self {
            auto_run: false,
            dry_run: true,
            interval_hours: 24,
            lookback_days: 1,
            target_agent: None,
        }
    }
}

impl SynthesisScheduleConfig {
    /// Parse `[skill_synthesis]` from a raw `config.toml` body. Unknown or
    /// malformed input fails safe to [`Default`] (auto_run = false).
    pub fn from_config_str(content: &str) -> Self {
        let mut cfg = Self::default();
        let table: toml::Value = match content.parse() {
            Ok(t) => t,
            Err(_) => return cfg,
        };
        let Some(s) = table.get("skill_synthesis") else {
            return cfg;
        };
        if let Some(v) = s.get("auto_run").and_then(|v| v.as_bool()) {
            cfg.auto_run = v;
        }
        if let Some(v) = s.get("dry_run").and_then(|v| v.as_bool()) {
            cfg.dry_run = v;
        }
        if let Some(v) = s.get("interval_hours").and_then(|v| v.as_integer()) {
            if v >= 1 {
                cfg.interval_hours = v as u64;
            }
        }
        if let Some(v) = s.get("lookback_days").and_then(|v| v.as_integer()) {
            if v >= 1 {
                cfg.lookback_days = (v as u32).min(30);
            }
        }
        if let Some(v) = s.get("target_agent").and_then(|v| v.as_str()) {
            let t = v.trim();
            if !t.is_empty() {
                cfg.target_agent = Some(t.to_string());
            }
        }
        cfg
    }

    /// Load from `<home>/config.toml`; absent/unreadable file → safe default.
    pub fn load_from_home(home_dir: &Path) -> Self {
        match std::fs::read_to_string(home_dir.join("config.toml")) {
            Ok(c) => Self::from_config_str(&c),
            Err(_) => Self::default(),
        }
    }
}

// ── Scheduler loop ───────────────────────────────────────────────────────────

/// Spawn the periodic synthesis scheduler as a background task.
///
/// Returns the [`JoinHandle`](tokio::task::JoinHandle) so the gateway can track
/// it for graceful shutdown alongside its other background tasks.
pub fn spawn(home_dir: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_periodic(home_dir))
}

/// The scheduler loop. Polls every [`POLL`] interval; runs the pipeline when
/// `auto_run` is enabled and the configured `interval_hours` has elapsed since
/// the last run.
async fn run_periodic(home_dir: PathBuf) {
    tokio::time::sleep(STARTUP_SETTLE).await;
    info!(
        target: "skill_synthesis",
        "Skill synthesis auto-run scheduler online (gated by config [skill_synthesis] auto_run)"
    );

    let mut last_run: Option<std::time::Instant> = None;
    loop {
        let cfg = SynthesisScheduleConfig::load_from_home(&home_dir);
        if cfg.auto_run {
            let interval = Duration::from_secs(cfg.interval_hours.saturating_mul(3600).max(3600));
            let due = last_run.map(|t| t.elapsed() >= interval).unwrap_or(true);
            if due {
                run_once(&home_dir, &cfg).await;
                last_run = Some(std::time::Instant::now());
            }
        }

        // G5 curator: deterministic skill-maintenance pass. Independently
        // gated by `[skill_curator] enabled` + its own 24h guard; errors are
        // logged inside and never abort this loop.
        crate::skill_lifecycle::curator::maybe_run(&home_dir).await;

        tokio::time::sleep(POLL).await;
    }
}

/// Execute a single pipeline run for the configured target agent.
async fn run_once(home_dir: &Path, cfg: &SynthesisScheduleConfig) {
    let target_agent = cfg
        .target_agent
        .clone()
        .unwrap_or_else(|| resolve_default_agent(home_dir));

    // WP1 master kill-switch: skill synthesis is an autonomous evolution path,
    // so a frozen target agent (`[evolution] enabled = false`) must be a no-op
    // even when the global `[skill_synthesis] auto_run` is on.
    let agent_dir = home_dir.join("agents").join(&target_agent);
    if !duduclaw_core::evolution_master_enabled(&agent_dir) {
        info!(
            target: "skill_synthesis",
            agent = %target_agent,
            "Auto-run skipped: [evolution] enabled = false (master switch off)"
        );
        return;
    }

    let api_key = resolve_api_key(home_dir);
    if !cfg.dry_run && api_key.is_none() {
        warn!(
            target: "skill_synthesis",
            agent = %target_agent,
            "Auto-run is live (dry_run=false) but no ANTHROPIC_API_KEY / [api] anthropic_api_key \
             is configured — skipping run to avoid silent no-op graduation"
        );
        return;
    }

    let events_dir = std::env::var("EVOLUTION_EVENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir.join("evolution").join("events"));

    let pconfig = PipelineConfig {
        events_dir,
        lookback_days: cfg.lookback_days,
        dry_run: cfg.dry_run,
        api_key,
        home_dir: home_dir.to_path_buf(),
        target_agent_id: target_agent.clone(),
        ..Default::default()
    };

    info!(
        target: "skill_synthesis",
        agent = %target_agent,
        dry_run = cfg.dry_run,
        lookback_days = cfg.lookback_days,
        "Auto-run: starting synthesis pipeline"
    );
    let run = pipeline::run(&pconfig).await;
    info!(
        target: "skill_synthesis",
        summary = %run.summary(),
        "Auto-run: synthesis pipeline complete"
    );
}

// ── Config resolution helpers ────────────────────────────────────────────────

/// Resolve the default agent id: `DUDUCLAW_AGENT_ID` env → `config.toml
/// [general] default_agent` → `"dudu"`. Mirrors `mcp.rs::get_default_agent`.
fn resolve_default_agent(home_dir: &Path) -> String {
    if let Ok(id) = std::env::var(duduclaw_core::ENV_AGENT_ID) {
        if !id.trim().is_empty() {
            return id;
        }
    }
    std::fs::read_to_string(home_dir.join("config.toml"))
        .ok()
        .and_then(|c| c.parse::<toml::Value>().ok())
        .and_then(|t| {
            t.get("general")
                .and_then(|g| g.get("default_agent"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "dudu".to_string())
}

/// Resolve the Anthropic API key: `ANTHROPIC_API_KEY` env →
/// `config.toml [api] anthropic_api_key`. Mirrors `handle_skill_synthesis_run`.
fn resolve_api_key(home_dir: &Path) -> Option<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    std::fs::read_to_string(home_dir.join("config.toml"))
        .ok()
        .and_then(|c| c.parse::<toml::Value>().ok())
        .and_then(|t| {
            t.get("api")
                .and_then(|a| a.get("anthropic_api_key"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_and_dry_run() {
        let cfg = SynthesisScheduleConfig::default();
        assert!(!cfg.auto_run, "scheduler must be off by default");
        assert!(cfg.dry_run, "must default to dry-run for safety");
        assert_eq!(cfg.interval_hours, 24);
        assert_eq!(cfg.lookback_days, 1);
        assert!(cfg.target_agent.is_none());
    }

    #[test]
    fn missing_section_falls_back_to_default() {
        let cfg = SynthesisScheduleConfig::from_config_str("[general]\ndefault_agent = \"x\"\n");
        assert_eq!(cfg, SynthesisScheduleConfig::default());
    }

    #[test]
    fn malformed_toml_fails_safe() {
        let cfg = SynthesisScheduleConfig::from_config_str("this is not = valid toml {{{");
        assert!(!cfg.auto_run, "malformed config must fail safe to disabled");
    }

    #[test]
    fn parses_full_section() {
        let raw = r#"
[skill_synthesis]
auto_run = true
dry_run = false
interval_hours = 6
lookback_days = 3
target_agent = "agnes"
"#;
        let cfg = SynthesisScheduleConfig::from_config_str(raw);
        assert!(cfg.auto_run);
        assert!(!cfg.dry_run);
        assert_eq!(cfg.interval_hours, 6);
        assert_eq!(cfg.lookback_days, 3);
        assert_eq!(cfg.target_agent.as_deref(), Some("agnes"));
    }

    #[test]
    fn clamps_out_of_range_values() {
        // lookback_days capped at 30; interval_hours floored at 1; zero ignored.
        let raw = r#"
[skill_synthesis]
auto_run = true
interval_hours = 0
lookback_days = 999
"#;
        let cfg = SynthesisScheduleConfig::from_config_str(raw);
        // interval_hours = 0 is rejected → stays at default 24.
        assert_eq!(cfg.interval_hours, 24);
        assert_eq!(cfg.lookback_days, 30);
    }

    #[test]
    fn blank_target_agent_is_none() {
        let raw = "[skill_synthesis]\nauto_run = true\ntarget_agent = \"   \"\n";
        let cfg = SynthesisScheduleConfig::from_config_str(raw);
        assert!(cfg.target_agent.is_none(), "whitespace target_agent must be None");
    }
}
