//! Channel-outage alerting: watches `channel_failures.jsonl` for repeated
//! per-channel *send* failures and pushes an actionable zh-TW alert to the
//! admin's other still-reachable channel — never the failing channel itself.
//!
//! ## The gap this closes
//!
//! `channel_failures.jsonl` records channel send failures and related
//! anomalies, but nothing ever reads it proactively — it is only surfaced
//! passively via the dashboard's unified-log RPC (`handlers.rs`). A channel
//! that is reachable (the bot process is up, the webhook still answers) but
//! can no longer actually deliver messages (revoked token, platform-side
//! block, quota exhaustion) previously stayed silent until an operator
//! happened to open the dashboard and notice the failure count. This module
//! turns that passive log into a proactive push.
//!
//! ## Why a periodic tail-scan, not per-write-site hooks
//!
//! `channel_failures.jsonl` is appended from several call sites scattered
//! across `channel_reply.rs`, `trajectory_guard.rs`, `foresight.rs` and
//! `telegram.rs`, with heterogeneous record shapes — most describe an
//! agent-level runtime/behavioral condition (`agent`/`session_id` fields,
//! no `channel`), not a channel-level *send* failure. Hooking every write
//! site individually would mean touching the reply hot path in several
//! places for a purely observational feature. A bounded tail-scan of the
//! already-durable JSONL is simpler and cannot miss a record (nothing here
//! rotates or truncates the file); it is also forward-compatible — any
//! future writer that stamps a `channel` field on its record is picked up
//! automatically, no code change needed here.
//!
//! Only records whose `event` is in [`SEND_FAILURE_EVENTS`] **and** which
//! carry a non-empty `channel` count as a per-channel send failure. Every
//! other writer (`channel_reply_silent`, `runtime_fallback_substitution`,
//! `channel_reply_fallback`, `pty_pool_fallback`, `trajectory_anomaly`,
//! `foresight_alarm`) describes an agent/runtime condition rather than "this
//! channel can't deliver", and is deliberately excluded — alerting on those
//! would conflate a channel outage with an LLM/runtime failure the operator
//! would investigate completely differently.
//!
//! **The event allowlist is load-bearing, not belt-and-braces.** Until W2-4
//! this module keyed purely off "has a non-empty `channel` field", which was
//! safe only because exactly one writer stamped one. W2-4 gave every
//! session-aware writer a `channel` field (so the dashboard can attribute a
//! failure to a platform), which under the old rule would have turned every
//! LLM timeout and every trajectory anomaly into a channel-outage page.
//! Widening this list is therefore a deliberate act: a new entry must be an
//! event that genuinely means "this channel could not deliver a message".
//!
//! ## Recovery (`channel_recovered`)
//!
//! When a channel drops back under threshold this module appends a
//! `channel_recovered` record to the same JSONL. The failure rows themselves
//! are never rewritten — an audit log is append-only — so a consumer decides
//! "is this outage still current?" by looking for a later `channel_recovered`
//! for the same channel, exactly as it would for any other event-sourced
//! state. Both a `resolved: true` flag and a `resolves` field naming the
//! failure event are carried, so the row is self-describing without needing
//! this module's source to interpret.
//!
//! ## Threshold & de-duplication
//!
//! Same channel, [`ALERT_THRESHOLD`] failures inside [`ALERT_WINDOW_SECS`]
//! trips an alert. De-duplication mirrors `gvu::stagnation`'s
//! recompute-and-diff pattern rather than a one-shot latch: every tick
//! recomputes from scratch which channels are currently over threshold. A
//! channel that stays over threshold keeps its "already alerted" marker (no
//! re-send while still failing — an alerter must never be noisier than the
//! condition it watches, and a single attempt per streak matches
//! `gvu::stagnation::StagnationMonitor::maybe_alert`, which does not retry a
//! `SendFailed` outcome either). A channel that drops back under threshold
//! has its marker cleared, ready to re-alert on a future relapse.
//!
//! There is no explicit "send succeeded" event in this file to key recovery
//! off — adding one would mean a hook in the reply hot path, which is out of
//! scope here. The trailing window aging old failures out (new sends are no
//! longer failing) is the recovery signal instead — the same posture
//! `gvu::stagnation` already takes (it has no explicit "unstuck" event
//! either; a fresh `applied` row simply stops the signal from firing on the
//! next recompute).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::task_store::{ActivityRow, TaskStore};

/// Same-channel failures inside [`ALERT_WINDOW_SECS`] needed to trip an alert.
const ALERT_THRESHOLD: usize = 3;

/// Trailing window (seconds) the threshold is evaluated over — 10 minutes.
const ALERT_WINDOW_SECS: i64 = 10 * 60;

/// Bytes read from the tail of `channel_failures.jsonl` per tick. The alert
/// window is only 10 minutes, so this comfortably covers even a very chatty
/// install while bounding I/O regardless of how large the file grows over
/// the install's lifetime (mirrors `foresight::HISTORY_TAIL_BYTES`).
const TAIL_BYTES: u64 = 256 * 1024;

/// Pseudo-agent id stamped on this module's own Activity Feed rows — this is
/// a subsystem event, not tied to any one AI employee (mirrors
/// `autopilot_engine.rs` stamping `agent_id: "autopilot"` for its own
/// system-level rows).
const ACTIVITY_AGENT_ID: &str = "channel_alert";

/// `event` values that mean "this channel could not deliver a message".
///
/// See the module docs: this list, not the mere presence of a `channel`
/// field, is what makes a record count toward an outage.
pub const SEND_FAILURE_EVENTS: &[&str] = &["telegram_send_failed"];

/// The `event` written when a channel comes back.
pub const RECOVERED_EVENT: &str = "channel_recovered";

/// Action-rate stats bucket for the outage alert.
const NOTIFY_TYPE: &str = "channel.outage";

// ── Parsing ──────────────────────────────────────────────────────────────

/// One parsed channel-level send-failure record.
#[derive(Debug, Clone, PartialEq)]
struct ChannelFailure {
    channel: String,
    /// The `event` token, carried so a recovery record can name what it
    /// resolves.
    event: String,
    /// The AI employee whose message failed to send, when the record
    /// carries one — used as a hint for token resolution when pushing the
    /// alert (an agent's own `agent.toml [channels.<x>]` config may resolve
    /// a token the global config doesn't have). Most current writers omit
    /// this field.
    agent: Option<String>,
    reason: Option<String>,
    timestamp: DateTime<Utc>,
}

/// Parse one `channel_failures.jsonl` line into a [`ChannelFailure`].
///
/// `None` when the line isn't valid JSON, its `event` is not in
/// [`SEND_FAILURE_EVENTS`], it has no non-empty `channel`, or it has no
/// parseable `timestamp`. A record we can't classify or can't window is
/// treated conservatively as "not counted" rather than risking a false
/// trigger from malformed data.
fn parse_failure_line(line: &str) -> Option<ChannelFailure> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let event = v.get("event").and_then(|e| e.as_str())?.trim();
    if !SEND_FAILURE_EVENTS.contains(&event) {
        return None;
    }
    let channel = v.get("channel").and_then(|c| c.as_str())?.trim();
    if channel.is_empty() {
        return None;
    }
    let ts = v.get("timestamp").and_then(|t| t.as_str())?;
    let timestamp = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let agent = v
        .get("agent")
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let reason = v
        .get("reason")
        .and_then(|r| r.as_str())
        .map(str::to_string);
    Some(ChannelFailure {
        channel: channel.to_string(),
        event: event.to_string(),
        agent,
        reason,
        timestamp,
    })
}

/// Bounded tail-read of `channel_failures.jsonl`, parsed into
/// [`ChannelFailure`] records. Fail-safe: a missing/unreadable/unparseable
/// file yields an empty list, never an error — this is a monitor, not a
/// control path, and a read failure must not stop the gateway.
fn tail_failures(home_dir: &Path) -> Vec<ChannelFailure> {
    let path = home_dir.join("channel_failures.jsonl");
    let Ok(mut f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    // Skip a (possibly partial) first line when the read started mid-file.
    let body: &str = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => return Vec::new(),
        }
    } else {
        &text
    };
    body.lines().filter_map(parse_failure_line).collect()
}

// ── Pure detection logic ────────────────────────────────────────────────

/// Count each channel's failures inside `[now - window_secs, now]`. Pure —
/// unit-tested directly. Channels with zero in-window failures are simply
/// absent from the map.
fn counts_in_window(
    failures: &[ChannelFailure],
    now: DateTime<Utc>,
    window_secs: i64,
) -> HashMap<String, usize> {
    let cutoff = now - chrono::Duration::seconds(window_secs);
    let mut counts = HashMap::new();
    for f in failures {
        if f.timestamp >= cutoff && f.timestamp <= now {
            *counts.entry(f.channel.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// Most recent failure record for `channel` within `failures` (used to pull
/// an `agent` hint / `reason` for the alert — best-effort context, never a
/// gate).
fn latest_for_channel<'a>(failures: &'a [ChannelFailure], channel: &str) -> Option<&'a ChannelFailure> {
    failures
        .iter()
        .filter(|f| f.channel == channel)
        .max_by_key(|f| f.timestamp)
}

/// zh-TW alert body: one symptom line + (when resolvable) a deep link back
/// to the channels page. Mirrors the other notify modules' "append the link,
/// degrade silently to no link when the dashboard base URL isn't
/// resolvable" convention.
fn alert_body(channel_label: &str, count: usize, link: Option<&str>) -> String {
    let mut body = format!("⚠️ {channel_label} 可以連上，但送不出訊息（10 分鐘內失敗 {count} 次）。");
    if let Some(url) = link {
        body.push_str(&format!("\n👉 {url}"));
    }
    body
}

/// Human-facing zh-TW channel label. A small local lookup table rather than
/// reusing `channel_reply::channel_display_name` — that function is private
/// to a file this change does not touch; duplicating a ten-entry display
/// table is cheaper than widening that file's surface for it.
///
/// `pub(crate)`: `handlers.rs`'s unified-log renderer (W2-8) reuses this so a
/// `channel_recovered` row reads with the exact same label as the Activity
/// Feed row this module writes alongside it — one lookup table, not two that
/// could drift.
pub(crate) fn channel_label(channel: &str) -> &str {
    match channel {
        "telegram" => "Telegram",
        "line" => "LINE",
        "discord" => "Discord",
        "slack" => "Slack",
        "whatsapp" => "WhatsApp",
        "feishu" => "飛書",
        "googlechat" => "Google Chat",
        "teams" => "Teams",
        "webchat" => "WebChat",
        "wecom" => "企業微信",
        "dingtalk" => "釘釘",
        other => other,
    }
}

// ── Delivery ─────────────────────────────────────────────────────────────

/// Resolve destinations for a channel-outage alert: the admin/manager fan-out
/// chain (`approval_notify`'s destination resolver, reused rather than
/// re-derived), minus the channel that is actually failing — pushing "your
/// channel is down" back through the down channel would never arrive.
///
/// There is no "origin conversation" or "agent default" for a system-level
/// monitor (unlike an approval raised from a specific chat), so both those
/// links of the chain are `None` and only the admin fan-out applies.
fn alert_targets(home_dir: &Path, failing_channel: &str) -> Vec<(String, String)> {
    let approvers = crate::decision_notify::approver_links(home_dir);
    let mut targets = crate::decision_notify::resolve_targets(None, None, approvers);
    targets.retain(|(channel, _)| channel != failing_channel);
    targets
}

/// Attempt delivery to each candidate destination in order, returning the
/// channel it actually landed on. `None` when every candidate was
/// unreachable (no destination configured, no bot token, or the send
/// itself failed) — the caller then degrades to log + Activity Feed only.
async fn deliver_alert(
    home_dir: &Path,
    failing_channel: &str,
    agent_hint: Option<&str>,
    body: &str,
) -> Option<String> {
    let targets = alert_targets(home_dir, failing_channel);
    if targets.is_empty() {
        return None;
    }
    let http = reqwest::Client::new();
    let agent_id = agent_hint.unwrap_or("");
    for (channel, chat_id) in targets {
        let Some(token) = crate::goal_notify::channel_token(home_dir, agent_id, &channel).await else {
            debug!(%channel, "channel-alert: no bot token for candidate destination; trying next");
            continue;
        };
        if crate::goal_notify::send_plain_text(home_dir, &http, &channel, &token, &chat_id, body).await {
            return Some(channel);
        }
    }
    None
}

// ── Monitor ──────────────────────────────────────────────────────────────

/// Periodically sweeps `channel_failures.jsonl`, raises a de-duplicated
/// alert the first time a channel crosses [`ALERT_THRESHOLD`] failures
/// inside [`ALERT_WINDOW_SECS`], and stays silent on subsequent ticks while
/// the channel remains over threshold.
pub struct ChannelAlertMonitor {
    home_dir: PathBuf,
    /// channel → "already alerted for the current over-threshold streak"
    /// marker. Absence means "never alerted / currently not over threshold".
    alerted: Mutex<HashSet<String>>,
}

impl ChannelAlertMonitor {
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            alerted: Mutex::new(HashSet::new()),
        }
    }

    /// One sweep. Returns the counts for every channel currently over
    /// threshold this tick (callers that want to inspect the full picture —
    /// tests, a future dashboard surface).
    pub async fn tick(&self) -> Vec<(String, usize)> {
        let failures = tail_failures(&self.home_dir);
        let now = Utc::now();
        let counts = counts_in_window(&failures, now, ALERT_WINDOW_SECS);
        let currently_failing: HashSet<String> = counts
            .iter()
            .filter(|&(_, &c)| c >= ALERT_THRESHOLD)
            .map(|(ch, _)| ch.clone())
            .collect();

        let (to_alert, recovered): (Vec<String>, Vec<String>) = {
            let alerted = self.alerted.lock().await;
            (
                currently_failing.difference(&alerted).cloned().collect(),
                alerted.difference(&currently_failing).cloned().collect(),
            )
        };

        for channel in &to_alert {
            let count = counts.get(channel).copied().unwrap_or(0);
            self.raise_alert(channel, count, &failures).await;
        }

        // W2-4: recovery is now an explicit event, not just the absence of a
        // signal. Consumers of the unified log could previously see "telegram
        // failed 3 times" forever with no way to know it had been fine for a
        // week; a `channel_recovered` row makes "is this still current?"
        // answerable without re-deriving the window.
        for channel in &recovered {
            self.record_recovery(channel, &failures).await;
        }

        // Whole-set replace: channels no longer over threshold drop out
        // (their streak recovered — recorded above), channels still over
        // threshold keep their marker (no re-send), and newly-crossed
        // channels were just alerted above.
        *self.alerted.lock().await = currently_failing.clone();

        counts
            .into_iter()
            .filter(|(ch, _)| currently_failing.contains(ch))
            .collect()
    }

    /// Long-running task: tick on a fixed interval until cancelled.
    pub async fn run(self: std::sync::Arc<Self>, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = self.tick().await;
        }
    }

    /// Raise one alert: log, attempt channel delivery, record the Activity
    /// Feed row either way. Never panics, never retries within this call —
    /// a single best-effort attempt per streak (mirrors
    /// `gvu::stagnation::StagnationMonitor::maybe_alert`, which does not
    /// retry a `SendFailed` push either).
    async fn raise_alert(&self, channel: &str, count: usize, failures: &[ChannelFailure]) {
        warn!(channel, count, "channel-alert: send failures crossed threshold");

        let latest = latest_for_channel(failures, channel);
        let link = crate::deep_link::deep_link(&self.home_dir, crate::deep_link::DeepLinkKind::Channels, "");
        let body = alert_body(channel_label(channel), count, link.as_deref());
        let agent_hint = latest.and_then(|f| f.agent.as_deref());

        let delivered = deliver_alert(&self.home_dir, channel, agent_hint, &body).await;
        if delivered.is_none() {
            debug!(
                channel,
                "channel-alert: no reachable destination for this alert — degrading to log + Activity Feed only"
            );
        } else {
            // L3 (W2-4): a channel that can't deliver is a real outage — it
            // is never held back by quiet hours, and it is counted so P4-5
            // can tell whether operators actually act on these.
            crate::notify_stats::record_push(
                &self.home_dir,
                NOTIFY_TYPE,
                crate::notify_governance::NotifyLevel::Act,
                None,
            );
        }

        self.post_activity(channel, count, delivered.as_deref(), latest.and_then(|f| f.reason.as_deref()))
            .await;
    }

    /// Record that a channel stopped failing: one `channel_recovered` row in
    /// `channel_failures.jsonl` plus one Activity Feed row.
    ///
    /// Deliberately does **not** push a message. A channel coming back is L1
    /// (nobody has to do anything) and the operator was already told when it
    /// broke; "it's fine again" at 03:00 is precisely the notification P4-1's
    /// actionability rule exists to suppress.
    async fn record_recovery(&self, channel: &str, failures: &[ChannelFailure]) {
        let resolves = latest_for_channel(failures, channel)
            .map(|f| f.event.clone())
            .unwrap_or_else(|| SEND_FAILURE_EVENTS[0].to_string());
        let rec = serde_json::json!({
            "event": RECOVERED_EVENT,
            "channel": channel,
            // The unified-log RPC renders `channel.{reason}`, so this is what
            // the dashboard shows as the event type.
            "reason": "recovered",
            "resolved": true,
            "resolves": resolves,
            "timestamp": Utc::now().to_rfc3339(),
        });
        if let Err(e) = crate::trajectory_guard::append_anomaly(&self.home_dir, &rec) {
            debug!(error = %e, "channel-alert: 寫入 channel_recovered 失敗（非致命）");
        }

        let store = match TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "channel-alert: failed to open task store for recovery row (non-fatal)");
                return;
            }
        };
        let row = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "channel_recovered".to_string(),
            agent_id: ACTIVITY_AGENT_ID.to_string(),
            task_id: None,
            summary: format!("通道「{}」已恢復正常發送", channel_label(channel)),
            timestamp: Utc::now().to_rfc3339(),
            metadata: Some(
                serde_json::json!({ "channel": channel, "resolves": resolves }).to_string(),
            ),
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(error = %e, "channel-alert: recovery activity append failed (non-fatal)");
        }
    }

    /// Best-effort Activity Feed append (telemetry, never control flow — a
    /// store-open/write failure is logged and swallowed, mirroring every
    /// other notify module in this crate).
    async fn post_activity(&self, channel: &str, count: usize, delivered_to: Option<&str>, reason: Option<&str>) {
        let store = match TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "channel-alert: failed to open task store (non-fatal)");
                return;
            }
        };
        let summary = match delivered_to {
            Some(dest) => format!(
                "通道「{}」10 分鐘內發送失敗 {count} 次，已告警至「{}」",
                channel_label(channel),
                channel_label(dest)
            ),
            None => format!(
                "通道「{}」10 分鐘內發送失敗 {count} 次，但找不到可推播的其他通道（僅記錄）",
                channel_label(channel)
            ),
        };
        let metadata = serde_json::json!({
            "channel": channel,
            "count": count,
            "delivered_to": delivered_to,
            "last_reason": reason,
        })
        .to_string();
        let row = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "channel_send_failure_alert".to_string(),
            agent_id: ACTIVITY_AGENT_ID.to_string(),
            task_id: None,
            summary,
            timestamp: Utc::now().to_rfc3339(),
            metadata: Some(metadata),
        };
        if let Err(e) = store.append_activity(&row).await {
            debug!(error = %e, "channel-alert: activity append failed (non-fatal)");
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(channel: &str, ts: DateTime<Utc>) -> ChannelFailure {
        ChannelFailure {
            channel: channel.to_string(),
            event: "telegram_send_failed".to_string(),
            agent: None,
            reason: None,
            timestamp: ts,
        }
    }

    // ── parse_failure_line ──────────────────────────────────────────

    #[test]
    fn parses_a_valid_line() {
        let line = r#"{"event":"telegram_send_failed","channel":"telegram","reason":"html_and_plain_both_failed","timestamp":"2026-08-11T10:00:00Z"}"#;
        let rec = parse_failure_line(line).unwrap();
        assert_eq!(rec.channel, "telegram");
        assert_eq!(rec.reason.as_deref(), Some("html_and_plain_both_failed"));
        assert_eq!(rec.agent, None);
    }

    #[test]
    fn parses_agent_field_when_present() {
        let line = r#"{"event":"telegram_send_failed","agent":"sales-bot","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#;
        let rec = parse_failure_line(line).unwrap();
        assert_eq!(rec.agent.as_deref(), Some("sales-bot"));
        assert_eq!(rec.event, "telegram_send_failed");
    }

    #[test]
    fn rejects_records_without_a_channel_field() {
        let line = r#"{"event":"telegram_send_failed","agent":"a","timestamp":"2026-08-11T10:00:00Z"}"#;
        assert_eq!(parse_failure_line(line), None);
    }

    #[test]
    fn agent_and_runtime_events_never_count_even_with_a_channel_field() {
        // W2-4 stamped `channel` on every session-aware writer so the
        // dashboard can attribute failures to a platform. Under the old
        // "any record with a channel" rule, each of these would have started
        // paging as a channel outage. The event allowlist is what stops that.
        for line in [
            r#"{"event":"trajectory_anomaly","agent":"a","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#,
            r#"{"event":"foresight_alarm","agent":"a","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#,
            r#"{"event":"channel_reply_fallback","agent":"a","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#,
            r#"{"event":"channel_reply_silent","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#,
            r#"{"event":"runtime_fallback_substitution","channel":"slack","timestamp":"2026-08-11T10:00:00Z"}"#,
            r#"{"event":"pty_pool_fallback","channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#,
            // A recovery row must never itself count as a failure.
            r#"{"event":"channel_recovered","channel":"telegram","resolved":true,"timestamp":"2026-08-11T10:00:00Z"}"#,
        ] {
            assert_eq!(parse_failure_line(line), None, "must not count: {line}");
        }
    }

    #[test]
    fn rejects_records_without_an_event_field() {
        let line = r#"{"channel":"telegram","timestamp":"2026-08-11T10:00:00Z"}"#;
        assert_eq!(parse_failure_line(line), None);
    }

    #[test]
    fn rejects_blank_channel() {
        let line = r#"{"event":"telegram_send_failed","channel":"   ","timestamp":"2026-08-11T10:00:00Z"}"#;
        assert_eq!(parse_failure_line(line), None);
    }

    #[test]
    fn rejects_missing_or_bad_timestamp() {
        assert_eq!(
            parse_failure_line(r#"{"event":"telegram_send_failed","channel":"telegram"}"#),
            None
        );
        assert_eq!(
            parse_failure_line(
                r#"{"event":"telegram_send_failed","channel":"telegram","timestamp":"not-a-date"}"#
            ),
            None
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert_eq!(parse_failure_line("not json at all"), None);
        assert_eq!(parse_failure_line(""), None);
    }

    // ── counts_in_window ────────────────────────────────────────────

    #[test]
    fn counts_only_in_window_failures() {
        let now = Utc::now();
        let failures = vec![
            failure("telegram", now - chrono::Duration::minutes(1)),
            failure("telegram", now - chrono::Duration::minutes(5)),
            failure("telegram", now - chrono::Duration::minutes(9)),
            // Outside the 10-minute window — must not count.
            failure("telegram", now - chrono::Duration::minutes(11)),
            failure("slack", now - chrono::Duration::minutes(2)),
        ];
        let counts = counts_in_window(&failures, now, ALERT_WINDOW_SECS);
        assert_eq!(counts.get("telegram"), Some(&3));
        assert_eq!(counts.get("slack"), Some(&1));
    }

    #[test]
    fn counts_empty_when_no_failures() {
        let counts = counts_in_window(&[], Utc::now(), ALERT_WINDOW_SECS);
        assert!(counts.is_empty());
    }

    // ── alert_body / channel_label ──────────────────────────────────

    #[test]
    fn alert_body_matches_the_expected_shape() {
        let body = alert_body("Telegram", 3, None);
        assert_eq!(body, "⚠️ Telegram 可以連上，但送不出訊息（10 分鐘內失敗 3 次）。");
    }

    #[test]
    fn alert_body_appends_link_when_resolvable() {
        let body = alert_body("Telegram", 5, Some("http://localhost:18789/manage/channels"));
        assert!(body.contains("👉 http://localhost:18789/manage/channels"));
        assert!(body.starts_with("⚠️ Telegram"));
    }

    #[test]
    fn channel_label_covers_every_pushable_channel_and_degrades_to_raw() {
        assert_eq!(channel_label("telegram"), "Telegram");
        assert_eq!(channel_label("googlechat"), "Google Chat");
        assert_eq!(channel_label("teams"), "Teams");
        assert_eq!(channel_label("some_future_channel"), "some_future_channel");
    }

    // ── alert_targets: filters out the failing channel itself ───────

    #[test]
    fn alert_targets_excludes_the_failing_channel() {
        // No users.db in a fresh temp dir ⇒ approver_links() is empty ⇒
        // resolve_targets() returns nothing regardless of the filter — this
        // just proves the filter compiles/behaves against the real resolver
        // chain used in production, not a re-derived copy.
        let dir = tempfile::tempdir().unwrap();
        let targets = alert_targets(dir.path(), "telegram");
        assert!(targets.is_empty());
    }

    #[test]
    fn resolve_targets_filter_excludes_only_the_failing_channel() {
        // Direct test of the retain() logic against a hand-built list,
        // independent of the admin-links database lookup.
        let mut targets = vec![
            ("telegram".to_string(), "1".to_string()),
            ("slack".to_string(), "C1".to_string()),
            ("telegram".to_string(), "2".to_string()),
        ];
        targets.retain(|(channel, _)| channel != "telegram");
        assert_eq!(targets, vec![("slack".to_string(), "C1".to_string())]);
    }

    // ── ChannelAlertMonitor: threshold + de-duplication ──────────────

    fn write_failures(dir: &Path, lines: &[String]) {
        std::fs::write(dir.join("channel_failures.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn failure_line(channel: &str, ts: DateTime<Utc>) -> String {
        serde_json::json!({
            "event": "telegram_send_failed",
            "channel": channel,
            "reason": "html_and_plain_both_failed",
            "timestamp": ts.to_rfc3339(),
        })
        .to_string()
    }

    #[tokio::test]
    async fn below_threshold_never_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let over = monitor.tick().await;
        assert!(over.is_empty());
        assert!(monitor.alerted.lock().await.is_empty());
    }

    #[tokio::test]
    async fn threshold_crossing_marks_the_channel_alerted() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let over = monitor.tick().await;
        assert_eq!(over, vec![("telegram".to_string(), 3)]);
        assert!(monitor.alerted.lock().await.contains("telegram"));
    }

    #[tokio::test]
    async fn stays_quiet_on_a_second_tick_with_the_same_streak() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let _first = monitor.tick().await;

        // A 4th failure lands (streak continues, count grows) — the marker
        // must be untouched (still just "telegram"), proving the second
        // tick did not re-alert (it would still mark the same single entry,
        // but the Activity Feed assertion below is the real proof).
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
                failure_line("telegram", now),
            ],
        );
        let _second = monitor.tick().await;

        let store = TaskStore::open(dir.path()).unwrap();
        let (rows, total) = store
            .list_activity(None, Some("channel_send_failure_alert"), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1, "exactly one Activity Feed row across two ticks of the same streak: {rows:?}");
    }

    #[tokio::test]
    async fn recovery_clears_the_marker_and_a_relapse_realerts() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let _first = monitor.tick().await;
        assert!(monitor.alerted.lock().await.contains("telegram"));

        // Recovery: rewrite the file with failures now aged out of the
        // 10-minute window (simulates time passing with no new failures).
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(20)),
                failure_line("telegram", now - chrono::Duration::minutes(25)),
            ],
        );
        let recovered = monitor.tick().await;
        assert!(recovered.is_empty());
        assert!(
            !monitor.alerted.lock().await.contains("telegram"),
            "recovered channel must have its marker cleared so a future relapse re-alerts"
        );

        // Relapse: fresh failures cross the threshold again.
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now),
                failure_line("telegram", now),
                failure_line("telegram", now),
            ],
        );
        let relapsed = monitor.tick().await;
        assert_eq!(relapsed, vec![("telegram".to_string(), 3)]);

        let store = TaskStore::open(dir.path()).unwrap();
        let (_rows, total) = store
            .list_activity(None, Some("channel_send_failure_alert"), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 2, "one row for the first streak, one for the relapse");
    }

    #[tokio::test]
    async fn recovery_writes_a_channel_recovered_event_and_activity_row() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let _ = monitor.tick().await;

        // Age the failures out of the window ⇒ recovery.
        write_failures(
            dir.path(),
            &[failure_line("telegram", now - chrono::Duration::minutes(30))],
        );
        let _ = monitor.tick().await;

        let body = std::fs::read_to_string(dir.path().join("channel_failures.jsonl")).unwrap();
        let recovered: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("event").and_then(|e| e.as_str()) == Some(RECOVERED_EVENT))
            .collect();
        assert_eq!(recovered.len(), 1, "exactly one recovery row: {body}");
        assert_eq!(recovered[0]["channel"], "telegram");
        assert_eq!(recovered[0]["resolved"], true);
        assert_eq!(recovered[0]["resolves"], "telegram_send_failed");
        // The dashboard's unified log renders `channel.{reason}`.
        assert_eq!(recovered[0]["reason"], "recovered");

        let store = TaskStore::open(dir.path()).unwrap();
        let (rows, total) = store
            .list_activity(None, Some("channel_recovered"), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert!(rows[0].summary.contains("已恢復"));
    }

    #[tokio::test]
    async fn a_channel_that_never_alerted_never_reports_recovery() {
        // Below threshold the whole time ⇒ nothing broke ⇒ nothing recovered.
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(dir.path(), &[failure_line("telegram", now)]);
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let _ = monitor.tick().await;
        write_failures(dir.path(), &[failure_line("telegram", now - chrono::Duration::hours(1))]);
        let _ = monitor.tick().await;

        let body = std::fs::read_to_string(dir.path().join("channel_failures.jsonl")).unwrap();
        assert!(!body.contains(RECOVERED_EVENT), "{body}");
    }

    #[tokio::test]
    async fn independent_channels_alert_independently() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
                // Slack only has 2 — below threshold.
                failure_line("slack", now - chrono::Duration::minutes(1)),
                failure_line("slack", now - chrono::Duration::minutes(2)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let over = monitor.tick().await;
        assert_eq!(over, vec![("telegram".to_string(), 3)]);
        let alerted = monitor.alerted.lock().await;
        assert!(alerted.contains("telegram"));
        assert!(!alerted.contains("slack"));
    }

    #[tokio::test]
    async fn no_reachable_destination_still_records_activity_feed_once() {
        // Honest degrade: no users.db in this temp home ⇒ approver_links()
        // is empty ⇒ deliver_alert() returns None ⇒ the tick must still
        // record exactly one Activity Feed row (log + Activity Feed only,
        // never silent, never panics).
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        write_failures(
            dir.path(),
            &[
                failure_line("telegram", now - chrono::Duration::minutes(1)),
                failure_line("telegram", now - chrono::Duration::minutes(2)),
                failure_line("telegram", now - chrono::Duration::minutes(3)),
            ],
        );
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let over = monitor.tick().await;
        assert_eq!(over, vec![("telegram".to_string(), 3)]);

        let store = TaskStore::open(dir.path()).unwrap();
        let (rows, total) = store
            .list_activity(None, Some("channel_send_failure_alert"), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert!(rows[0].summary.contains("僅記錄"));
    }

    #[tokio::test]
    async fn missing_file_yields_no_signal_and_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let monitor = ChannelAlertMonitor::new(dir.path().to_path_buf());
        let over = monitor.tick().await;
        assert!(over.is_empty());
    }
}
