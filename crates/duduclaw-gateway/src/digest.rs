//! P9 (2026-10-08) — "while you were away" daily digest and feedback on
//! finished work.
//!
//! **Digest.** Once a day, at `[digest] hour` in `[digest] timezone`, the
//! gateway that holds the instance lock (`duduclaw_core::gateway_instance`)
//! assembles a per-employee summary from data it already has — no model
//! call: tasks finished since the last digest (goals included), Activity
//! Feed rows, pending approvals, settled responsibility runs and spend
//! (`cost_telemetry.db`). It is saved as `<home>/digest/<date>.json` (what
//! the dashboard home shows, RPC `digest.latest`) and sent as plain text to
//! every active Admin account's verified linked channels. Off by default
//! (`config.toml [digest] enabled = false`); `exclude_agents` opts employees
//! out. Deduplicated per local date across restarts by
//! `<home>/digest/state.json` (claimed under a file lock before anything is
//! sent, so a crash between claim and send skips that day rather than
//! sending twice).
//!
//! **Feedback.** Each finished item (a done task or goal) can get 👍 / 👎 /
//! "needs changes" on the dashboard (RPC `digest.feedback`). It is appended to
//! `<home>/feedback.jsonl` — the user-feedback store the evolution
//! reflection already reads (`external_factors::collect_user_feedback`) — as
//! `type` `positive` / `negative` / `correction` plus
//! `source = "deliverable"`, `item_kind`, `item_id`, `verdict`, `user_id`
//! and an optional note. Rows with that source are ignored by the proactive
//! dismissal probe (`proactive_feedback`), which reads `negative` as "the
//! person dismissed a proactive message". No channel buttons yet.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

/// Items listed per employee in the digest.
const MAX_ITEMS_PER_AGENT: usize = 8;
/// Longest look-back window, whatever the last digest says.
const MAX_WINDOW_DAYS: i64 = 7;
/// Feedback note length cap (characters).
pub const MAX_NOTE_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestConfig {
    pub enabled: bool,
    pub hour: u32,
    pub timezone: chrono_tz::Tz,
    pub exclude_agents: Vec<String>,
}

impl DigestConfig {
    fn off() -> Self {
        Self {
            enabled: false,
            hour: 8,
            timezone: chrono_tz::UTC,
            exclude_agents: Vec::new(),
        }
    }

    /// `[digest] enabled` (bool, default false), `hour` (0–23, default 8),
    /// `timezone` (IANA, default UTC), `exclude_agents` (employee ids). Read
    /// per tick. A wrong type or value anywhere reads as off.
    pub fn from_home(home: &Path) -> Self {
        let Some(table) = std::fs::read_to_string(home.join("config.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
        else {
            return Self::off();
        };
        let Some(sec) = table.get("digest").and_then(|v| v.as_table()) else {
            return Self::off();
        };
        Self::from_table(sec).unwrap_or_else(Self::off)
    }

    fn from_table(sec: &toml::Table) -> Option<Self> {
        let enabled = match sec.get("enabled") {
            None => false,
            Some(v) => v.as_bool()?,
        };
        let hour = match sec.get("hour") {
            None => 8,
            Some(v) => {
                let h = v.as_integer()?;
                if !(0..=23).contains(&h) {
                    return None;
                }
                h as u32
            }
        };
        let timezone = match sec.get("timezone") {
            None => chrono_tz::UTC,
            Some(v) => v.as_str()?.parse::<chrono_tz::Tz>().ok()?,
        };
        let exclude_agents = match sec.get("exclude_agents") {
            None => Vec::new(),
            Some(v) => v
                .as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            enabled,
            hour,
            timezone,
            exclude_agents,
        })
    }

    /// The local date a digest is due for at `now`, or `None` before the
    /// configured hour.
    pub fn due_date(&self, now: DateTime<Utc>) -> Option<NaiveDate> {
        let local = now.with_timezone(&self.timezone);
        (local.hour() >= self.hour).then(|| local.date_naive())
    }
}

/// One finished item (feedback target).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestItem {
    /// `task` | `goal`
    pub kind: String,
    pub id: String,
    pub title: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDigest {
    pub agent_id: String,
    pub finished: Vec<DigestItem>,
    pub finished_total: usize,
    pub activity_count: usize,
    pub pending_approvals: usize,
    pub runs_done: usize,
    pub runs_not_done: usize,
    /// `None` when cost telemetry could not be read.
    pub spend_usd: Option<f64>,
}

impl AgentDigest {
    fn is_empty(&self) -> bool {
        self.finished_total == 0
            && self.activity_count == 0
            && self.pending_approvals == 0
            && self.runs_done == 0
            && self.runs_not_done == 0
            && self.spend_usd.unwrap_or(0.0) <= 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub date: String,
    pub since: String,
    pub generated_at: String,
    pub agents: Vec<AgentDigest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DigestState {
    last_sent_date: Option<String>,
    last_generated_at: Option<String>,
}

fn digest_dir(home: &Path) -> PathBuf {
    home.join("digest")
}

fn state_path(home: &Path) -> PathBuf {
    digest_dir(home).join("state.json")
}

fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// Claim `date` for sending. `Ok(Some(previous_generated_at))` for the one
/// caller that claimed it; `Ok(None)` when it was already claimed.
pub fn claim_day(
    home: &Path,
    date: &str,
    now: DateTime<Utc>,
) -> std::io::Result<Option<Option<String>>> {
    let path = state_path(home);
    std::fs::create_dir_all(digest_dir(home))?;
    duduclaw_core::with_file_lock(&path.with_extension("lock"), || {
        let state: DigestState = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("digest state: {e}"),
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DigestState::default(),
            Err(e) => return Err(e),
        };
        if state.last_sent_date.as_deref() == Some(date) {
            return Ok(None);
        }
        let next = DigestState {
            last_sent_date: Some(date.to_string()),
            last_generated_at: Some(now.to_rfc3339()),
        };
        write_atomic(&path, &serde_json::to_string(&next).unwrap_or_default())?;
        Ok(Some(state.last_generated_at))
    })
}

/// Employee ids with an `agent.toml`, sorted.
fn employees(home: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(home.join("agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (!name.starts_with('.')
                && !name.starts_with('_')
                && duduclaw_core::is_valid_agent_id(&name)
                && e.path().join("agent.toml").is_file())
            .then_some(name)
        })
        .collect();
    out.sort();
    out
}

fn after(ts: Option<&str>, since: DateTime<Utc>) -> bool {
    ts.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t.with_timezone(&Utc) >= since)
}

/// Assemble the digest for the window `[since, now]` (no model call).
pub async fn assemble(
    home: &Path,
    cfg: &DigestConfig,
    date: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Digest {
    let store = if home.join("tasks.db").exists() {
        crate::task_store::TaskStore::open(home).ok()
    } else {
        None
    };
    let broker = if home.join("approvals.db").exists() {
        crate::approval::ApprovalBroker::open(home).ok()
    } else {
        None
    };
    let telemetry = crate::cost_telemetry::get_telemetry();
    let since_day = since.format("%Y-%m-%d").to_string();
    let mut agents = Vec::new();
    for agent in employees(home) {
        if cfg.exclude_agents.iter().any(|a| a == &agent) {
            continue;
        }
        let mut d = AgentDigest {
            agent_id: agent.clone(),
            finished: Vec::new(),
            finished_total: 0,
            activity_count: 0,
            pending_approvals: 0,
            runs_done: 0,
            runs_not_done: 0,
            spend_usd: None,
        };
        if let Some(store) = &store {
            if let Ok(done) = store.list_tasks(Some("done"), Some(&agent), None).await {
                let mut done: Vec<_> = done
                    .into_iter()
                    .filter(|t| after(t.completed_at.as_deref().or(Some(&t.updated_at)), since))
                    .collect();
                done.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                d.finished_total = done.len();
                d.finished = done
                    .into_iter()
                    .take(MAX_ITEMS_PER_AGENT)
                    .map(|t| DigestItem {
                        kind: if t.goal_mode { "goal" } else { "task" }.into(),
                        id: t.id,
                        title: duduclaw_core::truncate_chars(&t.title, 120).to_string(),
                        completed_at: t.completed_at,
                    })
                    .collect();
            }
            if let Ok((rows, _)) = store.list_activity(Some(&agent), None, 500, 0).await {
                d.activity_count = rows
                    .iter()
                    .filter(|r| after(Some(&r.timestamp), since))
                    .count();
            }
            if let Ok(resps) = store.list_responsibilities(Some(&agent)).await {
                for r in resps {
                    if let Ok(occs) = store.list_occurrences(&r.responsibility_id).await {
                        for o in occs
                            .iter()
                            .filter(|o| after(o.settled_at.as_deref(), since))
                        {
                            if o.outcome.as_deref() == Some("done") {
                                d.runs_done += 1;
                            } else {
                                d.runs_not_done += 1;
                            }
                        }
                    }
                }
            }
        }
        if let Some(b) = &broker {
            d.pending_approvals = b
                .list_pending(Some(&agent))
                .await
                .map(|p| p.len())
                .unwrap_or(0);
        }
        if let Some(t) = telemetry {
            let days = (now - since).num_days().clamp(1, MAX_WINDOW_DAYS) as u64 + 1;
            d.spend_usd = t.daily_cost_series(&agent, days).await.ok().map(|series| {
                series
                    .iter()
                    .filter(|(day, _)| day.as_str() >= since_day.as_str())
                    .map(|(_, mc)| *mc as f64)
                    .sum::<f64>()
                    / 100_000.0
            });
        }
        if !d.is_empty() {
            agents.push(d);
        }
    }
    Digest {
        date: date.to_string(),
        since: since.to_rfc3339(),
        generated_at: now.to_rfc3339(),
        agents,
    }
}

/// Plain-text channel message for a digest (zh-TW, like every notice).
pub fn render_text(d: &Digest, link: Option<&str>) -> String {
    let mut s = format!("🐾 你不在的時候（{}）\n", d.date);
    if d.agents.is_empty() {
        s.push_str("這段時間沒有新的完成項目、待決定事項或花費。\n");
    }
    for a in &d.agents {
        s.push_str(&format!("\n【{}】", a.agent_id));
        let mut parts = Vec::new();
        if a.finished_total > 0 {
            parts.push(format!("完成 {} 項", a.finished_total));
        }
        if a.pending_approvals > 0 {
            parts.push(format!("{} 項等你決定", a.pending_approvals));
        }
        if a.runs_done + a.runs_not_done > 0 {
            parts.push(format!(
                "持續任務 {} 輪（未完成 {}）",
                a.runs_done + a.runs_not_done,
                a.runs_not_done
            ));
        }
        if a.activity_count > 0 {
            parts.push(format!("{} 則動態", a.activity_count));
        }
        if let Some(usd) = a.spend_usd.filter(|v| *v > 0.0) {
            parts.push(format!("花費約 ${usd:.2}"));
        }
        s.push_str(&parts.join("、"));
        s.push('\n');
        for item in a.finished.iter().take(3) {
            s.push_str(&format!(
                "・{}\n",
                duduclaw_core::truncate_chars(&item.title, 60)
            ));
        }
    }
    if let Some(l) = link {
        s.push_str(&format!("\n在儀表板給成果回饋：{l}"));
    }
    s
}

/// Where a saved digest lives.
pub fn digest_path(home: &Path, date: &str) -> Option<PathBuf> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|_| digest_dir(home).join(format!("{date}.json")))
}

/// The newest saved digest, if any.
pub fn latest(home: &Path) -> Option<Digest> {
    let mut names: Vec<String> = std::fs::read_dir(digest_dir(home))
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.len() == 15 && n.ends_with(".json"))
        .collect();
    names.sort();
    let name = names.pop()?;
    serde_json::from_str(&std::fs::read_to_string(digest_dir(home).join(name)).ok()?).ok()
}

/// One pass of the daily schedule. Only the gateway holding the instance
/// lock sends; everything fails quietly (logged) and never panics.
pub async fn tick(home: &Path, now: DateTime<Utc>) {
    let cfg = DigestConfig::from_home(home);
    if !cfg.enabled || !duduclaw_core::gateway_instance::held(home) {
        return;
    }
    let Some(date) = cfg.due_date(now) else {
        return;
    };
    let date = date.format("%Y-%m-%d").to_string();
    let prev = {
        let h = home.to_path_buf();
        let d = date.clone();
        match tokio::task::spawn_blocking(move || claim_day(&h, &d, now)).await {
            Ok(Ok(Some(prev))) => prev,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                warn!(error = %e, "digest: state unreadable — skipped");
                return;
            }
            Err(e) => {
                warn!(error = %e, "digest: claim task failed");
                return;
            }
        }
    };
    let floor = now - Duration::days(MAX_WINDOW_DAYS);
    let since = prev
        .and_then(|p| DateTime::parse_from_rfc3339(&p).ok())
        .map(|p| p.with_timezone(&Utc).max(floor))
        .unwrap_or(now - Duration::hours(24));
    let digest = assemble(home, &cfg, &date, since, now).await;
    if let Some(path) = digest_path(home, &date) {
        if let Err(e) = write_atomic(&path, &serde_json::to_string(&digest).unwrap_or_default()) {
            warn!(error = %e, "digest: could not save");
        }
    }
    let link = crate::deep_link::dashboard_base_url(home).map(|b| format!("{b}/"));
    let text = render_text(&digest, link.as_deref());
    deliver(home, &text).await;
    info!(date = %date, employees = digest.agents.len(), "digest: sent");
}

/// Plain text to every active Admin's verified linked channels.
async fn deliver(home: &Path, text: &str) {
    let db = match crate::decision_notify::open_user_db(home) {
        Ok(Some(db)) => db,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, "digest: identity store unavailable — not sent");
            return;
        }
    };
    let users = match db.list_users() {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "digest: cannot list users — not sent");
            return;
        }
    };
    let http = reqwest::Client::new();
    for u in users.iter().filter(|u| {
        u.role == duduclaw_auth::UserRole::Admin && u.status == duduclaw_auth::UserStatus::Active
    }) {
        let Ok(channels) = db.verified_channels_for_user(&u.id) else {
            continue;
        };
        for ident in channels {
            crate::install_notify::send_plain_text(
                home,
                &http,
                &ident.channel,
                &ident.channel_user_id,
                text,
            )
            .await;
        }
    }
}

/// Background loop: one pass every five minutes.
pub fn spawn(home: PathBuf) {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            iv.tick().await;
            tick(&home, Utc::now()).await;
        }
    });
}

// ── Feedback on finished work ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Up,
    Down,
    Changes,
}

impl Verdict {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "changes" => Some(Self::Changes),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Changes => "changes",
        }
    }
    /// The `feedback.jsonl` `type` the evolution reflection understands.
    pub fn signal_type(self) -> &'static str {
        match self {
            Self::Up => "positive",
            Self::Down => "negative",
            Self::Changes => "correction",
        }
    }
}

/// `feedback.jsonl` `source` of a deliverable feedback row.
pub const FEEDBACK_SOURCE: &str = "deliverable";

/// Append one deliverable feedback row (cross-process lock, convention 3).
pub fn record_feedback(
    home: &Path,
    agent_id: &str,
    item_kind: &str,
    item_id: &str,
    title: &str,
    verdict: Verdict,
    note: Option<&str>,
    user_id: &str,
    now: DateTime<Utc>,
) -> std::io::Result<Value> {
    let note = note
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| duduclaw_core::truncate_chars(n, MAX_NOTE_CHARS).to_string());
    let detail = match &note {
        Some(n) => format!(
            "deliverable {} on {item_kind} 「{}」: {n}",
            verdict.as_str(),
            duduclaw_core::truncate_chars(title, 80)
        ),
        None => format!(
            "deliverable {} on {item_kind} 「{}」",
            verdict.as_str(),
            duduclaw_core::truncate_chars(title, 80)
        ),
    };
    let row = json!({
        "agent_id": agent_id,
        "type": verdict.signal_type(),
        "channel": "dashboard",
        "detail": detail,
        "timestamp": now.to_rfc3339(),
        "source": FEEDBACK_SOURCE,
        "item_kind": item_kind,
        "item_id": item_id,
        "verdict": verdict.as_str(),
        "note": note,
        "user_id": user_id,
    });
    let path = home.join("feedback.jsonl");
    let line = format!("{row}\n");
    duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())
    })?;
    Ok(row)
}

/// Latest deliverable verdict per `item_id` (last 2,000 rows read).
pub fn feedback_by_item(home: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(content) = std::fs::read_to_string(home.join("feedback.jsonl")) else {
        return out;
    };
    let lines: Vec<&str> = content.lines().collect();
    for line in &lines[lines.len().saturating_sub(2000)..] {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("source").and_then(Value::as_str) != Some(FEEDBACK_SOURCE) {
            continue;
        }
        if let (Some(id), Some(verdict)) = (
            v.get("item_id").and_then(Value::as_str),
            v.get("verdict").and_then(Value::as_str),
        ) {
            out.insert(id.to_string(), verdict.to_string());
        }
    }
    out
}

#[cfg(test)]
mod digest_tests {
    use super::*;
    use chrono::TimeZone;

    fn home_with(cfg: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), cfg).unwrap();
        dir
    }

    #[test]
    fn config_is_off_by_default_and_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        assert!(!DigestConfig::from_home(d.path()).enabled);
        let d = home_with("[digest]\nenabled = true\nhour = 7\ntimezone = \"Asia/Taipei\"\n");
        let c = DigestConfig::from_home(d.path());
        assert!(c.enabled);
        assert_eq!(c.hour, 7);
        for bad in [
            "[digest]\nenabled = true\nhour = 24\n",
            "[digest]\nenabled = true\ntimezone = \"Mars/Base\"\n",
            "[digest]\nenabled = \"yes\"\n",
            "[digest]\nenabled = true\nexclude_agents = [1]\n",
        ] {
            assert!(
                !DigestConfig::from_home(home_with(bad).path()).enabled,
                "{bad}"
            );
        }
    }

    #[test]
    fn due_only_after_the_local_hour() {
        let c = DigestConfig {
            enabled: true,
            hour: 8,
            timezone: "Asia/Taipei".parse().unwrap(),
            exclude_agents: vec![],
        };
        // 23:30 UTC = 07:30 Taipei next day ⇒ not yet.
        let before = Utc.with_ymd_and_hms(2026, 10, 7, 23, 30, 0).unwrap();
        assert_eq!(c.due_date(before), None);
        let at = Utc.with_ymd_and_hms(2026, 10, 8, 0, 5, 0).unwrap();
        assert_eq!(c.due_date(at).unwrap().to_string(), "2026-10-08");
    }

    #[test]
    fn a_day_is_claimed_once_across_restarts() {
        let d = tempfile::tempdir().unwrap();
        let now = Utc::now();
        assert_eq!(claim_day(d.path(), "2026-10-08", now).unwrap(), Some(None));
        assert_eq!(claim_day(d.path(), "2026-10-08", now).unwrap(), None);
        let next = claim_day(d.path(), "2026-10-09", now).unwrap();
        assert_eq!(next, Some(Some(now.to_rfc3339())));
    }

    #[tokio::test]
    async fn assembles_without_a_model_and_skips_quiet_or_excluded_employees() {
        let d = home_with("");
        for a in ["alice", "bob", "carol"] {
            let dir = d.path().join("agents").join(a);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), "[agent]\n").unwrap();
        }
        let store = crate::task_store::TaskStore::open(d.path()).unwrap();
        for (id, who) in [("t1", "alice"), ("t2", "carol")] {
            let mut t = crate::task_store::TaskRow::new(
                id.into(),
                format!("報告 {id}"),
                "".into(),
                "medium".into(),
                who.into(),
                "op".into(),
            );
            t.status = "done".into();
            t.completed_at = Some(Utc::now().to_rfc3339());
            store.insert_task(&t).await.unwrap();
        }
        let cfg = DigestConfig {
            enabled: true,
            hour: 0,
            timezone: chrono_tz::UTC,
            exclude_agents: vec!["carol".into()],
        };
        let now = Utc::now();
        let dg = assemble(d.path(), &cfg, "2026-10-08", now - Duration::hours(24), now).await;
        let ids: Vec<_> = dg.agents.iter().map(|a| a.agent_id.as_str()).collect();
        assert_eq!(ids, vec!["alice"]); // bob quiet, carol opted out
        assert_eq!(dg.agents[0].finished[0].id, "t1");
        let text = render_text(&dg, Some("http://x/"));
        assert!(text.contains("alice") && text.contains("完成 1 項") && text.contains("報告 t1"));
    }

    #[test]
    fn feedback_rows_feed_the_existing_user_feedback_store() {
        let d = tempfile::tempdir().unwrap();
        record_feedback(
            d.path(),
            "alice",
            "task",
            "t1",
            "週報",
            Verdict::Changes,
            Some("請補上數字"),
            "u1",
            Utc::now(),
        )
        .unwrap();
        record_feedback(
            d.path(),
            "alice",
            "task",
            "t1",
            "週報",
            Verdict::Up,
            None,
            "u1",
            Utc::now(),
        )
        .unwrap();
        let content = std::fs::read_to_string(d.path().join("feedback.jsonl")).unwrap();
        let first: Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(first["type"], "correction");
        assert_eq!(first["source"], "deliverable");
        assert_eq!(
            crate::external_factors::feedback_signal_kind(&first),
            "correction"
        );
        assert_eq!(
            feedback_by_item(d.path()).get("t1").map(String::as_str),
            Some("up")
        );
    }
}
