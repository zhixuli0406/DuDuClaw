//! Proactive-message timing engine (U1) — "send at the next natural moment".
//!
//! Evidence base: arXiv:2602.00880 (timing aligned to the user's cognitive
//! state improves outcomes) and arXiv:2509.24073 (the three killers of
//! proactive agents: rigid schedules, barging into active turns,
//! over-promising). Before U1, agent-initiated sends fired the moment a
//! fixed interval elapsed. This module converts that into: *the interval
//! decides that a message is due; the gate decides when it lands.*
//!
//! ## What is gated (and what is not)
//!
//! - GATED: heartbeat silence breaker + PROACTIVE.md check notifications
//!   (agent-initiated nudges, see `heartbeat.rs`).
//! - NOT GATED: user-scheduled reminders (`reminder_scheduler.rs` in the
//!   gateway). "Remind me at 9am" fires at 9am, period — see [`gate_applies`].
//!
//! ## Deterministic timing model (zero LLM)
//!
//! Per (agent, optional channel) rhythm is recomputed on demand from the
//! session store (`<home>/sessions.db`, `session_messages.role = 'user'`
//! timestamps). Recompute-on-demand was chosen over a persisted histogram
//! file because:
//!   1. the session store already records every user message with agent
//!      attribution — a parallel JSON store would need writer hooks in the
//!      channel ingest paths and would inevitably drift;
//!   2. the query runs only when a proactive event is actually due (minutes
//!      to hours apart), is `LIMIT`-capped, and opens the DB read-only;
//!   3. a missing / corrupt / unreadable DB degrades to cold start
//!      (= today's behaviour), never a crash and never a lost message.
//!
//! Histogram granularity: 24 hour-of-day slots (not 168 weekday×hour slots).
//! Per-(agent,channel) message volume is modest; a 168-slot histogram would
//! be mostly zeros and classify everything as quiet hours. Daily rhythm
//! (sleep) is the dominant signal and converges ~7× faster.
//!
//! Timezone handling: buckets are keyed by **UTC hour**, matching the
//! heartbeat scheduler's UTC-default convention. Because the histogram is
//! both *learned* and *evaluated* on the same clock, no IANA conversion is
//! needed; a DST shift moves the learned pattern by one hour twice a year
//! and self-corrects as new activity accumulates.
//!
//! ## Deferral guarantees
//!
//! Deferral only — a due message is never dropped:
//! - hard cap: a message deferred for [`TimingGate::max_defer_hours`]
//!   (default 6h) sends regardless of quiet hours / mid-flow;
//! - quiet-hours deferrals target "quiet run ends + 30 min", clamped to the
//!   cap, whichever comes first;
//! - cold start (no history) ⇒ `SendNow` (exactly today's behaviour);
//! - any evaluation error ⇒ `SendNow` (fail-safe, per project convention:
//!   a gate failure must never lose a message).
//!
//! ## Kill switch
//!
//! Global `config.toml`:
//!
//! ```toml
//! [proactive]
//! natural_timing = false   # default: true
//! ```
//!
//! Default ON is justified because the gate is strictly gentler than the
//! pre-U1 behaviour: it can only *delay* (never drop) a nudge, is bounded
//! by a 6h cap, and is invisible for agents with no interaction history.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Timelike, Utc};
use rusqlite::OpenFlags;
use tracing::{debug, info, warn};

// ── Public decision types ─────────────────────────────────────

/// Why a send was deferred. Stable labels — they appear in info logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason {
    /// The current UTC hour has zero historical user activity — the user is
    /// most likely asleep / away; waking them costs goodwill.
    QuietHours,
    /// The user sent a message within the mid-flow window — they are in an
    /// active turn; barging in interrupts their flow.
    MidFlow,
}

impl std::fmt::Display for DeferReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeferReason::QuietHours => write!(f, "quiet-hours"),
            DeferReason::MidFlow => write!(f, "mid-flow"),
        }
    }
}

/// The gate's verdict for a due proactive message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendDecision {
    /// Send immediately (today's behaviour).
    SendNow,
    /// Hold the message and re-evaluate at (or after) `until`.
    Defer {
        until: DateTime<Utc>,
        reason: DeferReason,
    },
}

/// Kind of outbound proactive message. Only agent-initiated nudges are
/// subject to the timing gate; user-scheduled reminders always fire on time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProactiveKind {
    /// PROACTIVE.md check notification (agent decides to nudge the user).
    AgentNudge,
    /// Heartbeat silence-breaker forced reflection trigger.
    SilenceBreaker,
    /// Explicit user-set reminder ("remind me at 9am"). NEVER gated.
    UserScheduledReminder,
}

/// Whether the natural-timing gate applies to this kind of message.
///
/// Structural guarantee for the reminder exemption: `reminder_scheduler.rs`
/// never calls the gate, and this predicate documents + tests that boundary.
pub const fn gate_applies(kind: ProactiveKind) -> bool {
    match kind {
        ProactiveKind::AgentNudge | ProactiveKind::SilenceBreaker => true,
        ProactiveKind::UserScheduledReminder => false,
    }
}

// ── Interaction history ───────────────────────────────────────

/// Learned rhythm for one (agent, optional channel) pair.
#[derive(Debug, Clone, Default)]
pub struct InteractionHistory {
    /// User-message counts bucketed by UTC hour of day.
    pub hour_counts: [u32; 24],
    /// Total user messages observed in the lookback window.
    pub total: u32,
    /// Timestamp of the most recent user message (for mid-flow detection).
    pub last_user_message: Option<DateTime<Utc>>,
}

impl InteractionHistory {
    /// Empty history — the cold-start / fail-safe value.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a histogram from raw user-message timestamps (any order).
    pub fn from_timestamps(timestamps: &[DateTime<Utc>]) -> Self {
        let mut h = Self::empty();
        for ts in timestamps {
            h.hour_counts[ts.hour() as usize] += 1;
            h.total += 1;
            if h.last_user_message.is_none_or(|last| *ts > last) {
                h.last_user_message = Some(*ts);
            }
        }
        h
    }
}

// ── TimingGate ────────────────────────────────────────────────

/// Pure, unit-testable decision function over timestamps. No I/O.
#[derive(Debug, Clone)]
pub struct TimingGate {
    /// User activity within this window ⇒ mid-flow, defer.
    pub mid_flow_window_minutes: i64,
    /// Hard cap on total deferral; past this the message sends regardless.
    pub max_defer_hours: i64,
    /// Grace added after a quiet run ends (don't fire the second they wake).
    pub quiet_exit_grace_minutes: i64,
    /// Minimum observed user messages before the quiet-hours histogram is
    /// trusted. Mid-flow detection needs no maturity — one recent message
    /// is already strong evidence of an active turn.
    pub min_quiet_events: u32,
}

impl Default for TimingGate {
    fn default() -> Self {
        Self {
            mid_flow_window_minutes: 10,
            max_defer_hours: 6,
            quiet_exit_grace_minutes: 30,
            min_quiet_events: 20,
        }
    }
}

impl TimingGate {
    /// Decide whether a due proactive message should send now or be deferred.
    ///
    /// `deferred_since` is when this message first got deferred (from the
    /// caller's [`DeferralLedger`]); `None` means this is the first
    /// evaluation. Pure function — synthetic histories in tests exercise
    /// every branch.
    pub fn evaluate(
        &self,
        now: DateTime<Utc>,
        history: &InteractionHistory,
        deferred_since: Option<DateTime<Utc>>,
    ) -> SendDecision {
        // 1. Hard cap: a message deferred past its cap sends regardless.
        //    This is the "never drop, never starve" guarantee.
        if let Some(since) = deferred_since {
            if now.signed_duration_since(since) >= Duration::hours(self.max_defer_hours) {
                return SendDecision::SendNow;
            }
        }
        // Any Defer target below is clamped so re-evaluation at `until`
        // lands at-or-past the cap and trips rule 1.
        let cap_deadline = deferred_since.unwrap_or(now) + Duration::hours(self.max_defer_hours);

        // 2. Cold start: no history ⇒ exactly today's behaviour.
        if history.total == 0 {
            return SendDecision::SendNow;
        }

        // 3. Mid-flow: user messaged within the window ⇒ they're in an
        //    active turn; wait for it to settle. Negative elapsed (future
        //    timestamp = corrupt row / clock skew) is ignored — fail-safe.
        if let Some(last) = history.last_user_message {
            let elapsed = now.signed_duration_since(last);
            let window = Duration::minutes(self.mid_flow_window_minutes);
            if elapsed >= Duration::zero() && elapsed < window {
                let until = (last + window).min(cap_deadline);
                return SendDecision::Defer {
                    until,
                    reason: DeferReason::MidFlow,
                };
            }
        }

        // 4. Quiet hours: only once the histogram is mature enough to mean
        //    something. A zero-count hour = the user has never been active
        //    then ⇒ don't wake them; defer to the first active hour + grace.
        if history.total >= self.min_quiet_events {
            let hour = now.hour() as usize;
            if history.hour_counts[hour] == 0 {
                // Truncate `now` down to the start of its hour, then scan
                // forward for the first historically-active hour.
                let hour_start = now
                    - Duration::minutes(now.minute() as i64)
                    - Duration::seconds(now.second() as i64)
                    - Duration::nanoseconds(now.nanosecond() as i64);
                for ahead in 1..=24usize {
                    if history.hour_counts[(hour + ahead) % 24] > 0 {
                        let quiet_end = hour_start + Duration::hours(ahead as i64);
                        let until = (quiet_end + Duration::minutes(self.quiet_exit_grace_minutes))
                            .min(cap_deadline);
                        return SendDecision::Defer {
                            until,
                            reason: DeferReason::QuietHours,
                        };
                    }
                }
                // All 24 slots quiet with total > 0 is impossible, but if a
                // future refactor breaks that invariant: fail-safe SendNow.
            }
        }

        SendDecision::SendNow
    }
}

// ── History loading (read-only over sessions.db) ──────────────

/// Lookback window for rhythm learning.
const HISTORY_WINDOW_DAYS: i64 = 30;
/// Row cap — recent messages dominate anyway; keeps the query O(1)-ish.
const HISTORY_ROW_CAP: i64 = 5000;

/// Load the interaction history for `agent_id` from `<home>/sessions.db`.
///
/// Read-only (`SQLITE_OPEN_READ_ONLY`) — this module never mutates the
/// session store. `channel` (e.g. `"telegram"`) restricts to sessions whose
/// id carries that channel prefix (`<channel>:...`, the gateway's session-id
/// convention) — an **anchored** prefix, not a substring match.
///
/// Every failure mode (missing DB, corrupt file, schema drift, bad
/// timestamps) returns an empty history ⇒ the gate cold-starts to
/// `SendNow`. Never crashes, never loses a message. Sync — call from
/// `spawn_blocking` in async contexts.
pub fn load_interaction_history(
    home_dir: &Path,
    agent_id: &str,
    channel: Option<&str>,
) -> InteractionHistory {
    let db_path = home_dir.join("sessions.db");
    if !db_path.exists() {
        return InteractionHistory::empty();
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            debug!(
                agent = agent_id,
                "Natural-timing: sessions.db open failed ({e}); cold start"
            );
            return InteractionHistory::empty();
        }
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));

    // RFC3339 strings from `Utc::now().to_rfc3339()` compare lexicographically
    // — the same convention `heartbeat::poll_assigned_tasks` already relies on.
    let cutoff = (Utc::now() - Duration::days(HISTORY_WINDOW_DAYS)).to_rfc3339();

    // `undone_at IS NULL`: tombstoned turns (/undo, /rollback) are not real
    // interactions — counting them would skew the timing model. Pre-migration
    // DBs without the column fail the prepare ⇒ empty history ⇒ SendNow
    // (the documented fail-safe cold start).
    let mut sql = String::from(
        "SELECT m.timestamp FROM session_messages m \
         JOIN sessions s ON m.session_id = s.id \
         WHERE s.agent_id = ?1 AND m.role = 'user' AND m.undone_at IS NULL \
           AND m.timestamp >= ?2",
    );
    let channel_prefix = channel.map(|c| format!("{c}:%"));
    if channel_prefix.is_some() {
        sql.push_str(" AND s.id LIKE ?3 ORDER BY m.id DESC LIMIT ?4");
    } else {
        sql.push_str(" ORDER BY m.id DESC LIMIT ?3");
    }

    let run = || -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(&sql)?;
        let rows: rusqlite::Result<Vec<String>> = match &channel_prefix {
            Some(prefix) => stmt
                .query_map(
                    rusqlite::params![agent_id, cutoff, prefix, HISTORY_ROW_CAP],
                    |row| row.get::<_, String>(0),
                )?
                .collect(),
            None => stmt
                .query_map(
                    rusqlite::params![agent_id, cutoff, HISTORY_ROW_CAP],
                    |row| row.get::<_, String>(0),
                )?
                .collect(),
        };
        rows
    };

    let raw = match run() {
        Ok(r) => r,
        Err(e) => {
            debug!(
                agent = agent_id,
                "Natural-timing: history query failed ({e}); cold start"
            );
            return InteractionHistory::empty();
        }
    };

    let timestamps: Vec<DateTime<Utc>> = raw
        .iter()
        .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .collect();

    InteractionHistory::from_timestamps(&timestamps)
}

// ── Kill switch ───────────────────────────────────────────────

/// Read `[proactive] natural_timing` from the **global** `config.toml`.
///
/// Default ON: missing file / missing key / malformed TOML all yield `true`.
/// Default-on is safe because the gate is deferral-only with a hard cap —
/// strictly gentler than today's fire-immediately, and a no-op with no
/// history. Mirrors the `gvu::trigger::agent_gvu_enabled` read pattern.
pub fn natural_timing_enabled(home_dir: &Path) -> bool {
    let path = home_dir.join("config.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return true;
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return true;
    };
    value
        .get("proactive")
        .and_then(|p| p.get("natural_timing"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

// ── Deferral ledger ───────────────────────────────────────────

/// One open deferral for a keyed message.
#[derive(Debug, Clone, Copy)]
pub struct DeferralWindow {
    /// First time this message was deferred (anchors the hard cap).
    pub since: DateTime<Utc>,
    /// Re-evaluate at (or after) this instant.
    pub until: DateTime<Utc>,
    pub reason: DeferReason,
}

/// In-memory tracker of open deferrals, keyed by caller-chosen strings
/// (e.g. `"silence:<agent>"`, `"proactive:<agent>"`).
///
/// In-memory is sufficient: state loss on restart merely restarts the 6h
/// cap clock — the underlying due-condition (silence threshold, PROACTIVE.md
/// check) re-fires on its own schedule, so no message is ever lost.
#[derive(Debug, Default)]
pub struct DeferralLedger {
    inner: Mutex<HashMap<String, DeferralWindow>>,
}

impl DeferralLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, DeferralWindow>> {
        // A poisoned lock must not take the send path down with it.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, key: &str) -> Option<DeferralWindow> {
        self.lock().get(key).copied()
    }

    /// Record a deferral. Keeps the original `since` if the key is already
    /// open (so the hard cap anchors at the FIRST deferral, not the latest).
    pub fn mark(&self, key: &str, now: DateTime<Utc>, until: DateTime<Utc>, reason: DeferReason) {
        let mut map = self.lock();
        let since = map.get(key).map(|w| w.since).unwrap_or(now);
        map.insert(
            key.to_string(),
            DeferralWindow {
                since,
                until,
                reason,
            },
        );
    }

    pub fn clear(&self, key: &str) {
        self.lock().remove(key);
    }
}

// ── One-stop keyed gate for the heartbeat send paths ──────────

/// Evaluate the natural-timing gate for one keyed agent-initiated message.
///
/// Handles the full lifecycle so callers stay thin:
/// 1. kill switch off ⇒ `SendNow` (pre-U1 behaviour, ledger cleared);
/// 2. still inside a previously-computed deferral window ⇒ `Defer` without
///    re-querying sessions.db (debug log only — the decision was already
///    logged at info when first made; no per-tick log noise);
/// 3. otherwise load history (`spawn_blocking`, read-only) and evaluate;
///    `SendNow` clears the key, `Defer` records it and logs at info.
///
/// Fail-safe: any internal error ⇒ `SendNow`. A gate failure must never
/// turn into a lost message.
pub async fn gate_keyed_send(
    home_dir: &Path,
    agent_id: &str,
    channel: Option<&str>,
    key: &str,
    ledger: &DeferralLedger,
    now: DateTime<Utc>,
    context: &str,
) -> SendDecision {
    if !natural_timing_enabled(home_dir) {
        ledger.clear(key);
        return SendDecision::SendNow;
    }

    if let Some(win) = ledger.get(key) {
        if now < win.until {
            debug!(
                key,
                context,
                until = %win.until.to_rfc3339(),
                reason = %win.reason,
                "Natural-timing deferral window still open"
            );
            return SendDecision::Defer {
                until: win.until,
                reason: win.reason,
            };
        }
    }

    let history = {
        let home = home_dir.to_path_buf();
        let aid = agent_id.to_string();
        let ch = channel.map(|s| s.to_string());
        match tokio::task::spawn_blocking(move || {
            load_interaction_history(&home, &aid, ch.as_deref())
        })
        .await
        {
            Ok(h) => h,
            Err(e) => {
                warn!(
                    key,
                    context, "Natural-timing history load panicked ({e}); sending now"
                );
                ledger.clear(key);
                return SendDecision::SendNow;
            }
        }
    };

    let deferred_since = ledger.get(key).map(|w| w.since);
    match TimingGate::default().evaluate(now, &history, deferred_since) {
        SendDecision::SendNow => {
            ledger.clear(key);
            SendDecision::SendNow
        }
        SendDecision::Defer { until, reason } => {
            let newly = deferred_since.is_none();
            ledger.mark(key, now, until, reason);
            if newly {
                info!(
                    key,
                    context,
                    until = %until.to_rfc3339(),
                    reason = %reason,
                    "Proactive send deferred to natural moment"
                );
            } else {
                debug!(
                    key,
                    context,
                    until = %until.to_rfc3339(),
                    reason = %reason,
                    "Proactive send re-deferred"
                );
            }
            SendDecision::Defer { until, reason }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// History with `per` user messages in each of the given UTC hours.
    fn history_active_in(hours: &[u32], per: u32) -> InteractionHistory {
        let mut h = InteractionHistory::empty();
        for &hr in hours {
            h.hour_counts[hr as usize] = per;
            h.total += per;
        }
        h
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    // ── Pure gate: cold start ──

    #[test]
    fn cold_start_sends_now() {
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 3, 15, 0);
        assert_eq!(
            gate.evaluate(now, &InteractionHistory::empty(), None),
            SendDecision::SendNow,
            "no history must behave exactly like today (SendNow)"
        );
    }

    // ── Pure gate: quiet hours ──

    #[test]
    fn quiet_hours_defers_until_active_hour_plus_grace() {
        let gate = TimingGate::default();
        // Active 09:00–17:59 UTC (45 events ≥ min_quiet_events).
        let history = history_active_in(&[9, 10, 11, 12, 13, 14, 15, 16, 17], 5);
        // 04:15 UTC is a zero-count hour → defer to 09:00 + 30min grace.
        let now = utc(2026, 7, 11, 4, 15, 0);
        match gate.evaluate(now, &history, None) {
            SendDecision::Defer { until, reason } => {
                assert_eq!(reason, DeferReason::QuietHours);
                assert_eq!(until, utc(2026, 7, 11, 9, 30, 0));
            }
            other => panic!("expected quiet-hours Defer, got {other:?}"),
        }
    }

    #[test]
    fn quiet_hours_defer_target_is_clamped_to_cap() {
        let gate = TimingGate::default();
        let history = history_active_in(&[9, 10, 11, 12, 13, 14, 15, 16, 17], 5);
        // 03:15 → natural target 09:30 exceeds now+6h = 09:15 → clamp.
        let now = utc(2026, 7, 11, 3, 15, 0);
        match gate.evaluate(now, &history, None) {
            SendDecision::Defer { until, .. } => {
                assert_eq!(until, utc(2026, 7, 11, 9, 15, 0), "defer clamped to 6h cap");
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn cap_overrides_quiet_hours_and_sends_regardless() {
        let gate = TimingGate::default();
        let history = history_active_in(&[9, 10, 11, 12, 13, 14, 15, 16, 17], 5);
        let now = utc(2026, 7, 11, 3, 15, 0);
        // Deferred since 6h ago → still quiet, but the cap wins.
        let since = now - Duration::hours(6);
        assert_eq!(
            gate.evaluate(now, &history, Some(since)),
            SendDecision::SendNow,
            "a message deferred past its cap must send regardless"
        );
    }

    #[test]
    fn immature_histogram_never_classifies_quiet_hours() {
        let gate = TimingGate::default();
        // Only 5 events (< min_quiet_events 20), all at 09:00.
        let history = history_active_in(&[9], 5);
        let now = utc(2026, 7, 11, 3, 15, 0); // zero-count hour
        assert_eq!(
            gate.evaluate(now, &history, None),
            SendDecision::SendNow,
            "sparse history must not invent quiet hours"
        );
    }

    // ── Pure gate: mid-flow ──

    #[test]
    fn mid_flow_defers_until_window_end() {
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 10, 0, 0);
        let mut history = history_active_in(&[9, 10, 11, 12, 13, 14, 15, 16, 17], 5);
        history.last_user_message = Some(now - Duration::minutes(3));
        match gate.evaluate(now, &history, None) {
            SendDecision::Defer { until, reason } => {
                assert_eq!(reason, DeferReason::MidFlow);
                assert_eq!(until, now + Duration::minutes(7)); // last + 10min
            }
            other => panic!("expected mid-flow Defer, got {other:?}"),
        }
    }

    #[test]
    fn mid_flow_applies_even_with_immature_history() {
        // A single recent message is strong evidence of an active turn —
        // no histogram maturity required.
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 10, 0, 0);
        let history = InteractionHistory::from_timestamps(&[now - Duration::minutes(2)]);
        match gate.evaluate(now, &history, None) {
            SendDecision::Defer { reason, .. } => assert_eq!(reason, DeferReason::MidFlow),
            other => panic!("expected mid-flow Defer, got {other:?}"),
        }
    }

    #[test]
    fn stale_activity_does_not_trigger_mid_flow() {
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 10, 0, 0);
        let mut history = history_active_in(&[9, 10, 11, 12, 13, 14, 15, 16, 17], 5);
        history.last_user_message = Some(now - Duration::minutes(30));
        assert_eq!(gate.evaluate(now, &history, None), SendDecision::SendNow);
    }

    #[test]
    fn future_timestamp_is_ignored_fail_safe() {
        // Corrupt row / clock skew: last message "in the future" must not
        // wedge the gate into deferring.
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 10, 0, 0);
        let mut history = history_active_in(&[9, 10, 11], 10);
        history.last_user_message = Some(now + Duration::minutes(5));
        assert_eq!(gate.evaluate(now, &history, None), SendDecision::SendNow);
    }

    #[test]
    fn cap_overrides_mid_flow() {
        let gate = TimingGate::default();
        let now = utc(2026, 7, 11, 10, 0, 0);
        let mut history = history_active_in(&[9, 10, 11], 10);
        history.last_user_message = Some(now - Duration::minutes(1));
        let since = now - Duration::hours(7);
        assert_eq!(
            gate.evaluate(now, &history, Some(since)),
            SendDecision::SendNow
        );
    }

    // ── Histogram learning ──

    #[test]
    fn histogram_learns_from_synthetic_timestamps() {
        let ts = vec![
            utc(2026, 7, 1, 9, 5, 0),
            utc(2026, 7, 2, 9, 55, 0),
            utc(2026, 7, 3, 14, 0, 0),
            utc(2026, 7, 4, 23, 59, 59),
        ];
        let h = InteractionHistory::from_timestamps(&ts);
        assert_eq!(h.total, 4);
        assert_eq!(h.hour_counts[9], 2);
        assert_eq!(h.hour_counts[14], 1);
        assert_eq!(h.hour_counts[23], 1);
        assert_eq!(h.hour_counts[10], 0);
        assert_eq!(h.last_user_message, Some(utc(2026, 7, 4, 23, 59, 59)));
    }

    // ── User-scheduled reminders are never gated ──

    #[test]
    fn user_scheduled_reminders_are_never_gated() {
        assert!(!gate_applies(ProactiveKind::UserScheduledReminder));
        assert!(gate_applies(ProactiveKind::AgentNudge));
        assert!(gate_applies(ProactiveKind::SilenceBreaker));
    }

    // ── Kill switch config parsing ──

    #[test]
    fn natural_timing_defaults_on_when_config_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(natural_timing_enabled(tmp.path()));
    }

    #[test]
    fn natural_timing_kill_switch_reads_false() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[proactive]\nnatural_timing = false\n",
        )
        .unwrap();
        assert!(!natural_timing_enabled(tmp.path()));
    }

    #[test]
    fn natural_timing_defaults_on_for_malformed_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "not = [valid toml").unwrap();
        assert!(natural_timing_enabled(tmp.path()));
    }

    // ── sessions.db loading ──

    /// Minimal sessions.db mirroring the gateway `SessionManager` schema.
    fn make_sessions_db(home: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(home.join("sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL,
                 summary TEXT DEFAULT '',
                 total_tokens INTEGER DEFAULT 0,
                 last_active TEXT NOT NULL,
                 model TEXT DEFAULT 'auto',
                 created_at TEXT NOT NULL
             );
             CREATE TABLE session_messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT NOT NULL,
                 tokens INTEGER DEFAULT 0,
                 timestamp TEXT NOT NULL,
                 undone_at TEXT
             );",
        )
        .unwrap();
        conn
    }

    fn insert_session(conn: &rusqlite::Connection, id: &str, agent: &str) {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (id, agent_id, last_active, created_at) VALUES (?1, ?2, ?3, ?3)",
            rusqlite::params![id, agent, now],
        )
        .unwrap();
    }

    fn insert_message(conn: &rusqlite::Connection, session: &str, role: &str, ts: DateTime<Utc>) {
        conn.execute(
            "INSERT INTO session_messages (session_id, role, content, timestamp)
             VALUES (?1, ?2, 'x', ?3)",
            rusqlite::params![session, role, ts.to_rfc3339()],
        )
        .unwrap();
    }

    /// A recent timestamp landing in a specific UTC hour bucket.
    fn recent_at_hour(hour: u32) -> DateTime<Utc> {
        (Utc::now() - Duration::days(1))
            .date_naive()
            .and_hms_opt(hour, 30, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn load_history_reads_session_store_with_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = make_sessions_db(tmp.path());
        insert_session(&conn, "telegram:123", "agent-a");
        insert_session(&conn, "discord:9", "agent-a");
        insert_session(&conn, "telegram:555", "agent-b");

        insert_message(&conn, "telegram:123", "user", recent_at_hour(9));
        insert_message(&conn, "telegram:123", "user", recent_at_hour(9));
        insert_message(&conn, "telegram:123", "assistant", recent_at_hour(9)); // excluded
        insert_message(&conn, "discord:9", "user", recent_at_hour(14));
        insert_message(&conn, "telegram:555", "user", recent_at_hour(20)); // other agent
                                                                           // Beyond the 30-day lookback window — excluded.
        insert_message(
            &conn,
            "telegram:123",
            "user",
            Utc::now() - Duration::days(45),
        );
        drop(conn);

        // Agent-level: both channels, user rows only, window-filtered.
        let h = load_interaction_history(tmp.path(), "agent-a", None);
        assert_eq!(h.total, 3);
        assert_eq!(h.hour_counts[9], 2);
        assert_eq!(h.hour_counts[14], 1);
        assert_eq!(
            h.hour_counts[20], 0,
            "other agent's activity must not leak in"
        );

        // Channel filter: anchored prefix.
        let h_tg = load_interaction_history(tmp.path(), "agent-a", Some("telegram"));
        assert_eq!(h_tg.total, 2);
        assert_eq!(
            h_tg.hour_counts[14], 0,
            "discord session excluded by channel filter"
        );
    }

    #[test]
    fn load_history_excludes_tombstoned_turns() {
        // /undo //rollback tombstones (`undone_at`) are not real interactions
        // and must not feed the timing model.
        let tmp = tempfile::tempdir().unwrap();
        let conn = make_sessions_db(tmp.path());
        insert_session(&conn, "telegram:1", "agent-a");
        insert_message(&conn, "telegram:1", "user", recent_at_hour(9));
        conn.execute(
            "INSERT INTO session_messages (session_id, role, content, timestamp, undone_at)
             VALUES ('telegram:1', 'user', 'x', ?1, ?2)",
            rusqlite::params![
                recent_at_hour(10).to_rfc3339(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
        drop(conn);

        let h = load_interaction_history(tmp.path(), "agent-a", None);
        assert_eq!(h.total, 1, "tombstoned user turn must be excluded");
        assert_eq!(h.hour_counts[10], 0);
    }

    #[test]
    fn load_history_missing_db_is_cold_start() {
        let tmp = tempfile::tempdir().unwrap();
        let h = load_interaction_history(tmp.path(), "any", None);
        assert_eq!(h.total, 0);
    }

    #[test]
    fn load_history_corrupt_db_is_cold_start_not_crash() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("sessions.db"),
            b"this is not a sqlite file at all",
        )
        .unwrap();
        let h = load_interaction_history(tmp.path(), "any", None);
        assert_eq!(h.total, 0, "corrupt store must degrade to cold start");
    }

    // ── Ledger ──

    #[test]
    fn ledger_mark_preserves_original_since() {
        let ledger = DeferralLedger::new();
        let t0 = utc(2026, 7, 11, 3, 0, 0);
        let t1 = t0 + Duration::minutes(30);
        ledger.mark("k", t0, t0 + Duration::hours(1), DeferReason::QuietHours);
        ledger.mark("k", t1, t1 + Duration::hours(1), DeferReason::MidFlow);
        let win = ledger.get("k").unwrap();
        assert_eq!(win.since, t0, "cap must anchor at the FIRST deferral");
        assert_eq!(win.reason, DeferReason::MidFlow);
        ledger.clear("k");
        assert!(ledger.get("k").is_none());
    }

    // ── End-to-end keyed gate + kill switch ──

    /// Home dir with a sessions.db whose learned rhythm makes "now" a quiet
    /// hour (all activity concentrated in one other hour, 25 events).
    fn home_with_quiet_now(now: DateTime<Utc>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let conn = make_sessions_db(tmp.path());
        insert_session(&conn, "telegram:1", "agent-a");
        let active_hour = (now.hour() + 12) % 24; // opposite side of the day
        for _ in 0..25 {
            insert_message(&conn, "telegram:1", "user", recent_at_hour(active_hour));
        }
        tmp
    }

    #[tokio::test]
    async fn gate_keyed_send_defers_in_learned_quiet_hours() {
        let now = Utc::now();
        let tmp = home_with_quiet_now(now);
        let ledger = DeferralLedger::new();
        let decision = gate_keyed_send(
            tmp.path(),
            "agent-a",
            None,
            "silence:agent-a",
            &ledger,
            now,
            "test",
        )
        .await;
        assert!(
            matches!(
                decision,
                SendDecision::Defer {
                    reason: DeferReason::QuietHours,
                    ..
                }
            ),
            "expected quiet-hours deferral, got {decision:?}"
        );
        assert!(
            ledger.get("silence:agent-a").is_some(),
            "deferral must be recorded"
        );

        // Second evaluation inside the window: same decision, no re-query churn.
        let again = gate_keyed_send(
            tmp.path(),
            "agent-a",
            None,
            "silence:agent-a",
            &ledger,
            now + Duration::minutes(1),
            "test",
        )
        .await;
        assert!(matches!(again, SendDecision::Defer { .. }));
    }

    #[tokio::test]
    async fn gate_keyed_send_kill_switch_restores_today_behavior() {
        let now = Utc::now();
        // Identical history to the deferring test above — only the switch differs.
        let tmp = home_with_quiet_now(now);
        std::fs::write(
            tmp.path().join("config.toml"),
            "[proactive]\nnatural_timing = false\n",
        )
        .unwrap();
        let ledger = DeferralLedger::new();
        let decision = gate_keyed_send(
            tmp.path(),
            "agent-a",
            None,
            "silence:agent-a",
            &ledger,
            now,
            "test",
        )
        .await;
        assert_eq!(
            decision,
            SendDecision::SendNow,
            "kill switch must restore pre-U1 fire-immediately behaviour"
        );
        assert!(
            ledger.get("silence:agent-a").is_none(),
            "switch off must clear ledger state"
        );
    }

    #[tokio::test]
    async fn gate_keyed_send_cap_forces_send_after_max_deferral() {
        let now = Utc::now();
        let tmp = home_with_quiet_now(now);
        let ledger = DeferralLedger::new();
        // Simulate a deferral opened 6h ago whose window has expired.
        ledger.mark(
            "k",
            now - Duration::hours(6),
            now - Duration::minutes(1),
            DeferReason::QuietHours,
        );
        let decision =
            gate_keyed_send(tmp.path(), "agent-a", None, "k", &ledger, now, "test").await;
        assert_eq!(
            decision,
            SendDecision::SendNow,
            "6h cap must force the send even in quiet hours"
        );
        assert!(ledger.get("k").is_none());
    }

    #[tokio::test]
    async fn gate_keyed_send_cold_start_sends_now() {
        let tmp = tempfile::tempdir().unwrap(); // no sessions.db at all
        let ledger = DeferralLedger::new();
        let decision = gate_keyed_send(
            tmp.path(),
            "agent-a",
            None,
            "k",
            &ledger,
            Utc::now(),
            "test",
        )
        .await;
        assert_eq!(decision, SendDecision::SendNow);
    }
}
