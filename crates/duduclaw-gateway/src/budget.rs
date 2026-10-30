//! Budget circuit breaker — from cost *observation* to cost *enforcement*.
//!
//! `CostTelemetry` records spend and can nudge routing, but nothing stops an
//! agent that has blown its budget. 2026 FinOps consensus is a hard kill switch,
//! not just an alert (single-month multi-hundred-million-dollar overruns are on
//! record). This module adds that switch at the LLM dispatch choke-points.
//!
//! ## Model
//!
//! A two-state breaker with a *time-based* reset (no manual cooldown needed):
//! - **Closed** (Allow): rolling spend is under the cap.
//! - **Open** (Deny): rolling spend ≥ cap AND `hard_stop` is set. It re-closes
//!   automatically when the rolling window (24h daily / 30d monthly) slides the
//!   spend back under the cap — the window *is* the cooldown.
//!
//! Two caps are enforced independently: `daily_cap_cents` (rolling 24h) and
//! `monthly_limit_cents` (rolling 30d). Either being exceeded (with `hard_stop`)
//! trips the breaker.
//!
//! Unavailable accounting is explicit and follows `[budget].when_unavailable`.
//! Its default is `allow`; `deny` stops model calls until accounting recovers.

use std::collections::HashMap;
use std::path::Path;

use crate::cost_telemetry::{CostTelemetry, get_telemetry, init_telemetry};

/// Hours in the rolling monthly window.
const MONTHLY_WINDOW_HOURS: u64 = 24 * 30;
/// Hours in the rolling daily window.
const DAILY_WINDOW_HOURS: u64 = 24;

/// Effective budget limits for one agent (from `agent.toml [budget]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    /// Rolling-24h hard cap in cents (0 = no daily cap).
    pub daily_cap_cents: u64,
    /// Rolling-30d hard cap in cents (0 = no monthly cap).
    pub monthly_limit_cents: u64,
    /// Warn (not block) at this percent of a cap (0 = no warn).
    pub warn_threshold_percent: u8,
    /// Master switch: only when true does an exceeded cap actually *block*.
    pub hard_stop: bool,
}

impl BudgetLimits {
    /// True when no cap can ever fire (nothing to check — skip telemetry).
    fn is_inert(&self) -> bool {
        (self.daily_cap_cents == 0 && self.monthly_limit_cents == 0) || !self.enforceable()
    }
    /// A cap can block only if hard_stop is on and at least one cap is set.
    fn enforceable(&self) -> bool {
        self.hard_stop && (self.daily_cap_cents > 0 || self.monthly_limit_cents > 0)
    }
}

/// The breaker's decision for one prospective LLM call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// Under budget — proceed.
    Allow,
    /// Accounting or effective configuration could not be read.
    Unavailable { reason: String },
    /// Over a cap with `hard_stop` — block. Carries which window tripped and the
    /// numbers, for a user-facing message and the audit trail.
    Deny {
        /// `"daily"` or `"monthly"`.
        scope: &'static str,
        spent_cents: u64,
        cap_cents: u64,
    },
}

impl BudgetVerdict {
    pub fn is_denied(&self) -> bool {
        matches!(self, BudgetVerdict::Deny { .. })
    }

    /// A user-facing zh-TW message for a denial (empty for Allow). Deliberately
    /// no internal paths/agent-ids — just the budget fact the end user needs.
    /// Wording matches the platform's external-facing state vocabulary
    /// ("已停工（花費達上限）") and — since [`check_agent_budget`] now pushes
    /// an admin alert on the same transition — truthfully tells the person
    /// talking to the agent that the admin already knows, instead of leaving
    /// them to wonder why the agent went quiet.
    pub fn user_message(&self) -> String {
        match self {
            BudgetVerdict::Allow => String::new(),
            BudgetVerdict::Unavailable { .. } => "⚠️ 暫停工作：目前無法確認預算，請管理員檢查預算設定或花費紀錄後重試。".into(),
            BudgetVerdict::Deny {
                scope,
                spent_cents,
                cap_cents,
            } => {
                let window = if *scope == "daily" {
                    "今日"
                } else {
                    "本月"
                };
                format!(
                    "⚠️ 已停工（花費達上限）。我目前因為花費達到上限暫停工作，已通知管理員。\
                     （{window}已使用 US${:.2} / 上限 US${:.2}，額度會在時間窗滑動後自動恢復）",
                    *spent_cents as f64 / 100.0,
                    *cap_cents as f64 / 100.0,
                )
            }
        }
    }
}

/// Load `[budget]` limits from an agent's `agent.toml`. Missing file /
/// section ⇒ inert limits (never blocks).
///
/// Goes through the shared typed parse point ([`duduclaw_core::agent_toml`])
/// rather than a hand-rolled `toml::Value` walk. Two properties of the old
/// reader are load-bearing and are carried by the view's field *types*, not
/// by convention:
///
/// * **absent ≠ wrong-typed.** A present-but-wrong-typed key must warn
///   LOUDLY; an absent one must stay silent. Hence
///   [`duduclaw_core::lenient::Tri`] rather than `Option`.
/// * **integer ≠ float.** An integer is clamped, a float (the common `100.0`
///   typo) is rounded. Hence [`duduclaw_core::lenient::TomlNumber`] rather
///   than `f64`, which would also lose precision past 2^53.
///
/// A `[budget]` section that is absent entirely and one that is present but
/// scalar (`budget = 1`) both produce the all-zero inert result, exactly as
/// before — `toml::Value::get` on a non-table returned `None` for every key.
pub fn load_budget_limits(agent_dir: Option<&Path>) -> BudgetLimits {
    let inert = BudgetLimits {
        daily_cap_cents: 0,
        monthly_limit_cents: 0,
        warn_threshold_percent: 0,
        hard_stop: false,
    };
    let Some(dir) = agent_dir else {
        return inert;
    };
    budget_limits_from_view(duduclaw_core::agent_toml::load(dir).budget)
}

fn budget_limits_from_view(b: duduclaw_core::agent_toml::BudgetSectionView) -> BudgetLimits {
    use duduclaw_core::lenient::{TomlFlag, TomlNumber, Tri};
    // Coerce ints, and floats (a common `100.0` typo) → rounded u64. A key that
    // is present but a genuinely wrong type is logged LOUDLY rather than silently
    // treated as 0 — a config typo must not silently disable a cost control.
    let num_of = |k: &str, field: Tri<TomlNumber>| -> u64 {
        match field {
            Tri::Absent => 0,
            Tri::Value(TomlNumber::Int(i)) => i.max(0) as u64,
            Tri::Value(TomlNumber::Float(f)) => f.max(0.0).round() as u64,
            Tri::WrongType => {
                tracing::warn!(
                    key = k,
                    "budget: [budget].{k} has an unexpected type — treated as no cap; \
                     it must be an integer number of cents"
                );
                0
            }
        }
    };
    let hard_stop = match b.hard_stop {
        Tri::Absent => false,
        Tri::Value(TomlFlag::Bool(v)) => v,
        // Tolerate `hard_stop = 1` / `0` (common mistake) but warn.
        Tri::Value(TomlFlag::Int(i)) => {
            tracing::warn!("budget: [budget].hard_stop should be a bool; coercing integer");
            i != 0
        }
        Tri::WrongType => {
            tracing::warn!("budget: [budget].hard_stop has an unexpected type — treated as false");
            false
        }
    };
    BudgetLimits {
        daily_cap_cents: num_of("daily_cap_cents", b.daily_cap_cents),
        monthly_limit_cents: num_of("monthly_limit_cents", b.monthly_limit_cents),
        warn_threshold_percent: num_of("warn_threshold_percent", b.warn_threshold_percent).min(100)
            as u8,
        hard_stop,
    }
}

/// Aggregate millicents before converting, including the ephemeral family.
async fn spent_cents(tel: &CostTelemetry, agent_id: &str, hours: u64) -> Result<u64, String> {
    tel.budget_spent_millicents(agent_id, hours).await.map(|cost| cost / 1000)
}

/// Missing agent directories and configurations have no configured budget.
/// Existing unreadable or malformed configurations remain unavailable.
fn read_budget_config(agent_dir: Option<&Path>) -> Result<duduclaw_core::agent_toml::AgentTomlSections, String> {
    let Some(dir) = agent_dir else { return Ok(Default::default()); };
    let metadata = match std::fs::metadata(dir) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(dir) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
                _ => Err(format!("agent directory: {e}")),
            };
        },
        Err(e) => return Err(format!("agent directory: {e}")),
    };
    if !metadata.is_dir() { return Err("agent directory is not a directory".into()); }
    match duduclaw_core::agent_toml::read_effective_text(dir) {
        Ok(text) => {
            let doc: toml::Value = toml::from_str(&text)
                .map_err(|_| "agent budget configuration is malformed".to_string())?;
            if doc.get("budget").is_some_and(|budget| !budget.is_table()) {
                return Err("agent budget section is not a table".into());
            }
            toml::from_str(&text).map_err(|_| "agent budget configuration is malformed".to_string())
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(dir.join("agent.toml")) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
                _ => Err(format!("agent budget configuration: {e}")),
            }
        },
        Err(e) => Err(format!("agent budget configuration: {e}")),
    }
}

#[cfg(test)]
fn validate_budget_config(agent_dir: Option<&Path>) -> Result<(), String> {
    read_budget_config(agent_dir).map(|_| ())
}

/// Refusal is returned before runtime rotation or provider classification.
#[derive(Debug, Clone)]
pub struct BudgetRefusal(pub BudgetVerdict);

impl std::fmt::Display for BudgetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.user_message())
    }
}
impl std::error::Error for BudgetRefusal {}

/// Match registry placement for ordinary and ephemeral employees.
pub fn agent_dir_for_id(home: &Path, agent_id: &str) -> std::path::PathBuf {
    let agents = home.join("agents");
    if crate::ephemeral::is_ephemeral_id(agent_id) {
        agents.join(crate::ephemeral::EPHEMERAL_DIR_NAME).join(agent_id)
    } else {
        agents.join(agent_id)
    }
}

pub const BUDGET_REFUSAL_PREFIX: &str = "[budget_preflight] ";

impl BudgetRefusal {
    /// Preserve refusal provenance across legacy String return values.
    pub fn legacy_message(&self) -> String {
        format!("{BUDGET_REFUSAL_PREFIX}{self}")
    }
}

pub fn budget_refusal_message(error: &str) -> Option<&str> {
    error.strip_prefix(BUDGET_REFUSAL_PREFIX)
}


#[cfg(test)]
fn unavailable_denied(agent_dir: Option<&Path>) -> bool {
    // An unreadable policy cannot authorize a fail-open decision.
    read_budget_config(agent_dir).map(|config| config.budget.when_unavailable
        == duduclaw_core::types::BudgetWhenUnavailable::Deny).unwrap_or(true)
}

pub async fn gate_before_spawn(
    home: &Path, agent_dir: Option<&Path>, agent_id: &str, lane: &str,
) -> Result<(), BudgetRefusal> {
    if agent_id.is_empty() {
        tracing::debug!(lane, "budget: no employee identity; skipping employee budget");
        return Ok(());
    }
    let (verdict, denied) = match read_budget_config(agent_dir) {
        Ok(config) => {
            let denied = config.budget.when_unavailable == duduclaw_core::types::BudgetWhenUnavailable::Deny;
            (check_budget_with_config(home, agent_dir, agent_id, config).await, denied)
        }
        Err(reason) => (BudgetVerdict::Unavailable { reason }, true),
    };
    finish_budget_gate(home, agent_id, lane, verdict, denied)
}

fn finish_budget_gate(
    home: &Path, agent_id: &str, lane: &str, verdict: BudgetVerdict, denied: bool,
) -> Result<(), BudgetRefusal> {
    match &verdict {
        BudgetVerdict::Allow => Ok(()),
        BudgetVerdict::Unavailable { reason } => {
            tracing::warn!(agent_id, lane, reason, denied, "budget unavailable");
            append_unavailable_event(home, agent_id, lane, reason, denied);
            if denied { Err(BudgetRefusal(verdict)) } else { Ok(()) }
        }
        BudgetVerdict::Deny { .. } => Err(BudgetRefusal(verdict)),
    }
}

fn append_unavailable_event(home: &Path, agent_id: &str, lane: &str, reason: &str, denied: bool) {
    if !home.is_absolute() { return; }
    let path = home.join("budget_events.jsonl");
    let line = serde_json::json!({"ts": chrono::Utc::now().to_rfc3339(),
        "agent_id": agent_id, "event": "budget_unavailable", "lane": lane,
        "reason": reason, "denied": denied}).to_string();
    if let Err(error) = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{line}")
    }) {
        tracing::warn!(agent_id, lane, %error, "budget unavailable audit write failed");
    }
}

/// Pure evaluation against a telemetry handle — the unit-testable core. Checks
/// the daily cap first (tighter window), then monthly.
pub async fn evaluate_budget(
    tel: &CostTelemetry,
    agent_id: &str,
    limits: &BudgetLimits,
) -> BudgetVerdict {
    if !limits.enforceable() {
        return BudgetVerdict::Allow;
    }
    if limits.daily_cap_cents > 0 {
        let spent = match spent_cents(tel, agent_id, DAILY_WINDOW_HOURS).await {
            Ok(spent) => spent,
            Err(reason) => return BudgetVerdict::Unavailable { reason },
        };
        if spent >= limits.daily_cap_cents {
            return BudgetVerdict::Deny {
                scope: "daily",
                spent_cents: spent,
                cap_cents: limits.daily_cap_cents,
            };
        }
        warn_if_approaching(
            agent_id,
            "daily",
            spent,
            limits.daily_cap_cents,
            limits.warn_threshold_percent,
        );
    }
    if limits.monthly_limit_cents > 0 {
        let spent = match spent_cents(tel, agent_id, MONTHLY_WINDOW_HOURS).await {
            Ok(spent) => spent,
            Err(reason) => return BudgetVerdict::Unavailable { reason },
        };
        if spent >= limits.monthly_limit_cents {
            return BudgetVerdict::Deny {
                scope: "monthly",
                spent_cents: spent,
                cap_cents: limits.monthly_limit_cents,
            };
        }
        warn_if_approaching(
            agent_id,
            "monthly",
            spent,
            limits.monthly_limit_cents,
            limits.warn_threshold_percent,
        );
    }
    BudgetVerdict::Allow
}

async fn evaluate_available_telemetry(
    telemetry: Option<&CostTelemetry>, agent_id: &str, limits: &BudgetLimits,
) -> BudgetVerdict {
    match telemetry {
        Some(tel) => evaluate_budget(tel, agent_id, limits).await,
        None => BudgetVerdict::Unavailable { reason: "cost telemetry unavailable".into() },
    }
}

/// Log a soft warning when spend crosses `warn_threshold_percent` of a cap
/// (but has not yet hit it). 0 threshold disables the warning.
fn warn_if_approaching(agent_id: &str, scope: &str, spent: u64, cap: u64, pct: u8) {
    if pct == 0 || cap == 0 {
        return;
    }
    // threshold = cap * pct / 100, computed to avoid overflow on large caps.
    let threshold = (cap as u128 * pct as u128 / 100) as u64;
    if spent >= threshold {
        tracing::warn!(
            agent_id,
            scope,
            spent_cents = spent,
            cap_cents = cap,
            pct,
            "budget approaching cap"
        );
    }
}

/// Top-level gate for a dispatch choke-point: resolve the agent's limits, then
/// evaluate against the global telemetry singleton (initialising it if needed).
///
/// No identity or inert limits allow; configuration/accounting failures stay explicit.
pub async fn check_agent_budget(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
) -> BudgetVerdict {
    if agent_id.is_empty() {
        tracing::debug!("budget: no employee identity; skipping employee budget");
        return BudgetVerdict::Allow;
    }
    let config = match read_budget_config(agent_dir) {
        Ok(config) => config,
        Err(reason) => return BudgetVerdict::Unavailable { reason },
    };
    check_budget_with_config(home_dir, agent_dir, agent_id, config).await
}

async fn check_budget_with_config(
    home_dir: &Path, agent_dir: Option<&Path>, agent_id: &str,
    config: duduclaw_core::agent_toml::AgentTomlSections,
) -> BudgetVerdict {
    // D12 (2026-09 feature audit): relative burn-rate check, deliberately
    // BEFORE the `is_inert` short-circuit below. Fixed caps and burn-rate
    // anomalies answer different questions, and an agent with no cap
    // configured is exactly the one whose runaway spend nothing else would
    // notice. Self-throttled to one query per agent per hour and to one alert
    // per agent per UTC day; warns, never blocks (see `cost_anomaly`).
    crate::cost_anomaly::maybe_scan(home_dir, agent_dir, agent_id).await;

    let limits = budget_limits_from_view(config.budget);
    if limits.is_inert() {
        return BudgetVerdict::Allow;
    }
    // Ensure telemetry exists (lazy init mirrors record_usage). Keep failures
    // explicit so the spawn gate can apply the employee policy.
    if get_telemetry().is_none() {
        let _ = init_telemetry(home_dir);
    }
    let verdict = evaluate_available_telemetry(get_telemetry(), agent_id, &limits).await;
    match &verdict {
        BudgetVerdict::Unavailable { .. } => {}
        BudgetVerdict::Deny {
            scope,
            spent_cents,
            cap_cents,
        } => {
            tracing::warn!(
                agent_id,
                scope,
                spent_cents,
                cap_cents,
                "budget circuit breaker OPEN — blocking LLM call"
            );
            append_budget_event(home_dir, agent_id, scope, *spent_cents, *cap_cents);
            notify_breaker_transition(home_dir, agent_dir, agent_id, true).await;
        }
        // Only reached once `limits.is_inert()` was already false above, i.e.
        // enforcement genuinely ran — an Allow here is a real "still/again
        // under budget" result, worth checking for an OPEN→CLOSED recovery,
        // not the earlier short-circuit for agents with no cap configured.
        BudgetVerdict::Allow => {
            notify_breaker_transition(home_dir, agent_dir, agent_id, false).await;
        }
    }
    verdict
}

/// Admin-facing push on a genuine CLOSED→OPEN or OPEN→CLOSED breaker
/// transition — de-duplicated so a breaker that stays open across many
/// dispatch attempts (every subsequent LLM call re-runs this same check)
/// pushes exactly one alert per state, not one per attempt. Mirrors
/// `StagnationMonitor`'s fingerprint de-dup (`gvu/stagnation.rs`), but that
/// monitor is a long-lived struct that keeps its last-alerted map in memory
/// across periodic ticks in one process; `check_agent_budget` has no such
/// home — it is a plain fn re-entered from scratch on every dispatch call,
/// possibly from a different process across a gateway restart — so the
/// de-dup memory has to be file-backed instead of an in-memory `HashMap`.
///
/// Best-effort: any failure (state file unreadable/unwritable, no `[proactive]`
/// destination, no bot token) degrades to a skipped push, never blocks or
/// panics the budget gate; notification failures never alter its verdict.
async fn notify_breaker_transition(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
    is_open: bool,
) {
    let new_state = if is_open { "open" } else { "closed" };
    if !record_breaker_transition(home_dir, agent_id, new_state) {
        return; // already alerted on this state (or was never open) — stay quiet
    }
    let name = agent_display_name(agent_dir, agent_id);
    let link =
        crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Billing, agent_id)
            .map(|url| format!("\n👉 {url}"))
            .unwrap_or_default();
    let text = if is_open {
        format!("⚠️ {name} 已停工：花費達上限。{link}")
    } else {
        format!("✅ {name} 已恢復工作：預算額度已回復。{link}")
    };
    // Stopping work is L3 — the agent is dead in the water until someone
    // raises the budget, and "found out at 08:00" costs a whole night's
    // throughput. Coming back online is L1: welcome news nobody has to act on.
    let level = if is_open {
        crate::notify_governance::NotifyLevel::Act
    } else {
        crate::notify_governance::NotifyLevel::Fyi
    };
    let outcome =
        crate::goal_notify::notify_agent_plain(home_dir, agent_id, level, "budget.breaker", &text)
            .await;
    if matches!(outcome, crate::goal_notify::NotifyOutcome::SendFailed) {
        tracing::debug!(
            agent_id,
            is_open,
            "budget: breaker-transition push failed (non-fatal)"
        );
    }
}

/// Compare-and-set the on-disk last-known breaker state for `agent_id`.
/// Returns `true` only when the stored state actually changed (a genuine
/// transition worth alerting on) — `false` for "already in that state" (the
/// common case: a breaker that stays open across many dispatch attempts) and
/// for any read/write failure (fail-open: an unwritable state file must
/// never crash the gate, worst case is a missed or duplicated push).
fn record_breaker_transition(home_dir: &Path, agent_id: &str, new_state: &'static str) -> bool {
    if !home_dir.is_absolute() { return false; }
    let path = breaker_state_path(home_dir);
    duduclaw_core::with_file_lock(&path, || {
        let mut states = read_breaker_states(&path);
        if states.get(agent_id).map(String::as_str) == Some(new_state) {
            return Ok(false);
        }
        states.insert(agent_id.to_string(), new_state.to_string());
        let json = serde_json::to_string_pretty(&states)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)?;
        Ok(true)
    })
    .unwrap_or(false)
}

fn breaker_state_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("budget_breaker_state.json")
}

fn read_breaker_states(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Resolve a human-readable agent name for the admin push — `agent.toml
/// [agent] display_name`, falling back to `name`, falling back to the raw
/// `agent_id` when neither is readable. Goes through the shared typed parse
/// point ([`duduclaw_core::agent_toml`]), mirroring [`load_budget_limits`],
/// rather than depending on the full `AgentRegistry` (this module has no
/// registry handle, only a dir + id).
///
/// The `[agent]` section is `Option` on the view precisely so a missing table
/// still short-circuits to `agent_id` here rather than silently reading an
/// all-empty one.
pub(crate) fn agent_display_name(agent_dir: Option<&Path>, agent_id: &str) -> String {
    let Some(dir) = agent_dir else {
        return agent_id.to_string();
    };
    let Some(a) = duduclaw_core::agent_toml::load(dir).agent else {
        return agent_id.to_string();
    };
    a.display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| a.name.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or(agent_id)
        .to_string()
}

/// Append a denial to `budget_events.jsonl` for dashboard surfacing. Best-effort
/// (an unwritable log must never block or crash the gate).
fn append_budget_event(
    home_dir: &Path,
    agent_id: &str,
    scope: &str,
    spent_cents: u64,
    cap_cents: u64,
) {
    if !home_dir.is_absolute() { return; }
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "agent_id": agent_id,
        "event": "budget_breaker_open",
        "scope": scope,
        "spent_cents": spent_cents,
        "cap_cents": cap_cents,
    })
    .to_string();
    let path = home_dir.join("budget_events.jsonl");
    let _ = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
        Ok::<(), std::io::Error>(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost_telemetry::{CostTelemetry, RequestType, TokenUsage};
    use tempfile::tempdir;

    fn limits(daily: u64, monthly: u64, hard: bool) -> BudgetLimits {
        BudgetLimits {
            daily_cap_cents: daily,
            monthly_limit_cents: monthly,
            warn_threshold_percent: 0,
            hard_stop: hard,
        }
    }

    #[tokio::test]
    async fn budget_missing_telemetry_is_unavailable() {
        assert!(matches!(evaluate_available_telemetry(None, "a", &limits(1, 0, true)).await,
            BudgetVerdict::Unavailable { .. }));
    }

    #[tokio::test]
    async fn budget_query_failure_is_unavailable_not_zero() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("cost.db");
        let tel = CostTelemetry::new(&db).unwrap();
        rusqlite::Connection::open(&db).unwrap().execute("DROP TABLE token_usage", []).unwrap();
        assert!(matches!(evaluate_budget(&tel, "a", &limits(1, 0, true)).await,
            BudgetVerdict::Unavailable { .. }));
    }

    #[tokio::test]
    async fn budget_bad_config_and_unreadable_directory_are_unavailable() {
        let dir = budget_dir("not toml [[[");
        assert!(matches!(check_agent_budget(dir.path(), Some(dir.path()), "a").await,
            BudgetVerdict::Unavailable { .. }));
        assert!(matches!(check_agent_budget(dir.path(), Some(&dir.path().join("missing")), "a").await,
            BudgetVerdict::Allow));
        let file = budget_dir("");
        std::fs::remove_file(file.path().join("agent.toml")).unwrap();
        std::fs::create_dir(file.path().join("agent.toml")).unwrap();
        assert!(validate_budget_config(Some(file.path())).is_err());
        assert!(validate_budget_config(Some(tempdir().unwrap().path())).is_ok());
    }

    #[tokio::test]
    async fn budget_malformed_section_refuses_before_accounting() {
        let home = tempdir().unwrap();
        for body in ["budget = 42", "not valid [[["] {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("agent.toml"), body).unwrap();
            assert!(matches!(check_agent_budget(home.path(), Some(dir.path()), "a").await,
                BudgetVerdict::Unavailable { .. }));
            assert!(gate_before_spawn(home.path(), Some(dir.path()), "a", "test").await.is_err());
            assert!(gate_before_spawn(home.path(), Some(dir.path()), "", "test").await.is_ok());
        }
    }

    #[tokio::test]
    async fn budget_missing_configuration_allows_before_accounting() {
        let home = tempdir().unwrap();
        for dir in [home.path().to_path_buf(), home.path().join("missing")] {
            assert_eq!(check_agent_budget(home.path(), Some(&dir), "a").await, BudgetVerdict::Allow);
            assert!(gate_before_spawn(home.path(), Some(&dir), "a", "test").await.is_ok());
        }
        assert!(!home.path().join("budget_events.jsonl").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn budget_dangling_configuration_is_unavailable() {
        let home = tempdir().unwrap();
        let absent = home.path().join("absent");
        std::os::unix::fs::symlink(&absent, home.path().join("agent.toml")).unwrap();
        assert!(gate_before_spawn(home.path(), Some(home.path()), "a", "test").await.is_err());
        let linked_dir = home.path().join("linked-agent");
        std::os::unix::fs::symlink(&absent, &linked_dir).unwrap();
        assert!(gate_before_spawn(home.path(), Some(&linked_dir), "a", "test").await.is_err());
    }

    #[test]
    fn budget_relative_home_does_not_write_audit() {
        let relative = std::path::PathBuf::from(format!("budget-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&relative).unwrap();
        append_unavailable_event(&relative, "a", "test", "unavailable", true);
        append_budget_event(&relative, "a", "daily", 2, 1);
        assert!(!record_breaker_transition(&relative, "a", "open"));
        let entries = std::fs::read_dir(&relative).unwrap().count();
        std::fs::remove_dir(&relative).unwrap();
        assert_eq!(entries, 0);
    }

    #[test]
    fn budget_unavailable_policy_and_text() {
        for (value, denied) in [("allow", false), ("deny", true), ("invalid", true)] {
            let dir = budget_dir(&format!("[budget]\nwhen_unavailable = \"{value}\"\n"));
            assert_eq!(unavailable_denied(Some(dir.path())), denied);
            let verdict = BudgetVerdict::Unavailable { reason: "test telemetry unavailable".into() };
            assert_eq!(finish_budget_gate(dir.path(), "a", "test", verdict, unavailable_denied(Some(dir.path()))).is_err(), denied);
            assert!(!breaker_state_path(dir.path()).exists());
        }
        let v = BudgetVerdict::Unavailable { reason: "database unavailable".into() };
        assert!(!v.user_message().contains("花費達上限"));
        let dir = tempdir().unwrap();
        append_unavailable_event(dir.path(), "a", "test", "missing", true);
        assert!(!breaker_state_path(dir.path()).exists());
        assert!(std::fs::read_to_string(dir.path().join("budget_events.jsonl")).unwrap()
            .contains("budget_unavailable"));
    }

    #[tokio::test]
    async fn budget_ephemeral_family_rolls_up_before_cent_conversion() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("cost.db");
        let tel = CostTelemetry::new(&db).unwrap();
        seed_cost(&tel, "parent", 10).await;
        seed_cost(&tel, "child", 10).await;
        seed_cost(&tel, "sibling", 10).await;
        seed_cost(&tel, "unrelated", 10).await;
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE token_usage SET cost_millicents = 400", []).unwrap();
        for child in ["child", "sibling"] {
            conn.execute("INSERT INTO ephemeral_parents VALUES (?1, 'parent', '')", [child]).unwrap();
        }
        for id in ["parent", "child", "sibling"] {
            assert_eq!(tel.budget_spent_millicents(id, 24).await.unwrap(), 1200);
            assert!(evaluate_budget(&tel, id, &limits(1, 0, true)).await.is_denied());
        }
    }

    // ── R5: `[budget]` / `[agent]` directions, pinned ────────────────────
    //
    //   every numeric key   absent OR wrong-typed ⇒ 0 ⇒ NO CAP. Deliberately
    //                 fail-*open*: a budget cap that materialises out of an
    //                 unreadable config would silently strand an agent
    //                 mid-task. The wrong-typed case is the one that WARNS —
    //                 which is why the view field is three-state and not an
    //                 Option.
    //   integer vs float   an integer is clamped at 0, a float (`100.0`, a
    //                 common typo) is ROUNDED. Both reachable, so the view
    //                 keeps the variants apart.
    //   hard_stop     absent ⇒ false; `1`/`0` tolerated-with-warning; any
    //                 other type ⇒ false.
    //   [agent] table absent ⇒ fall back to the raw agent id, never to a
    //                 blank display name.

    fn budget_dir(body: &str) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), body).unwrap();
        dir
    }

    #[test]
    fn default_direction_budget_absent_or_unreadable_is_no_cap() {
        for body in [
            "",                        // empty file
            "[agent]\nname = \"a\"\n", // no [budget]
            "[budget]\n",              // section, no keys
            "budget = 1\n",            // scalar where a table was expected
            "not toml [[[",            // malformed file
        ] {
            let dir = budget_dir(body);
            let l = load_budget_limits(Some(dir.path()));
            assert_eq!(l.daily_cap_cents, 0, "for {body:?}");
            assert_eq!(l.monthly_limit_cents, 0, "for {body:?}");
            assert_eq!(l.warn_threshold_percent, 0, "for {body:?}");
            assert!(!l.hard_stop, "for {body:?}");
        }

        // No agent dir at all, and a dir with no file — same direction.
        assert_eq!(load_budget_limits(None).monthly_limit_cents, 0);
        let empty = tempdir().unwrap();
        assert_eq!(
            load_budget_limits(Some(empty.path())).monthly_limit_cents,
            0
        );
    }

    #[test]
    fn default_direction_budget_wrong_typed_key_is_no_cap_not_fatal() {
        // Present-but-wrong-type ⇒ 0 (and a loud warn!, which is the whole
        // reason the field is three-state). It must NOT abort the parse and
        // it must NOT be coerced from a string.
        let dir = budget_dir(
            "[budget]\n\
             monthly_limit_cents = \"5000\"\n\
             daily_cap_cents = true\n\
             warn_threshold_percent = [80]\n\
             hard_stop = \"yes\"\n",
        );
        let l = load_budget_limits(Some(dir.path()));
        assert_eq!(
            l.monthly_limit_cents, 0,
            "a string is not coerced to a number"
        );
        assert_eq!(l.daily_cap_cents, 0);
        assert_eq!(l.warn_threshold_percent, 0);
        assert!(!l.hard_stop, "a non-bool, non-int hard_stop is false");
    }

    #[test]
    fn default_direction_budget_integer_and_float_are_handled_differently() {
        // Integers clamp at zero; floats round. Both are live paths, so the
        // typed view must not flatten them into one `f64`.
        let ints = budget_dir(
            "[budget]\nmonthly_limit_cents = 5000\ndaily_cap_cents = -20\n\
             warn_threshold_percent = 150\n",
        );
        let l = load_budget_limits(Some(ints.path()));
        assert_eq!(l.monthly_limit_cents, 5000);
        assert_eq!(l.daily_cap_cents, 0, "negative clamps to 0");
        assert_eq!(l.warn_threshold_percent, 100, "capped at 100");

        let floats = budget_dir("[budget]\nmonthly_limit_cents = 100.0\ndaily_cap_cents = 49.6\n");
        let l = load_budget_limits(Some(floats.path()));
        assert_eq!(l.monthly_limit_cents, 100, "the `100.0` typo still works");
        assert_eq!(l.daily_cap_cents, 50, "floats round, not truncate");
    }

    #[test]
    fn default_direction_hard_stop_tolerates_integers_only() {
        for (body, want) in [
            ("[budget]\nhard_stop = true\n", true),
            ("[budget]\nhard_stop = false\n", false),
            ("[budget]\nhard_stop = 1\n", true), // tolerated with a warn
            ("[budget]\nhard_stop = 0\n", false), // tolerated with a warn
            ("[budget]\nhard_stop = \"1\"\n", false), // NOT coerced
            ("[budget]\n", false),
        ] {
            let dir = budget_dir(body);
            assert_eq!(
                load_budget_limits(Some(dir.path())).hard_stop,
                want,
                "for {body:?}"
            );
        }
    }

    #[test]
    fn default_direction_display_name_falls_back_to_the_agent_id() {
        // The `[agent]` section is `Option` on the shared view precisely so a
        // file with no `[agent]` table short-circuits here instead of reading
        // an all-empty one and reporting a blank name.
        for body in [
            "",
            "[budget]\nhard_stop = true\n", // no [agent] table at all
            "agent = \"scalar\"\n",         // wrong-typed section
            "[agent]\n",                    // table, no keys
            "[agent]\ndisplay_name = \"  \"\n", // blank ⇒ falls through
            "[agent]\ndisplay_name = 42\n", // wrong type
            "not toml [[[",
        ] {
            let dir = budget_dir(body);
            assert_eq!(
                agent_display_name(Some(dir.path()), "alpha"),
                "alpha",
                "for {body:?}"
            );
        }

        assert_eq!(agent_display_name(None, "alpha"), "alpha");

        // display_name wins; `name` is the documented second choice.
        let d = budget_dir("[agent]\nname = \"alpha\"\ndisplay_name = \" 阿爾法 \"\n");
        assert_eq!(agent_display_name(Some(d.path()), "alpha"), "阿爾法");
        let n = budget_dir("[agent]\nname = \"Alpha Bot\"\n");
        assert_eq!(agent_display_name(Some(n.path()), "alpha"), "Alpha Bot");
    }

    // Record enough usage to exceed a cent budget. Pricing is model-dependent;
    // we push a large output-token count on a known-priced model so cost > cap.
    async fn seed_cost(tel: &CostTelemetry, agent: &str, output_tokens: u64) {
        tel.record(
            agent,
            RequestType::Chat,
            "claude-sonnet-5",
            &TokenUsage {
                input_tokens: 1000,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                output_tokens,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn under_cap_allows_over_cap_denies() {
        let dir = tempdir().unwrap();
        let tel = CostTelemetry::new(&dir.path().join("c.db")).unwrap();
        let agent = "spender";

        // No spend yet → allow even with a tiny cap.
        assert_eq!(
            evaluate_budget(&tel, agent, &limits(1, 0, true)).await,
            BudgetVerdict::Allow
        );

        // Rack up cost well over a 1-cent daily cap.
        seed_cost(&tel, agent, 5_000_000).await;
        let v = evaluate_budget(&tel, agent, &limits(1, 0, true)).await;
        assert!(v.is_denied(), "over-cap must deny: {v:?}");
        if let BudgetVerdict::Deny { scope, .. } = v {
            assert_eq!(scope, "daily");
        }
    }

    #[tokio::test]
    async fn hard_stop_off_never_blocks() {
        let dir = tempdir().unwrap();
        let tel = CostTelemetry::new(&dir.path().join("c.db")).unwrap();
        let agent = "spender";
        seed_cost(&tel, agent, 5_000_000).await;
        // Same spend, but hard_stop=false → warn-only semantics, always Allow.
        assert_eq!(
            evaluate_budget(&tel, agent, &limits(1, 1, false)).await,
            BudgetVerdict::Allow
        );
    }

    #[tokio::test]
    async fn inert_limits_short_circuit() {
        let dir = tempdir().unwrap();
        let tel = CostTelemetry::new(&dir.path().join("c.db")).unwrap();
        assert_eq!(
            evaluate_budget(&tel, "x", &limits(0, 0, true)).await,
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn load_limits_from_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\nmonthly_limit_cents = 5000\nwarn_threshold_percent = 80\nhard_stop = true\ndaily_cap_cents = 200\n",
        )
        .unwrap();
        let l = load_budget_limits(Some(dir.path()));
        assert_eq!(l.daily_cap_cents, 200);
        assert_eq!(l.monthly_limit_cents, 5000);
        assert!(l.hard_stop);
    }

    #[test]
    fn float_caps_are_coerced_not_zeroed() {
        // A `100.0`-style float (common typo) must still enforce, not silently
        // become "no cap". hard_stop = 1 (int) is tolerated too.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\ndaily_cap_cents = 200.0\nmonthly_limit_cents = 5000.9\nhard_stop = 1\n",
        )
        .unwrap();
        let l = load_budget_limits(Some(dir.path()));
        assert_eq!(l.daily_cap_cents, 200);
        assert_eq!(l.monthly_limit_cents, 5001); // rounded
        assert!(l.hard_stop, "int 1 coerced to true");
        assert!(!l.is_inert(), "float caps still enforce");
    }

    #[test]
    fn missing_config_is_inert() {
        let dir = tempdir().unwrap();
        assert!(load_budget_limits(Some(dir.path())).is_inert());
        assert!(load_budget_limits(None).is_inert());
    }

    #[test]
    fn deny_message_is_user_facing_zhtw() {
        let v = BudgetVerdict::Deny {
            scope: "daily",
            spent_cents: 250,
            cap_cents: 200,
        };
        let m = v.user_message();
        assert!(m.contains("今日") && m.contains("2.50") && m.contains("2.00"));
    }

    #[test]
    fn deny_message_uses_stopped_working_wording_and_says_admin_notified() {
        let v = BudgetVerdict::Deny {
            scope: "monthly",
            spent_cents: 100,
            cap_cents: 100,
        };
        let m = v.user_message();
        assert!(
            m.contains("已停工"),
            "must use the platform's external state wording: {m}"
        );
        assert!(m.contains("花費達上限"));
        assert!(
            m.contains("已通知管理員"),
            "must truthfully tell the user the admin was alerted: {m}"
        );
    }

    // ── record_breaker_transition: the admin-push de-dup ─────────────────

    #[test]
    fn first_open_transition_fires_repeat_does_not() {
        let dir = tempdir().unwrap();
        assert!(
            record_breaker_transition(dir.path(), "a1", "open"),
            "CLOSED→OPEN (first time) must report a real transition"
        );
        assert!(
            !record_breaker_transition(dir.path(), "a1", "open"),
            "repeated OPEN while already OPEN must be a no-op — breaker re-tripping every \
             dispatch attempt must not re-fire the admin push"
        );
        assert!(
            !record_breaker_transition(dir.path(), "a1", "open"),
            "third consecutive OPEN check must still be deduped"
        );
    }

    #[test]
    fn open_then_close_then_reopen_each_fire_once() {
        let dir = tempdir().unwrap();
        assert!(record_breaker_transition(dir.path(), "a1", "open"));
        assert!(!record_breaker_transition(dir.path(), "a1", "open"));

        assert!(
            record_breaker_transition(dir.path(), "a1", "closed"),
            "OPEN→CLOSED recovery must report a real transition"
        );
        assert!(
            !record_breaker_transition(dir.path(), "a1", "closed"),
            "repeated CLOSED (e.g. every subsequent Allow check) must be deduped"
        );

        // Relapse into OPEN again after having recovered must re-fire.
        assert!(
            record_breaker_transition(dir.path(), "a1", "open"),
            "a relapse after recovery must fire again, not stay silenced forever"
        );
    }

    #[test]
    fn distinct_agents_do_not_share_dedup_state() {
        let dir = tempdir().unwrap();
        assert!(record_breaker_transition(dir.path(), "a1", "open"));
        assert!(
            record_breaker_transition(dir.path(), "a2", "open"),
            "agent a2's first OPEN must fire even though a1 is already OPEN"
        );
        assert!(!record_breaker_transition(dir.path(), "a1", "open"));
        assert!(!record_breaker_transition(dir.path(), "a2", "open"));
    }

    #[test]
    fn breaker_state_persists_across_reads() {
        let dir = tempdir().unwrap();
        assert!(record_breaker_transition(dir.path(), "a1", "open"));
        // A fresh read (simulating a new process / a later dispatch call)
        // must see the persisted state, not re-fire.
        let states = read_breaker_states(&breaker_state_path(dir.path()));
        assert_eq!(states.get("a1").map(String::as_str), Some("open"));
    }

    // ── agent_display_name ────────────────────────────────────────────────

    #[test]
    fn display_name_prefers_display_name_field() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[agent]\nname = \"assistant\"\ndisplay_name = \"嘟嘟\"\n",
        )
        .unwrap();
        assert_eq!(agent_display_name(Some(dir.path()), "assistant"), "嘟嘟");
    }

    #[test]
    fn display_name_falls_back_to_name_then_agent_id() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[agent]\nname = \"assistant\"\n",
        )
        .unwrap();
        assert_eq!(
            agent_display_name(Some(dir.path()), "assistant"),
            "assistant"
        );

        let empty_dir = tempdir().unwrap();
        assert_eq!(
            agent_display_name(Some(empty_dir.path()), "fallback-id"),
            "fallback-id"
        );
        assert_eq!(agent_display_name(None, "no-dir-id"), "no-dir-id");
    }
}
