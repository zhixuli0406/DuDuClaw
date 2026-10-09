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
//! every active Admin account's verified linked channels. On by default
//! (owner decision 2026-10-08; `config.toml [digest] enabled = false` turns it
//! off, and a `config.toml` that exists but cannot be read or parsed, or any
//! wrong value in `[digest]`, also reads as off); `exclude_agents` opts
//! employees out. Deduplicated per local date across restarts by
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
//! person dismissed a proactive message".
//!
//! **Channel buttons (2026-10-08 close-out).** The channel message numbers
//! the items it shows (`[n]`) and, on Telegram / Discord / Slack / LINE,
//! carries 👍 / 👎 / ✏️ buttons for the first
//! [`crate::channel_format::DIGEST_BUTTON_ITEMS`] of them. A press is a
//! `decision_action` of source `Digest` with the reference
//! `<yyyymmdd>.<n>` ([`feedback_ref`]) and is authorized like
//! `digest.feedback`: the presser's channel identity must map to a
//! **verified**, active dashboard account, which must hold Operator on the
//! employee and pass the task's audience (and the channel must pass it too);
//! a task must be `done`. A press cannot carry a note: "needs changes" from a
//! channel is recorded without one (the dashboard takes notes).
//!
//! **Artifacts.** Files the employee handed over in the window (outbound
//! rows of the `artifacts.jsonl` ledger) are listed per employee and take
//! the same feedback (`item_kind = "artifact"`, `item_id` from
//! [`artifact_item_id`]); one tied to a task is authorized like that task,
//! one without a task needs Operator on the employee.
//!
//! **Notification governance.** Channel delivery is an L1 (`Fyi`) notice:
//! inside `config.toml [notify] quiet_hours` it is queued
//! (`notify_governance`, kind `digest`) and delivered by the drainer when
//! the window ends, re-rendered from the saved digest; every push is counted
//! under `digest.daily` in the action-rate stats. In the channel text, a
//! task whose audience does not admit that channel is listed without its
//! title.

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
/// Delivered files listed per employee.
const MAX_ARTIFACTS_PER_AGENT: usize = 5;
/// Items of each kind (finished work, files) shown per employee in the
/// channel text; these are the numbered `[n]` items.
const SHOWN_PER_KIND: usize = 3;
/// Stats bucket of a delivered digest.
pub const NOTIFY_TYPE: &str = "digest.daily";

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
            ..Self::defaults()
        }
    }

    fn defaults() -> Self {
        Self {
            enabled: true,
            hour: 8,
            timezone: chrono_tz::UTC,
            exclude_agents: Vec::new(),
        }
    }

    /// `[digest] enabled` (bool, default true), `hour` (0–23, default 8),
    /// `timezone` (IANA, default UTC), `exclude_agents` (employee ids). Read
    /// per tick. No `config.toml` or no `[digest]` section ⇒ the defaults
    /// (on). A `config.toml` that exists but cannot be read or parsed, a
    /// non-table `[digest]`, or a wrong type or value anywhere reads as off.
    pub fn from_home(home: &Path) -> Self {
        let text = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::defaults(),
            Err(_) => return Self::off(),
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            return Self::off();
        };
        match table.get("digest") {
            None => Self::defaults(),
            Some(v) => match v.as_table() {
                Some(sec) => Self::from_table(sec).unwrap_or_else(Self::off),
                None => Self::off(),
            },
        }
    }

    fn from_table(sec: &toml::Table) -> Option<Self> {
        let enabled = match sec.get("enabled") {
            None => true,
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
    /// The `[n]` the channel message shows for this item; 0 = not shown
    /// there (dashboard only).
    #[serde(default)]
    pub no: u32,
}

/// One file the employee handed over in the window (`artifacts.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestArtifact {
    pub agent_id: String,
    /// The `/api/files/download?name=` key.
    pub archived_name: String,
    /// Human-meaningful name (cut at 120 characters).
    pub name: String,
    pub task_id: Option<String>,
    pub produced_at: String,
    /// See [`DigestItem::no`].
    #[serde(default)]
    pub no: u32,
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
    /// Newest delivered files (at most [`MAX_ARTIFACTS_PER_AGENT`]).
    #[serde(default)]
    pub artifacts: Vec<DigestArtifact>,
    #[serde(default)]
    pub artifacts_total: usize,
}

impl AgentDigest {
    fn is_empty(&self) -> bool {
        self.finished_total == 0
            && self.artifacts_total == 0
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
            artifacts: Vec::new(),
            artifacts_total: 0,
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
                        no: 0,
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
        let delivered = crate::artifacts::delivered_between(home, &agent, since, now);
        d.artifacts_total = delivered.len();
        d.artifacts = delivered
            .into_iter()
            .take(MAX_ARTIFACTS_PER_AGENT)
            .map(|r| DigestArtifact {
                agent_id: agent.clone(),
                name: duduclaw_core::truncate_chars(
                    if r.display_name.is_empty() {
                        &r.archived_name
                    } else {
                        &r.display_name
                    },
                    120,
                )
                .to_string(),
                archived_name: r.archived_name,
                task_id: r.task_id,
                produced_at: r.produced_at,
                no: 0,
            })
            .collect();
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
    number_items(&mut agents);
    Digest {
        date: date.to_string(),
        since: since.to_rfc3339(),
        generated_at: now.to_rfc3339(),
        agents,
    }
}

/// Give the items the channel text shows their `[n]`, in display order.
fn number_items(agents: &mut [AgentDigest]) {
    let mut n = 0u32;
    for a in agents.iter_mut() {
        for item in a.finished.iter_mut().take(SHOWN_PER_KIND) {
            n += 1;
            item.no = n;
        }
        for art in a.artifacts.iter_mut().take(SHOWN_PER_KIND) {
            n += 1;
            art.no = n;
        }
    }
}

/// The reference a channel button carries for item `no` of the digest of
/// `date` (`YYYY-MM-DD`): `<yyyymmdd>.<n>`.
pub fn feedback_ref(date: &str, no: u32) -> String {
    format!("{}.{no}", date.replace('-', ""))
}

/// Inverse of [`feedback_ref`]: `(date, n)`. Strict: eight digits that form a
/// real date, a dot, 1–4 digits, `n ≥ 1`.
pub fn parse_ref(r: &str) -> Option<(String, u32)> {
    let (day, no) = r.split_once('.')?;
    if day.len() != 8 || !day.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if no.is_empty() || no.len() > 4 || !no.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = NaiveDate::parse_from_str(day, "%Y%m%d").ok()?;
    let no: u32 = no.parse().ok()?;
    (no >= 1).then(|| (date.format("%Y-%m-%d").to_string(), no))
}

/// The `feedback.jsonl` `item_id` of a delivered file.
pub fn artifact_item_id(agent_id: &str, archived_name: &str) -> String {
    format!("artifact:{agent_id}/{archived_name}")
}

/// What a numbered item points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestTarget {
    Task(DigestItem),
    Artifact(DigestArtifact),
}

/// The item shown as `[no]`.
pub fn target_by_no(d: &Digest, no: u32) -> Option<DigestTarget> {
    if no == 0 {
        return None;
    }
    d.agents.iter().find_map(|a| {
        a.finished
            .iter()
            .find(|i| i.no == no)
            .map(|i| DigestTarget::Task(i.clone()))
            .or_else(|| {
                a.artifacts
                    .iter()
                    .find(|x| x.no == no)
                    .map(|x| DigestTarget::Artifact(x.clone()))
            })
    })
}

/// `(n, reference)` for every numbered item, in display order.
pub fn button_refs(d: &Digest) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = d
        .agents
        .iter()
        .flat_map(|a| {
            a.finished
                .iter()
                .map(|i| i.no)
                .chain(a.artifacts.iter().map(|x| x.no))
        })
        .filter(|n| *n > 0)
        .map(|n| (n, feedback_ref(&d.date, n)))
        .collect();
    out.sort_by_key(|(n, _)| *n);
    out
}

/// Plain-text channel message for a digest (zh-TW, like every notice).
pub fn render_text(d: &Digest, link: Option<&str>) -> String {
    render_text_for(d, link, &|_| true, false)
}

/// [`render_text`] for one channel: `title_visible(task_id)` says whether
/// that channel may carry the task's title (its audience), `buttons` whether
/// the message carries feedback buttons.
pub fn render_text_for(
    d: &Digest,
    link: Option<&str>,
    title_visible: &dyn Fn(&str) -> bool,
    buttons: bool,
) -> String {
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
        if a.artifacts_total > 0 {
            parts.push(format!("交付 {} 個檔案", a.artifacts_total));
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
        let label = |no: u32| if no > 0 { format!("[{no}] ") } else { String::new() };
        for item in a.finished.iter().take(SHOWN_PER_KIND) {
            if title_visible(&item.id) {
                s.push_str(&format!(
                    "・{}{}\n",
                    label(item.no),
                    duduclaw_core::truncate_chars(&item.title, 60)
                ));
            } else {
                s.push_str(&format!("・{}（受限任務，標題不在此顯示）\n", label(item.no)));
            }
        }
        for art in a.artifacts.iter().take(SHOWN_PER_KIND) {
            let visible = art.task_id.as_deref().is_none_or(|t| title_visible(t));
            if visible {
                s.push_str(&format!(
                    "・{}📎 {}\n",
                    label(art.no),
                    duduclaw_core::truncate_chars(&art.name, 60)
                ));
            } else {
                s.push_str(&format!("・{}📎（受限任務的檔案）\n", label(art.no)));
            }
        }
    }
    if buttons {
        let shown = button_refs(d).len().min(crate::channel_format::DIGEST_BUTTON_ITEMS);
        if shown > 0 {
            s.push_str(&format!(
                "\n用下方按鈕給第 1–{shown} 項回饋（👍 好／👎 不好／✏️ 需要修改）。"
            ));
        }
        if let Some(l) = link {
            s.push_str(&format!("\n其他項目與修改說明請到儀表板：{l}"));
        }
    } else if let Some(l) = link {
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
    deliver(home, &digest).await;
    info!(date = %date, employees = digest.agents.len(), "digest: sent");
}

/// Every active Admin's verified linked channels, through notification
/// governance ([`deliver_governed`]).
async fn deliver(home: &Path, digest: &Digest) {
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
            deliver_governed(home, &http, &ident.channel, &ident.channel_user_id, digest, Utc::now())
                .await;
        }
    }
}

/// What [`deliver_governed`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Sent,
    /// Quiet hours: queued for the drainer.
    Deferred,
    Failed,
}

/// One destination: an L1 (`Fyi`) notice under the deployment's quiet hours
/// (`config.toml [notify] quiet_hours`; a digest belongs to no single
/// employee). Inside the window it is queued (kind `digest`, the saved
/// digest's date as the reference) and sent by the drainer afterwards.
pub async fn deliver_governed(
    home: &Path,
    http: &reqwest::Client,
    channel: &str,
    chat_id: &str,
    digest: &Digest,
    now: DateTime<Utc>,
) -> DeliveryOutcome {
    use crate::notify_governance as ng;
    let level = ng::NotifyLevel::Fyi;
    let policy = ng::QuietPolicy {
        window: ng::load_global_window(home),
        tz: ng::NotifyTz::System,
    };
    if let Some(until) = policy.decide(level, now) {
        let queued = ng::enqueue(
            home,
            ng::DeferredNotice {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: String::new(),
                channel: channel.to_string(),
                chat_id: chat_id.to_string(),
                level: level.as_str().to_string(),
                notify_type: NOTIFY_TYPE.to_string(),
                queued_at: now.to_rfc3339(),
                deliver_after: until.to_rfc3339(),
                kind: ng::NoticeKind::Digest,
                text: String::new(),
                link: None,
                no_button_hint: None,
                decision_source: None,
                decision_id: Some(digest.date.clone()),
            },
        );
        return if queued {
            DeliveryOutcome::Deferred
        } else {
            DeliveryOutcome::Failed
        };
    }
    if send_digest(home, http, channel, chat_id, digest).await {
        crate::notify_stats::record_push(home, NOTIFY_TYPE, level, None);
        DeliveryOutcome::Sent
    } else {
        DeliveryOutcome::Failed
    }
}

/// The drainer's half: re-render a queued digest from its saved file and
/// send it. `false` when the reference or the file is gone.
pub(crate) async fn deliver_deferred(
    home: &Path,
    http: &reqwest::Client,
    n: &crate::notify_governance::DeferredNotice,
) -> bool {
    let Some(path) = n.decision_id.as_deref().and_then(|d| digest_path(home, d)) else {
        warn!("digest: queued notice without a valid date — dropped");
        return false;
    };
    let Some(digest) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Digest>(&s).ok())
    else {
        warn!("digest: queued digest file unreadable — dropped");
        return false;
    };
    send_digest(home, http, &n.channel, &n.chat_id, &digest).await
}

/// Render for `channel` (titles the channel may not carry are left out) and
/// send, with feedback buttons where the platform has them; the
/// deployment's DM bot tokens are tried in turn.
async fn send_digest(
    home: &Path,
    http: &reqwest::Client,
    channel: &str,
    chat_id: &str,
    digest: &Digest,
) -> bool {
    use crate::review_evidence::audience::{channel_may_read_task, task_audience};
    let link = crate::deep_link::dashboard_base_url(home).map(|b| format!("{b}/"));
    let refs = button_refs(digest);
    let markup = crate::channel_format::digest_feedback_markup(channel, &refs);
    let visible = |task_id: &str| channel_may_read_task(channel, &task_audience(home, task_id));
    let text = render_text_for(digest, link.as_deref(), &visible, markup.is_some());
    let candidates = crate::config_crypto::channel_dm_token_candidates(home, channel).await;
    if candidates.is_empty() {
        info!(channel = %channel, "digest: no bot token configured; skipping");
        return false;
    }
    for token in &candidates {
        let ok = match &markup {
            Some(m) => crate::channel_sender::send_with_markup(
                http,
                channel,
                token,
                chat_id,
                &text,
                m.clone(),
            )
            .await
            .is_ok(),
            None => {
                crate::channel_sender::send_plain_text(home, http, channel, token, chat_id, &text)
                    .await
            }
        };
        if ok {
            return true;
        }
    }
    warn!(channel = %channel, "digest: send failed");
    false
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

/// Append one deliverable feedback row given on the dashboard.
#[allow(clippy::too_many_arguments)]
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
    record_feedback_via(
        home, agent_id, item_kind, item_id, title, verdict, note, user_id, "dashboard", now,
    )
}

/// Append one deliverable feedback row (cross-process lock, convention 3).
/// `via` is where it was given (`dashboard` or the channel name).
#[allow(clippy::too_many_arguments)]
pub fn record_feedback_via(
    home: &Path,
    agent_id: &str,
    item_kind: &str,
    item_id: &str,
    title: &str,
    verdict: Verdict,
    note: Option<&str>,
    user_id: &str,
    via: &str,
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
        "channel": via,
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

impl Verdict {
    /// The verdict a digest button carries.
    pub fn from_act(act: crate::decision_action::DecisionAct) -> Option<Self> {
        use crate::decision_action::DecisionAct;
        match act {
            DecisionAct::Up => Some(Self::Up),
            DecisionAct::Down => Some(Self::Down),
            DecisionAct::Changes => Some(Self::Changes),
            _ => None,
        }
    }

    fn ack(self) -> &'static str {
        match self {
            Self::Up => "👍 好",
            Self::Down => "👎 不好",
            Self::Changes => "✏️ 需要修改",
        }
    }
}

/// Every refusal of a digest press that happens before the presser is
/// shown to be allowed reads the same (no hint whether the item exists).
const PRESS_REFUSED: &str = "無法記錄這則回饋：此帳號沒有對這項成果給回饋的權限，或這則摘要已失效。";

/// A digest feedback press from a channel (`decision_notify::route_press`,
/// source `Digest`). Authorized like `digest.feedback`: the presser's
/// channel identity must map to a verified, active dashboard account
/// (re-read now) holding Operator on the employee, the task's audience must
/// admit that account and this channel, and a task must be `done`. Fails
/// closed on any unreadable store.
pub async fn apply_channel_feedback(
    home: &Path,
    channel: &str,
    channel_user_id: &str,
    reference: &str,
    act: crate::decision_action::DecisionAct,
) -> Result<String, String> {
    use crate::review_evidence::audience::{
        channel_may_read_task, dashboard_may_read_task, task_audience, trusted_dashboard_principal,
    };
    use duduclaw_auth::AccessLevel;
    let Some(verdict) = Verdict::from_act(act) else {
        return Err(PRESS_REFUSED.into());
    };
    let Some((date, no)) = parse_ref(reference) else {
        return Err(PRESS_REFUSED.into());
    };
    // Verified identity only; an unlinked presser is told how to link.
    let db = match crate::decision_notify::open_user_db(home) {
        Ok(Some(db)) => db,
        Ok(None) | Err(_) => {
            return Err(crate::decision_notify::refusal_text(
                crate::decision_notify::PressAuth::DenyUnknown,
                "給回饋",
            ));
        }
    };
    let uid = match db.find_verified_user_id_by_channel(channel, channel_user_id) {
        Ok(Some(uid)) if uid != "system" => uid,
        _ => {
            return Err(crate::decision_notify::refusal_text(
                crate::decision_notify::PressAuth::DenyUnknown,
                "給回饋",
            ));
        }
    };
    let live = trusted_dashboard_principal(home, &uid).map_err(|_| PRESS_REFUSED.to_string())?;
    let digest = digest_path(home, &date)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Digest>(&s).ok())
        .ok_or_else(|| PRESS_REFUSED.to_string())?;
    let target = target_by_no(&digest, no).ok_or_else(|| PRESS_REFUSED.to_string())?;

    // A task (or the task a file belongs to): Operator + audience, for the
    // account and for this channel.
    let task_allowed = |task: &crate::task_store::TaskRow| {
        let audience = task_audience(home, &task.id);
        live.has_agent_access(&task.assigned_to, AccessLevel::Operator)
            && dashboard_may_read_task(&live, &audience)
            && channel_may_read_task(channel, &audience)
    };
    let open_store = || {
        if home.join("tasks.db").exists() {
            crate::task_store::TaskStore::open(home).ok()
        } else {
            None
        }
    };
    let (agent_id, kind, item_id, title) = match target {
        DigestTarget::Task(item) => {
            let store = open_store().ok_or_else(|| PRESS_REFUSED.to_string())?;
            let task = match store.get_task(&item.id).await {
                Ok(Some(t)) => t,
                _ => return Err(PRESS_REFUSED.into()),
            };
            if !task_allowed(&task) {
                return Err(PRESS_REFUSED.into());
            }
            if task.status != "done" {
                return Err("這項工作目前不是已完成狀態，無法給回饋。".into());
            }
            let kind = if task.goal_mode { "goal" } else { "task" };
            (task.assigned_to.clone(), kind, task.id.clone(), task.title.clone())
        }
        DigestTarget::Artifact(art) => {
            let row = crate::artifacts::find_delivered(home, &art.agent_id, &art.archived_name)
                .ok_or_else(|| PRESS_REFUSED.to_string())?;
            match &row.task_id {
                Some(task_id) => {
                    let store = open_store().ok_or_else(|| PRESS_REFUSED.to_string())?;
                    let task = match store.get_task(task_id).await {
                        Ok(Some(t)) => t,
                        _ => return Err(PRESS_REFUSED.into()),
                    };
                    if task.assigned_to != row.agent_id || !task_allowed(&task) {
                        return Err(PRESS_REFUSED.into());
                    }
                }
                None => {
                    if !live.has_agent_access(&row.agent_id, AccessLevel::Operator) {
                        return Err(PRESS_REFUSED.into());
                    }
                }
            }
            (
                row.agent_id.clone(),
                "artifact",
                artifact_item_id(&row.agent_id, &row.archived_name),
                art.name.clone(),
            )
        }
    };
    let home_b = home.to_path_buf();
    let (agent_b, item_b, title_b, via) =
        (agent_id, item_id, title.clone(), channel.to_string());
    let written = tokio::task::spawn_blocking(move || {
        record_feedback_via(
            &home_b, &agent_b, kind, &item_b, &title_b, verdict, None, &uid, &via, Utc::now(),
        )
    })
    .await;
    match written {
        Ok(Ok(_)) => Ok(format!(
            "已記錄回饋：{}「{}」。{}",
            verdict.ack(),
            duduclaw_core::truncate_chars(&title, 40),
            if verdict == Verdict::Changes {
                "要說明怎麼改，請到儀表板補充。"
            } else {
                ""
            }
        )),
        _ => Err("回饋沒有存下來，請稍後再試。".into()),
    }
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
    fn config_is_on_by_default_and_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        assert!(DigestConfig::from_home(d.path()).enabled, "no config.toml");
        assert!(DigestConfig::from_home(home_with("[gateway]\n").path()).enabled, "no [digest]");
        assert!(DigestConfig::from_home(home_with("[digest]\nhour = 9\n").path()).enabled);
        assert!(!DigestConfig::from_home(home_with("[digest]\nenabled = false\n").path()).enabled);
        assert!(!DigestConfig::from_home(home_with("not = [valid").path()).enabled, "unparsable");
        assert!(!DigestConfig::from_home(home_with("digest = 1\n").path()).enabled, "non-table");
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
    fn references_round_trip_and_are_strict() {
        assert_eq!(feedback_ref("2026-10-08", 3), "20261008.3");
        assert_eq!(parse_ref("20261008.3"), Some(("2026-10-08".into(), 3)));
        for bad in ["20261008.0", "2026108.1", "20261332.1", "20261008.", "20261008.12345", "x.1", "20261008.1a", "20261008"] {
            assert_eq!(parse_ref(bad), None, "{bad}");
        }
    }

    fn agent(home: &Path, id: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\n").unwrap();
    }

    async fn done_task(home: &Path, id: &str, who: &str) {
        let store = crate::task_store::TaskStore::open(home).unwrap();
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

    fn deliver_file(home: &Path, agent: &str, name: &str, task: Option<&str>) {
        let line = serde_json::json!({
            "produced_at": Utc::now().to_rfc3339(),
            "agent_id": agent,
            "archived_name": format!("1700000000_{name}"),
            "display_name": name,
            "size": 10,
            "origin": "declared",
            "task_id": task,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join(crate::artifacts::ARTIFACTS_FILE))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    #[tokio::test]
    async fn files_are_listed_numbered_and_rendered_with_buttons() {
        let d = home_with("");
        agent(d.path(), "alice");
        done_task(d.path(), "t1", "alice").await;
        deliver_file(d.path(), "alice", "週報.xlsx", Some("t1"));
        deliver_file(d.path(), "alice", "草稿.md", None);
        let cfg = DigestConfig::from_home(d.path());
        let now = Utc::now();
        let dg = assemble(d.path(), &cfg, "2026-10-08", now - Duration::hours(24), now).await;
        let a = &dg.agents[0];
        assert_eq!(a.artifacts_total, 2);
        assert_eq!(a.finished[0].no, 1);
        let nos: Vec<u32> = a.artifacts.iter().map(|x| x.no).collect();
        assert_eq!(nos, vec![2, 3]);
        assert_eq!(
            button_refs(&dg),
            vec![(1, "20261008.1".into()), (2, "20261008.2".into()), (3, "20261008.3".into())]
        );
        assert!(matches!(target_by_no(&dg, 1), Some(DigestTarget::Task(i)) if i.id == "t1"));
        assert!(matches!(target_by_no(&dg, 3), Some(DigestTarget::Artifact(_))));
        assert_eq!(target_by_no(&dg, 9), None);
        let text = render_text_for(&dg, Some("http://x/"), &|_| true, true);
        assert!(text.contains("[1] 報告 t1") && text.contains("📎") && text.contains("交付 2 個檔案"), "{text}");
        assert!(text.contains("第 1–3 項"), "{text}");
        // A channel the task's audience does not admit gets no title.
        let hidden = render_text_for(&dg, None, &|_| false, false);
        assert!(!hidden.contains("報告 t1") && hidden.contains("受限任務"), "{hidden}");
        assert!(hidden.contains("草稿.md"), "a file without a task keeps its name");
        assert!(!hidden.contains("週報.xlsx"), "{hidden}");
        let markup = crate::channel_format::digest_feedback_markup("telegram", &button_refs(&dg)).unwrap();
        assert_eq!(markup["inline_keyboard"].as_array().unwrap().len(), 3);
        assert_eq!(
            markup["inline_keyboard"][0][2]["callback_data"],
            "duduclaw:decide:dig:chg:20261008.1"
        );
        assert!(crate::channel_format::digest_feedback_markup("whatsapp", &button_refs(&dg)).is_none());
    }

    /// A channel press: verified account with Operator records; an
    /// unverified identity, a Viewer, an unfinished task and a stale
    /// reference are refused.
    #[tokio::test]
    async fn channel_presses_are_authorized_like_the_dashboard() {
        use crate::decision_action::DecisionAct;
        use duduclaw_auth::{AccessLevel, UserDb, UserRole};
        let d = home_with("");
        let h = d.path();
        agent(h, "alice");
        done_task(h, "t1", "alice").await;
        deliver_file(h, "alice", "草稿.md", None);
        let cfg = DigestConfig::from_home(h);
        let now = Utc::now();
        let dg = assemble(h, &cfg, "2026-10-08", now - Duration::hours(24), now).await;
        std::fs::create_dir_all(h.join("digest")).unwrap();
        std::fs::write(digest_path(h, "2026-10-08").unwrap(), serde_json::to_string(&dg).unwrap()).unwrap();
        let db = UserDb::new(&h.join("users.db")).unwrap();
        let mk = |email: &str, level: AccessLevel, verified: bool, chat: &str| {
            let u = db
                .create_user(email, email, "isolated-test-password", UserRole::Employee)
                .unwrap();
            db.bind_agent(&u.id, "alice", level).unwrap();
            db.bind_channel_identity(&u.id, "telegram", chat, verified).unwrap();
        };
        mk("op@test.invalid", AccessLevel::Operator, true, "100");
        mk("viewer@test.invalid", AccessLevel::Viewer, true, "200");
        mk("unverified@test.invalid", AccessLevel::Operator, false, "300");

        let ok = apply_channel_feedback(h, "telegram", "100", "20261008.1", DecisionAct::Up).await;
        assert!(ok.as_deref().is_ok_and(|m| m.contains("已記錄回饋")), "{ok:?}");
        let ok = apply_channel_feedback(h, "telegram", "100", "20261008.2", DecisionAct::Changes).await;
        assert!(ok.is_ok(), "{ok:?}");
        for (who, r) in [("200", "20261008.1"), ("300", "20261008.1"), ("999", "20261008.1"), ("100", "20261008.9"), ("100", "20261009.1")] {
            assert!(
                apply_channel_feedback(h, "telegram", who, r, DecisionAct::Down).await.is_err(),
                "{who} {r}"
            );
        }
        let rows = std::fs::read_to_string(h.join("feedback.jsonl")).unwrap();
        assert_eq!(rows.lines().count(), 2);
        assert!(rows.contains("\"channel\":\"telegram\""));
        let by_item = feedback_by_item(h);
        assert_eq!(by_item.get("t1").map(String::as_str), Some("up"));
        let file_id = artifact_item_id("alice", "1700000000_草稿.md");
        assert_eq!(by_item.get(&file_id).map(String::as_str), Some("changes"));

        // The shared router decodes and authorizes the same press.
        let wire = crate::decision_action::encode(
            crate::decision_action::DecisionSource::Digest,
            DecisionAct::Down,
            "20261008.1",
        );
        let routed = crate::decision_notify::route_press(h, "telegram", "200", &wire).await;
        assert!(matches!(routed, Some(Err(_))), "viewer refused through the router");
    }

    #[tokio::test]
    async fn quiet_hours_queue_the_digest_for_the_drainer() {
        let d = home_with("[notify]\nquiet_hours = \"00:00-23:59\"\n");
        let dg = Digest {
            date: "2026-10-08".into(),
            since: Utc::now().to_rfc3339(),
            generated_at: Utc::now().to_rfc3339(),
            agents: vec![],
        };
        let http = reqwest::Client::new();
        // Pick an instant inside the window in the host's local time.
        let local_noon = chrono::Local::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .with_timezone(&Utc);
        let out = deliver_governed(d.path(), &http, "telegram", "42", &dg, local_noon).await;
        assert_eq!(out, DeliveryOutcome::Deferred);
        let queue = std::fs::read_to_string(d.path().join("notify_queue.jsonl")).unwrap();
        assert!(queue.contains("\"kind\":\"digest\"") && queue.contains("2026-10-08"), "{queue}");
        assert!(queue.contains(NOTIFY_TYPE));
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
