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

use std::collections::HashMap;
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
    /// Wording matches the platform's external-facing state vocabulary
    /// ("已停工（花費達上限）") and — since [`check_agent_budget`] now pushes
    /// an admin alert on the same transition — truthfully tells the person
    /// talking to the agent that the admin already knows, instead of leaving
    /// them to wonder why the agent went quiet.
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
    use duduclaw_core::lenient::{TomlFlag, TomlNumber, Tri};

    let inert = BudgetLimits {
        daily_cap_cents: 0,
        monthly_limit_cents: 0,
        warn_threshold_percent: 0,
        hard_stop: false,
    };
    let Some(dir) = agent_dir else {
        return inert;
    };
    let b = duduclaw_core::agent_toml::load(dir).budget;

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
    match &verdict {
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
/// panics the budget gate — the gate's own fail-open posture (module docs)
/// extends to this notification path too.
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
    let link = crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Billing, agent_id)
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
        tracing::debug!(agent_id, is_open, "budget: breaker-transition push failed (non-fatal)");
    }
}

/// Compare-and-set the on-disk last-known breaker state for `agent_id`.
/// Returns `true` only when the stored state actually changed (a genuine
/// transition worth alerting on) — `false` for "already in that state" (the
/// common case: a breaker that stays open across many dispatch attempts) and
/// for any read/write failure (fail-open: an unwritable state file must
/// never crash the gate, worst case is a missed or duplicated push).
fn record_breaker_transition(home_dir: &Path, agent_id: &str, new_state: &'static str) -> bool {
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
fn agent_display_name(agent_dir: Option<&Path>, agent_id: &str) -> String {
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
        .or_else(|| {
            a.name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
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
            "",                              // empty file
            "[agent]\nname = \"a\"\n",       // no [budget]
            "[budget]\n",                    // section, no keys
            "budget = 1\n",                  // scalar where a table was expected
            "not toml [[[",                  // malformed file
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
        assert_eq!(load_budget_limits(Some(empty.path())).monthly_limit_cents, 0);
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
        assert_eq!(l.monthly_limit_cents, 0, "a string is not coerced to a number");
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

        let floats = budget_dir(
            "[budget]\nmonthly_limit_cents = 100.0\ndaily_cap_cents = 49.6\n",
        );
        let l = load_budget_limits(Some(floats.path()));
        assert_eq!(l.monthly_limit_cents, 100, "the `100.0` typo still works");
        assert_eq!(l.daily_cap_cents, 50, "floats round, not truncate");
    }

    #[test]
    fn default_direction_hard_stop_tolerates_integers_only() {
        for (body, want) in [
            ("[budget]\nhard_stop = true\n", true),
            ("[budget]\nhard_stop = false\n", false),
            ("[budget]\nhard_stop = 1\n", true),   // tolerated with a warn
            ("[budget]\nhard_stop = 0\n", false),  // tolerated with a warn
            ("[budget]\nhard_stop = \"1\"\n", false), // NOT coerced
            ("[budget]\n", false),
        ] {
            let dir = budget_dir(body);
            assert_eq!(load_budget_limits(Some(dir.path())).hard_stop, want, "for {body:?}");
        }
    }

    #[test]
    fn default_direction_display_name_falls_back_to_the_agent_id() {
        // The `[agent]` section is `Option` on the shared view precisely so a
        // file with no `[agent]` table short-circuits here instead of reading
        // an all-empty one and reporting a blank name.
        for body in [
            "",
            "[budget]\nhard_stop = true\n",         // no [agent] table at all
            "agent = \"scalar\"\n",                 // wrong-typed section
            "[agent]\n",                            // table, no keys
            "[agent]\ndisplay_name = \"  \"\n",     // blank ⇒ falls through
            "[agent]\ndisplay_name = 42\n",         // wrong type
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
        assert!(m.contains("已停工"), "must use the platform's external state wording: {m}");
        assert!(m.contains("花費達上限"));
        assert!(m.contains("已通知管理員"), "must truthfully tell the user the admin was alerted: {m}");
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
        std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = \"assistant\"\n").unwrap();
        assert_eq!(agent_display_name(Some(dir.path()), "assistant"), "assistant");

        let empty_dir = tempdir().unwrap();
        assert_eq!(agent_display_name(Some(empty_dir.path()), "fallback-id"), "fallback-id");
        assert_eq!(agent_display_name(None, "no-dir-id"), "no-dir-id");
    }
}
