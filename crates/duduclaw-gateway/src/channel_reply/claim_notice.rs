//! Workspace action-claim check on the channel-reply path.
//!
//! A reply that says an email was sent, a calendar event / task was created,
//! or a spreadsheet / Google Doc was written is checked against this
//! employee's tool-call rows in this session (this turn or the last 30
//! minutes — `duduclaw_security::action_claim_verifier`).
//!
//! Order ([`check_reply`] → [`run_check`]): extract claims (pure) → none ⇒
//! done → decide the audience FIRST ([`staff_audience`]):
//! - anyone else (visitors, unverified senders, group chats, internal or
//!   unknown origins): only a process-wide counter and at most one summary
//!   log line per minute (count only). No lookup, no blocking task, no lock,
//!   no audit row, no autopilot event, no notice;
//! - company staff: one of [`CLAIM_CHECK_PERMITS`] process-wide permits is
//!   tried (never waited for); none free ⇒ the check is skipped for this
//!   reply (no notice, nothing recorded, counted). An admitted check runs ONE
//!   `spawn_blocking` task that owns the permit until it really ends: the
//!   evidence read (log lock wait bounded by [`CLAIM_CHECK_DEADLINE`]), the
//!   evidence sent to the reply, then — only if the reply is still waiting —
//!   the rate-limiter decision and, for `Audit`, the `tool_hallucination`
//!   audit row (lock wait bounded by [`CLAIM_CHECK_DEADLINE`]) + log line +
//!   autopilot event. At most one row per (employee, claim family) per
//!   [`AUDIT_WINDOW`], and at most one row per key being written at a time;
//!   occurrences inside the window or during that write are counted and
//!   reported on the next row (`suppressed_since_last_row`).
//!
//! The reply waits at most [`CLAIM_CHECK_DEADLINE`] for the evidence. Every
//! skip or failure (no permit, no answer in time, lock timeout, read error,
//! task failure, audit write failure, limiter table full) means no notice
//! for that reply and nothing recorded for it, and is counted in ONE
//! process-wide aggregator ([`SkipLog`]) with at most one summary warning
//! per minute.
//!
//! `config.toml [action_claims] mode = "notice" | "shadow"` (default
//! `notice`). In `notice` mode ONE short notice paragraph is appended to the
//! reply before it is saved to the session, so the model also sees it next
//! turn — but only for company staff (fail closed). The notice names no
//! tools, files or ids. Recording is the same in both modes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use duduclaw_security::action_claim_verifier::{
    ActionClaim, ClaimType, UnbackedClaim, UnbackedKind, WORKSPACE_FAMILIES, extract_workspace_claims,
    verify_claims_in_session,
};
use tokio::sync::{Semaphore, oneshot};
use tracing::{debug, warn};

/// Most action kinds one notice names.
const MAX_NOTICE_KINDS: usize = 3;

/// Hard bound on each wait of a staff check: the tool-call log lock wait,
/// the reply's wait for the evidence, and the audit log lock wait.
const CLAIM_CHECK_DEADLINE: Duration = Duration::from_secs(2);

/// Most staff checks whose blocking work may be outstanding at once
/// (process-wide). A reply that finds none free is not checked.
pub(crate) const CLAIM_CHECK_PERMITS: usize = 4;

/// At most one audit row + autopilot event per (employee, claim family) in
/// this window (staff audiences only).
pub(crate) const AUDIT_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Most (employee, claim family) keys the rate limiter keeps. Entries inside
/// their window, with a pending suppressed count or with an audit write in
/// flight are never dropped; a new
/// key on a table still full after reclaiming the others is not audited.
pub(crate) const LIMITER_MAX_KEYS: usize = 1024;

/// At most one non-staff summary log line per this interval.
const NON_STAFF_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// At most one skip / failure summary log line per this interval.
const SKIP_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// `config.toml [action_claims] mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimCheckMode {
    /// Record, and append a staff-only notice (default).
    Notice,
    /// Record only; the reply is never changed.
    Shadow,
}

/// The "unknown mode" warning is written once per process.
static UNKNOWN_MODE_WARNED: AtomicBool = AtomicBool::new(false);

impl ClaimCheckMode {
    /// Absent ⇒ `notice`. `notice` / `shadow` (trimmed, ASCII
    /// case-insensitive) select that mode; any other value or a non-string
    /// ⇒ `shadow` (record only), with one warning per process.
    pub(crate) fn from_value(value: Option<&toml::Value>) -> Self {
        let Some(value) = value else {
            return Self::Notice;
        };
        match value.as_str().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("notice") => Self::Notice,
            Some("shadow") => Self::Shadow,
            _ => {
                if !UNKNOWN_MODE_WARNED.swap(true, Ordering::Relaxed) {
                    warn!("config.toml [action_claims] mode is not \"notice\" or \"shadow\"; using \"shadow\" (record only)");
                }
                Self::Shadow
            }
        }
    }

    /// Read from `<home>/config.toml` on every call (hot reload). A missing
    /// file or section ⇒ `notice`; a file that cannot be read or parsed ⇒
    /// `shadow` (no reply change while the setting cannot be told).
    pub(crate) fn from_home(home: &Path) -> Self {
        let content = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::Notice,
            Err(_) => return Self::Shadow,
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::Shadow;
        };
        match table.get("action_claims") {
            None => Self::Notice,
            Some(toml::Value::Table(t)) => Self::from_value(t.get("mode")),
            Some(other) => Self::from_value(Some(other)),
        }
    }
}

/// Whether the person reading this reply is company staff. Fail closed:
/// - Odoo Live Chat (public visitor) sessions, website-widget visitors,
///   gateway-internal sessions and unknown origins ⇒ never;
/// - WebChat ⇒ only a signed-in dashboard account;
/// - a messaging app ⇒ only a 1:1 chat whose sender's verified identity maps
///   to an admin or to an account bound to this employee (the thinking-summary
///   rule); group chats never, since others in the group may be outsiders.
pub(crate) fn staff_audience(
    home: &Path,
    conversation: &str,
    session_id: &str,
    user_id: &str,
    agent_id: &str,
) -> bool {
    use crate::session_origin::OriginKind;
    if crate::odoo_livechat::is_public_visitor_session(conversation)
        || crate::odoo_livechat::is_public_visitor_session(session_id)
    {
        return false;
    }
    let origin = crate::session_origin::origin_for_current_turn(conversation);
    match origin.kind() {
        OriginKind::WebchatUser => origin.owner_user_id().is_some_and(|owner| {
            !crate::webchat::is_widget_visitor(owner)
                && !crate::ccr_runtime::is_placeholder_principal(owner)
        }),
        OriginKind::Channel => {
            let channel = conversation.split(':').next().unwrap_or("");
            channel != "webchat"
                && duduclaw_core::is_private_session(conversation, user_id)
                && crate::thinking::channel_policy::sender_is_internal(home, channel, user_id, agent_id)
        }
        OriginKind::WebchatVisitor
        | OriginKind::PublicVisitor
        | OriginKind::Internal
        | OriginKind::Unknown => false,
    }
}

/// Language bucket, chosen the way `failure_notice` chooses it.
fn language_bucket(language: Option<&str>) -> &'static str {
    let language = language.unwrap_or("zh-TW");
    if matches!(language, "en" | "en_US") || language.starts_with("en-") {
        "en"
    } else if matches!(language, "ja" | "ja-JP" | "ja_JP") {
        "ja-JP"
    } else if matches!(language, "zh-CN" | "zh_CN") {
        "zh-CN"
    } else {
        "zh-TW"
    }
}

fn action_label(claim_type: &ClaimType, lang: &str) -> &'static str {
    match (lang, claim_type) {
        ("en", ClaimType::MailSent) => "sending the email",
        ("en", ClaimType::CalendarEventUpdated) => "editing the calendar event",
        ("en", ClaimType::CalendarEventCreated) => "creating the calendar event",
        ("en", ClaimType::SheetWritten) => "writing to the spreadsheet",
        ("en", ClaimType::DocWritten) => "writing to the document",
        ("en", _) => "creating the task",
        ("ja-JP", ClaimType::MailSent) => "メール送信",
        ("ja-JP", ClaimType::CalendarEventUpdated) => "カレンダーの予定変更",
        ("ja-JP", ClaimType::CalendarEventCreated) => "カレンダーの予定作成",
        ("ja-JP", ClaimType::SheetWritten) => "スプレッドシートへの書き込み",
        ("ja-JP", ClaimType::DocWritten) => "ドキュメントへの書き込み",
        ("ja-JP", _) => "タスク作成",
        ("zh-CN", ClaimType::MailSent) => "发送邮件",
        ("zh-CN", ClaimType::CalendarEventUpdated) => "修改日历活动",
        ("zh-CN", ClaimType::CalendarEventCreated) => "创建日历活动",
        ("zh-CN", ClaimType::SheetWritten) => "写入电子表格",
        ("zh-CN", ClaimType::DocWritten) => "写入文档",
        ("zh-CN", _) => "创建任务",
        (_, ClaimType::MailSent) => "寄信",
        (_, ClaimType::CalendarEventUpdated) => "修改行事曆活動",
        (_, ClaimType::CalendarEventCreated) => "建立行事曆活動",
        (_, ClaimType::SheetWritten) => "寫入試算表",
        (_, ClaimType::DocWritten) => "寫入文件",
        (_, _) => "建立任務",
    }
}

fn no_record_text(lang: &str, labels: &[&str]) -> String {
    match lang {
        "en" => format!(
            "(System note: this reply says the following was completed: {}. No matching success record was found in this conversation, so please check that it really happened.)",
            labels.join(", ")
        ),
        "ja-JP" => format!(
            "（システムからのお知らせ：この返信では「{}」が完了したとされていますが、この会話では対応する成功記録が見つかりません。本当に完了しているかご確認ください。）",
            labels.join("」、「")
        ),
        "zh-CN" => format!(
            "（系统提醒：这条回复提到已完成「{}」，但系统在这段对话中查不到对应的成功记录，请再确认是否真的完成。）",
            labels.join("」、「")
        ),
        _ => format!(
            "（系統提醒：這則回覆提到已完成「{}」，但系統在這段對話中查不到對應的成功紀錄，請再確認是否真的完成。）",
            labels.join("」、「")
        ),
    }
}

fn draft_only_text(lang: &str) -> &'static str {
    match lang {
        "en" => "(System note: this email is only a draft or is waiting for confirmation, and has not been sent. It will be sent only after an administrator confirms it in the dashboard.)",
        "ja-JP" => "（システムからのお知らせ：このメールはまだ下書きまたは確認待ちで、送信されていません。管理者がダッシュボードで確認した後に送信されます。）",
        "zh-CN" => "（系统提醒：这封邮件目前只是草稿或待确认，尚未发送；需要管理员在仪表板确认后才会发送。）",
        _ => "（系統提醒：這封信目前只是草稿或待確認，尚未寄出；需要管理員在儀表板確認後才會寄出。）",
    }
}

/// The notice for one reply, or `None`. Pure. `None` unless the mode is
/// `notice`, the reader is staff (`staff`), the session is not a public
/// visitor session, the reply is not blank, and at least one workspace claim
/// is unbacked. One paragraph, at most [`MAX_NOTICE_KINDS`] action kinds.
pub(crate) fn notice_for_reply(
    mode: ClaimCheckMode,
    staff: bool,
    session_id: &str,
    reply: &str,
    unbacked: &[UnbackedClaim],
    language: Option<&str>,
) -> Option<String> {
    if mode != ClaimCheckMode::Notice
        || !staff
        || crate::odoo_livechat::is_public_visitor_session(session_id)
        || reply.trim().is_empty()
    {
        return None;
    }
    let lang = language_bucket(language);
    let mut no_record: Vec<&'static str> = Vec::new();
    let mut draft_only = false;
    let mut kinds = 0;
    for family in WORKSPACE_FAMILIES.iter() {
        if kinds >= MAX_NOTICE_KINDS {
            break;
        }
        let Some(found) = unbacked.iter().find(|u| u.claim.claim_type == *family) else {
            continue;
        };
        kinds += 1;
        if *family == ClaimType::MailSent && found.kind == UnbackedKind::DraftOnly {
            draft_only = true;
        } else {
            no_record.push(action_label(family, lang));
        }
    }
    if kinds == 0 {
        return None;
    }
    let mut out = String::new();
    if !no_record.is_empty() {
        out.push_str(&no_record_text(lang, &no_record));
    }
    if draft_only {
        if lang == "en" && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(draft_only_text(lang));
    }
    Some(out)
}

/// Append `notice` as its own paragraph; a blank reply stays untouched.
pub(crate) fn append_notice(reply: String, notice: Option<String>) -> String {
    match notice {
        Some(notice) if !reply.trim().is_empty() => format!("{reply}\n\n{notice}"),
        _ => reply,
    }
}

/// What one unbacked workspace claim records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    /// Not a staff audience: the process-wide counter only (plus at most one
    /// count-only log line per minute). No lookup, no audit row, no
    /// autopilot event. Decided in the foreground; never spawns anything.
    CountOnly,
    /// Staff, but this (employee, family) was audited within
    /// [`AUDIT_WINDOW`] or its audit row is being written: counted, reported
    /// on the next audit row.
    Suppressed,
    /// Staff: `tool_hallucination` audit row, then (only once the row is
    /// written) log line + autopilot event + the limiter's window starts.
    Audit,
    /// Staff, a new key on a limiter table full of live entries: not
    /// audited, counted as `limiter_full` (outcome of [`record_unbacked`]).
    Overflow,
    /// Staff, `Audit` decided but the row could not be written: no
    /// suppression starts, counted as `audit_write_failed` (outcome of
    /// [`record_unbacked`]).
    WriteFailed,
}

/// The recording decision for one claim. Pure. `staff` is
/// [`staff_audience`]; `last_audit` is when this (employee, family) last got
/// an audit row in this process.
pub(crate) fn record_decision(staff: bool, last_audit: Option<Instant>, now: Instant) -> Record {
    if !staff {
        return Record::CountOnly;
    }
    match last_audit {
        Some(at) if now.saturating_duration_since(at) < AUDIT_WINDOW => Record::Suppressed,
        _ => Record::Audit,
    }
}

/// A reservation older than this is treated as released (its task never
/// resolved it, e.g. it panicked). Well above every bounded wait of the
/// write ([`CLAIM_CHECK_DEADLINE`]).
pub(crate) const RESERVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// The one audit write in flight for a key.
#[derive(Debug, Clone, Copy)]
struct Reservation {
    token: u64,
    since: Instant,
    /// The count the row reports; subtracted exactly on commit.
    reported: u64,
}

impl Reservation {
    fn live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) < RESERVATION_TIMEOUT
    }
}

#[derive(Debug, Clone, Copy)]
struct LimiterEntry {
    /// When the key's last row was written (`None`: never, the entry exists
    /// for a reservation or a pending count only).
    last_audit: Option<Instant>,
    suppressed: u64,
    writing: Option<Reservation>,
}

impl LimiterEntry {
    /// Never dropped: inside its window, a pending count, or a live
    /// reservation.
    fn live(&self, now: Instant) -> bool {
        self.suppressed > 0
            || self.last_audit.is_some_and(|at| now.saturating_duration_since(at) < AUDIT_WINDOW)
            || self.writing.is_some_and(|r| r.live(now))
    }
}

/// Proof of an `Audit` decision: the count to report on the row and the
/// reservation token that [`ClaimLimiter::commit`] / [`ClaimLimiter::release`]
/// must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuditTicket {
    pub(crate) reported: u64,
    token: u64,
}

/// [`ClaimLimiter::decide`]'s answer for one staff claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Write the row, then `commit` (written) or `release` (failed).
    Audit(AuditTicket),
    /// Inside the window or a row for this key is being written: counted.
    Suppressed,
    /// New key, table full of live entries: nothing changed.
    Overflow,
}

/// In-process rate limiter for staff audit rows, keyed by (employee, claim
/// family). Fixed memory: at most [`LIMITER_MAX_KEYS`] keys. An entry inside
/// its window, carrying a pending suppressed count or a live reservation is
/// never dropped; when the table is full, only the other entries are
/// reclaimed, and a new key that still finds no room is not audited
/// ([`Record::Overflow`]). Two-phase with a per-key reservation:
/// [`Self::decide`] never starts a window but reserves the key for ONE audit
/// write (further occurrences are suppressed while it is in flight);
/// [`Self::commit`] starts the window, only after the row was written, and
/// [`Self::release`] drops the reservation after a failed write. Only staff
/// checks reach it, so non-staff traffic never touches it.
#[derive(Debug, Default)]
pub(crate) struct ClaimLimiter {
    entries: HashMap<(String, ClaimType), LimiterEntry>,
    next_token: u64,
}

impl ClaimLimiter {
    /// Phase one, for one staff claim. `Suppressed`: the key is inside its
    /// window or has a live reservation, and its pending count is
    /// incremented. `Audit`: the key is now reserved (a stale reservation
    /// older than [`RESERVATION_TIMEOUT`] is replaced) and the ticket
    /// carries the pending count to report. `Overflow`: new key, table full
    /// of live entries; nothing changes, nothing is dropped.
    pub(crate) fn decide(&mut self, agent_id: &str, family: &ClaimType, now: Instant) -> Decision {
        let key = (agent_id.to_string(), family.clone());
        let token = self.next_token;
        if let Some(entry) = self.entries.get_mut(&key) {
            let in_flight = entry.writing.is_some_and(|r| r.live(now));
            if in_flight || record_decision(true, entry.last_audit, now) == Record::Suppressed {
                entry.suppressed = entry.suppressed.saturating_add(1);
                return Decision::Suppressed;
            }
            let reported = entry.suppressed;
            entry.writing = Some(Reservation { token, since: now, reported });
            self.next_token = token.wrapping_add(1);
            return Decision::Audit(AuditTicket { reported, token });
        }
        if !self.has_room(now) {
            return Decision::Overflow;
        }
        let writing = Some(Reservation { token, since: now, reported: 0 });
        self.entries.insert(key, LimiterEntry { last_audit: None, suppressed: 0, writing });
        self.next_token = token.wrapping_add(1);
        Decision::Audit(AuditTicket { reported: 0, token })
    }

    /// Phase two, only after the row for `ticket` was written: the window
    /// starts at `now`, exactly `ticket.reported` is subtracted (occurrences
    /// counted during the write stay pending for the next row) and the
    /// reservation ends. `false` (nothing changes) for a stale ticket: the
    /// reservation expired and the key was reserved again or released.
    pub(crate) fn commit(&mut self, agent_id: &str, family: &ClaimType, ticket: AuditTicket, now: Instant) -> bool {
        let key = (agent_id.to_string(), family.clone());
        let Some(entry) = self.entries.get_mut(&key) else {
            return false;
        };
        match entry.writing {
            Some(r) if r.token == ticket.token => {
                entry.last_audit = Some(now);
                entry.suppressed = entry.suppressed.saturating_sub(r.reported);
                entry.writing = None;
                true
            }
            _ => false,
        }
    }

    /// After a failed write for `ticket`: the reservation ends, the pending
    /// count is kept and no window starts. An entry left with nothing (never
    /// audited, no count) is removed. A stale ticket changes nothing.
    pub(crate) fn release(&mut self, agent_id: &str, family: &ClaimType, ticket: AuditTicket) {
        let key = (agent_id.to_string(), family.clone());
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        if entry.writing.is_some_and(|r| r.token == ticket.token) {
            entry.writing = None;
            if entry.last_audit.is_none() && entry.suppressed == 0 {
                self.entries.remove(&key);
            }
        }
    }

    /// Room for one more key, after reclaiming (only when full) the entries
    /// that are not live (window expired, zero count, no live reservation).
    fn has_room(&mut self, now: Instant) -> bool {
        if self.entries.len() >= LIMITER_MAX_KEYS {
            self.entries.retain(|_, entry| entry.live(now));
        }
        self.entries.len() < LIMITER_MAX_KEYS
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

static CLAIM_LIMITER: LazyLock<Mutex<ClaimLimiter>> =
    LazyLock::new(|| Mutex::new(ClaimLimiter::default()));

/// Process-wide admission bound for staff checks ([`CLAIM_CHECK_PERMITS`]).
static CLAIM_CHECK_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(CLAIM_CHECK_PERMITS)));

/// The process-wide skip / failure aggregator.
static SKIPS: SkipLog = SkipLog::new();

/// Workspace claims seen in non-staff conversations (process-wide). They are
/// not checked against the tool-call log.
static NON_STAFF_CLAIMS: AtomicU64 = AtomicU64::new(0);

/// When the last non-staff summary line was written.
static NON_STAFF_LAST_LOG: Mutex<Option<Instant>> = Mutex::new(None);

/// Count `n` non-staff claims; at most one summary line per
/// [`NON_STAFF_LOG_INTERVAL`], with the running total only (no reply text,
/// no ids).
fn count_non_staff(n: u64, now: Instant) {
    let total = NON_STAFF_CLAIMS.fetch_add(n, Ordering::Relaxed).saturating_add(n);
    let mut last = NON_STAFF_LAST_LOG.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous: Option<Instant> = *last;
    if previous.is_none_or(|at| now.saturating_duration_since(at) >= NON_STAFF_LOG_INTERVAL) {
        *last = Some(now);
        warn!(
            total,
            "{total} workspace claim(s) in non-staff conversations since start (counted only: not checked, not audited)"
        );
    }
}

/// Why a staff check gave no notice or recorded nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Skip {
    /// No free permit: the check did not run.
    Busy,
    /// The reply stopped waiting for the evidence ([`CLAIM_CHECK_DEADLINE`]).
    NoAnswer,
    /// The check task ended without an answer (panic / cancelled).
    TaskFailed,
    /// The tool-call log lock was not acquired in time.
    LockTimeout,
    /// The tool-call log could not be read.
    ReadError,
    /// The audit row could not be written (lock timeout or I/O error).
    AuditWriteFailed,
    /// The limiter table was full of live entries: a key was not audited.
    LimiterFull,
}

const SKIP_KINDS: usize = 7;

/// Process-wide counters for every skip / failure path, with at most one
/// summary warning per [`SKIP_LOG_INTERVAL`] (running totals only; no reply
/// text, no ids). There is no per-reply warning.
#[derive(Debug)]
pub(crate) struct SkipLog {
    counts: [AtomicU64; SKIP_KINDS],
    last_warn: Mutex<Option<Instant>>,
}

impl SkipLog {
    pub(crate) const fn new() -> Self {
        Self { counts: [const { AtomicU64::new(0) }; SKIP_KINDS], last_warn: Mutex::new(None) }
    }

    /// Running total for one kind.
    pub(crate) fn total(&self, skip: Skip) -> u64 {
        self.counts[skip as usize].load(Ordering::Relaxed)
    }

    /// Count one `skip`; `true` when this call wrote the summary warning.
    pub(crate) fn note(&self, skip: Skip, now: Instant) -> bool {
        self.counts[skip as usize].fetch_add(1, Ordering::Relaxed);
        {
            let mut last = self.last_warn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.is_some_and(|at| now.saturating_duration_since(at) < SKIP_LOG_INTERVAL) {
                return false;
            }
            *last = Some(now);
        }
        warn!(
            busy = self.total(Skip::Busy),
            no_answer = self.total(Skip::NoAnswer),
            task_failed = self.total(Skip::TaskFailed),
            lock_timeout = self.total(Skip::LockTimeout),
            read_error = self.total(Skip::ReadError),
            audit_write_failed = self.total(Skip::AuditWriteFailed),
            limiter_full = self.total(Skip::LimiterFull),
            "Action-claim checks skipped or not recorded since start (no notice / nothing recorded for those replies; counts only)"
        );
        true
    }
}

/// Writes one staff audit row: `(claim, suppressed_since_last_row,
/// limiter_overflow_total)`. `Err` ⇒ the row is not written.
pub(crate) type AuditWrite<'a> = &'a dyn Fn(&UnbackedClaim, u64, u64) -> std::io::Result<()>;

/// The production [`AuditWrite`]: the `tool_hallucination` row in
/// `<home>/security_audit.jsonl`, lock wait bounded by
/// [`CLAIM_CHECK_DEADLINE`].
pub(crate) fn audit_writer<'a>(
    home: &'a Path,
    agent_id: &'a str,
) -> impl Fn(&UnbackedClaim, u64, u64) -> std::io::Result<()> + 'a {
    move |u: &UnbackedClaim, suppressed: u64, overflow: u64| {
        duduclaw_security::audit::log_workspace_claim_hallucination(
            home,
            agent_id,
            &u.claim.matched_text,
            u.claim.claim_type.expected_tool(),
            suppressed,
            overflow,
            CLAIM_CHECK_DEADLINE,
        )
    }
}

/// Record `unbacked` for one STAFF reply (non-staff never gets here). Per
/// claim: [`ClaimLimiter::decide`] (an `Audit` reserves the key, so only one
/// row per key is in flight); the row is written with `write`, and only on
/// success the limiter window starts ([`ClaimLimiter::commit`]) and the log
/// line + autopilot event follow. A failed write releases the reservation
/// ([`ClaimLimiter::release`]) and starts no suppression; it and a full
/// limiter table are counted in `skips`. Returns one [`Record`] outcome per
/// claim, in order.
/// Blocking (audit file writes): runs on the check's blocking task.
pub(crate) fn record_unbacked(
    agent_id: &str,
    session_id: &str,
    unbacked: &[UnbackedClaim],
    limiter: &Mutex<ClaimLimiter>,
    skips: &SkipLog,
    now: Instant,
    write: AuditWrite<'_>,
) -> Vec<Record> {
    fn lock(limiter: &Mutex<ClaimLimiter>) -> std::sync::MutexGuard<'_, ClaimLimiter> {
        limiter.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    let mut outcomes = Vec::with_capacity(unbacked.len());
    for u in unbacked {
        let family = &u.claim.claim_type;
        // The limiter lock is held for the decision, the commit and the
        // release only, never across the write. While this write is in
        // flight the key is reserved: other occurrences are suppressed.
        let decision = lock(limiter).decide(agent_id, family, now);
        let outcome = match decision {
            Decision::Audit(ticket) => match write(u, ticket.reported, skips.total(Skip::LimiterFull)) {
                Ok(()) => {
                    let suppressed = ticket.reported;
                    if !lock(limiter).commit(agent_id, family, ticket, now) {
                        // The reservation expired meanwhile: the row stands,
                        // the window belongs to whoever holds the key now.
                        debug!(agent = %agent_id, claim_type = ?family, "stale action-claim reservation; window not started");
                    }
                    warn!(
                        agent = %agent_id,
                        session_id,
                        claim_type = ?family,
                        kind = ?u.kind,
                        matched_text = %u.claim.matched_text,
                        reason = %u.reason,
                        suppressed_since_last_row = suppressed,
                        "unbacked workspace claim"
                    );
                    crate::security_autopilot::emit_tool_hallucination(agent_id);
                    Record::Audit
                }
                Err(_) => {
                    lock(limiter).release(agent_id, family, ticket);
                    skips.note(Skip::AuditWriteFailed, now);
                    Record::WriteFailed
                }
            },
            Decision::Overflow => {
                skips.note(Skip::LimiterFull, now);
                Record::Overflow
            }
            Decision::Suppressed => {
                debug!(
                    agent = %agent_id,
                    claim_type = ?family,
                    "unbacked workspace claim counted (audited within the window or being audited)"
                );
                Record::Suppressed
            }
        };
        outcomes.push(outcome);
    }
    outcomes
}

/// What a check's blocking work sends to the waiting reply: `Some` = the
/// unbacked claims (possibly none); `None` = the evidence could not be read
/// (already counted in the aggregator by the work itself).
pub(crate) type Evidence = Option<Vec<UnbackedClaim>>;

/// Gate and run one reply's check. `staff` is [`staff_audience`], decided
/// by the caller BEFORE anything else. `work` is everything an admitted
/// check does on its blocking thread; it must send the evidence on the
/// oneshot as soon as it has it.
///
/// - no claims ⇒ `None`;
/// - not staff ⇒ count only, `None`: `work` never runs, no permit, no task;
/// - no free permit in `slots` ⇒ [`Skip::Busy`], `None`: `work` never runs;
/// - otherwise ONE `spawn_blocking` task runs `work`, owning the permit
///   until `work` really returns (also after this function stopped
///   waiting), and this waits at most `wait` for the evidence. No answer in
///   time ⇒ [`Skip::NoAnswer`]; the task ending without an answer ⇒
///   [`Skip::TaskFailed`]; both `None` (no notice).
pub(crate) async fn run_check<W>(
    staff: bool,
    claims: Vec<ActionClaim>,
    slots: &Arc<Semaphore>,
    skips: &SkipLog,
    wait: Duration,
    work: W,
) -> Evidence
where
    W: FnOnce(Vec<ActionClaim>, oneshot::Sender<Evidence>) + Send + 'static,
{
    if claims.is_empty() {
        return None;
    }
    if !staff {
        count_non_staff(claims.len() as u64, Instant::now());
        return None;
    }
    let Ok(permit) = Arc::clone(slots).try_acquire_owned() else {
        skips.note(Skip::Busy, Instant::now());
        return None;
    };
    let (tx, rx) = oneshot::channel();
    // Detached on purpose: the task's own waits are bounded, and the permit
    // it owns keeps the number of outstanding tasks at the permit count.
    drop(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(claims, tx);
    }));
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(evidence)) => evidence,
        Ok(Err(_)) => {
            skips.note(Skip::TaskFailed, Instant::now());
            None
        }
        Err(_) => {
            skips.note(Skip::NoAnswer, Instant::now());
            None
        }
    }
}

/// The production `work` for [`run_check`]: the evidence read (log lock
/// wait bounded by [`CLAIM_CHECK_DEADLINE`]), the evidence sent to the
/// reply, then — only when the reply took it — [`record_unbacked`] with the
/// bounded [`audit_writer`]. A read failure is counted here; nothing is
/// recorded when the read failed or the reply stopped waiting.
fn check_job(
    home: &Path,
    agent_id: &str,
    session_id: &str,
    turn_id: &str,
) -> impl FnOnce(Vec<ActionClaim>, oneshot::Sender<Evidence>) + Send + 'static {
    let (home, agent_id) = (home.to_path_buf(), agent_id.to_string());
    let (session_id, turn_id) = (session_id.to_string(), turn_id.to_string());
    move |claims: Vec<ActionClaim>, tx: oneshot::Sender<Evidence>| {
        let read = verify_claims_in_session(
            &home,
            &agent_id,
            Some(session_id.as_str()),
            Some(turn_id.as_str()).filter(|t| !t.is_empty()),
            &claims,
            chrono::Utc::now(),
            CLAIM_CHECK_DEADLINE,
        );
        let unbacked = match read {
            Ok(unbacked) => unbacked,
            Err(e) => {
                let skip = if e.kind() == std::io::ErrorKind::TimedOut { Skip::LockTimeout } else { Skip::ReadError };
                SKIPS.note(skip, Instant::now());
                let _ = tx.send(None);
                return;
            }
        };
        if unbacked.is_empty() {
            let _ = tx.send(Some(Vec::new()));
            return;
        }
        // The notice is not delayed by the audit write. A reply that stopped
        // waiting gave no notice, so nothing is recorded for it either.
        if tx.send(Some(unbacked.clone())).is_err() {
            return;
        }
        let write = audit_writer(&home, &agent_id);
        record_unbacked(&agent_id, &session_id, &unbacked, &CLAIM_LIMITER, &SKIPS, Instant::now(), &write);
    }
}

/// The channel-reply call site: check `reply`, record unbacked claims and,
/// in `notice` mode for staff, return the reply with one notice appended.
/// `conversation` is the conversation the turn came from; `session_id` the
/// session this employee answers in (the one its tool calls are stamped
/// with).
pub(super) async fn check_reply(
    home: &Path,
    agent_id: &str,
    conversation: &str,
    session_id: &str,
    turn_id: &str,
    user_id: &str,
    reply: String,
) -> String {
    if agent_id.is_empty() || session_id.is_empty() || reply.trim().is_empty() {
        return reply;
    }
    // Pure and bounded (`MAX_SCAN_CHARS`): no I/O on the async path.
    let claims = extract_workspace_claims(&reply);
    if claims.is_empty() {
        return reply;
    }
    // The audience is decided FIRST: a non-staff reply is only counted (no
    // lookup, no task, no lock, no audit). `staff_audience` reads a
    // task-local, so it runs on this task.
    let staff = staff_audience(home, conversation, session_id, user_id, agent_id);
    let job = check_job(home, agent_id, session_id, turn_id);
    let Some(unbacked) = run_check(staff, claims, &CLAIM_CHECK_SLOTS, &SKIPS, CLAIM_CHECK_DEADLINE, job).await else {
        return reply;
    };
    if unbacked.is_empty() || !staff {
        return reply;
    }
    let mode = ClaimCheckMode::from_home(home);
    if mode != ClaimCheckMode::Notice {
        return reply;
    }
    let language = crate::prompt_identity::resolve_fixed_message_language_for_id(home, agent_id).await;
    let notice = notice_for_reply(mode, true, session_id, &reply, &unbacked, language.as_deref());
    append_notice(reply, notice)
}

#[cfg(test)]
#[path = "claim_notice_tests.rs"]
mod tests;
