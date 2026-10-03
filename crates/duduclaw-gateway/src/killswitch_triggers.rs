//! Enforcement of `KILLSWITCH.toml [triggers]` on the channel reply path.
//!
//! Before v1.68.0 the four trigger thresholds the dashboard's 緊急停止 card
//! edits were only parsed and validated. They now act as documented in
//! `docs/examples/KILLSWITCH.toml`:
//!
//! | key | effect |
//! |---|---|
//! | `max_replies_per_minute` | per scope (conversation): more replies than this in a sliding 60 s window are rate-limited (message dropped, recorded as a silent reply) |
//! | `max_consecutive_errors` | per scope: this many failed replies in a row escalate the scope's failsafe level by one step |
//! | `error_rate_threshold` | per scope: over the last [`ERROR_WINDOW`] outcomes (at least [`MIN_RATE_SAMPLES`]), a failure share above this escalates the failsafe level by one step |
//! | `cost_limit_usd` | all agents: when the API spend recorded in the last 24 h reaches this, the global failsafe scope goes to L2 Restricted (canned reply, no AI call). `0` disables it |
//!
//! **Only keys written in `[triggers]` are enforced.** The built-in defaults
//! (10 replies/min, $50/day, …) stay display-only for installations whose
//! file does not set them, so upgrading never silently caps a running
//! deployment. The file is re-read when its mtime changes, so a dashboard
//! save applies to the next message.
//!
//! Escalation goes through the existing `FailsafeManager` (same levels,
//! canned replies, auto-recovery and `!RESUME` as the circuit breaker).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Outcomes kept per scope for the error-rate trigger.
pub const ERROR_WINDOW: usize = 20;
/// Minimum outcomes in the window before the error rate is judged.
pub const MIN_RATE_SAMPLES: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const COST_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_SCOPES: usize = 10_000;

/// Trigger thresholds that are explicitly set (`None` = not enforced).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnforcedTriggers {
    pub max_replies_per_minute: Option<u32>,
    pub max_consecutive_errors: Option<u32>,
    pub error_rate_threshold: Option<f64>,
    pub cost_limit_usd: Option<f64>,
}

impl EnforcedTriggers {
    /// Parse the explicitly-written keys of `[triggers]`. Out-of-range values
    /// (what `killswitch.update` would refuse) are ignored rather than
    /// enforced with a surprising meaning.
    pub fn from_table(table: &toml::Table) -> Self {
        let Some(t) = table.get("triggers").and_then(|v| v.as_table()) else {
            return Self::default();
        };
        let int = |k: &str| t.get(k).and_then(|v| v.as_integer());
        let num = |k: &str| {
            t.get(k)
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        };
        Self {
            max_replies_per_minute: int("max_replies_per_minute")
                .filter(|v| (1..=10_000).contains(v))
                .map(|v| v as u32),
            max_consecutive_errors: int("max_consecutive_errors")
                .filter(|v| (1..=1_000).contains(v))
                .map(|v| v as u32),
            error_rate_threshold: num("error_rate_threshold")
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            cost_limit_usd: num("cost_limit_usd").filter(|v| v.is_finite() && *v > 0.0),
        }
    }

    /// Read `<home>/KILLSWITCH.toml`; missing or unparsable ⇒ nothing enforced.
    pub fn load(home: &Path) -> Self {
        std::fs::read_to_string(home.join("KILLSWITCH.toml"))
            .ok()
            .and_then(|c| c.parse::<toml::Table>().ok())
            .map(|t| Self::from_table(&t))
            .unwrap_or_default()
    }
}

/// What the error triggers decided after recording an outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeVerdict {
    Ok,
    /// Escalate the scope's failsafe level; the string is the audit reason.
    Escalate(String),
}

#[derive(Default)]
struct ScopeState {
    /// LRU stamp (see [`TriggerState::scope`]).
    last_used: u64,
    replies: VecDeque<Instant>,
    consecutive_errors: u32,
    outcomes: VecDeque<bool>,
}

/// Per-scope counters. Pure (no I/O) so the rules are unit-testable.
#[derive(Default)]
pub struct TriggerState {
    scopes: HashMap<String, ScopeState>,
    /// Monotonic use counter for LRU eviction.
    clock: u64,
}

impl TriggerState {
    fn scope(&mut self, scope: &str) -> &mut ScopeState {
        if self.scopes.len() >= MAX_SCOPES && !self.scopes.contains_key(scope) {
            // Bounded memory, LRU: evict the least recently used scopes
            // (a batch, so a flood of new scopes stays amortised O(1)). A
            // burst of new sessions can no longer reset the counters of
            // the sessions that are actually active.
            let batch = (MAX_SCOPES / 100).max(1);
            let mut by_age: Vec<(u64, String)> =
                self.scopes.iter().map(|(k, s)| (s.last_used, k.clone())).collect();
            by_age.sort_unstable();
            for (_, k) in by_age.into_iter().take(batch) {
                self.scopes.remove(&k);
            }
        }
        self.clock += 1;
        let now = self.clock;
        let s = self.scopes.entry(scope.to_string()).or_default();
        s.last_used = now;
        s
    }

    /// Count one inbound message for `scope`. Returns `false` when it would
    /// exceed `limit` replies in the last 60 s (rate-limited; not counted).
    pub fn admit_reply(&mut self, scope: &str, limit: Option<u32>, now: Instant) -> bool {
        let Some(limit) = limit else { return true };
        let s = self.scope(scope);
        while s
            .replies
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            s.replies.pop_front();
        }
        if s.replies.len() >= limit as usize {
            return false;
        }
        s.replies.push_back(now);
        true
    }

    /// Record a reply outcome and apply the two error triggers.
    pub fn record_outcome(
        &mut self,
        scope: &str,
        success: bool,
        triggers: &EnforcedTriggers,
    ) -> OutcomeVerdict {
        let s = self.scope(scope);
        s.outcomes.push_back(success);
        while s.outcomes.len() > ERROR_WINDOW {
            s.outcomes.pop_front();
        }
        if success {
            s.consecutive_errors = 0;
            return OutcomeVerdict::Ok;
        }
        s.consecutive_errors = s.consecutive_errors.saturating_add(1);
        if let Some(max) = triggers.max_consecutive_errors {
            if s.consecutive_errors >= max {
                let n = s.consecutive_errors;
                s.consecutive_errors = 0;
                s.outcomes.clear();
                return OutcomeVerdict::Escalate(format!(
                    "killswitch trigger: {n} consecutive errors (max_consecutive_errors = {max})"
                ));
            }
        }
        if let Some(threshold) = triggers.error_rate_threshold {
            if s.outcomes.len() >= MIN_RATE_SAMPLES {
                let failures = s.outcomes.iter().filter(|ok| !**ok).count();
                let rate = failures as f64 / s.outcomes.len() as f64;
                if rate > threshold {
                    let total = s.outcomes.len();
                    s.outcomes.clear();
                    s.consecutive_errors = 0;
                    return OutcomeVerdict::Escalate(format!(
                        "killswitch trigger: error rate {failures}/{total} over error_rate_threshold = {threshold}"
                    ));
                }
            }
        }
        OutcomeVerdict::Ok
    }
}

/// Process-wide monitor: counters plus the mtime-cached trigger config and a
/// short-lived cache of the 24 h spend.
pub struct TriggerMonitor {
    state: Mutex<TriggerState>,
    config: Mutex<Option<(PathBuf, Option<SystemTime>, EnforcedTriggers)>>,
    cost: Mutex<Option<(Instant, f64)>>,
}

static GLOBAL: OnceLock<TriggerMonitor> = OnceLock::new();

pub fn global() -> &'static TriggerMonitor {
    GLOBAL.get_or_init(|| TriggerMonitor {
        state: Mutex::new(TriggerState::default()),
        config: Mutex::new(None),
        cost: Mutex::new(None),
    })
}

impl TriggerMonitor {
    /// Current enforced triggers for `home` (re-read when the file changes).
    pub fn triggers(&self, home: &Path) -> EnforcedTriggers {
        let path = home.join("KILLSWITCH.toml");
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let mut cached = self.config.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((p, m, t)) = cached.as_ref() {
            if *p == path && *m == mtime {
                return t.clone();
            }
        }
        let t = EnforcedTriggers::load(home);
        *cached = Some((path, mtime, t.clone()));
        t
    }

    pub fn admit_reply(&self, home: &Path, scope: &str) -> bool {
        let limit = self.triggers(home).max_replies_per_minute;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit_reply(scope, limit, Instant::now())
    }

    pub fn record_outcome(&self, home: &Path, scope: &str, success: bool) -> OutcomeVerdict {
        let triggers = self.triggers(home);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_outcome(scope, success, &triggers)
    }

    /// `Some((spent_usd, limit_usd))` when the 24 h spend reached the
    /// configured `cost_limit_usd`. Telemetry unavailable ⇒ `None` (this is a
    /// spend cap, not an authorization gate; the per-agent budget gate is the
    /// fail-closed one).
    pub async fn cost_limit_reached(&self, home: &Path) -> Option<(f64, f64)> {
        let limit = self.triggers(home).cost_limit_usd?;
        let cached = *self.cost.lock().unwrap_or_else(|e| e.into_inner());
        let spent = match cached {
            Some((at, usd)) if at.elapsed() < COST_CACHE_TTL => usd,
            _ => {
                let telemetry = crate::cost_telemetry::get_telemetry()?;
                let summary = telemetry.summary_global(24).await.ok()?;
                // millicents → USD
                let usd = summary.total_cost_millicents as f64 / 100_000.0;
                *self.cost.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), usd));
                usd
            }
        };
        (spent >= limit).then_some((spent, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triggers(toml_src: &str) -> EnforcedTriggers {
        EnforcedTriggers::from_table(&toml_src.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn only_written_keys_are_enforced() {
        assert_eq!(triggers(""), EnforcedTriggers::default());
        let t = triggers("[triggers]\nmax_replies_per_minute = 3\n");
        assert_eq!(t.max_replies_per_minute, Some(3));
        assert_eq!(t.max_consecutive_errors, None);
        assert_eq!(t.cost_limit_usd, None);
        // Out of range / zero cost ⇒ not enforced.
        let t = triggers(
            "[triggers]\nmax_replies_per_minute = 0\nerror_rate_threshold = 1.5\ncost_limit_usd = 0.0\n",
        );
        assert_eq!(t, EnforcedTriggers::default());
        // Integer cost is accepted.
        assert_eq!(triggers("[triggers]\ncost_limit_usd = 5\n").cost_limit_usd, Some(5.0));
    }

    #[test]
    fn reply_rate_limits_per_scope_and_window() {
        let mut s = TriggerState::default();
        let t0 = Instant::now();
        assert!(s.admit_reply("a", Some(2), t0));
        assert!(s.admit_reply("a", Some(2), t0));
        assert!(!s.admit_reply("a", Some(2), t0 + Duration::from_secs(1)));
        // Other scopes unaffected; unset limit never limits.
        assert!(s.admit_reply("b", Some(2), t0));
        assert!(s.admit_reply("a", None, t0));
        // Window slides.
        assert!(s.admit_reply("a", Some(2), t0 + Duration::from_secs(61)));
    }

    #[test]
    fn consecutive_errors_escalate_and_reset() {
        let t = triggers("[triggers]\nmax_consecutive_errors = 3\n");
        let mut s = TriggerState::default();
        assert_eq!(s.record_outcome("a", false, &t), OutcomeVerdict::Ok);
        assert_eq!(s.record_outcome("a", false, &t), OutcomeVerdict::Ok);
        // A success resets the streak.
        assert_eq!(s.record_outcome("a", true, &t), OutcomeVerdict::Ok);
        assert_eq!(s.record_outcome("a", false, &t), OutcomeVerdict::Ok);
        assert_eq!(s.record_outcome("a", false, &t), OutcomeVerdict::Ok);
        assert!(matches!(
            s.record_outcome("a", false, &t),
            OutcomeVerdict::Escalate(_)
        ));
        // Counter restarts after escalating.
        assert_eq!(s.record_outcome("a", false, &t), OutcomeVerdict::Ok);
    }

    #[test]
    fn error_rate_needs_min_samples_then_escalates() {
        let t = triggers("[triggers]\nerror_rate_threshold = 0.3\n");
        let mut s = TriggerState::default();
        // 3 failures in 9 samples: below the sample floor.
        for i in 0..9 {
            assert_eq!(s.record_outcome("a", i % 3 != 0, &t), OutcomeVerdict::Ok);
        }
        // 10th sample, 4/10 = 0.4 > 0.3.
        assert!(matches!(
            s.record_outcome("a", false, &t),
            OutcomeVerdict::Escalate(_)
        ));
        // Not enforced when the key is absent.
        let none = EnforcedTriggers::default();
        let mut s = TriggerState::default();
        for _ in 0..30 {
            assert_eq!(s.record_outcome("a", false, &none), OutcomeVerdict::Ok);
        }
    }

    #[test]
    fn monitor_rereads_file_after_change() {
        let dir = tempfile::tempdir().unwrap();
        let m = TriggerMonitor {
            state: Mutex::new(TriggerState::default()),
            config: Mutex::new(None),
            cost: Mutex::new(None),
        };
        assert_eq!(m.triggers(dir.path()), EnforcedTriggers::default());
        std::fs::write(
            dir.path().join("KILLSWITCH.toml"),
            "[triggers]\nmax_replies_per_minute = 1\n",
        )
        .unwrap();
        assert_eq!(m.triggers(dir.path()).max_replies_per_minute, Some(1));
        assert!(m.admit_reply(dir.path(), "s"));
        assert!(!m.admit_reply(dir.path(), "s"));
    }

    #[tokio::test]
    async fn cost_limit_unset_never_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(global().cost_limit_reached(dir.path()).await.is_none());
    }

    #[test]
    fn scope_eviction_is_lru_and_keeps_active_scopes() {
        let mut st = TriggerState::default();
        let t = EnforcedTriggers { max_consecutive_errors: Some(3), ..Default::default() };
        // An active scope with two errors recorded.
        st.record_outcome("active", false, &t);
        st.record_outcome("active", false, &t);
        for i in 0..MAX_SCOPES + 50 {
            st.record_outcome(&format!("flood-{i}"), true, &t);
            if i % 1000 == 0 {
                // Touch the active scope so it stays recent.
                let _ = st.scope("active");
            }
        }
        assert!(st.scopes.len() <= MAX_SCOPES);
        // Its counter survived the flood: the third error escalates.
        assert!(matches!(st.record_outcome("active", false, &t), OutcomeVerdict::Escalate(_)));
    }
}

/// Reply-path hook: record one outcome for `session_id` and, when an error
/// trigger fires, escalate that scope's failsafe level and audit it.
pub(crate) async fn record_reply_outcome(
    ctx: &crate::channel_reply::ReplyContext,
    session_id: &str,
    agent_id: &str,
    success: bool,
) {
    let verdict = global().record_outcome(&ctx.home_dir, session_id, success);
    let OutcomeVerdict::Escalate(reason) = verdict else {
        return;
    };
    let level = match ctx.failsafe {
        Some(ref failsafe) => failsafe.escalate(session_id, &reason).await.label(),
        None => "unavailable",
    };
    tracing::warn!(session_id, agent_id, %reason, level, "Killswitch trigger escalated failsafe");
    crate::security_autopilot::audit_and_emit(
        &ctx.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "killswitch_trigger",
            agent_id,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({ "session_id": session_id, "reason": reason, "level": level }),
        ),
    );
}

/// Reply-path gate run before any AI call: per-scope reply rate and the
/// global 24 h cost limit. `Some(reply)` stops the turn with that reply
/// (empty string = silent drop, already recorded).
pub(crate) async fn gate_before_reply(
    ctx: &crate::channel_reply::ReplyContext,
    session_id: &str,
    user_id: &str,
    agent_id: &str,
) -> Option<String> {
    let monitor = global();
    if let Some((spent, limit)) = monitor.cost_limit_reached(&ctx.home_dir).await {
        use duduclaw_security::failsafe::FailsafeLevel;
        let reason = format!(
            "killswitch trigger: 24h API spend ${spent:.2} reached cost_limit_usd = ${limit:.2}"
        );
        if let Some(ref failsafe) = ctx.failsafe {
            if failsafe.get_level("__global__").await < FailsafeLevel::L2Restricted {
                failsafe
                    .set_level("__global__", FailsafeLevel::L2Restricted, &reason)
                    .await;
                tracing::warn!(%reason, "Killswitch cost limit reached — global scope restricted");
                crate::security_autopilot::audit_and_emit(
                    &ctx.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "killswitch_trigger",
                        agent_id,
                        duduclaw_security::audit::Severity::Warning,
                        serde_json::json!({ "scope": "__global__", "reason": reason, "level": "Restricted" }),
                    ),
                );
            }
            return Some(
                failsafe
                    .canned_reply(FailsafeLevel::L2Restricted)
                    .unwrap_or("Service restricted.")
                    .to_string(),
            );
        }
        return Some("Service restricted.".to_string());
    }
    if !monitor.admit_reply(&ctx.home_dir, session_id) {
        tracing::warn!(session_id, "Killswitch max_replies_per_minute reached — message dropped");
        crate::channel_reply::record_silent_reply(
            &ctx.home_dir,
            session_id,
            user_id,
            "silent_by_design: killswitch_rate_limited",
        );
        return Some(String::new());
    }
    None

}
