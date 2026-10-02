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

use serde_json::{Value, json};

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
    /// Hours an unresolved fork's retained branch workspaces
    /// (`<home>/fork_ws/<fork_id>/`) survive before the sweep deletes them.
    /// Missing / `< 1` ⇒ 24.
    pub retained_workspace_ttl_hours: u64,
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
            retained_workspace_ttl_hours: 24,
        }
    }
}

/// Parse `[fork]` out of an `agent.toml` string. Pure + fail-safe for testing.
///
/// **R2 schema unification:** `[fork]` is now a typed section on `AgentConfig`
/// and this reads it through the shared parse point
/// ([`duduclaw_core::agent_toml`]) rather than walking a `toml::Value`. Every
/// missing-key default and range filter below is unchanged, including the two
/// historical quirks called out inline.
pub fn parse_fork_settings(toml_str: &str) -> ForkSettings {
    fork_settings_from_section(duduclaw_core::agent_toml::parse(toml_str).fork)
}

/// Apply the reader-side range filters and validation to a typed `[fork]`
/// section. The single place the historical defaults are materialized, shared
/// by [`parse_fork_settings`] and [`load_fork_settings`].
fn fork_settings_from_section(f: duduclaw_core::types::ForkSection) -> ForkSettings {
    let def = ForkSettings::default();

    ForkSettings {
        enabled: f.enabled,
        max_branches: f
            .max_branches
            .filter(|n| *n >= 1)
            .map(|n| n as usize)
            .unwrap_or(def.max_branches),
        // Quirk, preserved: the raw reader used `as_float()` only, so an
        // integer literal (`default_budget_usd = 1`) never matched and fell
        // through to the default. The lenient `Option<f64>` reproduces that —
        // a TOML integer is not an f64 and deserializes to `None`. Fixing it
        // would change the effective budget of any config written that way,
        // which is a behavioral decision, not a refactor.
        default_budget_usd: f
            .default_budget_usd
            .filter(|n| *n > 0.0)
            .unwrap_or(def.default_budget_usd),
        aggregate_budget_usd: f
            .aggregate_budget_usd
            .filter(|n| *n > 0.0)
            .unwrap_or(def.aggregate_budget_usd),
        merge_mode: f.merge_mode.unwrap_or(def.merge_mode),
        test_command: f
            .test_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        test_timeout_s: f
            .test_timeout_s
            .filter(|n| *n >= 1)
            .map(|n| n as u64)
            .unwrap_or(def.test_timeout_s),
        fine_grained_judge: f.fine_grained_judge,
        // Judge validation stays at the reader: an unknown value warns and
        // falls back rather than being rejected at parse time, so a typo can
        // never cost the agent its whole config.
        judge: match f.judge.as_deref() {
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
        retained_workspace_ttl_hours: f
            .retained_workspace_ttl_hours
            .filter(|n| *n >= 1)
            .map(|n| n as u64)
            .unwrap_or(def.retained_workspace_ttl_hours),
    }
}

/// Load `[fork]` settings for an agent from `<home>/agents/<id>/agent.toml`.
pub fn load_fork_settings(home_dir: &Path, agent_id: &str) -> ForkSettings {
    fork_settings_from_section(duduclaw_core::agent_toml::load_for_agent(home_dir, agent_id).fork)
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

// ── Retained workspaces (`<home>/fork_ws`) ──────────────────────────────────

/// Root of retained branch workspaces for this home.
pub fn retained_root(home_dir: &Path) -> std::path::PathBuf {
    duduclaw_fork::retention::retained_root(home_dir)
}

/// Delete retained fork workspaces older than the configured TTL. Best-effort.
fn sweep_retained(home_dir: &Path, settings: &ForkSettings) {
    let ttl = std::time::Duration::from_secs(
        settings.retained_workspace_ttl_hours.saturating_mul(3600),
    );
    duduclaw_fork::retention::sweep_expired(&retained_root(home_dir), ttl);
}

/// Remove a fork's retained workspaces and clear their store paths. Best-effort.
pub(crate) fn discard_retained_fork(home_dir: &Path, store: &ForkStore, fork_id: &str) {
    if let Err(e) = duduclaw_fork::retention::remove_fork(&retained_root(home_dir), fork_id) {
        tracing::warn!("fork {fork_id}: removing retained workspaces failed: {e}");
    }
    let _ = store.clear_fork_workspaces(fork_id);
}

/// Resolve the retained workspace + recorded parent for a manual selection.
/// Every check fails closed with an explicit reason.
fn retained_promotion_paths(
    home_dir: &Path,
    store: &ForkStore,
    fork_id: &str,
    branch_id: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let fork_dir = duduclaw_fork::retention::fork_dir(&retained_root(home_dir), fork_id)
        .map_err(|e| e.to_string())?;
    let ws = store
        .branch_workspace(branch_id)
        .map_err(|e| format!("store error: {e}"))?
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            format!(
                "no retained workspace for branch {branch_id} (the fork was never executed, \
                 the branch did not finish, or its workspace expired); nothing can be promoted"
            )
        })?;
    if !ws.is_dir() {
        return Err(format!(
            "retained workspace for branch {branch_id} is missing on disk; nothing can be promoted"
        ));
    }
    if !duduclaw_fork::retention::is_contained(&fork_dir, &ws) {
        return Err(format!(
            "retained workspace for branch {branch_id} is outside this fork's retention directory"
        ));
    }
    let parent = store
        .parent_workspace(fork_id)
        .map_err(|e| format!("store error: {e}"))?
        .map(std::path::PathBuf::from)
        .ok_or_else(|| format!("no parent workspace recorded for fork {fork_id}"))?;
    if !parent.is_dir() {
        return Err(format!(
            "parent workspace {} no longer exists; nothing was promoted",
            parent.display()
        ));
    }
    Ok((ws, parent))
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
        Some(err(
            "forking is disabled for this agent (set [fork] enabled = true in agent.toml)",
        ))
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
    if let Err(error) = duduclaw_core::with_file_lock(&home_dir.join("fork_resolution.lock"), || {
        sweep_retained(home_dir, &settings);
        Ok(())
    }) {
        return err(format!("fork retention lock unavailable: {error}"));
    }

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => return err("prompt is required"),
    };

    // Branch count: from `n` or from `strategies` length; capped at max_branches.
    let strategies: Vec<String> = args
        .get("strategies")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
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
            if account_cap == usize::MAX {
                settings.max_branches
            } else {
                account_cap
            }
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
///
/// An explicit selection promotes the branch's **retained** workspace
/// (`<home>/fork_ws/<fork_id>/<branch_id>/`, kept when the fork finished
/// without a final resolution) into the parent the fork was taken from, under
/// the same copy policy as execution-time promotion. When that workspace or
/// the parent is gone, the call errors and the fork stays unresolved — it never
/// reports a promotion that did not happen.
pub async fn handle_merge_or_select(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    match duduclaw_core::with_file_lock(&home_dir.join("fork_resolution.lock"), || {
        let value = (|| {
            let store = match open_store(home_dir) {
                Ok(s) => s,
                Err(e) => return e,
            };
            sweep_retained(home_dir, &settings);
            let fork_id = match args.get("fork_id").and_then(|v| v.as_str()) {
                Some(f) => f.to_string(),
                None => return err("fork_id is required"),
            };
            let explicit = args
                .get("branch_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // Ids end up in filesystem paths: validate before any lookup (fail closed).
            if let Err(e) = duduclaw_fork::retention::validate_id(&fork_id) {
                return err(e.to_string());
            }
            if let Some(id) = &explicit {
                if let Err(e) = duduclaw_fork::retention::validate_id(id) {
                    return err(e.to_string());
                }
            }

            let fork = match store.get_fork(&fork_id) {
                Ok(Some(f)) => f,
                Ok(None) => return err(format!("fork not found: {fork_id}")),
                Err(e) => return err(format!("store error: {e}")),
            };
            // Promotion now writes files into the fork's parent workspace: only the
            // agent that owns the fork may do that.
            if fork.agent_id != agent_id {
                return err(format!("fork {fork_id} belongs to another agent"));
            }
            if fork.resolved {
                discard_retained_fork(home_dir, &store, &fork_id);
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
                    return err(
                        "automatic judge selection runs during fork execution; pass branch_id to select explicitly here",
                    );
                }
            };

            let (ws, parent) = match retained_promotion_paths(home_dir, &store, &fork_id, &winner) {
                Ok(p) => p,
                Err(e) => return err(e),
            };
            match duduclaw_fork::with_parent_publication(&parent, |publication| {
                let report = match publication.promote(
                    &ws,
                    &duduclaw_fork::CopyPolicy::promote_for_parent(&parent, home_dir),
                ) {
                    Ok(r) => r,
                    Err(e) => return Ok(err(format!("promotion failed, fork left unresolved: {e}"))),
                };

                let aggregate = branches.iter().map(|b| b.spent_usd).sum();
                if let Err(e) = store.set_resolution(&fork_id, Some(&winner), true, true, aggregate) {
                    return Ok(err(format!("files were promoted but the store update failed: {e}")));
                }
                discard_retained_fork(home_dir, &store, &fork_id);
                Ok(ok_json(json!({
                    "fork_id": fork_id,
                    "resolved": true,
                    "promoted": true,
                    "winner": winner,
                    "files_copied": report.files_copied,
                    "dropped_by_policy": report.excluded + report.symlinks_dropped + report.special_dropped,
                })))
            }) {
                Ok(value) => value,
                Err(error) => err(format!("fork parent publication lock unavailable: {error}")),
            }
        })();
        Ok(value)
    }) {
        Ok(value) => value,
        Err(error) => err(format!("fork resolution lock unavailable: {error}")),
    }
}

/// `terminate_branch` — mark a branch terminated (kills its subprocess in P4 follow-up).
pub async fn handle_terminate_branch(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let settings = load_fork_settings(home_dir, agent_id);
    if let Some(e) = require_enabled(&settings) {
        return e;
    }
    match duduclaw_core::with_file_lock(&home_dir.join("fork_resolution.lock"), || {
        let value = (|| {
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
            for id in [&fork_id, &branch_id] {
                if let Err(e) = duduclaw_fork::retention::validate_id(id) {
                    return err(e.to_string());
                }
            }
            match store.get_fork(&fork_id) {
                Ok(Some(fork)) if fork.agent_id == agent_id => {}
                Ok(Some(_)) => return err("fork belongs to another agent"),
                Ok(None) => return err(format!("fork not found: {fork_id}")),
                Err(error) => return err(format!("store error: {error}")),
            }
            let branches = store.list_branches(&fork_id).unwrap_or_default();
            let current = match branches.iter().find(|b| b.branch_id == branch_id) {
                Some(b) => b,
                None => return err(format!("branch not found in fork: {branch_id}")),
            };
            // Signal the executor to skip the branch if it hasn't started yet (a running
            // subprocess is killed on task drop / shutdown via kill_on_drop).
            crate::mcp_fork_exec::request_cancel(&branch_id);
            // A terminated branch can no longer be selected: drop its retained
            // workspace (and the fork dir once empty) instead of leaving it behind.
            if let Err(e) =
                duduclaw_fork::retention::remove_branch(&retained_root(home_dir), &fork_id, &branch_id)
            {
                tracing::warn!("fork {fork_id}: removing retained branch {branch_id} failed: {e}");
            }
            let _ = store.set_branch_workspace(&branch_id, None);
            match store.update_branch(
                &branch_id,
                "terminated",
                current.spent_usd,
                &current.output,
                current.test_exit_code,
            ) {
                Ok(true) => ok(format!("branch {branch_id} terminated")),
                Ok(false) => err(format!("branch not found in fork: {branch_id}")),
                Err(e) => err(format!("store error: {e}")),
            }
        })();
        Ok(value)
    }) {
        Ok(value) => value,
        Err(error) => err(format!("fork resolution lock unavailable: {error}")),
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
        let s =
            parse_fork_settings("[fork]\nenabled=true\nmax_branches=0\ndefault_budget_usd=-1.0\n");
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
        assert!(!parse_fork_settings("[fork]\nfine_grained_judge=\"yes\"\n").fine_grained_judge);
    }

    #[test]
    fn parse_judge_setting() {
        // Default: heuristic — byte-identical behavior for existing configs.
        assert_eq!(
            parse_fork_settings("[fork]\nenabled=true\n").judge,
            "heuristic"
        );
        assert_eq!(ForkSettings::default().judge, "heuristic");
        // Opt-in LLM judge parses (case/whitespace tolerant).
        assert_eq!(parse_fork_settings("[fork]\njudge=\"llm\"\n").judge, "llm");
        assert_eq!(
            parse_fork_settings("[fork]\njudge=\" LLM \"\n").judge,
            "llm"
        );
        // Unknown / malformed values fall back to heuristic (fail-safe).
        assert_eq!(
            parse_fork_settings("[fork]\njudge=\"gpt9\"\n").judge,
            "heuristic"
        );
        assert_eq!(parse_fork_settings("[fork]\njudge=42\n").judge, "heuristic");
    }

    // ── R5 default-direction locks ──────────────────────────────────────
    //
    // `[fork]` moved onto the typed schema. Its defaults are pinned here
    // because the section mixes an opt-in master switch with fail-to-constant
    // knobs, and because it carries one genuine historical quirk (integer
    // budget literals are ignored) that this migration deliberately preserved
    // rather than fixed — see the note in `fork_settings_from_section`.

    #[test]
    fn default_direction_fork_absent_section_is_fully_disabled_defaults() {
        for body in ["", "[agent]\nname = \"a\"\n", "[fork]\n", "not toml {{{"] {
            let s = parse_fork_settings(body);
            assert!(!s.enabled, "fork is opt-in: {body:?}");
            assert_eq!(s.max_branches, 4, "{body:?}");
            assert_eq!(s.default_budget_usd, 0.50, "{body:?}");
            assert_eq!(s.aggregate_budget_usd, 1.50, "{body:?}");
            assert_eq!(s.merge_mode, "auto_with_fallback", "{body:?}");
            assert_eq!(s.test_command, None, "{body:?}");
            assert_eq!(s.test_timeout_s, 120, "{body:?}");
            assert!(!s.fine_grained_judge, "{body:?}");
            assert_eq!(s.judge, "heuristic", "{body:?}");
            assert_eq!(s.retained_workspace_ttl_hours, 24, "{body:?}");
        }
    }

    /// **Historical quirk, deliberately preserved.** The pre-migration reader
    /// used `as_float()` only, so a TOML *integer* budget silently fell back
    /// to the default. Any config written as `default_budget_usd = 1` has been
    /// running at 0.50 all along; making the integer work here would silently
    /// double some agent's spend ceiling. That is a product decision, not a
    /// refactor — this test exists so the change can't happen by accident.
    #[test]
    fn default_direction_fork_budget_rejects_integer_literal() {
        let s = parse_fork_settings(
            "[fork]\nenabled=true\ndefault_budget_usd=1\naggregate_budget_usd=3\n",
        );
        assert_eq!(
            s.default_budget_usd, 0.50,
            "integer literal ⇒ default (historical)"
        );
        assert_eq!(
            s.aggregate_budget_usd, 1.50,
            "integer literal ⇒ default (historical)"
        );

        // Float literals are honored, which is the documented way to write it.
        let s = parse_fork_settings(
            "[fork]\nenabled=true\ndefault_budget_usd=1.0\naggregate_budget_usd=3.0\n",
        );
        assert_eq!(s.default_budget_usd, 1.0);
        assert_eq!(s.aggregate_budget_usd, 3.0);
    }

    #[test]
    fn default_direction_fork_out_of_range_counts_as_unset() {
        // `>= 1` / `> 0.0` filters treat an out-of-range written value as
        // absent rather than clamping it — preserved from the raw reader.
        let s = parse_fork_settings(
            "[fork]\nmax_branches=0\ntest_timeout_s=0\ndefault_budget_usd=-1.0\n",
        );
        assert_eq!(s.max_branches, 4);
        assert_eq!(s.test_timeout_s, 120);
        assert_eq!(s.default_budget_usd, 0.50);
    }

    #[test]
    fn default_direction_fork_wrong_typed_keys_never_fail_the_parse() {
        // Pre-migration these were invisible to `AgentConfig`; typing the
        // section must not let a typo cost the agent its whole config.
        let s = parse_fork_settings(
            "[fork]\nenabled=\"yes\"\nmax_branches=\"four\"\ntest_timeout_s=true\n",
        );
        assert!(!s.enabled);
        assert_eq!(s.max_branches, 4);
        assert_eq!(s.test_timeout_s, 120);
    }

    #[test]
    fn default_direction_load_and_parse_agree() {
        // `load_fork_settings` (file) and `parse_fork_settings` (string) must
        // resolve identically — they now share one section→settings step.
        let home = tempfile::tempdir().unwrap();
        let body = "[fork]\nenabled=true\nmax_branches=2\njudge=\"llm\"\n";
        let dir = home.path().join("agents").join("a1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
        assert_eq!(
            load_fork_settings(home.path(), "a1"),
            parse_fork_settings(body)
        );

        // Unknown agent ⇒ defaults, never an error.
        assert_eq!(
            load_fork_settings(home.path(), "nope"),
            ForkSettings::default()
        );
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

        let inspect =
            handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
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

    /// Fork via the handler (no execution backend in tests), then stage what a
    /// finished-but-unresolved execution would have left behind: a retained
    /// workspace for `branch` and the recorded parent.
    fn stage_retained(
        home: &Path,
        fork_id: &str,
        branch: &str,
        parent: &Path,
    ) -> std::path::PathBuf {
        let ws = retained_root(home).join(fork_id).join(branch);
        std::fs::create_dir_all(&ws).unwrap();
        let store = open_store(home).unwrap();
        assert!(
            store
                .set_parent_workspace(fork_id, Some(&parent.to_string_lossy()))
                .unwrap()
        );
        assert!(
            store
                .set_branch_workspace(branch, Some(&ws.to_string_lossy()))
                .unwrap()
        );
        ws
    }

    async fn fork_two(home: &Path) -> (String, String) {
        let run = handle_fork_run(&json!({"prompt": "x", "n": 2}), home, "a1").await;
        let payload: Value = serde_json::from_str(&text(&run)).unwrap();
        (
            payload["fork_id"].as_str().unwrap().to_string(),
            payload["branches"][0]["branch_id"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    #[tokio::test]
    async fn merge_explicit_selection_promotes_retained_workspace() {
        let home = enabled_home();
        let parent = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("keep.txt"), "parent").unwrap();
        let (fork_id, winner) = fork_two(home.path()).await;
        let ws = stage_retained(home.path(), &fork_id, &winner, parent.path());
        std::fs::write(ws.join("made_in_branch.txt"), "branch output").unwrap();
        std::fs::write(ws.join(".env"), "SECRET=1").unwrap();

        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(!is_error(&m), "{}", text(&m));
        assert_eq!(
            std::fs::read_to_string(parent.path().join("made_in_branch.txt")).unwrap(),
            "branch output"
        );
        assert!(parent.path().join("keep.txt").is_file());
        assert!(!parent.path().join(".env").exists(), "secrets are never promoted");
        let fork = open_store(home.path()).unwrap().get_fork(&fork_id).unwrap().unwrap();
        assert!(fork.promoted && fork.resolved);
        assert_eq!(fork.winner.as_deref(), Some(winner.as_str()));
        assert!(!retained_root(home.path()).join(&fork_id).exists());

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
    async fn concurrent_manual_selection_adopts_only_once() {
        let home = enabled_home();
        let parent = tempfile::tempdir().unwrap();
        let (fork_id, winner) = fork_two(home.path()).await;
        let workspace = stage_retained(home.path(), &fork_id, &winner, parent.path());
        for i in 0..100 {
            std::fs::write(workspace.join(format!("file-{i}")), vec![b'x'; 1024]).unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads = (0..2).map(|_| {
            let home = home.path().to_path_buf();
            let args = json!({"fork_id":fork_id,"branch_id":winner});
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                barrier.wait();
                runtime.block_on(handle_merge_or_select(&args, &home, "a1"))
            })
        }).collect::<Vec<_>>();
        let results = threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|value| !is_error(value)).count(), 1);
        assert_eq!(results.iter().filter(|value| is_error(value)).count(), 1);
        assert!(parent.path().join("file-99").is_file());
        let record = open_store(home.path()).unwrap().get_fork(&fork_id).unwrap().unwrap();
        assert!(record.resolved && record.promoted);
        assert_eq!(record.winner, Some(winner));
    }

    #[test]
    fn same_parent_different_homes_share_publication_through_commit_and_cleanup() {
        let first_home = enabled_home();
        let second_home = enabled_home();
        let parent = tempfile::tempdir().unwrap();
        for name in ["winner.txt", "paired.txt"] {
            std::fs::write(parent.path().join(name), "original").unwrap();
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut fixtures = Vec::new();
        for (home, tag) in [(first_home.path(), "first"), (second_home.path(), "second")] {
            let (fork_id, winner) = runtime.block_on(fork_two(home));
            let workspace = stage_retained(home, &fork_id, &winner, parent.path());
            for name in ["winner.txt", "paired.txt"] {
                std::fs::write(workspace.join(name), tag).unwrap();
            }
            fixtures.push((home.to_path_buf(), fork_id, winner, workspace));
        }
        let mut workers = Vec::new();
        let (started, await_started) = std::sync::mpsc::channel();
        let before_release = duduclaw_fork::with_parent_publication(parent.path(), |_| {
            for (home, fork_id, winner, _) in &fixtures {
                let home = home.clone();
                let args = json!({"fork_id":fork_id,"branch_id":winner});
                let started = started.clone();
                workers.push(std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    started.send(()).unwrap();
                    runtime.block_on(handle_merge_or_select(&args, &home, "a1"))
                }));
            }
            for _ in 0..2 { await_started.recv_timeout(std::time::Duration::from_secs(5)).unwrap(); }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let contents = ["winner.txt", "paired.txt"].map(|name| std::fs::read_to_string(parent.path().join(name)).unwrap());
            let sources_and_state = fixtures.iter().all(|(home, fork, _, workspace)| {
                workspace.exists() && !open_store(home).unwrap().get_fork(fork).unwrap().unwrap().resolved
            });
            Ok((contents, sources_and_state))
        }).unwrap();
        let responses = workers.into_iter().map(|worker| worker.join().unwrap()).collect::<Vec<_>>();
        assert_eq!(before_release.0, ["original", "original"]);
        assert!(before_release.1, "waiting publishers cannot commit or clean their source");
        assert!(responses.iter().all(|response| !is_error(response)));
        let winner = std::fs::read_to_string(parent.path().join("winner.txt")).unwrap();
        assert!(winner == "first" || winner == "second");
        assert_eq!(std::fs::read_to_string(parent.path().join("paired.txt")).unwrap(), winner);
        for (home, fork_id, _, workspace) in fixtures {
            let fork = open_store(&home).unwrap().get_fork(&fork_id).unwrap().unwrap();
            assert!(fork.resolved && fork.promoted);
            assert!(!workspace.exists());
        }
    }

    #[tokio::test]
    async fn merge_without_retained_workspace_errors_and_stays_unpromoted() {
        let home = enabled_home();
        let (fork_id, winner) = fork_two(home.path()).await;
        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&m));
        assert!(text(&m).contains("no retained workspace"));
        let fork = open_store(home.path()).unwrap().get_fork(&fork_id).unwrap().unwrap();
        assert!(!fork.promoted && !fork.resolved);
        assert_eq!(fork.winner, None);

        // Recorded but deleted on disk: still an honest error, still unresolved.
        let parent = tempfile::tempdir().unwrap();
        let ws = stage_retained(home.path(), &fork_id, &winner, parent.path());
        std::fs::remove_dir_all(&ws).unwrap();
        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&m));
        let fork = open_store(home.path()).unwrap().get_fork(&fork_id).unwrap().unwrap();
        assert!(!fork.promoted && !fork.resolved);
    }

    #[tokio::test]
    async fn merge_rejects_workspace_outside_retention_dir() {
        let home = enabled_home();
        let parent = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let (fork_id, winner) = fork_two(home.path()).await;
        stage_retained(home.path(), &fork_id, &winner, parent.path());
        // Tamper: point the branch at a directory outside fork_ws/<fork_id>.
        open_store(home.path())
            .unwrap()
            .set_branch_workspace(&winner, Some(&elsewhere.path().to_string_lossy()))
            .unwrap();
        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&m));
        let fork = open_store(home.path()).unwrap().get_fork(&fork_id).unwrap().unwrap();
        assert!(!fork.promoted);
    }

    #[tokio::test]
    async fn merge_rejects_malicious_ids() {
        let home = enabled_home();
        let (fork_id, _) = fork_two(home.path()).await;
        for bad in ["../x", "..", "a/b", "..\\x"] {
            let m = handle_merge_or_select(
                &json!({"fork_id": fork_id, "branch_id": bad}),
                home.path(),
                "a1",
            )
            .await;
            assert!(is_error(&m), "{bad}");
            assert!(text(&m).contains("invalid"), "{bad}: {}", text(&m));
        }
        let m = handle_merge_or_select(
            &json!({"fork_id": "../etc", "branch_id": "b"}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&m));
    }

    #[tokio::test]
    async fn merge_by_other_agent_is_refused() {
        let home = enabled_home();
        let other = home.path().join("agents").join("a2");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("agent.toml"), "[fork]\nenabled = true\n").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let (fork_id, winner) = fork_two(home.path()).await;
        stage_retained(home.path(), &fork_id, &winner, parent.path());
        let m = handle_merge_or_select(
            &json!({"fork_id": fork_id, "branch_id": winner}),
            home.path(),
            "a2",
        )
        .await;
        assert!(is_error(&m));
    }

    #[tokio::test]
    async fn terminate_by_other_agent_preserves_workspace_and_store() {
        let home = enabled_home();
        let other = home.path().join("agents/a2");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("agent.toml"), "[fork]\nenabled = true\n").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let (fork_id, bid) = fork_two(home.path()).await;
        let ws = stage_retained(home.path(), &fork_id, &bid, parent.path());
        std::fs::write(ws.join("owner-output.txt"), "preserved").unwrap();
        let before = handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        let denied = handle_terminate_branch(
            &json!({"fork_id": fork_id, "branch_id": bid}), home.path(), "a2",
        ).await;
        assert!(is_error(&denied));
        assert!(text(&denied).contains("another agent"));
        assert_eq!(std::fs::read_to_string(ws.join("owner-output.txt")).unwrap(), "preserved");
        assert_eq!(open_store(home.path()).unwrap().branch_workspace(&bid).unwrap(),
            Some(ws.to_string_lossy().into_owned()));
        let after = handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn terminate_branch_drops_retained_workspace() {
        let home = enabled_home();
        let parent = tempfile::tempdir().unwrap();
        let (fork_id, bid) = fork_two(home.path()).await;
        stage_retained(home.path(), &fork_id, &bid, parent.path());
        let t = handle_terminate_branch(
            &json!({"fork_id": fork_id, "branch_id": bid}),
            home.path(),
            "a1",
        )
        .await;
        assert!(!is_error(&t));
        assert!(!retained_root(home.path()).join(&fork_id).exists());
        assert_eq!(open_store(home.path()).unwrap().branch_workspace(&bid).unwrap(), None);
    }

    #[test]
    fn parse_retained_workspace_ttl() {
        assert_eq!(parse_fork_settings("[fork]\n").retained_workspace_ttl_hours, 24);
        assert_eq!(
            parse_fork_settings("[fork]\nretained_workspace_ttl_hours = 6\n")
                .retained_workspace_ttl_hours,
            6
        );
        assert_eq!(
            parse_fork_settings("[fork]\nretained_workspace_ttl_hours = 0\n")
                .retained_workspace_ttl_hours,
            24
        );
        assert_eq!(
            parse_fork_settings("[fork]\nretained_workspace_ttl_hours = \"x\"\n")
                .retained_workspace_ttl_hours,
            24
        );
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
        let bid = payload["branches"][0]["branch_id"]
            .as_str()
            .unwrap()
            .to_string();

        let t = handle_terminate_branch(
            &json!({"fork_id": fork_id, "branch_id": bid}),
            home.path(),
            "a1",
        )
        .await;
        assert!(!is_error(&t));

        let inspect =
            handle_inspect_branches(&json!({"fork_id": fork_id}), home.path(), "a1").await;
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
        let v = handle_diff_branches(
            &json!({"fork_id": fork_id, "branch_a": "x"}),
            home.path(),
            "a1",
        )
        .await;
        assert!(is_error(&v)); // branch_b missing
    }
}
