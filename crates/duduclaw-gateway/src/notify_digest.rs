//! Daily digest (W2-4, C8 / C1).
//!
//! GitHub's scheduled reminders and Zapier's "instant + hourly summary" tier
//! are the model: **two** notification channels, not one — event-driven for
//! things that cannot wait, and a scheduled roll-up for everything that can.
//! `03-analogous-products.md` C8 records that DuDuClaw had only the first
//! half. This is the second.
//!
//! One message per day, at a configurable local time, containing:
//! yesterday's completed tasks, the number of decisions still waiting on a
//! person, learning events (evolution / stagnation), spend, and channel
//! health.
//!
//! ## "無事不寄" is a hard rule (C1)
//!
//! [`DigestStats::is_empty`] gates the send. A day where nothing finished,
//! nothing is pending, nothing was learned, nothing was spent, and no channel
//! misbehaved produces **no message at all** — not a message saying "no
//! activity". This is the same posture `opus-playbook` §6 takes for patrol
//! tasks, and the reason it matters is P4-5: a digest nobody ever needs to
//! read trains people to ignore digests they do.
//!
//! ## Ceiling
//!
//! Exactly one digest per local calendar day, enforced by a state file
//! (`<home>/notify_digest_state.json`) rather than an in-memory flag, so a
//! gateway restart at 09:05 does not produce a second one. The scheduler ticks
//! every minute and fires on the first tick at or after the configured time;
//! a gateway that was down at 09:00 and comes up at 11:00 still gets that
//! day's digest, once.
//!
//! ## Configuration
//!
//! ```toml
//! [notify]
//! daily_digest = false      # default OFF — opt-in, per the rollout plan
//! daily_digest_at = "09:00" # local time, host timezone
//! quiet_hours = ""          # deployment-wide fallback (see notify_governance)
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::notify_governance::NotifyLevel;

/// State file recording the last local date a digest went out.
const STATE_FILE: &str = "notify_digest_state.json";

/// Activity rows scanned per digest. Bounded: a digest is a summary, and an
/// install generating more than this in a day has a different problem.
const ACTIVITY_SCAN_LIMIT: i64 = 2000;

/// Event-type prefixes that count as "learning events".
///
/// A prefix list rather than an enumeration of exact names: the evolution
/// subsystem adds event types regularly (`gvu_observation_confirmed`,
/// `playbook_evolved`, …) and an exact allowlist would silently under-count
/// from the first one that was added without updating this file.
const LEARNING_PREFIXES: &[&str] = &["gvu_", "playbook_", "soul_", "evolution_"];

/// The Activity Feed event the channel-outage monitor writes.
const CHANNEL_ALERT_EVENT: &str = "channel_send_failure_alert";

/// W3-2 (D.14 「試行結果通知(採用/回退)」, L1 = digest, never its own push).
///
/// 「學習事件 N 則」 alone answers "did anything happen" but not "did it go
/// well" — the single number reads identically whether every trial was adopted
/// or every one was rolled back. These three sets break that number down into
/// the outcome the reader actually cares about. Exact names, not prefixes:
/// unlike [`LEARNING_PREFIXES`] (which must stay permissive so a new event type
/// is still *counted*), mis-classifying an outcome would report a rollback as
/// an adoption, so an unknown event stays in the undifferentiated remainder.
const RULE_ADOPTED_EVENTS: &[&str] = &["gvu_observation_confirmed", "playbook_rules_updated"];
/// Trials that were reverted after the observation window measured a regression.
const RULE_REVERTED_EVENTS: &[&str] = &["gvu_observation_rolled_back"];
/// Trials that expired without enough evidence to judge either way (§C.9
/// 「證據不足,未採用」) — deliberately NOT counted as a rollback: nothing was
/// shown to be wrong, there just was not enough traffic to tell.
const RULE_NO_EVIDENCE_EVENTS: &[&str] = &["gvu_observation_expired_no_data"];

// ── Config ───────────────────────────────────────────────────────────────

/// `config.toml [notify]`, digest half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestConfig {
    pub enabled: bool,
    /// Local wall-clock time to send at.
    pub at: NaiveTime,
}

impl Default for DigestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // 09:00 — after a typical quiet window ends, before the workday
            // has generated the next batch of noise.
            at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        }
    }
}

impl DigestConfig {
    /// Load `[notify]` from `<home>/config.toml`. Absent / malformed file or
    /// section ⇒ [`Self::default`] (feature off) — the `tick_config` idiom.
    pub fn from_home(home_dir: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        Self::from_toml_str(&content)
    }

    /// Parse a whole `config.toml` body. Public for tests.
    pub fn from_toml_str(content: &str) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        let Some(section) = table.get("notify").and_then(|v| v.as_table()) else {
            return Self::default();
        };
        let mut cfg = Self {
            enabled: section
                .get("daily_digest")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ..Self::default()
        };
        if let Some(raw) = section.get("daily_digest_at").and_then(|v| v.as_str()) {
            match parse_clock(raw) {
                Some(t) => cfg.at = t,
                None => warn!(
                    value = %raw,
                    "notify-digest: daily_digest_at 格式無法解析（需 \"HH:MM\"），改用預設 09:00"
                ),
            }
        }
        cfg
    }
}

/// Parse `H:MM` / `HH:MM`. Shares the shape of the quiet-hours parser but
/// stays local — one field, one meaning.
///
/// `pub(crate)`: `handlers.rs`'s `system.update_config` (W2-8 dashboard
/// toggle) reuses this to validate `daily_digest_at` at write time, so a
/// malformed value is rejected immediately instead of silently falling back
/// to 09:00 the way [`DigestConfig::from_toml_str`]'s fail-open READ path
/// does.
pub(crate) fn parse_clock(raw: &str) -> Option<NaiveTime> {
    let (h, m) = raw.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

// ── Stats ────────────────────────────────────────────────────────────────

/// What one digest reports on. All counts are for the window `[since, now]`
/// except `pending_decisions`, which is a point-in-time backlog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DigestStats {
    /// Tasks that reached `done` in the window.
    pub tasks_done: usize,
    /// Decisions waiting on a person right now (approvals + install requests
    /// + goal tasks parked `needs_human`).
    pub pending_decisions: usize,
    /// Evolution / learning Activity Feed rows in the window.
    pub learning_events: usize,
    /// W3-2 breakdown of `learning_events`: experience rules whose trial ended
    /// in adoption. Always `<= learning_events` (a strict subset of the same
    /// rows), which is why none of the three participate in [`Self::is_empty`]
    /// — `learning_events` already covers them and double-counting would let a
    /// day be "non-empty" twice for one event.
    pub rules_adopted: usize,
    /// Trials reverted after measuring a regression.
    pub rules_reverted: usize,
    /// Trials that expired without enough evidence to judge (§C.9).
    pub rules_no_evidence: usize,
    /// Spend over the window, in millicents.
    pub cost_millicents: u64,
    /// Channel-outage alerts raised in the window.
    pub channel_alerts: usize,
    /// Notification types the SRE 50% rule flagged as broken (P4-5). Not part
    /// of the emptiness test — a broken-notification finding on an otherwise
    /// silent day is still not worth a message.
    pub broken_notify_types: Vec<String>,
}

impl DigestStats {
    /// C1: nothing to say ⇒ say nothing.
    pub fn is_empty(&self) -> bool {
        self.tasks_done == 0
            && self.pending_decisions == 0
            && self.learning_events == 0
            && self.cost_millicents == 0
            && self.channel_alerts == 0
    }
}

/// True when an Activity Feed event type counts as a learning event.
pub fn is_learning_event(event_type: &str) -> bool {
    LEARNING_PREFIXES.iter().any(|p| event_type.starts_with(p))
}

/// Render the zh-TW digest body. `None` when there is nothing worth sending
/// (C1) — the caller must treat `None` as "stay silent", never as an error.
///
/// Pure — unit-tested directly.
pub fn render(stats: &DigestStats, link: Option<&str>) -> Option<String> {
    if stats.is_empty() {
        return None;
    }
    let mut lines = vec!["📊 昨日摘要".to_string()];
    if stats.tasks_done > 0 {
        lines.push(format!("・完成任務 {} 件", stats.tasks_done));
    }
    if stats.pending_decisions > 0 {
        lines.push(format!("・待你決定 {} 件", stats.pending_decisions));
    }
    if stats.learning_events > 0 {
        lines.push(format!("・學習事件 {} 則", stats.learning_events));
        if let Some(breakdown) = render_rule_trials(stats) {
            lines.push(format!("  ↳ 經驗法則試行結果：{breakdown}"));
        }
    }
    if stats.cost_millicents > 0 {
        lines.push(format!("・花費 {}", format_millicents(stats.cost_millicents)));
    }
    if stats.channel_alerts > 0 {
        lines.push(format!("・通道異常告警 {} 次", stats.channel_alerts));
    }
    if !stats.broken_notify_types.is_empty() {
        lines.push(format!(
            "・行動率偏低的通知類別：{}（建議降級或關閉）",
            stats.broken_notify_types.join("、")
        ));
    }
    if let Some(url) = link {
        lines.push(format!("👉 {url}"));
    }
    Some(lines.join("\n"))
}

/// W3-2 — the 「採用 X 條、回退 Y 條、證據不足 Z 條」 sub-line, or `None` when
/// no trial settled in the window (a learning-events day made entirely of
/// other evolution activity says nothing about trials, and inventing a
/// 「採用 0 條」 line would read as a failure report).
///
/// Uses only the §C.9 outward vocabulary — 「採用」/「回退」/「證據不足」 —
/// never the internal state names.
fn render_rule_trials(stats: &DigestStats) -> Option<String> {
    let mut parts = Vec::new();
    if stats.rules_adopted > 0 {
        parts.push(format!("採用 {} 條", stats.rules_adopted));
    }
    if stats.rules_reverted > 0 {
        parts.push(format!("回退 {} 條", stats.rules_reverted));
    }
    if stats.rules_no_evidence > 0 {
        parts.push(format!("證據不足 {} 條", stats.rules_no_evidence));
    }
    (!parts.is_empty()).then(|| parts.join("、"))
}

/// Millicents → a human amount. 100_000 millicents = US$1.00.
fn format_millicents(mc: u64) -> String {
    let dollars = mc as f64 / 100_000.0;
    if dollars < 0.01 {
        "< US$0.01".to_string()
    } else {
        format!("US${dollars:.2}")
    }
}

// ── Collection ───────────────────────────────────────────────────────────

/// Gather everything a digest reports on. Every source is best-effort: a
/// store that cannot be opened contributes zero and logs, rather than
/// aborting the digest — a partial digest beats no digest.
pub async fn collect(home_dir: &Path, since: DateTime<Utc>, now: DateTime<Utc>) -> DigestStats {
    let mut stats = DigestStats::default();

    if let Ok(store) = crate::task_store::TaskStore::open(home_dir) {
        match store.list_tasks(Some("done"), None, None).await {
            Ok(tasks) => {
                stats.tasks_done = tasks
                    .iter()
                    .filter(|t| in_window(t.completed_at.as_deref(), since, now))
                    .count();
            }
            Err(e) => debug!(error = %e, "notify-digest: 讀取已完成任務失敗（略過此項）"),
        }
        if let Ok(parked) = store.tasks_in_status("needs_human").await {
            stats.pending_decisions += parked.len();
        }
        match store.list_activity(None, None, ACTIVITY_SCAN_LIMIT, 0).await {
            Ok((rows, _)) => {
                for r in &rows {
                    if !in_window(Some(&r.timestamp), since, now) {
                        continue;
                    }
                    if is_learning_event(&r.event_type) {
                        stats.learning_events += 1;
                        let ev = r.event_type.as_str();
                        if RULE_ADOPTED_EVENTS.contains(&ev) {
                            stats.rules_adopted += 1;
                        } else if RULE_REVERTED_EVENTS.contains(&ev) {
                            stats.rules_reverted += 1;
                        } else if RULE_NO_EVIDENCE_EVENTS.contains(&ev) {
                            stats.rules_no_evidence += 1;
                        }
                    } else if r.event_type == CHANNEL_ALERT_EVENT {
                        stats.channel_alerts += 1;
                    }
                }
            }
            Err(e) => debug!(error = %e, "notify-digest: 讀取活動紀錄失敗（略過此項）"),
        }
    }

    if let Ok(broker) = crate::approval::ApprovalBroker::open(home_dir) {
        match broker.list_pending(None).await {
            Ok(rows) => stats.pending_decisions += rows.len(),
            Err(e) => debug!(error = %e, "notify-digest: 讀取待審批失敗（略過此項）"),
        }
    }

    if let Ok(store) = crate::install_requests::InstallRequestStore::open(home_dir) {
        // `list_pending` does not expire on its own (unlike ApprovalBroker),
        // so sweep first or the count includes rows nobody can act on.
        let _ = store.expire_stale().await;
        match store.list_pending().await {
            Ok(rows) => stats.pending_decisions += rows.len(),
            Err(e) => debug!(error = %e, "notify-digest: 讀取安裝申請失敗（略過此項）"),
        }
    }

    if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
        let hours = ((now - since).num_hours().max(1)) as u64;
        match telemetry.summary_global(hours).await {
            Ok(sum) => stats.cost_millicents = sum.total_cost_millicents,
            Err(e) => debug!(error = %e, "notify-digest: 讀取花費統計失敗（略過此項）"),
        }
    }

    stats.broken_notify_types = crate::notify_stats::broken_types(home_dir, 30);
    stats
}

/// RFC3339 timestamp inside `[since, now]`?
fn in_window(ts: Option<&str>, since: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let Some(raw) = ts else { return false };
    let Ok(parsed) = DateTime::parse_from_rfc3339(raw) else {
        return false;
    };
    let t = parsed.with_timezone(&Utc);
    t >= since && t <= now
}

// ── Once-per-day state ───────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct DigestState {
    /// Last local date (`YYYY-MM-DD`) a digest was sent.
    #[serde(default)]
    last_sent_date: String,
}

fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STATE_FILE)
}

/// Should a digest go out now? Pure — the whole scheduling rule in one
/// testable function.
///
/// - Not yet at the configured time today ⇒ no.
/// - Already sent today ⇒ no (the hard "one per day" ceiling).
/// - Otherwise ⇒ yes, including catching up a gateway that was down at the
///   configured time.
pub fn should_send(at: NaiveTime, last_sent_date: &str, local_now: chrono::NaiveDateTime) -> bool {
    if local_now.time() < at {
        return false;
    }
    let today = format!(
        "{:04}-{:02}-{:02}",
        local_now.year(),
        local_now.month(),
        local_now.day()
    );
    last_sent_date != today
}

/// Compare-and-set the last-sent date under the advisory lock. Returns `true`
/// only when this call is the one that claimed today — two gateway processes
/// racing produce exactly one digest.
fn claim_today(home_dir: &Path, today: &str) -> bool {
    let path = state_path(home_dir);
    let result = duduclaw_core::with_file_lock(&path, || {
        let current: DigestState = std::fs::read_to_string(&path)
            .ok()
            .and_then(|b| serde_json::from_str(&b).ok())
            .unwrap_or_default();
        if current.last_sent_date == today {
            return Ok(false);
        }
        let next = DigestState {
            last_sent_date: today.to_string(),
        };
        let body = serde_json::to_string_pretty(&next).unwrap_or_default();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, &path)?;
        Ok(true)
    });
    match result {
        Ok(claimed) => claimed,
        Err(e) => {
            // Fail-closed for a *notification*: an unwritable state file must
            // not license one digest per tick.
            warn!(error = %e, "notify-digest: 無法更新每日摘要狀態檔，本次不發送");
            false
        }
    }
}

// ── Scheduler ────────────────────────────────────────────────────────────

/// Fires the daily digest. Ticks on a short interval and does nothing at all
/// unless `[notify] daily_digest = true`.
pub struct DailyDigestScheduler {
    home_dir: PathBuf,
}

impl DailyDigestScheduler {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    /// One evaluation. Returns the body that was sent, for tests and logging.
    pub async fn tick(&self) -> Option<String> {
        let cfg = DigestConfig::from_home(&self.home_dir);
        if !cfg.enabled {
            return None;
        }
        let now = Utc::now();
        // The digest is a human-calendar thing; read it on the host clock,
        // matching `[notify] quiet_hours`'s system-timezone default.
        let local_now = now.with_timezone(&chrono::Local).naive_local();
        let today = format!(
            "{:04}-{:02}-{:02}",
            local_now.year(),
            local_now.month(),
            local_now.day()
        );

        let last = read_last_sent(&self.home_dir);
        if !should_send(cfg.at, &last, local_now) {
            return None;
        }

        let since = now - chrono::Duration::hours(24);
        let stats = collect(&self.home_dir, since, now).await;
        if stats.is_empty() {
            // C1: silence IS the report. Claim the day anyway so a quiet
            // morning does not re-evaluate every minute until noon.
            let _ = claim_today(&self.home_dir, &today);
            debug!("notify-digest: 昨日無事，依「無事不寄」原則不發送");
            return None;
        }

        // Claim before sending: a send that fails must not license a retry
        // every minute for the rest of the day.
        if !claim_today(&self.home_dir, &today) {
            return None;
        }

        let Some(agent_id) = read_default_agent(&self.home_dir).await else {
            info!("notify-digest: 未設定 [general] default_agent，摘要無處可送");
            return None;
        };
        // The digest's landing spot is the HITL inbox: the pending-decision
        // count is the only line in it a person can act on.
        let link = crate::deep_link::deep_link(
            &self.home_dir,
            crate::deep_link::DeepLinkKind::Approval,
            "",
        );
        let body = render(&stats, link.as_deref())?;

        let outcome = crate::goal_notify::notify_agent_plain(
            &self.home_dir,
            &agent_id,
            NotifyLevel::Fyi,
            "digest.daily",
            &body,
        )
        .await;
        match outcome {
            crate::goal_notify::NotifyOutcome::Sent
            | crate::goal_notify::NotifyOutcome::Deferred => {
                info!(agent = %agent_id, "notify-digest: 已送出每日摘要")
            }
            crate::goal_notify::NotifyOutcome::NoTarget => {
                info!(agent = %agent_id, "notify-digest: 代理未設定 [proactive] 推播目標，摘要僅記錄")
            }
            crate::goal_notify::NotifyOutcome::SendFailed => {
                warn!(agent = %agent_id, "notify-digest: 每日摘要送出失敗（今日不再重試）")
            }
        }
        Some(body)
    }

    /// Long-running task: evaluate every `interval` until cancelled.
    pub async fn run(self: std::sync::Arc<Self>, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = self.tick().await;
        }
    }
}

fn read_last_sent(home_dir: &Path) -> String {
    std::fs::read_to_string(state_path(home_dir))
        .ok()
        .and_then(|b| serde_json::from_str::<DigestState>(&b).ok())
        .map(|s| s.last_sent_date)
        .unwrap_or_default()
}

/// `config.toml [general] default_agent`.
async fn read_default_agent(home_dir: &Path) -> Option<String> {
    let content = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .ok()?;
    let table: toml::Table = content.parse().ok()?;
    let v = table
        .get("general")
        .and_then(|g| g.as_table())?
        .get("default_agent")
        .and_then(|v| v.as_str())?
        .trim();
    (!v.is_empty()).then(|| v.to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn local(y: i32, m: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    // ── config ───────────────────────────────────────────────────

    #[test]
    fn digest_is_off_by_default() {
        let cfg = DigestConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.at, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
    }

    #[test]
    fn missing_file_or_section_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(DigestConfig::from_home(dir.path()), DigestConfig::default());
        assert_eq!(DigestConfig::from_toml_str("[general]\nname = \"x\"\n"), DigestConfig::default());
        assert_eq!(DigestConfig::from_toml_str("not [ toml"), DigestConfig::default());
    }

    #[test]
    fn config_reads_enable_and_time() {
        let cfg = DigestConfig::from_toml_str(
            "[notify]\ndaily_digest = true\ndaily_digest_at = \"07:30\"\n",
        );
        assert!(cfg.enabled);
        assert_eq!(cfg.at, NaiveTime::from_hms_opt(7, 30, 0).unwrap());
    }

    #[test]
    fn a_malformed_time_falls_back_to_the_default_rather_than_disabling() {
        let cfg = DigestConfig::from_toml_str("[notify]\ndaily_digest = true\ndaily_digest_at = \"早上\"\n");
        assert!(cfg.enabled, "a bad time must not silently disable the feature");
        assert_eq!(cfg.at, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
    }

    // ── emptiness (C1) ───────────────────────────────────────────

    #[test]
    fn an_all_zero_day_renders_nothing() {
        let stats = DigestStats::default();
        assert!(stats.is_empty());
        assert_eq!(render(&stats, Some("http://x/tasks")), None);
    }

    #[test]
    fn a_broken_notify_finding_alone_is_still_silence() {
        // Otherwise a permanently-noisy type would generate a daily message
        // about being noisy — a self-reinforcing loop.
        let stats = DigestStats {
            broken_notify_types: vec!["decision.install".into()],
            ..Default::default()
        };
        assert!(stats.is_empty());
        assert_eq!(render(&stats, None), None);
    }

    #[test]
    fn any_single_non_zero_counter_makes_it_worth_sending() {
        for stats in [
            DigestStats { tasks_done: 1, ..Default::default() },
            DigestStats { pending_decisions: 1, ..Default::default() },
            DigestStats { learning_events: 1, ..Default::default() },
            DigestStats { cost_millicents: 1, ..Default::default() },
            DigestStats { channel_alerts: 1, ..Default::default() },
        ] {
            assert!(!stats.is_empty());
            assert!(render(&stats, None).is_some());
        }
    }

    // ── rendering ────────────────────────────────────────────────

    #[test]
    fn render_lists_only_the_non_zero_lines() {
        let stats = DigestStats {
            tasks_done: 3,
            pending_decisions: 0,
            learning_events: 2,
            cost_millicents: 0,
            channel_alerts: 0,
            broken_notify_types: vec![],
            ..Default::default()
        };
        let body = render(&stats, None).unwrap();
        assert!(body.starts_with("📊 昨日摘要"));
        assert!(body.contains("完成任務 3 件"));
        assert!(body.contains("學習事件 2 則"));
        assert!(
            !body.contains("試行結果"),
            "no trial settled ⇒ no breakdown line: {body}"
        );
        assert!(!body.contains("待你決定"), "zero counters must not appear: {body}");
        assert!(!body.contains("花費"));
    }

    #[test]
    fn render_appends_the_deep_link_when_there_is_one() {
        let stats = DigestStats { tasks_done: 1, ..Default::default() };
        let body = render(&stats, Some("http://localhost:18789/tasks")).unwrap();
        assert!(body.ends_with("👉 http://localhost:18789/tasks"));
        // No link ⇒ text reads exactly as it would without the feature.
        assert!(!render(&stats, None).unwrap().contains("👉"));
    }

    #[test]
    fn render_breaks_learning_events_down_into_trial_outcomes() {
        let stats = DigestStats {
            learning_events: 5,
            rules_adopted: 2,
            rules_reverted: 1,
            rules_no_evidence: 1,
            ..Default::default()
        };
        let body = render(&stats, None).unwrap();
        assert!(body.contains("學習事件 5 則"), "{body}");
        assert!(body.contains("經驗法則試行結果：採用 2 條、回退 1 條、證據不足 1 條"), "{body}");
        // §C.9 wording only — no internal artifact names in a user-facing push.
        for internal in ["playbook", "shadow", "probation", "GVU", "SOUL"] {
            assert!(!body.contains(internal), "leaked `{internal}`: {body}");
        }
    }

    #[test]
    fn trial_breakdown_omits_the_outcomes_that_did_not_happen() {
        let stats = DigestStats { learning_events: 3, rules_adopted: 1, ..Default::default() };
        let body = render(&stats, None).unwrap();
        assert!(body.contains("試行結果：採用 1 條"), "{body}");
        assert!(!body.contains("回退"), "a zero must not read as a failure report: {body}");
        assert!(!body.contains("證據不足"), "{body}");
    }

    #[test]
    fn trial_counters_alone_never_make_an_otherwise_empty_day_worth_sending() {
        // They are a strict subset of `learning_events`; counting them again
        // in `is_empty` would let one event make the day non-empty twice.
        let stats = DigestStats { rules_adopted: 2, ..Default::default() };
        assert!(stats.is_empty(), "{stats:?}");
        assert_eq!(render(&stats, None), None);
    }

    #[test]
    fn render_names_broken_notification_types_when_there_is_other_news() {
        let stats = DigestStats {
            tasks_done: 1,
            broken_notify_types: vec!["decision.install".into(), "budget.breaker".into()],
            ..Default::default()
        };
        let body = render(&stats, None).unwrap();
        assert!(body.contains("decision.install、budget.breaker"));
        assert!(body.contains("建議降級或關閉"));
    }

    #[test]
    fn cost_is_formatted_as_money_not_millicents() {
        assert_eq!(format_millicents(100_000), "US$1.00");
        assert_eq!(format_millicents(250_000), "US$2.50");
        assert_eq!(format_millicents(500), "< US$0.01");
        let stats = DigestStats { cost_millicents: 123_456, ..Default::default() };
        assert!(render(&stats, None).unwrap().contains("US$1.23"));
    }

    // ── learning-event classification ────────────────────────────

    #[test]
    fn learning_events_are_matched_by_subsystem_prefix() {
        for t in [
            "gvu_stagnation_detected",
            "gvu_consolidated",
            "gvu_observation_confirmed",
            "playbook_evolved",
            "soul_version_confirmed",
            "evolution_advanced",
        ] {
            assert!(is_learning_event(t), "{t} should count as a learning event");
        }
        for t in ["task_created", "autopilot_triggered", CHANNEL_ALERT_EVENT, "", "os_file"] {
            assert!(!is_learning_event(t), "{t} should NOT count as a learning event");
        }
    }

    // ── the one-per-day rule ─────────────────────────────────────

    #[test]
    fn nothing_fires_before_the_configured_time() {
        let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert!(!should_send(at, "", local(2026, 8, 11, 8, 59)));
        assert!(should_send(at, "", local(2026, 8, 11, 9, 0)));
    }

    #[test]
    fn only_one_digest_per_local_day() {
        let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert!(!should_send(at, "2026-08-11", local(2026, 8, 11, 9, 0)));
        assert!(!should_send(at, "2026-08-11", local(2026, 8, 11, 23, 59)));
        // Next day it fires again.
        assert!(should_send(at, "2026-08-11", local(2026, 8, 12, 9, 0)));
    }

    #[test]
    fn a_gateway_that_was_down_at_nine_still_gets_its_digest() {
        let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert!(should_send(at, "2026-08-10", local(2026, 8, 11, 14, 30)));
    }

    #[test]
    fn claim_today_is_a_compare_and_set() {
        let dir = tempfile::tempdir().unwrap();
        assert!(claim_today(dir.path(), "2026-08-11"));
        assert!(!claim_today(dir.path(), "2026-08-11"), "a second claim must lose");
        assert!(claim_today(dir.path(), "2026-08-12"));
        assert_eq!(read_last_sent(dir.path()), "2026-08-12");
    }

    #[test]
    fn a_fresh_home_has_no_last_sent_date() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_last_sent(dir.path()), "");
    }

    // ── scheduler ────────────────────────────────────────────────

    #[tokio::test]
    async fn scheduler_does_nothing_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[notify]\ndaily_digest = false\n").unwrap();
        let sched = DailyDigestScheduler::new(dir.path().to_path_buf());
        assert_eq!(sched.tick().await, None);
        // Disabled must not even claim the day.
        assert_eq!(read_last_sent(dir.path()), "");
    }

    #[tokio::test]
    async fn an_empty_day_claims_the_day_but_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[notify]\ndaily_digest = true\ndaily_digest_at = \"00:00\"\n",
        )
        .unwrap();
        let sched = DailyDigestScheduler::new(dir.path().to_path_buf());
        assert_eq!(sched.tick().await, None, "nothing happened ⇒ nothing sent");
        assert_ne!(read_last_sent(dir.path()), "", "the day is claimed so it stops re-evaluating");
    }

    #[tokio::test]
    async fn collect_on_an_empty_home_is_all_zeroes_and_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let stats = collect(dir.path(), now - chrono::Duration::hours(24), now).await;
        assert!(stats.is_empty(), "{stats:?}");
    }

    #[tokio::test]
    async fn collect_classifies_rule_trial_outcomes_inside_the_learning_count() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let store = crate::task_store::TaskStore::open(dir.path()).unwrap();

        // One of each outcome, plus an undifferentiated learning event and a
        // stale row outside the window.
        for (i, (ev, ts)) in [
            ("gvu_observation_confirmed", now - chrono::Duration::hours(1)),
            ("playbook_rules_updated", now - chrono::Duration::hours(2)),
            ("gvu_observation_rolled_back", now - chrono::Duration::hours(3)),
            ("gvu_observation_expired_no_data", now - chrono::Duration::hours(4)),
            ("gvu_consolidated", now - chrono::Duration::hours(5)),
            ("gvu_observation_confirmed", now - chrono::Duration::days(3)),
        ]
        .iter()
        .enumerate()
        {
            store
                .append_activity(&crate::task_store::ActivityRow {
                    id: format!("trial-{i}"),
                    event_type: (*ev).into(),
                    agent_id: "kiki".into(),
                    task_id: None,
                    summary: "x".into(),
                    timestamp: ts.to_rfc3339(),
                    metadata: None,
                })
                .await
                .unwrap();
        }

        let stats = collect(dir.path(), now - chrono::Duration::hours(24), now).await;
        assert_eq!(stats.learning_events, 5, "the 3-day-old row is out of window");
        assert_eq!(stats.rules_adopted, 2, "confirmed observation + committed rules");
        assert_eq!(stats.rules_reverted, 1);
        assert_eq!(stats.rules_no_evidence, 1);
        // The breakdown never exceeds the total it decomposes.
        assert!(
            stats.rules_adopted + stats.rules_reverted + stats.rules_no_evidence
                <= stats.learning_events
        );
        let body = render(&stats, None).unwrap();
        assert!(body.contains("採用 2 條、回退 1 條、證據不足 1 條"), "{body}");
    }

    #[tokio::test]
    async fn collect_counts_completed_tasks_and_learning_events_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let store = crate::task_store::TaskStore::open(dir.path()).unwrap();

        store
            .append_activity(&crate::task_store::ActivityRow {
                id: "act-1".into(),
                event_type: "gvu_consolidated".into(),
                agent_id: "kiki".into(),
                task_id: None,
                summary: "整併".into(),
                timestamp: (now - chrono::Duration::hours(2)).to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();
        // Outside the window — must not count.
        store
            .append_activity(&crate::task_store::ActivityRow {
                id: "act-2".into(),
                event_type: "gvu_consolidated".into(),
                agent_id: "kiki".into(),
                task_id: None,
                summary: "很久以前".into(),
                timestamp: (now - chrono::Duration::days(3)).to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();
        store
            .append_activity(&crate::task_store::ActivityRow {
                id: "act-3".into(),
                event_type: CHANNEL_ALERT_EVENT.into(),
                agent_id: "channel_alert".into(),
                task_id: None,
                summary: "通道失敗".into(),
                timestamp: (now - chrono::Duration::hours(1)).to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();

        let stats = collect(dir.path(), now - chrono::Duration::hours(24), now).await;
        assert_eq!(stats.learning_events, 1);
        assert_eq!(stats.channel_alerts, 1);
        assert!(!stats.is_empty());
        assert!(render(&stats, None).is_some());
    }

    #[test]
    fn in_window_rejects_missing_and_unparseable_timestamps() {
        let now = Utc::now();
        let since = now - chrono::Duration::hours(24);
        assert!(!in_window(None, since, now));
        assert!(!in_window(Some("not-a-date"), since, now));
        assert!(in_window(Some(&(now - chrono::Duration::hours(1)).to_rfc3339()), since, now));
        assert!(!in_window(Some(&(now - chrono::Duration::days(2)).to_rfc3339()), since, now));
    }
}
