//! Notification governance — the one gate every outbound notification passes
//! through before it is allowed to interrupt a person (W2-4).
//!
//! ## Why this exists
//!
//! Before this module, every notification exit in the gateway
//! (`goal_notify::notify_agent_plain`, `decision_notify::deliver`,
//! `channel_alerts`, the budget breaker push, the GVU stagnation /
//! consolidation pushes, the skill-gap digest) decided *on its own* whether
//! and when to interrupt someone. There was no way to say "not at 3am", no
//! way to tell an urgent alert apart from an FYI, and no way to measure
//! whether any of it was worth sending.
//!
//! The design follows the escalation ladder in
//! `commercial/docs/ux-redesign-2026-08/02-ux-methodology.md` §主題 4 (P4-1),
//! which is Google SRE's page/ticket/report triage rendered as four levels.
//! [`NotifyLevel`] is that ladder as a type, stamped at every send site:
//!
//! | Level | Criterion | Behaviour here |
//! |---|---|---|
//! | L0 (absent) | no state change | never reaches this module — log/audit only |
//! | [`NotifyLevel::Fyi`] (L1) | happened, needs nobody | deferred out of quiet hours |
//! | [`NotifyLevel::Confirm`] (L2) | needs a person to know + tap | deferred out of quiet hours |
//! | [`NotifyLevel::Act`] (L3) | urgent + important + actionable + real | **always delivered** |
//!
//! L3 is deliberately un-suppressable: `needs_human`, a high-risk approval, a
//! budget breaker tripping the agent to a stop, and a channel outage are all
//! things a person would rather be woken for than discover eight hours late.
//!
//! ## Quiet hours (C7)
//!
//! `agent.toml [proactive] quiet_hours = "22:00-08:00"`, with
//! `config.toml [notify] quiet_hours` as the deployment-wide fallback for
//! notifications that have no owning agent. Evaluated in
//! `agent.toml [proactive] timezone` when that names a real IANA zone, else in
//! the host's system timezone.
//!
//! **Fail-open by construction.** A missing, blank, malformed, or degenerate
//! (`start == end`) value means *no quiet hours* — a parse bug must never
//! silently mute a deployment's notifications. Every rejection is logged at
//! `warn` with the offending value.
//!
//! The legacy numeric pair `[proactive] quiet_hours_start` / `quiet_hours_end`
//! is deliberately NOT read here. It defaults to 23–8 for **every** agent, so
//! honouring it would silently mute L1/L2 notifications overnight on every
//! existing install that never asked for that — exactly the invisible
//! suppression this design exists to prevent. It keeps its original, narrower
//! job: scheduling `[proactive]` checks (`duduclaw-agent::proactive`).
//!
//! ## Deferral, not deletion (F10)
//!
//! A suppressed notification is **queued**, never dropped:
//! `<home>/notify_queue.jsonl`, appended under
//! [`duduclaw_core::with_file_lock`] (project convention 3). When quiet hours
//! end, [`DeferredNotifyDrainer`] delivers everything due — plain notices for
//! the same destination merged into **one** message (P4-6: "批量結果一律
//! digest 成一則"), decision cards individually because each carries its own
//! buttons.
//!
//! The queue is bounded on both axes: [`MAX_QUEUE_ENTRIES`] newest entries are
//! kept, and anything older than [`MAX_QUEUE_AGE_HOURS`] is dropped rather
//! than delivered a day and a half stale. Both prunes are logged at `warn` —
//! a dropped notification is reported, never silent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Queue file, relative to the DuDuClaw home directory.
const QUEUE_FILE: &str = "notify_queue.jsonl";

/// Hard cap on queued notices. A deployment that generates more than this in
/// one quiet window has a noise problem the digest/action-rate tooling is
/// meant to surface; delivering 10k lines at 08:00 would be its own outage.
pub const MAX_QUEUE_ENTRIES: usize = 500;

/// A queued notice older than this is dropped instead of delivered — a
/// day-and-a-half-old "task finished" is noise, not news.
pub const MAX_QUEUE_AGE_HOURS: i64 = 36;

/// Merged-message item cap. Beyond this the drainer appends a "還有 N 則"
/// pointer rather than emitting a wall of text a channel would truncate.
const MERGE_ITEM_CAP: usize = 20;

/// Per-item character cap inside a merged message (CJK-safe — counts
/// codepoints, never slices raw bytes; project convention 1).
const MERGE_ITEM_CHARS: usize = 160;

// ── The ladder ───────────────────────────────────────────────────────────

/// Escalation level of one outbound notification (P4-1).
///
/// Ordered: `Fyi < Confirm < Act`. The ordering is meaningful — a future
/// per-user "only bother me at or above X" preference is a comparison against
/// this enum, not a new table.
///
/// Deliberately NOT `Serialize`/`Deserialize`: every wire surface (queue rows,
/// stats rows, the `notify.stats` payload) uses the [`NotifyLevel::as_str`]
/// tokens `L1`/`L2`/`L3`, and a derived encoding would quietly introduce a
/// second, different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotifyLevel {
    /// **L1** — something happened; nobody has to do anything. Dashboard
    /// badges, daily digest, evolution events, task completions.
    Fyi,
    /// **L2** — a person has to know and tap once. Kickoff gates, a paused
    /// autopilot rule, a goal awaiting acceptance.
    Confirm,
    /// **L3** — urgent AND important AND actionable AND real. `needs_human`,
    /// high-risk approvals, budget stop-work, channel outages. Never
    /// suppressed by quiet hours.
    Act,
}

impl NotifyLevel {
    /// Stable wire token (queue file, stats records, RPC payloads).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fyi => "L1",
            Self::Confirm => "L2",
            Self::Act => "L3",
        }
    }

    /// Parse a wire token back. Unknown input is `None` — a corrupt queue row
    /// must not silently become the most-permissive level.
    pub fn from_str_token(s: &str) -> Option<Self> {
        match s.trim() {
            "L1" => Some(Self::Fyi),
            "L2" => Some(Self::Confirm),
            "L3" => Some(Self::Act),
            _ => None,
        }
    }

    /// True when quiet hours may hold this notification back.
    pub fn is_suppressible(self) -> bool {
        !matches!(self, Self::Act)
    }
}

// ── Quiet hours ──────────────────────────────────────────────────────────

/// A parsed `HH:MM-HH:MM` quiet window. May wrap past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietWindow {
    start: NaiveTime,
    end: NaiveTime,
}

impl QuietWindow {
    /// Parse `"22:00-08:00"`. Returns `None` — fail-open, no quiet hours —
    /// for anything not exactly two `H:MM`/`HH:MM` times separated by `-`,
    /// and for a degenerate `start == end` (which reads equally well as "no
    /// quiet hours" and "mute forever"; the safe reading is off).
    ///
    /// Pure. The caller logs; this never does, so it stays usable in tests
    /// and hot paths alike.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        let (a, b) = s.split_once('-')?;
        let start = parse_hhmm(a)?;
        let end = parse_hhmm(b)?;
        if start == end {
            return None;
        }
        Some(Self { start, end })
    }

    /// Is `t` inside the window? Half-open `[start, end)` so a window ending
    /// at 08:00 delivers at 08:00 sharp.
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start < self.end {
            t >= self.start && t < self.end
        } else {
            // Wraps midnight (22:00-08:00).
            t >= self.start || t < self.end
        }
    }

    /// The first local instant at or after `now` at which the window is over.
    /// Only meaningful when `contains(now.time())`; callers gate on that.
    pub fn next_end_after(&self, now: NaiveDateTime) -> NaiveDateTime {
        let today = now.date().and_time(self.end);
        if today > now {
            today
        } else {
            // `succ_opt` only fails at the end of representable time.
            match now.date().succ_opt() {
                Some(d) => d.and_time(self.end),
                None => now,
            }
        }
    }

    /// `HH:MM-HH:MM` round-trip, for operator-facing surfaces.
    pub fn to_display(self) -> String {
        format!("{}-{}", self.start.format("%H:%M"), self.end.format("%H:%M"))
    }
}

/// Parse one `H:MM` / `HH:MM` clock time. Rejects 24:00, negative, extra
/// fields, and non-numeric input.
fn parse_hhmm(raw: &str) -> Option<NaiveTime> {
    let s = raw.trim();
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Which clock the quiet window is read against.
///
/// `Named` when `[proactive] timezone` holds a real IANA zone; `System`
/// otherwise (the spec's default: the host's own timezone). Keeping both in
/// one enum means the gate has a single code path regardless of which applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyTz {
    Named(chrono_tz::Tz),
    System,
}

impl NotifyTz {
    /// Resolve a configured timezone string. Blank or unparseable ⇒
    /// [`NotifyTz::System`] (logged by the caller that holds the context).
    pub fn resolve(raw: &str) -> Self {
        let s = raw.trim();
        if s.is_empty() {
            return Self::System;
        }
        match s.parse::<chrono_tz::Tz>() {
            Ok(tz) => Self::Named(tz),
            Err(_) => {
                warn!(timezone = %s, "notify-governance: 無法解析時區，改用系統時區");
                Self::System
            }
        }
    }

    /// Wall-clock time in this zone for a UTC instant.
    pub fn local_naive(self, now: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Named(tz) => now.with_timezone(&tz).naive_local(),
            Self::System => now.with_timezone(&chrono::Local).naive_local(),
        }
    }

    /// Convert a local wall-clock instant back to UTC.
    ///
    /// DST is handled explicitly rather than by `unwrap`: an **ambiguous**
    /// local time (fall-back hour, happens twice) takes the earlier instant —
    /// delivering at the first 08:00 is the friendlier of two correct answers;
    /// a **nonexistent** one (spring-forward gap) walks forward in 30-minute
    /// steps until it lands on a real instant, so a quiet window that ends
    /// inside the gap still ends.
    pub fn to_utc(self, local: NaiveDateTime) -> DateTime<Utc> {
        match self {
            Self::Named(tz) => resolve_local(&tz, local),
            Self::System => resolve_local(&chrono::Local, local),
        }
    }
}

/// Shared DST-aware local→UTC resolution for any [`TimeZone`].
fn resolve_local<T: TimeZone>(tz: &T, local: NaiveDateTime) -> DateTime<Utc> {
    let mut candidate = local;
    // 4 × 30 min covers every real-world DST gap (max observed: 2 hours).
    for _ in 0..5 {
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(t) => return t.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => return earliest.with_timezone(&Utc),
            LocalResult::None => {
                candidate += chrono::Duration::minutes(30);
            }
        }
    }
    // Unreachable for real zones; treat as "deliver now" rather than panic.
    warn!("notify-governance: 無法將本地時間換算為 UTC，改為立即投遞");
    Utc::now()
}

// ── The gate ─────────────────────────────────────────────────────────────

/// What the gate decided, in local wall-clock terms. Pure — unit-tested
/// without a clock, a filesystem, or a timezone database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Send it now.
    Deliver,
    /// Hold it until this local instant (the end of the quiet window).
    DeferUntil(NaiveDateTime),
}

/// The whole suppression rule, as one pure function.
///
/// - [`NotifyLevel::Act`] always delivers, quiet hours or not.
/// - No window configured ⇒ always delivers.
/// - Outside the window ⇒ delivers.
/// - Inside the window ⇒ defer to the window's end.
pub fn gate(level: NotifyLevel, window: Option<QuietWindow>, local_now: NaiveDateTime) -> GateDecision {
    if !level.is_suppressible() {
        return GateDecision::Deliver;
    }
    let Some(w) = window else {
        return GateDecision::Deliver;
    };
    if w.contains(local_now.time()) {
        GateDecision::DeferUntil(w.next_end_after(local_now))
    } else {
        GateDecision::Deliver
    }
}

// ── Policy loading ───────────────────────────────────────────────────────

/// The effective quiet-hours policy for one notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietPolicy {
    pub window: Option<QuietWindow>,
    pub tz: NotifyTz,
}

impl QuietPolicy {
    /// No suppression at all — the fail-open default every error path returns.
    pub fn none() -> Self {
        Self {
            window: None,
            tz: NotifyTz::System,
        }
    }

    /// Apply the gate at a UTC instant, returning a UTC deadline.
    pub fn decide(&self, level: NotifyLevel, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match gate(level, self.window, self.tz.local_naive(now)) {
            GateDecision::Deliver => None,
            GateDecision::DeferUntil(local) => Some(self.tz.to_utc(local)),
        }
    }

    /// One zh-TW sentence stating exactly what is being held back and until
    /// when — the operator-facing text behind F10 ("靜默失敗條件攤在陽光下").
    /// Rendered by the dashboard next to the agent's channel settings; `None`
    /// when nothing is ever suppressed.
    pub fn suppression_note_zh(&self) -> Option<String> {
        let w = self.window?;
        Some(format!(
            "勿擾時段 {}：週知／待確認通知會延後到時段結束後合併送出，須處理的通知（需人工決定、高風險審批、預算停工、通道故障）照常即時送達。",
            w.to_display()
        ))
    }
}

/// Read `agent.toml [proactive] quiet_hours` + `timezone` for one agent,
/// falling back to `config.toml [notify] quiet_hours` when the agent sets
/// none.
///
/// Fail-open at every step: unreadable file, unparseable TOML, missing
/// section, or a malformed window all yield "no quiet hours". A malformed
/// **present** value is logged at `warn` — silence about a setting that is
/// being ignored is the failure mode this whole module exists to prevent.
pub fn load_agent_policy(home_dir: &Path, agent_id: &str) -> QuietPolicy {
    let mut tz = NotifyTz::System;
    let mut window = None;

    let agent_toml = home_dir
        .join("agents")
        .join(agent_id)
        .join("agent.toml");
    if let Ok(content) = std::fs::read_to_string(&agent_toml) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(p) = table.get("proactive").and_then(|v| v.as_table()) {
                if let Some(z) = p.get("timezone").and_then(|v| v.as_str()) {
                    tz = NotifyTz::resolve(z);
                }
                if let Some(raw) = p.get("quiet_hours").and_then(|v| v.as_str()) {
                    let raw = raw.trim();
                    if !raw.is_empty() {
                        window = QuietWindow::parse(raw);
                        if window.is_none() {
                            warn!(
                                agent = %agent_id, value = %raw,
                                "notify-governance: quiet_hours 格式無法解析（需 \"HH:MM-HH:MM\"），本代理視為未設定勿擾時段"
                            );
                        }
                    }
                }
            }
        }
    }

    if window.is_none() {
        window = load_global_window(home_dir);
    }
    QuietPolicy { window, tz }
}

/// The agent's OWN `agent.toml [proactive] quiet_hours` string, verbatim
/// (trimmed) — never falling back to `config.toml [notify] quiet_hours` the
/// way [`load_agent_policy`] does.
///
/// [`load_agent_policy`] returns the EFFECTIVE window after that fallback,
/// which is right for display but wrong for an edit form's initial value:
/// prefilling from the effective value and saving it unchanged would pin the
/// deployment-wide default into this one agent's config, silently opting it
/// out of ever following a future change to the global fallback. The
/// dashboard (W2-8) reads this to seed the field it lets an operator edit,
/// and [`QuietPolicy::suppression_note_zh`] (built from [`load_agent_policy`]
/// instead) to show what's effectively in force alongside it.
///
/// Empty string when unset, unreadable, or unparseable TOML — this is a raw
/// read, not a validating one; the write path validates.
pub fn agent_raw_quiet_hours(home_dir: &Path, agent_id: &str) -> String {
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    let Ok(content) = std::fs::read_to_string(&agent_toml) else {
        return String::new();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return String::new();
    };
    table
        .get("proactive")
        .and_then(|v| v.as_table())
        .and_then(|p| p.get("quiet_hours"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// The deployment-wide fallback window (`config.toml [notify] quiet_hours`),
/// used by notifications with no owning agent and by agents that set none.
pub fn load_global_window(home_dir: &Path) -> Option<QuietWindow> {
    let content = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    let raw = table
        .get("notify")
        .and_then(|v| v.as_table())?
        .get("quiet_hours")
        .and_then(|v| v.as_str())?
        .trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = QuietWindow::parse(raw);
    if parsed.is_none() {
        warn!(
            value = %raw,
            "notify-governance: [notify] quiet_hours 格式無法解析（需 \"HH:MM-HH:MM\"），全域勿擾時段視為未設定"
        );
    }
    parsed
}

// ── Deferred queue ───────────────────────────────────────────────────────

/// What a queued notice turns back into at delivery time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A plain line. Mergeable with other plain notices for the same
    /// destination.
    Plain,
    /// A decision card. Re-rendered with its buttons at delivery time and
    /// never merged — each one needs its own tap targets.
    Decision,
}

impl Default for NoticeKind {
    fn default() -> Self {
        Self::Plain
    }
}

/// One notification held back by quiet hours.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredNotice {
    pub id: String,
    /// Agent whose bot token / config resolves the destination. May be empty
    /// for system-level notices.
    #[serde(default)]
    pub agent_id: String,
    pub channel: String,
    pub chat_id: String,
    /// Wire token of the [`NotifyLevel`] this was queued at.
    pub level: String,
    /// Stats bucket, e.g. `evolution.stagnation` (see [`crate::notify_stats`]).
    pub notify_type: String,
    pub queued_at: String,
    pub deliver_after: String,
    #[serde(default)]
    pub kind: NoticeKind,
    /// Body text (plain line, or the decision card's composed body).
    pub text: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub no_button_hint: Option<String>,
    /// [`crate::decision_action::DecisionSource::token`], for `Decision`.
    #[serde(default)]
    pub decision_source: Option<String>,
    #[serde(default)]
    pub decision_id: Option<String>,
}

impl DeferredNotice {
    fn queued_at_dt(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.queued_at)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    fn deliver_after_dt(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.deliver_after)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }
}

fn queue_path(home_dir: &Path) -> PathBuf {
    home_dir.join(QUEUE_FILE)
}

/// Read every parseable queue row. Unparseable rows are dropped with a
/// `debug` (a corrupt line must not wedge the drainer); a missing file is an
/// empty queue, not an error.
fn read_queue(path: &Path) -> Vec<DeferredNotice> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<DeferredNotice>(l) {
            Ok(n) => Some(n),
            Err(e) => {
                debug!(error = %e, "notify-governance: 略過無法解析的排隊行");
                None
            }
        })
        .collect()
}

/// Atomically replace the queue file with `notices` (temp + rename). Called
/// only while holding the advisory lock.
fn write_queue(path: &Path, notices: &[DeferredNotice]) -> std::io::Result<()> {
    let mut body = String::new();
    for n in notices {
        if let Ok(line) = serde_json::to_string(n) {
            body.push_str(&line);
            body.push('\n');
        }
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// Drop rows past [`MAX_QUEUE_AGE_HOURS`], then keep only the newest
/// [`MAX_QUEUE_ENTRIES`]. Pure — returns `(kept, dropped_stale, dropped_overflow)`
/// so the caller can report both losses honestly.
pub fn prune(
    mut notices: Vec<DeferredNotice>,
    now: DateTime<Utc>,
) -> (Vec<DeferredNotice>, usize, usize) {
    let cutoff = now - chrono::Duration::hours(MAX_QUEUE_AGE_HOURS);
    let before = notices.len();
    notices.retain(|n| match n.queued_at_dt() {
        Some(t) => t >= cutoff,
        // An unparseable timestamp can never be aged out; treat it as stale
        // rather than immortal.
        None => false,
    });
    let stale = before - notices.len();

    let overflow = notices.len().saturating_sub(MAX_QUEUE_ENTRIES);
    if overflow > 0 {
        notices.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
        notices.drain(0..overflow);
    }
    (notices, stale, overflow)
}

/// Append one notice to the queue, pruning in the same locked read-modify-write
/// so the file can never grow without bound.
///
/// Best-effort by contract: an I/O failure is logged and reported as `false`,
/// letting the caller decide (every current caller treats it as "the push did
/// not happen", which is the truth).
pub fn enqueue(home_dir: &Path, notice: DeferredNotice) -> bool {
    let path = queue_path(home_dir);
    let now = Utc::now();
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut notices = read_queue(&path);
        notices.push(notice.clone());
        let (kept, stale, overflow) = prune(notices, now);
        if stale > 0 || overflow > 0 {
            warn!(
                stale, overflow,
                "notify-governance: 排隊中的通知已達上限或過期，已丟棄（未送出）"
            );
        }
        write_queue(&path, &kept)
    });
    match result {
        Ok(()) => true,
        Err(e) => {
            warn!(error = %e, "notify-governance: 寫入 notify_queue.jsonl 失敗，本則通知未排隊");
            false
        }
    }
}

/// Take every notice whose `deliver_after` has passed, leaving the rest in
/// the file. One locked read-modify-write, so two drainers cannot deliver the
/// same notice twice.
pub fn take_due(home_dir: &Path, now: DateTime<Utc>) -> Vec<DeferredNotice> {
    let path = queue_path(home_dir);
    if !path.exists() {
        return Vec::new();
    }
    let result = duduclaw_core::with_file_lock(&path, || {
        let notices = read_queue(&path);
        let (kept, stale, overflow) = prune(notices, now);
        if stale > 0 || overflow > 0 {
            warn!(stale, overflow, "notify-governance: 排隊通知過期或溢位，已丟棄（未送出）");
        }
        let (due, pending): (Vec<_>, Vec<_>) = kept.into_iter().partition(|n| {
            n.deliver_after_dt().map(|t| t <= now).unwrap_or(true)
        });
        write_queue(&path, &pending)?;
        Ok(due)
    });
    match result {
        Ok(due) => due,
        Err(e) => {
            warn!(error = %e, "notify-governance: 讀取 notify_queue.jsonl 失敗；本輪不投遞");
            Vec::new()
        }
    }
}

// ── Merging ──────────────────────────────────────────────────────────────

/// A merged batch: one destination, one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedBatch {
    pub agent_id: String,
    pub channel: String,
    pub chat_id: String,
    pub text: String,
    /// The `(stats bucket, level)` of every notice this one message stands in
    /// for. Recorded per-notice in the action-rate stats so a merged delivery
    /// is not undercounted, and at each notice's ORIGINAL level so a merged
    /// L2 does not get filed as an L1.
    pub notify_types: Vec<(String, NotifyLevel)>,
}

/// Group plain notices by destination and render one zh-TW message each
/// (P4-6). Pure — the drainer's only formatting logic, unit-tested directly.
///
/// A single queued notice is rendered as itself with a short quiet-hours
/// preface, not as a one-item numbered list.
pub fn merge_plain(notices: &[DeferredNotice]) -> Vec<MergedBatch> {
    let mut groups: BTreeMap<(String, String, String), Vec<&DeferredNotice>> = BTreeMap::new();
    for n in notices.iter().filter(|n| n.kind == NoticeKind::Plain) {
        groups
            .entry((n.agent_id.clone(), n.channel.clone(), n.chat_id.clone()))
            .or_default()
            .push(n);
    }

    groups
        .into_iter()
        .map(|((agent_id, channel, chat_id), items)| {
            let text = render_merged(&items);
            let notify_types = items
                .iter()
                .map(|n| {
                    // A queue row with an unrecognisable level is filed as the
                    // lowest one rather than dropped — it is telemetry, and
                    // losing the count would be worse than a slightly wrong
                    // bucket.
                    let lvl = NotifyLevel::from_str_token(&n.level).unwrap_or(NotifyLevel::Fyi);
                    (n.notify_type.clone(), lvl)
                })
                .collect();
            MergedBatch {
                agent_id,
                channel,
                chat_id,
                text,
                notify_types,
            }
        })
        .collect()
}

fn render_merged(items: &[&DeferredNotice]) -> String {
    if items.len() == 1 {
        return format!("🌙 勿擾時段收到 1 則通知：\n{}", items[0].text.trim());
    }
    let mut out = format!("🌙 勿擾時段收到 {} 則通知：", items.len());
    for (i, n) in items.iter().take(MERGE_ITEM_CAP).enumerate() {
        let line = duduclaw_core::truncate_chars(n.text.trim(), MERGE_ITEM_CHARS);
        // Collapse to one line so the numbering stays readable.
        let line = line.replace('\n', " ");
        out.push_str(&format!("\n{}. {}", i + 1, line.trim()));
    }
    if items.len() > MERGE_ITEM_CAP {
        out.push_str(&format!(
            "\n…另有 {} 則，請至儀表板查看。",
            items.len() - MERGE_ITEM_CAP
        ));
    }
    out
}

// ── Drainer ──────────────────────────────────────────────────────────────

/// Delivers deferred notices once their quiet window has passed.
///
/// Runs on its own interval rather than hooking the gate: a notice queued at
/// 23:00 has nobody to piggyback on at 08:00, and a timer is the only thing
/// guaranteed to still be around.
pub struct DeferredNotifyDrainer {
    home_dir: PathBuf,
}

impl DeferredNotifyDrainer {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    /// One sweep. Returns how many notices were delivered (merged messages
    /// count once per underlying notice).
    pub async fn tick(&self) -> usize {
        let now = Utc::now();
        let due = take_due(&self.home_dir, now);
        if due.is_empty() {
            return 0;
        }
        let http = reqwest::Client::new();
        let mut delivered = 0usize;

        // Plain notices → one merged message per destination.
        for batch in merge_plain(&due) {
            let Some(token) =
                crate::goal_notify::channel_token(&self.home_dir, &batch.agent_id, &batch.channel).await
            else {
                warn!(
                    channel = %batch.channel,
                    count = batch.notify_types.len(),
                    "notify-governance: 延後通知找不到 bot token，未能投遞"
                );
                continue;
            };
            let ok = crate::goal_notify::send_plain_text(
                &self.home_dir,
                &http,
                &batch.channel,
                &token,
                &batch.chat_id,
                &batch.text,
            )
            .await;
            if ok {
                delivered += batch.notify_types.len();
                for (t, lvl) in &batch.notify_types {
                    crate::notify_stats::record_push(&self.home_dir, t, *lvl, None);
                }
            } else {
                warn!(
                    channel = %batch.channel,
                    count = batch.notify_types.len(),
                    "notify-governance: 延後通知投遞失敗（本批已離開佇列，不再重試）"
                );
            }
        }

        // Decision cards → individually, buttons re-rendered.
        for n in due.iter().filter(|n| n.kind == NoticeKind::Decision) {
            if self.deliver_decision(&http, n).await {
                delivered += 1;
            }
        }

        delivered
    }

    /// Re-render and push one deferred decision card.
    ///
    /// Known limitation, stated plainly: a decision settled *during* the quiet
    /// window is still delivered here, because there is no cross-store
    /// "is this still pending" predicate spanning all five decision sources.
    /// Pressing such a card fails closed against its owning store with a
    /// "此決定已處理" style refusal, so the cost is one stale card, never a
    /// double-apply.
    async fn deliver_decision(&self, http: &reqwest::Client, n: &DeferredNotice) -> bool {
        let (Some(src_token), Some(decision_id)) = (n.decision_source.as_deref(), n.decision_id.as_deref())
        else {
            warn!("notify-governance: 延後的決定卡缺少來源或 id，略過");
            return false;
        };
        let Some(source) = crate::decision_action::DecisionSource::from_token(src_token) else {
            warn!(token = %src_token, "notify-governance: 延後的決定卡來源不明，略過");
            return false;
        };
        let Some(token) =
            crate::goal_notify::channel_token(&self.home_dir, &n.agent_id, &n.channel).await
        else {
            warn!(channel = %n.channel, "notify-governance: 延後的決定卡找不到 bot token，未能投遞");
            return false;
        };
        let card = crate::decision_notify::DecisionCard {
            source,
            decision_id,
            body: &n.text,
            link: n.link.as_deref(),
            no_button_hint: n
                .no_button_hint
                .as_deref()
                .unwrap_or("請至儀表板處理。"),
        };
        crate::decision_notify::deliver_now(
            &self.home_dir,
            http,
            &n.channel,
            &token,
            &n.chat_id,
            &card,
        )
        .await
    }

    /// Long-running task: tick on a fixed interval until cancelled.
    pub async fn run(self: std::sync::Arc<Self>, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let n = self.tick().await;
            if n > 0 {
                debug!(delivered = n, "notify-governance: 已投遞延後通知");
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_time(t(h, mi))
    }

    // ── parsing ──────────────────────────────────────────────────

    #[test]
    fn parses_a_wrapping_window() {
        let w = QuietWindow::parse("22:00-08:00").unwrap();
        assert!(w.contains(t(23, 0)));
        assert!(w.contains(t(3, 30)));
        assert!(w.contains(t(22, 0)), "start is inclusive");
        assert!(!w.contains(t(8, 0)), "end is exclusive — 08:00 delivers");
        assert!(!w.contains(t(12, 0)));
    }

    #[test]
    fn parses_a_same_day_window() {
        let w = QuietWindow::parse("09:00-17:30").unwrap();
        assert!(w.contains(t(9, 0)));
        assert!(w.contains(t(17, 29)));
        assert!(!w.contains(t(17, 30)));
        assert!(!w.contains(t(8, 59)));
        assert!(!w.contains(t(23, 0)));
    }

    #[test]
    fn accepts_single_digit_hours_and_surrounding_space() {
        assert_eq!(
            QuietWindow::parse("  9:05 - 17:00 "),
            Some(QuietWindow { start: t(9, 5), end: t(17, 0) })
        );
    }

    #[test]
    fn malformed_windows_fail_open_to_none() {
        // Every one of these must mean "no quiet hours", never "mute all".
        for bad in [
            "", "   ", "22:00", "22:00-", "-08:00", "22-08", "22:00-08:00-09:00",
            "25:00-08:00", "22:61-08:00", "abc-def", "22:00–08:00", // en-dash
            "22:00_08:00", "-", "::",
        ] {
            assert_eq!(QuietWindow::parse(bad), None, "must fail open: {bad:?}");
        }
    }

    #[test]
    fn degenerate_window_is_treated_as_off() {
        // "mute forever" is never what someone means to type; the safe read
        // of an ambiguous config is no suppression at all.
        assert_eq!(QuietWindow::parse("00:00-00:00"), None);
        assert_eq!(QuietWindow::parse("22:00-22:00"), None);
    }

    #[test]
    fn display_round_trips() {
        assert_eq!(QuietWindow::parse("22:00-08:00").unwrap().to_display(), "22:00-08:00");
        assert_eq!(QuietWindow::parse("9:05-17:00").unwrap().to_display(), "09:05-17:00");
    }

    // ── next_end_after ───────────────────────────────────────────

    #[test]
    fn wrapping_window_ends_tomorrow_morning_when_it_is_late_tonight() {
        let w = QuietWindow::parse("22:00-08:00").unwrap();
        let now = dt(2026, 8, 11, 23, 30);
        assert_eq!(w.next_end_after(now), dt(2026, 8, 12, 8, 0));
    }

    #[test]
    fn wrapping_window_ends_this_morning_when_it_is_the_small_hours() {
        let w = QuietWindow::parse("22:00-08:00").unwrap();
        let now = dt(2026, 8, 12, 3, 0);
        assert_eq!(w.next_end_after(now), dt(2026, 8, 12, 8, 0));
    }

    #[test]
    fn same_day_window_ends_the_same_day() {
        let w = QuietWindow::parse("09:00-17:00").unwrap();
        assert_eq!(w.next_end_after(dt(2026, 8, 11, 10, 0)), dt(2026, 8, 11, 17, 0));
    }

    #[test]
    fn month_and_year_rollover_are_handled() {
        let w = QuietWindow::parse("22:00-08:00").unwrap();
        assert_eq!(w.next_end_after(dt(2026, 12, 31, 23, 0)), dt(2027, 1, 1, 8, 0));
        assert_eq!(w.next_end_after(dt(2026, 1, 31, 23, 0)), dt(2026, 2, 1, 8, 0));
    }

    // ── the gate ─────────────────────────────────────────────────

    #[test]
    fn l3_is_never_suppressed() {
        let w = QuietWindow::parse("22:00-08:00");
        assert_eq!(gate(NotifyLevel::Act, w, dt(2026, 8, 11, 3, 0)), GateDecision::Deliver);
    }

    #[test]
    fn l1_and_l2_are_deferred_inside_the_window() {
        let w = QuietWindow::parse("22:00-08:00");
        let now = dt(2026, 8, 11, 3, 0);
        let expected = GateDecision::DeferUntil(dt(2026, 8, 11, 8, 0));
        assert_eq!(gate(NotifyLevel::Fyi, w, now), expected);
        assert_eq!(gate(NotifyLevel::Confirm, w, now), expected);
    }

    #[test]
    fn everything_delivers_outside_the_window() {
        let w = QuietWindow::parse("22:00-08:00");
        let now = dt(2026, 8, 11, 12, 0);
        for lvl in [NotifyLevel::Fyi, NotifyLevel::Confirm, NotifyLevel::Act] {
            assert_eq!(gate(lvl, w, now), GateDecision::Deliver);
        }
    }

    #[test]
    fn no_window_means_no_suppression_at_any_level() {
        for lvl in [NotifyLevel::Fyi, NotifyLevel::Confirm, NotifyLevel::Act] {
            assert_eq!(gate(lvl, None, dt(2026, 8, 11, 3, 0)), GateDecision::Deliver);
        }
    }

    #[test]
    fn boundary_minutes_behave_as_documented() {
        let w = QuietWindow::parse("22:00-08:00");
        // 21:59 delivers, 22:00 defers, 07:59 defers, 08:00 delivers.
        assert_eq!(gate(NotifyLevel::Fyi, w, dt(2026, 8, 11, 21, 59)), GateDecision::Deliver);
        assert!(matches!(
            gate(NotifyLevel::Fyi, w, dt(2026, 8, 11, 22, 0)),
            GateDecision::DeferUntil(_)
        ));
        assert!(matches!(
            gate(NotifyLevel::Fyi, w, dt(2026, 8, 11, 7, 59)),
            GateDecision::DeferUntil(_)
        ));
        assert_eq!(gate(NotifyLevel::Fyi, w, dt(2026, 8, 11, 8, 0)), GateDecision::Deliver);
    }

    // ── level vocabulary ─────────────────────────────────────────

    #[test]
    fn level_tokens_round_trip_and_reject_junk() {
        for lvl in [NotifyLevel::Fyi, NotifyLevel::Confirm, NotifyLevel::Act] {
            assert_eq!(NotifyLevel::from_str_token(lvl.as_str()), Some(lvl));
        }
        assert_eq!(NotifyLevel::from_str_token("L4"), None);
        assert_eq!(NotifyLevel::from_str_token(""), None);
        assert_eq!(NotifyLevel::from_str_token("act"), None);
    }

    #[test]
    fn only_act_is_unsuppressible_and_the_ladder_is_ordered() {
        assert!(NotifyLevel::Fyi.is_suppressible());
        assert!(NotifyLevel::Confirm.is_suppressible());
        assert!(!NotifyLevel::Act.is_suppressible());
        assert!(NotifyLevel::Fyi < NotifyLevel::Confirm);
        assert!(NotifyLevel::Confirm < NotifyLevel::Act);
    }

    // ── timezone handling ────────────────────────────────────────

    #[test]
    fn named_timezone_is_used_when_valid() {
        let tz = NotifyTz::resolve("Asia/Taipei");
        assert_eq!(tz, NotifyTz::Named(chrono_tz::Asia::Taipei));
        // 2026-08-11T18:00Z is 02:00 next day in Taipei (UTC+8).
        let utc = Utc.with_ymd_and_hms(2026, 8, 11, 18, 0, 0).unwrap();
        assert_eq!(tz.local_naive(utc), dt(2026, 8, 12, 2, 0));
    }

    #[test]
    fn invalid_or_blank_timezone_falls_back_to_system() {
        assert_eq!(NotifyTz::resolve(""), NotifyTz::System);
        assert_eq!(NotifyTz::resolve("   "), NotifyTz::System);
        assert_eq!(NotifyTz::resolve("Mars/Olympus"), NotifyTz::System);
    }

    #[test]
    fn named_timezone_round_trips_local_to_utc() {
        let tz = NotifyTz::Named(chrono_tz::Asia::Taipei);
        let utc = tz.to_utc(dt(2026, 8, 12, 8, 0));
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0).unwrap());
    }

    #[test]
    fn dst_spring_forward_gap_still_resolves() {
        // America/New_York 2026-03-08: 02:00-03:00 local does not exist.
        let tz = NotifyTz::Named(chrono_tz::America::New_York);
        let utc = tz.to_utc(dt(2026, 3, 8, 2, 30));
        // Must land on a real instant just after the gap, not panic.
        let back = utc.with_timezone(&chrono_tz::America::New_York).naive_local();
        assert!(back >= dt(2026, 3, 8, 3, 0), "resolved to {back}");
    }

    #[test]
    fn policy_decide_returns_a_utc_deadline() {
        let policy = QuietPolicy {
            window: QuietWindow::parse("22:00-08:00"),
            tz: NotifyTz::Named(chrono_tz::Asia::Taipei),
        };
        // 2026-08-11T18:00Z = 02:00 Taipei on the 12th → defer to 08:00
        // Taipei on the 12th = 2026-08-12T00:00Z.
        let now = Utc.with_ymd_and_hms(2026, 8, 11, 18, 0, 0).unwrap();
        assert_eq!(
            policy.decide(NotifyLevel::Fyi, now),
            Some(Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0).unwrap())
        );
        assert_eq!(policy.decide(NotifyLevel::Act, now), None);
    }

    #[test]
    fn suppression_note_states_what_is_held_and_what_is_not() {
        let policy = QuietPolicy {
            window: QuietWindow::parse("22:00-08:00"),
            tz: NotifyTz::System,
        };
        let note = policy.suppression_note_zh().unwrap();
        assert!(note.contains("22:00-08:00"));
        assert!(note.contains("延後"));
        assert!(note.contains("照常即時送達"), "the exemption must be stated, not implied");
        assert!(QuietPolicy::none().suppression_note_zh().is_none());
    }

    // ── policy loading ───────────────────────────────────────────

    fn write_agent(home: &Path, agent: &str, body: &str) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
    }

    #[test]
    fn agent_policy_reads_window_and_timezone() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "kiki",
            "[proactive]\nquiet_hours = \"22:00-08:00\"\ntimezone = \"Asia/Taipei\"\n",
        );
        let p = load_agent_policy(dir.path(), "kiki");
        assert_eq!(p.window, QuietWindow::parse("22:00-08:00"));
        assert_eq!(p.tz, NotifyTz::Named(chrono_tz::Asia::Taipei));
    }

    #[test]
    fn missing_agent_or_section_is_no_quiet_hours() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_agent_policy(dir.path(), "ghost").window, None);
        write_agent(dir.path(), "kiki", "[model]\npreferred = \"x\"\n");
        assert_eq!(load_agent_policy(dir.path(), "kiki").window, None);
    }

    #[test]
    fn legacy_numeric_quiet_hours_are_deliberately_ignored() {
        // 23-8 is the ProactiveConfig default on EVERY agent; honouring it
        // here would silently mute L1/L2 on every existing install.
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "kiki",
            "[proactive]\nquiet_hours_start = 23\nquiet_hours_end = 8\n",
        );
        assert_eq!(load_agent_policy(dir.path(), "kiki").window, None);
    }

    #[test]
    fn malformed_agent_window_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "kiki", "[proactive]\nquiet_hours = \"晚上到早上\"\n");
        assert_eq!(load_agent_policy(dir.path(), "kiki").window, None);
    }

    #[test]
    fn global_window_is_the_fallback_and_the_agent_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[notify]\nquiet_hours = \"01:00-05:00\"\n",
        )
        .unwrap();
        // No agent value ⇒ global applies.
        assert_eq!(
            load_agent_policy(dir.path(), "ghost").window,
            QuietWindow::parse("01:00-05:00")
        );
        // Agent value ⇒ agent wins.
        write_agent(dir.path(), "kiki", "[proactive]\nquiet_hours = \"22:00-08:00\"\n");
        assert_eq!(
            load_agent_policy(dir.path(), "kiki").window,
            QuietWindow::parse("22:00-08:00")
        );
    }

    // ── agent_raw_quiet_hours (W2-8, the edit-form's own prefill) ─────────

    #[test]
    fn raw_quiet_hours_returns_the_agents_own_value_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "kiki", "[proactive]\nquiet_hours = \"  22:00-08:00 \"\n");
        assert_eq!(agent_raw_quiet_hours(dir.path(), "kiki"), "22:00-08:00");
    }

    #[test]
    fn raw_quiet_hours_never_falls_back_to_the_global_default() {
        // Unlike `load_agent_policy`, an agent with no value of its own must
        // read back empty — never the deployment-wide fallback — so an
        // untouched edit-form save can't silently pin the global default.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[notify]\nquiet_hours = \"01:00-05:00\"\n",
        )
        .unwrap();
        write_agent(dir.path(), "kiki", "[model]\npreferred = \"x\"\n");
        assert_eq!(agent_raw_quiet_hours(dir.path(), "kiki"), "");
        // Sanity: the effective policy DOES pick up the fallback.
        assert_eq!(
            load_agent_policy(dir.path(), "kiki").window,
            QuietWindow::parse("01:00-05:00")
        );
    }

    #[test]
    fn raw_quiet_hours_is_returned_even_when_malformed() {
        // Raw read, not a validating one — the write path is where malformed
        // input is rejected. An operator re-opening the form must see
        // exactly what's on disk, not a silently-blanked field.
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "kiki", "[proactive]\nquiet_hours = \"晚上到早上\"\n");
        assert_eq!(agent_raw_quiet_hours(dir.path(), "kiki"), "晚上到早上");
    }

    #[test]
    fn raw_quiet_hours_is_empty_for_a_missing_agent_or_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(agent_raw_quiet_hours(dir.path(), "ghost"), "");
    }

    #[test]
    fn malformed_global_window_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[notify]\nquiet_hours = \"nope\"\n").unwrap();
        assert_eq!(load_global_window(dir.path()), None);
        std::fs::write(dir.path().join("config.toml"), "not [ valid toml").unwrap();
        assert_eq!(load_global_window(dir.path()), None);
    }

    // ── queue ────────────────────────────────────────────────────

    fn notice(id: &str, queued: DateTime<Utc>, due: DateTime<Utc>, text: &str) -> DeferredNotice {
        DeferredNotice {
            id: id.to_string(),
            agent_id: "kiki".into(),
            channel: "telegram".into(),
            chat_id: "555".into(),
            level: "L1".into(),
            notify_type: "evolution.stagnation".into(),
            queued_at: queued.to_rfc3339(),
            deliver_after: due.to_rfc3339(),
            kind: NoticeKind::Plain,
            text: text.to_string(),
            link: None,
            no_button_hint: None,
            decision_source: None,
            decision_id: None,
        }
    }

    #[test]
    fn enqueue_then_take_due_returns_only_ripe_notices() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        assert!(enqueue(dir.path(), notice("a", now, now - chrono::Duration::minutes(1), "ripe")));
        assert!(enqueue(dir.path(), notice("b", now, now + chrono::Duration::hours(4), "later")));

        let due = take_due(dir.path(), now);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "a");

        // "b" is still queued and comes out when its time arrives.
        let later = take_due(dir.path(), now + chrono::Duration::hours(5));
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].id, "b");
        assert!(take_due(dir.path(), now + chrono::Duration::hours(6)).is_empty());
    }

    #[test]
    fn taking_due_notices_is_idempotent() {
        // A second drainer tick must not re-deliver what the first took.
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        enqueue(dir.path(), notice("a", now, now, "x"));
        assert_eq!(take_due(dir.path(), now).len(), 1);
        assert_eq!(take_due(dir.path(), now).len(), 0);
    }

    #[test]
    fn missing_queue_file_is_an_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take_due(dir.path(), Utc::now()).is_empty());
    }

    #[test]
    fn corrupt_lines_do_not_wedge_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        enqueue(dir.path(), notice("a", now, now, "x"));
        // Append junk the way a partial write would.
        let path = queue_path(dir.path());
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("{not json\n\n");
        std::fs::write(&path, body).unwrap();
        assert_eq!(take_due(dir.path(), now).len(), 1);
    }

    #[test]
    fn prune_drops_stale_and_caps_the_queue() {
        let now = Utc::now();
        let fresh = notice("f", now, now, "fresh");
        let stale = notice("s", now - chrono::Duration::hours(MAX_QUEUE_AGE_HOURS + 1), now, "stale");
        let (kept, dropped_stale, overflow) = prune(vec![fresh, stale], now);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "f");
        assert_eq!(dropped_stale, 1);
        assert_eq!(overflow, 0);

        // Overflow keeps the newest.
        let many: Vec<_> = (0..MAX_QUEUE_ENTRIES + 5)
            .map(|i| {
                notice(
                    &format!("n{i}"),
                    now - chrono::Duration::seconds((MAX_QUEUE_ENTRIES + 5 - i) as i64),
                    now,
                    "x",
                )
            })
            .collect();
        let (kept, _, overflow) = prune(many, now);
        assert_eq!(kept.len(), MAX_QUEUE_ENTRIES);
        assert_eq!(overflow, 5);
        assert_eq!(kept.last().unwrap().id, format!("n{}", MAX_QUEUE_ENTRIES + 4));
    }

    #[test]
    fn a_notice_with_an_unparseable_timestamp_is_pruned_not_immortal() {
        let now = Utc::now();
        let mut bad = notice("bad", now, now, "x");
        bad.queued_at = "yesterday-ish".into();
        let (kept, stale, _) = prune(vec![bad], now);
        assert!(kept.is_empty());
        assert_eq!(stale, 1);
    }

    // ── merging ──────────────────────────────────────────────────

    #[test]
    fn one_notice_renders_without_numbering() {
        let now = Utc::now();
        let batches = merge_plain(&[notice("a", now, now, "預算已恢復")]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].text, "🌙 勿擾時段收到 1 則通知：\n預算已恢復");
    }

    #[test]
    fn same_destination_merges_into_one_numbered_message() {
        let now = Utc::now();
        let batches = merge_plain(&[
            notice("a", now, now, "第一件"),
            notice("b", now, now, "第二件"),
        ]);
        assert_eq!(batches.len(), 1, "one message per destination (P4-6)");
        assert!(batches[0].text.starts_with("🌙 勿擾時段收到 2 則通知："));
        assert!(batches[0].text.contains("1. 第一件"));
        assert!(batches[0].text.contains("2. 第二件"));
        assert_eq!(batches[0].notify_types.len(), 2);
    }

    #[test]
    fn a_merged_batch_keeps_each_notices_original_level() {
        let now = Utc::now();
        let mut confirm = notice("b", now, now, "待確認的");
        confirm.level = "L2".into();
        confirm.notify_type = "decision.autopilot".into();
        let mut junk = notice("c", now, now, "壞掉的等級");
        junk.level = "L9".into();

        let batches = merge_plain(&[notice("a", now, now, "週知的"), confirm, junk]);
        let types = &batches[0].notify_types;
        assert_eq!(types.len(), 3);
        assert!(types.contains(&("evolution.stagnation".to_string(), NotifyLevel::Fyi)));
        assert!(
            types.contains(&("decision.autopilot".to_string(), NotifyLevel::Confirm)),
            "a merged L2 must not be filed as an L1: {types:?}"
        );
        // An unparseable level is counted at the lowest level, not dropped.
        assert_eq!(
            types.iter().filter(|(_, l)| *l == NotifyLevel::Fyi).count(),
            2
        );
    }

    #[test]
    fn different_destinations_stay_separate() {
        let now = Utc::now();
        let mut b = notice("b", now, now, "slack 的");
        b.channel = "slack".into();
        b.chat_id = "C1".into();
        let batches = merge_plain(&[notice("a", now, now, "tg 的"), b]);
        assert_eq!(batches.len(), 2);
    }

    #[test]
    fn merged_items_are_cjk_safe_truncated_and_capped() {
        let now = Utc::now();
        let long = "中".repeat(500);
        let items: Vec<_> = (0..MERGE_ITEM_CAP + 3)
            .map(|i| notice(&format!("n{i}"), now, now, &long))
            .collect();
        let batches = merge_plain(&items);
        let text = &batches[0].text;
        // No panic (the whole point of truncate_chars) and the overflow is
        // stated rather than silently dropped.
        assert!(text.contains(&format!("…另有 3 則")));
        assert!(text.contains(&format!("{}. ", MERGE_ITEM_CAP)));
        assert!(!text.contains(&format!("{}. ", MERGE_ITEM_CAP + 1)));
    }

    #[test]
    fn newlines_inside_an_item_are_collapsed() {
        let now = Utc::now();
        let batches = merge_plain(&[
            notice("a", now, now, "第一行\n第二行"),
            notice("b", now, now, "另一則"),
        ]);
        assert!(batches[0].text.contains("1. 第一行 第二行"));
    }

    #[test]
    fn decision_notices_are_never_merged() {
        let now = Utc::now();
        let mut d = notice("d", now, now, "決定卡");
        d.kind = NoticeKind::Decision;
        let batches = merge_plain(&[d, notice("a", now, now, "普通")]);
        assert_eq!(batches.len(), 1);
        assert!(!batches[0].text.contains("決定卡"));
    }

    #[tokio::test]
    async fn drainer_tick_on_an_empty_queue_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let drainer = DeferredNotifyDrainer::new(dir.path().to_path_buf());
        assert_eq!(drainer.tick().await, 0);
    }

    // ── end-to-end against the real clock ────────────────────────
    //
    // These build the window from `now` rather than hard-coding hours, so
    // they assert the same thing at 03:00 and at 15:00 (a fixed "22:00-08:00"
    // string would only exercise the deferral branch overnight).

    /// A window guaranteed to contain the current local time.
    pub(crate) fn window_covering_now() -> String {
        let now = chrono::Local::now().naive_local();
        let start = now - chrono::Duration::hours(1);
        let end = now + chrono::Duration::hours(1);
        format!("{}-{}", start.format("%H:%M"), end.format("%H:%M"))
    }

    /// A window guaranteed NOT to contain the current local time.
    pub(crate) fn window_excluding_now() -> String {
        let now = chrono::Local::now().naive_local();
        let start = now + chrono::Duration::hours(2);
        let end = now + chrono::Duration::hours(4);
        format!("{}-{}", start.format("%H:%M"), end.format("%H:%M"))
    }

    #[test]
    fn a_live_window_defers_l1_and_l2_but_never_l3() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "kiki",
            &format!("[proactive]\nquiet_hours = \"{}\"\n", window_covering_now()),
        );
        let policy = load_agent_policy(dir.path(), "kiki");
        let now = Utc::now();
        assert!(policy.decide(NotifyLevel::Fyi, now).is_some());
        assert!(policy.decide(NotifyLevel::Confirm, now).is_some());
        assert_eq!(policy.decide(NotifyLevel::Act, now), None);
    }

    #[test]
    fn a_window_that_has_not_started_defers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "kiki",
            &format!("[proactive]\nquiet_hours = \"{}\"\n", window_excluding_now()),
        );
        let policy = load_agent_policy(dir.path(), "kiki");
        let now = Utc::now();
        for lvl in [NotifyLevel::Fyi, NotifyLevel::Confirm, NotifyLevel::Act] {
            assert_eq!(policy.decide(lvl, now), None);
        }
    }

    #[test]
    fn a_deferral_deadline_is_always_in_the_future_and_within_a_day() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "kiki",
            &format!("[proactive]\nquiet_hours = \"{}\"\n", window_covering_now()),
        );
        let now = Utc::now();
        let until = load_agent_policy(dir.path(), "kiki")
            .decide(NotifyLevel::Fyi, now)
            .unwrap();
        assert!(until > now, "a deferral that is already due would deliver instantly");
        assert!(
            until - now <= chrono::Duration::hours(25),
            "no quiet window can hold a notification for more than a day"
        );
    }
}
