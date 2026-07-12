//! RFC-26 P3: Live Run Forking MCP tool surface.
//!
//! Six tools — `fork_run`, `inspect_branches`, `diff_branches`, `merge_or_select`,
//! `terminate_branch`, `fork_cost` — gated by `Scope::ForkExecute` (in
//! `mcp_auth.rs`) **and** the per-agent `agent.toml [fork] enabled` toggle checked
//! at handler entry (defence-in-depth, fail-closed).
//!
//! This module owns the config loader, param validation, and the JSON
//! tool-result handlers. Fork state is persisted in the cross-process
//! [`duduclaw_fork::ForkStore`] (WAL SQLite at `<home>/fork_store.db`) so the
//! gateway `/metrics` endpoint and the dashboard can observe forks even though
//! execution happens in the MCP-server process.

use std::path::Path;

use serde_json::{json, Value};

use duduclaw_fork::store::{BranchRow, ForkRow, ForkStore};

// ── Config (agent.toml [fork]) ──────────────────────────────────────────────

/// Settings parsed from an agent's `agent.toml [fork]` section. Fail-safe: a
/// missing or malformed section yields the disabled default.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkSettings {
    pub enabled: bool,
    pub max_branches: usize,
    pub default_budget_usd: f64,
    pub aggregate_budget_usd: f64,
    pub merge_mode: String,
    pub test_command: Option<String>,
    pub test_timeout_s: u64,
    /// O3 (2026-07): switch an LLM judge to FineVerify-style per-candidate
    /// scoring (`duduclaw_fork::judge::LlmJudge::with_fine_grained`). Default
    /// false. Only effective when `judge = "llm"` — the heuristic judge has
    /// no LLM pass to fine-grain.
    pub fine_grained_judge: bool,
    /// Which judge resolves the fork winner: `"heuristic"` (default —
    /// deterministic, zero LLM cost) or `"llm"` (opt-in —
    /// `LlmJudge::new(caller).with_fine_grained(fine_grained_judge)` backed by
    /// the operator's utility runtime, with automatic fallback to
    /// `HeuristicJudge` on any LLM failure). Unknown values fall back to
    /// `"heuristic"` with a logged warning (fail-safe).
    pub judge: String,
}

impl Default for ForkSettings {
    fn default() -> Self {
        ForkSettings {
            enabled: false,
            max_branches: 4,
            default_budget_usd: 0.50,
            aggregate_budget_usd: 1.50,
            merge_mode: "auto_with_fallback".to_string(),
            test_command: None,
            test_timeout_s: 120,
            fine_grained_judge: false,
            judge: "heuristic".to_string(),
        }
    }
}

/// Parse `[fork]` out of an `agent.toml` string. Pure + fail-safe for testing.
pub fn parse_fork_settings(toml_str: &str) -> ForkSettings {
    let def = ForkSettings::default();
    let parsed: toml::Value = match toml_str.parse() {
        Ok(v) => v,
        Err(_) => return def,
    };
    let fork = match parsed.get("fork").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return def,
    };
    let test_command = fork
        .get("test_command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    ForkSettings {
        enabled: fork.get("enabled").and_then(|v| v.as_bool()).unwrap_or(def.enabled),
        max_branches: fork
            .get("max_branches")
            .and_then(|v| v.as_integer())
            .filter(|n| *n >= 1)
            .map(|n| n as usize)
            .unwrap_or(def.max_branches),
        default_budget_usd: fork
            .get("default_budget_usd")
            .and_then(|v| v.as_float())
            .filter(|n| *n > 0.0)
            .unwrap_or(def.default_budget_usd),
        aggregate_budget_usd: fork
            .get("aggregate_budget_usd")
            .and_then(|v| v.as_float())
            .filter(|n| *n > 0.0)
            .unwrap_or(def.aggregate_budget_usd),
        merge_mode: fork
            .get("merge_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or(def.merge_mode),
        test_command,
        test_timeout_s: fork
            .get("test_timeout_s")
            .and_then(|v| v.as_integer())
            .filter(|n| *n >= 1)
            .map(|n| n as u64)
            .unwrap_or(def.test_timeout_s),
        fine_grained_judge: fork
            .get("fine_grained_judge")
            .and_then(|v| v.as_bool())
            .unwrap_or(def.fine_grained_judge),
        judge: match fork.get("judge").and_then(|v| v.as_str()) {
            Some(s) => {
                let normalized = s.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "heuristic" | "llm" => normalized,
                    other => {
                        tracing::warn!(
                            "unknown [fork] judge '{other}' — falling back to 'heuristic'"
                        );
                        def.judge.clone()
                    }
                }
            }
            None => def.judge.clone(),
        },
    }
}

/// Load `[fork]` settings for an agent from `<home>/agents/<id>/agent.toml`.
pub fn load_fork_settings(home_dir: &Path, agent_id: &str) -> ForkSettings {
    let path = home_dir.join("agents").join(agent_id).join("agent.toml");
    match std::fs::read_to_string(&path) {
        Ok(s) => parse_fork_settings(&s),
        Err(_) => ForkSettings::default(),
    }
}

/// Map a merge-mode string to the typed enum; unknown ⇒ default + warn.
pub fn parse_merge_mode(s: &str) -> duduclaw_fork::MergeMode {
    use duduclaw_fork::MergeMode;
    match s.trim().to_ascii_lowercase().as_str() {
        "manual" => MergeMode::Manual,
        "auto" => MergeMode::Auto,
        "auto_with_fallback" => MergeMode::AutoWithFallback,
        "vote" => MergeMode::Vote,
        other => {
            tracing::warn!("unknown fork merge_mode '{other}', defaulting to auto_with_fallback");
            MergeMode::AutoWithFallback
        }
    }
}

// ── Store access ────────────────────────────────────────────────────────────

/// Path to the per-home fork store DB.
pub fn fork_store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("fork_store.db")
}

/// Open the cross-process fork store for this home. A fresh WAL connection per
/// call keeps handlers test-isolatable; WAL handles concurrent access.
pub fn open_store(home_dir: &Path) -> Result<ForkStore, Value> {
    ForkStore::open(fork_store_path(home_dir))
        .map_err(|e| err(format!("could not open fork store: {e}")))
}

// ── JSON helpers (match existing handler envelope) ──────────────────────────

fn ok(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

fn ok_json(value: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": value.to_string() }] })
}

fn err(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("Error: {}", text.into()) }], "isError": true })
}

/// Fail-closed gate: returns an error envelope when forking is disabled for the agent.
fn require_enabled(settings: &ForkSettings) -> Option<Value> {
    if settings.enabled {
        None
    } else {
        Some(err("forking is disabled for this agent (set [fork] enabled = true in agent.toml)"))
    }
}

// ── LLM judge caller ────────────────────────────────────────────────────────

/// Production [`duduclaw_fork::judge::LlmCaller`] for `[fork] judge = "llm"`,
/// backed by the same provider-agnostic utility choke-point the `duduclaw
/// eval` live judge uses (`eval::judge::GatewayJudgeCaller` — module-private,
/// so mirrored here): honours `config.toml [runtime]` utility provider/model
/// settings and account rotation.
struct UtilityJudgeCaller {
    home_dir: std::path::PathBuf,
}

#[async_trait::async_trait]
impl duduclaw_fork::judge::LlmCaller for UtilityJudgeCaller {
    async fn complete(&self, prompt: &str) -> duduclaw_fork::Result<String> {
        duduclaw_gateway::runtime_dispatch::run_utility_prompt(
            &self.home_dir,
            None,         // agent-less: resolve the global utility runtime
            "fork-judge", // attribution id for telemetry
            "",           // judge instructions live in the prompt itself
            prompt,
            duduclaw_gateway::runtime_dispatch::UTILITY_MAX_TOKENS,
        )
        .await
        .map_err(duduclaw_fork::ForkError::Executor)
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `fork_run` — split the current task into N competing branches.
pub async fn handle_fork_run(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => return err("prompt is required"),
    };

    // Branch count: from `n` or from `strategies` length; capped at max_branches.
    let strategies: Vec<String> = args
        .get("strategies")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let requested = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or_else(|| strategies.len().max(2));
    if requested < 2 {
        return err("a fork needs at least 2 branches");
    }

    // Build the account provider up front so branches can be capped to *distinct*
    // accounts (parallel branches sharing one account collide on its rate limit).
    let provider = crate::mcp_fork_exec::build_rotator_provider(home_dir).await;
    let account_cap = match &provider {
        Some(p) => {
            use crate::mcp_fork_exec::AccountProvider;
            p.account_count().await.max(1)
        }
        None => usize::MAX,
    };
    let n = requested.min(settings.max_branches).min(account_cap);
    if n < requested {
        tracing::info!(
            "fork_run: capped branches {requested} -> {n} (max_branches={}, accounts={})",
            settings.max_branches,
            if account_cap == usize::MAX { settings.max_branches } else { account_cap }
        );
    }

    let budget = args
        .get("budget_usd")
        .and_then(|v| v.as_f64())
        .filter(|b| *b > 0.0)
        .unwrap_or(settings.default_budget_usd);
    let merge_mode = args
        .get("merge_mode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| settings.merge_mode.clone());

    let fork_id = format!("fork-{}", duduclaw_fork::BranchId::new().0);
    let branch_rows: Vec<BranchRow> = (0..n)
        .map(|i| BranchRow {
            branch_id: duduclaw_fork::BranchId::new().0,
            fork_id: fork_id.clone(),
            steering: strategies.get(i).cloned(),
            budget_usd: budget,
            state: "pending".to_string(),
            spent_usd: 0.0,
            output: String::new(),
            test_exit_code: None,
        })
        .collect();

    let fork_row = ForkRow {
        fork_id: fork_id.clone(),
        agent_id: agent_id.to_string(),
        prompt: prompt.clone(),
        merge_mode: merge_mode.clone(),
        resolved: false,
        winner: None,
        promoted: false,
        aggregate_spent_usd: 0.0,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = store.insert_fork(&fork_row, &branch_rows) {
        return err(format!("could not persist fork: {e}"));
    }

    // Launch real execution in the background when an account provider is
    // available; otherwise leave the fork persisted for manual handling. Running
    // synchronously would block the MCP stdio loop (the calling agent is itself a
    // claude process awaiting this response).
    let status = match provider {
        Some(provider) => {
            let fork_branches: Vec<duduclaw_fork::Branch> = branch_rows
                .iter()
                .map(|b| {
                    duduclaw_fork::Branch::with_id(
                        duduclaw_fork::BranchId(b.branch_id.clone()),
                        duduclaw_fork::BranchSpec {
                            steering: b.steering.clone(),
                            budget_usd: b.budget_usd,
                        },
                    )
                })
                .collect();
            let parent_ws = std::env::current_dir().unwrap_or_else(|_| home_dir.to_path_buf());
            let req = crate::mcp_fork_exec::ForkExecRequest {
                fork_id: fork_id.clone(),
                prompt,
                branches: fork_branches,
                parent_workspace: parent_ws,
                settings: settings.clone(),
                home_dir: home_dir.to_path_buf(),
            };
            let spawner = std::sync::Arc::new(crate::mcp_fork_exec::ClaudeCliSpawner);
            if settings.judge == "llm" {
                // Opt-in LLM judge ([fork] judge = "llm"): utility-runtime
                // backed caller + FineVerify toggle, wrapped so any LLM
                // failure degrades to the deterministic HeuristicJudge with
                // a logged warning instead of failing the fork.
                let llm_judge = duduclaw_fork::judge::LlmJudge::new(UtilityJudgeCaller {
                    home_dir: home_dir.to_path_buf(),
                })
                .with_fine_grained(settings.fine_grained_judge);
                tokio::spawn(crate::mcp_fork_exec::execute_fork(
                    req,
                    provider,
                    spawner,
                    std::sync::Arc::new(duduclaw_fork::judge::FallbackJudge::new(
                        llm_judge,
                        duduclaw_fork::judge::HeuristicJudge,
                    )),
                ));
            } else {
                tokio::spawn(crate::mcp_fork_exec::execute_fork(
                    req,
                    provider,
                    spawner,
                    std::sync::Arc::new(duduclaw_fork::judge::HeuristicJudge),
                ));
            }
            "running"
        }
        None => "pending_execution_backend",
    };

    ok_json(json!({
        "fork_id": fork_id,
        "branches": branch_rows.iter().map(|b| json!({
            "branch_id": b.branch_id,
            "steering": b.steering,
            "budget_usd": b.budget_usd,
        })).collect::<Vec<_>>(),
        "merge_mode": merge_mode,
        "aggregate_budget_usd": settings.aggregate_budget_usd,
        "status": status,
        "note": "poll inspect_branches for progress; resolve with merge_or_select",
    }))
}

/// `inspect_branches` — list a fork's branches + state + spend.
pub async fn handle_inspect_branches(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return err("fork_id is required"),
    };
    let fork = match store.get_fork(fork_id) {
        Ok(Some(f)) => f,
        Ok(None) => return err(format!("fork not found: {fork_id}")),
        Err(e) => return err(format!("store error: {e}")),
    };
    let branches = store.list_branches(fork_id).unwrap_or_default();
    ok_json(json!({
        "fork_id": fork.fork_id,
        "resolved": fork.resolved,
        "winner": fork.winner,
        "branches": branches.iter().map(|b| json!({
            "branch_id": b.branch_id,
            "state": b.state,
            "steering": b.steering,
            "spent_usd": b.spent_usd,
            "test_exit_code": b.test_exit_code,
        })).collect::<Vec<_>>(),
    }))
}

/// `diff_branches` — show outputs of two branches side by side.
pub async fn handle_diff_branches(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return err("fork_id is required"),
    };
    let (a, b) = match (
        args.get("branch_a").and_then(|v| v.as_str()),
        args.get("branch_b").and_then(|v| v.as_str()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return err("branch_a and branch_b are required"),
    };
    let branches = match store.list_branches(fork_id) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return err(format!("fork not found: {fork_id}")),
        Err(e) => return err(format!("store error: {e}")),
    };
    let find = |id: &str| branches.iter().find(|x| x.branch_id == id);
    let (ba, bb) = match (find(a), find(b)) {
        (Some(ba), Some(bb)) => (ba, bb),
        _ => return err("branch_a or branch_b not found in this fork"),
    };
    ok_json(json!({
        "fork_id": fork_id,
        "branch_a": { "branch_id": ba.branch_id, "state": ba.state, "output": duduclaw_core::truncate_bytes(&ba.output, 8000) },
        "branch_b": { "branch_id": bb.branch_id, "state": bb.state, "output": duduclaw_core::truncate_bytes(&bb.output, 8000) },
    }))
}

/// `merge_or_select` — resolve a fork. With `branch_id` selects explicitly.
pub async fn handle_merge_or_select(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
        Some(f) => f.to_string(),
        None => return err("fork_id is required"),
    };
    let explicit = args.get("branch_id").and_then(|v| v.as_str()).map(|s| s.to_string());

    let fork = match store.get_fork(&fork_id) {
        Ok(Some(f)) => f,
        Ok(None) => return err(format!("fork not found: {fork_id}")),
        Err(e) => return err(format!("store error: {e}")),
    };
    if fork.resolved {
        return err(format!("fork already resolved (winner: {:?})", fork.winner));
    }
    let branches = store.list_branches(&fork_id).unwrap_or_default();

    let winner = match explicit {
        Some(id) => {
            if !branches.iter().any(|b| b.branch_id == id) {
                return err(format!("branch not found in fork: {id}"));
            }
            id
        }
        None => {
            return err("automatic judge selection runs during fork execution; pass branch_id to select explicitly here");
        }
    };

    let aggregate = branches.iter().map(|b| b.spent_usd).sum();
    if let Err(e) = store.set_resolution(&fork_id, Some(&winner), true, true, aggregate) {
        return err(format!("store error: {e}"));
    }
    ok_json(json!({ "fork_id": fork_id, "resolved": true, "winner": winner }))
}

/// `terminate_branch` — mark a branch terminated (kills its subprocess in P4 follow-up).
pub async fn handle_terminate_branch(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
        Some(f) => f.to_string(),
        None => return err("fork_id is required"),
    };
    let branch_id = match args.get("branch_id").and_then(|v| v.as_str()) {
        Some(b) => b.to_string(),
        None => return err("branch_id is required"),
    };
    let branches = store.list_branches(&fork_id).unwrap_or_default();
    let current = match branches.iter().find(|b| b.branch_id == branch_id) {
        Some(b) => b,
        None => return err(format!("branch not found in fork: {branch_id}")),
    };
    // Signal the executor to skip the branch if it hasn't started yet (a running
    // subprocess is killed on task drop / shutdown via kill_on_drop).
    crate::mcp_fork_exec::request_cancel(&branch_id);
    match store.update_branch(&branch_id, "terminated", current.spent_usd, &current.output, current.test_exit_code) {
        Ok(true) => ok(format!("branch {branch_id} terminated")),
        Ok(false) => err(format!("branch not found in fork: {branch_id}")),
        Err(e) => err(format!("store error: {e}")),
    }
}

/// `fork_cost` — aggregate + per-branch spend.
pub async fn handle_fork_cost(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    let store = match open_store(home_dir) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return err("fork_id is required"),
    };
    match store.get_fork(fork_id) {
        Ok(None) => err(format!("fork not found: {fork_id}")),
        Err(e) => err(format!("store error: {e}")),
        Ok(Some(_)) => {
            let branches = store.list_branches(fork_id).unwrap_or_default();
            let aggregate: f64 = branches.iter().map(|b| b.spent_usd).sum();
            ok_json(json!({
                "fork_id": fork_id,
                "aggregate_spent_usd": aggregate,
                "per_branch": branches.iter().map(|b| json!({
                    "branch_id": b.branch_id,
                    "spent_usd": b.spent_usd,
                })).collect::<Vec<_>>(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_error(v: &Value) -> bool {
        v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
    }

    fn text(v: &Value) -> String {
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn settings_default_disabled() {
        assert!(!ForkSettings::default().enabled);
    }

    #[test]
    fn parse_missing_section_is_disabled_default() {
        let s = parse_fork_settings("[agent]\nname='x'\n");
        assert_eq!(s, ForkSettings::default());
    }

    #[test]
    fn parse_malformed_toml_is_failsafe() {
        let s = parse_fork_settings("this is not toml {{{");
        assert!(!s.enabled);
    }

    #[test]
    fn parse_reads_fork_section() {
        let toml = r#"
[fork]
enabled = true
max_branches = 3
default_budget_usd = 0.25
aggregate_budget_usd = 0.75
merge_mode = "vote"
test_command = "pytest -q"
test_timeout_s = 60
"#;
        let s = parse_fork_settings(toml);
        assert!(s.enabled);
        assert_eq!(s.max_branches, 3);
        assert_eq!(s.merge_mode, "vote");
        assert_eq!(s.test_command.as_deref(), Some("pytest -q"));
        assert_eq!(s.test_timeout_s, 60);
    }

    #[test]
    fn parse_rejects_invalid_values_failsafe() {
        let s = parse_fork_settings("[fork]\nenabled=true\nmax_branches=0\ndefault_budget_usd=-1.0\n");
        // invalid max_branches/budget fall back to defaults, enabled honored
        assert!(s.enabled);
        assert_eq!(s.max_branches, 4);
        assert_eq!(s.default_budget_usd, 0.50);
    }

    #[test]
    fn empty_test_command_is_none() {
        let s = parse_fork_settings("[fork]\nenabled=true\ntest_command=\"  \"\n");
        assert_eq!(s.test_command, None);
    }

    #[test]
    fn parse_fine_grained_judge_flag() {
        // O3 (2026-07): config surface for LlmJudge::with_fine_grained.
        assert!(!parse_fork_settings("[fork]\nenabled=true\n").fine_grained_judge);
        assert!(
            parse_fork_settings("[fork]\nenabled=true\nfine_grained_judge=true\n")
                .fine_grained_judge
        );
        // Malformed value falls back to the default (false).
        assert!(
            !parse_fork_settings("[fork]\nfine_grained_judge=\"yes\"\n").fine_grained_judge
        );
    }

    #[test]
    fn parse_judge_setting() {
        // Default: heuristic — byte-identical behavior for existing configs.
        assert_eq!(parse_fork_settings("[fork]\nenabled=true\n").judge, "heuristic");
        assert_eq!(ForkSettings::default().judge, "heuristic");
        // Opt-in LLM judge parses (case/whitespace tolerant).
        assert_eq!(parse_fork_settings("[fork]\njudge=\"llm\"\n").judge, "llm");
        assert_eq!(parse_fork_settings("[fork]\njudge=\" LLM \"\n").judge, "llm");
        // Unknown / malformed values fall back to heuristic (fail-safe).
        assert_eq!(parse_fork_settings("[fork]\njudge=\"gpt9\"\n").judge, "heuristic");
        assert_eq!(parse_fork_settings("[fork]\njudge=42\n").judge, "heuristic");
    }

    #[test]
    fn merge_mode_parsing() {
        use duduclaw_fork::MergeMode;
        assert_eq!(parse_merge_mode("manual"), MergeMode::Manual);
        assert_eq!(parse_merge_mode("AUTO"), MergeMode::Auto);
        assert_eq!(parse_merge_mode("vote"), MergeMode::Vote);
        assert_eq!(parse_merge_mode("nonsense"), MergeMode::AutoWithFallback);
    }

    #[tokio::test]
    async fn disabled_agent_is_gated() {
        // empty temp home ⇒ no agent.toml ⇒ disabled default
        let home = tempfile::tempdir().unwrap();
        let v = handle_fork_run(&json!({"prompt": "x", "n": 2}), home.path(), "agentX").await;
        assert!(is_error(&v));
        assert!(text(&v).contains("disabled"));
    }

    fn enabled_home() -> tempfile::TempDir {
        // Deterministic + host-independent: never load real accounts or spawn
        // claude during unit tests (the dev/CI host may have a logged-in account).
        // SAFETY: tests set this once; all fork tests want it set, none unset it.
        unsafe { std::env::set_var("DUDUCLAW_FORK_NO_EXEC", "1") };
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("a1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[fork]\nenabled = true\nmax_branches = 3\n",
        )
        .unwrap();
        home
    }

    #[tokio::test]
    async fn fork_run_requires_prompt() {
        let home = enabled_home();
        let v = handle_fork_run(&json!({"n": 2}), home.path(), "a1").await;
        assert!(is_error(&v));
    }

    #[tokio::test]
    async fn fork_run_min_two_branches() {
        let home = enabled_home();
        let v = handle_fork_run(&json!({"prompt": "x", "n": 1}), home.path(), "a1").await;
        assert!(is_error(&v));
    }

    #[tokio::test]
    async fn fork_run_caps_to_max_branches() {
        let home = enabled_home();
        let v = handle_fork_run(&json!({"prompt": "solve", "n": 10}), home.path(), "a1").await;
        assert!(!is_error(&v));
        let payload: Value = serde_json::from_str(&text(&v)).unwrap();
        assert_eq!(payload["branches"].as_array().unwrap().len(), 3); // capped at max_branches=3
    }

    #[tokio::test]
    async fn fork_run_then_inspect_and_cost() {
        let home = enabled_home();
        let run = handle_fork_run(
            &json!({"prompt": "solve", "strategies": ["a", "b"]}),
            home.path(),
            "a1",
        )
        .await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        let fork_id = payload["fork_id"].as_str().unwrap();

        let inspect = handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        assert!(!is_error(&inspect));
        let ip: Value = serde_json::from_str(&text(&inspect)).unwrap();
        assert_eq!(ip["branches"].as_array().unwrap().len(), 2);

        let cost = handle_fork_cost(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        let cp: Value = serde_json::from_str(&text(&cost)).unwrap();
        assert_eq!(cp["aggregate_spent_usd"], 0.0);
    }

    #[tokio::test]
    async fn inspect_unknown_fork_errors() {
        let home = enabled_home();
        let v = handle_inspect_branches(&json!({"fork_id": "nope"}), home.path(), "a1").await;
        assert!(is_error(&v));
    }

    #[tokio::test]
    async fn merge_explicit_selection() {
        let home = enabled_home();
        let run = handle_fork_run(&json!({"prompt": "x", "n": 2}), home.path(), "a1").await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        let fork_id = payload["fork_id"].as_str().unwrap().to_string();
        let winner = payload["branches"][0]["branch_id"].as_str().unwrap().to_string();

        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(!is_error(&m));

        // Second resolve fails (already resolved).
        let m2 = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&m2));
    }

    #[tokio::test]
    async fn merge_without_branch_id_defers_to_p4() {
        let home = enabled_home();
        let run = handle_fork_run(&json!({"prompt": "x", "n": 2}), home.path(), "a1").await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        let fork_id = payload["fork_id"].as_str().unwrap().to_string();
        let m = handle_merge_or_select(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        assert!(is_error(&m)); // judge auto-select is P4
    }

    #[tokio::test]
    async fn terminate_branch_marks_state() {
        let home = enabled_home();
        let run = handle_fork_run(&json!({"prompt": "x", "n": 2}), home.path(), "a1").await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        let fork_id = payload["fork_id"].as_str().unwrap().to_string();
        let bid = payload["branches"][0]["branch_id"].as_str().unwrap().to_string();

        let t = handle_terminate_branch(
            &json!({"fork_id": fork_id, "branch_id": bid}),
            home.path(),
            "a1",
        )
        .await;
        assert!(!is_error(&t));

        let inspect = handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        let ip: Value = serde_json::from_str(&text(&inspect)).unwrap();
        let states: Vec<&str> = ip["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["state"].as_str().unwrap())
            .collect();
        assert!(states.contains(&"terminated"));
    }

    #[tokio::test]
    async fn diff_requires_both_branches() {
        let home = enabled_home();
        let run = handle_fork_run(&json!({"prompt": "x", "n": 2}), home.path(), "a1").await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        let fork_id = payload["fork_id"].as_str().unwrap().to_string();
        let v = handle_diff_branches(&json!({"fork_id": fork_id, "branch_a": "x"}), home.path(), "a1").await;
        assert!(is_error(&v)); // branch_b missing
    }
}
