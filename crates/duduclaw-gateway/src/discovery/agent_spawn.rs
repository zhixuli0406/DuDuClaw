//! Discovery attempt runner (work package B1): one agent CLI session per
//! attempt, inside a prepared node directory, with an empty MCP surface, a
//! restricted built-in tool list, OS confinement and per-call cost capture.
//!
//! Each process reserves a shared call budget before spawning. Retries use
//! identical prompts and stay inside the selected runtime and account pool.
//!
//! # Adding a runner for another runtime
//! A new runner is one more `impl AttemptRunner` plus one arm in
//! [`AttemptRunnerFactory::for_runtime`]. It must provide, per call:
//! 1. cwd control — the process runs with `cwd = req.node_dir`;
//! 2. an empty tool surface — no MCP servers, no web tools, no sub-agents,
//!    only file read/write/edit/search and a shell;
//! 3. per-call cost — tokens and dollars of THIS call from the CLI's own
//!    output (I4), never a time-window attribution;
//! 4. confinement — read explicit completed workspaces + toolchain, write only `req.node_dir`
//!    and a private temp dir; no backend ⇒ refuse (fail closed).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::contracts::{AttemptInfraError, AttemptOutcome, AttemptRequest, AttemptRunner, IsolationBackend};
use super::budget::SharedBudget;
use super::config::{DiscoveryConfig, AttemptSettings, AttemptSandbox};
use super::isolation::{confine_command, ConfinementSpec};
use super::tree::{CostSource, NodeCost};
use std::sync::Arc;
use std::process::Command;
use std::time::Duration;
use duduclaw_agent::account_rotator::{AccountRotator, RotationStrategy};
use crate::cost_telemetry::TokenUsage;

/// Upper bound of [`AttemptOutcome::final_text`].
pub const FINAL_TEXT_MAX_BYTES: usize = 64 * 1024;

/// `config.toml` key (under `[discovery]`) listing extra read-only paths for
/// the attempt confinement profile. To be folded into the config module.
pub const EXTRA_READ_PATHS_KEY: &str = "attempt_extra_read_paths";

/// Built-in tools an attempt may use: file read/write/edit/search and shell.
/// No WebFetch/WebSearch, no Task (sub-agents).
pub const ATTEMPT_TOOLS: &str = "Read,Write,Edit,Glob,Grep,Bash";

/// Content of the strict, empty MCP config file.
pub const EMPTY_MCP_CONFIG: &str = "{\"mcpServers\":{}}";

/// Variables a surrounding Claude Code session exports that make a nested
/// `claude` behave as a child session. Removed after the rotator env is
/// applied (the allowlist already drops them; this is defence in depth).
pub const NESTED_SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Build the attempt argv (program excluded). The prompt is NOT in argv: it
/// is written to stdin byte-for-byte, so retries re-send identical bytes and
/// the variadic `--tools` flag cannot swallow it.
pub fn build_claude_argv(model: &str, max_turns: u32, mcp_config_path: &Path) -> Vec<String> {
    vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--model".into(),
        model.into(),
        "--max-turns".into(),
        max_turns.to_string(),
        "--tools".into(),
        ATTEMPT_TOOLS.into(),
        "--allowedTools".into(),
        ATTEMPT_TOOLS.into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        mcp_config_path.to_string_lossy().into_owned(),
        "--setting-sources".into(),
        String::new(),
        "--restricted".into(),
        "--disable-slash-commands".into(),
        "--no-session-persistence".into(),
    ]
}

/// Final child environment overlay, applied AFTER
/// `duduclaw_core::spawn_env::apply_agent_cli_env_allowlist` and the rotator
/// account env. `None` value ⇒ `env_remove`.
pub fn attempt_env_overlay(
    account_env: &std::collections::HashMap<String, String>,
    private_tmp: &Path,
) -> BTreeMap<String, Option<String>> {
    let mut out: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (k, v) in account_env {
        // Rotator contract: an empty value means "remove".
        let val = if v.is_empty() { None } else { Some(v.clone()) };
        out.insert(k.clone(), val);
    }
    let tmp = private_tmp.to_string_lossy().into_owned();
    // Claude Code otherwise uses /tmp/claude-<uid> (denied under confinement:
    // "EPERM: operation not permitted, open '/tmp/claude-501'").
    out.insert("CLAUDE_CODE_TMPDIR".into(), Some(tmp.clone()));
    // Bun extracts native modules under TMPDIR.
    out.insert("TMPDIR".into(), Some(tmp));
    out.insert("DISABLE_AUTOUPDATER".into(), Some("1".into()));
    for k in NESTED_SESSION_ENV {
        out.insert((*k).to_string(), None);
    }
    out
}

/// What the stream-json output of one call reported.
#[derive(Debug, Clone, Default)]
pub struct StreamSummary {
    pub usage: Option<TokenUsage>,
    pub total_cost_usd: Option<f64>,
    pub final_text: Option<String>,
    pub is_error: bool,
    pub model: Option<String>,
    /// Number of parseable JSON events seen (0 ⇒ empty output).
    pub events: usize,
    /// True only for an authoritative final usage envelope.
    pub complete: bool,
}

/// Parse a whole stream-json transcript. The final `result` event is
/// authoritative for usage, cost and text; assistant-message usage is summed
/// only when no `result` event arrived.
pub fn parse_stream(stdout: &str) -> StreamSummary {
    let mut s = StreamSummary::default();
    let mut summed = TokenUsage::default();
    let mut saw_assistant_usage = false;
    let mut saw_result = false;
    let mut message_ids=std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if !v.is_object() {
            continue;
        }
        s.events += 1;
        match v.get("type").and_then(|t| t.as_str()) {
            Some("result") => {
                saw_result=true;
                s.usage = v.get("usage").and_then(complete_usage);
                s.total_cost_usd = v.get("total_cost_usd").and_then(|c| c.as_f64());
                s.final_text = v.get("result").and_then(|r| r.as_str()).map(str::to_string);
                s.is_error = v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
                s.complete = !s.is_error && s.usage.is_some();
            }
            Some("assistant") => {
                let msg = v.get("message");
                if let Some(m) = msg.and_then(|m| m.get("model")).and_then(|m| m.as_str()) {
                    if crate::runtime_dispatch::is_observed_claude_model(m) {
                        s.model = Some(m.trim().to_string());
                    }
                }
                if let Some(u) = msg
                    .and_then(|m| m.get("usage"))
                    .and_then(complete_usage)
                {
                    if let Some(id)=msg.and_then(|m|m.get("id")).and_then(|id|id.as_str()) { if !message_ids.insert(id.to_owned()) {continue;} }
                    saw_assistant_usage = true;
                    summed.input_tokens = summed.input_tokens.saturating_add(u.input_tokens);
                    summed.cache_read_tokens = summed.cache_read_tokens.saturating_add(u.cache_read_tokens);
                    summed.cache_creation_tokens = summed.cache_creation_tokens.saturating_add(u.cache_creation_tokens);
                    summed.output_tokens = summed.output_tokens.saturating_add(u.output_tokens);
                }
            }
            _ => {}
        }
    }
    if (!saw_result || !s.complete) && s.usage.is_none() && saw_assistant_usage {
        s.usage = Some(summed);
    }
    s
}

/// Bound `text` to [`FINAL_TEXT_MAX_BYTES`] on a char boundary.
pub fn cap_final_text(text: &str) -> String {
    duduclaw_core::truncate_bytes(text, FINAL_TEXT_MAX_BYTES).to_string()
}

fn complete_usage(value: &serde_json::Value) -> Option<TokenUsage> {
    // Missing output is unknown, never an inferred zero-token generation.
    value.get("input_tokens")?.as_u64()?;
    value.get("output_tokens")?.as_u64()?;
    let usage=TokenUsage::from_json(value)?;
    usage.input_tokens.checked_add(usage.cache_read_tokens)?.checked_add(usage.cache_creation_tokens)?;
    Some(usage)
}

#[derive(Debug, Clone, Copy)]
pub struct CallCost { pub usd: f64, pub source: CostSource }

/// A CLI bill is reported; every token-table calculation is an estimate.
/// Unknown uses NaN only at the budget boundary, never in persisted NodeCost.
pub fn call_cost(model: &str, summary: &StreamSummary) -> CallCost {
    call_cost_for(super::attempt_adapter::RuntimeFamily::Claude, model, summary)
}

/// Family-aware cost: only Claude keeps the legacy price fallback for a model
/// missing from the registry; any other family's unpriced model is Unknown
/// (operators add prices in `~/.duduclaw/models.toml`).
pub fn call_cost_for(family: super::attempt_adapter::RuntimeFamily, model: &str, summary: &StreamSummary) -> CallCost {
    if let Some(usd) = summary.total_cost_usd.filter(|c| c.is_finite() && *c >= 0.0) {
        return CallCost { usd, source: CostSource::Reported };
    }
    let Some(u) = &summary.usage else {
        return CallCost { usd: f64::NAN, source: CostSource::Unknown };
    };
    if u.input_tokens.checked_add(u.cache_read_tokens).and_then(|v|v.checked_add(u.cache_creation_tokens)).is_none() { return CallCost {usd:f64::NAN,source:CostSource::Unknown}; }
    let registry = crate::cost_telemetry::model_registry();
    let usd = match registry.get(model) {
        Some(info) => registry.cost_millicents(&duduclaw_llm::NormalizedUsage {
            input_tokens: u.input_tokens, output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens, cache_write_tokens: u.cache_creation_tokens,
            reasoning_tokens: 0,
        }, info) as f64 / 100_000.0,
        None if family != super::attempt_adapter::RuntimeFamily::Claude => {
            return CallCost { usd: f64::NAN, source: CostSource::Unknown };
        }
        None => {
            // Explicitly estimated legacy fallback, preserving sub-cent cost.
            let long = u.total_input() > 200_000;
            (u.input_tokens as f64 * if long { 6.0 } else { 3.0 }
                + u.output_tokens as f64 * if long { 22.5 } else { 15.0 }
                + u.cache_read_tokens as f64 * 0.30
                + u.cache_creation_tokens as f64 * 3.75) / 1_000_000.0
        }
    };
    CallCost { usd, source: if summary.complete {CostSource::Estimated}else{CostSource::Unknown} }
}

/// Numeric compatibility API. NaN means an unknown liability, never free.
pub fn usd_for(model: &str, summary: &StreamSummary) -> f64 {
    call_cost(model, summary).usd
}
fn usd_for_family(family: super::attempt_adapter::RuntimeFamily, model: &str, summary: &StreamSummary) -> f64 {
    call_cost_for(family, model, summary).usd
}

fn accumulate_cost(total: &mut NodeCost, cost: CallCost) {
    if cost.usd.is_finite() && cost.usd >= 0.0 { total.usd += cost.usd; }
    match cost.source {
        CostSource::Unknown | CostSource::Pending => {
            total.unknown_calls = total.unknown_calls.saturating_add(1);
            total.usd_source = CostSource::Unknown;
        }
        CostSource::Estimated if total.usd_source == CostSource::Reported => {
            total.usd_source = CostSource::Estimated;
        }
        _ => {}
    }
}

/// Runner for the Claude CLI family. Unconfined execution requires BOTH
/// the operator-only identity and its separate global opt-in.
#[derive(Clone)]
pub struct ClaudeAttemptRunner {
    pub home_dir: PathBuf,
    pub quota: super::attempt_container::QuotaLimits,
    pub claude_bin: Option<PathBuf>,
    pub allow_unconfined: bool,
    pub operator_identity: bool,
    pub budget: Option<SharedBudget>,
    pub account_rotator: Option<Arc<AccountRotator>>,
    pub extra_read_paths: Vec<PathBuf>,
    pub max_concurrency: u32,
}

struct LeaseGuard { home: PathBuf, lease: duduclaw_core::concurrency_gate::Lease }
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if duduclaw_core::concurrency_gate::release_checked(&self.home, &self.lease).is_err() {
            crate::security_autopilot::audit_and_emit(&self.home,
                &duduclaw_security::audit::AuditEvent::new("discovery_worker_lease_release_failed", "discovery-attempt",
                    duduclaw_security::audit::Severity::Warning,
                    serde_json::json!({"reason":"bounded_release_failed","release_confirmed":false})));
        }
    }
}
fn timeout_lease_secs(timeout: std::time::Duration) -> u64 { timeout.as_secs().saturating_add(30) }

/// Capacity is shared across runs. Wait without spending a call or weakening
/// the admission cap; cancellation and the original attempt deadline win.
async fn acquire_attempt_slot(home: &Path, cap: u32, budget: &SharedBudget,
    deadline: std::time::Instant) -> Result<LeaseGuard, AttemptInfraError> {
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now())
            .min(budget.remaining_wall());
        if budget.is_cancelled() || remaining.is_zero() { return Err(AttemptInfraError::BudgetExhausted); }
        match duduclaw_core::concurrency_gate::try_acquire_checked(home, "discovery", Some(cap), timeout_lease_secs(remaining))
            .map_err(|error| { budget.cancel(); AttemptInfraError::Spawn(error.to_string()) })? {
            duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) if lease.is_guarded() => {
                let guard = LeaseGuard { home: home.to_path_buf(), lease };
                if budget.is_cancelled() || budget.remaining_wall().is_zero() || std::time::Instant::now() >= deadline {
                    return Err(AttemptInfraError::BudgetExhausted);
                }
                return Ok(guard);
            }
            duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(_) => return Err(AttemptInfraError::IsolationUnavailable),
            duduclaw_core::concurrency_gate::AcquireOutcome::AtCapacity { .. } => {
                tokio::select! {
                    _ = budget.cancelled() => return Err(AttemptInfraError::BudgetExhausted),
                    _ = tokio::time::sleep(remaining.min(Duration::from_millis(25))) => {},
                }
            }
        }
    }
}

fn binary_path(explicit: Option<&Path>) -> Result<PathBuf, AttemptInfraError> {
    if let Some(path) = explicit {
        return path.canonicalize().map_err(|e| AttemptInfraError::Spawn(e.to_string()));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("claude"))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| AttemptInfraError::Spawn("claude CLI is unavailable".into()))
}

/// Infrastructure errors never become scored nodes. A started attempt with
/// tool activity does, even if it was interrupted before the result event.
fn infra_reason(summary: &StreamSummary, stderr: &str, worked: bool, timeout: bool) -> Option<String> {
    if timeout && !worked { return Some("timeout_before_work".into()); }
    if !worked && summary.final_text.as_ref().is_none_or(|s| s.trim().is_empty()) {
        return Some("empty_output".into());
    }
    if summary.is_error || summary.final_text.is_none() {
        let blob = format!("{} {}", stderr, summary.final_text.as_deref().unwrap_or(""));
        // Error classification is deliberately separate from routing or authorization.
        let pattern = regex::Regex::new(r"(?i)rate.?limit|429|overloaded|529|usage limit|quota|credit balance|temporarily unavailable|ECONNRESET|ETIMEDOUT|network error|API Error: 5[0-9][0-9]|not logged in|please run /login|invalid api key").expect("static regex");
        if pattern.is_match(&blob) { return Some("provider_unavailable".into()); }
        if !worked && !timeout { return Some("cli_died_before_work".into()); }
    }
    None
}


pub(crate) fn rate_limit_message(text: &str) -> bool {
    regex::Regex::new(r"(?i)rate.?limit|\b429\b|usage limit|quota|credit balance|hit your limit|out of extra usage")
        .expect("static rate limit regex").is_match(text)
}

pub(crate) fn rate_limit_error_envelope(event: &serde_json::Value) -> bool {
    let has_error = |value: &serde_json::Value| match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(fields) => !fields.is_empty(),
        serde_json::Value::Bool(flag) => *flag,
        _ => true,
    };
    match event.get("type").and_then(|value| value.as_str()) {
        Some("result") => {
            let failed = event.get("is_error").and_then(|value| value.as_bool()) == Some(true)
                || ["error", "errors"].iter().any(|field| event.get(field).is_some_and(has_error));
            failed && ["result", "error", "errors"].iter().any(|field|
                event.get(field).is_some_and(|value| rate_limit_message(&value.to_string())))
        }
        Some("error") => ["error", "message", "code"].iter().any(|field|event.get(field).is_some_and(|value|rate_limit_message(&value.to_string()))),
        Some("assistant") => ["error", "errors"].iter().any(|field|
            event.get(field).is_some_and(|value| rate_limit_message(&value.to_string()))),
        _ => false,
    }
}

fn exact_read_workspaces(req: &AttemptRequest, run: &Path, node: &Path) -> Result<Vec<PathBuf>, AttemptInfraError> {
    let mut paths = std::collections::BTreeSet::new();
    for path in &req.read_workspaces {
        let path = super::workspace::canonical_real_directory(path).map_err(|_| AttemptInfraError::IsolationUnavailable)?;
        if !path.starts_with(run) || path.file_name() != Some(std::ffi::OsStr::new("ws")) || path == node || path.starts_with(node) || node.starts_with(&path) {
            return Err(AttemptInfraError::IsolationUnavailable);
        }
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}
struct RetrySeed {
    _private: tempfile::TempDir,
    workspace: PathBuf,
    guard: super::workspace::IntegrityGuard,
    home: PathBuf,
    run_id: String,
    node: PathBuf,
    quota: super::attempt_container::QuotaLimits,
    deadline: std::time::Instant,
}
impl RetrySeed {
    fn capture(home:&Path,req:&AttemptRequest,quota:super::attempt_container::QuotaLimits,deadline:std::time::Instant)->Result<Self,AttemptInfraError> {
        let fail=|e:std::io::Error|AttemptInfraError::Spawn(e.to_string());
        let home=super::workspace::canonical_real_directory(home).map_err(fail)?;
        let run=super::workspace::canonical_real_directory(&req.run_dir).map_err(fail)?;
        let node=super::workspace::canonical_real_directory(&req.node_dir).map_err(fail)?;
        super::attempt_container::request_role(&home,&run,&node,req)?;
        if !node.starts_with(&run) || node.file_name()!=Some(std::ffi::OsStr::new("ws")) {return Err(AttemptInfraError::IsolationUnavailable);}
        let original=super::workspace::IntegrityGuard::capture(&node).map_err(fail)?;
        let bytes=super::workspace::tree_bytes(&node).map_err(fail)?;
        let (private,(workspace,guard))=super::attempt_container::allocate_associated_snapshot(&home,&req.run_id,quota,deadline,bytes,"retry-seeds",|path|{
            original.verify().map_err(fail)?;
            let workspace=path.join("seed");super::workspace::create_private_directory(&workspace).map_err(fail)?;
            duduclaw_fork::CopyPolicy::with_excludes(std::iter::empty::<&str>()).copy_tree(&node,&workspace)
                .map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
            original.verify().map_err(fail)?;
            if super::workspace::directory_sha256(&node).map_err(fail)?!=super::workspace::directory_sha256(&workspace).map_err(fail)? {
                return Err(AttemptInfraError::Spawn("retry seed copy integrity mismatch".into()));
            }
            let guard=super::workspace::IntegrityGuard::capture(&workspace).map_err(fail)?;
            Ok((workspace,guard))
        })?;
        Ok(Self {_private:private,workspace,guard,home,run_id:req.run_id.clone(),node,quota,deadline})
    }
    fn restore(&self,node:&Path)->Result<(),AttemptInfraError> {
        let fail=|e:std::io::Error|AttemptInfraError::Spawn(e.to_string());
        if node!=self.node {return Err(AttemptInfraError::IsolationUnavailable);}
        let root=self.home.join("discovery");
        let _lock=super::attempt_container::quota_lock(&root,self.deadline).map_err(fail)?;
        self.guard.verify().map_err(fail)?;
        let parent=node.parent().ok_or(AttemptInfraError::IsolationUnavailable)?;
        if super::workspace::canonical_real_directory(parent).map_err(fail)?!=parent
            || super::workspace::canonical_real_directory(node).map_err(fail)?!=node {return Err(AttemptInfraError::IsolationUnavailable);}
        let replacing=super::workspace::tree_bytes(node).map_err(fail)?;
        let bytes=super::workspace::tree_bytes(&self.workspace).map_err(fail)?;
        super::attempt_container::check_quota(&root,&self.run_id,self.quota,bytes.saturating_sub(replacing)).map_err(fail)?;
        std::fs::remove_dir_all(node).map_err(fail)?;
        super::workspace::create_private_directory(node).map_err(fail)?;
        // Direct strict copy has no OS-temp intermediate overlay to evade quota.
        duduclaw_fork::CopyPolicy::with_excludes(std::iter::empty::<&str>()).copy_tree(&self.workspace,node)
            .map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
        self.guard.verify().map_err(fail)?;
        if super::workspace::directory_sha256(&self.workspace).map_err(fail)?!=super::workspace::directory_sha256(node).map_err(fail)? {
            return Err(AttemptInfraError::Spawn("retry restore integrity mismatch".into()));
        }
        super::attempt_container::check_quota(&root,&self.run_id,self.quota,0).map_err(fail)?;
        if std::time::Instant::now()>=self.deadline {return Err(AttemptInfraError::BudgetExhausted);}
        Ok(())
    }
}

#[async_trait]
impl AttemptRunner for ClaudeAttemptRunner {
    async fn run_attempt(&self, req: &AttemptRequest) -> Result<AttemptOutcome, AttemptInfraError> {
        let run = req.run_dir.canonicalize().map_err(|_| AttemptInfraError::IsolationUnavailable)?;
        let node = req.node_dir.canonicalize().map_err(|_| AttemptInfraError::IsolationUnavailable)?;
        if node == run || !node.starts_with(&run) || req.timeout.is_zero() || req.max_turns == 0 {
            return Err(AttemptInfraError::IsolationUnavailable);
        }
        // Seatbelt cannot enforce namespace-wide descendant termination or
        // hard memory/process ceilings. Do not advertise it as production B1
        // isolation. The explicit operator experiment is the only current path.
        if !(self.allow_unconfined && self.operator_identity) {
            crate::security_autopilot::audit_and_emit(&self.home_dir,
                &duduclaw_security::audit::AuditEvent::new("discovery_isolation_refused", &req.agent_id,
                duduclaw_security::audit::Severity::Warning, serde_json::json!({"run_id":req.run_id,"cell_id":req.cell_id,"reason":"native_missing_memory_pids_and_descendant_boundary"})));
            return Err(AttemptInfraError::IsolationUnavailable);
        }
        let budget = self.budget.as_ref().ok_or(AttemptInfraError::BudgetExhausted)?;
        if budget.is_cancelled() || budget.remaining_wall().is_zero() {return Err(AttemptInfraError::BudgetExhausted);}
        let rotator = self.account_rotator.as_ref().ok_or(AttemptInfraError::NoAccount)?;
        let binary = binary_path(self.claude_bin.as_deref())?;
        let model = match req.model.as_ref() {
            Some(model) if !model.trim().is_empty() => model.clone(),
            _ => {
                let config = std::fs::read_to_string(self.home_dir.join("agents").join(&req.agent_id).join("agent.toml"))
                    .map_err(|e| AttemptInfraError::Spawn(e.to_string()))?;
                config.parse::<toml::Table>().ok()
                    .and_then(|v| v.get("model")?.get("preferred")?.as_str().map(str::to_owned))
                    .ok_or_else(|| AttemptInfraError::Spawn("no model configured for the discovery attempt".into()))?
            }
        };
        let read_workspaces = exact_read_workspaces(req, &run, &node)?;
        let read_guards = read_workspaces.iter().map(|path| super::workspace::IntegrityGuard::capture(path).map_err(|e| AttemptInfraError::Spawn(e.to_string()))).collect::<Result<Vec<_>, _>>()?;
        let seed_deadline=std::time::Instant::now().checked_add(req.timeout.min(budget.remaining_wall())).ok_or(AttemptInfraError::BudgetExhausted)?;
        let seed = RetrySeed::capture(&self.home_dir,req,self.quota,seed_deadline)?;
        let mut incurred = NodeCost { usd_source: CostSource::Reported, ..Default::default() };
        let mut last = String::new();
        for retry in 0..=2u32 {
            if retry > 0 { seed.restore(&node)?; }
            read_guards.iter().try_for_each(super::workspace::IntegrityGuard::verify).map_err(|e| AttemptInfraError::Spawn(e.to_string()))?;
            let account = rotator.select_with_pool(&req.account_pool).await.ok_or(AttemptInfraError::NoAccount)?;
            // General-purpose rotation is availability-first. Discovery pools are
            // strict so experimental traffic cannot spill into channel accounts.
            if !req.account_pool.is_empty() {
                let label = rotator.status().await.into_iter().find(|a| a.id == account.id).map(|a| a.label);
                if !req.account_pool.iter().any(|p| p.trim().eq_ignore_ascii_case(&account.id)
                    || label.as_ref().is_some_and(|l| p.trim().eq_ignore_ascii_case(l))) {
                    return Err(AttemptInfraError::NoAccount);
                }
            }
            let config_deadline=std::time::Instant::now().checked_add(req.timeout.min(budget.remaining_wall())).ok_or(AttemptInfraError::BudgetExhausted)?;
            let (private,(private_path,guard_private,settings,guard_integrity,empty))=super::attempt_container::allocate_controlled_snapshot(
                &self.home_dir,&req.run_id,self.quota,config_deadline,EMPTY_MCP_CONFIG.len() as u64,|path|{
                    let fail=|e:std::io::Error|AttemptInfraError::Spawn(e.to_string());
                    let private_path=path.join("runtime");let guard_private=path.join("guard");
                    super::workspace::create_private_directory(&private_path).map_err(fail)?;
                    super::workspace::create_private_directory(&guard_private).map_err(fail)?;
                    let settings=super::workspace::write_guard_settings_with_read_paths(&guard_private,&node,&run,&read_workspaces).map_err(fail)?;
                    let guard_integrity=super::workspace::IntegrityGuard::capture(&guard_private).map_err(fail)?;
                    let empty=private_path.join("mcp.json");std::fs::write(&empty,EMPTY_MCP_CONFIG).map_err(fail)?;
                    Ok((private_path,guard_private,settings,guard_integrity,empty))
                })?;
            let mut cmd = Command::new(&binary);
            cmd.args(build_claude_argv(&model, req.max_turns, &empty)).arg("--settings").arg(&settings).current_dir(&node)
                .env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs());
            for path in &read_workspaces { cmd.arg("--add-dir").arg(path); }
            for (key, value) in attempt_env_overlay(&account.env_vars, &private_path) {
                match value { Some(v) => { cmd.env(key, v); }, None => { cmd.env_remove(key); } }
            }
            // Explicitly erase ambient vendor configuration; only the selected
            // account's credentials are injected above.
            cmd.env_remove("PYTHONPATH").env_remove("LD_PRELOAD").env_remove("DYLD_INSERT_LIBRARIES");
            let unconfined = self.allow_unconfined && self.operator_identity;
            let isolation = if unconfined {
                crate::security_autopilot::audit_and_emit(&self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new("discovery_unconfined", &req.agent_id,
                    duduclaw_security::audit::Severity::Warning, serde_json::json!({"run_id":req.run_id,"cell_id":req.cell_id,"experimental":true,"os_boundary":false,"limitations":["unconfined_host_access","detached_descendants","memory_and_process_limits"]})));
                IsolationBackend::None
            } else {
                let config_dir = private_path.join("config");
                std::fs::create_dir(&config_dir).map_err(|e| AttemptInfraError::Spawn(e.to_string()))?;
                cmd.env("CLAUDE_CONFIG_DIR", &config_dir);
                let mut readonly = vec![guard_private.canonicalize().map_err(|e|AttemptInfraError::Spawn(e.to_string()))?, binary.parent().unwrap_or(&binary).to_path_buf()];
                if readonly.iter().any(|path| path.starts_with(&run) || run.starts_with(path)) { return Err(AttemptInfraError::IsolationUnavailable); }
                readonly.extend(read_workspaces.iter().cloned());
                for path in &self.extra_read_paths {
                    let path = path.canonicalize().map_err(|_| AttemptInfraError::IsolationUnavailable)?;
                    if path.starts_with(&run) || run.starts_with(&path) { return Err(AttemptInfraError::IsolationUnavailable); }
                    readonly.push(path);
                }
                confine_command(&mut cmd, &ConfinementSpec { readonly,
                    writable: vec![node.clone(), private_path.clone()], network: true,
                    cpu_secs: req.timeout.as_secs().max(1), memory_bytes: 4 * 1024 * 1024 * 1024,
                    pids: 256, allow_unconfined: false, operator_identity: false })
                    .map_err(|_| AttemptInfraError::IsolationUnavailable)?
            };
            let _lease_guard = acquire_attempt_slot(&self.home_dir, self.max_concurrency, budget, seed_deadline).await?;
            let cfg = duduclaw_core::dispatch_guard::DispatchGuardConfig::from_home(&self.home_dir);
            if let duduclaw_core::dispatch_guard::DispatchGuardDecision::Trip { reason, .. } =
                duduclaw_core::dispatch_guard::check_and_record(&self.home_dir,
                    duduclaw_core::dispatch_guard::PATH_KIND_DISCOVERY, &req.agent_id, &cfg) {
                return Err(AttemptInfraError::Spawn(format!("discovery circuit breaker: {reason}")));
            }
            // Admission and the dispatch guard may consume the attempt's
            // original deadline even while the overall run has budget left.
            let timeout = seed_deadline.saturating_duration_since(std::time::Instant::now()).min(budget.remaining_wall());
            if timeout.is_zero() { return Err(AttemptInfraError::BudgetExhausted); }
            let call = budget.reserve_call()?;
            let Some(ceiling) = budget.call_limit(call) else {
                budget.finish_accounted_call(call, 0.0, CostSource::Reported);
                return Err(AttemptInfraError::BudgetExhausted);
            };
            cmd.arg("--max-budget-usd").arg(format!("{ceiling:.8}"));
            let timeout = seed_deadline.saturating_duration_since(std::time::Instant::now()).min(budget.remaining_wall());
            if timeout.is_zero() {
                // A successful reservation may block on durable accounting.
                // No process has started: settle known-zero dollars, retaining
                // the conservative call count. Never refund spawned calls.
                budget.finish_accounted_call(call, 0.0, CostSource::Reported);
                return Err(AttemptInfraError::BudgetExhausted);
            }
            let mut summary = StreamSummary::default();
            let mut worked = false;
            let mut saw_result = false;
            let mut message_ids=std::collections::BTreeSet::new();
            let mut rate_limited = false;
            let execution = super::process::run(cmd, req.prompt.as_bytes(), timeout, 256 * 1024, |line| {
                let text = String::from_utf8_lossy(line);
                let parsed = parse_stream(&text);
                summary.events += parsed.events;
                let json = serde_json::from_slice::<serde_json::Value>(line).ok();
                if json.as_ref().and_then(|v| v.get("type")).and_then(|v|v.as_str()) == Some("result") {
                    saw_result = true;
                    if parsed.usage.is_some() || parsed.complete {summary.usage = parsed.usage;}
                    summary.complete=parsed.complete;
                    summary.total_cost_usd = parsed.total_cost_usd;
                    summary.final_text = parsed.final_text;
                    summary.is_error = parsed.is_error;
                } else if !saw_result {
                    let duplicate=json.as_ref().and_then(|v|v.get("message")).and_then(|m|m.get("id")).and_then(|v|v.as_str()).is_some_and(|id|!message_ids.insert(id.to_owned()));
                    if let Some(usage) = parsed.usage.filter(|_|!duplicate) {
                        let total = summary.usage.get_or_insert_with(Default::default);
                        total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
                        total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
                        total.cache_read_tokens = total.cache_read_tokens.saturating_add(usage.cache_read_tokens);
                        total.cache_creation_tokens = total.cache_creation_tokens.saturating_add(usage.cache_creation_tokens);
                    }
                }
                if parsed.model.is_some() { summary.model = parsed.model; }
                if json.as_ref().and_then(|v|v.get("message")).and_then(|v|v.get("content"))
                    .and_then(|v|v.as_array()).is_some_and(|blocks| blocks.iter().any(|b|b.get("type").and_then(|v|v.as_str())==Some("tool_use"))) { worked = true; }
                if json.as_ref().is_some_and(rate_limit_error_envelope) {
                    rate_limited = true;
                    budget.stop_for_rate_limit();
                    return false;
                }
                { let observed=usd_for(summary.model.as_deref().unwrap_or(&model), &summary);
                    if observed.is_finite() { budget.observe_cost(call, observed) } else { !budget.remaining_wall().is_zero() } }
            });
            let result = tokio::select! {
                result = execution => result,
                _ = budget.cancelled() => Err(std::io::Error::other("discovery run cancelled")),
            };
            let cost = call_cost(summary.model.as_deref().unwrap_or(&model), &summary);
            let usd = cost.usd;
            budget.finish_accounted_call(call, usd, cost.source);
            guard_integrity.verify().map_err(|_|AttemptInfraError::Spawn("attempt changed its guard settings".into()))?;
            seed.guard.verify().map_err(|_|AttemptInfraError::Spawn("attempt changed its private retry seed".into()))?;
            read_guards.iter().try_for_each(super::workspace::IntegrityGuard::verify).map_err(|_|AttemptInfraError::Spawn("attempt changed a completed read workspace".into()))?;
            accumulate_cost(&mut incurred, cost);
            if let Some(usage) = &summary.usage {
                incurred.input_tokens = incurred.input_tokens.saturating_add(usage.input_tokens.saturating_add(usage.cache_creation_tokens));
                incurred.output_tokens = incurred.output_tokens.saturating_add(usage.output_tokens);
                incurred.cache_read_tokens = incurred.cache_read_tokens.saturating_add(usage.cache_read_tokens);
                if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                    crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(crate::runtime::GoalRoundAttribution {
                        episode_id: req.run_id.clone(), round: req.cell_id.strip_prefix('r').and_then(|v|v.split('-').next()).and_then(|v|v.parse().ok())
                    }, telemetry.record(&req.agent_id, crate::cost_telemetry::RequestType::Dispatch,
                        summary.model.as_deref().unwrap_or(""), usage)).await;
                }
            }
            rate_limited |= result.as_ref().is_ok_and(|output|
                (summary.is_error || summary.final_text.is_none() || !output.status.success())
                && rate_limit_message(&output.stderr));
            if rate_limited {
                budget.stop_for_rate_limit();
                crate::security_autopilot::audit_and_emit(&self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new("discovery_rate_limit_stopped", &req.agent_id,
                        duduclaw_security::audit::Severity::Warning,
                        serde_json::json!({"run_id":req.run_id,"cell_id":req.cell_id,"retry":retry})));
                return Err(AttemptInfraError::RateLimited);
            }
            {
                let root=self.home_dir.join("discovery");
                let check_deadline=std::time::Instant::now()+std::time::Duration::from_millis(500);
                let _lock=super::attempt_container::quota_lock(&root,check_deadline).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
                super::attempt_container::check_quota(&root,&req.run_id,self.quota,0).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
            }
            // `private` owns runtime/config bytes throughout the call and check.
            let _=private.path();
            match result {
                Err(e) => last = format!("spawn_failed: {e}"),
                Ok(output) => {
                    incurred.wall_secs += output.wall_secs;
                    if output.stopped && (usd >= ceiling || budget.snapshot().spent_usd >= budget.limits().max_usd) {
                        return Err(AttemptInfraError::BudgetExhausted);
                    }
                    if let Some(reason) = infra_reason(&summary, &output.stderr, worked, output.timed_out) {
                        last = reason;
                        rotator.on_error(&account.id).await;
                    } else {
                        if usd.is_finite() { rotator.on_success(&account.id, (usd * 100.0).ceil() as u64).await; }
                        return Ok(AttemptOutcome { cost: incurred, runtime: "claude".into(),
                            model: summary.model.unwrap_or_default(), isolation,
                            final_text: cap_final_text(summary.final_text.as_deref().unwrap_or("")),
                            timed_out: output.timed_out, infra_retries: retry });
                    }
                }
            }
            if retry < 2 {
                let backoff = std::time::Duration::from_secs(1 << retry);
                if budget.remaining_wall() <= backoff { return Err(AttemptInfraError::BudgetExhausted); }
                tokio::time::sleep(backoff).await;
            }
        }
        Err(AttemptInfraError::RetriesExhausted { retries: 2, last })
    }
}

/// Runtime capability gate. No cross-family fallback occurs here.
#[derive(Clone)]
pub struct AttemptRunnerFactory {
    pub home_dir: PathBuf,
    pub attempt: AttemptSettings,
    pub quota:super::attempt_container::QuotaLimits,
    pub allow_unconfined: bool,
    pub operator_identity: bool,
    pub budget: Option<SharedBudget>,
    pub account_rotator: Option<Arc<AccountRotator>>,
    pub extra_read_paths: Vec<PathBuf>,
    pub max_concurrency: u32,
}
impl AttemptRunnerFactory {
    pub async fn for_run(home_dir: PathBuf, config: &DiscoveryConfig,
        budget: SharedBudget, operator_identity: bool) -> Result<Self, AttemptInfraError> {
        if config.account_pool.is_empty() && !(operator_identity && config.attempt.allow_shared_account_pool) { return Err(AttemptInfraError::NoAccount); }
        let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 120);
        rotator.load_from_config(&home_dir).await.map_err(AttemptInfraError::Spawn)?;
        Ok(Self { home_dir, attempt: config.attempt.clone(), quota:super::attempt_container::QuotaLimits {max_run_bytes:config.max_run_bytes,max_total_bytes:config.max_total_bytes}, allow_unconfined: config.allow_unconfined, operator_identity,
            budget: Some(budget), account_rotator: Some(Arc::new(rotator)),
            extra_read_paths: config.attempt_extra_read_paths.clone(), max_concurrency: 4 })
    }
    /// The same pure capability gate serves discovery creation, catalog and
    /// dispatch. It does not change interactive platform runtime support.
    /// Operator identity/account authorization is checked by the caller.
    pub fn check_runtime_capability(attempt:&AttemptSettings,runtime:&str)
        ->Result<super::attempt_adapter::RuntimeFamily,AttemptInfraError> {
        let family=super::attempt_adapter::RuntimeFamily::parse(runtime)?;
        if attempt.strict_usd {return Err(AttemptInfraError::StrictUsdUnsupported(runtime.into()));}
        match attempt.sandbox {
            AttemptSandbox::Native=>return Err(AttemptInfraError::IsolationUnavailable),
            // The operator-only host experiment uses Claude-specific hooks
            // and a host binary; other CLIs need their own native sandbox.
            AttemptSandbox::None if family!=super::attempt_adapter::RuntimeFamily::Claude=>return Err(AttemptInfraError::CapabilityUnsupported {
                runtime:runtime.into(),capability:"unconfined operator experiment".into()}),
            AttemptSandbox::Container=>{
                // Tool surface and the step ceiling of families without a
                // native flag are enforced on the host (attempt_guard).
                if !attempt.runtimes.contains_key(family.name()) {return Err(AttemptInfraError::IsolationUnavailable);}
            }
            _=>{},
        }
        // R1 (2026-10): a configured `gemini` attempt runtime still runs;
        // warn once per process where it is resolved (create/catalog/run),
        // not in `RuntimeFamily::parse`, which write paths share.
        if let Some(rt)=duduclaw_core::types::RuntimeType::from_id(family.name()) {
            crate::runtime_config::warn_once_if_deprecated_runtime(rt,crate::runtime_config::DeprecatedRuntimeSource::DiscoveryAttempt);
        }
        Ok(family)
    }
    pub fn for_runtime(&self, runtime: &str) -> Result<Box<dyn AttemptRunner>, AttemptInfraError> {
        let family=Self::check_runtime_capability(&self.attempt,runtime)?;
        match self.attempt.sandbox {
            AttemptSandbox::Container => {
                let image=self.attempt.runtimes.get(family.name()).cloned().ok_or(AttemptInfraError::IsolationUnavailable)?;
                Ok(Box::new(ContainerAttemptRunner { factory:self.clone(),family,image }))
            }
            AttemptSandbox::None if family==super::attempt_adapter::RuntimeFamily::Claude
                && self.allow_unconfined && self.operator_identity => Ok(Box::new(ClaudeAttemptRunner {
                home_dir:self.home_dir.clone(),quota:self.quota,claude_bin:None,allow_unconfined:true,operator_identity:true,
                budget:self.budget.clone(),account_rotator:self.account_rotator.clone(),
                extra_read_paths:self.extra_read_paths.clone(),max_concurrency:self.max_concurrency,
            })),
            _ => Err(AttemptInfraError::IsolationUnavailable),
        }
    }
}

#[cfg(test)]
#[path = "tests_agent_spawn.rs"]
mod tests;

struct ContainerAttemptRunner {
    factory:AttemptRunnerFactory,
    family:super::attempt_adapter::RuntimeFamily,
    image:super::config::AttemptRuntimeConfig,
}
#[async_trait]
impl AttemptRunner for ContainerAttemptRunner {
    async fn run_attempt(&self,req:&AttemptRequest)->Result<AttemptOutcome,AttemptInfraError> {
        let factory=&self.factory;
        let budget=factory.budget.as_ref().ok_or(AttemptInfraError::BudgetExhausted)?;
        if budget.is_cancelled() || budget.remaining_wall().is_zero() {return Err(AttemptInfraError::BudgetExhausted);}
        let rotator=factory.account_rotator.as_ref().ok_or(AttemptInfraError::NoAccount)?;
        if req.account_pool.is_empty() && !(factory.operator_identity && factory.attempt.allow_shared_account_pool) {
            return Err(AttemptInfraError::NoAccount);
        }
        let model=req.model.as_ref().filter(|m|!m.trim().is_empty()).ok_or_else(||AttemptInfraError::Spawn("no model configured".into()))?;
        if req.timeout.is_zero() || req.max_turns==0 { return Err(AttemptInfraError::IsolationUnavailable); }
        let node=super::workspace::canonical_real_directory(&req.node_dir).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
        let run=super::workspace::canonical_real_directory(&req.run_dir).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
        let read_paths=exact_read_workspaces(req,&run,&node)?;
        let read_guards=read_paths.iter().map(|path|super::workspace::IntegrityGuard::capture(path).map_err(|e|AttemptInfraError::Spawn(e.to_string()))).collect::<Result<Vec<_>,_>>()?;
        let seed_deadline=std::time::Instant::now().checked_add(req.timeout.min(budget.remaining_wall())).ok_or(AttemptInfraError::BudgetExhausted)?;
        let seed=RetrySeed::capture(&factory.home_dir,req,factory.quota,seed_deadline)?;
        let mut incurred=NodeCost {usd_source:CostSource::Reported,..Default::default()};
        let mut last=String::new();
        let mut rejected=std::collections::BTreeSet::new();
        let mut next=None;
        for retry in 0..=2 {
            if retry>0 {seed.restore(&node)?;}
            read_guards.iter().try_for_each(super::workspace::IntegrityGuard::verify).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
            let timeout=req.timeout.min(budget.remaining_wall());
            let (account,env)=match next.take() { Some(found)=>found, None=>self.usable_account(rotator,req,&rejected).await? };
            let argv=self.family.argv(model,req.max_turns,&node);
            let files=super::attempt_adapter::runtime_files(self.family,&node,req.max_turns,&req.prompt);
            let prepared=super::attempt_container::prepare_with_files(&factory.home_dir,req,&factory.attempt,factory.quota,&self.image,&argv,&env,&files,timeout)?;
            drop(env);
            let _lease=acquire_attempt_slot(&factory.home_dir,factory.max_concurrency,budget,seed_deadline).await?;
            let guard=duduclaw_core::dispatch_guard::DispatchGuardConfig::from_home(&factory.home_dir);
            if let duduclaw_core::dispatch_guard::DispatchGuardDecision::Trip{reason,..}=duduclaw_core::dispatch_guard::check_and_record(&factory.home_dir,duduclaw_core::dispatch_guard::PATH_KIND_DISCOVERY,&req.agent_id,&guard) {
                return Err(AttemptInfraError::Spawn(reason));
            }
            let timeout=seed_deadline.saturating_duration_since(std::time::Instant::now()).min(budget.remaining_wall());
            if timeout.is_zero(){return Err(AttemptInfraError::BudgetExhausted);}
            let call=budget.reserve_call()?;
            let family=self.family;
            let mut adapter=super::attempt_adapter::StreamAdapter::default();
            let mut stream_guard=super::attempt_guard::StreamGuard::new(family,req.max_turns);
            let mut transcript=String::new();let mut worked=false;let mut rate_limited=false;
            let mut stop:Option<StopCause>=None;let mut unparsable=0u32;
            let payload=super::attempt_adapter::stdin_payload(family,&req.prompt);
            let execution=prepared.run(&payload,timeout,|line| {
                if line.iter().all(u8::is_ascii_whitespace) {return true;}
                // Undecodable output could hide a tool call from the guard.
                let native=match serde_json::from_slice::<serde_json::Value>(line) {
                    Ok(native) if native.is_object()=>native,
                    _=>{
                        unparsable+=1;
                        if unparsable>MAX_UNPARSABLE_LINES {stop=Some(StopCause::ToolViolation(UNPARSABLE_STREAM.into()));return false;}
                        return true;
                    }
                };
                // The guard reads the native event; normalisation drops detail.
                let verdict=stream_guard.observe(&native);
                let event=adapter.normalize(family,native);
                worked|=event["message"]["content"].as_array().is_some_and(|blocks|blocks.iter().any(|b|b["type"]=="tool_use"));
                transcript.push_str(&event.to_string());transcript.push('\n');
                match verdict {
                    super::attempt_guard::GuardVerdict::ToolViolation{tool}=>{stop=Some(StopCause::ToolViolation(tool));return false;}
                    super::attempt_guard::GuardVerdict::StepLimit=>{
                        // agy: every generation's usage is in the stream. Codex
                        // reports usage only per turn, so its total stays unknown.
                        transcript.push_str(&adapter.synthetic_result().to_string());transcript.push('\n');
                        stop=Some(StopCause::StepLimit);return false;
                    }
                    super::attempt_guard::GuardVerdict::Continue=>{}
                }
                // Bounded normalized transcript; never silently discard evidence.
                if transcript.len()>1024*1024 {stop=Some(StopCause::TranscriptOverflow);return false;}
                if rate_limit_error_envelope(&event) {rate_limited=true;budget.stop_for_rate_limit();stop=Some(StopCause::RateLimited);return false;}
                let summary=parse_stream(&transcript);let observed=usd_for_family(family,summary.model.as_deref().unwrap_or(model),&summary);
                let keep=if observed.is_finite() {budget.observe_cost(call,observed)}else{!budget.remaining_wall().is_zero()};
                if !keep {stop=Some(StopCause::Budget);}
                keep
            });
            let result=tokio::select! { result=execution=>result, _=budget.cancelled()=>Err(AttemptInfraError::BudgetExhausted) };
            // A natural end (or the wall clock) with a forbidden tool whose
            // hook denial was never confirmed is a violation.
            if stop.is_none() && result.is_ok() {
                if let super::attempt_guard::GuardVerdict::ToolViolation{tool}=stream_guard.finish() { stop=Some(StopCause::ToolViolation(tool)); }
            }
            let summary=parse_stream(&transcript);let cost=call_cost_for(family,summary.model.as_deref().unwrap_or(model),&summary);
            budget.finish_accounted_call(call,cost.usd,cost.source);accumulate_cost(&mut incurred,cost);
            seed.guard.verify().map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
            read_guards.iter().try_for_each(super::workspace::IntegrityGuard::verify).map_err(|e|AttemptInfraError::Spawn(e.to_string()))?;
            if let Some(usage)=&summary.usage {
                incurred.input_tokens=incurred.input_tokens.saturating_add(usage.input_tokens.saturating_add(usage.cache_creation_tokens));
                incurred.output_tokens=incurred.output_tokens.saturating_add(usage.output_tokens);incurred.cache_read_tokens=incurred.cache_read_tokens.saturating_add(usage.cache_read_tokens);
                if let Some(telemetry)=crate::cost_telemetry::get_telemetry() {
                    crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(crate::runtime::GoalRoundAttribution {
                        episode_id:req.run_id.clone(),round:req.cell_id.strip_prefix('r').and_then(|v|v.split('-').next()).and_then(|v|v.parse().ok())
                    },telemetry.record(&req.agent_id,crate::cost_telemetry::RequestType::Dispatch,summary.model.as_deref().unwrap_or(""),usage)).await;
                }
            }
            if let Err(AttemptInfraError::CleanupFailed(reason))=&result {return Err(AttemptInfraError::CleanupFailed(reason.clone()));}
            if let Some(StopCause::ToolViolation(tool))=&stop {
                // Contract breach: the attempt is void and never retried.
                crate::security_autopilot::audit_and_emit(&factory.home_dir,
                    &duduclaw_security::audit::AuditEvent::new("discovery_tool_surface_violation", &req.agent_id,
                        duduclaw_security::audit::Severity::Warning,
                        serde_json::json!({"run_id":req.run_id,"cell_id":req.cell_id,"runtime":family.name(),"tool":tool,"retry":retry})));
                return Err(AttemptInfraError::ToolSurfaceViolation {runtime:family.name().into(),tool:tool.clone()});
            }
            rate_limited|=result.as_ref().is_ok_and(|o|(!o.status.success() || summary.is_error) && rate_limit_message(&o.stderr));
            if rate_limited {budget.stop_for_rate_limit();return Err(AttemptInfraError::RateLimited);}
            match result {
                Ok(output)=>{
                    incurred.wall_secs+=output.wall_secs;
                    // The trusted PID 1 refused its own setup: retrying replays the same input.
                    if output.status.code()==Some(SUPERVISOR_SETUP_FAILED) && transcript.is_empty() && unparsable==0 {
                        return Err(AttemptInfraError::Spawn("attempt supervisor setup failed".into()));
                    }
                    let step_limit=matches!(stop,Some(StopCause::StepLimit));
                    if (output.stopped && !step_limit) || transcript.len()>1024*1024 {return Err(AttemptInfraError::BudgetExhausted);}
                    if !worked && !step_limit && auth_failure(&transcript,&output.stderr) {
                        // Never replay a credential the provider just rejected.
                        rotator.on_error(&account.id).await;
                        rejected.insert(account.id.clone());
                        next=Some(self.usable_account(rotator,req,&rejected).await?);
                        last="provider_rejected_credentials".into();
                        continue;
                    }
                    // A step-limit stop is scored like Claude's error_max_turns.
                    let reason=if step_limit {None} else {infra_reason(&summary,&output.stderr,worked,output.timed_out)};
                    if let Some(reason)=reason {last=reason;rotator.on_error(&account.id).await;}
                    else { if cost.usd.is_finite(){rotator.on_success(&account.id,(cost.usd*100.0).ceil() as u64).await;}
                        return Ok(AttemptOutcome {cost:incurred,runtime:family.name().into(),model:summary.model.unwrap_or_default(),
                            isolation:IsolationBackend::Container,final_text:cap_final_text(summary.final_text.as_deref().unwrap_or("")),
                            timed_out:output.timed_out && !step_limit,infra_retries:retry});
                    }
                }
                Err(error)=>{if budget.remaining_wall().is_zero(){return Err(AttemptInfraError::BudgetExhausted);}last=error.to_string();}
            }
            if retry<2 {let backoff=Duration::from_secs(1<<retry);if budget.remaining_wall()<=backoff{return Err(AttemptInfraError::BudgetExhausted);}tokio::select! {_=tokio::time::sleep(backoff)=>{},_=budget.cancelled()=>return Err(AttemptInfraError::BudgetExhausted)};}
        }
        Err(AttemptInfraError::RetriesExhausted {retries:2,last})
    }
}

impl ContainerAttemptRunner {
    /// Bounded walk over the pool: an account of the right provider without a
    /// usable credential for this family (no key, no or an invalid login
    /// document) or already rejected in this attempt is skipped. The strict
    /// dedicated-pool membership check still ends the attempt.
    async fn usable_account(&self,rotator:&AccountRotator,req:&AttemptRequest,rejected:&std::collections::BTreeSet<String>)
        ->Result<(duduclaw_agent::account_rotator::AccountEnv,BTreeMap<String,String>),AttemptInfraError> {
        use super::attempt_adapter::{self as adapter, RuntimeFamily};
        let provider=if self.family==RuntimeFamily::OpenAiCompat {self.image.provider.as_deref().unwrap_or("openai")}else{self.family.provider()};
        let base_url=if self.family==RuntimeFamily::OpenAiCompat {
            Some(adapter::compatible_endpoint(self.image.base_url.as_deref().ok_or(AttemptInfraError::IsolationUnavailable)?)?)
        } else {None};
        let mut seen=std::collections::BTreeSet::new();
        for _ in 0..MAX_ACCOUNT_SELECTIONS {
            let Some(account)=rotator.select_for_provider_with_pool(provider,&req.account_pool).await else {break};
            if !seen.insert(account.id.clone()) {break;}
            if !req.account_pool.is_empty() {
                let label=rotator.status().await.into_iter().find(|a|a.id==account.id).map(|a|a.label);
                if !req.account_pool.iter().any(|p|p.trim().eq_ignore_ascii_case(&account.id) || label.as_ref().is_some_and(|l|p.trim().eq_ignore_ascii_case(l))) {return Err(AttemptInfraError::NoAccount);}
            }
            if rejected.contains(&account.id) {continue;}
            let mut selected=account.env_vars.clone();
            if self.family==RuntimeFamily::OpenAiCompat {
                if let Some(key)=duduclaw_core::provider_env::provider_env_key_names(provider).first().and_then(|name|account.env_vars.get(*name)) { selected.insert("OPENAI_API_KEY".into(),key.clone()); }
            }
            let mut env=adapter::environment(self.family,&selected);
            if let Some(url)=&base_url {env.insert("DUDU_ATTEMPT_BASE_URL".into(),url.clone());}
            // A subscription login document, never logged (§6).
            let Ok(credential)=adapter::credential_document(self.family,account.seat_token.as_deref()) else {continue};
            if !adapter::has_credentials(self.family,&env,credential.as_deref()) {continue;}
            if let (Some(doc),Some(dest))=(credential,adapter::credential_destination(self.family)) {
                env.insert(adapter::CREDENTIAL_DOC_ENV.into(),doc);
                env.insert(adapter::CREDENTIAL_DEST_ENV.into(),dest.into());
            }
            return Ok((account,env));
        }
        Err(AttemptInfraError::NoAccount)
    }
}

const MAX_ACCOUNT_SELECTIONS: usize = 8;
/// Non-JSON stdout lines tolerated per attempt before the stream is void.
pub(crate) const MAX_UNPARSABLE_LINES: u32 = 3;
pub(crate) const UNPARSABLE_STREAM: &str = "unparsable_stream";
/// attempt_supervisor.py exit code for a refused setup.
pub(crate) const SUPERVISOR_SETUP_FAILED: i32 = 125;

/// Provider rejected the credential. Read only for an attempt that did no
/// work: stderr plus the CLI's own error fields, never assistant text.
pub(crate) fn auth_failure(transcript: &str, stderr: &str) -> bool {
    let mut blob = stderr.to_owned();
    for event in transcript.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()) {
        let fields: &[&str] = match event["type"].as_str() {
            Some("result") if event["is_error"].as_bool() == Some(true) => &["result", "error", "errors"],
            Some("error") => &["error", "message"],
            _ => continue,
        };
        for field in fields { if let Some(value) = event.get(*field).filter(|v| !v.is_null()) { blob.push(' '); blob.push_str(&value.to_string()); } }
    }
    regex::Regex::new(r"(?i)not signed in|incorrect api key|api key not valid|unauthorized|not logged in|please run /login|invalid api key")
        .expect("static auth regex").is_match(&blob)
}

/// Why the event callback stopped an attempt (§4). Budget, transcript
/// overflow and rate limits keep their earlier outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopCause { Budget, TranscriptOverflow, RateLimited, StepLimit, ToolViolation(String) }
