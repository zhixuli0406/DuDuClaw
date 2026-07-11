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
//! ## Fail-open, deliberately
//!
//! If telemetry is unavailable/uninitialised the breaker **allows** the call
//! (and logs). A hard kill switch that fails *closed* would block ALL work the
//! moment its own datastore hiccups — worse than a small overspend. Budget is a
//! cost control, not a security gate (contrast the fail-closed MCP auth). The
//! choice is logged so it is never silent.

use std::path::Path;

use crate::cost_telemetry::{get_telemetry, init_telemetry, CostTelemetry};

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
    pub fn user_message(&self) -> String {
        match self {
            BudgetVerdict::Allow => String::new(),
            BudgetVerdict::Deny {
                scope,
                spent_cents,
                cap_cents,
            } => {
                let window = if *scope == "daily" { "今日" } else { "本月" };
                format!(
                    "⚠️ 已達{window}預算上限（已使用 US${:.2} / 上限 US${:.2}），\
                     暫停回應以避免超支。額度會在時間窗滑動後自動恢復，\
                     或請調整 agent.toml 的 [budget] 設定。",
                    *spent_cents as f64 / 100.0,
                    *cap_cents as f64 / 100.0,
                )
            }
        }
    }
}

/// Load `[budget]` limits from an agent's `agent.toml` (lightweight generic
/// parse, mirroring `runtime_config`). Missing file / section ⇒ inert limits
/// (never blocks).
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
    let Ok(text) = std::fs::read_to_string(dir.join("agent.toml")) else {
        return inert;
    };
    let Ok(v) = text.parse::<toml::Value>() else {
        return inert;
    };
    let b = match v.get("budget") {
        Some(b) => b,
        None => return inert,
    };
    // Coerce ints, and floats (a common `100.0` typo) → rounded u64. A key that
    // is present but a genuinely wrong type is logged LOUDLY rather than silently
    // treated as 0 — a config typo must not silently disable a cost control.
    let num_of = |k: &str| -> u64 {
        match b.get(k) {
            None => 0,
            Some(x) if x.as_integer().is_some() => x.as_integer().unwrap().max(0) as u64,
            Some(x) if x.as_float().is_some() => x.as_float().unwrap().max(0.0).round() as u64,
            Some(_) => {
                tracing::warn!(
                    key = k,
                    "budget: [budget].{k} has an unexpected type — treated as no cap; \
                     it must be an integer number of cents"
                );
                0
            }
        }
    };
    let hard_stop = match b.get("hard_stop") {
        None => false,
        Some(x) if x.as_bool().is_some() => x.as_bool().unwrap(),
        // Tolerate `hard_stop = 1` / `0` (common mistake) but warn.
        Some(x) if x.as_integer().is_some() => {
            tracing::warn!("budget: [budget].hard_stop should be a bool; coercing integer");
            x.as_integer().unwrap() != 0
        }
        Some(_) => {
            tracing::warn!("budget: [budget].hard_stop has an unexpected type — treated as false");
            false
        }
    };
    BudgetLimits {
        daily_cap_cents: num_of("daily_cap_cents"),
        monthly_limit_cents: num_of("monthly_limit_cents"),
        warn_threshold_percent: num_of("warn_threshold_percent").min(100) as u8,
        hard_stop,
    }
}

/// Cents spent by `agent_id` over the last `hours` (rolling window). Telemetry
/// stores cost in millicents; 0 on any query error.
async fn spent_cents(tel: &CostTelemetry, agent_id: &str, hours: u64) -> u64 {
    match tel.summary_by_agent(agent_id, hours).await {
        Ok(s) => s.summary.total_cost_millicents / 1000,
        Err(e) => {
            tracing::warn!(agent_id, "budget: cost query failed: {e}");
            0
        }
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
        let spent = spent_cents(tel, agent_id, DAILY_WINDOW_HOURS).await;
        if spent >= limits.daily_cap_cents {
            return BudgetVerdict::Deny {
                scope: "daily",
                spent_cents: spent,
                cap_cents: limits.daily_cap_cents,
            };
        }
        warn_if_approaching(agent_id, "daily", spent, limits.daily_cap_cents, limits.warn_threshold_percent);
    }
    if limits.monthly_limit_cents > 0 {
        let spent = spent_cents(tel, agent_id, MONTHLY_WINDOW_HOURS).await;
        if spent >= limits.monthly_limit_cents {
            return BudgetVerdict::Deny {
                scope: "monthly",
                spent_cents: spent,
                cap_cents: limits.monthly_limit_cents,
            };
        }
        warn_if_approaching(agent_id, "monthly", spent, limits.monthly_limit_cents, limits.warn_threshold_percent);
    }
    BudgetVerdict::Allow
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
/// Fail-open: no agent id, inert limits, or telemetry unavailable ⇒ `Allow`.
pub async fn check_agent_budget(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
) -> BudgetVerdict {
    if agent_id.is_empty() {
        return BudgetVerdict::Allow;
    }
    let limits = load_budget_limits(agent_dir);
    if limits.is_inert() {
        return BudgetVerdict::Allow;
    }
    // Ensure telemetry exists (lazy init mirrors record_usage). If it still
    // isn't available, fail open.
    if get_telemetry().is_none() {
        let _ = init_telemetry(home_dir);
    }
    let Some(tel) = get_telemetry() else {
        tracing::warn!(agent_id, "budget: telemetry unavailable — failing open");
        return BudgetVerdict::Allow;
    };
    let verdict = evaluate_budget(tel, agent_id, &limits).await;
    if let BudgetVerdict::Deny {
        scope,
        spent_cents,
        cap_cents,
    } = &verdict
    {
        tracing::warn!(
            agent_id,
            scope,
            spent_cents,
            cap_cents,
            "budget circuit breaker OPEN — blocking LLM call"
        );
        append_budget_event(home_dir, agent_id, scope, *spent_cents, *cap_cents);
    }
    verdict
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
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
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
}
